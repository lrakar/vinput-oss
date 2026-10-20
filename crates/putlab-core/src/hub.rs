//! The way from a program of the family to the PUTLAB hub, the one window
//! that installs and updates all of them.
//!
//! A program does not need the hub and does not ship it. It only knows
//! where the hub is when it is installed, and where it is downloaded from
//! when it is not, so that its tray menu and its About page can lead there.

use std::path::PathBuf;
use std::process::Command;

use crate::{instance, log, shell};

/// The page the hub is downloaded from.
pub const PAGE: &str = "https://vinput-dasvcr.web.app/putlab";

/// What the tray menu calls the way to the hub, when it is on this PC.
pub const OPEN: &str = "Open PUTLAB";
/// What it calls the way to where the hub is downloaded from.
pub const GET: &str = "Get PUTLAB\u{2026}";

/// Where the hub is when it is installed for the signed-in user:
/// `%LOCALAPPDATA%\Programs\PUTLAB\putlab.exe`.
#[must_use]
pub fn program() -> Option<PathBuf> {
    let local = PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
    Some(local.join("Programs").join("PUTLAB").join("putlab.exe"))
}

/// Whether the hub is on this PC.
#[must_use]
pub fn installed() -> bool {
    program().is_some_and(|program| program.is_file())
}

/// What the tray menu calls the way to the hub right now.
#[must_use]
pub fn caption() -> &'static str {
    if installed() { OPEN } else { GET }
}

/// Opens the hub, or the page it is downloaded from if it is not on this
/// PC. A copy that keeps to itself opens neither and says so in its log.
pub fn open() {
    if instance::is_quiet() {
        log::write("a separate copy does not open the hub");
        return;
    }
    let started = program()
        .filter(|program| program.is_file())
        .map(|program| shell::spawn_detached(&mut Command::new(program)));
    match started {
        Some(Ok(())) => {}
        Some(Err(error)) => log::write(&format!("the hub did not start: {error}")),
        None => {
            if !shell::open(PAGE) {
                log::write("could not open the browser for the page of the hub");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hub_is_looked_for_where_it_installs_itself() {
        let Some(program) = program() else {
            // Without the variable there is nowhere to look.
            assert!(!installed());
            return;
        };
        assert!(program.ends_with(r"Programs\PUTLAB\putlab.exe") || cfg!(not(windows)));
        assert_eq!(installed(), program.is_file());
        assert_eq!(caption(), if installed() { OPEN } else { GET });
    }

    #[test]
    fn the_page_of_the_hub_is_one_a_window_may_open() {
        assert!(shell::is_link(PAGE));
        // A test is a separate copy: asking for the hub starts nothing.
        open();
    }
}
