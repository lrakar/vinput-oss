//! Having Windows open something for the user, and starting a program that
//! then runs on its own.
//!
//! What decides whether something may be opened at all is plain code that
//! can be tested; only the hand-over to Windows is not.
#![allow(unsafe_code)]

use std::process::Command;

/// The longest web address opened for a window; Windows itself stops a
/// little above this.
const MAX_LINK: usize = 2000;

/// Whether a window may have `address` opened in the browser: a plain
/// `https://` address and nothing else, because it is handed to Windows to
/// open and must never name a program or a file.
#[must_use]
pub fn is_link(address: &str) -> bool {
    let plain = |c: char| c.is_ascii_graphic() && !matches!(c, '"' | '<' | '>' | '\\' | '^' | '`');
    address.len() <= MAX_LINK
        && address
            .strip_prefix("https://")
            .is_some_and(|rest| !rest.is_empty() && !rest.starts_with('/'))
        && address.chars().all(plain)
}

/// Has Windows open a folder, or a web address in the default browser.
/// Returns whether it did. The caller has checked what it hands over.
#[must_use]
pub fn open(what: &str) -> bool {
    #[cfg(windows)]
    {
        use std::ptr::{null, null_mut};

        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        let wide = |text: &str| -> Vec<u16> { text.encode_utf16().chain([0]).collect() };
        let (verb, what) = (wide("open"), wide(what));
        // SAFETY: both strings are NUL-terminated and outlive the call.
        // Values above 32 mean success.
        let opened = unsafe {
            ShellExecuteW(
                null_mut(),
                verb.as_ptr(),
                what.as_ptr(),
                null(),
                null(),
                SW_SHOWNORMAL,
            )
        };
        opened as usize > 32
    }
    #[cfg(not(windows))]
    {
        let _ = what;
        false
    }
}

/// Starts `command` as a process of its own, with nothing of this one's: no
/// console, no standard handles, not in its process group and, where Windows
/// allows it, not in its job. It goes on when this process ends.
///
/// # Errors
///
/// Returns why Windows did not start it.
pub fn spawn_detached(command: &mut Command) -> std::io::Result<()> {
    use std::process::Stdio;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        /// `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`.
        const APART: u32 = 0x0000_0008 | 0x0000_0200;
        /// `CREATE_BREAKAWAY_FROM_JOB`, which a job may refuse.
        const OUT_OF_THE_JOB: u32 = 0x0100_0000;
        if command
            .creation_flags(APART | OUT_OF_THE_JOB)
            .spawn()
            .is_ok()
        {
            return Ok(());
        }
        command.creation_flags(APART);
    }
    command.spawn().map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_plain_https_address_is_a_link() {
        for good in [
            "https://vinput-dasvcr.web.app",
            "https://example.com/a?b=c#d",
        ] {
            assert!(is_link(good), "{good}");
        }
        for bad in [
            "",
            "https://",
            "https:///x",
            "http://example.com",
            "file:///C:/Windows/System32/calc.exe",
            r"C:\Windows\System32\calc.exe",
            "https://example.com/ \"& calc",
            "calc",
        ] {
            assert!(!is_link(bad), "{bad}");
        }
        assert!(!is_link(&format!(
            "https://example.com/{}",
            "a".repeat(2000)
        )));
    }
}
