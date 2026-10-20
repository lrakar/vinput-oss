//! API keys and the session with `VinPUT` Pro, kept in the Windows credential
//! store.
//!
//! Secrets never go into the settings file or the log. The credential store
//! encrypts them for the signed-in user, and they show up under "Windows
//! Credentials" in the Credential Manager, where they can be inspected and
//! removed without Vinput.

use crate::Result;

/// The key for Google's Gemini API.
pub const GEMINI_API_KEY: &str = "gemini-api-key";

/// A service account key for Google Cloud Speech-to-Text, cut down to what
/// asking for access tokens needs.
pub const GOOGLE_CLOUD_KEY: &str = "google-cloud-key";

/// The session with `VinPUT` Pro: its refresh token and whose it is; see
/// [`crate::cloud`].
pub const PRO_SESSION: &str = "vinput-pro-session";

/// Reads a secret. `None` if it was never stored.
#[must_use]
pub fn get(name: &str) -> Option<String> {
    platform::get(name)
}

/// Stores a secret, replacing an earlier value.
///
/// # Errors
///
/// Returns [`Error::Host`](crate::Error::Host) if the credential store refuses it.
pub fn set(name: &str, secret: &str) -> Result<()> {
    platform::set(name, secret)
}

/// Deletes a secret. Deleting one that is not there is not an error.
pub fn delete(name: &str) {
    platform::delete(name);
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod platform {
    use std::ptr::null_mut;

    use windows_sys::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree,
        CredReadW, CredWriteW,
    };

    use crate::{Error, Result};

    /// The name a secret is stored under, NUL-terminated.
    fn target(name: &str) -> Vec<u16> {
        format!("{}/{name}", crate::instance::named("Vinput"))
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect()
    }

    pub(super) fn get(name: &str) -> Option<String> {
        let target = target(name);
        let mut credential: *mut CREDENTIALW = null_mut();
        // SAFETY: `target` is NUL-terminated and `credential` is writable. On
        // success it points to a structure owned by the system, whose blob is
        // `CredentialBlobSize` bytes long; it is read and then freed exactly once.
        unsafe {
            if CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &raw mut credential) == 0 {
                return None;
            }
            let blob = std::slice::from_raw_parts(
                (*credential).CredentialBlob,
                (*credential).CredentialBlobSize as usize,
            );
            let secret = String::from_utf8(blob.to_vec()).ok();
            CredFree(credential.cast());
            secret.filter(|secret| !secret.is_empty())
        }
    }

    pub(super) fn set(name: &str, secret: &str) -> Result<()> {
        let mut target = target(name);
        let mut user: Vec<u16> = "Vinput\0".encode_utf16().collect();
        let mut blob = secret.as_bytes().to_vec();
        let credential = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: target.as_mut_ptr(),
            CredentialBlobSize: u32::try_from(blob.len())
                .map_err(|_| Error::Host("the key is too long to store".into()))?,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            UserName: user.as_mut_ptr(),
            ..Default::default()
        };
        // SAFETY: every pointer in `credential` refers to a live, NUL-terminated
        // or length-counted buffer above; the system copies them during the call.
        if unsafe { CredWriteW(&raw const credential, 0) } == 0 {
            return Err(Error::Host(format!(
                "storing the key in the Windows credential store failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(())
    }

    pub(super) fn delete(name: &str) {
        let target = target(name);
        // SAFETY: `target` is NUL-terminated.
        unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) };
    }
}

#[cfg(not(windows))]
mod platform {
    use crate::{Error, Result};

    pub(super) fn get(_name: &str) -> Option<String> {
        None
    }

    pub(super) fn set(_name: &str, _secret: &str) -> Result<()> {
        Err(Error::Host("the credential store needs Windows".into()))
    }

    pub(super) fn delete(_name: &str) {}
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn a_secret_is_stored_read_back_and_deleted() {
        let name = format!("test-{}", std::process::id());
        assert_eq!(get(&name), None);
        assert!(set(&name, "s3cret čšž").is_ok());
        assert_eq!(get(&name).as_deref(), Some("s3cret čšž"));
        assert!(set(&name, "replaced").is_ok());
        assert_eq!(get(&name).as_deref(), Some("replaced"));
        delete(&name);
        assert_eq!(get(&name), None);
    }
}
