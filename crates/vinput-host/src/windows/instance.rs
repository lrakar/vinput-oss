//! One Vinput at a time: how a copy that is starting finds the one that is
//! running, and what it can ask of it.
//!
//! The running copy is known by two things. A named mutex says that a copy
//! holds the hotkeys, from before it has a window until it has let go of
//! everything. The title of its hidden window is where another copy, or the
//! setup program, posts what it wants: show Settings, or exit so that the
//! program can be replaced. Both names are part of what installed versions
//! look for and never change.
#![allow(unsafe_code)]

use std::path::Path;
use std::ptr::{null, null_mut};
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND,
};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowW, MB_ICONERROR, MB_OK, MessageBoxW, PostMessageW,
};

use super::{OPEN_SETTINGS, QUIT};
use crate::win::wide;
use crate::{autostart, shortcut};
use vinput_core::{instance, log};

const SINGLE_INSTANCE_MUTEX: &str = "Local\\VinputSingleInstance";
/// Title of the hidden window, by which a second copy finds the running one.
const HOST_WINDOW_TITLE: &str = "Vinput host";
/// The title of the hidden window of this copy: the one above, unless this
/// is a separate copy with names of its own.
pub(super) fn host_window_title() -> String {
    instance::named(HOST_WINDOW_TITLE)
}

/// How long the setup program waits for the running copy to exit.
const QUIT_TIMEOUT: Duration = Duration::from_secs(8);
/// How long a copy that is starting waits for one that is on its way out to
/// let go of the mutex.
const LEAVING_TIMEOUT: Duration = Duration::from_secs(5);
/// How often either of them looks again.
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// Shows a blocking error box. Used for failures of what the user started
/// themselves, which would otherwise be invisible in a release build with no
/// console. A copy that keeps to itself ([`vinput_core::instance::is_quiet`])
/// shows none: whoever started it finds the failure in its log.
pub fn show_error(message: &str) {
    if instance::is_quiet() {
        return;
    }
    let (text, title) = (wide(message), wide("Vinput"));
    // SAFETY: both strings are NUL-terminated and outlive the call.
    unsafe {
        MessageBoxW(
            null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        )
    };
}

fn running_instance() -> HWND {
    let title = wide(&host_window_title());
    // SAFETY: the title is NUL-terminated.
    unsafe { FindWindowW(null(), title.as_ptr()) }
}

/// Tells an already-running Vinput to show its settings window, if asked to.
/// Returns whether a running copy was found.
#[must_use]
pub fn activate_running_instance(open_settings: bool) -> bool {
    let running = running_instance();
    if running.is_null() {
        return false;
    }
    if open_settings {
        // SAFETY: posting to a window that has gone away in the meantime fails
        // harmlessly.
        unsafe { PostMessageW(running, OPEN_SETTINGS, 0, 0) };
    }
    true
}

/// Asks a running Vinput to exit and waits until it has. Returns whether no
/// copy is running any more.
#[must_use]
pub fn stop_running_instance() -> bool {
    let started = Instant::now();
    loop {
        let running = running_instance();
        if running.is_null() {
            return true;
        }
        if started.elapsed() > QUIT_TIMEOUT {
            return false;
        }
        // SAFETY: posting to a window that has gone away in the meantime fails
        // harmlessly.
        unsafe { PostMessageW(running, QUIT, 0, 0) };
        thread::sleep(LOOK_AGAIN);
    }
}

/// Whether a copy of Vinput is running.
#[must_use]
pub fn is_running() -> bool {
    !running_instance().is_null()
}

/// Registers the copy installed at `exe` with Windows as it would itself
/// when first started: the Start menu entry, and the entry that starts it at
/// sign-in unless the settings say otherwise. For a setup program that
/// installs without starting Vinput, which would otherwise leave nothing
/// behind to start it by. Failures are logged; the program is installed
/// either way.
pub fn register_installed(exe: &Path) {
    // SAFETY: COM is needed for the Start menu entry, on this thread, and
    // is left again once that is made, if it was entered here.
    let entered = unsafe {
        windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
        )
    };
    if let Err(err) = shortcut::ensure_for(exe) {
        log::write(&err.to_string());
    }
    if entered.is_ok() {
        // SAFETY: balances the call above, after the last use of COM.
        unsafe { windows::Win32::System::Com::CoUninitialize() };
    }
    if vinput_core::settings::Settings::load().autostart
        && let Err(err) = autostart::set_for(exe, true)
    {
        log::write(&err.to_string());
    }
}

/// Removes what a running Vinput registers with Windows: the entry that starts
/// it at sign-in and the Start menu entry. Used when uninstalling.
pub fn unregister() {
    if let Err(err) = autostart::set(false) {
        log::write(&err.to_string());
    }
    shortcut::remove();
}

/// Holds a named mutex so a second copy of Vinput can tell the first is
/// running. The mutex is let go of when this is dropped, which is the moment
/// the next copy may start.
pub(super) struct SingleInstance(HANDLE);

impl SingleInstance {
    /// Takes the place of the one running copy. Returns `None` if another
    /// copy is running and staying.
    ///
    /// A copy that still holds the mutex but has no window any more is on its
    /// way out: an update has asked it to exit and started this one in its
    /// place. It is waited for, for a moment, instead of taken for a copy
    /// that is running, or nothing would be running once it has gone.
    pub(super) fn acquire() -> Option<Self> {
        Self::acquire_when_free(
            &instance::named(SINGLE_INSTANCE_MUTEX),
            || !running_instance().is_null(),
            LEAVING_TIMEOUT,
        )
    }

    /// Takes the mutex called `name`, waiting up to `patience` for whoever
    /// holds it to let go, unless `staying` says that the holder is up and
    /// running.
    fn acquire_when_free(
        name: &str,
        staying: impl Fn() -> bool,
        patience: Duration,
    ) -> Option<Self> {
        let started = Instant::now();
        loop {
            if let Some(instance) = Self::try_acquire(name) {
                return Some(instance);
            }
            if staying() || started.elapsed() >= patience {
                return None;
            }
            thread::sleep(LOOK_AGAIN.min(patience));
        }
    }

    /// Takes the mutex called `name` if nobody holds it.
    fn try_acquire(name: &str) -> Option<Self> {
        let name = wide(name);
        // SAFETY: the name is NUL-terminated. A handle to a mutex that was
        // there already is closed at once: it is somebody else's.
        unsafe {
            let handle = CreateMutexW(null(), 0, name.as_ptr());
            if handle.is_null() {
                return None;
            }
            if GetLastError() == ERROR_ALREADY_EXISTS {
                CloseHandle(handle);
                return None;
            }
            Some(Self(handle))
        }
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        // SAFETY: the handle came from `CreateMutexW` and is closed once.
        unsafe { CloseHandle(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    /// A name no running Vinput has, and no other test.
    fn name(of: &str) -> String {
        format!("Local\\VinputTest-{}-{of}", std::process::id())
    }

    #[test]
    fn only_one_copy_holds_the_name_and_the_next_takes_it_when_it_is_free() {
        let name = name("one");
        let first = SingleInstance::try_acquire(&name);
        assert!(first.is_some());
        assert!(SingleInstance::try_acquire(&name).is_none());
        drop(first);
        assert!(SingleInstance::try_acquire(&name).is_some());
    }

    #[test]
    fn a_copy_that_is_running_is_not_waited_for() {
        let name = name("staying");
        let _running = SingleInstance::try_acquire(&name);
        let started = Instant::now();
        let second = SingleInstance::acquire_when_free(&name, || true, Duration::from_secs(30));
        assert!(second.is_none());
        assert!(started.elapsed() < Duration::from_secs(10));
        // Nor is one that never leaves waited for longer than the patience.
        let second = SingleInstance::acquire_when_free(&name, || false, Duration::from_millis(50));
        assert!(second.is_none());
    }

    #[test]
    fn a_copy_that_is_on_its_way_out_is_waited_for() {
        let name = name("leaving");
        let (held, holding) = mpsc::channel();
        let (leave, leaving) = mpsc::channel::<()>();
        let theirs = name.clone();
        let old = thread::spawn(move || {
            let instance = SingleInstance::try_acquire(&theirs);
            let _ = held.send(instance.is_some());
            let _ = leaving.recv_timeout(Duration::from_secs(30));
            drop(instance);
        });
        assert_eq!(holding.recv_timeout(Duration::from_secs(30)), Ok(true));
        let waiting = thread::spawn(move || {
            SingleInstance::acquire_when_free(&name, || false, Duration::from_secs(30)).is_some()
        });
        thread::sleep(LOOK_AGAIN);
        let _ = leave.send(());
        assert!(old.join().is_ok());
        assert_eq!(waiting.join().ok(), Some(true));
    }
}
