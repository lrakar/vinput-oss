//! Putting text on the clipboard.
#![allow(unsafe_code)]

use windows::Win32::Foundation::{GlobalFree, HANDLE, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};

/// The clipboard format for text in UTF-16.
const CF_UNICODETEXT: u32 = 13;
/// How often the clipboard is asked for while another program holds it.
const OPEN_TRIES: u32 = 5;
/// How long to wait between two tries.
const OPEN_PAUSE: std::time::Duration = std::time::Duration::from_millis(10);

/// Puts `text` on the clipboard in the name of the window `owner`. Returns
/// whether it got there.
pub(crate) fn copy(owner: HWND, text: &str) -> bool {
    let units: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    // SAFETY: the memory is allocated for exactly `units`, locked while it
    // is written and handed to the clipboard only if every step before
    // worked; the clipboard owns it from then on, otherwise it is freed
    // here. The clipboard is closed on every path after it was opened.
    unsafe {
        let Ok(memory) = GlobalAlloc(GMEM_MOVEABLE, units.len() * size_of::<u16>()) else {
            return false;
        };
        let target = GlobalLock(memory).cast::<u16>();
        if target.is_null() {
            let _ = GlobalFree(Some(memory));
            return false;
        }
        std::ptr::copy_nonoverlapping(units.as_ptr(), target, units.len());
        let _ = GlobalUnlock(memory);
        // Another program may be looking at the clipboard this instant;
        // it is asked for a few times before giving up.
        let opened = (0..OPEN_TRIES).any(|attempt| {
            if attempt > 0 {
                std::thread::sleep(OPEN_PAUSE);
            }
            OpenClipboard(Some(owner)).is_ok()
        });
        if !opened {
            let _ = GlobalFree(Some(memory));
            return false;
        }
        let placed = EmptyClipboard().is_ok()
            && SetClipboardData(CF_UNICODETEXT, Some(HANDLE(memory.0))).is_ok();
        if !placed {
            let _ = GlobalFree(Some(memory));
        }
        let _ = CloseClipboard();
        placed
    }
}
