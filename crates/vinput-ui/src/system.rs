//! What the user asked of Windows: light or dark, the accent color, high
//! contrast, larger text.
#![allow(unsafe_code)]

use windows::Win32::Graphics::Gdi::{
    COLOR_BTNFACE, COLOR_BTNTEXT, COLOR_GRAYTEXT, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT,
    COLOR_HOTLIGHT, COLOR_WINDOW, COLOR_WINDOWTEXT, GetSysColor, SYS_COLOR_INDEX,
};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RRF_RT_REG_DWORD, RegGetValueW,
};
use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows::Win32::UI::WindowsAndMessaging::{
    SPI_GETHIGHCONTRAST, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
};
use windows::core::{PCWSTR, w};

use crate::theme::{Accent, Color, SystemColors, Theme};

const PERSONALIZE: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
const ACCENT: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent");
const ACCESSIBILITY: PCWSTR = w!(r"Software\Microsoft\Accessibility");

/// A number the user's part of the registry holds.
fn number(key: PCWSTR, name: PCWSTR) -> Option<u32> {
    let mut value = 0_u32;
    let mut size = 4_u32;
    // SAFETY: the names are NUL-terminated literals; `value` is as large as
    // `size` says.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key,
            name,
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut value).cast()),
            Some(&raw mut size),
        )
    };
    status.is_ok().then_some(value)
}

/// The accent color and its shades, as Windows keeps them: eight colors of
/// four bytes each, from the lightest to the darkest.
fn accent() -> Option<Accent> {
    let mut shades = [0_u8; 32];
    let mut size = 32_u32;
    // SAFETY: the names are NUL-terminated literals; the buffer is as large
    // as `size` says.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            ACCENT,
            w!("AccentPalette"),
            RRF_RT_REG_BINARY,
            None,
            Some(shades.as_mut_ptr().cast()),
            Some(&raw mut size),
        )
    };
    if status.is_err() || size != 32 {
        return None;
    }
    let shade = |index: usize| {
        let at = index * 4;
        Color::rgb(
            (u32::from(shades[at]) << 16)
                | (u32::from(shades[at + 1]) << 8)
                | u32::from(shades[at + 2]),
        )
    };
    Some(Accent {
        light3: shade(0),
        light2: shade(1),
        light1: shade(2),
        base: shade(3),
        dark1: shade(4),
        dark2: shade(5),
        dark3: shade(6),
    })
}

fn system_color(index: SYS_COLOR_INDEX) -> Color {
    // SAFETY: a plain call with a valid index.
    Color::from_colorref(unsafe { GetSysColor(index) })
}

/// The colors of the high-contrast theme, if one is on.
fn high_contrast() -> Option<SystemColors> {
    let mut state = HIGHCONTRASTW {
        cbSize: u32::try_from(size_of::<HIGHCONTRASTW>()).unwrap_or(0),
        ..Default::default()
    };
    // SAFETY: `state` is valid for the call and says its own size.
    let asked = unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            state.cbSize,
            Some((&raw mut state).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    if asked.is_err() || !state.dwFlags.contains(HCF_HIGHCONTRASTON) {
        return None;
    }
    Some(SystemColors {
        window: system_color(COLOR_WINDOW),
        text: system_color(COLOR_WINDOWTEXT),
        gray_text: system_color(COLOR_GRAYTEXT),
        button: system_color(COLOR_BTNFACE),
        button_text: system_color(COLOR_BTNTEXT),
        highlight: system_color(COLOR_HIGHLIGHT),
        highlight_text: system_color(COLOR_HIGHLIGHTTEXT),
        hot: system_color(COLOR_HOTLIGHT),
    })
}

/// How much larger than usual the user wants text.
#[must_use]
pub fn text_scale() -> f32 {
    let percent = number(ACCESSIBILITY, w!("TextScaleFactor")).unwrap_or(100);
    f32::from(u16::try_from(percent.clamp(100, 225)).unwrap_or(100)) / 100.0
}

/// Whether Windows is set to dark for apps.
#[must_use]
pub fn prefers_dark() -> bool {
    number(PERSONALIZE, w!("AppsUseLightTheme")) == Some(0)
}

/// The theme Windows is set to right now.
#[must_use]
pub fn theme() -> Theme {
    let mut theme = match high_contrast() {
        Some(colors) => Theme::high_contrast(&colors),
        None => Theme::of(prefers_dark(), &accent().unwrap_or_default()),
    };
    theme.text_scale = text_scale();
    theme
}

/// The light or the dark theme with the user's accent color and text size,
/// whatever Windows is set to.
#[must_use]
pub fn theme_forced(dark: bool) -> Theme {
    let mut theme = Theme::of(dark, &accent().unwrap_or_default());
    theme.text_scale = text_scale();
    theme
}
