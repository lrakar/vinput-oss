//! Vinput types speech into whatever text input currently has focus.
//!
//! This crate is the engine: everything that listens, recognizes and types,
//! and nothing that shows a window. The crates around it are `vinput-host`,
//! the background process with its hotkeys, tray icon and indicator, which
//! drives the engine through a [`session::Controller`]; `vinput-view`, the
//! contract between that process and its windows; `vinput-ui`, the windows;
//! and `vinput`, the thin binary on top.
//!
//! Each stage of the dictation pipeline is a trait, so the orchestration in
//! [`pipeline`] is platform-independent and testable with fakes, while the
//! Windows code and the speech providers stay behind those traits.
//!
//! ```text
//! AudioSource -> Transcriber -> TextSink
//! (microphone)   (provider)     (focused window)
//! ```
//!
//! Vinput itself only types and records. A session can instead hand its text,
//! or a voice [`memo`], to whatever program is connected to the local [`api`],
//! which is how everything else is built: outside this crate. The one thing
//! it does with a dictation besides typing it is a web [`search`].

pub mod api;
pub mod audio;
pub mod cloud;
#[cfg(windows)]
pub mod dictation;
pub mod download;
pub mod error;
pub mod gpu;
pub mod inject;
pub mod instance;
#[cfg(windows)]
pub mod job;
pub mod keys;
pub mod log;
pub mod memo;
pub mod models;
mod net;
pub mod pipeline;
pub mod search;
pub mod secrets;
pub mod session;
pub mod settings;
pub mod sound;
pub mod stt;
mod suspend;
pub mod update;

pub use error::{Error, Result};
