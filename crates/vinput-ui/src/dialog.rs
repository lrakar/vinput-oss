//! Small windows that ask or tell, for the parts of Vinput that run without
//! a background process: setup, removal, a failure at startup.
//!
//! Each call opens a window in the process and on the thread that makes it
//! and returns when the window is closed. What the window says is a
//! [`vinput_view::Dialog`]; how that looks is [`crate::pages::dialog`].
//!
//! ```no_run
//! use vinput_view::Dialog;
//!
//! let setup = Dialog {
//!     title: "VinPUT setup".to_owned(),
//!     heading: "Install VinPUT 0.3.0".to_owned(),
//!     detail: "Type with your voice into any text field.".to_owned(),
//!     lines: vec!["For your Windows account only".to_owned()],
//!     primary: "Install".to_owned(),
//!     secondary: "Cancel".to_owned(),
//! };
//! // The window stays open while the closure runs, shows "Installing" with
//! // a moving bar, and offers to try again if it fails.
//! let installed = vinput_ui::dialog::confirm_and_run(&setup, "Installing", || {
//!     std::fs::create_dir_all("somewhere").map_err(|error| error.to_string())
//! });
//! # let _ = installed;
//! ```

use std::fmt::Display;

use vinput_view::{Dialog, View};

use crate::Appearance;
use crate::error::Result;
use crate::scene::{Asking, Showing};
use crate::window::{self, Closed, Spec, Work};

/// The title of a window that was given none.
const TITLE: &str = "VinPUT";

fn open(dialog: &Dialog, working: &str, work: Option<Work<'_>>) -> Result<Closed> {
    let spec = Spec {
        showing: Showing::Asking(Asking::new(dialog.clone())),
        view: View::default(),
        appearance: Appearance::System,
        source: None,
        working,
    };
    window::open(spec, work)
}

/// Shows `dialog` and returns whether its primary button was pressed, as
/// opposed to its secondary button, Esc or the close button of the window.
///
/// # Errors
///
/// Returns [`crate::Error::System`] if the window cannot be shown.
pub fn confirm(dialog: &Dialog) -> Result<bool> {
    Ok(open(dialog, "", None)? == Closed::Accepted)
}

/// Shows `dialog`; when its primary button is pressed, runs `work` on a
/// thread of its own while the window stays open, shows `working` beside a
/// moving bar and lets nothing be pressed. If the work succeeds the window
/// closes and this returns `true`. If it fails the window shows why and its
/// primary button offers to try again. Returns `false` if the user closes
/// the window without the work having succeeded.
///
/// # Errors
///
/// Returns [`crate::Error::System`] if the window cannot be shown.
pub fn confirm_and_run<E: Display>(
    dialog: &Dialog,
    working: &str,
    mut work: impl FnMut() -> std::result::Result<(), E> + Send,
) -> Result<bool> {
    let mut work = move || work().map_err(|error| error.to_string());
    Ok(open(dialog, working, Some(&mut work))? == Closed::Accepted)
}

/// Asks a question that can also be backed out of: `Some(true)` for the
/// primary button, `Some(false)` for the secondary one, `None` if the user
/// closed the window or pressed Esc.
///
/// # Errors
///
/// Returns [`crate::Error::System`] if the window cannot be shown.
pub fn ask(dialog: &Dialog) -> Result<Option<bool>> {
    open(dialog, "", None).map(Option::from)
}

/// A window that only tells: its first line in large type, the rest under
/// it, and one button.
fn telling(message: &str) -> Dialog {
    let (heading, detail) = message.split_once('\n').unwrap_or((message, ""));
    Dialog {
        title: TITLE.to_owned(),
        heading: heading.trim().to_owned(),
        detail: detail.trim().to_owned(),
        ..Dialog::default()
    }
}

/// Tells the user something and waits for them to acknowledge it. The first
/// line of `message` is shown in large type and the rest under it.
///
/// # Errors
///
/// Returns [`crate::Error::System`] if the window cannot be shown.
pub fn tell(message: &str) -> Result<()> {
    open(&telling(message), "", None).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_is_a_heading_with_the_rest_under_it() {
        let dialog = telling("VinPUT could not start.\n\nThe microphone is in use.\n");
        assert_eq!(dialog.title, TITLE);
        assert_eq!(dialog.heading, "VinPUT could not start.");
        assert_eq!(dialog.detail, "The microphone is in use.");
        assert!(dialog.secondary.is_empty());
        assert_eq!(telling("Done").detail, "");
    }
}
