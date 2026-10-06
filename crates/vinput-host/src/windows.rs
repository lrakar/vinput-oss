//! The background process on Windows: one hidden window, and everything
//! that hangs off its message loop.
//!
//! [`Host`] owns every Win32 resource and every piece of state; the modules
//! below each add the part of it they are about:
//!
//! - [`mod@run`]: the message loop and the procedure of the hidden window
//! - [`instance`]: one Vinput at a time, and what a second copy asks of the first
//! - [`hotkeys`]: registering the keys, and letting go of them for Settings
//! - [`sessions`]: starting, watching and ending dictation sessions
//! - [`apiserve`]: carrying out what clients of the local API ask for
//! - [`tray`] and [`traymenu`]: the icon, its notifications and its menu
//! - [`children`]: the window processes, opened, shown pictures and let go of
//! - [`picture`]: gathering what a picture for the windows is drawn from
//! - [`commands`]: what the user did in a window, carried out
//! - [`picker`]: the file and folder pickers of Windows
//! - [`updates`]: looking for, downloading and handing over to a newer version
//! - [`pro`]: the account and the subscription of `VinPUT` Pro
//! - [`google`]: the setup of a Google service of one's own
//!
//! Only this thread touches the host. Other threads leave what they have
//! found where the host looks for it and post one of the messages below.
#![allow(unsafe_code)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "Win32 structure sizes, flags and coordinates are small values in fixed-width fields"
)]

use std::cell::Cell;
use std::ptr::{null, null_mut};
use std::sync::{Arc, Mutex};

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, KillTimer, PostMessageW, RegisterClassW, SetTimer, WM_APP,
    WNDCLASSW,
};

mod apiserve;
mod children;
mod commands;
mod google;
mod hotkeys;
mod instance;
mod picker;
mod picture;
mod pro;
mod run;
mod sessions;
mod tray;
mod traymenu;
mod updates;

pub use instance::{
    activate_running_instance, is_running, register_installed, show_error, stop_running_instance,
    unregister,
};
pub use run::{Launch, run};

pub(crate) use crate::win::{last_error, wide};

use self::children::Windows;
use self::google::Google;
use self::hotkeys::Hotkeys;
use self::instance::host_window_title;
use self::picker::{Pick, Picked};
use self::picture::Slow;
use self::pro::Pro;
use self::run::host_proc;
use self::tray::{About, Tray};
use self::updates::Updates;
use crate::finder::Finder;
use crate::indicator::{Indicator, Placement};
use crate::{autostart, shortcut};
use vinput_core::models::{self, Installer};
use vinput_core::session::{Controller, Target, Trigger};
use vinput_core::settings::Settings;
use vinput_core::{Result, api, log, update};
use vinput_view::{self as wire, Page};

const TRAY_CALLBACK: u32 = WM_APP + 1;
const TICK_TIMER_ID: usize = 1;
/// Redraw and key-release polling interval while a session runs: about 30 Hz.
const TICK_MS: u32 = 33;
const WINDOW_TIMER_ID: usize = 3;
/// How often an open window is shown how things stand, if that has changed:
/// about four times a second. The timer runs only while a window is open.
const WINDOW_MS: u32 = 250;
const UPDATE_TIMER_ID: usize = 4;
/// How often Vinput looks for a newer version while it runs: every six hours.
const UPDATE_CHECK_MS: u32 = 6 * 60 * 60 * 1000;
/// The class of the hidden window, whose procedure hears the tray icon.
const HOST_WINDOW_CLASS: &str = "VinputHost";
/// Posted by the hidden window when the tray icon was used; `wParam` is the
/// [`tray::TrayEvent`].
const TRAY_EVENT: u32 = WM_APP + 11;
/// Posted by the hidden window when Explorer has restarted and the tray icon
/// has to be added again.
const TRAY_RESTORE: u32 = WM_APP + 12;
/// Posted by the thread that reads a window process, when the window has said
/// something or is gone.
const WINDOW_SPOKE: u32 = WM_APP + 2;
/// Posted by the search for a model to add, when a step of it is done.
const FINDER_DONE: u32 = WM_APP + 3;
/// Posted by the thread a file or folder picker runs on, when it has closed.
const PICKED: u32 = WM_APP + 13;
/// Posted by the thread that downloads an update, when it is done.
const UPDATE_DOWNLOADED: u32 = WM_APP + 14;
/// Posted by a second copy to make the running one show its settings window.
const OPEN_SETTINGS: u32 = WM_APP + 4;
/// Posted by the local API thread when a client has sent a command.
const API_COMMAND: u32 = WM_APP + 5;
/// Posted by the setup program to make the running copy exit, so that it can
/// be replaced.
const QUIT: u32 = WM_APP + 6;
/// Posted by the thread that looked for a newer version, when it is done.
const UPDATE_CHECKED: u32 = WM_APP + 9;
/// Posted by the thread that fetched an engine build, when it is installed.
const ENGINE_READY: u32 = WM_APP + 10;
/// Posted by the thread of a session when that session has ended and what
/// it leaves behind is there to be passed on.
const SESSION_ENDED: u32 = WM_APP + 15;
/// Posted by a thread that asked the server of `VinPUT` Pro, when it has an
/// answer.
const PRO_HEARD: u32 = WM_APP + 16;
/// Posted by a thread that asked Google whether a key or a project works,
/// when it has the answer.
const GOOGLE_CHECKED: u32 = WM_APP + 17;

/// Owns every Win32 resource of the background process and releases them on drop.
struct Host {
    hwnd: HWND,
    settings: Settings,
    trigger: Trigger,
    /// The key that started the current session, watched for its release.
    held_vk: Option<u16>,
    /// The hotkeys Windows has given this thread.
    hotkeys: Hotkeys,
    /// The tray icon.
    tray: Tray,
    indicator: Option<Indicator>,
    /// The larger indicator at the bottom of the screen, for sessions whose text
    /// is not typed at the caret.
    screen_indicator: Option<Indicator>,
    /// Which of the two the running session shows.
    on_screen: bool,
    /// The mode of the latest session and the id an API client gave it, for
    /// the events about it.
    session: Option<(Target, Option<String>)>,
    ticking: bool,
    /// The windows that are open, each in a process of its own.
    windows: Windows,
    /// Whether a hotkey field in Settings has the keys: the hotkeys are let
    /// go of for as long as it does.
    capturing: bool,
    /// What the windows are to tell the user about the last thing they
    /// asked for.
    notice: Option<wire::Notice>,
    /// The search for a model to add.
    finder: Finder,
    slow: Slow,
    /// The picker of Windows that is open, if one is, and where its thread leaves
    /// what was picked.
    picking: Option<Pick>,
    picked: Arc<Mutex<Option<Picked>>>,
    /// The local API, if it is switched on and could be started.
    api: Option<api::Server>,
    /// Installs local speech models; outlives the windows.
    installer: Installer,
    updates: Updates,
    /// The account of `VinPUT` Pro.
    pro: Pro,
    /// The setup of a Google service of one's own.
    google: Google,
    /// The page a key press that could not dictate turned Settings to. Until
    /// that window closes, no press turns it there or brings it to the front
    /// again.
    led_to: Option<Page>,
    /// Whether what the chosen model needs on this PC, its files or the key
    /// of its service, was there when a key press last looked. What is
    /// there is not looked for at every press: only again once the settings
    /// have changed or a session has failed for want of it.
    found: Cell<bool>,
}

impl Host {
    fn new(settings: Settings) -> Result<Self> {
        let class = wide(&vinput_core::instance::named(HOST_WINDOW_CLASS));
        let title = wide(&host_window_title());
        // SAFETY: both strings are NUL-terminated and outlive the calls. The
        // window is top-level, so that Explorer's broadcast reaches it, and
        // never shown.
        let hwnd = unsafe {
            let instance = GetModuleHandleW(null());
            let registered = WNDCLASSW {
                lpfnWndProc: Some(host_proc),
                hInstance: instance,
                lpszClassName: class.as_ptr(),
                ..Default::default()
            };
            RegisterClassW(&raw const registered);
            CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                null_mut(),
                null_mut(),
                instance,
                null(),
            )
        };
        if hwnd.is_null() {
            return Err(last_error("creating the hidden window"));
        }
        let tray = match Tray::add(hwnd, &tray::tooltip(&settings, None)) {
            Ok(tray) => tray,
            Err(err) => {
                // SAFETY: the window was created above and is destroyed once.
                unsafe { DestroyWindow(hwnd) };
                return Err(err);
            }
        };
        let host = Self {
            hwnd,
            trigger: Trigger::default(),
            held_vk: None,
            hotkeys: Hotkeys::default(),
            tray,
            // The indicator is a nicety: dictation works without it.
            indicator: Indicator::new(Placement::Caret)
                .map_err(|err| log::write(&err.to_string()))
                .ok(),
            screen_indicator: Indicator::new(Placement::Screen)
                .map_err(|err| log::write(&err.to_string()))
                .ok(),
            on_screen: false,
            session: None,
            ticking: false,
            windows: Windows::default(),
            capturing: false,
            notice: None,
            // A window handle is an address that any thread may post to.
            finder: Finder::new({
                let window = hwnd as usize;
                move || {
                    // SAFETY: posting to a window that has gone away fails
                    // harmlessly.
                    unsafe { PostMessageW(window as HWND, FINDER_DONE, 0, 0) };
                }
            }),
            slow: Slow::default(),
            picking: None,
            picked: Arc::default(),
            // Like the indicator, the API is optional: dictation works without it.
            api: settings
                .api_enabled
                .then(|| apiserve::start_api(hwnd))
                .flatten(),
            installer: Installer::new(),
            updates: Updates::default(),
            pro: Pro::new(&settings),
            google: Google::default(),
            led_to: None,
            found: Cell::new(false),
            settings,
        };
        // Clears a stale "listening" left behind if a previous copy was killed.
        sessions::publish_state(false);
        Ok(host)
    }

    /// Registers the hotkeys and brings the startup entry in line with the
    /// settings. Creates the settings file on first run so it can be found.
    fn apply_startup_settings(&mut self) {
        self.register_hotkeys();
        self.apply_autostart();
        // Like the startup entry, the Start menu entry belongs to installed builds.
        if !cfg!(debug_assertions)
            && let Err(err) = shortcut::ensure()
        {
            log::write(&err.to_string());
        }
        if Settings::path().is_some_and(|path| !path.exists())
            && let Err(err) = self.settings.save()
        {
            log::write(&err.to_string());
        }
        // A window handle is an address that any thread may post to.
        let window = self.hwnd as usize;
        self.installer.on_engine_ready(move || {
            // SAFETY: posting to a window that has gone away fails harmlessly.
            unsafe { PostMessageW(window as HWND, ENGINE_READY, 0, 0) };
        });
        self.ensure_engine();
        // Whoever dictates with VinPUT Pro is asked about once, so that a
        // month whose hours are used up is known before the first key
        // press and costs it nothing.
        if self.settings.provider == vinput_core::stt::ProviderKind::Cloud {
            self.pro_ask(false);
        }
        // Setup programs of earlier updates have done their work, and so has
        // the program the last one put aside.
        update::clean_up();
        crate::install::tidy();
        if self.settings.check_updates
            && !cfg!(debug_assertions)
            && !vinput_core::instance::is_separate()
        {
            self.check_for_updates(false);
            // SAFETY: `hwnd` is our live window; the timer id is ours.
            unsafe { SetTimer(self.hwnd, UPDATE_TIMER_ID, UPDATE_CHECK_MS, None) };
        }
    }

    /// Fetches the engine build the settings ask for, if a local model is in
    /// use and that build is not here. Until it is, the model runs on the
    /// build that is; nothing is asked of the user. A model that comes as
    /// two files has an engine of one build, which came with it.
    fn ensure_engine(&self) {
        if models::wants_engine(self.settings.provider, &self.settings.model) {
            self.installer.ensure_engine(self.settings.use_gpu());
        }
    }

    /// Brings the entry that starts Vinput with Windows in line with the
    /// settings.
    fn set_autostart(&self) -> Result<()> {
        // A debug build would put its console window on screen at every sign-in.
        if self.settings.autostart && cfg!(debug_assertions) {
            return Ok(());
        }
        autostart::set(self.settings.autostart)
    }

    fn apply_autostart(&mut self) {
        if let Err(err) = self.set_autostart() {
            log::write(&err.to_string());
            self.tray.warn(
                About::Settings(Page::General),
                "Startup setting not applied",
                &err.to_string(),
            );
        }
    }

    /// Puts the model, the use of the graphics card or the microphone that
    /// was just chosen, in a window or in the tray menu, to work at once
    /// and keeps it.
    fn apply_choices(&mut self, chosen: Settings, controller: &mut dyn Controller) {
        if chosen == self.settings {
            return;
        }
        self.settings = chosen;
        self.persist();
        controller.configure(&self.settings, false);
        self.ensure_engine();
        self.refresh_tip();
        self.show_state();
    }

    fn persist(&mut self) {
        self.found.set(false);
        self.pro.prefer(&self.settings);
        // With a model chosen the welcome has done what it was for.
        if self.settings.has_model() {
            self.settings.first_run = false;
        }
        if let Err(err) = self.settings.save() {
            log::write(&err.to_string());
            self.tray
                .warn(About::Nothing, "Settings not saved", &err.to_string());
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        sessions::publish_state(false);
        self.hotkeys.unregister();
        self.hotkeys.hold_cancel(false);
        // The windows are closed, and their processes gone, before the
        // hidden window is: whoever waits for this copy to exit, such as
        // setup, finds nothing of it running once that window has gone.
        self.cancel_update();
        self.windows.finish();
        // The pipe of the local API is given up too: the copy that an update
        // starts next wants its name.
        self.api = None;
        self.indicator = None;
        self.screen_indicator = None;
        self.tray.remove();
        // SAFETY: the timers and the window are ours, and released once.
        unsafe {
            for timer in [TICK_TIMER_ID, WINDOW_TIMER_ID, UPDATE_TIMER_ID] {
                KillTimer(self.hwnd, timer);
            }
            DestroyWindow(self.hwnd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_messages_of_the_host_do_not_overlap() {
        let mut messages = [
            TRAY_CALLBACK,
            WINDOW_SPOKE,
            FINDER_DONE,
            OPEN_SETTINGS,
            API_COMMAND,
            QUIT,
            UPDATE_CHECKED,
            ENGINE_READY,
            TRAY_EVENT,
            TRAY_RESTORE,
            PICKED,
            UPDATE_DOWNLOADED,
            SESSION_ENDED,
            PRO_HEARD,
            GOOGLE_CHECKED,
        ];
        let count = messages.len();
        messages.sort_unstable();
        let mut distinct = messages.to_vec();
        distinct.dedup();
        assert_eq!(distinct.len(), count);
        let timers = [TICK_TIMER_ID, WINDOW_TIMER_ID, UPDATE_TIMER_ID];
        for (index, timer) in timers.iter().enumerate() {
            assert!(!timers[..index].contains(timer));
        }
    }
}
