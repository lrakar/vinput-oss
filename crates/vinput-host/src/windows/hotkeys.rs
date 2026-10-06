//! The hotkeys: which key starts which kind of session, registering them
//! with Windows, and letting go of them while a field in Settings wants to
//! see the keys for itself.
//!
//! Hotkeys are registered with the system rather than read through a keyboard
//! hook, so Windows delivers them to Vinput exclusively and nothing Vinput
//! does can slow down ordinary typing. They are registered for the thread,
//! not for a window, so a press arrives in the message loop as a thread
//! message.
//!
//! Escape calls a dictation off, and is a hotkey like the others, so that
//! there is still no hook. But Escape belongs to every program, and a
//! registered hotkey is taken from all of them. So it is registered only
//! while a session listens or is still at work on its last words, and let
//! go of the moment none is ([`Hotkeys::hold_cancel`]): the rest of the time
//! Escape does everywhere what it always did.
#![allow(unsafe_code)]

use std::ptr::null_mut;

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey, UnregisterHotKey,
};

use super::Host;
use super::tray::About;
use vinput_core::keys::Hotkey;
use vinput_core::log;
use vinput_core::session::{Controller, Target};
use vinput_core::settings::Settings;
use vinput_view::Page;

const HOTKEY_ID: i32 = 1;
const HOTKEY_ENTER_ID: i32 = 2;
const HOTKEY_MEMO_ID: i32 = 3;
const HOTKEY_SEARCH_ID: i32 = 4;
/// Escape, while there is a dictation to call off.
const HOTKEY_CANCEL_ID: i32 = 5;
/// The virtual key code of Escape.
const VK_ESCAPE: u32 = 0x1B;
/// An id no hotkey of ours has, for asking Windows whether a key is free.
const HOTKEY_PROBE_ID: i32 = 99;

/// Every hotkey with the kind of session it starts.
fn table(settings: &Settings) -> [(i32, Target, Option<Hotkey>); 4] {
    [
        (HOTKEY_ID, Target::Focused, Some(settings.hotkey)),
        (HOTKEY_ENTER_ID, Target::FocusedEnter, settings.hotkey_enter),
        (HOTKEY_SEARCH_ID, Target::Search, settings.hotkey_search),
        (HOTKEY_MEMO_ID, Target::Memo, settings.hotkey_memo),
    ]
}

/// What the hotkey Windows reports as `id` starts, and the key itself to
/// watch for its release. `None` for an id that is not one of ours.
pub(super) fn pressed(settings: &Settings, id: usize) -> Option<(Target, Option<Hotkey>)> {
    table(settings)
        .into_iter()
        .find(|(hotkey_id, _, _)| i32::try_from(id) == Ok(*hotkey_id))
        .map(|(_, target, hotkey)| (target, hotkey))
}

fn modifiers(hotkey: Hotkey) -> u32 {
    [
        (hotkey.ctrl, MOD_CONTROL),
        (hotkey.alt, MOD_ALT),
        (hotkey.shift, MOD_SHIFT),
        (hotkey.win, MOD_WIN),
    ]
    .into_iter()
    .filter(|(held, _)| *held)
    .fold(0, |flags, (_, flag)| flags | flag)
}

/// Whether Windows would give us `hotkey`: no other program holds it. Asked
/// by registering it for a moment under an id no hotkey of ours has.
pub(super) fn is_free(hotkey: Hotkey) -> bool {
    // SAFETY: plain value arguments; what was registered here is let go of
    // again before anything else happens on this thread.
    unsafe {
        let free = RegisterHotKey(
            null_mut(),
            HOTKEY_PROBE_ID,
            modifiers(hotkey) | MOD_NOREPEAT,
            u32::from(hotkey.vk),
        ) != 0;
        if free {
            UnregisterHotKey(null_mut(), HOTKEY_PROBE_ID);
        }
        free
    }
}

/// Whether the hotkey Windows reports as `id` is the one that calls a
/// dictation off.
pub(super) fn is_cancel(id: usize) -> bool {
    i32::try_from(id) == Ok(HOTKEY_CANCEL_ID)
}

/// The hotkeys Windows has given this thread.
#[derive(Debug, Default)]
pub(super) struct Hotkeys {
    registered: Vec<i32>,
    /// Whether Escape is registered.
    cancel: bool,
}

impl Hotkeys {
    /// Registers the hotkeys of `settings`. Returns the ones Windows
    /// refused, which another program holds.
    pub(super) fn register(&mut self, settings: &Settings) -> Vec<Hotkey> {
        // Registering a key twice would be refused like one somebody else
        // holds, so whatever is registered goes first.
        self.unregister();
        let mut taken = Vec::new();
        for (id, _, hotkey) in table(settings) {
            let Some(hotkey) = hotkey else { continue };
            // A null window makes WM_HOTKEY a thread message, read by the loop.
            // SAFETY: plain value arguments.
            let ok = unsafe {
                RegisterHotKey(
                    null_mut(),
                    id,
                    modifiers(hotkey) | MOD_NOREPEAT,
                    u32::from(hotkey.vk),
                )
            };
            if ok == 0 {
                taken.push(hotkey);
            } else {
                self.registered.push(id);
            }
        }
        taken
    }

    /// Takes Escape while there is something to call off, and gives it back
    /// when there is not. Asked for what it already has, it does nothing.
    pub(super) fn hold_cancel(&mut self, wanted: bool) {
        if wanted == self.cancel {
            return;
        }
        if wanted {
            // SAFETY: plain value arguments.
            let ok =
                unsafe { RegisterHotKey(null_mut(), HOTKEY_CANCEL_ID, MOD_NOREPEAT, VK_ESCAPE) };
            if ok == 0 {
                // Another program holds Escape as a hotkey of its own.
                // Asked again at the next session, and said once in the log.
                log::write("Escape is taken by another program and will not cancel dictation");
                return;
            }
        } else {
            // SAFETY: the id was registered on this thread.
            unsafe { UnregisterHotKey(null_mut(), HOTKEY_CANCEL_ID) };
        }
        self.cancel = wanted;
    }

    /// Whether Escape is held.
    #[cfg(test)]
    pub(super) fn holds_cancel(&self) -> bool {
        self.cancel
    }

    /// Lets go of every hotkey that starts a session. Escape is not one of
    /// them and follows the sessions alone.
    pub(super) fn unregister(&mut self) {
        for id in self.registered.drain(..) {
            // SAFETY: `id` was registered on this thread.
            unsafe { UnregisterHotKey(null_mut(), id) };
        }
    }
}

impl Host {
    /// Registers the hotkeys and tells the user about the ones another
    /// program holds.
    pub(super) fn register_hotkeys(&mut self) {
        let taken = self.hotkeys.register(&self.settings);
        if !taken.is_empty() {
            let keys = taken
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" and ");
            log::write(&format!("could not register hotkey {keys}"));
            self.tray.warn(
                About::Settings(Page::General),
                "Hotkey unavailable",
                &format!("{keys} is used by another program. Click here to pick another key."),
            );
        }
    }

    /// Lets go of the hotkeys so that a hotkey field can see the keys.
    pub(super) fn start_capturing(&mut self, controller: &mut dyn Controller) {
        if self.capturing {
            return;
        }
        // A dictation that only its key could stop is stopped now.
        if self.trigger.is_running() {
            self.stop(controller);
            self.trigger.session_ended();
        }
        self.hotkeys.unregister();
        self.capturing = true;
    }

    /// Takes the hotkeys back from a hotkey field.
    pub(super) fn stop_capturing(&mut self) {
        if self.capturing {
            self.capturing = false;
            self.register_hotkeys();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_flags_follow_the_hotkey() {
        assert_eq!(modifiers(Hotkey::plain(0x70)), 0);
        let hotkey = Hotkey {
            ctrl: true,
            shift: true,
            ..Hotkey::plain(0x20)
        };
        assert_eq!(modifiers(hotkey), MOD_CONTROL | MOD_SHIFT);
    }

    #[test]
    fn escape_is_held_only_while_asked_for() {
        let mut hotkeys = Hotkeys::default();
        assert!(!hotkeys.holds_cancel());
        hotkeys.hold_cancel(false);
        assert!(!hotkeys.holds_cancel());
        // Whether Windows gives it depends on what else runs on this PC;
        // given or not, letting go leaves nothing held.
        hotkeys.hold_cancel(true);
        let held = hotkeys.holds_cancel();
        hotkeys.hold_cancel(true);
        assert_eq!(hotkeys.holds_cancel(), held);
        // Letting go of the keys that start sessions leaves Escape alone.
        hotkeys.unregister();
        assert_eq!(hotkeys.holds_cancel(), held);
        hotkeys.hold_cancel(false);
        assert!(!hotkeys.holds_cancel());
    }

    #[test]
    fn each_hotkey_starts_its_own_kind_of_session() {
        let settings = Settings {
            hotkey_memo: Some(Hotkey::plain(0x78)),
            ..Settings::default()
        };
        assert_eq!(
            pressed(&settings, 1),
            Some((Target::Focused, Some(settings.hotkey)))
        );
        assert_eq!(
            pressed(&settings, 2),
            Some((Target::FocusedEnter, settings.hotkey_enter))
        );
        assert_eq!(
            pressed(&settings, 3),
            Some((Target::Memo, Some(Hotkey::plain(0x78))))
        );
        // The id used to ask whether a key is free starts nothing, and
        // neither does Escape, which only calls off.
        assert_eq!(pressed(&settings, 99), None);
        assert_eq!(pressed(&settings, 5), None);
        assert!(is_cancel(5) && !is_cancel(1) && !is_cancel(99) && !is_cancel(usize::MAX));
        assert_eq!(pressed(&settings, usize::MAX), None);
        let ids: Vec<i32> = table(&settings).iter().map(|(id, _, _)| *id).collect();
        assert!(!ids.contains(&HOTKEY_PROBE_ID) && !ids.contains(&HOTKEY_CANCEL_ID));
        let mut distinct = ids.clone();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(distinct.len(), ids.len());
    }
}
