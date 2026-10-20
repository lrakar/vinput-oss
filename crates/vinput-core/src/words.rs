//! The user's own words: a dictionary of names and terms, snippets that a
//! spoken phrase stands for, and whether spoken formatting is listened for.
//!
//! The dictionary does two things. Its words are handed to the recognizer
//! where it takes such a list ([`listened_for`]), so that it hears them
//! more readily; and whatever the recognizer wrote, a word of the
//! dictionary is put in its own spelling afterwards, as is what the user
//! says it sounds like. Snippets and spoken formatting only ever work on
//! the text that came back. All of that is `words/shape.rs`.
//!
//! They live in the settings file, a line for each word and each snippet,
//! so a version that does not know them keeps them as it keeps every line
//! it does not know.

mod shape;

use std::sync::{Mutex, PoisonError};

pub(crate) use shape::{Rules, Shaped, Shaper};

/// The most words a dictionary holds.
pub const MAX_WORDS: usize = 200;
/// The most snippets there can be.
pub const MAX_SNIPPETS: usize = 100;
/// The longest a word, or the phrase a snippet is said with, may be, in
/// characters.
pub const MAX_WORD: usize = 60;
/// The longest the ways a word may sound are together, in characters.
pub const MAX_SOUNDS_LIKE: usize = 120;
/// The longest the text of a snippet may be, in characters.
pub const MAX_SNIPPET: usize = 1000;

/// About how many bytes of the dictionary a recognizer is handed. The
/// first engine reads only the end of what it is handed, and the text so
/// far has to fit in after this.
const MAX_LISTENED_FOR: usize = 400;

/// A word of the dictionary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    /// The word as it is to be written: `VinPUT`.
    pub word: String,
    /// How it may come out when it is not recognized, as the user typed
    /// it, several ways apart by commas: `vin put, win put`. May be empty.
    pub sounds_like: String,
}

/// A text that a spoken phrase stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snippet {
    /// What is said: `my email`.
    pub trigger: String,
    /// What is typed for it.
    pub text: String,
}

/// Everything of the user's own that recognized text is shaped with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Words {
    /// The dictionary, in the order the words were added.
    pub dictionary: Vec<Word>,
    /// The snippets, in the order they were added.
    pub snippets: Vec<Snippet>,
    /// Whether spoken formatting is listened for: `new line`, `snake case`
    /// and the others in `words/shape.rs`.
    pub commands: bool,
}

impl Default for Words {
    fn default() -> Self {
        Self {
            dictionary: Vec::new(),
            snippets: Vec::new(),
            commands: true,
        }
    }
}

/// Why a word or a snippet was not taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Nothing was entered.
    Empty,
    /// It is longer than it may be.
    TooLong,
    /// There is no room for another one.
    Full,
    /// The phrase of a snippet has no letter or digit in it, so it cannot
    /// be said.
    Unspoken,
}

/// `text` on one line, without what the settings file could not hold: the
/// stroke that parts the two halves of a line, and control characters.
fn tidy(text: &str) -> String {
    let kept: String = text
        .chars()
        .map(|c| if c == '|' || c.is_control() { ' ' } else { c })
        .collect();
    kept.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The words a phrase is said with: lower case, and nothing but letters
/// and digits.
pub(crate) fn spoken(phrase: &str) -> Vec<String> {
    phrase
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

impl Words {
    /// Adds a word to the dictionary, or changes how the one that is there
    /// already is written and what it sounds like. Says whether anything
    /// changed.
    ///
    /// # Errors
    ///
    /// Returns why the word cannot be taken; the dictionary is as it was.
    pub fn add_word(&mut self, word: &str, sounds_like: &str) -> Result<bool, Refused> {
        let (word, sounds_like) = (tidy(word), tidy(sounds_like));
        if word.is_empty() {
            return Err(Refused::Empty);
        }
        if word.chars().count() > MAX_WORD || sounds_like.chars().count() > MAX_SOUNDS_LIKE {
            return Err(Refused::TooLong);
        }
        let entry = Word { word, sounds_like };
        let same = |known: &Word| known.word.to_lowercase() == entry.word.to_lowercase();
        if let Some(known) = self.dictionary.iter_mut().find(|known| same(known)) {
            let changed = *known != entry;
            *known = entry;
            return Ok(changed);
        }
        if self.dictionary.len() >= MAX_WORDS {
            return Err(Refused::Full);
        }
        self.dictionary.push(entry);
        Ok(true)
    }

    /// Takes a word out of the dictionary. Says whether it was there.
    pub fn remove_word(&mut self, word: &str) -> bool {
        let before = self.dictionary.len();
        self.dictionary.retain(|known| known.word != word);
        self.dictionary.len() != before
    }

    /// Adds a snippet, or changes the text of the one that is said the
    /// same way. Says whether anything changed.
    ///
    /// # Errors
    ///
    /// Returns why the snippet cannot be taken; the snippets are as they
    /// were.
    pub fn add_snippet(&mut self, trigger: &str, text: &str) -> Result<bool, Refused> {
        let trigger = tidy(trigger);
        let text: String = text
            .replace("\r\n", "\n")
            .trim()
            .chars()
            .filter(|c| *c == '\n' || !c.is_control())
            .collect();
        if trigger.is_empty() || text.is_empty() {
            return Err(Refused::Empty);
        }
        let said = spoken(&trigger);
        if said.is_empty() {
            return Err(Refused::Unspoken);
        }
        if trigger.chars().count() > MAX_WORD || text.chars().count() > MAX_SNIPPET {
            return Err(Refused::TooLong);
        }
        let entry = Snippet { trigger, text };
        let same = |known: &Snippet| spoken(&known.trigger) == said;
        if let Some(known) = self.snippets.iter_mut().find(|known| same(known)) {
            let changed = *known != entry;
            *known = entry;
            return Ok(changed);
        }
        if self.snippets.len() >= MAX_SNIPPETS {
            return Err(Refused::Full);
        }
        self.snippets.push(entry);
        Ok(true)
    }

    /// Takes the snippet said with `trigger` away. Says whether it was
    /// there.
    pub fn remove_snippet(&mut self, trigger: &str) -> bool {
        let before = self.snippets.len();
        self.snippets.retain(|known| known.trigger != trigger);
        self.snippets.len() != before
    }

    /// Reads one line of the settings file, if it is one of these. Says
    /// whether it was. A line that says nothing usable is dropped.
    pub(crate) fn read(&mut self, key: &str, value: &str) -> bool {
        match key {
            "spoken_formatting" => self.commands = value.eq_ignore_ascii_case("true"),
            "word" => {
                let (word, sounds_like) = value.split_once('|').unwrap_or((value, ""));
                let _ = self.add_word(word, sounds_like);
            }
            "snippet" => {
                if let Some((trigger, text)) = value.split_once('|') {
                    let _ = self.add_snippet(trigger, &unescaped(text.trim()));
                }
            }
            _ => return false,
        }
        true
    }

    /// Writes the lines [`read`](Self::read) reads to the end of `file`.
    pub(crate) fn write(&self, file: &mut String) {
        file.push_str(if self.commands {
            "spoken_formatting = true\n"
        } else {
            "spoken_formatting = false\n"
        });
        for Word { word, sounds_like } in &self.dictionary {
            file.push_str("word = ");
            file.push_str(word);
            if !sounds_like.is_empty() {
                file.push_str(" | ");
                file.push_str(sounds_like);
            }
            file.push('\n');
        }
        for Snippet { trigger, text } in &self.snippets {
            file.push_str("snippet = ");
            file.push_str(trigger);
            file.push_str(" | ");
            file.push_str(&escaped(text));
            file.push('\n');
        }
    }

    /// The words a recognizer is asked to listen for: the dictionary as it
    /// is written, from its start, as far as a recognizer is handed it.
    #[must_use]
    pub fn terms(&self) -> Vec<String> {
        let mut room = MAX_LISTENED_FOR;
        self.dictionary
            .iter()
            .map(|known| known.word.clone())
            .take_while(|word| {
                let fits = word.len() + 2 <= room;
                room = room.saturating_sub(word.len() + 2);
                fits
            })
            .collect()
    }
}

/// A text on one line: a line break in it is written `\n`, and a backslash
/// twice.
fn escaped(text: &str) -> String {
    text.replace('\\', "\\\\").replace('\n', "\\n")
}

/// What [`escaped`] was made from.
fn unescaped(line: &str) -> String {
    let mut text = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            text.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => text.push('\n'),
            Some(other) => text.push(other),
            None => text.push(c),
        }
    }
    text
}

/// The words every dictation from here on asks its recognizer to listen
/// for. One list for the whole process: a provider is set up once and
/// shared, and learns of the dictionary when a dictation starts.
static LISTENED_FOR: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Has every dictation from here on listen for `terms`.
pub(crate) fn listen_for(terms: Vec<String>) {
    *LISTENED_FOR.lock().unwrap_or_else(PoisonError::into_inner) = terms;
}

/// The words a dictation that starts now asks its recognizer to listen for.
pub(crate) fn listened_for() -> Vec<String> {
    LISTENED_FOR
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// `terms` as a line of text, for a recognizer that is led by what it
/// reads: `VinPUT, Soniox, Lovro Rakar.` Empty without terms.
pub(crate) fn glossary(terms: &[String]) -> String {
    if terms.is_empty() {
        return String::new();
    }
    let mut line = terms.join(", ");
    line.push('.');
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words() -> Words {
        let mut words = Words::default();
        assert_eq!(words.add_word("VinPUT", "vin put, win put"), Ok(true));
        assert_eq!(words.add_word(" Lovro  Rakar ", ""), Ok(true));
        assert_eq!(
            words.add_snippet("my email", "someone@example.com"),
            Ok(true)
        );
        words
    }

    #[test]
    fn a_word_is_added_once_and_adding_it_again_changes_it() {
        let mut words = words();
        assert_eq!(words.dictionary[1].word, "Lovro Rakar");
        assert_eq!(words.add_word("VinPUT", "vin put, win put"), Ok(false));
        assert_eq!(words.add_word("vinput", ""), Ok(true));
        assert_eq!(words.dictionary.len(), 2);
        assert_eq!(words.dictionary[0].word, "vinput");
        assert!(words.remove_word("vinput") && !words.remove_word("vinput"));
        assert_eq!(words.dictionary.len(), 1);
    }

    #[test]
    fn what_cannot_be_kept_is_refused_and_nothing_changes() {
        let mut words = words();
        let before = words.clone();
        let long = "x".repeat(MAX_WORD + 1);
        assert_eq!(words.add_word("  ", "x"), Err(Refused::Empty));
        assert_eq!(words.add_word(&long, ""), Err(Refused::TooLong));
        assert_eq!(words.add_snippet("", "text"), Err(Refused::Empty));
        assert_eq!(words.add_snippet("phrase", " "), Err(Refused::Empty));
        assert_eq!(words.add_snippet("...", "text"), Err(Refused::Unspoken));
        assert_eq!(words.add_snippet(&long, "text"), Err(Refused::TooLong));
        assert_eq!(
            words.add_snippet("phrase", &"x".repeat(MAX_SNIPPET + 1)),
            Err(Refused::TooLong)
        );
        assert_eq!(words, before);
        for number in words.dictionary.len()..MAX_WORDS {
            assert_eq!(words.add_word(&format!("word{number}"), ""), Ok(true));
        }
        assert_eq!(words.add_word("one more", ""), Err(Refused::Full));
        // One that is there can still be changed.
        assert_eq!(words.add_word("VinPUT", ""), Ok(true));
    }

    #[test]
    fn a_snippet_said_the_same_way_takes_the_place_of_the_one_before() {
        let mut words = words();
        assert_eq!(words.add_snippet("My  Email!", "other"), Ok(true));
        assert_eq!(words.snippets.len(), 1);
        assert_eq!(words.snippets[0].trigger, "My Email!");
        assert!(words.remove_snippet("My Email!"));
        assert!(words.snippets.is_empty());
    }

    #[test]
    fn the_lines_of_the_file_come_back_as_they_were_written() {
        let mut words = words();
        words.commands = false;
        assert_eq!(
            words.add_snippet("sign off", "Best regards,\nLovro \\ PUTLAB | x"),
            Ok(true)
        );
        let mut file = String::new();
        words.write(&mut file);
        assert_eq!(
            file,
            "spoken_formatting = false\n\
             word = VinPUT | vin put, win put\n\
             word = Lovro Rakar\n\
             snippet = my email | someone@example.com\n\
             snippet = sign off | Best regards,\\nLovro \\\\ PUTLAB | x\n"
        );
        let mut read = Words::default();
        for line in file.lines() {
            let Some((key, value)) = line.split_once('=') else {
                unreachable!("every line has a key");
            };
            assert!(read.read(key.trim(), value.trim()), "{line}");
        }
        assert_eq!(read, words);
        assert!(!read.read("hotkey", "F1"));
    }

    #[test]
    fn a_damaged_line_is_dropped() {
        let mut words = Words::default();
        assert!(words.read("word", ""));
        assert!(words.read("snippet", "no text"));
        assert!(words.read("snippet", " | text"));
        assert!(words.read("spoken_formatting", "banana"));
        assert!(words.dictionary.is_empty() && words.snippets.is_empty());
        assert!(!words.commands);
    }

    #[test]
    fn a_recognizer_is_handed_the_start_of_a_long_dictionary() {
        let mut words = words();
        assert_eq!(words.terms(), ["VinPUT", "Lovro Rakar"]);
        assert_eq!(glossary(&words.terms()), "VinPUT, Lovro Rakar.");
        assert_eq!(glossary(&[]), "");
        for number in 0..150 {
            assert_eq!(words.add_word(&format!("word{number:03}"), ""), Ok(true));
        }
        let terms = words.terms();
        assert!(terms.len() < words.dictionary.len());
        assert!(glossary(&terms).len() <= MAX_LISTENED_FOR);
        assert_eq!(terms[0], "VinPUT");
    }
}
