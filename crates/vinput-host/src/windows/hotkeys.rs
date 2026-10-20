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
//!
//! Enter ends a dictation that types where the caret is, and is a hotkey
//! too, for as long as such a dictation listens ([`hold_finish`]). It has to
//! be taken from the program it was pressed in: left alone, it would get
//! there before the last words do, and send half a message. The dictation
//! presses it again after its last words. A hotkey is taken from everyone,
//! though, the dictation included: an Enter it types would come back here as
//! a key press and never arrive. So the key is let go of around every Enter
//! a session types, and taken back after ([`typing_enter`]).
#![allow(unsafe_code)]

use std::cell::Cell;
use std::ptr::null_mut;

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey, UnregisterHotKey,
};

use super::Host;
use super::tray::About;
use vinput_core::actions::MAX_ACTIONS;
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
/// Enter, while a dictation that types at the caret listens.
const HOTKEY_FINISH_ID: i32 = 6;
/// The virtual key code of Enter.
const VK_RETURN: u32 = 0x0D;
/// The key that edits the selected text.
const HOTKEY_COMMAND_ID: i32 = 7;
/// An id no hotkey of ours has, for asking Windows whether a key is free.
const HOTKEY_PROBE_ID: i32 = 99;
/// The key of the first action of the user's own; the others follow.
const HOTKEY_ACTION_ID: i32 = 20;

/// Every hotkey with the kind of session it starts.
fn table(settings: &Settings) -> Vec<(i32, Target, Option<Hotkey>)> {
    let actions = (settings.actions.iter())
        .take(MAX_ACTIONS)
        .zip(0u8..)
        .map(|(action, slot)| {
            let id = HOTKEY_ACTION_ID + i32::from(slot);
            (id, Target::Action(slot), action.hotkey)
        });
    [
        (HOTKEY_ID, Target::Focused, Some(settings.hotkey)),
        (HOTKEY_ENTER_ID, Target::FocusedEnter, settings.hotkey_enter),
        (HOTKEY_SEARCH_ID, Target::Search, settings.hotkey_search),
        (HOTKEY_MEMO_ID, Target::Memo, settings.hotkey_memo),
        (HOTKEY_COMMAND_ID, Target::Command, settings.hotkey_command),
    ]
    .into_iter()
    .chain(actions)
    .collect()
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

/// Whether the hotkey Windows reports as `id` is Enter, which ends a
/// dictation that types at the caret.
pub(super) fn is_finish(id: usize) -> bool {
    i32::try_from(id) == Ok(HOTKEY_FINISH_ID)
}

/// Whether Enter is ours, and why.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Finish {
    /// Whether the host wants the key: a dictation that types at the caret
    /// listens, or one that Enter ended is at its last words.
    wanted: bool,
    /// Whether a session is typing an Enter of its own this moment.
    typing: bool,
    /// Whether Windows has given it to us.
    held: bool,
}

impl Finish {
    /// Takes the key or lets go of it, as things stand now, with `take`,
    /// which says whether Windows gave it. Asked for what it already has, it
    /// does nothing.
    fn settle(&mut self, take: impl FnOnce(bool) -> bool) {
        let ours = self.wanted && !self.typing;
        if ours != self.held {
            self.held = take(ours) && ours;
        }
    }
}

thread_local! {
    /// How Enter stands on the thread of the message loop, the only one
    /// that asks: hotkeys belong to the thread that registers them.
    static FINISH: Cell<Finish> = const {
        Cell::new(Finish {
            wanted: false,
            typing: false,
            held: false,
        })
    };
}

/// Brings what Windows has given in line with `now`, and keeps it. Another
/// program that holds Enter as a hotkey of its own keeps it: asked again
/// the next time.
fn keep_finish(mut now: Finish) {
    now.settle(|take| {
        // SAFETY: plain value arguments; the id is let go of on the thread
        // that registered it.
        unsafe {
            if take {
                RegisterHotKey(null_mut(), HOTKEY_FINISH_ID, MOD_NOREPEAT, VK_RETURN) != 0
            } else {
                UnregisterHotKey(null_mut(), HOTKEY_FINISH_ID);
                false
            }
        }
    });
    FINISH.set(now);
}

/// Takes Enter while the host wants it, and gives it back when it does not.
pub(super) fn hold_finish(wanted: bool) {
    let mut finish = FINISH.get();
    finish.wanted = wanted;
    if !wanted {
        // Nobody is left who could still be typing one.
        finish.typing = false;
    }
    keep_finish(finish);
}

/// A session is about to type an Enter of its own (`typing`), or has typed
/// it: the key is not ours in between.
pub(super) fn typing_enter(typing: bool) {
    let mut finish = FINISH.get();
    finish.typing = typing;
    keep_finish(finish);
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
                About::Settings(Page::Shortcuts),
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
    use vinput_core::actions::Action;

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

    /// What Windows was asked for Enter, in order: taken or let go of.
    fn settled(finish: &mut Finish, given: bool, asked: &mut Vec<bool>) {
        finish.settle(|take| {
            asked.push(take);
            given
        });
    }

    #[test]
    fn enter_is_ours_while_wanted_and_never_while_a_session_types_one() {
        // Windows is not asked for the real key here: it is the one the
        // person at this PC is most likely to be pressing.
        let (mut finish, mut asked) = (Finish::default(), Vec::new());
        settled(&mut finish, true, &mut asked);
        assert!(asked.is_empty() && !finish.held);
        // A dictation listens: taken, once.
        finish.wanted = true;
        settled(&mut finish, true, &mut asked);
        settled(&mut finish, true, &mut asked);
        assert_eq!(asked, [true]);
        // A session types an Enter of its own, say the one a dictation
        // that was stopped before ends with: it is let through,
        finish.typing = true;
        settled(&mut finish, true, &mut asked);
        assert_eq!(asked, [true, false]);
        assert!(!finish.held);
        // and the key is taken back when it has been typed.
        finish.typing = false;
        settled(&mut finish, true, &mut asked);
        assert_eq!(asked, [true, false, true]);
        // The dictation is over: given back, and not asked for again.
        finish.wanted = false;
        settled(&mut finish, true, &mut asked);
        settled(&mut finish, true, &mut asked);
        assert_eq!(asked, [true, false, true, false]);

        // Another program holds Enter: asked again the next time, and
        // nothing is let go of that was never given.
        let (mut finish, mut asked) = (Finish::default(), Vec::new());
        finish.wanted = true;
        settled(&mut finish, false, &mut asked);
        assert!(!finish.held);
        settled(&mut finish, false, &mut asked);
        finish.wanted = false;
        settled(&mut finish, false, &mut asked);
        assert_eq!(asked, [true, true]);
    }

    #[test]
    fn each_hotkey_starts_its_own_kind_of_session() {
        let action = |vk| Action {
            hotkey: Some(Hotkey::plain(vk)),
            ..Action::default()
        };
        let settings = Settings {
            hotkey_memo: Some(Hotkey::plain(0x78)),
            hotkey_command: Some(Hotkey::plain(0x77)),
            actions: vec![action(0x75), Action::default(), action(0x7B)],
            ..Settings::default()
        };
        assert_eq!(
            pressed(&settings, 20),
            Some((Target::Action(0), Some(Hotkey::plain(0x75))))
        );
        assert_eq!(pressed(&settings, 21), Some((Target::Action(1), None)));
        assert_eq!(
            pressed(&settings, 22),
            Some((Target::Action(2), Some(Hotkey::plain(0x7B))))
        );
        assert_eq!(pressed(&settings, 23), None);
        assert_eq!(
            pressed(&settings, 7),
            Some((Target::Command, Some(Hotkey::plain(0x77))))
        );
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
        // Nor does Enter, which only ends one.
        assert_eq!(pressed(&settings, 6), None);
        assert!(is_finish(6) && !is_finish(2) && !is_finish(5) && !is_finish(usize::MAX));
        assert!(!is_cancel(6));
        assert_eq!(pressed(&settings, usize::MAX), None);
        let full = Settings {
            actions: vec![Action::default(); MAX_ACTIONS],
            ..settings
        };
        let ids: Vec<i32> = table(&full).iter().map(|(id, _, _)| *id).collect();
        assert_eq!(ids.len(), 5 + MAX_ACTIONS);
        assert!(!ids.contains(&HOTKEY_PROBE_ID) && !ids.contains(&HOTKEY_CANCEL_ID));
        assert!(!ids.contains(&HOTKEY_FINISH_ID));
        let mut distinct = ids.clone();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(distinct.len(), ids.len());
    }
}
