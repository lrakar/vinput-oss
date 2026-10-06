//! The message loop of the background process, and the procedure of its
//! hidden window.
//!
//! Everything the host does happens on the thread that calls [`run`]: a
//! hotkey, a timer, a click on the tray icon and a word from another thread
//! each arrive as a message, and the loop hands each to the part of the host
//! it belongs to. Other threads never touch the host; they leave what they
//! have found where the host looks for it and post a message to wake it.
#![allow(unsafe_code)]

use std::ptr::null_mut;
use std::sync::atomic::{AtomicU32, Ordering};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, DispatchMessageW, GetMessageW, MSG, PostMessageW, RegisterWindowMessageW,
    TranslateMessage, WM_APP, WM_HOTKEY, WM_TIMER,
};

use super::instance::{SingleInstance, activate_running_instance};
use super::tray::{TrayEvent, tray_event};
use super::{
    API_COMMAND, ENGINE_READY, FINDER_DONE, GOOGLE_CHECKED, Host, OPEN_SETTINGS, PICKED, PRO_HEARD,
    QUIT, SESSION_ENDED, TRAY_CALLBACK, TRAY_EVENT, TRAY_RESTORE, UPDATE_CHECKED,
    UPDATE_DOWNLOADED, UPDATE_TIMER_ID, WINDOW_SPOKE, WINDOW_TIMER_ID,
};
use crate::win::{last_error, wide};
use vinput_core::session::Controller;
use vinput_core::settings::Settings;
use vinput_core::{Result, log};
use vinput_view::Page;

/// How a copy of Vinput was started, which decides what the user sees of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launch {
    /// With Windows, or by anything else that wants it quietly in the tray.
    Background,
    /// By the user, from the Start menu: the settings window is shown.
    Settings,
    /// By the setup program, after an update the user asked for. The copy
    /// they were using has just closed under their eyes, so this one shows
    /// itself: the About page, which names the new version, and a
    /// notification that says the update is in.
    Updated,
}

impl Launch {
    /// Whether this launch puts a window in front of the user.
    #[must_use]
    pub fn shows_a_window(self) -> bool {
        self != Self::Background
    }
}

/// Runs the background host on the calling thread until the user picks Exit.
///
/// # Errors
///
/// Returns [`vinput_core::Error::Host`] if the window or tray icon cannot be created, or if the
/// message loop fails. A hotkey that is already taken is not an error: the user is
/// told and can pick another. If another copy of Vinput is already running, this
/// returns `Ok` straight away.
///
/// `launch` says what the user sees once Vinput is up: nothing, the settings
/// window, or that an update has been installed.
pub fn run(settings: Settings, controller: &mut dyn Controller, launch: Launch) -> Result<()> {
    // A second copy has nothing to do: the first one already owns the hotkeys.
    // Declared before the host, so that it is let go of after everything
    // the host owns has gone.
    let Some(_instance) = SingleInstance::acquire() else {
        log::write("Vinput is already running");
        // The first one was not up yet when this one looked for it.
        if launch.shows_a_window() {
            let _ = activate_running_instance(true);
        }
        return Ok(());
    };
    // SAFETY: plain calls made once, before any window exists. Per-monitor DPI
    // awareness makes every coordinate below a real pixel. COM is needed on this
    // thread for what it asks of the shell, such as the Start menu entry, and
    // stays initialized for the life of the process. The caret is looked for
    // on a thread of its own, which enters COM for itself.
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
        );
        // Explorer broadcasts this when it (re)starts and has lost our icon.
        TASKBAR_CREATED.store(
            RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
            Ordering::Relaxed,
        );
    }

    let mut host = Host::new(settings)?;
    host.hear_sessions_end(controller);
    host.apply_startup_settings();
    match launch {
        Launch::Background => {}
        Launch::Settings => {
            // Someone who has just installed is welcomed. Anyone else
            // without a model has one to choose before anything works.
            let page = if host.settings.owes_welcome() {
                Page::Welcome
            } else if host.settings.has_model() {
                Page::General
            } else {
                Page::Models
            };
            host.open_settings(page);
        }
        Launch::Updated => host.show_updated(),
    }
    loop {
        let mut msg = MSG::default();
        // SAFETY: `msg` is a valid, writable `MSG`.
        match unsafe { GetMessageW(&raw mut msg, null_mut(), 0, 0) } {
            0 => return Ok(()),
            -1 => return Err(last_error("reading the message queue")),
            _ => {}
        }
        // Everything of ours is posted to the hidden window. The same number
        // in a message for another window of this thread, such as the
        // indicator, means something else and goes to that window.
        if msg.hwnd != host.hwnd && msg.message != WM_HOTKEY {
            Host::dispatch(&msg);
            continue;
        }
        match msg.message {
            // Registered for the thread, so it comes without a window.
            WM_HOTKEY => host.on_hotkey(msg.wParam, controller),
            WM_TIMER => match msg.wParam {
                WINDOW_TIMER_ID => host.on_window_tick(controller),
                UPDATE_TIMER_ID => host.check_for_updates(false),
                _ => host.on_tick(controller),
            },
            TRAY_EVENT => match TrayEvent::from_code(msg.wParam) {
                Some(TrayEvent::Menu) => {
                    let chosen = host.show_menu();
                    // What other threads said while the menu was open went
                    // to the procedure of the hidden window instead of
                    // here; it is said again, and heard after the pick.
                    host.replay_missed();
                    if host.on_menu(chosen, controller) {
                        return Ok(());
                    }
                }
                Some(TrayEvent::Open) => host.open_home(),
                Some(TrayEvent::Notification) => host.on_notification_click(),
                None => {}
            },
            TRAY_RESTORE => host.tray.restore(),
            WINDOW_SPOKE => host.on_windows(controller),
            FINDER_DONE => host.on_finder_done(controller),
            PICKED => host.on_picked(controller),
            UPDATE_DOWNLOADED => {
                if host.on_update_downloaded() {
                    return Ok(());
                }
            }
            OPEN_SETTINGS => host.open_home(),
            API_COMMAND => host.on_api_command(controller),
            UPDATE_CHECKED => host.on_update_checked(),
            // The model in use moves to the engine that has just arrived.
            ENGINE_READY => controller.configure(&host.settings, true),
            SESSION_ENDED => host.on_session_ended(controller),
            PRO_HEARD => host.on_pro_heard(controller),
            GOOGLE_CHECKED => host.on_google_checked(),
            QUIT => return Ok(()),
            // The tray icon and Explorer reach the hidden window through its
            // procedure, which turns what they say into `TRAY_EVENT` and
            // `TRAY_RESTORE`.
            _ => Host::dispatch(&msg),
        }
    }
}

/// The message Explorer broadcasts when it has restarted, registered at
/// startup.
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

/// The messages for the loop that reached the procedure of the hidden window
/// instead, a bit each, counted from `WM_APP`.
static MISSED: AtomicU32 = AtomicU32::new(0);

/// The messages another thread, or another copy of Vinput, wakes the loop
/// with. Each says "look at what was left for you", so hearing one twice
/// does no harm, and missing one would leave something undone for good.
const WAKES: [u32; 12] = [
    WINDOW_SPOKE,
    FINDER_DONE,
    PICKED,
    UPDATE_DOWNLOADED,
    OPEN_SETTINGS,
    API_COMMAND,
    UPDATE_CHECKED,
    ENGINE_READY,
    SESSION_ENDED,
    PRO_HEARD,
    GOOGLE_CHECKED,
    QUIT,
];

/// The bit that stands for `message` among the missed ones, if it is one
/// that must not be lost.
fn wake_bit(message: u32) -> Option<u32> {
    WAKES
        .contains(&message)
        .then(|| 1u32.checked_shl(message - WM_APP))
        .flatten()
}

/// The messages a set of bits stands for, in the order of [`WAKES`].
fn wakes_in(missed: u32) -> impl Iterator<Item = u32> {
    WAKES
        .into_iter()
        .filter(move |message| wake_bit(*message).is_some_and(|bit| missed & bit != 0))
}

/// The procedure of the hidden window.
///
/// Explorer sends what happens to the tray icon straight to this procedure
/// rather than through the message queue, so the host loop would never see
/// it. The clicks that matter are posted back to the queue as `TRAY_EVENT`,
/// where the loop handles them; the constant stream of mouse moves over the
/// icon is dropped here.
///
/// While the tray menu is open its own loop reads the queue and dispatches
/// what it finds back here. A click on the icon is dropped then: one menu at
/// a time. What another thread posted to wake the host is remembered, and
/// posted again when the menu has closed.
pub(super) unsafe extern "system" fn host_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: called by the system with our live window.
    unsafe {
        if message == TRAY_CALLBACK {
            if let Some(event) = tray_event(lparam) {
                PostMessageW(hwnd, TRAY_EVENT, event as usize, 0);
            }
            return 0;
        }
        if let Some(bit) = wake_bit(message) {
            MISSED.fetch_or(bit, Ordering::Relaxed);
            return 0;
        }
        let restarted = TASKBAR_CREATED.load(Ordering::Relaxed);
        if restarted != 0 && message == restarted {
            PostMessageW(hwnd, TRAY_RESTORE, 0, 0);
            return 0;
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }
}

impl Host {
    /// Posts again what reached the procedure of the hidden window while
    /// the tray menu was open, for the loop to hear after all.
    fn replay_missed(&self) {
        for message in wakes_in(MISSED.swap(0, Ordering::Relaxed)) {
            // SAFETY: `hwnd` is our live window.
            unsafe { PostMessageW(self.hwnd, message, 0, 0) };
        }
    }

    /// Hands a message nobody above asked for to its window procedure.
    fn dispatch(msg: &MSG) {
        // SAFETY: `msg` was filled in by `GetMessageW`.
        unsafe {
            TranslateMessage(msg);
            DispatchMessageW(msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_wakes_the_host_while_the_menu_is_open_is_remembered() {
        let mut missed = 0;
        for message in [QUIT, WINDOW_SPOKE, QUIT, ENGINE_READY] {
            missed |= wake_bit(message).unwrap_or_default();
        }
        // Each once, however often it came, in the order the loop knows.
        assert_eq!(
            wakes_in(missed).collect::<Vec<_>>(),
            [WINDOW_SPOKE, ENGINE_READY, QUIT]
        );
        assert_eq!(wakes_in(0).count(), 0);
        // Every wake has a bit of its own.
        let all = WAKES.iter().fold(0, |bits, message| {
            bits | wake_bit(*message).unwrap_or_default()
        });
        assert_eq!(all.count_ones() as usize, WAKES.len());
    }

    #[test]
    fn clicks_on_the_tray_icon_are_not_replayed() {
        // A click while the menu is open is dropped: one menu at a time.
        for message in [
            TRAY_CALLBACK,
            TRAY_EVENT,
            TRAY_RESTORE,
            WM_TIMER,
            WM_HOTKEY,
            0,
        ] {
            assert_eq!(wake_bit(message), None, "{message}");
        }
    }
}
