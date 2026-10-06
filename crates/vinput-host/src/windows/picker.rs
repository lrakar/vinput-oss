//! The file and folder pickers of Windows, shown for a window process.
//!
//! A picker is modal: it runs a message loop of its own until the user is
//! done with it. So it gets a thread of its own, and hotkeys and everything
//! else go on while it is open; the host is woken with what was picked.
#![allow(unsafe_code)]

use std::path::PathBuf;
use std::ptr::null_mut;
use std::sync::{Arc, PoisonError};
use std::thread;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowThreadProcessId, PostMessageW,
};

use super::children::Which;
use super::{Host, PICKED};
use crate::folder;
use vinput_core::session::Controller;
use vinput_core::{log, memo};
use vinput_view as wire;

/// What a picker of Windows is asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Pick {
    /// The folder for voice memos.
    MemoDir,
    /// The file of a model to add.
    ModelFile,
    /// A service account key file, for the setup of Google Cloud Speech.
    GoogleKey,
}

/// What the user picked, or `None` inside if they picked nothing.
pub(super) enum Picked {
    /// A folder for voice memos.
    MemoDir(Option<PathBuf>),
    /// The file of a model to add.
    ModelFile(Option<PathBuf>),
    /// A service account key file.
    GoogleKey(Option<PathBuf>),
}

impl Host {
    /// The Settings window, if it is the window in front: the one a picker
    /// belongs to. Null if something else is in front.
    fn window_in_front(&self) -> HWND {
        let settings = self.windows.process_id(Which::Settings);
        let mut process = 0u32;
        // SAFETY: plain calls; `process` is writable, and a window that has
        // gone in the meantime leaves it at zero.
        let front = unsafe {
            let front = GetForegroundWindow();
            GetWindowThreadProcessId(front, &raw mut process);
            front
        };
        if settings == Some(process) {
            front
        } else {
            null_mut()
        }
    }

    /// Asks the user for a folder or a file with the picker of Windows. The
    /// picker runs on a thread of its own, so that hotkeys and everything
    /// else go on while it is open; the host is woken with what was picked.
    pub(super) fn pick(&mut self, what: Pick) {
        if self.picking.is_some() {
            return;
        }
        // Window handles are addresses that any thread may use.
        let (owner, hwnd) = (self.window_in_front() as usize, self.hwnd as usize);
        let start = memo::folder(&self.settings.memo_dir);
        let picked = Arc::clone(&self.picked);
        let spawned = thread::Builder::new()
            .name("vinput-picker".into())
            .spawn(move || {
                let found = match what {
                    Pick::MemoDir => Picked::MemoDir(folder::pick(owner as _, &start)),
                    Pick::ModelFile => {
                        Picked::ModelFile(folder::pick_file(owner as _, folder::File::Model))
                    }
                    Pick::GoogleKey => {
                        Picked::GoogleKey(folder::pick_file(owner as _, folder::File::GoogleKey))
                    }
                };
                *picked.lock().unwrap_or_else(PoisonError::into_inner) = Some(found);
                // SAFETY: posting to a window that has gone away fails harmlessly.
                unsafe { PostMessageW(hwnd as HWND, PICKED, 0, 0) };
            });
        match spawned {
            Ok(_) => self.picking = Some(what),
            Err(err) => log::write(&format!("starting the picker failed: {err}")),
        }
    }

    /// Puts what the user picked to use, if a picker has closed.
    pub(super) fn on_picked(&mut self, controller: &mut dyn Controller) {
        let picked = self
            .picked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(picked) = picked else { return };
        self.picking = None;
        match picked {
            Picked::MemoDir(Some(folder)) => {
                let folder = folder.to_string_lossy().into_owned();
                self.set(wire::Change::MemoDir(folder), controller);
            }
            // With Settings closed meanwhile, nobody waits for the answer.
            Picked::ModelFile(Some(file)) if self.windows.is_open(Which::Settings) => {
                self.finder.find(&file.to_string_lossy());
            }
            Picked::GoogleKey(Some(file)) if self.windows.is_open(Which::Settings) => {
                self.google_key_picked(&file);
            }
            Picked::MemoDir(None) | Picked::ModelFile(_) | Picked::GoogleKey(_) => {}
        }
        self.show_state();
    }
}
