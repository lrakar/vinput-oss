//! The Vinput background process on Windows: hotkeys, the tray icon, the
//! listening indicator, the line to the window processes, and setup.
//!
//! Everything that listens and types is in `vinput_core`, and the windows
//! are drawn by a process of their own that exists only while one is open;
//! this crate is the part that stays, and that touches the desktop. It owns
//! every piece of state the windows show: [`link`] is the line to a window
//! process, over which the host sends pictures of that state and hears what
//! the user did.

#[cfg(windows)]
mod autostart;
#[cfg(windows)]
mod caret;
#[cfg(windows)]
mod change;
pub mod draw;
#[cfg(windows)]
mod finder;
#[cfg(windows)]
mod folder;
#[cfg(windows)]
mod indicator;
#[cfg(windows)]
pub mod install;
pub mod link;
#[cfg(windows)]
mod menu;
#[cfg(windows)]
mod shortcut;
#[cfg(windows)]
mod view;
#[cfg(windows)]
mod win;
#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::{
    Launch, activate_running_instance, is_running, register_installed, run, show_error,
    stop_running_instance, unregister,
};
