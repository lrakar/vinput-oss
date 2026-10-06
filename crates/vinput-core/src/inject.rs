//! Text injection stage.

mod keys;
#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::SendInputSink;

use crate::Result;

/// Somewhere recognized text can be typed, normally the focused window.
pub trait TextSink {
    /// Types `text` at the current cursor position.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Injection`](crate::Error::Injection) if the input cannot be sent.
    fn type_text(&mut self, text: &str) -> Result<()>;
}
