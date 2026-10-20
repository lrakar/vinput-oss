//! Minimal diagnostics: every message goes to stderr and to a log file, because
//! release builds have no console.

use std::fs::OpenOptions;
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::folders::data_dir;
use crate::product;

/// Records `message` on stderr and appends it to the log of this program,
/// `%APPDATA%\Vinput\vinput.log` for Vinput.
/// Failures to write the log are ignored: logging must never take the app down.
pub fn write(message: &str) {
    eprintln!("{message}");
    // Tests provoke failures on purpose; those do not belong in a log.
    if cfg!(any(test, feature = "test-instance")) {
        return;
    }
    let Some(dir) = data_dir() else { return };
    let _ = std::fs::create_dir_all(&dir);
    let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(product::current().log))
    else {
        return;
    };
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let _ = writeln!(file, "[{seconds}] {message}");
}

/// Has a panic leave a line in the log before it ends the program.
///
/// A build that ships aborts on a panic and has no console, so without this
/// a program that hit one is simply gone, from the tray and from the screen,
/// and nothing says why. Called once, first thing, by each program. The line
/// names the thread, the place in the source and the message.
pub fn record_panics() {
    std::panic::set_hook(Box::new(|panic| {
        let thread = std::thread::current();
        write(&panic_line(thread.name(), &panic.to_string()));
    }));
}

/// The line a panic leaves: one line, whatever the message holds.
fn panic_line(thread: Option<&str>, panic: &str) -> String {
    let thread = thread.unwrap_or("a thread without a name");
    let panic = panic.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("panic on {thread}: {panic}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_is_one_line_that_names_its_thread_and_place() {
        let line = panic_line(
            Some("vinput-dictation"),
            "panicked at crates/vinput-core/src/pipeline.rs:91:17:
index out of bounds",
        );
        assert_eq!(
            line,
            "panic on vinput-dictation: panicked at crates/vinput-core/src/pipeline.rs:91:17: index out of bounds"
        );
        assert!(panic_line(None, "x").starts_with("panic on a thread without a name"));
    }
}
