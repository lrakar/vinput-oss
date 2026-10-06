//! The tray icon: what it looks like, what its tooltip says, the
//! notifications that come out of it, and what the user did to it.
//!
//! A [`Tray`] owns the icon from the moment it is added until it is removed.
//! Explorer tells the hidden window about clicks through its procedure, which
//! hands them to [`tray_event`]; what a click on a notification opens is
//! remembered here as an [`About`], because Windows only says that it was
//! clicked, not which one it was.
//!
//! Not everything that asks for a notification gets one on screen. A copy
//! that keeps to itself (`vinput_core::instance::is_quiet`) shows none at
//! all. And the warning that a session failed is not shown again within
//! [`REPEAT_AFTER`] if it says the same: a failure that comes back with
//! every press would otherwise be said over and over. Every other
//! notification answers something the user did, or tells of something that
//! happens once, and is always shown. What is not shown is written to the
//! log.
#![allow(unsafe_code)]

use std::ptr::null;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Gdi::{CreateBitmap, DeleteObject};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    NIF_GUID, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIIF_WARNING, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, DestroyIcon, GetSystemMetrics, HICON, ICONINFO, IMAGE_ICON, LoadImageW,
    SM_CXSMICON, WM_CONTEXTMENU, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_RBUTTONUP, WM_USER,
};
use windows_sys::core::GUID;

use super::{Host, TRAY_CALLBACK};
use crate::draw;
use crate::win::copy_text;
use vinput_core::settings::Settings;
use vinput_core::{Error, Result, log, models};
use vinput_view::Page;

const TRAY_ICON_ID: u32 = 1;
/// The tray tells us with this that the user clicked a notification.
const NIN_BALLOONUSERCLICK: u32 = WM_USER + 5;
const ICON_DATA_SIZE: u32 = size_of::<NOTIFYICONDATAW>() as u32;
/// What the tray icon of an installed Vinput is known to Windows by:
/// `95f2e894-15b1-44ca-bb70-36111c0f31c4`.
const TRAY_ICON_GUID: GUID = GUID {
    data1: 0x95f2_e894,
    data2: 0x15b1,
    data3: 0x44ca,
    data4: [0xbb, 0x70, 0x36, 0x11, 0x1c, 0x0f, 0x31, 0xc4],
};
/// What every change to the icon keeps: its message, its picture and its
/// tooltip.
const KEPT: u32 = NIF_MESSAGE | NIF_ICON | NIF_TIP;
/// How long the warning that a session failed is not shown again, with
/// the same words, after it was shown.
const REPEAT_AFTER: Duration = Duration::from_secs(60);

/// What kind of notification one is, which decides whether showing it again
/// can be left out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// It answers something the user did, or tells of something that
    /// happens once: always shown.
    Answer,
    /// A session failed, which it may do again with every press: not
    /// repeated in the same words within [`REPEAT_AFTER`].
    Failure,
}

/// The failure shown last, and when: what keeps the same one from being
/// shown over and over.
#[derive(Debug, Default)]
struct Shown {
    last: Option<(String, String, Instant)>,
}

impl Shown {
    /// Whether a failure with this title and text was shown less than
    /// [`REPEAT_AFTER`] before `now`.
    fn is_repeat(&self, title: &str, text: &str, now: Instant) -> bool {
        self.last
            .as_ref()
            .is_some_and(|(shown_title, shown_text, at)| {
                shown_title == title
                    && shown_text == text
                    && now.saturating_duration_since(*at) < REPEAT_AFTER
            })
    }

    /// Remembers that this failure was shown at `now`. Only one that
    /// Windows really took is remembered; one that was held back leaves the
    /// time of the showing as it was, so that a failure which keeps
    /// happening does come back once in a while.
    fn remember(&mut self, title: &str, text: &str, now: Instant) {
        self.last = Some((title.to_owned(), text.to_owned(), now));
    }
}

/// Whether a notification reaches the screen, and why not if it does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Showing {
    Shown,
    /// This copy keeps to itself.
    Quiet,
    /// The same failure was shown a moment ago.
    Repeated,
}

/// Decides whether a notification is shown. `quiet` is whether this copy
/// keeps to itself; such a copy shows nothing.
fn showing(
    quiet: bool,
    kind: Kind,
    shown: &Shown,
    title: &str,
    text: &str,
    now: Instant,
) -> Showing {
    if quiet {
        Showing::Quiet
    } else if kind == Kind::Failure && shown.is_repeat(title, text, now) {
        Showing::Repeated
    } else {
        Showing::Shown
    }
}

/// The flag that goes with every change to an icon that is known by its
/// identifier, and nothing for one known by its window.
fn identity(tray: &NOTIFYICONDATAW) -> u32 {
    if tray.guidItem.data1 == 0 {
        0
    } else {
        NIF_GUID
    }
}

/// What the user did to the tray icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TrayEvent {
    /// A right click, or the menu key: the menu.
    Menu = 1,
    /// A click: Settings.
    Open = 2,
    /// A notification was clicked.
    Notification = 3,
}

impl TrayEvent {
    /// The event a `wParam` of the message that carries one stands for.
    pub(super) fn from_code(code: usize) -> Option<Self> {
        [Self::Menu, Self::Open, Self::Notification]
            .into_iter()
            .find(|event| *event as usize == code)
    }
}

/// Decodes a tray callback: the low word of `lParam` is the mouse message.
pub(super) fn tray_event(lparam: isize) -> Option<TrayEvent> {
    match u32::try_from(lparam & 0xFFFF).ok()? {
        WM_RBUTTONUP | WM_CONTEXTMENU => Some(TrayEvent::Menu),
        WM_LBUTTONUP | WM_LBUTTONDBLCLK => Some(TrayEvent::Open),
        NIN_BALLOONUSERCLICK => Some(TrayEvent::Notification),
        _ => None,
    }
}

/// What a click on a notification opens. It is remembered for as long as the
/// notification may be on screen or in the notification center, which is
/// until the next one replaces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum About {
    /// Nothing to act on.
    Nothing,
    /// A newer version: the update window.
    Update,
    /// Something to change in Settings, on this page.
    Settings(Page),
    /// A saved voice memo: its folder.
    Memo,
}

/// The tray tooltip: the keys, and the model that is at work. `spare` is
/// the model on this PC that serves in place of `VinPUT` Pro while the
/// month's hours are used up.
pub(super) fn tooltip(settings: &Settings, spare: Option<&str>) -> String {
    let keys = match settings.hotkey_enter {
        Some(enter) => format!(
            "Vinput: {} to dictate, {enter} to dictate and send",
            settings.hotkey
        ),
        None => format!("Vinput: {} to dictate", settings.hotkey),
    };
    match models::name_in_use(settings.provider, &settings.model) {
        Some(model) if spare.is_some() => format!(
            "{keys}\n{model} hours used up, using {}",
            spare.unwrap_or_default()
        ),
        Some(model) => format!("{keys}\nModel: {model}"),
        None => format!("{keys}\nNo speech model chosen yet"),
    }
}

/// The tray icon, from when it is added until it is removed or dropped.
pub(super) struct Tray {
    /// What Windows was told about the icon last; `None` once it is removed.
    data: Option<NOTIFYICONDATAW>,
    icon_idle: HICON,
    icon_listening: HICON,
    /// What the latest notification was about.
    about: About,
    /// The failure shown last, not to be shown again right away.
    shown: Shown,
}

impl Tray {
    /// Adds the icon for the hidden window `hwnd`, with `tip` as its tooltip.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Host`] if Windows does not take the icon.
    pub(super) fn add(hwnd: HWND, tip: &str) -> Result<Self> {
        // The size is read once: an icon made for the scaling Windows had at
        // startup is shown scaled by Explorer if that changes later. Making
        // the icons anew on a change of display wants a look at what
        // `GetSystemMetrics` reports then for a process that is aware of
        // every monitor, on a real second display.
        // SAFETY: no arguments beyond a metric index.
        let icon_size = u32::try_from(unsafe { GetSystemMetrics(SM_CXSMICON) })
            .unwrap_or(16)
            .max(16);
        let mut tray = Self {
            data: None,
            icon_idle: make_icon(icon_size, false),
            icon_listening: make_icon(icon_size, true),
            about: About::Nothing,
            shown: Shown::default(),
        };
        let mut data = NOTIFYICONDATAW {
            cbSize: ICON_DATA_SIZE,
            hWnd: hwnd,
            uID: TRAY_ICON_ID,
            uFlags: KEPT,
            uCallbackMessage: TRAY_CALLBACK,
            hIcon: tray.icon_idle,
            ..Default::default()
        };
        copy_text(&mut data.szTip, tip);
        // An installed Vinput names its icon by an identifier of its own
        // rather than by this window. A copy that was killed leaves its icon
        // behind, dead to every click; by the identifier the next copy takes
        // that icon away and puts a live one in its place. Windows ties the
        // identifier to the path of the program, so builds run from elsewhere
        // do without it, and so does a separate copy, which would take the
        // icon of the installed one away.
        let mut added = false;
        if !cfg!(debug_assertions) && !vinput_core::instance::is_separate() {
            let named = NOTIFYICONDATAW {
                uFlags: data.uFlags | NIF_GUID,
                guidItem: TRAY_ICON_GUID,
                ..data
            };
            // SAFETY: `named` is fully initialized with a correct `cbSize`.
            // Deleting an icon that is not there fails harmlessly.
            unsafe {
                Shell_NotifyIconW(NIM_DELETE, &raw const named);
                added = Shell_NotifyIconW(NIM_ADD, &raw const named) != 0;
            }
            if added {
                data = named;
            }
        }
        // SAFETY: `data` is fully initialized with a correct `cbSize`.
        if !added && unsafe { Shell_NotifyIconW(NIM_ADD, &raw const data) } == 0 {
            return Err(Error::Host("adding the tray icon failed".into()));
        }
        tray.data = Some(data);
        Ok(tray)
    }

    /// Shows whether Vinput is listening, and `tip` as the tooltip.
    pub(super) fn show(&mut self, listening: bool, tip: &str) {
        let icon = if listening {
            self.icon_listening
        } else {
            self.icon_idle
        };
        self.update(|data| {
            data.uFlags = KEPT;
            data.hIcon = icon;
            copy_text(&mut data.szTip, tip);
        });
    }

    /// Changes the tooltip and nothing else.
    pub(super) fn set_tip(&mut self, tip: &str) {
        self.update(|data| {
            data.uFlags = KEPT;
            copy_text(&mut data.szTip, tip);
        });
    }

    /// Shows a notification for information. `about` is what a click on it
    /// opens.
    pub(super) fn inform(&mut self, about: About, title: &str, text: &str) {
        self.notify(about, Kind::Answer, NIIF_INFO, title, text);
    }

    /// Shows a warning. `about` is what a click on it opens.
    pub(super) fn warn(&mut self, about: About, title: &str, text: &str) {
        self.notify(about, Kind::Answer, NIIF_WARNING, title, text);
    }

    /// Shows the warning that a session failed, unless the same one was
    /// shown within the last minute. `about` is what a click on it opens.
    pub(super) fn warn_of_failure(&mut self, about: About, title: &str, text: &str) {
        self.notify(about, Kind::Failure, NIIF_WARNING, title, text);
    }

    /// What the latest notification was about.
    pub(super) fn about(&self) -> About {
        self.about
    }

    /// Puts the icon back after Explorer has restarted and lost it.
    pub(super) fn restore(&mut self) {
        if let Some(data) = self.data.as_mut() {
            data.uFlags = KEPT | identity(data);
            // SAFETY: `data` is the structure the icon was first added with.
            unsafe { Shell_NotifyIconW(NIM_ADD, &raw const *data) };
        }
    }

    /// Takes the icon out of the tray. Doing so twice is harmless.
    pub(super) fn remove(&mut self) {
        if let Some(data) = self.data.take() {
            // SAFETY: `data` is the structure the icon was added with.
            unsafe { Shell_NotifyIconW(NIM_DELETE, &raw const data) };
        }
    }

    fn notify(&mut self, about: About, kind: Kind, icon: u32, title: &str, text: &str) {
        let quiet = vinput_core::instance::is_quiet();
        let now = Instant::now();
        match showing(quiet, kind, &self.shown, title, text, now) {
            Showing::Shown => {}
            Showing::Quiet => {
                log::write(&format!(
                    "not shown, this copy keeps to itself: {title}: {text}"
                ));
                return;
            }
            Showing::Repeated => {
                log::write(&format!("not shown again so soon: {title}: {text}"));
                return;
            }
        }
        self.about = about;
        let taken = self.update(|data| {
            data.uFlags = KEPT | NIF_INFO;
            data.dwInfoFlags = icon;
            copy_text(&mut data.szInfoTitle, title);
            copy_text(&mut data.szInfo, text);
        });
        // One that Windows did not take was not shown, and is not held
        // back the next time.
        if taken && kind == Kind::Failure {
            self.shown.remember(title, text, now);
        }
    }

    /// Changes what Windows knows about the icon. Returns whether Windows
    /// took the change.
    fn update(&mut self, change: impl FnOnce(&mut NOTIFYICONDATAW)) -> bool {
        let Some(data) = self.data.as_mut() else {
            return false;
        };
        change(data);
        data.uFlags |= identity(data);
        // SAFETY: `data` is the structure the icon was added with. If Windows
        // has lost the icon, which a restart of Explorer does, it is added
        // again as it should be now.
        unsafe {
            if Shell_NotifyIconW(NIM_MODIFY, &raw const *data) == 0
                && Shell_NotifyIconW(NIM_ADD, &raw const *data) == 0
            {
                log::write("updating the tray icon failed");
                return false;
            }
        }
        true
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.remove();
        // SAFETY: the icons were made in `add` and are destroyed once.
        unsafe {
            DestroyIcon(self.icon_idle);
            DestroyIcon(self.icon_listening);
        }
    }
}

impl Host {
    /// Opens what the notification the user clicked was about.
    pub(super) fn on_notification_click(&mut self) {
        match self.tray.about() {
            About::Nothing => {}
            About::Update => self.open_update_window(),
            About::Settings(page) => self.open_settings(page),
            About::Memo => self.open_memo_folder(),
        }
    }

    /// Brings the tooltip of the tray icon in line with the settings.
    pub(super) fn refresh_tip(&mut self) {
        let tip = tooltip(&self.settings, self.pro.carrying_with(&self.settings));
        self.tray.set_tip(&tip);
    }
}

/// Loads a tray icon: the logo, or the logo with a red dot while listening,
/// which are resources 1 and 2 of the executable. An executable built without
/// its resources gets the drawn artwork instead. Returns null if Windows refuses
/// that too, which shows as a blank icon rather than stopping the app.
fn make_icon(size: u32, listening: bool) -> HICON {
    let resource = if listening { 2 } else { 1 };
    // SAFETY: a small integer in place of the name selects a resource by id.
    let icon = unsafe {
        LoadImageW(
            GetModuleHandleW(null()),
            resource as _,
            IMAGE_ICON,
            size.cast_signed(),
            size.cast_signed(),
            0,
        )
    };
    if !icon.is_null() {
        return icon;
    }
    // Icons want straight alpha, the drawing code produces premultiplied.
    let pixels: Vec<u32> = draw::icon(size, listening)
        .into_iter()
        .map(draw::unpremultiply)
        .collect();
    let dimension = size.cast_signed();
    let mask = vec![0u8; (size as usize).div_ceil(16) * 2 * size as usize];
    // SAFETY: `pixels` holds `size * size` 32-bit values and `mask` holds `size`
    // word-aligned rows of 1-bit values, matching the dimensions passed. The
    // bitmaps are copied into the icon and can be deleted straight away.
    unsafe {
        let color = CreateBitmap(dimension, dimension, 1, 32, pixels.as_ptr().cast());
        let mask = CreateBitmap(dimension, dimension, 1, 1, mask.as_ptr().cast());
        let info = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&raw const info);
        DeleteObject(color);
        DeleteObject(mask);
        icon
    }
}

#[cfg(test)]
mod tests {
    use vinput_core::keys::Hotkey;

    use super::*;

    #[test]
    fn tray_clicks_open_settings_or_the_menu_and_mouse_moves_do_nothing() {
        let event = |message: u32| tray_event(message as isize).map(|event| event as usize);
        assert_eq!(event(WM_RBUTTONUP), Some(TrayEvent::Menu as usize));
        assert_eq!(event(WM_CONTEXTMENU), Some(TrayEvent::Menu as usize));
        assert_eq!(event(WM_LBUTTONUP), Some(TrayEvent::Open as usize));
        assert_eq!(
            event(NIN_BALLOONUSERCLICK),
            Some(TrayEvent::Notification as usize)
        );
        // WM_MOUSEMOVE, sent all the time while the pointer is over the icon.
        assert_eq!(event(0x0200), None);
        for code in [TrayEvent::Menu, TrayEvent::Open, TrayEvent::Notification] {
            assert_eq!(TrayEvent::from_code(code as usize), Some(code));
        }
        assert_eq!(TrayEvent::from_code(0), None);
    }

    /// Shows a failure at `now` as the tray does: if it is to be shown,
    /// and Windows takes it, it is remembered.
    fn fail(shown: &mut Shown, title: &str, text: &str, now: Instant, taken: bool) -> Showing {
        let showing = showing(false, Kind::Failure, shown, title, text, now);
        if showing == Showing::Shown && taken {
            shown.remember(title, text, now);
        }
        showing
    }

    #[test]
    fn the_same_failure_is_not_shown_again_within_a_minute() {
        let mut shown = Shown::default();
        let start = Instant::now();
        let at = |seconds: u64| start + Duration::from_secs(seconds);
        let (title, text) = ("Vinput stopped listening", "no connection");
        assert_eq!(fail(&mut shown, title, text, at(0), true), Showing::Shown);
        // It fails again with every press.
        for seconds in [0, 1, 20, 59] {
            assert_eq!(
                fail(&mut shown, title, text, at(seconds), true),
                Showing::Repeated,
                "{seconds}"
            );
        }
        // A minute after it was shown, not after it was last held back, it
        // is said once more, and then not again for another minute.
        assert_eq!(fail(&mut shown, title, text, at(60), true), Showing::Shown);
        assert_eq!(
            fail(&mut shown, title, text, at(119), true),
            Showing::Repeated
        );
        assert_eq!(fail(&mut shown, title, text, at(120), true), Showing::Shown);
    }

    #[test]
    fn another_failure_is_shown_at_once() {
        let mut shown = Shown::default();
        let now = Instant::now();
        let mut show = |title: &str, text: &str| fail(&mut shown, title, text, now, true);
        assert_eq!(
            show("Vinput stopped listening", "no connection"),
            Showing::Shown
        );
        // Another text and another title each make it another failure, and
        // so does the same run of words split elsewhere.
        assert_eq!(
            show("Vinput stopped listening", "no microphone"),
            Showing::Shown
        );
        assert_eq!(
            show("Vinput could not finish", "no microphone"),
            Showing::Shown
        );
        assert_eq!(
            show("Vinput could not", " finishno microphone"),
            Showing::Shown
        );
        // Only the one shown last is remembered: the first is new again.
        assert_eq!(
            show("Vinput stopped listening", "no connection"),
            Showing::Shown
        );
        assert_eq!(
            show("Vinput stopped listening", "no connection"),
            Showing::Repeated
        );
    }

    #[test]
    fn what_answers_the_user_is_shown_every_time() {
        // A press without a model, a memo that was saved, an update: said
        // as often as they happen, also in the same words, and also right
        // after a failure in those words.
        let mut shown = Shown::default();
        let now = Instant::now();
        let (title, text) = ("Choose a speech model first", "Click here to choose one.");
        for _ in 0..3 {
            assert_eq!(
                showing(false, Kind::Answer, &shown, title, text, now),
                Showing::Shown
            );
        }
        assert_eq!(fail(&mut shown, title, text, now, true), Showing::Shown);
        assert_eq!(fail(&mut shown, title, text, now, true), Showing::Repeated);
        assert_eq!(
            showing(false, Kind::Answer, &shown, title, text, now),
            Showing::Shown
        );
    }

    #[test]
    fn a_failure_windows_did_not_take_is_not_remembered_as_shown() {
        let mut shown = Shown::default();
        let now = Instant::now();
        let (title, text) = ("Vinput could not finish", "no connection");
        // Explorer was restarting: the notification never reached the screen.
        assert_eq!(fail(&mut shown, title, text, now, false), Showing::Shown);
        // So the next one is not held back.
        assert_eq!(fail(&mut shown, title, text, now, true), Showing::Shown);
        assert_eq!(fail(&mut shown, title, text, now, true), Showing::Repeated);
    }

    #[test]
    fn a_copy_that_keeps_to_itself_shows_no_notification_at_all() {
        let shown = Shown::default();
        let now = Instant::now();
        for kind in [Kind::Answer, Kind::Failure] {
            assert_eq!(
                showing(true, kind, &shown, "Voice memo saved", "a.mp3", now),
                Showing::Quiet
            );
        }
        assert_eq!(
            showing(
                false,
                Kind::Answer,
                &shown,
                "Voice memo saved",
                "a.mp3",
                now
            ),
            Showing::Shown
        );
    }

    #[test]
    fn the_tooltip_names_the_keys_and_says_when_no_model_is_chosen() {
        let mut settings = Settings::default();
        assert_eq!(
            tooltip(&settings, None),
            "Vinput: F1 to dictate, F2 to dictate and send\nNo speech model chosen yet"
        );
        settings.hotkey_enter = None;
        settings.hotkey = Hotkey {
            ctrl: true,
            ..Hotkey::plain(0x20)
        };
        assert!(tooltip(&settings, None).starts_with("Vinput: Ctrl+Space to dictate\n"));
        // Windows shows no more than this of a tooltip.
        assert!(tooltip(&settings, None).encode_utf16().count() < 128);
        // While the hours of VinPUT Pro are used up, it says what types.
        settings.provider = vinput_core::stt::ProviderKind::Cloud;
        settings.model = vinput_core::cloud::MODEL_ID.to_owned();
        let tip = tooltip(&settings, Some("Whisper large-v3 turbo (compact)"));
        assert!(
            tip.ends_with("\nVinPUT Pro hours used up, using Whisper large-v3 turbo (compact)"),
            "{tip}"
        );
        assert!(tip.encode_utf16().count() < 128);
    }
}
