//! Typing words while they may still change.
//!
//! A recognizer that guesses says two things as a dictation goes on: text
//! that is settled, and its guess at what follows, which it replaces with
//! every message. Typing only the settled text puts a word on the screen a
//! second or more after it was said. A [`Typist`] types the guess as well,
//! the moment it arrives, and when the recognizer changes its mind takes
//! back only the end that changed, with Backspace, and types what stands
//! there now.
//!
//! # What is never done
//!
//! Text is only ever taken back from what this dictation typed as a guess:
//! settled text that reads on the screen as it was settled is forgotten
//! here, and nothing reaches behind it. A guess is typed only as far as its
//! characters are sure to be deleted by one Backspace each in any program
//! ([`erasable`]); the rest of it waits to be settled, as all of it used to.
//! And when the sink says that what was typed may no longer be where the
//! caret is ([`TextSink::disturbed`]: another window has the focus, a mouse
//! button or a key that moves the caret went down), what is on the screen
//! stands as it is: Backspace would delete something else. The settled text
//! then goes on after as many words as stood there ([`Stands`]), which may
//! leave a word as it was guessed and not as it was settled.
//!
//! # Pace
//!
//! Words arrive a few at a time. They are typed over the tenth of a second
//! after they arrive rather than all at once, the first character at once,
//! so the caret moves steadily; the further the screen is behind, the faster
//! it is typed ([`Pace`]). Taking text back is never spread out, and once the
//! key is let go nothing is.

use std::time::{Duration, Instant};

use super::TextSink;
use crate::Result;

/// The time over which what is waiting to be typed is typed.
const DRAIN: Duration = Duration::from_millis(100);
/// The slowest the screen is caught up, in characters a second: a word of
/// five letters takes a twelfth of a second, and speech is a quarter as
/// fast.
const SLOWEST: f32 = 60.0;
/// The longest time between two steps that is typed for: a session that
/// stood still does not type a burst for it.
const LONGEST_STEP: Duration = Duration::from_millis(50);
/// How long the end of a guess stands after its beginning was settled
/// without the recognizer having said what follows now. It says so in the
/// same breath, so this only bridges two messages read apart.
const CARRIED_FOR: Duration = Duration::from_millis(60);
/// After how many times that what was typed could not be taken back no
/// guess is typed any more in this dictation.
const GIVE_UP_AFTER: u32 = 2;

/// Whether one Backspace deletes `c`, and nothing else, wherever it was
/// typed: a letter, digit or mark of the Latin, Greek and Cyrillic scripts,
/// or of Chinese, Japanese or Korean, that is one character by itself.
///
/// Left out is everything a program may delete in part or together with its
/// neighbours: characters outside the first plane (two key events, and two
/// Backspaces in some programs), marks that combine with the letter before
/// them, the scripts written in clusters, and line ends and tabs, which a
/// program may turn into something else altogether.
fn erasable(c: char) -> bool {
    matches!(
        u32::from(c),
        0x20..=0x7E
            | 0xA0..=0x2FF
            | 0x370..=0x482
            | 0x48A..=0x52F
            | 0x1E00..=0x1FFF
            | 0x2010..=0x2027
            | 0x2030..=0x205E
            | 0x20A0..=0x20BF
            | 0x3000..=0x3098
            | 0x309B..=0x30FF
            | 0x4E00..=0x9FFF
            | 0xAC00..=0xD7A3
            | 0xFF01..=0xFF60
    )
}

/// As much of a guess, from its start, as may be typed before it is settled.
fn typeable(guess: &str) -> &str {
    let end = guess
        .char_indices()
        .find(|&(_, c)| !erasable(c))
        .map_or(guess.len(), |(at, _)| at);
    &guess[..end]
}

/// How many bytes two texts have in common at their start, in whole
/// characters.
fn common(a: &str, b: &str) -> usize {
    a.char_indices()
        .zip(b.chars())
        .find(|&((_, x), y)| x != y)
        .map_or_else(|| a.len().min(b.len()), |((at, _), _)| at)
}

/// How many bytes the first `count` characters of `text` are.
fn first(text: &str, count: usize) -> usize {
    text.char_indices()
        .nth(count)
        .map_or(text.len(), |(at, _)| at)
}

/// What of the settled text still to come stands on the screen already, in
/// the words of a guess that could not be taken back, and is therefore not
/// typed again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Stands {
    /// Nothing does.
    #[default]
    Nothing,
    /// This many words, of which the settled text is `inside` one. Words
    /// are counted because a word guessed and the same word settled seldom
    /// differ in number, and often in length.
    Words { left: usize, inside: bool },
    /// This many characters, for text that does not put spaces between its
    /// words.
    Characters(usize),
}

impl Stands {
    /// What a guess that reads `shown` on the screen stands for.
    fn of(shown: &str) -> Self {
        let words = shown.split_whitespace().count();
        // Chinese and Japanese: no spaces to count words by.
        if words == 0 || shown.chars().any(|c| u32::from(c) >= 0x3000) {
            return match shown.chars().count() {
                0 => Self::Nothing,
                characters => Self::Characters(characters),
            };
        }
        Self::Words {
            left: words,
            inside: false,
        }
    }

    /// What of `text`, which follows the settled text so far, is still to
    /// be typed.
    fn after<'t>(&mut self, text: &'t str) -> &'t str {
        match *self {
            Self::Nothing => text,
            Self::Characters(left) => {
                let skipped = first(text, left);
                let left = left - text[..skipped].chars().count();
                *self = if left == 0 {
                    Self::Nothing
                } else {
                    Self::Characters(left)
                };
                &text[skipped..]
            }
            Self::Words {
                mut left,
                mut inside,
            } => {
                for (at, c) in text.char_indices() {
                    // A word ends where the space before the next begins.
                    if c.is_whitespace() && inside {
                        inside = false;
                        left -= 1;
                        if left == 0 {
                            *self = Self::Nothing;
                            return &text[at..];
                        }
                    } else if !c.is_whitespace() {
                        inside = true;
                    }
                }
                *self = Self::Words { left, inside };
                ""
            }
        }
    }
}

/// How many characters may be typed now.
#[derive(Debug, Default)]
struct Pace {
    /// When it was last asked, while something was waiting.
    last: Option<Instant>,
    /// What was allowed and not typed: the part of a character.
    credit: f32,
}

impl Pace {
    /// How many of the `waiting` characters may be typed at `now`.
    fn allow(&mut self, now: Instant, waiting: usize) -> usize {
        let Some(last) = self.last.replace(now).filter(|_| waiting > 0) else {
            if waiting == 0 {
                self.last = None;
                return 0;
            }
            // The first character of what has just arrived waits for nothing.
            self.credit = 0.0;
            return 1;
        };
        let passed = now.saturating_duration_since(last).min(LONGEST_STEP);
        #[expect(
            clippy::cast_precision_loss,
            reason = "a number of characters waiting, never large"
        )]
        let waiting_for = waiting as f32;
        let rate = (waiting_for / DRAIN.as_secs_f32()).max(SLOWEST);
        self.credit = (self.credit + rate * passed.as_secs_f32()).min(waiting_for);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "between nothing and the number waiting"
        )]
        let allowed = self.credit.floor() as usize;
        #[expect(clippy::cast_precision_loss, reason = "as above")]
        let typed = allowed as f32;
        self.credit -= typed;
        allowed
    }
}

/// Types the text of one dictation into a sink that can take text back:
/// the settled text, and the guess at what follows it while it stands.
pub(crate) struct Typist<'a, S: TextSink + ?Sized> {
    sink: &'a mut S,
    /// What this dictation typed that may still be taken back: what is on
    /// the screen in the place of `wanted`, as far as it has been typed.
    shown: String,
    /// What should stand there: the settled text that is not on the screen
    /// yet as it was settled, and after it the guess.
    wanted: String,
    /// How many bytes of `wanted`, from its start, are settled.
    settled: usize,
    /// What of the settled text still to come stands on the screen already.
    stands: Stands,
    /// When the end of a guess was kept past the settling of its beginning.
    carried: Option<Instant>,
    /// How many times what was typed could not be taken back.
    let_stand: u32,
    pace: Pace,
}

impl<'a, S: TextSink + ?Sized> Typist<'a, S> {
    pub(crate) fn new(sink: &'a mut S) -> Self {
        Self {
            sink,
            shown: String::new(),
            wanted: String::new(),
            settled: 0,
            stands: Stands::Nothing,
            carried: None,
            let_stand: 0,
            pace: Pace::default(),
        }
    }

    /// Takes text the recognizer has settled, which follows all it settled
    /// before. It takes the place of the guess; what the guess said beyond
    /// it stands until the recognizer says what follows now.
    pub(crate) fn settle(&mut self, text: &str, now: Instant) {
        // What stands on the screen in other words is not typed a second
        // time.
        let text = self.stands.after(text);
        let rest = self.wanted[self.settled..]
            .strip_prefix(text)
            .filter(|rest| !rest.is_empty())
            .map(str::to_owned);
        self.wanted.truncate(self.settled);
        self.wanted.push_str(text);
        self.settled = self.wanted.len();
        self.carried = rest.as_ref().map(|_| now);
        self.wanted.push_str(rest.as_deref().unwrap_or_default());
    }

    /// Takes the recognizer's guess at what follows the settled text, in
    /// place of the one before.
    pub(crate) fn guess(&mut self, guess: &str) {
        self.carried = None;
        self.wanted.truncate(self.settled);
        if self.stands == Stands::Nothing && self.let_stand < GIVE_UP_AFTER {
            self.wanted.push_str(typeable(guess));
        }
    }

    /// Brings the screen closer to what should stand on it, as far as the
    /// pace allows at `now`.
    pub(crate) fn step(&mut self, now: Instant) -> Result<()> {
        if self
            .carried
            .is_some_and(|since| now.saturating_duration_since(since) > CARRIED_FOR)
        {
            self.guess("");
        }
        self.type_some(Some(now))
    }

    /// Brings the screen to what should stand on it, at once.
    pub(crate) fn flush(&mut self) -> Result<()> {
        self.type_some(None)
    }

    /// The recognizer has said all it had to say: a guess it never settled
    /// is taken back.
    pub(crate) fn end(&mut self) -> Result<()> {
        self.guess("");
        self.flush()
    }

    fn type_some(&mut self, now: Option<Instant>) -> Result<()> {
        if !self.shown.is_empty() && self.sink.disturbed() {
            self.let_stand();
        }
        let keep = common(&self.shown, &self.wanted);
        let erase = self.shown[keep..].chars().count();
        let waiting = self.wanted[keep..].chars().count();
        let allowed = match now {
            // What takes the place of text that is taken back is typed with
            // it, so that the caret never jumps back and waits there.
            Some(now) => self.pace.allow(now, waiting).max(erase.min(waiting)),
            None => waiting,
        };
        if erase == 0 && allowed == 0 {
            return Ok(());
        }
        let end = keep + first(&self.wanted[keep..], allowed);
        let text = &self.wanted[keep..end];
        self.sink.rewrite(erase, text)?;
        self.shown.truncate(keep);
        self.shown.push_str(text);
        // What is settled and on the screen is done with for good.
        let done = common(&self.shown, &self.wanted).min(self.settled);
        self.shown.drain(..done);
        self.wanted.drain(..done);
        self.settled -= done;
        Ok(())
    }

    /// What is on the screen can no longer be taken back. It stands for as
    /// many words of what was wanted; no guess is typed until the settled
    /// text has passed it.
    fn let_stand(&mut self) {
        self.let_stand += 1;
        self.stands = Stands::of(&self.shown);
        self.shown.clear();
        self.wanted.truncate(self.settled);
        let settled = std::mem::take(&mut self.wanted);
        self.wanted.push_str(self.stands.after(&settled));
        self.settled = self.wanted.len();
        self.carried = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A text field: what was typed into it, and each time it was typed
    /// into, how much was taken back and what was typed.
    #[derive(Default)]
    struct Field {
        text: String,
        edits: Vec<(usize, String)>,
        /// Whether the caret has moved away from what was typed.
        disturbed: bool,
    }

    impl TextSink for Field {
        fn type_text(&mut self, text: &str) -> Result<()> {
            self.rewrite(0, text)
        }

        fn rewrites(&self) -> bool {
            true
        }

        fn rewrite(&mut self, erase: usize, text: &str) -> Result<()> {
            for _ in 0..erase {
                assert!(self.text.pop().is_some(), "more taken back than typed");
            }
            self.text.push_str(text);
            self.edits.push((erase, text.to_owned()));
            Ok(())
        }

        fn disturbed(&mut self) -> bool {
            self.disturbed
        }
    }

    fn edits(field: &Field) -> Vec<(usize, &str)> {
        field
            .edits
            .iter()
            .map(|(erase, text)| (*erase, text.as_str()))
            .collect()
    }

    #[test]
    fn a_guess_is_typed_and_only_its_changed_end_is_taken_back() -> Result<()> {
        let mut field = Field::default();
        let mut typist = Typist::new(&mut field);
        typist.guess("Hello wor");
        typist.flush()?;
        typist.guess("Hello world");
        typist.flush()?;
        typist.guess("Hello word is");
        typist.flush()?;
        // The same guess again types nothing.
        typist.guess("Hello word is");
        typist.flush()?;
        assert_eq!(field.text, "Hello word is");
        assert_eq!(edits(&field), [(0, "Hello wor"), (0, "ld"), (2, "d is")]);
        Ok(())
    }

    #[test]
    fn settling_what_was_guessed_types_nothing() -> Result<()> {
        let now = Instant::now();
        let mut field = Field::default();
        let mut typist = Typist::new(&mut field);
        typist.guess("Hello wor");
        typist.flush()?;
        // The beginning is settled; the recognizer goes on guessing the end.
        typist.settle("Hello", now);
        typist.guess(" world");
        typist.flush()?;
        typist.settle(" world", now);
        typist.end()?;
        assert_eq!(field.text, "Hello world");
        assert_eq!(edits(&field), [(0, "Hello wor"), (0, "ld")]);
        Ok(())
    }

    #[test]
    fn text_settled_in_other_words_than_guessed_takes_their_place() -> Result<()> {
        let now = Instant::now();
        let mut field = Field::default();
        let mut typist = Typist::new(&mut field);
        typist.guess("I scream");
        typist.flush()?;
        typist.settle("Ice cream.", now);
        typist.flush()?;
        assert_eq!(field.text, "Ice cream.");
        assert_eq!(edits(&field), [(0, "I scream"), (7, "ce cream.")]);
        Ok(())
    }

    #[test]
    fn settled_text_is_never_taken_back() -> Result<()> {
        let now = Instant::now();
        let mut field = Field::default();
        let mut typist = Typist::new(&mut field);
        typist.settle("One. ", now);
        typist.guess("Two");
        typist.flush()?;
        // Whatever follows, the most that is ever taken back is the guess.
        typist.guess("");
        typist.flush()?;
        typist.guess("Three");
        typist.flush()?;
        typist.settle("Four.", now);
        typist.end()?;
        assert_eq!(field.text, "One. Four.");
        let most = field.edits.iter().map(|(erase, _)| *erase).max();
        assert_eq!(most, Some(5));
        Ok(())
    }

    #[test]
    fn the_end_of_a_guess_stands_until_the_recognizer_says_what_follows() -> Result<()> {
        let now = Instant::now();
        let mut field = Field::default();
        let mut typist = Typist::new(&mut field);
        typist.guess("Hello wor");
        typist.flush()?;
        // The settled beginning and the new guess are read apart.
        typist.settle("Hello", now);
        typist.step(now + Duration::from_millis(10))?;
        assert_eq!(field_of(&typist), "Hello wor");
        typist.guess(" world");
        typist.flush()?;
        assert_eq!(field_of(&typist), "Hello world");

        // A recognizer that says nothing more has withdrawn it.
        typist.settle(" world", now);
        typist.guess(" and");
        typist.flush()?;
        typist.settle(" a", now);
        typist.step(now + Duration::from_millis(10))?;
        assert_eq!(field_of(&typist), "Hello world and");
        typist.step(now + Duration::from_millis(100))?;
        assert_eq!(field_of(&typist), "Hello world a");
        Ok(())
    }

    fn field_of<'a>(typist: &'a Typist<'_, Field>) -> &'a str {
        &typist.sink.text
    }

    #[test]
    fn a_guess_is_typed_only_as_far_as_one_backspace_deletes_each_character() -> Result<()> {
        let now = Instant::now();
        let mut field = Field::default();
        let mut typist = Typist::new(&mut field);
        typist.guess("Živjo 😀 svet");
        typist.flush()?;
        assert_eq!(field_of(&typist), "Živjo ");
        typist.guess("Line\nnext");
        typist.flush()?;
        assert_eq!(field_of(&typist), "Line");
        // Settled, all of it is typed.
        typist.settle("Živjo 😀\n", now);
        typist.end()?;
        assert_eq!(field.text, "Živjo 😀\n");
        for kept in [
            "čšž ĐÆ",
            "Ελληνικά",
            "Привет",
            "日本語のテキスト",
            "한국어",
            "€ 5 – “x”…",
        ] {
            assert_eq!(typeable(kept), kept);
        }
        for cut in ["e\u{301}", "नमस्ते", "שלום", "مرحبا", "a\tb", "x\u{200d}y"]
        {
            assert!(typeable(cut).len() < cut.len(), "{cut}");
        }
        Ok(())
    }

    #[test]
    fn text_is_typed_over_a_moment_and_its_first_character_at_once() -> Result<()> {
        let start = Instant::now();
        let at = |millis: u64| start + Duration::from_millis(millis);
        let mut field = Field::default();
        let mut typist = Typist::new(&mut field);
        typist.guess(" hello");
        typist.step(at(0))?;
        assert_eq!(field_of(&typist), " ");
        let mut lengths = Vec::new();
        for tick in 1..=20 {
            typist.step(at(tick * 10))?;
            lengths.push(field_of(&typist).len());
        }
        // Never more than a character in a step of 10 ms, and all of it
        // within the time a word is given.
        assert!(lengths.windows(2).all(|pair| pair[1] - pair[0] <= 1));
        assert_eq!(lengths[9], 6, "{lengths:?}");

        // A long text that arrives at once is typed faster, and slows down
        // as it is caught up with.
        let long = " and then a great deal more than anyone says in a breath";
        typist.guess(&format!(" hello{long}"));
        let mut lengths = vec![6];
        for tick in 21..=60 {
            typist.step(at(tick * 10))?;
            lengths.push(field_of(&typist).len());
        }
        assert!(lengths[3] - lengths[1] >= 8, "{lengths:?}");
        assert!(lengths[30] - lengths[28] <= 2, "{lengths:?}");
        assert_eq!(field_of(&typist).len(), 6 + long.len());
        // A session that stood still does not make up for it in one burst.
        typist.guess(&format!(" hello{long} and on"));
        typist.step(at(5000))?;
        typist.step(at(5010))?;
        assert!(field_of(&typist).len() < 6 + long.len() + 4);
        Ok(())
    }

    #[test]
    fn what_takes_the_place_of_text_taken_back_is_typed_with_it() -> Result<()> {
        let start = Instant::now();
        let mut field = Field::default();
        let mut typist = Typist::new(&mut field);
        typist.guess("their house");
        typist.flush()?;
        typist.guess("there is a");
        typist.step(start)?;
        assert_eq!(edits(&field)[1], (8, "re is a"));
        Ok(())
    }

    #[test]
    fn nothing_is_taken_back_once_the_caret_may_be_elsewhere() -> Result<()> {
        let now = Instant::now();
        let mut field = Field::default();
        let mut typist = Typist::new(&mut field);
        typist.settle("One. ", now);
        typist.guess("I scream");
        typist.flush()?;
        typist.sink.disturbed = true;
        typist.guess("Ice cream for");
        typist.flush()?;
        // What is settled goes on after as many words as stood there,
        typist.settle("Ice cr", now);
        typist.flush()?;
        typist.sink.disturbed = false;
        typist.settle("eam for all.", now);
        typist.flush()?;
        assert_eq!(field_of(&typist), "One. I scream for all.");
        // and from there guesses are typed again.
        typist.guess(" More");
        typist.flush()?;
        assert_eq!(field_of(&typist), "One. I scream for all. More");
        assert!(field.edits.iter().all(|(erase, _)| *erase == 0));
        Ok(())
    }

    #[test]
    fn a_dictation_that_keeps_being_disturbed_types_settled_text_alone() -> Result<()> {
        let now = Instant::now();
        let mut field = Field {
            disturbed: true,
            ..Field::default()
        };
        let mut typist = Typist::new(&mut field);
        for word in [" one", " two", " three"] {
            typist.guess(word);
            typist.flush()?;
            typist.settle(word, now);
            typist.flush()?;
        }
        typist.sink.disturbed = false;
        typist.guess(" four");
        typist.end()?;
        assert_eq!(field.text, " one two three");
        assert!(field.edits.iter().all(|(erase, _)| *erase == 0));
        Ok(())
    }

    #[test]
    fn what_stood_is_counted_in_words_or_where_there_are_none_in_characters() {
        let mut stands = Stands::of(" quick bro");
        assert_eq!(stands.after(" qu"), "");
        assert_eq!(stands.after("ick"), "");
        assert_eq!(stands.after(" brown fox"), " fox");
        assert_eq!(stands, Stands::Nothing);
        assert_eq!(stands.after(" jumps"), " jumps");

        // Settled text that was waiting to be typed is passed over as well.
        let mut stands = Stands::of("One two");
        assert_eq!(stands.after("One"), "");
        assert_eq!(stands.after(" two."), "");
        assert_eq!(stands.after(" Three"), " Three");

        let mut stands = Stands::of("今日は");
        assert_eq!(stands.after("今日"), "");
        assert_eq!(stands.after("は晴れ"), "晴れ");
        assert_eq!(stands, Stands::Nothing);

        assert_eq!(Stands::of(""), Stands::Nothing);
        assert_eq!(Stands::of(" "), Stands::Characters(1));
    }

    /// Replays what the server of `VinPUT` Pro sent for two recorded
    /// sentences, one English and one Slovenian, each message at the moment
    /// it arrived, with the screen brought up to date every 10 ms as a
    /// dictation does. The recordings are in `recorded/`: the milliseconds
    /// since the audio began, `partial` or `final`, and the text.
    ///
    /// Says, with `--nocapture`, when the first word was on the screen and
    /// when it would have been had only settled text been typed.
    #[test]
    fn recorded_dictations_are_on_the_screen_as_they_are_said() -> Result<()> {
        let recordings = [
            ("en-02", include_str!("recorded/en-02.json")),
            ("sl-17", include_str!("recorded/sl-17.json")),
        ];
        for (name, recorded) in recordings {
            let recorded: serde_json::Value = serde_json::from_str(recorded).unwrap_or_default();
            let events: Vec<(u64, bool, &str)> = recorded
                .as_array()
                .into_iter()
                .flatten()
                .map(|event| {
                    let at = event[0].as_u64().unwrap_or_default();
                    let text = event[2].as_str().unwrap_or_default();
                    (at, event[1] == "final", text)
                })
                .collect();
            assert!(events.len() > 20, "{name}");
            let said: String = events
                .iter()
                .filter(|event| event.1)
                .map(|event| event.2)
                .collect();
            let first_guess = events.first().map_or(0, |event| event.0);
            let first_settled = events
                .iter()
                .find(|event| event.1)
                .map_or(0, |event| event.0);
            let last = events.last().map_or(0, |event| event.0);

            let start = Instant::now();
            let mut field = Field::default();
            let mut typist = Typist::new(&mut field);
            let mut coming = events.iter().peekable();
            let (mut first_shown, mut behind) = (None, 0);
            for tick in (0..last + 300).step_by(10) {
                let now = start + Duration::from_millis(tick);
                while let Some(&(_, settled, text)) = coming.next_if(|event| event.0 <= tick) {
                    if settled {
                        typist.settle(text, now);
                    } else {
                        typist.guess(text);
                    }
                }
                typist.step(now)?;
                if !field_of(&typist).is_empty() {
                    first_shown.get_or_insert(tick);
                }
                // How far the screen is behind what the server has said.
                let waiting = typist.wanted.len() - common(&typist.shown, &typist.wanted);
                behind = behind.max(waiting);
            }
            typist.end()?;
            assert_eq!(field.text, said, "{name}");

            let first_shown = first_shown.unwrap_or(u64::MAX);
            let erased: usize = field.edits.iter().map(|(erase, _)| erase).sum();
            eprintln!(
                "{name}: first word on the screen {first_shown} ms after the audio began, with \
                 settled text alone {first_settled} ms; {erased} of {} characters taken back, \
                 never more than {behind} bytes behind",
                said.chars().count()
            );
            // The first guess is typed in the step it arrived in, long
            // before anything is settled.
            assert!(first_shown < first_guess + 10, "{name}");
            assert!(first_shown + 500 < first_settled, "{name}");
            // The guesses of this service grow at their end; little of what
            // was typed ever has to be taken back.
            assert!(erased * 10 < said.chars().count(), "{name}: {erased}");
        }
        Ok(())
    }

    /// Whatever the recognizer guesses on the way, and however the steps
    /// fall, what is on the screen in the end is what it settled.
    #[test]
    fn the_screen_ends_as_the_settled_text_whatever_was_guessed() -> Result<()> {
        let words = [
            " the", " their", " there", " a", " is", ".", " Ice", " I", " scream", "😀",
        ];
        let mut seed = 0x2545_F491_u32;
        let mut next = |below: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as usize % below
        };
        for _ in 0..300 {
            let start = Instant::now();
            let mut clock = 0;
            let mut field = Field::default();
            let mut typist = Typist::new(&mut field);
            let mut settled = String::new();
            for _ in 0..40 {
                clock += next(40) as u64;
                let now = start + Duration::from_millis(clock);
                match next(4) {
                    0 => {
                        let text: String = (0..next(3)).map(|_| words[next(words.len())]).collect();
                        settled.push_str(&text);
                        typist.settle(&text, now);
                    }
                    1 | 2 => {
                        let text: String = (0..next(4)).map(|_| words[next(words.len())]).collect();
                        typist.guess(&text);
                    }
                    _ => {}
                }
                if next(5) == 0 {
                    typist.flush()?;
                } else {
                    typist.step(now)?;
                }
            }
            typist.end()?;
            assert_eq!(field.text, settled);
        }
        Ok(())
    }
}
