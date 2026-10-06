//! What dictation with `VinPUT` Pro carries on with when the month's hours
//! are used up: a model on this PC.
//!
//! The choice in the settings stays `VinPUT` Pro. That the hours are used up
//! is a state of the running app, kept here and nowhere else: a time until
//! which no stream is asked for, set when the server refuses a dictation for
//! want of hours ([`Standby::take_over`]) or when the account says that none
//! are left ([`Standby::heard`]), and over when that time has passed or the
//! account says there are hours again. After a restart it is found out
//! again, from the account or from the first refusal.
//!
//! Which provider serves a key press is one number read from memory
//! ([`Standby::session`]); the clock is looked at only while that number is
//! set. The model is the one the settings name as last used on this PC, if
//! it is still here, and otherwise the first in the catalog that is; nothing
//! is ever downloaded for it. It is loaded ahead when little is left of the
//! month ([`WARM_BELOW`]) or the hours have run out, the way a chosen local
//! model is loaded when Vinput starts, and from then on it is let go and
//! loaded again as any local model is (`stt/local.rs`). No timer runs here.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{Lazy, LocalSpeech, ProviderKind, SessionOptions, SpeechProvider, Transcriber};
use crate::{log, models};

/// With less than this many seconds of the month left, the model on this
/// PC is loaded ahead: five minutes of speech.
const WARM_BELOW: u64 = 5 * 60;
/// For how long the hours are taken to be used up when the server has said
/// so to a dictation and the account has not said until when: an hour,
/// after which the server is asked again.
const ASSUMED: u64 = 3600;

/// Finds the provider to carry on with, given the model last used on this
/// PC and whether models run on the graphics card; `None` if no model is
/// on this PC.
type Find = Box<dyn Fn(&str, bool) -> Option<Arc<Lazy>> + Send + Sync>;

#[derive(Default)]
struct Local {
    /// The model last used on this PC, as the settings name it.
    model: String,
    gpu: bool,
    /// The provider that was found, until it fails or the settings change.
    ready: Option<Arc<Lazy>>,
}

/// The model on this PC that takes over, and whether it has.
#[derive(Default)]
pub struct Standby {
    /// `None` carries on with nothing.
    find: Option<Find>,
    /// Until when the hours are used up, in seconds since 1970; zero while
    /// they are not.
    until: AtomicU64,
    /// Seconds of the month left, as far as known; zero while unknown.
    left: AtomicU64,
    /// Whether a dictation was served here since [`Standby::take_served`]:
    /// 1 if so, 2 if some of what was said could not be handed over.
    served: AtomicU8,
    /// Whether the model was loaded ahead for the state the month is in.
    warmed: AtomicBool,
    local: Mutex<Local>,
}

impl std::fmt::Debug for Standby {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Standby")
            .field("until", &self.until)
            .finish_non_exhaustive()
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// A model that is on this PC, set up when it is first needed.
fn installed(preferred: &str, gpu: bool) -> Option<Arc<Lazy>> {
    let id = models::spare(preferred, gpu)?.id;
    Some(Arc::new(Lazy::new(ProviderKind::Local, move || {
        Ok(Arc::new(LocalSpeech::connect(id, gpu)?))
    })))
}

impl Standby {
    /// A standby that carries on with what `find` finds.
    pub fn new(find: impl Fn(&str, bool) -> Option<Arc<Lazy>> + Send + Sync + 'static) -> Self {
        Self {
            find: Some(Box::new(find)),
            ..Self::default()
        }
    }

    /// The one standby of this process, which carries on with a model that
    /// is downloaded.
    #[must_use]
    pub fn shared() -> Arc<Self> {
        static SHARED: OnceLock<Arc<Standby>> = OnceLock::new();
        Arc::clone(SHARED.get_or_init(|| Arc::new(Self::new(installed))))
    }

    fn local(&self) -> std::sync::MutexGuard<'_, Local> {
        self.local.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Notes the model last used on this PC and whether models run on the
    /// graphics card, as the settings say.
    pub fn prefer(&self, model: &str, gpu: bool) {
        let mut local = self.local();
        if local.model != model || local.gpu != gpu {
            *local = Local {
                model: model.to_owned(),
                gpu,
                ready: None,
            };
        }
    }

    /// The account has said how the month stands: `used` of `cap` seconds,
    /// counted anew at `resets_at`. With none left the model on this PC
    /// serves until then; with some left, `VinPUT` Pro does again.
    pub fn heard(self: &Arc<Self>, used: u64, cap: u64, resets_at: u64) {
        let left = cap.saturating_sub(used);
        let until = match (cap > 0 && left == 0, now()) {
            (false, _) => 0,
            (true, now) if resets_at > now => resets_at,
            (true, now) => now + ASSUMED,
        };
        self.left.store(left, Ordering::Relaxed);
        self.until.store(until, Ordering::Relaxed);
        self.warm_if_low();
    }

    /// Nobody is signed in, or the account cannot use `VinPUT` Pro: there
    /// are no hours to have run out.
    pub fn clear(&self) {
        self.left.store(0, Ordering::Relaxed);
        self.until.store(0, Ordering::Relaxed);
    }

    /// Until when the model on this PC serves in place of `VinPUT` Pro, in
    /// seconds since 1970; zero while it does not.
    #[must_use]
    pub fn until(&self) -> u64 {
        match self.until.load(Ordering::Relaxed) {
            0 => 0,
            until if until > now() => until,
            _ => {
                // The month is over: the server is asked again.
                self.clear();
                0
            }
        }
    }

    /// Whether a dictation was served by the model on this PC since this
    /// was last asked, and if so whether all of what was said reached it.
    pub fn take_served(&self) -> Option<bool> {
        match self.served.swap(0, Ordering::Relaxed) {
            0 => None,
            served => Some(served == 1),
        }
    }

    /// A stream has ended with this many seconds of it counted.
    pub(super) fn counted(self: &Arc<Self>, seconds: u64) {
        let left = self.left.load(Ordering::Relaxed);
        if left > 0 {
            // Never down to "unknown": the server says when none is left.
            self.left
                .store(left.saturating_sub(seconds).max(1), Ordering::Relaxed);
            self.warm_if_low();
        }
    }

    /// Has the model loaded, once, when little of the month is left.
    fn warm_if_low(self: &Arc<Self>) {
        let left = self.left.load(Ordering::Relaxed);
        let low = self.until.load(Ordering::Relaxed) != 0 || (1..WARM_BELOW).contains(&left);
        if !low {
            self.warmed.store(false, Ordering::Relaxed);
        } else if !self.warmed.swap(true, Ordering::Relaxed) {
            let standby = Arc::clone(self);
            // Should the thread not start, the dictation that needs the
            // model loads it.
            let _ = std::thread::Builder::new()
                .name("vinput-connect".into())
                .spawn(move || {
                    if let Some(Err(err)) = standby.provider().map(|local| local.warm_up()) {
                        log::write(&format!("the model to carry on with is not ready: {err}"));
                    }
                });
        }
    }

    /// The provider to carry on with, found now if it was not before.
    fn provider(&self) -> Option<Arc<Lazy>> {
        let mut local = self.local();
        if local.ready.is_none() {
            local.ready = (self.find.as_ref()?)(&local.model, local.gpu);
        }
        local.ready.clone()
    }

    /// A session of the model on this PC, if there is such a model and it
    /// starts. One that does not is looked for anew the next time.
    fn start(&self, options: SessionOptions<'_>) -> Option<Box<dyn Transcriber + Send>> {
        let started = self.provider()?.start_session(options);
        started
            .map_err(|err| {
                log::write(&format!("the model to carry on with did not start: {err}"));
                self.local().ready = None;
            })
            .ok()
    }

    /// The session for a dictation that starts now, while the hours are
    /// used up; `None` while they are not, which is all a key press costs
    /// then, and when no model on this PC can serve.
    pub(super) fn session(
        &self,
        options: SessionOptions<'_>,
    ) -> Option<Box<dyn Transcriber + Send>> {
        if self.until.load(Ordering::Relaxed) == 0 || self.until() == 0 {
            return None;
        }
        let session = self.start(options)?;
        self.served.fetch_max(1, Ordering::Relaxed);
        Some(session)
    }

    /// The server has just refused a dictation because the hours are used
    /// up: the session that carries it on, if a model on this PC can. From
    /// then on that model serves, until the account says until when or for
    /// [`ASSUMED`]. `whole` says whether all of what the server did not
    /// count can be handed to the session.
    pub(super) fn take_over(
        self: &Arc<Self>,
        options: SessionOptions<'_>,
        whole: bool,
    ) -> Option<Box<dyn Transcriber + Send>> {
        let session = self.start(options)?;
        if self.until() == 0 {
            self.until.store(now() + ASSUMED, Ordering::Relaxed);
        }
        self.warmed.store(true, Ordering::Relaxed);
        self.served
            .fetch_max(if whole { 1 } else { 2 }, Ordering::Relaxed);
        Some(session)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::stt::{Emit, Transcript};
    use crate::{Error, Result};

    /// A model on this PC as a test sees it: what it was fed, and how many
    /// sessions it was asked for.
    #[derive(Default)]
    pub(crate) struct Heard {
        pub(crate) fed: Mutex<Vec<i16>>,
        pub(crate) sessions: AtomicU64,
        /// Whether its sessions fail to start.
        pub(crate) broken: AtomicBool,
    }

    impl Heard {
        pub(crate) fn fed(&self) -> Vec<i16> {
            self.fed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }

        pub(crate) fn sessions(&self) -> u64 {
            self.sessions.load(Ordering::Relaxed)
        }
    }

    struct Model(Arc<Heard>);

    impl SpeechProvider for Model {
        fn kind(&self) -> ProviderKind {
            ProviderKind::Local
        }

        fn start_session(&self, _: SessionOptions<'_>) -> Result<Box<dyn Transcriber + Send>> {
            if self.0.broken.load(Ordering::Relaxed) {
                return Err(Error::Transcription("the model is gone".into()));
            }
            self.0.sessions.fetch_add(1, Ordering::Relaxed);
            Ok(Box::new(Session(Arc::clone(&self.0))))
        }
    }

    /// Settles nothing while it listens and " Locally." at the end.
    struct Session(Arc<Heard>);

    impl Transcriber for Session {
        fn feed(&mut self, pcm: &[i16]) -> Result<Option<Transcript>> {
            let mut fed = self.0.fed.lock().unwrap_or_else(PoisonError::into_inner);
            fed.extend_from_slice(pcm);
            Ok(None)
        }

        fn finish(&mut self, emit: &mut Emit<'_>) -> Result<()> {
            emit(Transcript {
                text: "Locally.".into(),
                is_final: true,
            })
        }
    }

    /// A standby over a made-up model, and what that model hears.
    pub(crate) fn with_model() -> (Arc<Standby>, Arc<Heard>) {
        let heard = Arc::<Heard>::default();
        let model = Arc::clone(&heard);
        let standby = Standby::new(move |_, _| {
            let model = Arc::clone(&model);
            Some(Arc::new(Lazy::new(ProviderKind::Local, move || {
                Ok(Arc::new(Model(Arc::clone(&model))))
            })))
        });
        (Arc::new(standby), heard)
    }

    impl Standby {
        /// As if the time until which the hours are used up had come.
        pub(crate) fn lapse(&self) {
            self.until.store(1, Ordering::Relaxed);
        }
    }

    const NONE: SessionOptions<'static> = SessionOptions { languages: &[] };

    #[test]
    fn vinput_pro_serves_while_there_are_hours_and_the_model_while_there_are_none() {
        let (standby, heard) = with_model();
        // Nothing known yet: the server is asked.
        assert!(standby.session(NONE).is_none());
        // Hours left.
        standby.heard(5210, 72_000, now() + 1000);
        assert!(standby.session(NONE).is_none());
        assert_eq!((standby.until(), standby.take_served()), (0, None));
        // None left: the model serves, until the month is over.
        let reset = now() + 1000;
        standby.heard(72_000, 72_000, reset);
        assert_eq!(standby.until(), reset);
        assert!(standby.session(NONE).is_some());
        assert_eq!(standby.take_served(), Some(true));
        assert_eq!(standby.take_served(), None);
        // The account says there are hours again: a tier with more of them.
        standby.heard(72_000, 144_000, reset);
        assert!(standby.session(NONE).is_none());
        // An account that names no allowance has not run out of it.
        standby.heard(10, 0, 0);
        assert!(standby.session(NONE).is_none());
        assert_eq!(heard.sessions(), 1);
    }

    #[test]
    fn vinput_pro_serves_again_once_the_month_is_over() {
        let (standby, _heard) = with_model();
        standby.heard(100, 100, now() + 1000);
        assert!(standby.session(NONE).is_some());
        // The time has come.
        standby.until.store(now() - 1, Ordering::Relaxed);
        assert!(standby.session(NONE).is_none());
        assert_eq!(standby.until(), 0);
        assert_eq!(standby.until.load(Ordering::Relaxed), 0);
        // A server that says none is left without saying until when is
        // asked again in an hour.
        standby.heard(100, 100, 0);
        let until = standby.until();
        assert!((now() + ASSUMED - 5..=now() + ASSUMED).contains(&until));
        // Signed out, there are no hours to have run out.
        standby.clear();
        assert!(standby.session(NONE).is_none());
    }

    #[test]
    fn a_refused_dictation_is_carried_on_and_the_model_serves_from_then() {
        let (standby, heard) = with_model();
        assert!(standby.take_over(NONE, false).is_some());
        assert_eq!(standby.take_served(), Some(false));
        let until = standby.until();
        assert!(until > now());
        assert!(standby.session(NONE).is_some());
        // A date the account gave is not replaced by a guess.
        let reset = now() + 50_000;
        standby.heard(9, 9, reset);
        assert!(standby.take_over(NONE, true).is_some());
        assert_eq!(standby.until(), reset);
        assert_eq!(heard.sessions(), 3);
    }

    #[test]
    fn without_a_model_on_this_pc_nothing_takes_over() {
        for standby in [
            Arc::new(Standby::default()),
            Arc::new(Standby::new(|_, _| None)),
        ] {
            standby.heard(100, 100, now() + 1000);
            assert!(standby.session(NONE).is_none());
            assert!(standby.take_over(NONE, true).is_none());
            assert_eq!(standby.take_served(), None);
        }
        // Nor is the server left out for an hour because of a refusal.
        let standby = Arc::new(Standby::new(|_, _| None));
        assert!(standby.take_over(NONE, true).is_none());
        assert_eq!(standby.until(), 0);
    }

    #[test]
    fn a_model_that_does_not_start_is_looked_for_anew() {
        let (standby, heard) = with_model();
        standby.heard(100, 100, now() + 1000);
        heard.broken.store(true, Ordering::Relaxed);
        assert!(standby.session(NONE).is_none());
        assert!(standby.local().ready.is_none());
        heard.broken.store(false, Ordering::Relaxed);
        assert!(standby.session(NONE).is_some());
    }

    #[test]
    fn the_model_is_the_one_last_used_and_found_again_when_that_changes() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let noted = Arc::clone(&asked);
        let standby = Arc::new(Standby::new(move |model, gpu| {
            let mut noted = noted.lock().unwrap_or_else(PoisonError::into_inner);
            noted.push((model.to_owned(), gpu));
            None
        }));
        standby.prefer("whisper-base", true);
        standby.heard(100, 100, now() + 1000);
        let _ = standby.session(NONE);
        standby.prefer("whisper-base", true);
        standby.prefer("whisper-base", false);
        let _ = standby.session(NONE);
        let asked = asked.lock().unwrap_or_else(PoisonError::into_inner);
        assert!(asked.contains(&("whisper-base".to_owned(), true)));
        assert_eq!(asked.last(), Some(&("whisper-base".to_owned(), false)));
    }

    #[test]
    fn the_model_is_loaded_ahead_once_when_little_of_the_month_is_left() {
        let loads = Arc::new(AtomicU64::new(0));
        let counted = Arc::clone(&loads);
        let standby = Arc::new(Standby::new(move |_, _| {
            let counted = Arc::clone(&counted);
            Some(Arc::new(Lazy::new(ProviderKind::Local, move || {
                counted.fetch_add(1, Ordering::Relaxed);
                Err(Error::Transcription("not for this test".into()))
            })))
        }));
        let loaded = |times: u64| {
            let started = std::time::Instant::now();
            while loads.load(Ordering::Relaxed) < times
                && started.elapsed() < std::time::Duration::from_secs(5)
            {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            std::thread::sleep(std::time::Duration::from_millis(30));
            loads.load(Ordering::Relaxed)
        };
        // Plenty left: nothing is loaded, also not after a dictation.
        standby.heard(0, 72_000, now() + 1000);
        standby.counted(600);
        assert_eq!(loaded(0), 0);
        // What dictations use is counted here as it is at the server.
        assert_eq!(standby.left.load(Ordering::Relaxed), 71_400);
        // Under five minutes left: loaded, once.
        standby.counted(71_400 - 299);
        assert_eq!(loaded(1), 1);
        standby.counted(10);
        standby.heard(71_800, 72_000, now() + 1000);
        assert_eq!(loaded(1), 1);
        // The count never says "none left" by itself; the server does.
        standby.counted(100_000);
        assert_eq!(standby.left.load(Ordering::Relaxed), 1);
        assert_eq!(standby.until(), 0);
        // A new month, and the end of it again.
        standby.heard(0, 72_000, now() + 1000);
        standby.heard(72_000, 72_000, now() + 1000);
        assert_eq!(loaded(2), 2);
    }
}
