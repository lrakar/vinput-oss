//! The few registry operations a program of the family needs, all under the
//! current user: what is installed and in which version, and what starts
//! with Windows.
#![allow(unsafe_code)]

/// Where Windows keeps what is listed under "Installed apps", for the
/// current user. The name of a program's entry follows.
pub const UNINSTALL: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";

/// Where Windows keeps what starts at sign-in.
pub const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// The text value `name` of `key`. `None` if there is none.
#[must_use]
pub fn read(key: &str, name: &str) -> Option<String> {
    platform::read(key, name)
}

/// The value `name` of `key` that is a list of texts, such as the languages
/// Windows is set to. Empty if there is none.
#[must_use]
pub fn read_list(key: &str, name: &str) -> Vec<String> {
    platform::read_list(key, name)
}

/// Sets the text value `name` of `key`, making the key if need be.
pub fn write(key: &str, name: &str, value: &str) {
    platform::write(key, name, value);
}

/// Sets the number value `name` of `key`, making the key if need be.
pub fn write_number(key: &str, name: &str, value: u32) {
    platform::write_number(key, name, value);
}

/// Deletes the value `name` of `key`. One that is not there is not an error.
pub fn delete_value(key: &str, name: &str) {
    platform::delete_value(key, name);
}

/// Deletes `key` with everything under it. One that is not there is not an
/// error.
pub fn delete_key(key: &str) {
    platform::delete_key(key);
}

#[cfg(windows)]
mod platform {
    use std::ptr::null_mut;

    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_DWORD, REG_SZ, RRF_RT_REG_MULTI_SZ, RRF_RT_REG_SZ,
        RegDeleteKeyValueW, RegDeleteKeyW, RegDeleteTreeW, RegGetValueW, RegSetKeyValueW,
    };

    pub(super) fn write_number(key: &str, name: &str, value: u32) {
        let (key, name) = (wide(key), wide(name));
        // SAFETY: both strings are NUL-terminated and the data is the four
        // bytes of `value`.
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                REG_DWORD,
                (&raw const value).cast(),
                4,
            );
        }
    }

    pub(super) fn delete_key(key: &str) {
        let key = wide(key);
        // SAFETY: the string is NUL-terminated. Deleting the tree of a key
        // empties it; the empty key is then removed like a value-less one.
        unsafe {
            RegDeleteTreeW(HKEY_CURRENT_USER, key.as_ptr());
            RegDeleteKeyW(HKEY_CURRENT_USER, key.as_ptr());
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub(super) fn write(key: &str, name: &str, value: &str) {
        let (key, name, value) = (wide(key), wide(name), wide(value));
        let bytes = u32::try_from(value.len() * size_of::<u16>()).unwrap_or(0);
        // SAFETY: all strings are NUL-terminated and `bytes` is the exact size
        // of `value` including its terminator.
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                REG_SZ,
                value.as_ptr().cast(),
                bytes,
            );
        }
    }

    pub(super) fn read(key: &str, name: &str) -> Option<String> {
        let (key, name) = (wide(key), wide(name));
        let mut buffer = [0u16; 520];
        let mut bytes = u32::try_from(size_of_val(&buffer)).unwrap_or(0);
        // SAFETY: both strings are NUL-terminated and `bytes` is the size of
        // the buffer the value is written to.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                null_mut(),
                buffer.as_mut_ptr().cast(),
                &raw mut bytes,
            )
        };
        if status != ERROR_SUCCESS {
            return None;
        }
        let length = buffer.iter().position(|unit| *unit == 0)?;
        Some(String::from_utf16_lossy(&buffer[..length]))
    }

    pub(super) fn read_list(key: &str, name: &str) -> Vec<String> {
        let (key, name) = (wide(key), wide(name));
        let mut buffer = [0u16; 1024];
        let mut bytes = u32::try_from(size_of_val(&buffer)).unwrap_or(0);
        // SAFETY: both strings are NUL-terminated and `bytes` is the size of
        // the buffer the value is written to.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_MULTI_SZ,
                null_mut(),
                buffer.as_mut_ptr().cast(),
                &raw mut bytes,
            )
        };
        if status != ERROR_SUCCESS {
            return Vec::new();
        }
        let units = usize::try_from(bytes).unwrap_or(0) / size_of::<u16>();
        // Every text ends with a NUL, and the list with one more.
        buffer[..units.min(buffer.len())]
            .split(|unit| *unit == 0)
            .filter(|text| !text.is_empty())
            .map(String::from_utf16_lossy)
            .collect()
    }

    pub(super) fn delete_value(key: &str, name: &str) {
        let (key, name) = (wide(key), wide(name));
        // SAFETY: both strings are NUL-terminated. A value that is not there
        // is not an error worth reporting.
        unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) };
    }
}

#[cfg(not(windows))]
mod platform {
    pub(super) fn write(_key: &str, _name: &str, _value: &str) {}
    pub(super) fn read(_key: &str, _name: &str) -> Option<String> {
        None
    }
    pub(super) fn read_list(_key: &str, _name: &str) -> Vec<String> {
        Vec::new()
    }
    pub(super) fn delete_value(_key: &str, _name: &str) {}
    pub(super) fn write_number(_key: &str, _name: &str, _value: u32) {}
    pub(super) fn delete_key(_key: &str) {}
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn a_value_is_written_read_and_deleted() {
        let key = format!(r"Software\PutlabTests\{}", std::process::id());
        assert_eq!(read(&key, "Version"), None);
        write(&key, "Version", "1.2.3");
        assert_eq!(read(&key, "Version").as_deref(), Some("1.2.3"));
        delete_value(&key, "Version");
        assert_eq!(read(&key, "Version"), None);
        // A number is no text, and a key goes with all it holds.
        write_number(&key, "Size", 7);
        assert_eq!(read(&key, "Size"), None);
        let under = format!(r"{key}\Under");
        write(&under, "Name", "x");
        assert_eq!(read(&under, "Name").as_deref(), Some("x"));
        delete_key(&key);
        assert_eq!(read(&under, "Name"), None);
    }
}
