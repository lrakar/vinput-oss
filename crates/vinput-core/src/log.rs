//! Minimal diagnostics: every message goes to stderr and to a log file, because
//! release builds have no console.

use std::fs::OpenOptions;
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::settings::data_dir;

/// Records `message` on stderr and appends it to `%APPDATA%\Vinput\vinput.log`.
/// Failures to write the log are ignored: logging must never take the app down.
pub fn write(message: &str) {
    eprintln!("{message}");
    // The tests of this crate provoke failures on purpose; those do not
    // belong in the log of a Vinput that is installed on the same PC.
    if cfg!(test) {
        return;
    }
    let Some(dir) = data_dir() else { return };
    let _ = std::fs::create_dir_all(&dir);
    let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("vinput.log"))
    else {
        return;
    };
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let _ = writeln!(file, "[{seconds}] {message}");
}
