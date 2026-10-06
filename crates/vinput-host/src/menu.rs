//! Pop-up menus: the tray menu and the lists inside it, such as the one of
//! microphones.
//!
//! A [`Menu`] is built item by item and shown with [`Menu::show`], which
//! returns the id of the item picked.
//!
//! The menus are Windows' own, drawn by Windows: on Windows 11 that gives
//! the rounded frame, the rounded highlight and its marks, and everywhere it
//! gives the keyboard, screen readers and high-contrast themes for free. All
//! that is done here is to let them be dark when the user has set Windows
//! dark, and to open them where a menu of a tray icon belongs.
#![allow(unsafe_code)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    reason = "menu ids and sizes are small numbers that Windows takes in either width"
)]

use std::ptr::null;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{HWND, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CheckMenuRadioItem, CreatePopupMenu, DestroyMenu, HMENU, MF_BYCOMMAND, MF_CHECKED,
    MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, PostMessageW, SetForegroundWindow,
    SetMenuDefaultItem, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_NONOTIFY, TPM_RETURNCMD,
    TPM_RIGHTALIGN, TPM_RIGHTBUTTON, TPM_TOPALIGN, TrackPopupMenu, WM_NULL,
};

use crate::windows::wide;
use vinput_core::audio::Microphone;

/// The gap left between a menu and the taskbar its icon is in, in 96-DPI
/// pixels, as Windows 11 leaves under its own.
const TASKBAR_GAP: i32 = 8;

/// How an item looks and behaves, beyond its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mark {
    /// A plain item.
    None,
    /// A tick: a switch that is on.
    Check,
    /// A dot: the one chosen of a group.
    Radio,
    /// Bold: what a click on the tray icon does.
    Default,
    /// Shown greyed out, for information; it cannot be picked.
    Disabled,
}

/// Whether a menu is light or dark. The numbers are what the theme library
/// calls these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "the host follows Windows; the fixed looks are for trying a menu out in both"
)]
pub(super) enum Theme {
    /// Dark if the user has set Windows to show apps dark, light otherwise.
    System = 1,
    Dark = 2,
    Light = 3,
}

/// A pop-up menu that is being built. It is destroyed when dropped, unless
/// it has become a submenu of another, which then owns it.
pub(super) struct Menu {
    handle: HMENU,
    theme: Theme,
}

impl Menu {
    /// An empty menu, or `None` if Windows is out of resources.
    pub(super) fn new() -> Option<Self> {
        Self::themed(Theme::System)
    }

    /// An empty menu of a given look, which is that of its submenus too
    /// when it is shown. `None` if Windows is out of resources.
    pub(super) fn themed(theme: Theme) -> Option<Self> {
        // SAFETY: no arguments.
        let handle = unsafe { CreatePopupMenu() };
        (!handle.is_null()).then_some(Self { handle, theme })
    }

    /// Adds an item. `id` must not be zero, which means "nothing picked".
    pub(super) fn item(&mut self, id: usize, text: &str, mark: Mark) -> &mut Self {
        let flags = match mark {
            Mark::Check => MF_STRING | MF_CHECKED,
            Mark::Disabled => MF_STRING | MF_GRAYED,
            Mark::None | Mark::Radio | Mark::Default => MF_STRING,
        };
        let text = wide(text);
        // SAFETY: the menu is live and the text is NUL-terminated; the menu
        // copies it.
        unsafe {
            AppendMenuW(self.handle, flags, id, text.as_ptr());
            match mark {
                Mark::Radio => {
                    let id = id as u32;
                    CheckMenuRadioItem(self.handle, id, id, id, MF_BYCOMMAND);
                }
                Mark::Default => {
                    SetMenuDefaultItem(self.handle, id as u32, 0);
                }
                _ => {}
            }
        }
        self
    }

    /// Adds a line between groups of items.
    pub(super) fn separator(&mut self) -> &mut Self {
        // SAFETY: the menu is live; a separator has no text.
        unsafe { AppendMenuW(self.handle, MF_SEPARATOR, 0, null()) };
        self
    }

    /// Adds `submenu` under `text`. This menu owns it from now on.
    pub(super) fn submenu(&mut self, text: &str, submenu: Self) -> &mut Self {
        let text = wide(text);
        // SAFETY: both menus are live and the text is NUL-terminated. Once
        // appended, the submenu is destroyed with this menu, so it must not
        // be destroyed on its own as well.
        let added = unsafe {
            AppendMenuW(
                self.handle,
                MF_POPUP,
                submenu.handle as usize,
                text.as_ptr(),
            )
        };
        if added != 0 {
            std::mem::forget(submenu);
        }
        self
    }

    /// Shows the menu at `x`, `y` in screen coordinates, waits until the
    /// user picks an item or dismisses the menu, and returns the id of the
    /// item picked, or zero.
    ///
    /// The point is where the user clicked, which for a tray icon is in the
    /// taskbar. The menu opens beside the taskbar rather than over it, on
    /// whichever edge of the screen that is, and from the point towards the
    /// middle of the screen.
    ///
    /// `owner` is one of our windows. It is brought to the front first, or
    /// the menu would neither take the keyboard nor close when the user
    /// clicks elsewhere.
    pub(super) fn show(&self, owner: HWND, x: i32, y: i32) -> usize {
        let (x, y, align) = place(x, y);
        apply(self.theme);
        // SAFETY: the menu and the window are live. The empty message posted
        // afterwards makes the menu close properly when it was opened from
        // the tray, as Windows asks of tray menus.
        let chosen = unsafe {
            SetForegroundWindow(owner);
            let chosen = TrackPopupMenu(
                self.handle,
                TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | align,
                x,
                y,
                0,
                owner,
                null(),
            );
            PostMessageW(owner, WM_NULL, 0, 0);
            chosen
        };
        usize::try_from(chosen).unwrap_or(0)
    }
}

impl Drop for Menu {
    fn drop(&mut self) {
        // SAFETY: the menu is live and owned by this value alone.
        unsafe { DestroyMenu(self.handle) };
    }
}

/// Where a menu asked for at a point on screen opens: the point to give
/// Windows and the `TPM_` flags that say which corner of the menu goes there.
fn place(x: i32, y: i32) -> (i32, i32, u32) {
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let (mut dpi, mut dpi_y) = (96u32, 96u32);
    // SAFETY: `info` has its size set and the out-pointers refer to live
    // locals. A monitor that has just been unplugged fails both calls.
    let known = unsafe {
        let monitor = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
        if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &raw mut dpi, &raw mut dpi_y) != 0 {
            dpi = 96;
        }
        GetMonitorInfoW(monitor, &raw mut info) != 0
    };
    if known {
        let gap = TASKBAR_GAP * dpi.clamp(96, 960) as i32 / 96;
        anchor(x, y, &info.rcMonitor, &info.rcWork, gap)
    } else {
        // Windows still keeps the menu on a screen.
        (x, y, TPM_LEFTALIGN | TPM_TOPALIGN)
    }
}

/// Places a menu for a click at `x`, `y` on a monitor of which the taskbar
/// leaves `work` free.
///
/// On each side where there is a taskbar, the point is kept `gap` away from
/// it, so that the menu floats beside the taskbar. The menu then opens from
/// the point towards the middle of the monitor.
fn anchor(x: i32, y: i32, monitor: &RECT, work: &RECT, gap: i32) -> (i32, i32, u32) {
    // No gap at an edge of the screen without a taskbar.
    let inset = |free: i32, edge: i32, gap: i32| if free == edge { free } else { free + gap };
    let left = inset(work.left, monitor.left, gap);
    let top = inset(work.top, monitor.top, gap);
    let right = inset(work.right, monitor.right, -gap);
    let bottom = inset(work.bottom, monitor.bottom, -gap);
    let x = x.min(right).max(left);
    let y = y.min(bottom).max(top);
    let across = if x > work.left + (work.right - work.left) / 2 {
        TPM_RIGHTALIGN
    } else {
        TPM_LEFTALIGN
    };
    let down = if y > work.top + (work.bottom - work.top) / 2 {
        TPM_BOTTOMALIGN
    } else {
        TPM_TOPALIGN
    };
    (x, y, across | down)
}

/// Tells Windows which look this program's menus have from now on.
///
/// Windows offers this only through two unnamed functions of the theme
/// library, which Explorer and Notepad use. Where they are missing, menus
/// keep the light look.
fn apply(theme: Theme) {
    type SetMode = unsafe extern "system" fn(i32) -> i32;
    type Flush = unsafe extern "system" fn();
    static FUNCTIONS: OnceLock<Option<(SetMode, Flush)>> = OnceLock::new();
    let functions = FUNCTIONS.get_or_init(|| {
        let library = wide("uxtheme.dll");
        // SAFETY: the library name is NUL-terminated. The functions are
        // looked up by ordinal, 135 `SetPreferredAppMode` and 136
        // `FlushMenuThemes`, and are given the signatures they have had
        // since Windows 10 version 1903. Before that, 135 took a single
        // `BOOL` that allowed dark menus, which every look passed to it
        // does as well.
        unsafe {
            let theme = LoadLibraryW(library.as_ptr());
            if theme.is_null() {
                return None;
            }
            let set_mode = GetProcAddress(theme, 135 as _)?;
            let flush = GetProcAddress(theme, 136 as _)?;
            Some((
                std::mem::transmute::<unsafe extern "system" fn() -> isize, SetMode>(set_mode),
                std::mem::transmute::<unsafe extern "system" fn() -> isize, Flush>(flush),
            ))
        }
    });
    if let Some((set_mode, flush)) = functions {
        // SAFETY: the two functions described above. Flushing makes the next
        // menu take the look asked for, and the user's choice of light or
        // dark as it is now, rather than what the last menu was drawn with.
        unsafe {
            set_mode(theme as i32);
            flush();
        }
    }
}

/// The first entry of a list of microphones: whichever one Windows uses by
/// default.
const DEFAULT_MICROPHONE: &str = "System default";

/// The list of microphones to choose from, with the one in `current`
/// marked, numbered from `first` on. The menu ids are `first` for the
/// default one and `first + 1 + index` for the connected ones.
pub(super) fn microphones(connected: &[Microphone], current: &str, first: usize) -> Option<Menu> {
    let mut menu = Menu::new()?;
    let mark = |chosen: bool| if chosen { Mark::Radio } else { Mark::None };
    menu.item(first, DEFAULT_MICROPHONE, mark(current.is_empty()));
    if !connected.is_empty() {
        menu.separator();
    }
    for (index, microphone) in connected.iter().enumerate() {
        menu.item(
            first + 1 + index,
            &microphone.name,
            mark(microphone.id == current),
        );
    }
    Some(menu)
}

/// The microphone a menu built by [`microphones`] from `first` on says was
/// picked, as its identifier: empty for the default one. `None` if nothing
/// was picked.
pub(super) fn picked_microphone(
    item: usize,
    connected: &[Microphone],
    first: usize,
) -> Option<String> {
    match item.checked_sub(first)? {
        0 => Some(String::new()),
        index => connected
            .get(index - 1)
            .map(|microphone| microphone.id.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A screen of 1920 by 1080.
    const SCREEN: RECT = RECT {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1080,
    };

    #[test]
    fn a_microphone_item_maps_back_to_its_device() {
        let connected = [
            Microphone {
                id: "a".into(),
                name: "A".into(),
            },
            Microphone {
                id: "b".into(),
                name: "B".into(),
            },
        ];
        assert_eq!(picked_microphone(200, &connected, 200), Some(String::new()));
        assert_eq!(picked_microphone(201, &connected, 200), Some("a".into()));
        assert_eq!(picked_microphone(202, &connected, 200), Some("b".into()));
        // Nothing picked, or an item that is not a microphone.
        assert_eq!(picked_microphone(0, &connected, 200), None);
        assert_eq!(picked_microphone(203, &connected, 200), None);
        assert_eq!(picked_microphone(7, &connected, 200), None);
    }

    #[test]
    fn a_tray_menu_floats_above_a_taskbar_at_the_bottom() {
        let work = RECT {
            bottom: 1032,
            ..SCREEN
        };
        // The pointer is on an icon in the taskbar, near the right end.
        assert_eq!(
            anchor(1700, 1060, &SCREEN, &work, 8),
            (1700, 1024, TPM_RIGHTALIGN | TPM_BOTTOMALIGN)
        );
        // At the very right there is no taskbar to keep away from.
        assert_eq!(
            anchor(1919, 1060, &SCREEN, &work, 8),
            (1919, 1024, TPM_RIGHTALIGN | TPM_BOTTOMALIGN)
        );
    }

    #[test]
    fn a_tray_menu_opens_beside_a_taskbar_on_any_other_edge() {
        let top = RECT { top: 48, ..SCREEN };
        assert_eq!(
            anchor(1700, 20, &SCREEN, &top, 8),
            (1700, 56, TPM_RIGHTALIGN | TPM_TOPALIGN)
        );
        let left = RECT { left: 60, ..SCREEN };
        assert_eq!(
            anchor(30, 1000, &SCREEN, &left, 8),
            (68, 1000, TPM_LEFTALIGN | TPM_BOTTOMALIGN)
        );
        let right = RECT {
            right: 1860,
            ..SCREEN
        };
        assert_eq!(
            anchor(1900, 100, &SCREEN, &right, 8),
            (1852, 100, TPM_RIGHTALIGN | TPM_TOPALIGN)
        );
    }

    #[test]
    fn a_menu_asked_for_inside_the_screen_opens_at_the_point() {
        let work = RECT {
            bottom: 1032,
            ..SCREEN
        };
        // An icon in the overflow flyout, or a click in a window.
        assert_eq!(
            anchor(300, 200, &SCREEN, &work, 8),
            (300, 200, TPM_LEFTALIGN | TPM_TOPALIGN)
        );
        assert_eq!(
            anchor(1700, 900, &SCREEN, &work, 8),
            (1700, 900, TPM_RIGHTALIGN | TPM_BOTTOMALIGN)
        );
    }

    #[test]
    fn a_menu_opens_rightly_on_a_monitor_left_of_the_main_one() {
        let monitor = RECT {
            left: -2560,
            top: 0,
            right: 0,
            bottom: 1440,
        };
        let work = RECT {
            bottom: 1392,
            ..monitor
        };
        assert_eq!(
            anchor(-100, 1420, &monitor, &work, 12),
            (-100, 1380, TPM_RIGHTALIGN | TPM_BOTTOMALIGN)
        );
        assert_eq!(
            anchor(-2500, 1420, &monitor, &work, 12),
            (-2500, 1380, TPM_LEFTALIGN | TPM_BOTTOMALIGN)
        );
    }
}
