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
//! which is how everything else is built: outside this crate. Two things it
//! does with a dictation besides typing it: a web [`search`], and having a
//! language model tidy it, or edit a selection the way it says
//! ([`rewrite`]).

pub mod actions;
pub mod api;
pub mod audio;
#[cfg(windows)]
pub mod dictation;
pub mod hardware;
pub mod inject;
pub mod instance;
pub mod memo;
pub mod models;
pub mod pipeline;
pub mod rewrite;
pub mod search;
pub mod session;
pub mod settings;
pub mod sound;
pub mod stt;
pub mod words;

// What Vinput shares with the programs beside it lives in `putlab-core`
// and goes by the names it always had here.
#[cfg(windows)]
pub use putlab_core::job;
pub use putlab_core::{
    Error, Result, cloud, download, error, gpu, hub, keys, languages, log, secrets, update,
};
pub(crate) use putlab_core::{net, suspend};
