//! Crate-wide error type.

use std::fmt;

/// Convenience alias for results produced by this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Everything that can go wrong in the dictation pipeline, by stage.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// Capturing audio from the microphone failed.
    Audio(String),
    /// The speech-to-text service failed or returned something unusable.
    Transcription(String),
    /// Nothing can be recognized until the user has done something about it
    /// in Settings: chosen or downloaded a model, signed in, subscribed.
    /// Pressing the key again would only fail again.
    Setup(String),
    /// Typing text into the focused window failed.
    Injection(String),
    /// Recording or saving a voice memo failed.
    Memo(String),
    /// Rewriting text with a language model failed: tidying up what was
    /// dictated, or editing a selection. In words for the user.
    Rewrite(String),
    /// Downloading or installing a speech model, or an update, failed.
    Install(String),
    /// The background host (hidden window, hotkey, tray icon) failed.
    Host(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Audio(msg) => write!(f, "audio capture failed: {msg}"),
            Self::Transcription(msg) | Self::Setup(msg) => write!(f, "transcription failed: {msg}"),
            Self::Injection(msg) => write!(f, "text injection failed: {msg}"),
            Self::Memo(msg) => write!(f, "voice memo failed: {msg}"),
            Self::Rewrite(msg) => f.write_str(msg),
            Self::Install(msg) => write!(f, "install failed: {msg}"),
            Self::Host(msg) => write!(f, "background host failed: {msg}"),
        }
    }
}

impl std::error::Error for Error {}
