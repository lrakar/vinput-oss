//! Putting a file on the clipboard, the way copying it in Explorer does.
//!
//! A program that accepts pasted files (a messenger, a mail window, a folder)
//! then takes the file itself on Ctrl+V.
#![allow(unsafe_code)]

use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::GlobalFree;
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};

use crate::{Error, Result};

/// The clipboard format for a list of files.
const CF_HDROP: u32 = 15;
/// Size of the `DROPFILES` header that precedes the file names.
const HEADER_BYTES: usize = 20;

/// The clipboard data for one file: a `DROPFILES` header saying that wide
/// names follow, then the path and the two terminators that end the list.
fn file_list(path: &Path) -> Vec<u8> {
    let mut data = Vec::with_capacity(HEADER_BYTES + path.as_os_str().len() * 2 + 4);
    // Offset of the names, a point that is not used, not a non-client drop.
    data.extend_from_slice(&u32::try_from(HEADER_BYTES).unwrap_or(0).to_le_bytes());
    data.extend_from_slice(&[0; 12]);
    // The names are UTF-16.
    data.extend_from_slice(&1u32.to_le_bytes());
    for unit in path.as_os_str().encode_wide().chain([0, 0]) {
        data.extend_from_slice(&unit.to_le_bytes());
    }
    data
}

/// Replaces the contents of the clipboard with the file at `path`.
///
/// # Errors
///
/// Returns [`Error::Memo`] if the clipboard is held by another program or
/// refuses the data.
pub(super) fn put_file(path: &Path) -> Result<()> {
    let refused = || {
        Error::Memo(format!(
            "putting the memo on the clipboard failed: {}",
            std::io::Error::last_os_error()
        ))
    };
    let data = file_list(path);
    // SAFETY: the memory block is as large as `data`, is locked only for the
    // copy, and belongs to the clipboard once `SetClipboardData` succeeds; it
    // is freed here only if it was not handed over. The clipboard is closed on
    // every way out after it was opened.
    unsafe {
        let block = GlobalAlloc(GMEM_MOVEABLE, data.len());
        if block.is_null() {
            return Err(refused());
        }
        let target = GlobalLock(block);
        if target.is_null() {
            GlobalFree(block);
            return Err(refused());
        }
        std::ptr::copy_nonoverlapping(data.as_ptr(), target.cast::<u8>(), data.len());
        GlobalUnlock(block);

        if OpenClipboard(null_mut()) == 0 {
            GlobalFree(block);
            return Err(refused());
        }
        EmptyClipboard();
        let placed = !SetClipboardData(CF_HDROP, block).is_null();
        let error = (!placed).then(refused);
        CloseClipboard();
        if let Some(error) = error {
            GlobalFree(block);
            return Err(error);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_list_has_a_header_wide_names_and_a_double_terminator() {
        let data = file_list(Path::new(r"C:\a.mp3"));
        assert_eq!(&data[..4], &20u32.to_le_bytes());
        assert_eq!(&data[16..20], &1u32.to_le_bytes());
        let names: Vec<u16> = data[20..]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let expected: Vec<u16> = r"C:\a.mp3".encode_utf16().chain([0, 0]).collect();
        assert_eq!(names, expected);
    }
}
