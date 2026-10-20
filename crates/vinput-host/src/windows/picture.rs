//! Gathering what a picture for the windows is drawn from.
//!
//! Most of it is at hand: the settings, the installer, what is known about
//! updates. A few things take a moment to find out, so they are kept in
//! [`Slow`] and found out again only now and then. Where the models and the
//! engine stand is read off their folders, file by file, so that is kept
//! too ([`OnDisk`]) for as long as nothing has happened that could change
//! it. The drawing itself is in [`crate::view`], which looks at nothing but
//! the facts gathered here.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::Host;
use crate::view::{self, Facts, UpdateFacts};
use vinput_core::models::{self, EngineState, Installer, Model, Runner, Status};
use vinput_core::stt::check;
use vinput_core::{api, audio, gpu, hardware, memo, rewrite, secrets};
use vinput_view as wire;

/// After this many ticks of the window timer, about two seconds, what takes
/// a while to find out is found out again: the microphones, the disk space
/// and whether a key is stored.
const SLOW_TICKS: u32 = 8;
/// How often at most this PC is looked over for a Google sign-in.
const SIGN_IN_EVERY: Duration = Duration::from_secs(10);

/// Whether this PC has a Google sign-in made with the Google Cloud CLI.
///
/// Finding out is a look at a file, and the environment may name one on a
/// share that takes its time to answer. So the look happens on a thread of
/// its own and the loop only reads what the last one found: hotkeys never
/// wait for a file server.
#[derive(Default)]
struct SignIn {
    found: Arc<AtomicBool>,
    looking: Arc<AtomicBool>,
    looked: Option<Instant>,
}

impl SignIn {
    fn is_found(&self) -> bool {
        self.found.load(Ordering::Relaxed)
    }

    /// Has it looked for again, unless a look is under way or `now` is too
    /// soon after the last one. `look` is what looks.
    fn look_again(&mut self, now: Instant, look: fn() -> bool) {
        let due = self
            .looked
            .is_none_or(|looked| now.duration_since(looked) >= SIGN_IN_EVERY);
        if !due || self.looking.swap(true, Ordering::AcqRel) {
            return;
        }
        self.looked = Some(now);
        let (found, looking) = (Arc::clone(&self.found), Arc::clone(&self.looking));
        let started = std::thread::Builder::new()
            .name("vinput-google".into())
            .spawn(move || {
                found.store(look(), Ordering::Relaxed);
                looking.store(false, Ordering::Release);
            });
        if started.is_err() {
            self.looking.store(false, Ordering::Release);
        }
    }

    /// Looks the next time it is asked to, however recent the last look.
    fn forget_when(&mut self) {
        self.looked = None;
    }
}

/// What the windows show that takes a moment to find out. It is found out
/// when a window opens and every couple of seconds while one is open, not
/// for every picture.
pub(super) struct Slow {
    /// The audio inputs that are connected.
    microphones: Vec<audio::Microphone>,
    /// Bytes the downloaded models and engines take on disk.
    disk_bytes: u64,
    /// Whether a Gemini API key is in the credential store.
    gemini_key_stored: bool,
    /// The address of the service account whose key for Google Cloud is in
    /// the credential store.
    google_account: Option<String>,
    /// Whether this PC has a Google sign-in made with the Google Cloud CLI.
    google_sign_in: SignIn,
    /// Where the models and the engine stand, if that has been read off
    /// the folders since anything could have changed it.
    on_disk: Option<OnDisk>,
    /// Ticks of the window timer since all this was found out.
    age: u32,
}

/// Where the models and the engine stand, which takes a look at a handful
/// of files for every model to find out.
///
/// It is read again for every picture while something is being installed,
/// since the picture then shows how far along that is, and after anything
/// the host did or heard: a command of a window, a search or a picker that
/// is done, a setting that changed. Between those, from one tick of the
/// window timer to the next, nothing in Vinput changes it, so the ticks
/// reuse what was read. What changes without Vinput's doing, a file someone
/// deletes by hand, is seen when the rest of [`Slow`] is found out again.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct OnDisk {
    /// Whether this is for models run on the graphics card.
    gpu: bool,
    /// Whether the engine that runs models on a graphics card is installed.
    card_engine: bool,
    /// Every model in the order to list them, with where each stands.
    models: Vec<(&'static Model, Status)>,
    /// The models that write, likewise.
    writers: Vec<(&'static Model, Status)>,
    /// Where the engine the settings ask for stands.
    engine: EngineState,
}

impl OnDisk {
    /// Reads it off the folders, and off the installs under way. `wanted`
    /// says whether the model in use has an engine with a build of its own
    /// for `gpu` ([`models::wants_engine`]).
    fn read(installer: &Installer, gpu: bool, wanted: bool) -> Self {
        Self {
            gpu,
            card_engine: models::engine_program(true, Runner::Server).is_some(),
            models: models::all()
                .into_iter()
                .map(|model| (model, installer.status(model, gpu)))
                .collect(),
            writers: models::writers()
                .iter()
                .map(|model| (model, installer.status(model, gpu)))
                .collect(),
            // No engine is fetched on its own for a PC without a local
            // model, nor for a model that brings the only engine it has.
            engine: if wanted {
                installer.engine_state(gpu)
            } else {
                EngineState::Ready
            },
        }
    }
}

impl Default for Slow {
    fn default() -> Self {
        Self {
            microphones: Vec::new(),
            disk_bytes: 0,
            gemini_key_stored: false,
            google_account: None,
            google_sign_in: SignIn::default(),
            on_disk: None,
            age: SLOW_TICKS,
        }
    }
}

impl Slow {
    pub(super) fn is_stale(&self) -> bool {
        self.age >= SLOW_TICKS
    }

    /// Counts one tick of the window timer.
    pub(super) fn tick(&mut self) {
        self.age = self.age.saturating_add(1);
    }

    /// Has it found out again before the next picture is drawn.
    pub(super) fn make_stale(&mut self) {
        self.age = SLOW_TICKS;
        self.google_sign_in.forget_when();
    }

    pub(super) fn refresh(&mut self) {
        self.microphones = audio::microphones().unwrap_or_default();
        self.disk_bytes = models::disk_usage();
        self.gemini_key_stored = secrets::get(secrets::GEMINI_API_KEY).is_some();
        self.google_account = check::stored_account();
        self.google_sign_in
            .look_again(Instant::now(), check::sign_in_found);
        // A file may have been deleted by hand in the meantime.
        self.on_disk = None;
        self.age = 0;
    }

    /// Something has happened that may have changed where the models and
    /// the engine stand: that is read again for the next picture.
    pub(super) fn forget_on_disk(&mut self) {
        self.on_disk = None;
    }

    /// Where the models and the engine stand, for models run on the
    /// graphics card or not: what is known if it still holds, and what
    /// `read` finds otherwise. `installing` says whether an install is
    /// under way, during which nothing is kept.
    fn on_disk(&mut self, gpu: bool, installing: bool, read: impl FnOnce() -> OnDisk) -> OnDisk {
        if installing {
            self.on_disk = None;
            return read();
        }
        match &self.on_disk {
            Some(known) if known.gpu == gpu => known.clone(),
            _ => {
                let found = read();
                self.on_disk = Some(found.clone());
                found
            }
        }
    }
}

impl Host {
    /// Everything the picture for the windows is drawn from, as it is now.
    pub(super) fn picture(&mut self) -> wire::View {
        let gpu = self.settings.use_gpu();
        let installer = &self.installer;
        let settings = &self.settings;
        let on_disk = self.slow.on_disk(gpu, installer.is_busy(), || {
            let wanted = models::wants_engine(settings.provider, &settings.model);
            OnDisk::read(installer, gpu, wanted)
        });
        let updates = &self.updates;
        view::build(&Facts {
            settings,
            listening: self.ticking,
            graphics_card: gpu::card().map(|card| card.name.as_str()),
            card_engine: on_disk.card_engine,
            card_use: view::CardUse::of(vinput_core::stt::fell_back()),
            pc: hardware::pc(),
            models: on_disk.models,
            engine: on_disk.engine,
            disk_bytes: self.slow.disk_bytes,
            microphones: &self.slow.microphones,
            clients: self
                .api
                .as_ref()
                .map(api::Server::clients)
                .unwrap_or_default(),
            gemini_key_stored: self.slow.gemini_key_stored,
            google: self.google.facts(
                self.slow.google_account.as_deref(),
                self.slow.google_sign_in.is_found(),
                settings,
            ),
            default_memo_dir: memo::folder("").to_string_lossy().into_owned(),
            update: UpdateFacts {
                checking: updates.checking.is_some(),
                last_check: &updates.last_check,
                release: updates.release.as_ref(),
                downloaded: updates.downloads.progress(),
                installing: updates.installing,
                failed: updates.failed.as_deref(),
            },
            pro: self.pro.facts(),
            writers: on_disk.writers,
            standing: rewrite::local::engine().standing(),
            finder: self.finder.snapshot(),
            notice: self.notice.clone(),
            system_languages: vinput_core::languages::preferred(),
            hub_installed: vinput_core::hub::installed(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_google_sign_in_is_looked_for_off_the_loop_and_not_all_the_time() {
        use std::sync::atomic::AtomicU32;
        static LOOKS: AtomicU32 = AtomicU32::new(0);
        fn slow_look() -> bool {
            LOOKS.fetch_add(1, Ordering::SeqCst);
            // A file on a share that takes its time.
            std::thread::sleep(Duration::from_millis(300));
            true
        }
        let mut sign_in = SignIn::default();
        let begun = Instant::now();
        sign_in.look_again(begun, slow_look);
        // The loop is not held up, and knows nothing new yet.
        assert!(begun.elapsed() < Duration::from_millis(200));
        assert!(!sign_in.is_found());
        // While one look is under way no second one is started, and a
        // moment after it neither.
        sign_in.forget_when();
        sign_in.look_again(begun, slow_look);
        let waiting = Instant::now();
        while !sign_in.is_found() && waiting.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(sign_in.is_found());
        while sign_in.looking.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        sign_in.look_again(begun + Duration::from_secs(1), slow_look);
        assert_eq!(LOOKS.load(Ordering::SeqCst), 1);
        // After a while, or when something was done, it is looked for anew.
        sign_in.look_again(begun + SIGN_IN_EVERY, slow_look);
        let waiting = Instant::now();
        while LOOKS.load(Ordering::SeqCst) < 2 && waiting.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(LOOKS.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn what_is_slow_to_find_out_is_found_out_at_first_and_now_and_then() {
        let mut slow = Slow::default();
        assert!(slow.is_stale());
        slow.age = 0;
        assert!(!slow.is_stale());
        slow.age = SLOW_TICKS - 1;
        assert!(!slow.is_stale());
        slow.make_stale();
        assert!(slow.is_stale());
    }

    /// Reads where the models stand through `slow`, counting in `reads`
    /// how often the folders were really looked at.
    fn look(slow: &mut Slow, gpu: bool, installing: bool, reads: &mut u32) -> OnDisk {
        slow.on_disk(gpu, installing, || {
            *reads += 1;
            OnDisk {
                gpu,
                card_engine: false,
                models: Vec::new(),
                writers: Vec::new(),
                engine: EngineState::Ready,
            }
        })
    }

    #[test]
    fn the_folders_are_not_read_again_from_one_tick_to_the_next() {
        let mut slow = Slow::default();
        let mut reads = 0;
        for _ in 0..5 {
            look(&mut slow, false, false, &mut reads);
        }
        assert_eq!(reads, 1);
    }

    #[test]
    fn the_folders_are_read_again_after_anything_that_could_change_them() {
        let mut slow = Slow::default();
        let mut reads = 0;
        look(&mut slow, false, false, &mut reads);
        // Something was done or heard.
        slow.forget_on_disk();
        look(&mut slow, false, false, &mut reads);
        assert_eq!(reads, 2);
        // The graphics card was switched on: another engine, other answers.
        assert!(look(&mut slow, true, false, &mut reads).gpu);
        assert_eq!(reads, 3);
        look(&mut slow, true, false, &mut reads);
        assert_eq!(reads, 3);
    }

    #[test]
    fn every_picture_reads_the_folders_while_something_is_installed() {
        let mut slow = Slow::default();
        let mut reads = 0;
        look(&mut slow, false, false, &mut reads);
        for _ in 0..3 {
            look(&mut slow, false, true, &mut reads);
        }
        assert_eq!(reads, 4);
        // And once more when the install is over: what was known before it
        // began no longer holds.
        look(&mut slow, false, false, &mut reads);
        look(&mut slow, false, false, &mut reads);
        assert_eq!(reads, 5);
    }
}
