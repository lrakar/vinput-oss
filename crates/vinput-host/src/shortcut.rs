//! The Start menu entry, which is what makes Vinput show up in Windows search.
#![allow(unsafe_code)]

use std::path::{Path, PathBuf};

use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::core::{HSTRING, Interface, w};

use vinput_core::{Error, Result};

/// Where the entry lives: the current user's Start menu, `Programs\Vinput.lnk`.
fn path() -> Option<PathBuf> {
    let programs = r"Microsoft\Windows\Start Menu\Programs";
    Some(
        PathBuf::from(std::env::var_os("APPDATA")?)
            .join(programs)
            .join("Vinput.lnk"),
    )
}

/// Removes the Start menu entry, if there is one.
pub(super) fn remove() {
    // The entry belongs to the installed Vinput, not to a separate copy.
    if vinput_core::instance::is_separate() {
        return;
    }
    if let Some(shortcut) = path() {
        let _ = std::fs::remove_file(shortcut);
    }
}

/// Creates or refreshes the Start menu entry so it points at the running
/// executable. Launching it opens the settings window.
///
/// COM must already be initialized on the calling thread.
pub(super) fn ensure() -> Result<()> {
    let exe =
        std::env::current_exe().map_err(|err| Error::Host(format!("Start menu entry: {err}")))?;
    ensure_for(&exe)
}

/// [`ensure`] for the program at `exe`, which need not be the one running:
/// the setup program makes the entry for the copy it has installed.
pub(super) fn ensure_for(exe: &Path) -> Result<()> {
    if vinput_core::instance::is_separate() {
        return Ok(());
    }
    let failed = |err: &dyn std::fmt::Display| Error::Host(format!("Start menu entry: {err}"));
    let shortcut = path().ok_or_else(|| failed(&"APPDATA is not set"))?;

    // SAFETY: plain COM calls on objects created here; every string is an owned,
    // NUL-terminated `HSTRING` or literal that outlives its call.
    let saved = unsafe {
        CoCreateInstance::<_, IShellLinkW>(&ShellLink, None, CLSCTX_INPROC_SERVER).and_then(
            |link| {
                link.SetPath(&HSTRING::from(exe.as_os_str()))?;
                link.SetDescription(w!("Type with your voice into any text field"))?;
                if let Some(folder) = exe.parent() {
                    link.SetWorkingDirectory(&HSTRING::from(folder.as_os_str()))?;
                }
                link.cast::<IPersistFile>()?
                    .Save(&HSTRING::from(shortcut.as_os_str()), true)
            },
        )
    };
    saved.map_err(|err| failed(&err.message()))
}
