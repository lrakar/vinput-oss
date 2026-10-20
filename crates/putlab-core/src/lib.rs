//! What Vinput and the programs beside it share: plain HTTPS requests,
//! checked downloads, the update check, the account, the keys in the Windows
//! credential store, a look at the hardware of this PC, the names by which a
//! running copy is known, the text that is selected in the program in front
//! ([`selection`]), and how a model that writes is asked ([`llm`]).
//!
//! Nothing here listens, speaks or shows a window, and the only keys it
//! presses are the ones that copy a selection and paste over it. Each program of
//! the family adds that for itself: `vinput-core` is dictation, and the
//! programs under `apps/` are the hub and reading aloud. Which of them a
//! process is, it says once ([`product`]); without that every name is
//! Vinput's.

pub mod cloud;
pub mod download;
pub mod engine;
pub mod error;
pub mod folders;
pub mod gpu;
pub mod hardware;
pub mod hub;
pub mod instance;
#[cfg(windows)]
pub mod job;
pub mod keys;
pub mod languages;
pub mod llm;
pub mod log;
#[cfg(windows)]
pub mod menu;
pub mod net;
pub mod process;
pub mod product;
pub mod registry;
pub mod secrets;
pub mod selection;
pub mod shell;
pub mod suspend;
pub mod update;

pub use error::{Error, Result};
