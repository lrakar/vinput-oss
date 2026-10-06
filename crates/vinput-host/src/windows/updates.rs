//! Updates: looking for a newer version, downloading its setup program, and
//! handing over to it.
//!
//! The look and the download each run on a thread of their own and leave
//! their outcome where the host finds it when woken. Once the setup program
//! has been started Vinput exits, so that the program can be replaced; setup
//! starts the new version again.
//!
//! A download cannot be stopped at once: its thread notices at the next
//! piece that arrives. [`Downloads`] keeps count of the ones that were told
//! to stop, so that a new one never starts while an old one may still be
//! writing the same file.
#![allow(unsafe_code)]

use std::path::PathBuf;
use std::process::Command as Process;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

use super::Page;
use super::children::Which;
use super::tray::About;
use super::{Host, UPDATE_CHECKED, UPDATE_DOWNLOADED};
use crate::view::LastCheck;
use vinput_core::log;
use vinput_core::update::{self, Release};
use vinput_view::Surface;

/// How a look for a newer version or a download ended: what it found, or
/// why it failed in words for the user.
type Outcome<T> = Result<T, String>;

/// What the thread that looks for a newer version leaves for the host.
type Checked = Arc<Mutex<Option<Outcome<Option<Release>>>>>;

/// A setup program being downloaded, shared with the thread that does it.
#[derive(Default)]
struct Download {
    /// Bytes on disk so far.
    done: AtomicU64,
    /// Set to tell the thread to stop.
    stop: AtomicBool,
    /// Where the thread leaves the setup program, or why there is none,
    /// as the last thing it does.
    finished: Mutex<Option<Outcome<PathBuf>>>,
}

impl Download {
    fn has_ended(&self) -> bool {
        self.finished
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }
}

/// What became of the downloads since they were last looked at.
#[derive(Debug, PartialEq, Eq)]
struct Settled {
    /// How the download that was running ended, if it has.
    result: Option<Outcome<PathBuf>>,
    /// Whether the download the user asked for while an earlier one was
    /// still stopping can start now.
    start: bool,
}

/// The download of a release, and the ones before it that were stopped and
/// have not ended yet.
#[derive(Default)]
pub(super) struct Downloads {
    running: Option<Arc<Download>>,
    stopping: Vec<Arc<Download>>,
    /// Whether the user asked for a download while one was still stopping.
    wanted: bool,
}

impl Downloads {
    /// Bytes downloaded so far, while a download runs or is about to.
    pub(super) fn progress(&self) -> Option<u64> {
        match &self.running {
            Some(download) => Some(download.done.load(Ordering::Relaxed)),
            None => self.wanted.then_some(0),
        }
    }

    /// The user asked for a download. Returns the one to run, if it can
    /// start now; one that has to wait for an earlier one to stop starts
    /// when [`settle`](Self::settle) says so.
    fn begin(&mut self) -> Option<Arc<Download>> {
        if self.running.is_some() {
            return None;
        }
        self.stopping.retain(|download| !download.has_ended());
        if !self.stopping.is_empty() {
            self.wanted = true;
            return None;
        }
        self.wanted = false;
        let download = Arc::new(Download::default());
        self.running = Some(Arc::clone(&download));
        Some(download)
    }

    /// Stops the download that runs, and forgets one that was waiting.
    fn cancel(&mut self) {
        self.wanted = false;
        if let Some(download) = self.running.take() {
            download.stop.store(true, Ordering::Relaxed);
            self.stopping.push(download);
        }
    }

    /// The thread of the download that was to run could not be started.
    fn abandon(&mut self) {
        self.running = None;
    }

    /// Looks at what the download threads have left. A download that was
    /// stopped is no longer here to be heard: only its end matters.
    fn settle(&mut self) -> Settled {
        self.stopping.retain(|download| !download.has_ended());
        let result = self.running.as_ref().and_then(|download| {
            download
                .finished
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
        });
        if result.is_some() {
            self.running = None;
        }
        Settled {
            result,
            start: self.wanted && self.running.is_none() && self.stopping.is_empty(),
        }
    }
}

/// Everything about updates: what is available and what the user is doing
/// about it.
#[derive(Default)]
pub(super) struct Updates {
    /// Where the checking thread leaves its result.
    checked: Checked,
    /// Whether a check is under way, and whether the user asked for it, in
    /// which case they are told the outcome either way.
    pub(super) checking: Option<bool>,
    /// How the last check ended, if it found nothing to install.
    pub(super) last_check: LastCheck,
    /// The newer release that was found.
    pub(super) release: Option<Release>,
    /// The download of that release.
    pub(super) downloads: Downloads,
    /// Why the last download did not end in an install.
    pub(super) failed: Option<String>,
    /// Whether setup has been started and Vinput is about to exit for it.
    pub(super) installing: bool,
}

impl Host {
    /// Shows that an update has been installed: the About page, where the
    /// version is, and a notification. The copy the user was using closed to
    /// make room for this one, and without a sign of life from it an update
    /// looks like Vinput having gone away.
    pub(super) fn show_updated(&mut self) {
        self.open_settings(Page::About);
        let (title, text) = updated_notice(update::CURRENT_VERSION);
        self.tray
            .inform(About::Settings(Page::About), &title, &text);
    }
}

/// What the notification after an update says.
fn updated_notice(version: &str) -> (String, String) {
    (
        format!("VinPUT {version} is installed"),
        "The update is done and VinPUT is running again.".to_owned(),
    )
}

impl Host {
    /// Shows the update window, if there is a release to show in it.
    pub(super) fn open_update_window(&mut self) {
        if self.updates.release.is_some() {
            self.open_window(Surface::Update);
        }
    }

    /// Looks for a newer version on another thread. `asked` says the user
    /// wants to hear the outcome even if there is nothing new.
    pub(super) fn check_for_updates(&mut self, asked: bool) {
        if let Some(pending) = &mut self.updates.checking {
            *pending |= asked;
            return;
        }
        self.updates.checking = Some(asked);
        let checked = Arc::clone(&self.updates.checked);
        // A window handle is an address that any thread may post to.
        let window = self.hwnd as usize;
        let spawned = thread::Builder::new()
            .name("vinput-update-check".into())
            .spawn(move || {
                let result = update::check().map_err(|err| err.to_string());
                *checked.lock().unwrap_or_else(PoisonError::into_inner) = Some(result);
                // SAFETY: posting to a window that has gone away fails harmlessly.
                unsafe { PostMessageW(window as HWND, UPDATE_CHECKED, 0, 0) };
            });
        if spawned.is_err() {
            self.updates.checking = None;
        }
        self.show_state();
    }

    /// Takes in the outcome of a look for a newer version, if one is there.
    pub(super) fn on_update_checked(&mut self) {
        let result = self
            .updates
            .checked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(result) = result else { return };
        let asked = self.updates.checking.take().unwrap_or(false);
        // With Settings open the answer is given there, in place.
        let in_settings = self.windows.is_open(Which::Settings);
        match result {
            Ok(Some(release)) => {
                // Said once per version, and again whenever the user asks.
                let news = self.updates.release.as_ref() != Some(&release);
                let version = release.version.clone();
                self.updates.release = Some(release);
                if asked && !in_settings {
                    self.open_update_window();
                } else if news && !asked {
                    self.tray.inform(
                        About::Update,
                        &format!("Vinput {version} is available"),
                        "Click here, or use the tray menu, to see what is new and install it.",
                    );
                }
            }
            Ok(None) => {
                self.updates.last_check = LastCheck::UpToDate;
                if asked && !in_settings {
                    self.tray.inform(
                        About::Nothing,
                        "Vinput is up to date",
                        &format!("Version {} is the newest.", update::CURRENT_VERSION),
                    );
                }
            }
            Err(err) => {
                log::write(&err);
                if asked && !in_settings {
                    self.tray
                        .warn(About::Nothing, "Could not check for updates", &err);
                }
                self.updates.last_check = LastCheck::Failed(err);
            }
        }
        self.show_state();
    }

    /// Starts downloading the release that was found.
    pub(super) fn start_update(&mut self) {
        let Some(release) = self.updates.release.clone() else {
            return;
        };
        if self.updates.installing {
            return;
        }
        // Setup would replace the installed Vinput, which is not this copy.
        if vinput_core::instance::is_separate() {
            self.updates.failed =
                Some("A separate copy of Vinput does not install updates.".to_owned());
            return;
        }
        let Some(job) = self.updates.downloads.begin() else {
            return;
        };
        self.updates.failed = None;
        // A window handle is an address that any thread may post to.
        let window = self.hwnd as usize;
        let spawned = thread::Builder::new()
            .name("vinput-update".into())
            .spawn(move || {
                let result = release
                    .download(&mut |bytes| {
                        job.done.store(bytes, Ordering::Relaxed);
                        !job.stop.load(Ordering::Relaxed)
                    })
                    .map_err(|err| err.to_string());
                *job.finished.lock().unwrap_or_else(PoisonError::into_inner) = Some(result);
                // SAFETY: posting to a window that has gone away fails harmlessly.
                unsafe { PostMessageW(window as HWND, UPDATE_DOWNLOADED, 0, 0) };
            });
        if let Err(err) = spawned {
            self.updates.downloads.abandon();
            self.updates.failed = Some(format!("The download could not be started: {err}"));
        }
    }

    /// Stops the download of the release, if one is running.
    pub(super) fn cancel_update(&mut self) {
        self.updates.downloads.cancel();
    }

    /// When the download of the release is complete, hands over to the
    /// setup program. Returns whether Vinput should now exit for it.
    pub(super) fn on_update_downloaded(&mut self) -> bool {
        let settled = self.updates.downloads.settle();
        if settled.start {
            self.start_update();
        }
        let Some(result) = settled.result else {
            return false;
        };
        let started = result.and_then(|setup| {
            // The setup program replaces this one and starts it again.
            Process::new(&setup)
                .arg("--update")
                .spawn()
                .map(drop)
                .map_err(|err| format!("starting {} failed: {err}", setup.display()))
        });
        match started {
            Ok(()) => {
                self.updates.installing = true;
                self.show_state();
                true
            }
            Err(err) => {
                log::write(&err);
                // With no window to say it in, the tray says it.
                if !self.windows.any() {
                    self.tray.warn(About::Update, "Update not installed", &err);
                }
                self.updates.failed = Some(err);
                self.show_state();
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the thread of a download does when it ends.
    fn end(download: &Download, result: Outcome<PathBuf>) {
        *download
            .finished
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(result);
    }

    const NOTHING: Settled = Settled {
        result: None,
        start: false,
    };

    #[test]
    fn a_download_runs_once_and_its_end_is_heard_once() {
        let mut downloads = Downloads::default();
        assert_eq!(downloads.progress(), None);
        let job = downloads.begin().unwrap_or_default();
        // Asking again while it runs starts nothing.
        assert!(downloads.begin().is_none());
        job.done.store(4096, Ordering::Relaxed);
        assert_eq!(downloads.progress(), Some(4096));
        assert_eq!(downloads.settle(), NOTHING);

        end(&job, Ok(PathBuf::from("setup.exe")));
        assert_eq!(
            downloads.settle(),
            Settled {
                result: Some(Ok(PathBuf::from("setup.exe"))),
                start: false,
            }
        );
        assert_eq!(downloads.settle(), NOTHING);
        assert_eq!(downloads.progress(), None);
    }

    #[test]
    fn a_stopped_download_is_told_to_stop_and_never_heard() {
        let mut downloads = Downloads::default();
        let job = downloads.begin();
        downloads.cancel();
        assert_eq!(downloads.progress(), None);
        let stopped = job
            .as_ref()
            .is_some_and(|job| job.stop.load(Ordering::Relaxed));
        assert!(stopped);
        // It had the whole file a moment before it was stopped: too late.
        if let Some(job) = &job {
            end(job, Ok(PathBuf::from("setup.exe")));
        }
        assert_eq!(downloads.settle(), NOTHING);
    }

    #[test]
    fn a_new_download_waits_until_the_stopped_one_has_ended() {
        let mut downloads = Downloads::default();
        let first = downloads.begin();
        downloads.cancel();
        // The thread of the first has not noticed yet; a second one now
        // would write the same file.
        assert!(downloads.begin().is_none());
        assert_eq!(downloads.progress(), Some(0));
        assert_eq!(downloads.settle(), NOTHING);

        if let Some(first) = &first {
            end(first, Err("stopped".to_owned()));
        }
        assert_eq!(
            downloads.settle(),
            Settled {
                result: None,
                start: true,
            }
        );
        let second = downloads.begin();
        assert!(second.is_some());
        assert!(!second.is_some_and(|job| job.stop.load(Ordering::Relaxed)));
        assert_eq!(downloads.settle(), NOTHING);
    }

    #[test]
    fn changing_ones_mind_twice_starts_nothing() {
        let mut downloads = Downloads::default();
        let first = downloads.begin();
        downloads.cancel();
        assert!(downloads.begin().is_none());
        downloads.cancel();
        if let Some(first) = &first {
            end(first, Err("stopped".to_owned()));
        }
        assert_eq!(downloads.settle(), NOTHING);
        assert_eq!(downloads.progress(), None);
        // A thread that could not be started leaves room for the next try.
        assert!(downloads.begin().is_some());
        downloads.abandon();
        assert!(downloads.begin().is_some());
    }
}
