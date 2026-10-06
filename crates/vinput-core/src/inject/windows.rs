//! Typing via the Win32 `SendInput` API.
#![allow(unsafe_code)]

use std::io;

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, SendInput, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN,
    VK_RCONTROL, VK_RETURN, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_TAB,
};

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

/// Types text into the window that has keyboard focus.
///
/// Characters are sent as Unicode key events, so the result does not depend on the
/// active keyboard layout. A whole call is submitted as one batch, which Windows
/// inserts into the input stream without interleaving the user's own keystrokes.
///
/// Windows refuses injected input aimed at a window of a more privileged process
/// (for example an app running as administrator); that surfaces as an error.
#[derive(Default)]
pub struct SendInputSink {
    /// Reused between calls so steady-state typing does not allocate.
    inputs: Vec<INPUT>,
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
}

impl TextSink for SendInputSink {
    fn type_text(&mut self, text: &str) -> Result<()> {
        let keys = keys::encode(text);
        if keys.is_empty() {
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
    }
}
