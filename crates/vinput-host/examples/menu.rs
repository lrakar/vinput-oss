//! Shows a menu like the tray icon's, to look at and to try with the mouse
//! and the keyboard. It closes by itself after a few seconds, and prints the
//! id of the item picked.
//!
//! ```text
//! cargo run -p vinput-host --example menu -- [system|light|dark] [seconds] [keys] [x y]
//! ```
//!
//! `keys` are pressed for you, one every quarter of a second, to open
//! submenus without touching anything: a list such as `down,down,right`, or
//! `-` for none. The menu opens at `x`, `y`, or at the pointer.
//!
//! The list of microphones is private to the host, so its source is
//! compiled into this example as it stands.
#![allow(unsafe_code)]

#[cfg(windows)]
#[allow(dead_code, unreachable_pub)]
#[path = "../src/menu.rs"]
mod menu;

/// Text in the form Windows takes it.
#[cfg(windows)]
mod windows {
    pub(crate) fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }
}

#[cfg(windows)]
mod demo {
    use std::cell::RefCell;
    use std::ptr::{null, null_mut};

    use vinput_core::audio::Microphone;
    use windows_sys::Win32::Foundation::{HWND, POINT};
    use windows_sys::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        VK_DOWN, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_UP,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, EndMenu, GetCursorPos, KillTimer, PostMessageW, SetTimer,
        WM_KEYDOWN, WS_EX_TOOLWINDOW, WS_POPUP,
    };

    use putlab_core::menu::Theme;

    use crate::menu::{self, Mark, Menu};
    use crate::windows::wide;

    /// How often a key is pressed, in milliseconds.
    const STEP_MS: u32 = 250;

    thread_local! {
        /// The keys still to press, last first, and the steps left after
        /// them until the menu is closed.
        static SCRIPT: RefCell<(Vec<u16>, u32)> = const { RefCell::new((Vec::new(), 0)) };
    }

    /// Presses the next key, or closes the menu when its time is up.
    unsafe extern "system" fn step(hwnd: HWND, _message: u32, _id: usize, _time: u32) {
        SCRIPT.with_borrow_mut(|(keys, steps)| {
            if let Some(key) = keys.pop() {
                // SAFETY: `hwnd` is the live window the timer belongs to.
                unsafe { PostMessageW(hwnd, WM_KEYDOWN, usize::from(key), 0) };
            } else if *steps == 0 {
                // SAFETY: no arguments; closes the menu this thread has open.
                unsafe { EndMenu() };
            } else {
                *steps -= 1;
            }
        });
    }

    fn key(name: &str) -> Option<u16> {
        Some(match name {
            "down" => VK_DOWN,
            "up" => VK_UP,
            "left" => VK_LEFT,
            "right" => VK_RIGHT,
            "enter" => VK_RETURN,
            "esc" => VK_ESCAPE,
            letter => u16::from(letter.as_bytes().first()?.to_ascii_uppercase()),
        })
    }

    /// A menu with everything the tray menu has.
    fn tray_menu(theme: Theme) -> Option<Menu> {
        let mut menu = Menu::themed(theme)?;
        menu.item(1, "Ready: Ctrl+Alt+Space to dictate", Mark::Disabled)
            .separator()
            .item(2, "Settings…", Mark::Default);

        let mut models = Menu::new()?;
        models
            .item(10, "Parakeet (local)", Mark::Radio)
            .item(11, "Whisper large v3 turbo (local)", Mark::None)
            .item(12, "A model that is not downloaded", Mark::Disabled)
            .separator()
            .item(13, "Download models…", Mark::None);
        menu.submenu("Model", models);

        let connected =
            ["Microphone (USB Audio Device)", "Headset (WH-1000XM5)"].map(|name| Microphone {
                id: name.to_owned(),
                name: name.to_owned(),
            });
        menu.submenu(
            "Audio input",
            menu::microphones(&connected, &connected[1].id, 20)?,
        );
        menu.item(
            3,
            "Use the graphics card (NVIDIA GeForce RTX 4070)",
            Mark::Check,
        )
        .separator()
        .item(4, "Open voice memos", Mark::None)
        .item(5, "Check for updates", Mark::None)
        .item(6, "Start with Windows", Mark::Check)
        .separator()
        .item(7, "Quit VinPUT", Mark::None);
        Some(menu)
    }

    pub(crate) fn run() {
        let arguments: Vec<String> = std::env::args().skip(1).collect();
        let argument = |index: usize| arguments.get(index).map(String::as_str);
        let theme = match argument(0) {
            Some("light") => Theme::Light,
            Some("dark") => Theme::Dark,
            _ => Theme::System,
        };
        let seconds: u32 = argument(1).and_then(|text| text.parse().ok()).unwrap_or(4);
        let mut keys: Vec<u16> = argument(2)
            .unwrap_or_default()
            .split(',')
            .filter_map(key)
            .collect();
        keys.reverse();
        SCRIPT.set((keys, seconds.clamp(1, 60) * 1000 / STEP_MS));

        // SAFETY: called once, before any window exists, as the host does.
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        let mut point = POINT::default();
        // SAFETY: `point` is writable.
        unsafe { GetCursorPos(&raw mut point) };
        if let (Some(x), Some(y)) = (
            argument(3).and_then(|text| text.parse().ok()),
            argument(4).and_then(|text| text.parse().ok()),
        ) {
            point = POINT { x, y };
        }

        let class = wide("STATIC");
        // SAFETY: the class name is NUL-terminated. The window is never
        // shown; it is there to own the menu, as the host's is.
        let owner = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                class.as_ptr(),
                null(),
                WS_POPUP,
                0,
                0,
                0,
                0,
                null_mut(),
                null_mut(),
                null_mut(),
                null(),
            )
        };
        if owner.is_null() {
            eprintln!("no window to own the menu");
            return;
        }
        if let Some(menu) = tray_menu(theme) {
            // SAFETY: `owner` is live; the timer is stopped below.
            unsafe { SetTimer(owner, 1, STEP_MS, Some(step)) };
            let picked = menu.show(owner, point.x, point.y);
            // SAFETY: as above.
            unsafe { KillTimer(owner, 1) };
            eprintln!("picked {picked}");
        }
        // SAFETY: the window was created above and is not used again.
        unsafe { DestroyWindow(owner) };
    }
}

#[cfg(windows)]
fn main() {
    demo::run();
}

#[cfg(not(windows))]
fn main() {}
