//! The windows write hotkeys as text and the engine reads them back. This
//! holds the two to the same names: whatever a hotkey field can produce must
//! parse, mean the same keys, and print back as the same text.

use vinput_core::keys::Hotkey;
use vinput_ui::hotkey::{self, Modifiers};

fn every_modifier_set() -> impl Iterator<Item = Modifiers> {
    (0..16_u8).map(|bits| Modifiers {
        ctrl: bits & 1 != 0,
        alt: bits & 2 != 0,
        shift: bits & 4 != 0,
        win: bits & 8 != 0,
    })
}

#[test]
fn everything_a_hotkey_field_can_write_the_engine_reads_back_the_same() {
    let mut names = 0;
    for vk in 0..=255_u16 {
        for modifiers in every_modifier_set() {
            let Some(text) = hotkey::name(modifiers, vk) else {
                assert!(
                    hotkey::is_modifier(vk) || !(1..=254).contains(&vk),
                    "vk {vk} has no name"
                );
                continue;
            };
            let parsed: Hotkey = match text.parse() {
                Ok(parsed) => parsed,
                Err(error) => unreachable!("{text:?} does not parse: {error}"),
            };
            let expected = Hotkey {
                ctrl: modifiers.ctrl,
                alt: modifiers.alt,
                shift: modifiers.shift,
                win: modifiers.win,
                vk,
            };
            assert_eq!(parsed, expected, "{text}");
            assert_eq!(parsed.to_string(), text);
            names += 1;
        }
    }
    // Every key but the eleven modifier codes, under every set of modifiers.
    assert_eq!(names, (254 - 11) * 16);
}

#[test]
fn the_names_people_see_are_the_ones_the_engine_documents() {
    let plain = Modifiers::default();
    for (vk, name) in [
        (0x70, "F1"),
        (0x87, "F24"),
        (0x41, "A"),
        (0x30, "0"),
        (0x20, "Space"),
        (0x0D, "Enter"),
        (0x09, "Tab"),
        (0x1B, "Esc"),
        (0x08, "Backspace"),
        (0x2D, "Insert"),
        (0x2E, "Delete"),
        (0x24, "Home"),
        (0x23, "End"),
        (0x21, "PageUp"),
        (0x22, "PageDown"),
        (0x25, "Left"),
        (0x26, "Up"),
        (0x27, "Right"),
        (0x28, "Down"),
        (0x13, "Pause"),
        (0x14, "CapsLock"),
        (0x91, "ScrollLock"),
        (0x2C, "PrintScreen"),
        (0x60, "Numpad0"),
        (0x69, "Numpad9"),
    ] {
        assert_eq!(hotkey::name(plain, vk).as_deref(), Some(name));
        assert_eq!(name.parse::<Hotkey>().map(|key| key.vk), Ok(vk));
    }
}
