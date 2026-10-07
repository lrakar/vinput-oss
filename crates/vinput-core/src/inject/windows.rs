//! Typing via the Win32 `SendInput` API.
#![allow(unsafe_code)]

use std::io;

use windows_sys::Win32::Foundation::{CloseHandle, HWND};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, SendInput, VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_HOME,
    VK_LBUTTON, VK_LCONTROL, VK_LEFT, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MBUTTON, VK_NEXT, VK_PRIOR,
    VK_RBUTTON, VK_RCONTROL, VK_RETURN, VK_RIGHT, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_TAB, VK_UP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

use super::TextSink;
use super::keys::{self, Key};
use crate::{Error, Result};

#[expect(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
const INPUT_SIZE: i32 = size_of::<INPUT>() as i32;

/// The keys that change what a typed character means to the program that
/// receives it.
const MODIFIERS: [u16; 8] = [
    VK_LCONTROL,
    VK_RCONTROL,
    VK_LMENU,
    VK_RMENU,
    VK_LSHIFT,
    VK_RSHIFT,
    VK_LWIN,
    VK_RWIN,
];

/// The scan code of Backspace, which goes along with its key code as it does
/// from a keyboard, for the programs that look at it.
const BACK_SCAN: u16 = 0x0E;

/// The buttons and keys with which the user puts the caret somewhere else.
/// While one of them is down, what was typed is not taken back.
const MOVES_THE_CARET: [u16; 12] = [
    VK_LBUTTON, VK_RBUTTON, VK_MBUTTON, VK_LEFT, VK_RIGHT, VK_UP, VK_DOWN, VK_HOME, VK_END,
    VK_PRIOR, VK_NEXT, VK_DELETE,
];

/// Programs in which Backspace does not always delete the character before
/// the caret, by the name of their executable: a cell of Excel completes
/// what is typed from the cells above it, and the first Backspace takes
/// that back and no character. Nothing is typed into them that may have to
/// be taken back.
const NEVER_TAKEN_BACK_IN: [&str; 1] = ["excel.exe"];

/// Whether `path`, the executable of a program, is one of
/// [`NEVER_TAKEN_BACK_IN`].
fn never_taken_back_in(path: &str) -> bool {
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
    NEVER_TAKEN_BACK_IN
        .iter()
        .any(|known| name.eq_ignore_ascii_case(known))
}

/// The path of the executable whose window is in the foreground.
fn foreground_program() -> Option<String> {
    let mut id = 0;
    // SAFETY: plain calls; `id` is a live `u32` the second one writes to.
    unsafe { GetWindowThreadProcessId(GetForegroundWindow(), &raw mut id) };
    // SAFETY: a plain call; the handle it returns is closed below.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, id) };
    if process.is_null() {
        return None;
    }
    let mut path = [0u16; 1024];
    let mut len = u32::try_from(path.len()).unwrap_or(0);
    // SAFETY: `path` is a live buffer of `len` units and `len` a live `u32`
    // that is given the length written; the handle is open, and closed once.
    let named = unsafe {
        let named = QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &raw mut len);
        CloseHandle(process);
        named
    };
    (named != 0).then(|| String::from_utf16_lossy(&path[..(len as usize).min(path.len())]))
}

/// Types text into the window that has keyboard focus.
///
/// Characters are sent as Unicode key events, so the result does not depend on the
/// active keyboard layout. A whole call is submitted as one batch, which Windows
/// inserts into the input stream without interleaving the user's own keystrokes.
///
/// Windows refuses injected input aimed at a window of a more privileged process
/// (for example an app running as administrator); that surfaces as an error.
///
/// Text is taken back with Backspace, one press a character, in the same
/// batch as what is typed in its place. That is only right while the caret
/// is where the text was typed, so the sink remembers the window it typed
/// into and says when that may no longer hold ([`TextSink::disturbed`]).
#[derive(Default)]
pub struct SendInputSink {
    /// Reused between calls so steady-state typing does not allocate.
    inputs: Vec<INPUT>,
    /// The window that was in the foreground when text was last typed.
    typed_into: Option<HWND>,
}

impl std::fmt::Debug for SendInputSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SendInputSink").finish_non_exhaustive()
    }
}

impl SendInputSink {
    /// Creates a sink.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Presses Backspace `erase` times and types `text`, as one batch.
    fn send(&mut self, erase: usize, text: &str) -> Result<()> {
        let keys = keys::encode(text);
        if keys.is_empty() && erase == 0 {
            return Ok(());
        }
        self.inputs.clear();
        // A hotkey such as Ctrl+Alt+Space may still be held while its words
        // arrive. With Ctrl or Alt down the text would reach the program as
        // shortcuts, so those keys are let go first, in the same batch. They
        // are not pressed again: the real release that follows is harmless,
        // and a made-up press of Alt or Win on its own would open a menu.
        for vk in MODIFIERS {
            // SAFETY: a plain call that reads the state of one key.
            if unsafe { GetAsyncKeyState(i32::from(vk)) } < 0 {
                self.inputs.push(keyboard_input(vk, 0, KEYEVENTF_KEYUP));
            }
        }
        for _ in 0..erase {
            self.inputs.push(keyboard_input(VK_BACK, BACK_SCAN, 0));
            self.inputs
                .push(keyboard_input(VK_BACK, BACK_SCAN, KEYEVENTF_KEYUP));
        }
        for key in keys {
            let (vk, scan, flags) = match key {
                Key::Unicode(unit) => (0, unit, KEYEVENTF_UNICODE),
                Key::Enter => (VK_RETURN, 0, 0),
                Key::Tab => (VK_TAB, 0, 0),
            };
            self.inputs.push(keyboard_input(vk, scan, flags));
            self.inputs
                .push(keyboard_input(vk, scan, flags | KEYEVENTF_KEYUP));
        }
        let count = u32::try_from(self.inputs.len())
            .map_err(|_| Error::Injection("text is too long to type in one call".into()))?;
        // SAFETY: a plain call that reads which window is in the foreground.
        self.typed_into = Some(unsafe { GetForegroundWindow() });
        // SAFETY: `inputs` is a live, initialized slice of exactly `count` `INPUT`
        // structures, and `INPUT_SIZE` is the size of one of them.
        let sent = unsafe { SendInput(count, self.inputs.as_ptr(), INPUT_SIZE) };
        if sent == count {
            Ok(())
        } else {
            Err(Error::Injection(format!(
                "Windows accepted {sent} of {count} key events: {}",
                io::Error::last_os_error()
            )))
        }
    }
}

impl TextSink for SendInputSink {
    fn type_text(&mut self, text: &str) -> Result<()> {
        self.send(0, text)
    }

    fn rewrites(&self) -> bool {
        // Asked as a dictation starts, about the program it is for.
        !foreground_program().is_some_and(|path| never_taken_back_in(&path))
    }

    fn rewrite(&mut self, erase: usize, text: &str) -> Result<()> {
        self.send(erase, text)
    }

    fn disturbed(&mut self) -> bool {
        let Some(typed_into) = self.typed_into else {
            return false;
        };
        // SAFETY: plain calls that read which window is in the foreground
        // and the state of one key each.
        unsafe {
            GetForegroundWindow() != typed_into
                || MOVES_THE_CARET
                    .iter()
                    .any(|&vk| GetAsyncKeyState(i32::from(vk)) < 0)
        }
    }
}

fn keyboard_input(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_sends_nothing() {
        assert!(SendInputSink::new().type_text("").is_ok());
        assert!(SendInputSink::new().rewrite(0, "").is_ok());
    }

    #[test]
    fn a_sink_that_has_typed_nothing_was_not_disturbed() {
        assert!(!SendInputSink::new().disturbed());
    }

    #[test]
    fn the_programs_nothing_is_taken_back_in_are_known_by_their_executable() {
        assert!(never_taken_back_in(
            r"C:\Program Files\Microsoft Office\root\Office16\EXCEL.EXE"
        ));
        assert!(never_taken_back_in("excel.exe"));
        assert!(!never_taken_back_in(r"C:\Windows\notepad.exe"));
        assert!(!never_taken_back_in(r"C:\tools\excel.exe\chrome.exe"));
    }
}
