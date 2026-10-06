//! Starting Vinput when the user signs in, via the per-user `Run` registry key.
#![allow(unsafe_code)]

use std::path::Path;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};

use crate::windows::wide;
use vinput_core::{Error, Result, instance};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "Vinput";

/// Whether a Vinput entry is present in the `Run` key.
pub(super) fn is_enabled() -> bool {
    if instance::is_separate() {
        return false;
    }
    let (key, value) = (wide(RUN_KEY), wide(VALUE_NAME));
    let mut size = 0u32;
    // SAFETY: both strings are NUL-terminated; a null data pointer with a size
    // pointer only queries whether the value exists.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            null_mut(),
            &raw mut size,
        )
    };
    status == ERROR_SUCCESS
}

/// Adds or removes the `Run` entry pointing at the running executable.
pub(super) fn set(enabled: bool) -> Result<()> {
    // The entry belongs to the installed Vinput; a separate copy, run
    // beside it for a try, leaves it alone.
    if instance::is_separate() {
        return Ok(());
    }
    let exe = std::env::current_exe()
        .map_err(|err| Error::Host(format!("finding the Vinput executable: {err}")))?;
    set_for(&exe, enabled)
}

/// [`set`] for the program at `exe`, which need not be the one running: the
/// setup program makes the entry for the copy it has installed.
pub(super) fn set_for(exe: &Path, enabled: bool) -> Result<()> {
    if instance::is_separate() {
        return Ok(());
    }
    let (key, value) = (wide(RUN_KEY), wide(VALUE_NAME));
    let status = if enabled {
        // `--background` keeps the settings window from opening at sign-in.
        let command = wide(&format!("\"{}\" --background", exe.display()));
        let bytes = u32::try_from(command.len() * size_of::<u16>())
            .map_err(|_| Error::Host("executable path is too long".into()))?;
        // SAFETY: all strings are NUL-terminated and `bytes` is the exact size of
        // `command` including its terminator.
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                REG_SZ,
                command.as_ptr().cast(),
                bytes,
            )
        }
    } else {
        // SAFETY: both strings are NUL-terminated.
        match unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr()) } {
            ERROR_FILE_NOT_FOUND => ERROR_SUCCESS,
            status => status,
        }
    };
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(Error::Host(format!(
            "updating the startup entry failed (error {status})"
        )))
    }
}
