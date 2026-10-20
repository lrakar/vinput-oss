//! Canned data for looking at the windows without the engine.
//!
//! A [`scene`] is a [`View`] that puts a window in one particular state: a
//! new user, a download under way, an update on offer. [`Fake`] is a
//! [`Backend`] over a scene that answers commands the way the real one
//! would, as far as the picture goes, so a window can be clicked through.
//! Each program of the family has its scenes in a file of its own.

#[cfg(feature = "hub")]
pub mod hub;
#[cfg(feature = "voutput")]
pub mod reader;
#[cfg(feature = "vinput")]
mod vinput;

use std::sync::Mutex;

use vinput_view::{Command, Event, View};

#[cfg(feature = "vinput")]
pub use vinput::SCENES;

use crate::{Backend, Listener};

/// The names [`scene`] knows of Vinput's windows: none in a program that
/// has none of them.
#[cfg(not(feature = "vinput"))]
pub const SCENES: [&str; 0] = [];

/// Every name [`scene`] knows, of every program whose pages this build has.
#[must_use]
pub fn names() -> Vec<&'static str> {
    #[allow(unused_mut)]
    let mut names: Vec<&'static str> = SCENES.to_vec();
    #[cfg(feature = "hub")]
    names.extend(hub::SCENES);
    #[cfg(feature = "voutput")]
    names.extend(reader::SCENES);
    names
}

/// The view that puts a window in the state `name` names, or `None` if no
/// scene has that name; see [`names`].
#[must_use]
pub fn scene(name: &str) -> Option<View> {
    #[cfg(feature = "vinput")]
    if let Some(view) = vinput::scene(name) {
        return Some(view);
    }
    #[cfg(feature = "hub")]
    if let Some(view) = hub::scene(name) {
        return Some(view);
    }
    #[cfg(feature = "voutput")]
    if let Some(view) = reader::scene(name) {
        return Some(view);
    }
    let _ = name;
    None
}

/// A backend with no engine behind it: it keeps a [`View`], changes it the
/// way the real backend would change the picture, and sends it back.
pub struct Fake {
    view: Mutex<View>,
    listener: Listener,
}

impl std::fmt::Debug for Fake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fake").finish_non_exhaustive()
    }
}

impl Fake {
    /// A fake that starts from `view` and reports to `listener`.
    #[must_use]
    pub fn new(view: View, listener: Listener) -> Self {
        Self {
            view: Mutex::new(view),
            listener,
        }
    }
}

/// Changes `view` the way the real backend would after `command`. Returns
/// whether the window should close.
fn apply(view: &mut View, command: Command) -> bool {
    match command {
        #[cfg(feature = "hub")]
        Command::Hub(ask) => {
            hub::apply(view, ask);
            false
        }
        #[cfg(feature = "voutput")]
        Command::Reader(ask) => {
            reader::apply(view, ask);
            false
        }
        #[cfg(feature = "vinput")]
        command => vinput::apply(view, command),
        #[cfg(not(feature = "vinput"))]
        Command::Closed => true,
        #[cfg(not(feature = "vinput"))]
        _ => {
            let _ = view;
            false
        }
    }
}

impl Backend for Fake {
    fn send(&self, command: Command) {
        let Ok(mut view) = self.view.lock() else {
            return;
        };
        if apply(&mut view, command) {
            (self.listener)(Event::Close);
        } else {
            (self.listener)(Event::State(Box::new(view.clone())));
        }
    }
}
