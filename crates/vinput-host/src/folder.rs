//! The folder and file pickers of Windows, for the fields in Settings.
#![allow(unsafe_code)]

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog,
    IShellItem, SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
};
use windows::core::{HSTRING, w};

/// Lets the user pick a folder, starting in `start` if that exists. `owner` is
/// the window the picker belongs to. Returns `None` if the user cancels, or
/// if Windows cannot show the picker.
pub(super) fn pick(owner: *mut core::ffi::c_void, start: &Path) -> Option<PathBuf> {
    // SAFETY: plain COM calls on objects created here. Initializing COM again
    // on a thread that already has it is harmless. The path string Windows
    // returns is copied before it is freed with the allocator it came from.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        dialog
            .SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM)
            .ok()?;
        let from: windows::core::Result<IShellItem> =
            SHCreateItemFromParsingName(&HSTRING::from(start.as_os_str()), None);
        if let Ok(from) = from {
            let _ = dialog.SetFolder(&from);
        }
        // Cancelling is reported as an error, like any other reason not to
        // have a folder.
        dialog.Show(Some(HWND(owner))).ok()?;
        let name = dialog
            .GetResult()
            .ok()?
            .GetDisplayName(SIGDN_FILESYSPATH)
            .ok()?;
        let folder = name.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(name.0.cast()));
        folder
    }
}

/// The kinds of file the picker is asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum File {
    /// A speech model.
    Model,
    /// A service account key of Google Cloud.
    GoogleKey,
}

/// Lets the user pick a file of the kind `file`. `owner` is the window the
/// picker belongs to. Returns `None` if the user cancels, or if Windows
/// cannot show the picker.
pub(super) fn pick_file(owner: *mut core::ffi::c_void, file: File) -> Option<PathBuf> {
    let (title, name, pattern) = match file {
        File::Model => (
            w!("Pick a Whisper model"),
            w!("Whisper models in ggml format (*.bin)"),
            w!("*.bin"),
        ),
        File::GoogleKey => (
            w!("Pick a service account key"),
            w!("Service account keys (*.json)"),
            w!("*.json"),
        ),
    };
    let types = [
        COMDLG_FILTERSPEC {
            pszName: name,
            pszSpec: pattern,
        },
        COMDLG_FILTERSPEC {
            pszName: w!("All files (*.*)"),
            pszSpec: w!("*.*"),
        },
    ];
    // SAFETY: plain COM calls on objects created here; the filter strings are
    // static. The path string Windows returns is copied before it is freed
    // with the allocator it came from.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        dialog
            .SetOptions(options | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM)
            .ok()?;
        dialog.SetFileTypes(&types).ok()?;
        dialog.SetTitle(title).ok()?;
        dialog.Show(Some(HWND(owner))).ok()?;
        let name = dialog
            .GetResult()
            .ok()?
            .GetDisplayName(SIGDN_FILESYSPATH)
            .ok()?;
        let file = name.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(name.0.cast()));
        file
    }
}
