//! Small Win32 helpers the rest of the host shares: text in the form Windows
//! wants it, the last error as one of ours, whether a key is down, and
//! having the shell open something.
//!
//! Nothing here keeps state. What decides whether something may be opened at
//! all is plain code that can be tested; only the hand-over to Windows is not.
#![allow(unsafe_code)]

use std::io;
use std::path::Path;
use std::ptr::{null, null_mut};

use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use vinput_core::cloud::Server;
use vinput_core::{Error, search};

/// The longest web address opened for a window; Windows itself stops a
/// little above this.
const MAX_LINK: usize = 2000;

/// `text` as NUL-terminated UTF-16.
pub(crate) fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The last error Windows reported on this thread, as a host error that says
/// what was being done.
pub(crate) fn last_error(action: &str) -> Error {
    Error::Host(format!("{action} failed: {}", io::Error::last_os_error()))
}

/// Copies `text` into a fixed-size UTF-16 buffer, truncating if needed and always
/// leaving a NUL terminator.
pub(crate) fn copy_text(buffer: &mut [u16], text: &str) {
    let capacity = buffer.len().saturating_sub(1);
    buffer.fill(0);
    for (slot, unit) in buffer.iter_mut().zip(text.encode_utf16().take(capacity)) {
        *slot = unit;
    }
}

/// Whether the key with this virtual-key code is down right now.
pub(crate) fn is_down(vk: u16) -> bool {
    // SAFETY: plain value argument. The sign bit is set while the key is down.
    unsafe { GetAsyncKeyState(i32::from(vk)) < 0 }
}

/// Has Windows open a folder, or a web address in the default browser.
pub(crate) fn open(what: &str) -> bool {
    let (verb, what) = (wide("open"), wide(what));
    // SAFETY: both strings are NUL-terminated and outlive the call. Values
    // above 32 mean success.
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

/// Whether a window may have `address` opened in the browser: a plain
/// `https://` address and nothing else, because it is handed to Windows to
/// open and must never name a program or a file.
pub(crate) fn is_link(address: &str) -> bool {
    address.len() <= MAX_LINK && address.starts_with("https://") && search::is_valid(address)
}

/// Whether a page the server of `VinPUT` Pro handed out may be opened in the
/// browser: a plain web address, and one that server may send people to.
pub(crate) fn is_page(address: &str, server: &Server) -> bool {
    address.len() <= MAX_LINK && search::is_valid(address) && server.may_open(address)
}

/// Whether `path` names a folder that is there, by its whole path. Windows
/// would open a program or a document just as readily as a folder, so
/// nothing else is ever handed to it as one.
pub(crate) fn is_folder(path: &Path) -> bool {
    path.is_absolute() && path.is_dir()
}

/// Opens `folder` in Explorer, creating it if it is not there yet. Returns
/// whether a folder was opened.
pub(crate) fn open_folder(folder: &Path) -> bool {
    let _ = std::fs::create_dir_all(folder);
    is_folder(folder) && open(&folder.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_truncated_with_a_terminator() {
        let mut buffer = [0xFFFFu16; 4];
        copy_text(&mut buffer, "abcdef");
        assert_eq!(
            buffer,
            [u16::from(b'a'), u16::from(b'b'), u16::from(b'c'), 0]
        );
    }

    #[test]
    fn only_plain_web_pages_are_opened_for_a_window() {
        assert!(is_link("https://vinput-dasvcr.web.app/docs"));
        assert!(is_link("https://github.com/lrakar/vinput/releases?page=2"));
        for refused in [
            "",
            "http://example.com",
            "https://",
            "calc.exe",
            r"C:\Windows\System32\calc.exe",
            "file:///C:/Windows/System32/calc.exe",
            "https://example.com/a b",
            "https://example.com/\"& calc",
            " https://example.com",
        ] {
            assert!(!is_link(refused), "{refused}");
        }
        assert!(!is_link(&format!(
            "https://example.com/{}",
            "a".repeat(MAX_LINK)
        )));
    }

    #[test]
    fn only_pages_the_server_may_send_people_to_are_opened_for_it() {
        let Some(server) = Server::at("https://cloud.example.com") else {
            unreachable!("an HTTPS address is a server");
        };
        assert!(is_page(
            "https://checkout.stripe.com/c/pay/cs_1#fid=a%2Fb",
            &server
        ));
        for refused in [
            "http://checkout.stripe.com/c/pay/cs_1",
            "http://127.0.0.1:8787/v1/auth/google/start",
            "https://example.com/\"& calc",
            "calc.exe",
            "",
        ] {
            assert!(!is_page(refused, &server), "{refused}");
        }
        // A test server on this computer hands out its own plain addresses.
        let local = Server::at("http://127.0.0.1:8787");
        assert!(local.is_some_and(|local| {
            is_page("http://127.0.0.1:8787/v1/auth/google/start?code=1", &local)
        }));
    }

    #[test]
    fn only_a_folder_that_is_there_is_opened_as_one() -> io::Result<()> {
        let folder = std::env::temp_dir().join(format!("vinput-win-{}", std::process::id()));
        std::fs::create_dir_all(&folder)?;
        let file = folder.join("calc.exe");
        std::fs::write(&file, b"not a program")?;
        assert!(is_folder(&folder));
        // A file, something that is not there, and a path that depends on
        // where this process happens to be.
        assert!(!is_folder(&file));
        assert!(!is_folder(&folder.join("missing")));
        assert!(!is_folder(Path::new(".")));
        std::fs::remove_dir_all(&folder)
    }
}
