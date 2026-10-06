//! Hotkey descriptions and their text form, such as `F1` or `Ctrl+Shift+Space`.

use std::fmt;
use std::str::FromStr;

/// A key with optional modifiers. `vk` is a Windows virtual-key code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(clippy::struct_excessive_bools, reason = "one flag per modifier key")]
pub struct Hotkey {
    /// Whether Ctrl must be held.
    pub ctrl: bool,
    /// Whether Alt must be held.
    pub alt: bool,
    /// Whether Shift must be held.
    pub shift: bool,
    /// Whether the Windows key must be held.
    pub win: bool,
    /// Virtual-key code of the main key.
    pub vk: u16,
}

impl Hotkey {
    /// A key with no modifiers.
    #[must_use]
    pub const fn plain(vk: u16) -> Self {
        Self {
            ctrl: false,
            alt: false,
            shift: false,
            win: false,
            vk,
        }
    }
}

/// Keys with a readable name. Letters and digits are handled separately.
const NAMED_KEYS: &[(&str, u16)] = &[
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
const VK_F1: u16 = 0x70;
const VK_F24: u16 = 0x87;
const VK_NUMPAD0: u16 = 0x60;
const VK_NUMPAD9: u16 = 0x69;

fn key_name(vk: u16) -> String {
    if let Some((name, _)) = NAMED_KEYS.iter().find(|(_, code)| *code == vk) {
        return (*name).to_owned();
    }
    match vk {
        VK_F1..=VK_F24 => format!("F{}", vk - VK_F1 + 1),
        VK_NUMPAD0..=VK_NUMPAD9 => format!("Numpad{}", vk - VK_NUMPAD0),
        // Letters and digits share their ASCII codes.
        0x30..=0x39 | 0x41..=0x5A => u8::try_from(vk).map(char::from).unwrap_or('?').to_string(),
        _ => format!("VK{vk}"),
    }
}

fn key_code(name: &str) -> Option<u16> {
    if let Some((_, code)) = NAMED_KEYS
        .iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(name))
    {
        return Some(*code);
    }
    let upper = name.to_ascii_uppercase();
    let number_after = |prefix: &str| upper.strip_prefix(prefix)?.parse::<u16>().ok();
    if let Some(n) = number_after("NUMPAD").filter(|n| *n <= 9) {
        return Some(VK_NUMPAD0 + n);
    }
    if let Some(vk) = number_after("VK").filter(|vk| (1..=254).contains(vk)) {
        return Some(vk);
    }
    if let Some(n) = number_after("F").filter(|n| (1..=24).contains(n)) {
        return Some(VK_F1 + n - 1);
    }
    match upper.as_bytes() {
        [c @ (b'0'..=b'9' | b'A'..=b'Z')] => Some(u16::from(*c)),
        _ => None,
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (held, name) in [
            (self.ctrl, "Ctrl"),
            (self.alt, "Alt"),
            (self.shift, "Shift"),
            (self.win, "Win"),
        ] {
            if held {
                write!(f, "{name}+")?;
            }
        }
        f.write_str(&key_name(self.vk))
    }
}

/// The text was not a recognizable hotkey.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseHotkeyError(String);

impl fmt::Display for ParseHotkeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unrecognized hotkey {:?}", self.0)
    }
}

impl std::error::Error for ParseHotkeyError {}

impl FromStr for Hotkey {
    type Err = ParseHotkeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let error = || ParseHotkeyError(s.to_owned());
        let mut parts: Vec<&str> = s.split('+').map(str::trim).collect();
        let key = parts
            .pop()
            .filter(|key| !key.is_empty())
            .ok_or_else(error)?;
        let mut hotkey = Self::plain(key_code(key).ok_or_else(error)?);
        for modifier in parts {
            match modifier.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => hotkey.ctrl = true,
                "alt" => hotkey.alt = true,
                "shift" => hotkey.shift = true,
                "win" | "windows" => hotkey.win = true,
                _ => return Err(error()),
            }
        }
        Ok(hotkey)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_function_keys_round_trip() {
        assert_eq!("F1".parse(), Ok(Hotkey::plain(0x70)));
        assert_eq!("f24".parse(), Ok(Hotkey::plain(0x87)));
        assert_eq!(Hotkey::plain(0x71).to_string(), "F2");
    }

    #[test]
    fn modifiers_parse_in_any_order_and_print_in_a_fixed_one() {
        let parsed: Result<Hotkey, _> = "shift + CTRL+space".parse();
        let expected = Hotkey {
            ctrl: true,
            shift: true,
            ..Hotkey::plain(0x20)
        };
        assert_eq!(parsed, Ok(expected));
        assert_eq!(expected.to_string(), "Ctrl+Shift+Space");
    }

    #[test]
    fn every_virtual_key_round_trips() {
        for vk in 1..=254u16 {
            let hotkey = Hotkey {
                alt: true,
                ..Hotkey::plain(vk)
            };
            assert_eq!(hotkey.to_string().parse(), Ok(hotkey), "vk {vk}");
        }
    }

    #[test]
    fn nonsense_is_rejected() {
        for bad in ["", "Ctrl+", "Hyper+F1", "F25", "Numpad10", "ab"] {
            assert!(bad.parse::<Hotkey>().is_err(), "{bad:?}");
        }
    }
}
