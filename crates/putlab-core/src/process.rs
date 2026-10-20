//! Starting helper programs, and unpacking what was downloaded for them.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Error, Result};

/// Keeps a console program started from the background app from flashing a
/// console window on screen.
pub fn hide_console(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        /// `CREATE_NO_WINDOW`.
        const NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

/// `path` with `suffix` added to its last component.
#[must_use]
pub fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Unpacks `archive`, a `.zip` or a `.tar` of any compression, into `dir`
/// with the `tar` that is part of Windows. The files appear under `dir` only
/// once all of them are unpacked.
///
/// # Errors
///
/// Returns [`Error::Install`] with what `tar` said.
pub fn unpack(archive: &Path, dir: &Path) -> Result<()> {
    let failed = |what: &str| Error::Install(format!("unpacking the engine failed: {what}"));
    let staging = suffixed(dir, ".unpacking");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|err| failed(&err.to_string()))?;
    let system = std::env::var_os("SystemRoot").map_or_else(
        || PathBuf::from("tar.exe"),
        |root| PathBuf::from(root).join("System32").join("tar.exe"),
    );
    let mut command = Command::new(system);
    command.arg("-xf").arg(archive).arg("-C").arg(&staging);
    hide_console(&mut command);
    let output = command.output().map_err(|err| failed(&err.to_string()))?;
    if !output.status.success() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(failed(String::from_utf8_lossy(&output.stderr).trim()));
    }
    let _ = std::fs::remove_dir_all(dir);
    std::fs::rename(&staging, dir).map_err(|err| failed(&err.to_string()))
}

/// A key for one start of a helper that listens on a port of this PC: 128
/// bits nobody can guess, in letters and digits. Any program on the PC can
/// reach such a port, and so can a page in a browser; without the key none
/// of them is answered. `None` if Windows has no randomness to give.
#[must_use]
pub fn secret() -> Option<String> {
    use ring::rand::{SecureRandom as _, SystemRandom};
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut bytes = [0u8; 16];
    SystemRandom::new().fill(&mut bytes).ok()?;
    let digits = bytes
        .iter()
        .flat_map(|byte| [HEX[usize::from(byte >> 4)], HEX[usize::from(byte & 15)]]);
    Some(digits.map(char::from).collect())
}
