//! Platform-independent translation of text into key presses.

/// One key press (down followed by up).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Key {
    /// A UTF-16 code unit delivered as a character, independent of keyboard layout.
    Unicode(u16),
    /// The Enter key. Sent as a real key because many apps ignore a `'\n'` character.
    Enter,
    /// The Tab key, for the same reason as [`Key::Enter`].
    Tab,
}

/// Translates `text` into the key presses that type it.
///
/// Line endings are normalized: `"\r\n"` and a lone `'\r'` each produce one Enter.
/// Characters outside the Basic Multilingual Plane become a surrogate pair.
pub(super) fn encode(text: &str) -> Vec<Key> {
    let mut keys = Vec::with_capacity(text.len());
    let mut units = [0u16; 2];
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                keys.push(Key::Enter);
            }
            '\n' => keys.push(Key::Enter),
            '\t' => keys.push(Key::Tab),
            _ => keys.extend(c.encode_utf16(&mut units).iter().copied().map(Key::Unicode)),
        }
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unicode(s: &str) -> Vec<Key> {
        s.encode_utf16().map(Key::Unicode).collect()
    }

    #[test]
    fn empty_text_has_no_keys() {
        assert!(encode("").is_empty());
    }

    #[test]
    fn plain_and_accented_text_is_unicode() {
        assert_eq!(encode("čšž Hi!"), unicode("čšž Hi!"));
    }

    #[test]
    fn astral_characters_become_surrogate_pairs() {
        assert_eq!(encode("😀"), [Key::Unicode(0xD83D), Key::Unicode(0xDE00)]);
    }

    #[test]
    fn line_endings_each_produce_one_enter() {
        let a = Key::Unicode(u16::from(b'a'));
        assert_eq!(encode("a\na"), [a, Key::Enter, a]);
        assert_eq!(encode("a\r\na"), [a, Key::Enter, a]);
        assert_eq!(encode("a\ra"), [a, Key::Enter, a]);
        assert_eq!(encode("\n\r\n"), [Key::Enter, Key::Enter]);
    }

    #[test]
    fn tab_is_a_real_key() {
        assert_eq!(encode("\t"), [Key::Tab]);
    }
}
