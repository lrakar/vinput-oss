//! Putting recognized text into the user's own words, as it arrives.
//!
//! Three things are done to the text of a dictation, in one pass:
//!
//! - A word of the dictionary is written the way the dictionary writes it,
//!   whatever the recognizer made of its capitals, and so is what the user
//!   says it sounds like: `vin put` becomes `VinPUT`.
//! - The phrase of a snippet becomes the text of the snippet, wherever it
//!   is said: `send it to my email` ends in the address.
//! - Spoken formatting is carried out: `new line` and `new paragraph`
//!   break the line, and `snake case`, `camel case`, `kebab case` and
//!   `all caps` write the words that follow them that way.
//!
//! # When a command is a command
//!
//! Words that are also ordinary English must not turn into formatting in
//! the middle of a sentence, so a command counts only where it was said as
//! one, which the recognizer shows with its punctuation:
//!
//! - A command starts the dictation, or follows a pause: a full stop, a
//!   comma or another mark. `I like snake case names` is left alone.
//! - `new line` and `new paragraph` stand by themselves: the next thing
//!   after them is a mark or the end. `Start a new line of products` is
//!   left alone. The mark that ends them, and the spaces around, go.
//! - A casing command takes the words up to the next mark or the end of
//!   the dictation: `snake case user id, then` gives `user_id, then`.
//!
//! A dictation that is nothing but a snippet, or nothing but a name in
//! snake, camel or kebab case, is typed without the full stop the
//! recognizer ends every sentence with: whoever dictates an address into a
//! field does not want `someone@example.com.` there.
//!
//! # As it arrives
//!
//! Text comes in pieces that end anywhere, in the middle of a word too. A
//! [`Shaper`] passes on what nothing can change any more and keeps the
//! rest: the start of what may become a phrase, a casing command whose
//! words are still coming, a command whose mark has not arrived. What it
//! has passed on in the end is what the whole text shaped at once would
//! be, however it was cut; a test holds this. The white space before a
//! word that is kept is kept with it, so that a line break can take its
//! place.
//!
//! Where words are typed while the recognizer may still change them, what
//! is kept is shown as part of the guess ([`Shaper::preview`]): it is on
//! the screen at once, and is taken back like any guess if it comes out
//! differently.

use std::sync::Arc;

use super::{Words, spoken};
use crate::Result;
use crate::stt::{Emit, Transcriber, Transcript};

/// The ways a casing command writes the words that follow it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Case {
    /// `user_id`
    Snake,
    /// `userId`
    Camel,
    /// `user-id`
    Kebab,
    /// `USER ID`
    Caps,
}

/// The commands that break the line, as they are said, and what is typed
/// for each. A recognizer writes some of them as one word.
const BREAKS: [(&str, &str); 3] = [
    ("new line", "\n"),
    ("newline", "\n"),
    ("new paragraph", "\n\n"),
];

/// The casing commands, as they are said.
const CASES: [(&str, Case); 8] = [
    ("snake case", Case::Snake),
    ("snakecase", Case::Snake),
    ("camel case", Case::Camel),
    ("camelcase", Case::Camel),
    ("kebab case", Case::Kebab),
    ("kebabcase", Case::Kebab),
    ("all caps", Case::Caps),
    ("allcaps", Case::Caps),
];

/// The marks a recognizer writes where the speaker paused.
const MARKS: [char; 6] = ['.', ',', ';', ':', '!', '?'];

/// Whether `c` may stand between the words of one phrase: a recognizer
/// writes `vin-put`, `snake_case` and `don't` for what was said as words.
fn joins(c: char) -> bool {
    matches!(c, ' ' | '\u{a0}' | '-' | '_' | '\'' | '\u{2019}')
}

/// How many bytes at the start of `text` are characters that `is` holds
/// for.
fn run(text: &str, is: impl Fn(char) -> bool) -> usize {
    text.char_indices()
        .find(|&(_, c)| !is(c))
        .map_or(text.len(), |(at, _)| at)
}

/// What a phrase becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Becomes {
    /// A word of the dictionary, as it is written.
    Spelling(String),
    /// The text of a snippet.
    Snippet(String),
    /// A line break, or two.
    Break(&'static str),
    /// The words that follow, written this way.
    Case(Case),
}

impl Becomes {
    fn is_command(&self) -> bool {
        matches!(self, Self::Break(_) | Self::Case(_))
    }
}

/// How a phrase compares with the text at some place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Match {
    /// It is there, and ends at this byte.
    Full(usize),
    /// The text ends before it could be told, and more may come.
    Partial,
    /// It is not there.
    No,
}

/// Something that is said, and what it becomes.
#[derive(Debug)]
struct Phrase {
    /// The words it is said with, in lower case.
    words: Vec<String>,
    becomes: Becomes,
}

impl Phrase {
    /// Whether the phrase stands in `text` at `at`, where a word starts.
    /// With `whole`, the text is all there is going to be.
    fn matches(&self, text: &str, at: usize, whole: bool) -> Match {
        let mut pos = at;
        for (index, word) in self.words.iter().enumerate() {
            let end = pos + run(&text[pos..], char::is_alphanumeric);
            // The word may still grow, and what follows it is not known.
            let open = !whole && end == text.len();
            let mut letters = word.chars();
            let begins = text[pos..end]
                .chars()
                .flat_map(char::to_lowercase)
                .all(|c| letters.next() == Some(c));
            if !begins || letters.next().is_some() {
                return if begins && open {
                    Match::Partial
                } else {
                    Match::No
                };
            }
            if open {
                return Match::Partial;
            }
            if index + 1 == self.words.len() {
                return Match::Full(end);
            }
            let gap = run(&text[end..], joins);
            pos = end + gap;
            if pos == text.len() {
                return if whole { Match::No } else { Match::Partial };
            }
            let next_is_a_word = text[pos..].starts_with(char::is_alphanumeric);
            if gap == 0 || !next_is_a_word {
                return Match::No;
            }
        }
        Match::No
    }
}

/// What the text is shaped with: every phrase that becomes something.
#[derive(Debug)]
pub(crate) struct Rules {
    phrases: Vec<Phrase>,
}

impl Rules {
    /// The rules `words` come to, or `None` if they change no text at all.
    pub(crate) fn of(words: &Words) -> Option<Arc<Self>> {
        Self::made(words, true, true)
    }

    /// The rules of a dictation that a model tidies: the dictionary and
    /// spoken formatting, for the text the model is given, so that it sees
    /// the words as the user writes them; and the snippets, for the text the
    /// model writes, so that it does not rewrite what they stand for.
    pub(crate) fn around_a_model(words: &Words) -> (Option<Arc<Self>>, Option<Arc<Self>>) {
        (
            Self::made(words, false, true),
            Self::made(words, true, false),
        )
    }

    /// The rules of `words`, with or without the snippets and with or
    /// without everything else.
    fn made(words: &Words, snippets: bool, others: bool) -> Option<Arc<Self>> {
        let mut phrases = Vec::new();
        let mut add = |said: &str, becomes: Becomes| {
            let words = spoken(said);
            if !words.is_empty() {
                phrases.push(Phrase { words, becomes });
            }
        };
        // Of two phrases that end at the same place the one listed first
        // is taken: what the user set up comes before what is built in.
        let snippets = if snippets { &words.snippets[..] } else { &[] };
        for snippet in snippets {
            add(&snippet.trigger, Becomes::Snippet(snippet.text.clone()));
        }
        if words.commands && others {
            for (said, typed) in BREAKS {
                add(said, Becomes::Break(typed));
            }
            for (said, case) in CASES {
                add(said, Becomes::Case(case));
            }
        }
        let dictionary = if others { &words.dictionary[..] } else { &[] };
        for known in dictionary {
            let spelling = || Becomes::Spelling(known.word.clone());
            // A word such as `C++` is not found by its letters alone: that
            // would be every `c`. It is found by what it sounds like.
            if known.word.chars().all(|c| c.is_alphanumeric() || joins(c)) {
                add(&known.word, spelling());
            }
            for sound in known.sounds_like.split(',') {
                add(sound, spelling());
            }
        }
        (!phrases.is_empty()).then(|| Arc::new(Self { phrases }))
    }

    /// Reads the word that starts at `at`.
    fn read(&self, text: &str, at: usize, before: Before, whole: bool) -> Read {
        let mut found: Vec<(usize, &Becomes)> = Vec::new();
        for phrase in &self.phrases {
            if phrase.becomes.is_command() && before == Before::Word {
                continue;
            }
            match phrase.matches(text, at, whole) {
                Match::Full(end) => found.push((end, &phrase.becomes)),
                // Something longer may be coming; nothing is decided.
                Match::Partial => return Read::Kept,
                Match::No => {}
            }
        }
        // The longest first, and of those as long the one listed first.
        while let Some(longest) = found.iter().map(|&(end, _)| end).max() {
            let read = found
                .iter()
                .filter(|&&(end, _)| end == longest)
                .find_map(|&(end, becomes)| put(text, end, becomes, before, whole));
            if let Some(read) = read {
                return read;
            }
            found.retain(|&(end, _)| end != longest);
        }
        Read::Plain
    }
}

/// What came before the place the text is read at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Before {
    /// Nothing: the dictation starts here.
    Start,
    /// The end of a sentence, or a line break.
    Sentence,
    /// A comma or another mark inside a sentence.
    Clause,
    /// A word.
    Word,
}

impl Before {
    /// What comes before the next place, after the character `c` that is
    /// neither a letter nor white space.
    fn after(self, c: char) -> Self {
        match c {
            '.' | '!' | '?' | '\u{2026}' => Self::Sentence,
            ',' | ';' | ':' | '(' | '\u{2013}' | '\u{2014}' => Self::Clause,
            _ => self,
        }
    }
}

/// What reading a word came to.
#[derive(Debug, PartialEq, Eq)]
enum Read {
    /// It cannot be told yet: the word, and the white space before it, are
    /// kept until more text has arrived.
    Kept,
    /// An ordinary word.
    Plain,
    /// The text up to `end` becomes `text`.
    Put {
        text: String,
        end: usize,
        before: Before,
        /// Whether this is a line break, which takes the place of the
        /// spaces on both sides of it.
        breaks: bool,
    },
}

/// What the phrase that ends at `end` and `becomes` this comes to here, if
/// it applies here at all.
fn put(text: &str, end: usize, becomes: &Becomes, before: Before, whole: bool) -> Option<Read> {
    let rest = &text[end..];
    match becomes {
        Becomes::Spelling(spelling) => Some(Read::Put {
            text: spelling.clone(),
            end,
            before: Before::Word,
            breaks: false,
        }),
        Becomes::Snippet(snippet) => Some(match alone(rest, before, whole) {
            Some(false) => return Some(Read::Kept),
            Some(true) => Read::Put {
                text: snippet.clone(),
                end: text.len(),
                before: Before::Sentence,
                breaks: false,
            },
            None => Read::Put {
                text: snippet.clone(),
                end,
                before: Before::Word,
                breaks: false,
            },
        }),
        Becomes::Break(typed) => {
            let mark = usize::from(rest.starts_with(MARKS));
            let after = &rest[mark..];
            if after.is_empty() && !whole {
                // The mark may be the point of `new line.5`.
                return Some(Read::Kept);
            }
            let ends_here =
                after.is_empty() || (mark == 1 && after.starts_with(char::is_whitespace));
            ends_here.then(|| Read::Put {
                text: (*typed).to_owned(),
                end: end + mark,
                before: Before::Sentence,
                breaks: true,
            })
        }
        Becomes::Case(case) => cased(text, end, *case, before, whole),
    }
}

/// Whether what `rest` holds after a phrase is the full stop that ends a
/// dictation which was nothing but that phrase. `None` if it is not;
/// `Some(false)` if that cannot be told until the dictation is over.
fn alone(rest: &str, before: Before, whole: bool) -> Option<bool> {
    let nothing_else = before == Before::Start
        && rest
            .strip_prefix('.')
            .is_some_and(|rest| rest.trim().is_empty());
    nothing_else.then_some(whole)
}

/// A casing command that ends at `end`: the words after it, up to the next
/// mark, written as `case` says.
fn cased(text: &str, end: usize, case: Case, before: Before, whole: bool) -> Option<Read> {
    // A recognizer may set the command apart: `Snake case: user id.`
    let mark = usize::from(text[end..].starts_with([',', ':']));
    let gap = run(&text[end + mark..], char::is_whitespace);
    let start = end + mark + gap;
    if start == text.len() && !whole {
        return Some(Read::Kept);
    }
    if gap == 0 {
        return None;
    }
    // The words end at a mark that ends a word, not at the point of `2.5`.
    let stop = text[start..].char_indices().find_map(|(at, c)| {
        let after = &text[start + at + c.len_utf8()..];
        let ends = c == '\n' || (MARKS.contains(&c) && !after.starts_with(char::is_alphanumeric));
        ends.then_some(start + at)
    });
    let Some(stop) = stop.or(whole.then_some(text.len())) else {
        return Some(Read::Kept);
    };
    if stop + 1 == text.len() && !whole {
        // What follows the mark says whether it ends the words.
        return Some(Read::Kept);
    }
    let words = text[start..stop].trim_end();
    if !words.contains(char::is_alphanumeric) {
        return None;
    }
    let name = case != Case::Caps;
    let (end, before) = match alone(&text[stop..], before, whole).filter(|_| name) {
        Some(false) => return Some(Read::Kept),
        Some(true) => (text.len(), Before::Sentence),
        None => (start + words.len(), Before::Word),
    };
    Some(Read::Put {
        text: case.write(words),
        end,
        before,
        breaks: false,
    })
}

impl Case {
    /// `words` written this way.
    fn write(self, words: &str) -> String {
        if self == Self::Caps {
            return words.to_uppercase();
        }
        // `don't` is one word of a name, `dont`.
        let plain = words.replace(['\'', '\u{2019}'], "").to_lowercase();
        let parts = plain
            .split(|c: char| !c.is_alphanumeric())
            .filter(|part| !part.is_empty());
        let mut name = String::new();
        for (index, part) in parts.enumerate() {
            match self {
                Self::Camel if index > 0 => {
                    let mut letters = part.chars();
                    name.extend(letters.next().into_iter().flat_map(char::to_uppercase));
                    name.push_str(letters.as_str());
                }
                Self::Snake if index > 0 => {
                    name.push('_');
                    name.push_str(part);
                }
                Self::Kebab if index > 0 => {
                    name.push('-');
                    name.push_str(part);
                }
                _ => name.push_str(part),
            }
        }
        name
    }
}

/// Where the shaping of a dictation stands between two pieces of its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Place {
    before: Before,
    /// Whether a line break was just made, which the spaces after it
    /// belong to.
    broke: bool,
}

/// Shapes `text`, which follows `place`. Returns the shaped text, how many
/// bytes of `text` it stands for, and the place after them. Unless the
/// text is `whole`, what more text could still change is left over.
fn shape(rules: &Rules, text: &str, mut place: Place, whole: bool) -> (String, usize, Place) {
    let mut out = String::new();
    let mut at = 0;
    // Where the white space before the next word began, in `text` and in
    // `out`, and the place it began at.
    let mut gap: Option<(usize, usize, Place)> = None;
    while let Some(c) = text[at..].chars().next() {
        if c.is_whitespace() {
            let end = at + run(&text[at..], char::is_whitespace);
            if end == text.len() && !whole {
                break;
            }
            let space = &text[at..end];
            if !place.broke {
                gap = Some((at, out.len(), place));
                out.push_str(space);
            }
            if space.contains('\n') {
                place.before = Before::Sentence;
            }
            at = end;
            continue;
        }
        if !c.is_alphanumeric() {
            out.push(c);
            at += c.len_utf8();
            place = Place {
                before: place.before.after(c),
                broke: false,
            };
            gap = None;
            continue;
        }
        match rules.read(text, at, place.before, whole) {
            Read::Kept => {
                if let Some((text_at, out_at, before)) = gap {
                    at = text_at;
                    out.truncate(out_at);
                    place = before;
                }
                break;
            }
            Read::Plain => {
                let end = at + run(&text[at..], char::is_alphanumeric);
                out.push_str(&text[at..end]);
                at = end;
                place.before = Before::Word;
            }
            Read::Put {
                text: put,
                end,
                before,
                breaks,
            } => {
                if breaks {
                    out.truncate(out.trim_end_matches([' ', '\t', '\u{a0}']).len());
                }
                out.push_str(&put);
                at = end;
                place = Place {
                    before,
                    broke: breaks,
                };
                gap = None;
                continue;
            }
        }
        place.broke = false;
        gap = None;
    }
    (out, at, place)
}

/// Shapes the text of one dictation as it arrives; see the module.
#[derive(Debug)]
pub(crate) struct Shaper {
    rules: Arc<Rules>,
    /// What has arrived and could still be changed by what follows.
    kept: String,
    place: Place,
}

impl Shaper {
    pub(crate) fn new(rules: Arc<Rules>) -> Self {
        Self {
            rules,
            kept: String::new(),
            place: Place {
                before: Before::Start,
                broke: false,
            },
        }
    }

    /// Takes the next piece of the text and returns what can be typed now.
    pub(crate) fn push(&mut self, text: &str) -> String {
        self.kept.push_str(text);
        self.shaped(false)
    }

    /// The text is over: returns what was still kept, shaped.
    pub(crate) fn finish(&mut self) -> String {
        self.shaped(true)
    }

    fn shaped(&mut self, whole: bool) -> String {
        let (out, used, place) = shape(&self.rules, &self.kept, self.place, whole);
        self.kept.drain(..used);
        self.place = place;
        out
    }

    /// What would be typed if `guess` were the rest of the text: what is
    /// kept and the guess, shaped. Nothing is taken or changed.
    pub(crate) fn preview(&self, guess: &str) -> String {
        let text = format!("{}{guess}", self.kept);
        shape(&self.rules, &text, self.place, true).0
    }
}

/// A recognizer whose text comes out shaped. Without rules it is the
/// recognizer itself.
pub(crate) struct Shaped<T> {
    inner: T,
    shaper: Option<Shaper>,
    /// The recognizer's last guess at what follows the text it settled.
    guess: String,
    /// Whether the guess to show has changed since it was last asked for.
    changed: bool,
}

impl<T: Transcriber> Shaped<T> {
    pub(crate) fn new(inner: T, rules: Option<Arc<Rules>>) -> Self {
        Self {
            inner,
            shaper: rules.map(Shaper::new),
            guess: String::new(),
            changed: false,
        }
    }
}

impl<T: Transcriber> Transcriber for Shaped<T> {
    fn feed(&mut self, pcm: &[i16]) -> Result<Option<Transcript>> {
        let heard = self.inner.feed(pcm)?;
        let (Some(shaper), Some(settled)) = (&mut self.shaper, &heard) else {
            return Ok(heard);
        };
        if !settled.is_final {
            return Ok(heard);
        }
        // What the guess said beyond the settled text stands until the
        // recognizer says what follows now.
        self.guess = self
            .guess
            .strip_prefix(settled.text.as_str())
            .unwrap_or_default()
            .to_owned();
        self.changed = true;
        let text = shaper.push(&settled.text);
        Ok((!text.is_empty()).then_some(Transcript {
            text,
            is_final: true,
        }))
    }

    fn finish(&mut self, emit: &mut Emit<'_>) -> Result<()> {
        let Some(shaper) = &mut self.shaper else {
            return self.inner.finish(emit);
        };
        self.inner.finish(&mut |transcript| {
            if !transcript.is_final {
                return emit(transcript);
            }
            let text = shaper.push(&transcript.text);
            if text.is_empty() {
                return Ok(());
            }
            emit(Transcript {
                text,
                is_final: true,
            })
        })?;
        let text = shaper.finish();
        if text.is_empty() {
            return Ok(());
        }
        emit(Transcript {
            text,
            is_final: true,
        })
    }

    fn guess(&mut self) -> Option<String> {
        let guess = self.inner.guess();
        let Some(shaper) = &self.shaper else {
            return guess;
        };
        if let Some(guess) = guess {
            self.guess = guess;
            self.changed = true;
        }
        std::mem::take(&mut self.changed).then(|| shaper.preview(&self.guess))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words() -> Words {
        let mut words = Words::default();
        let added = [
            words.add_word("VinPUT", "vin put, win put"),
            words.add_word("Lovro Rakar", ""),
            words.add_word("C++", "c plus plus"),
            words.add_word("Qwen3-ASR", "quen three asr"),
            words.add_snippet("my email", "someone@example.com"),
            words.add_snippet("sign off", "Best regards,\nLovro"),
        ];
        assert!(added.iter().all(|added| *added == Ok(true)));
        words
    }

    fn rules(words: &Words) -> Arc<Rules> {
        let Some(rules) = Rules::of(words) else {
            unreachable!("there are words to shape with");
        };
        rules
    }

    /// `text` shaped at once.
    fn whole(text: &str) -> String {
        let mut shaper = Shaper::new(rules(&words()));
        let mut out = shaper.push(text);
        out.push_str(&shaper.finish());
        out
    }

    #[test]
    fn nothing_to_shape_with_is_no_rules_at_all() {
        let mut none = Words::default();
        assert!(Rules::of(&none).is_some(), "spoken formatting is on");
        none.commands = false;
        assert!(Rules::of(&none).is_none());
    }

    #[test]
    fn a_word_of_the_dictionary_is_written_its_way() {
        assert_eq!(whole("I use vinput daily."), "I use VinPUT daily.");
        assert_eq!(whole("Vin put is here."), "VinPUT is here.");
        assert_eq!(whole("Try Win-Put's menu"), "Try VinPUT's menu");
        assert_eq!(whole("ask lovro rakar."), "ask Lovro Rakar.");
        assert_eq!(
            whole("QWEN3 asr and quen three ASR"),
            "Qwen3-ASR and Qwen3-ASR"
        );
        // Only as a whole word.
        assert_eq!(whole("vinputs and unvinput"), "vinputs and unvinput");
        assert_eq!(whole("lovro. Rakar"), "lovro. Rakar");
    }

    #[test]
    fn a_word_with_signs_in_it_is_found_by_its_sound_alone() {
        assert_eq!(whole("I write C plus plus."), "I write C++.");
        assert_eq!(whole("Vitamin c is"), "Vitamin c is");
    }

    #[test]
    fn a_snippet_is_typed_wherever_it_is_said() {
        assert_eq!(
            whole("Send it to my email, please."),
            "Send it to someone@example.com, please."
        );
        assert_eq!(whole("It is in my email."), "It is in someone@example.com.");
        assert_eq!(whole("my emails"), "my emails");
    }

    #[test]
    fn a_dictation_that_is_only_a_snippet_loses_its_full_stop() {
        assert_eq!(whole("My email."), "someone@example.com");
        assert_eq!(whole(" My email. "), " someone@example.com");
        assert_eq!(whole("Sign off."), "Best regards,\nLovro");
        // Not when more was said, or with another mark.
        assert_eq!(
            whole("My email. Write soon."),
            "someone@example.com. Write soon."
        );
        assert_eq!(whole("My email?"), "someone@example.com?");
    }

    #[test]
    fn a_line_break_is_made_where_it_is_said_by_itself() {
        assert_eq!(whole("One. New line. Two."), "One.\nTwo.");
        assert_eq!(whole("One, new line, two"), "One,\ntwo");
        assert_eq!(whole("One. Newline. Two"), "One.\nTwo");
        assert_eq!(whole("One. New paragraph. Two."), "One.\n\nTwo.");
        assert_eq!(whole("New line"), "\n");
        assert_eq!(whole("One. New line."), "One.\n");
        assert_eq!(whole("One. New line. New line. Two"), "One.\n\nTwo");
        assert_eq!(whole("One.\nNew-line! Two"), "One.\n\nTwo");
    }

    #[test]
    fn words_of_a_sentence_are_not_a_line_break() {
        for said in [
            "Start a new line of products.",
            "New line of products.",
            "A new line. Two.",
            "We need a new paragraph here.",
            "The newline character.",
        ] {
            assert_eq!(whole(said), said);
        }
    }

    #[test]
    fn a_casing_command_writes_the_words_up_to_the_next_mark() {
        assert_eq!(whole("Snake case user id"), "user_id");
        assert_eq!(
            whole("Call it, camel case user id, for now."),
            "Call it, userId, for now."
        );
        assert_eq!(
            whole("Done. Kebab-case My Page Title! Next"),
            "Done. my-page-title! Next"
        );
        assert_eq!(
            whole("All caps do not touch. Thanks."),
            "DO NOT TOUCH. Thanks."
        );
        assert_eq!(whole("CamelCase: get user's name"), "getUsersName");
        assert_eq!(
            whole("snake_case version 2.5 beta, ok"),
            "version_2_5_beta, ok"
        );
        assert_eq!(whole("Note: snake case a b"), "Note: a_b");
    }

    #[test]
    fn a_dictation_that_is_only_a_name_loses_its_full_stop() {
        assert_eq!(whole("Snake case user id."), "user_id");
        assert_eq!(whole("Camel case user id. "), "userId");
        assert_eq!(whole("All caps stop."), "STOP.");
        assert_eq!(whole("Snake case user id. Next."), "user_id. Next.");
        assert_eq!(whole("So, snake case user id."), "So, user_id.");
    }

    #[test]
    fn words_of_a_sentence_are_not_a_casing_command() {
        for said in [
            "I like snake case names.",
            "Use camel case here",
            "Written in all caps.",
            "Snake case.",
            "All caps",
        ] {
            assert_eq!(whole(said), said, "{said}");
        }
    }

    #[test]
    fn a_snippet_comes_before_a_command_said_the_same_way() {
        let mut words = words();
        assert_eq!(words.add_snippet("new line", "<br>"), Ok(true));
        let mut shaper = Shaper::new(rules(&words));
        let mut out = shaper.push("One. New line. Two, a new line");
        out.push_str(&shaper.finish());
        assert_eq!(out, "One. <br>. Two, a <br>");
    }

    #[test]
    fn with_spoken_formatting_off_only_the_users_words_are_shaped() {
        let mut words = words();
        words.commands = false;
        let mut shaper = Shaper::new(rules(&words));
        let mut out = shaper.push("Snake case vinput. New line.");
        out.push_str(&shaper.finish());
        assert_eq!(out, "Snake case VinPUT. New line.");
    }

    #[test]
    fn only_what_may_still_change_is_kept() {
        let mut shaper = Shaper::new(rules(&words()));
        // Nothing here could become anything.
        assert_eq!(shaper.push("Hello there,"), "Hello there,");
        // `my` may be the start of the snippet.
        assert_eq!(shaper.push(" this is my"), " this is");
        assert_eq!(shaper.push(" dog"), " my dog");
        // `Vin` may be the start of a word of the dictionary.
        assert_eq!(shaper.push(". Vin"), ".");
        assert_eq!(shaper.push("put"), "");
        assert_eq!(shaper.push(" works"), " VinPUT works");
        // A command, once the mark after it and what follows are there.
        assert_eq!(shaper.push(". New line"), ".");
        assert_eq!(shaper.push("."), "");
        assert_eq!(shaper.push(" Snake case user"), "\n");
        assert_eq!(shaper.push(" id, and"), "user_id, and");
        assert_eq!(shaper.push(" my email"), "");
        assert_eq!(shaper.finish(), " someone@example.com");
    }

    /// Every text of the tests above, and a few more.
    const TEXTS: [&str; 22] = [
        "I use vinput daily. Vin put is here, and Win-Put's menu.",
        "ask lovro rakar. lovro. Rakar",
        "I write C plus plus. Vitamin c is",
        "Send it to my email, please. It is in my email.",
        "My email.",
        " My email. ",
        "My email. Write soon.",
        "One. New line. Two. New paragraph. Three, new line, four",
        "One. New line. New line. Two",
        "New line",
        "Start a new line of products. A new line. Two.",
        "Snake case user id",
        "Snake case user id.",
        "Snake case user id. Next.",
        "Call it, camel case user id, for now.",
        "Done. Kebab-case My Page Title! Next",
        "All caps do not touch. Thanks.",
        "snake_case version 2.5 beta, ok",
        "I like snake case names. Snake case. All caps",
        "Sign off.",
        "Številka: vinput, čas. Nova vrstica. New line.\nSnake case žaba in čaj.",
        "new line.5 and snake case a.b c. d",
    ];

    #[test]
    fn however_the_text_is_cut_it_comes_out_as_it_would_whole() {
        for text in TEXTS {
            let expected = whole(text);
            let cuts: Vec<usize> = text.char_indices().map(|(at, _)| at).collect();
            // In two pieces, cut at every place.
            for &cut in &cuts {
                let mut shaper = Shaper::new(rules(&words()));
                let mut out = shaper.push(&text[..cut]);
                out.push_str(&shaper.push(&text[cut..]));
                out.push_str(&shaper.finish());
                assert_eq!(out, expected, "{text:?} cut at {cut}");
            }
            // A character at a time, and the guess at the rest is what
            // the whole comes to.
            let mut shaper = Shaper::new(rules(&words()));
            let mut out = String::new();
            for (at, c) in text.char_indices() {
                let guessed = format!("{out}{}", shaper.preview(&text[at..]));
                assert_eq!(guessed, expected, "{text:?} guessed at {at}");
                out.push_str(&shaper.push(c.encode_utf8(&mut [0; 4])));
            }
            out.push_str(&shaper.finish());
            assert_eq!(out, expected, "{text:?} a character at a time");
        }
    }

    /// Says what a script has it say: for each piece of audio, the text it
    /// settles and its guess at what follows.
    struct Scripted {
        script: std::vec::IntoIter<(&'static str, Option<&'static str>)>,
        guess: Option<String>,
    }

    impl Scripted {
        fn new(script: Vec<(&'static str, Option<&'static str>)>) -> Self {
            Self {
                script: script.into_iter(),
                guess: None,
            }
        }
    }

    impl Transcriber for Scripted {
        fn feed(&mut self, _pcm: &[i16]) -> Result<Option<Transcript>> {
            let Some((settled, guess)) = self.script.next() else {
                return Ok(None);
            };
            self.guess = guess.map(str::to_owned);
            Ok((!settled.is_empty()).then(|| Transcript {
                text: settled.to_owned(),
                is_final: true,
            }))
        }

        fn finish(&mut self, emit: &mut Emit<'_>) -> Result<()> {
            for (settled, _) in self.script.by_ref() {
                emit(Transcript {
                    text: settled.to_owned(),
                    is_final: true,
                })?;
            }
            Ok(())
        }

        fn guess(&mut self) -> Option<String> {
            self.guess.take()
        }
    }

    #[test]
    fn a_recognizer_without_rules_is_itself() -> Result<()> {
        let script = vec![("Vin put", Some(" is")), (" is here.", None)];
        let mut session = Shaped::new(Scripted::new(script), None);
        let settled = session.feed(&[])?;
        assert_eq!(
            settled.map(|settled| settled.text).as_deref(),
            Some("Vin put")
        );
        assert_eq!(session.guess().as_deref(), Some(" is"));
        assert_eq!(session.guess(), None);
        let mut rest = String::new();
        session.finish(&mut |transcript| {
            rest.push_str(&transcript.text);
            Ok(())
        })?;
        assert_eq!(rest, " is here.");
        Ok(())
    }

    #[test]
    fn settled_text_and_guesses_come_out_shaped() -> Result<()> {
        let script = vec![
            ("", Some("Send it to my")),
            ("Send it", Some(" to my email")),
            (" to my", None),
            ("", Some(" email and")),
            (" email", Some(" and vin")),
            (" and vin put.", None),
        ];
        let mut session = Shaped::new(Scripted::new(script), Some(rules(&words())));
        let mut typed = String::new();
        let mut step = |session: &mut Shaped<Scripted>| -> Result<(String, Option<String>)> {
            let settled = session.feed(&[])?.map(|settled| settled.text);
            let settled = settled.unwrap_or_default();
            typed.push_str(&settled);
            Ok((settled, session.guess()))
        };
        assert_eq!(
            step(&mut session)?,
            (String::new(), Some("Send it to my".into()))
        );
        // The guess is what follows the text that was passed on.
        assert_eq!(
            step(&mut session)?,
            ("Send it".into(), Some(" to someone@example.com".into()))
        );
        // `my` is kept, and shown as a guess with what the old guess said
        // beyond it.
        assert_eq!(
            step(&mut session)?,
            (" to".into(), Some(" someone@example.com".into()))
        );
        assert_eq!(
            step(&mut session)?,
            (String::new(), Some(" someone@example.com and".into()))
        );
        assert_eq!(
            step(&mut session)?,
            (String::new(), Some(" someone@example.com and vin".into()))
        );
        // Nothing new was said, so no guess is made again.
        assert_eq!(session.guess(), None);
        session.finish(&mut |transcript| {
            typed.push_str(&transcript.text);
            Ok(())
        })?;
        assert_eq!(typed, "Send it to someone@example.com and VinPUT.");
        Ok(())
    }
}
