//! The text of a key combination, as a hotkey field writes it.
//!
//! The background process reads this text back with the parser of the
//! engine, so the two must agree on every name. A test in the `vinput`
//! crate, which sees both, holds them to it.

/// Which modifier keys are held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[expect(clippy::struct_excessive_bools, reason = "one flag per modifier key")]
pub struct Modifiers {
    /// Ctrl.
    pub ctrl: bool,
    /// Alt.
    pub alt: bool,
    /// Shift.
    pub shift: bool,
    /// The Windows key.
    pub win: bool,
}

impl Modifiers {
    /// Whether none is held.
    #[must_use]
    pub fn is_empty(self) -> bool {
        !(self.ctrl || self.alt || self.shift || self.win)
    }

    /// The held ones as text, each followed by a plus: `Ctrl+Shift+`.
    #[must_use]
    pub fn prefix(self) -> String {
        let mut text = String::new();
        for (held, name) in [
            (self.ctrl, "Ctrl+"),
            (self.alt, "Alt+"),
            (self.shift, "Shift+"),
            (self.win, "Win+"),
        ] {
            if held {
                text.push_str(name);
            }
        }
        text
    }
}

/// Keys with a name of their own, by virtual-key code.
const NAMED: [(&str, u16); 19] = [
    ("Space", 0x20),
    ("Enter", 0x0D),
    ("Tab", 0x09),
    ("Esc", 0x1B),
    ("Backspace", 0x08),
    ("Insert", 0x2D),
    ("Delete", 0x2E),
    ("Home", 0x24),
    ("End", 0x23),
    ("PageUp", 0x21),
    ("PageDown", 0x22),
    ("Left", 0x25),
    ("Up", 0x26),
    ("Right", 0x27),
    ("Down", 0x28),
    ("Pause", 0x13),
    ("CapsLock", 0x14),
    ("ScrollLock", 0x91),
    ("PrintScreen", 0x2C),
];

/// Virtual-key code of Esc.
pub const VK_ESCAPE: u16 = 0x1B;
/// Virtual-key code of Backspace.
pub const VK_BACK: u16 = 0x08;
/// Virtual-key code of Delete.
pub const VK_DELETE: u16 = 0x2E;
const VK_F1: u16 = 0x70;
const VK_F24: u16 = 0x87;
const VK_NUMPAD0: u16 = 0x60;
const VK_NUMPAD9: u16 = 0x69;

/// Whether `vk` is Shift, Ctrl, Alt or the Windows key, on either side: a
/// key that is held with another rather than pressed for itself.
#[must_use]
pub const fn is_modifier(vk: u16) -> bool {
    matches!(vk, 0x10..=0x12 | 0x5B | 0x5C | 0xA0..=0xA5)
}

/// The name of the key with virtual-key code `vk`, or `None` for a modifier
/// key and for codes that are not keys.
#[must_use]
pub fn key_name(vk: u16) -> Option<String> {
    if is_modifier(vk) || !(1..=254).contains(&vk) {
        return None;
    }
    if let Some((name, _)) = NAMED.iter().find(|(_, code)| *code == vk) {
        return Some((*name).to_owned());
    }
    Some(match vk {
        VK_F1..=VK_F24 => format!("F{}", vk - VK_F1 + 1),
        VK_NUMPAD0..=VK_NUMPAD9 => format!("Numpad{}", vk - VK_NUMPAD0),
        // Letters and digits have the codes of their characters.
        0x30..=0x39 | 0x41..=0x5A => char::from(u8::try_from(vk).unwrap_or(b'?')).to_string(),
        _ => format!("VK{vk}"),
    })
}

/// Whether the key `vk` types or edits text when it is pressed by itself or
/// with Shift: letters, digits, punctuation, the number pad, Space, Enter,
/// Tab, Esc, Backspace, Delete and the keys that move the caret.
const fn types(vk: u16) -> bool {
    matches!(
        vk,
        0x08 | 0x09 | 0x0D | 0x1B | 0x20..=0x28 | 0x2E | 0x30..=0x39 | 0x41..=0x5A
            | VK_NUMPAD0..=0x6F | 0xBA..=0xC0 | 0xDB..=0xDF | 0xE2
    )
}

/// Whether `vk` is a code that arrives like a key but is none: a mouse
/// button, a key of an input method, the stand-in for text that was not
/// typed on keys.
const fn is_key(vk: u16) -> bool {
    !matches!(vk, 0x01..=0x07 | 0x15..=0x1A | 0x1C..=0x1F | 0xE5 | 0xE7)
}

/// Whether Windows keeps the combination for itself, so that it can never be
/// a hotkey of a program: switching windows, the task manager, the window
/// menu, closing a window, locking the PC.
const fn reserved(modifiers: Modifiers, vk: u16) -> bool {
    const TAB: u16 = 0x09;
    const SPACE: u16 = 0x20;
    const F4: u16 = 0x73;
    const L: u16 = 0x4C;
    match vk {
        TAB => modifiers.alt,
        VK_ESCAPE => modifiers.alt || modifiers.ctrl,
        SPACE | F4 => modifiers.alt && !modifiers.ctrl && !modifiers.win,
        VK_DELETE => modifiers.ctrl && modifiers.alt,
        L => modifiers.win && !modifiers.ctrl && !modifiers.alt && !modifiers.shift,
        _ => false,
    }
}

/// Whether the combination of `modifiers` and the key `vk` may be made a
/// hotkey by pressing it in a hotkey field.
///
/// A hotkey works everywhere, so a key that types or edits text needs Ctrl,
/// Alt or the Windows key with it: with `A` or `Shift+A` for a hotkey, that
/// letter could not be typed any more. Function keys and the like may stand
/// alone. Combinations Windows keeps for itself are never offered.
#[must_use]
pub const fn capturable(modifiers: Modifiers, vk: u16) -> bool {
    if is_modifier(vk) || !is_key(vk) || vk == 0 || vk > 254 || reserved(modifiers, vk) {
        return false;
    }
    modifiers.ctrl || modifiers.alt || modifiers.win || !types(vk)
}

/// The text of the combination of `modifiers` and the key `vk`, such as
/// `Ctrl+Shift+Space`, or `None` if `vk` is not a key to end one with.
#[must_use]
pub fn name(modifiers: Modifiers, vk: u16) -> Option<String> {
    key_name(vk).map(|key| modifiers.prefix() + &key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combinations_read_modifiers_first_in_a_fixed_order() {
        let all = Modifiers {
            ctrl: true,
            alt: true,
            shift: true,
            win: true,
        };
        assert_eq!(name(all, 0x20).as_deref(), Some("Ctrl+Alt+Shift+Win+Space"));
        assert_eq!(name(Modifiers::default(), 0x70).as_deref(), Some("F1"));
        assert_eq!(name(Modifiers::default(), 0x87).as_deref(), Some("F24"));
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        assert_eq!(name(shift, 0x4D).as_deref(), Some("Shift+M"));
        assert_eq!(name(Modifiers::default(), 0x67).as_deref(), Some("Numpad7"));
        assert_eq!(name(Modifiers::default(), 0xBA).as_deref(), Some("VK186"));
    }

    #[test]
    fn keys_that_type_need_a_strong_modifier_to_be_a_hotkey() {
        let none = Modifiers::default();
        let shift = Modifiers {
            shift: true,
            ..none
        };
        let ctrl = Modifiers { ctrl: true, ..none };
        let alt = Modifiers { alt: true, ..none };
        let win = Modifiers { win: true, ..none };
        // Function keys and the keys nobody types with stand alone.
        for vk in [0x70, 0x87, 0x13, 0x91, 0x2C, 0x2D, 0x14, 0xB3] {
            assert!(capturable(none, vk), "vk {vk:#x}");
        }
        // Letters, digits, punctuation, Space, Enter, the arrows do not.
        for vk in [
            0x41, 0x5A, 0x30, 0x20, 0x0D, 0x25, 0x24, 0x2E, 0x60, 0x6B, 0xBA, 0xE2,
        ] {
            assert!(!capturable(none, vk), "vk {vk:#x}");
            assert!(!capturable(shift, vk), "shift vk {vk:#x}");
            assert!(capturable(ctrl, vk), "ctrl vk {vk:#x}");
            assert!(capturable(win, vk) || vk == 0x4C, "win vk {vk:#x}");
        }
        assert!(capturable(alt, 0x41));
        // A modifier, a mouse button, an input method key: never.
        for vk in [
            0x10, 0x11, 0x12, 0x5B, 0xA5, 0x01, 0x05, 0x15, 0x19, 0xE5, 0xE7, 0, 255,
        ] {
            assert!(!capturable(ctrl, vk), "vk {vk:#x}");
        }
    }

    #[test]
    fn what_windows_keeps_for_itself_is_not_offered() {
        let none = Modifiers::default();
        let alt = Modifiers { alt: true, ..none };
        let ctrl = Modifiers { ctrl: true, ..none };
        let win = Modifiers { win: true, ..none };
        let ctrl_alt = Modifiers { ctrl: true, ..alt };
        let ctrl_shift = Modifiers {
            shift: true,
            ..ctrl
        };
        for (modifiers, vk) in [
            (alt, 0x09),
            (alt, 0x1B),
            (alt, 0x20),
            (alt, 0x73),
            (ctrl, 0x1B),
            (ctrl_shift, 0x1B),
            (ctrl_alt, 0x2E),
            (win, 0x4C),
        ] {
            assert!(!capturable(modifiers, vk), "{modifiers:?} vk {vk:#x}");
        }
        // Their neighbors are free.
        assert!(capturable(ctrl, 0x73));
        assert!(capturable(ctrl_alt, 0x4C));
        assert!(capturable(ctrl, 0x20));
    }

    #[test]
    fn a_modifier_alone_is_not_a_combination() {
        for vk in [0x10, 0x11, 0x12, 0x5B, 0x5C, 0xA0, 0xA5, 0, 255] {
            assert_eq!(name(Modifiers::default(), vk), None, "vk {vk}");
        }
        assert!(Modifiers::default().is_empty());
        assert_eq!(
            Modifiers {
                ctrl: true,
                win: true,
                ..Modifiers::default()
            }
            .prefix(),
            "Ctrl+Win+"
        );
    }
}
