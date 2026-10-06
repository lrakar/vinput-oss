//! The search for a model to add: looking up what a Hugging Face link or a
//! path on this PC offers, and adding the file picked from it.
//!
//! Both steps reach out to the network or the disk, so each runs on a thread
//! of its own that lasts as long as the step. The background process looks
//! at where the search stands whenever it draws a picture for its windows,
//! and is woken when a step is done.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;

use vinput_core::models::Model;
use vinput_core::models::custom::{self, Choice, Origin};
use vinput_core::{Error, Result};
use vinput_view::FoundFile;

/// What the search says while it looks a link or a path up.
const LOOKING: &str = "Looking it up";
/// What it says while it reads the first bytes of the file picked.
const CHECKING: &str = "Checking the file";
/// What it says when asked to look up nothing.
const NOTHING_TO_FIND: &str = "Paste a Hugging Face link, or pick a file on this PC.";

/// Where the search stands.
#[derive(Default)]
struct State {
    /// Counts the steps, so that the answer to one that was replaced by a
    /// newer one is dropped.
    round: u64,
    /// What it is doing, while it is at work.
    busy: Option<&'static str>,
    /// The files the last search found.
    choices: Vec<Choice>,
    /// Why the last step failed.
    error: Option<String>,
    /// The model just added, until the host has put it to work.
    added: Option<&'static Model>,
    /// Its id, for the windows, until the search is cleared or goes on.
    shown: Option<String>,
}

/// What a step of adding a model found.
enum Found {
    /// The files a link or a path offers.
    Files(Vec<Choice>),
    /// The model added.
    Model(&'static Model),
}

/// The search as the windows are to show it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Snapshot {
    /// What it is doing, while it is at work.
    pub(crate) busy: Option<&'static str>,
    /// The files the last search found, each with whether it is on the list
    /// of models already.
    pub(crate) files: Vec<(Choice, bool)>,
    /// Why the last step failed.
    pub(crate) error: Option<String>,
    /// Id of the model just added.
    pub(crate) added: Option<String>,
}

/// What the windows say about a file: what kind of model it is when its
/// first bytes have been read, where it comes from otherwise.
pub(crate) fn found_file(choice: &Choice, added: bool) -> FoundFile {
    let place = match &choice.origin {
        Origin::Hub(repo) => format!("huggingface.co/{repo}"),
        Origin::Disk => "On this PC".to_owned(),
    };
    let detail = match (&choice.whisper, added) {
        (_, true) => "On the list of models already".to_owned(),
        (Some(whisper), false) => format!("{}  \u{b7}  {place}", whisper.describe()),
        (None, false) => place,
    };
    FoundFile {
        path: choice.path.clone(),
        bytes: choice.bytes,
        detail,
        usable: true,
    }
}

/// The search for a model to add.
pub(crate) struct Finder {
    state: Arc<Mutex<State>>,
    /// Called, on the thread of a step, when that step is done.
    wake: Arc<dyn Fn() + Send + Sync>,
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Finder {
    /// A search that has found nothing yet. `wake` is called, on another
    /// thread, each time a step is done.
    pub(crate) fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            state: Arc::default(),
            wake: Arc::new(wake),
        }
    }

    /// Looks up what `text`, a link or a path, offers.
    pub(crate) fn find(&self, text: &str) {
        let text = text.trim().to_owned();
        if text.is_empty() {
            let mut state = lock(&self.state);
            state.round += 1;
            state.busy = None;
            state.choices.clear();
            state.shown = None;
            state.error = Some(NOTHING_TO_FIND.to_owned());
            return;
        }
        lock(&self.state).choices.clear();
        self.start(LOOKING, move || custom::choices(&text).map(Found::Files));
    }

    /// Adds the file at `index` of what was found to the models, once its
    /// first bytes show that it is a Whisper model.
    pub(crate) fn add(&self, index: usize) {
        let Some(choice) = lock(&self.state).choices.get(index).cloned() else {
            return;
        };
        self.start(CHECKING, move || custom::add(&choice).map(Found::Model));
    }

    /// Forgets what was found. A step that is under way is forgotten too:
    /// its answer is dropped when it comes.
    pub(crate) fn clear(&self) {
        let mut state = lock(&self.state);
        *state = State {
            round: state.round + 1,
            // A model that was added and not put to work yet still is.
            added: state.added,
            ..State::default()
        };
    }

    /// The model that was just added, once: it is the caller's to install.
    pub(crate) fn take_added(&self) -> Option<&'static Model> {
        lock(&self.state).added.take()
    }

    /// Where the search stands.
    pub(crate) fn snapshot(&self) -> Snapshot {
        // The list of models is asked about each file with the search let
        // go of: it has a lock of its own.
        let (busy, choices, error, added) = {
            let state = lock(&self.state);
            (
                state.busy,
                state.choices.clone(),
                state.error.clone(),
                state.shown.clone(),
            )
        };
        Snapshot {
            busy,
            files: choices
                .into_iter()
                .map(|choice| {
                    let added = choice.is_added();
                    (choice, added)
                })
                .collect(),
            error,
            added,
        }
    }

    /// Runs one step in the background. Its answer is kept unless a newer
    /// step has started, or the search was cleared, in the meantime.
    fn start(&self, busy: &'static str, work: impl FnOnce() -> Result<Found> + Send + 'static) {
        let round = {
            let mut state = lock(&self.state);
            state.round += 1;
            state.busy = Some(busy);
            state.error = None;
            state.shown = None;
            state.round
        };
        let (shared, wake) = (Arc::clone(&self.state), Arc::clone(&self.wake));
        let spawned = thread::Builder::new()
            .name("vinput-add-model".into())
            .spawn(move || {
                let found = work();
                {
                    let mut state = lock(&shared);
                    if state.round != round {
                        return;
                    }
                    state.busy = None;
                    match found {
                        Ok(Found::Files(choices)) => state.choices = choices,
                        Ok(Found::Model(model)) => {
                            state.choices.clear();
                            state.shown = Some(model.id.to_owned());
                            state.added = Some(model);
                        }
                        Err(Error::Install(why)) => state.error = Some(why),
                        Err(err) => state.error = Some(err.to_string()),
                    }
                }
                wake();
            });
        if let Err(err) = spawned {
            let mut state = lock(&self.state);
            state.busy = None;
            state.error = Some(format!("Could not start: {err}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use vinput_core::models::CATALOG;
    use vinput_core::models::ggml::Whisper;

    use super::*;

    const WAIT: Duration = Duration::from_secs(10);

    fn choice(path: &str, origin: Origin) -> Choice {
        Choice {
            path: path.into(),
            url: format!(
                "https://huggingface.co/org/model/resolve/{}/{path}",
                "0".repeat(40)
            ),
            sha256: None,
            bytes: 1_533_763_059,
            origin,
            whisper: None,
        }
    }

    fn finder() -> (Finder, mpsc::Receiver<()>) {
        let (woken, wakes) = mpsc::channel();
        let woken = Mutex::new(woken);
        let finder = Finder::new(move || {
            if let Ok(woken) = woken.lock() {
                let _ = woken.send(());
            }
        });
        (finder, wakes)
    }

    #[test]
    fn a_search_shows_that_it_is_at_work_and_then_what_it_found() {
        let (finder, wakes) = finder();
        let (go, wait) = mpsc::channel::<()>();
        let found = vec![choice("ggml-medium.bin", Origin::Hub("org/model".into()))];
        let answer = found.clone();
        finder.start(LOOKING, move || {
            let _ = wait.recv_timeout(WAIT);
            Ok(Found::Files(answer))
        });
        assert_eq!(finder.snapshot().busy, Some(LOOKING));
        assert!(finder.snapshot().files.is_empty());

        let _ = go.send(());
        assert_eq!(wakes.recv_timeout(WAIT), Ok(()));
        let after = finder.snapshot();
        assert_eq!(after.busy, None);
        assert_eq!(after.error, None);
        assert_eq!(after.files.len(), 1);
        assert_eq!(after.files[0].0, found[0]);
    }

    #[test]
    fn a_failed_step_says_why_in_the_words_it_was_given() {
        let (finder, wakes) = finder();
        finder.start(LOOKING, || {
            Err(Error::Install("This is a GGUF file.".into()))
        });
        assert_eq!(wakes.recv_timeout(WAIT), Ok(()));
        let after = finder.snapshot();
        assert_eq!(after.error.as_deref(), Some("This is a GGUF file."));
        assert_eq!(after.busy, None);
    }

    #[test]
    fn the_answer_to_a_search_that_was_cleared_is_dropped() {
        let (finder, wakes) = finder();
        let (go, wait) = mpsc::channel::<()>();
        let (left, gone) = mpsc::channel::<()>();
        finder.start(LOOKING, move || {
            let _ = wait.recv_timeout(WAIT);
            let _ = left.send(());
            Ok(Found::Files(vec![choice("a.bin", Origin::Disk)]))
        });
        finder.clear();
        assert_eq!(finder.snapshot(), Snapshot::default());
        let _ = go.send(());
        assert_eq!(gone.recv_timeout(WAIT), Ok(()));
        // Nobody is woken for an answer that nobody waits for.
        assert!(wakes.recv_timeout(Duration::from_millis(200)).is_err());
        assert_eq!(finder.snapshot(), Snapshot::default());
    }

    #[test]
    fn looking_up_nothing_says_what_to_do_instead() {
        let (finder, _wakes) = finder();
        finder.find("   ");
        let after = finder.snapshot();
        assert_eq!(after.error.as_deref(), Some(NOTHING_TO_FIND));
        assert_eq!(after.busy, None);
    }

    #[test]
    fn a_model_that_was_added_is_handed_over_once_and_named_until_cleared() {
        let (finder, wakes) = finder();
        let model = &CATALOG[0];
        finder.start(CHECKING, move || Ok(Found::Model(model)));
        assert_eq!(wakes.recv_timeout(WAIT), Ok(()));
        assert_eq!(finder.snapshot().added.as_deref(), Some(model.id));
        assert_eq!(finder.take_added().map(|added| added.id), Some(model.id));
        assert!(finder.take_added().is_none());
        // The windows still know which model to point at.
        assert_eq!(finder.snapshot().added.as_deref(), Some(model.id));
        finder.clear();
        assert_eq!(finder.snapshot().added, None);
    }

    #[test]
    fn clearing_keeps_a_model_that_is_still_to_be_installed() {
        let (finder, wakes) = finder();
        let model = &CATALOG[0];
        finder.start(CHECKING, move || Ok(Found::Model(model)));
        assert_eq!(wakes.recv_timeout(WAIT), Ok(()));
        finder.clear();
        assert_eq!(finder.take_added().map(|added| added.id), Some(model.id));
    }

    #[test]
    fn a_file_is_described_by_what_is_known_about_it() {
        let hub = choice("q/ggml-medium-q5_0.bin", Origin::Hub("org/model".into()));
        let shown = found_file(&hub, false);
        assert_eq!(shown.path, "q/ggml-medium-q5_0.bin");
        assert_eq!(shown.bytes, 1_533_763_059);
        assert_eq!(shown.detail, "huggingface.co/org/model");
        assert!(shown.usable);

        let disk = choice(r"C:\Models\ggml-small.bin", Origin::Disk);
        assert_eq!(found_file(&disk, false).detail, "On this PC");
        assert!(found_file(&disk, true).detail.contains("already"));

        // Once its first bytes have been read, they say what it is.
        let known = Choice {
            whisper: Some(Whisper {
                vocabulary: 51_865,
                audio_width: 1024,
                text_layers: 24,
            }),
            ..disk
        };
        assert_eq!(
            found_file(&known, false).detail,
            "Whisper medium, 99 languages  \u{b7}  On this PC"
        );
    }
}
