//! Text injection stage.

mod keys;
pub(crate) mod live;
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

    /// Whether what was typed can be taken back again with
    /// [`rewrite`](Self::rewrite), here and now. Only into such a sink are
    /// words typed while the recognizer may still change them.
    fn rewrites(&self) -> bool {
        false
    }

    /// Takes back the last `erase` characters that were typed and types
    /// `text` in their place, as one.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Injection`](crate::Error::Injection) if the input
    /// cannot be sent, or if `erase` is not zero and the sink cannot take
    /// text back.
    fn rewrite(&mut self, erase: usize, text: &str) -> Result<()> {
        if erase > 0 {
            return Err(crate::Error::Injection(
                "what was typed cannot be taken back here".into(),
            ));
        }
        self.type_text(text)
    }

    /// Whether what was typed last may no longer be right before the caret:
    /// the focus has gone elsewhere, or the caret was moved. Taking text
    /// back would then delete something else.
    fn disturbed(&mut self) -> bool {
        false
    }
}
