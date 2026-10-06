//! The frame Windows draws around a window: its title bar colors, the
//! wallpaper tint behind the navigation pane, and the icons of the title bar
//! and the taskbar.
#![allow(unsafe_code)]

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMSBT_MAINWINDOW, DWMWA_CAPTION_COLOR, DWMWA_SYSTEMBACKDROP_TYPE,
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWINDOWATTRIBUTE, DwmExtendFrameIntoClientArea,
    DwmSetWindowAttribute,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, HICON, ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_DEFAULTCOLOR, LoadImageW, SM_CXICON,
    SM_CXSMICON, SYSTEM_METRICS_INDEX, SendMessageW, WM_SETICON,
};
use windows::core::{BOOL, PCWSTR};

use crate::scene::Showing;
use crate::theme::{Theme, metric};

use super::{APP_ICON, px};

/// What tells Windows to color a title bar the way it does by default.
const DEFAULT_COLOR: u32 = 0xFFFF_FFFF;
/// The backdrop that is no backdrop.
const NO_BACKDROP: i32 = 1;

fn set_attribute<T>(hwnd: HWND, attribute: DWMWINDOWATTRIBUTE, value: &T) -> bool {
    let size = u32::try_from(size_of::<T>()).unwrap_or(0);
    // SAFETY: `value` is valid for `size` bytes for the call.
    unsafe { DwmSetWindowAttribute(hwnd, attribute, std::ptr::from_ref(value).cast(), size) }
        .is_ok()
}

/// How far the frame reaches into the window from the left, in units: under
/// the pane and under the rounded corner of the sheet beside it, where the
/// tint shows through as well. Without a pane it does not reach in at all:
/// what Windows itself draws there, the edit control over a field that is
/// typed in, would let the tint through and stand out from the field.
fn glass(pane: bool, mica: bool) -> f32 {
    if pane && mica {
        metric::PANE + metric::RADIUS_FLYOUT
    } else {
        0.0
    }
}

/// Dresses the window frame for the theme, with a navigation pane or
/// without. Returns whether Windows now shows its wallpaper tint (Mica)
/// behind the title bar and the pane.
pub(super) fn dress(hwnd: HWND, theme: &Theme, showing: &Showing, pane: bool, scale: f32) -> bool {
    set_attribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &BOOL::from(theme.dark));
    let compact = !showing.resizable();
    let wants_mica = !compact && !theme.contrast;
    let backdrop = if wants_mica {
        DWMSBT_MAINWINDOW.0
    } else {
        NO_BACKDROP
    };
    let mica = set_attribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &backdrop) && wants_mica;
    let margins = MARGINS {
        cxLeftWidth: px(glass(pane, mica), scale),
        ..MARGINS::default()
    };
    // SAFETY: `margins` is valid for the call.
    let extended = unsafe { DwmExtendFrameIntoClientArea(hwnd, &raw const margins) }.is_ok();
    // A small window has a title bar in the color of what is under it; the
    // Settings window keeps the one Windows gives it, tint and all.
    let caption = if theme.contrast || !compact {
        DEFAULT_COLOR
    } else {
        theme.palette.sheet.colorref()
    };
    set_attribute(hwnd, DWMWA_CAPTION_COLOR, &COLORREF(caption));
    mica && extended
}

/// The icons of a window, at the sizes its display shows them in: the small
/// one of the title bar and the large one of the taskbar and Alt+Tab. Given
/// back to Windows when dropped, which must be after the window stopped
/// using them.
pub(super) struct Icons {
    small: HICON,
    large: HICON,
}

/// The program's icon at the size Windows means by `metric` on a display of
/// `dpi` dots per inch, drawn from the frame of the icon that is that size
/// rather than scaled down from a larger one. Null if the program has none.
fn icon(metric: SYSTEM_METRICS_INDEX, dpi: u32) -> HICON {
    // SAFETY: plain queries; the resource is named by number as
    // `MAKEINTRESOURCE` does, and the handle is checked by whoever uses it.
    unsafe {
        let side = GetSystemMetricsForDpi(metric, dpi);
        let Ok(module) = GetModuleHandleW(None) else {
            return HICON::default();
        };
        LoadImageW(
            Some(module.into()),
            PCWSTR(usize::from(APP_ICON) as *const u16),
            IMAGE_ICON,
            side,
            side,
            LR_DEFAULTCOLOR,
        )
        .map(|handle| HICON(handle.0))
        .unwrap_or_default()
    }
}

impl Icons {
    /// Loads the icons for the display `hwnd` is on and gives them to it.
    /// The window uses them until it is given others or destroyed.
    pub(super) fn give(hwnd: HWND) -> Self {
        // SAFETY: a plain query of a live window.
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        let icons = Self {
            small: icon(SM_CXSMICON, dpi),
            large: icon(SM_CXICON, dpi),
        };
        for (which, handle) in [(ICON_SMALL, icons.small), (ICON_BIG, icons.large)] {
            if handle.is_invalid() {
                // The window keeps the icon of its class.
                continue;
            }
            // SAFETY: the window is live and of this thread; the icon lives
            // for as long as `icons`, which outlives its use by the window.
            unsafe {
                SendMessageW(
                    hwnd,
                    WM_SETICON,
                    Some(WPARAM(which as usize)),
                    Some(LPARAM(handle.0 as isize)),
                );
            }
        }
        icons
    }
}

impl Drop for Icons {
    fn drop(&mut self) {
        for handle in [self.small, self.large] {
            if !handle.is_invalid() {
                // SAFETY: the handle came from `LoadImageW`, is not shared,
                // and the window that showed it has others or is gone.
                let _ = unsafe { DestroyIcon(handle) };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tint_reaches_into_the_window_only_under_a_pane() {
        let under_pane = glass(true, true);
        assert!((under_pane - (metric::PANE + metric::RADIUS_FLYOUT)).abs() < f32::EPSILON);
        // The welcome has no pane, and a field of it may lie where one would be.
        assert!(glass(false, true).abs() < f32::EPSILON);
        assert!(glass(true, false).abs() < f32::EPSILON);
    }
}
