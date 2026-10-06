//! What the engine says when nobody spoke.
//!
//! Its model learned from subtitled video, and silence in a video is where
//! the subtitles thank the viewer, name the music or credit the translator.
//! So for a breath, the hum of a room or the click of a key it answers
//! "Thank you.", "Thanks for watching!" or `*phone rings*`. Sounds named in
//! brackets, parentheses or asterisks are never speech and are taken out of
//! every answer ([`clean`]). The phrases are things people do say, so they
//! are only dropped where the audio was too quiet to have been said aloud;
//! [`super::dictation`] decides that.

/// What the engine most often makes of audio without speech, in the
/// languages it is used in here, as [`words`] gives them.
const MADE_UP: &[&str] = &[
    "thank you",
    "thank you very much",
    "thank you so much",
    "thank you for watching",
    "thank you for listening",
    "thank you for your attention",
    "thanks",
    "thanks for watching",
    "thanks for listening",
    "please subscribe",
    "bye",
    "bye bye",
    "goodbye",
    "see you next time",
    "you",
    "okay",
    "ok",
    "so",
    "oh",
    "hmm",
    "hvala",
    "hvala lepa",
    "hvala vam",
    "najlepša hvala",
    "hvala za ogled",
    "hvala za gledanje",
    "hvala za pozornost",
    "hvala za poslušanje",
    "hvala ker ste gledali",
    "hvala ker ste nas gledali",
    "hvala ker ste poslušali",
    "hvala što pratite",
    "hvala na pažnji",
    "nasvidenje",
    "adijo",
    "ja",
    "no",
    "vielen dank",
    "danke",
    "merci",
    "gracias",
    "grazie",
];
/// Credits the engine learned from the last line of subtitles; an answer
/// that contains one of them is one.
const CREDITS: &[&str] = &[
    "amara org",
    "subtitles by",
    "subtitled by",
    "transcribed by",
    "translated by",
    "podnapisi",
    "prevod in priredba",
    "prevedel",
    "prevedla",
    "untertitel",
    "sous titres",
];

/// Whether `text` is nothing but a phrase the engine is known to make up.
pub(super) fn made_up(text: &str) -> bool {
    let said = words(text).join(" ");
    MADE_UP.contains(&said.as_str()) || CREDITS.iter().any(|credit| said.contains(credit))
}

/// Whether `text` is one word or phrase said over and over, which is what
/// the engine makes of a steady noise.
pub(super) fn stutters(text: &str) -> bool {
    let words = words(text);
    (1..=words.len() / 2).any(|phrase| {
        words.len().is_multiple_of(phrase)
            && words
                .chunks(phrase)
                .all(|said| Some(said) == words.get(..phrase))
    })
}

/// The words of `text`, whatever the capitals and the punctuation.
pub(super) fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Tidies what an engine prints: one line, without the names it gives to
/// what is not speech, such as `[BLANK_AUDIO]`, `(applause)`, `*BEEP*` or a
/// note for music. Empty if no word is left.
pub(super) fn clean(raw: &str) -> String {
    let mut text = String::with_capacity(raw.len());
    let mut depth = 0usize;
    for c in raw.chars() {
        match c {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            '♪' | '♫' | '♬' | '♩' => {}
            _ if depth == 0 => text.push(c),
            _ => {}
        }
    }
    // `*phone rings*`, but not the signs of `2 * 3 * 4`.
    let text = without_pairs(&text, '*', '*', |sound| {
        !sound.starts_with(char::is_whitespace) && !sound.ends_with(char::is_whitespace)
    });
    let text = without_pairs(&text, '<', '>', |tag| {
        tag.trim_start_matches('/').chars().all(char::is_alphabetic)
    });
    let text = without_pairs(&text, '(', ')', names_a_sound);
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    // A whole answer in parentheses is a description of a sound, not speech,
    // and one without a letter or a digit says nothing.
    if (joined.starts_with('(') && joined.ends_with(')'))
        || !joined.chars().any(char::is_alphanumeric)
    {
        return String::new();
    }
    joined
}

/// Sounds the engine names in parentheses in the middle of speech. What a
/// speaker put in parentheses does not read like these.
const SOUNDS: &[&str] = &[
    "music",
    "applause",
    "laughter",
    "laughs",
    "laughing",
    "sighs",
    "sigh",
    "coughs",
    "coughing",
    "clears throat",
    "silence",
    "inaudible",
    "noise",
    "beep",
    "beeping",
    "click",
    "clicking",
    "typing",
    "ringing",
    "rings",
    "buzzing",
    "static",
    "breathing",
    "blowing",
    "crashing",
    "footsteps",
    "glasba",
    "aplavz",
    "smeh",
];

/// Whether what stands between two parentheses names a sound.
fn names_a_sound(inside: &str) -> bool {
    let said = words(inside);
    said.iter().any(|word| SOUNDS.contains(&word.as_str()))
        || SOUNDS.contains(&said.join(" ").as_str())
}

/// `text` without what stands between an `open` and the next `close`, the
/// two included, wherever `drop` says so of what is between them.
fn without_pairs(text: &str, open: char, close: char, drop: impl Fn(&str) -> bool) -> String {
    let mut kept = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(open) {
        let after = &rest[start + open.len_utf8()..];
        let Some(end) = after.find(close) else {
            break;
        };
        let inside = &after[..end];
        if !inside.trim().is_empty() && drop(inside) {
            kept.push_str(&rest[..start]);
        } else {
            kept.push_str(&rest[..start + open.len_utf8() + end + close.len_utf8()]);
        }
        rest = &after[end + close.len_utf8()..];
    }
    kept.push_str(rest);
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_output_is_tidied() {
        assert_eq!(
            clean(" Hello there.\n How are you?\n"),
            "Hello there. How are you?"
        );
        assert_eq!(clean("[BLANK_AUDIO]"), "");
        assert_eq!(clean(" (wind blowing) "), "");
        assert_eq!(clean("Yes [MUSIC] (I think) so"), "Yes (I think) so");
    }

    #[test]
    fn the_names_of_sounds_are_not_typed() {
        for sound in [
            "*phone rings*",
            " *BEEP*",
            "[Music]",
            "(applause)",
            "♪♪",
            "♪ ♪",
            "(dź)",
            "[XBOX SOUND]",
            "<i>...</i>",
            "...",
            " . ",
            "-",
        ] {
            assert_eq!(clean(sound), "", "{sound}");
        }
        assert_eq!(
            clean("Call me back. *phone rings* (upbeat music) Tomorrow is fine. [CLICK]"),
            "Call me back. Tomorrow is fine."
        );
        assert_eq!(clean("<i>Dober dan.</i>"), "Dober dan.");
        // Signs a speaker dictated stay where they are.
        assert_eq!(clean("5 * 3 is 15"), "5 * 3 is 15");
        assert_eq!(clean("2 * 3 * 4"), "2 * 3 * 4");
        assert_eq!(clean("if a < b"), "if a < b");
        assert_eq!(clean("a < b and c > d"), "a < b and c > d");
        assert_eq!(
            clean("The Sun (and even the other stars) moved."),
            "The Sun (and even the other stars) moved."
        );
    }

    #[test]
    fn the_phrases_the_engine_makes_up_are_known() {
        for phrase in [
            "Thank you.",
            " Thanks for watching!",
            "Hvala.",
            "Hvala za ogled.",
            "Hvala, ker ste gledali!",
            "you",
            "Subtitles by the Amara.org community",
        ] {
            assert!(made_up(phrase), "{phrase}");
        }
        for speech in [
            "Thank you for the flowers.",
            "Hvala za včeraj.",
            "",
            "You are.",
        ] {
            assert!(!made_up(speech), "{speech}");
        }
    }

    #[test]
    fn one_phrase_over_and_over_is_a_stutter() {
        assert!(stutters("You guys, you guys, you guys."));
        assert!(stutters("-Jak je tako. -Jak je tako."));
        assert!(stutters("no no"));
        assert!(!stutters("No."));
        assert!(!stutters("We meet at ten, at ten sharp."));
        assert!(!stutters(""));
    }
}
