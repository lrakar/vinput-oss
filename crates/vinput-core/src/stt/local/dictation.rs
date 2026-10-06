//! Recognizing one dictation with an engine server, a stretch at a time.
//!
//! Each stretch is one request, or up to three when the language has to be
//! found first. What has been recognized so far goes along as the engine's
//! prompt, so a sentence that was cut in two reads as one, and what comes
//! back is ready to type behind what came before.
//!
//! Which language: the engine is never asked how likely each language is,
//! because that costs a second pass over the audio on every request. With one
//! configured language the engine is told it. With several it detects the
//! language itself and says which it was. If that is not a configured one,
//! which short recordings and small models invite, the stretch is recognized
//! again in a configured language, this once with the likelihoods, and a
//! third time if they point at another configured one. One detection on a
//! few seconds can be wrong, so every stretch is detected afresh until two
//! in a row agree, or one language has been heard for eight seconds on end.
//! From there it stands for the rest of the dictation: no later stretch is
//! taken for another language, and none pays for detection again.
//!
//! The engine makes words up for audio without speech: "Thank you." for a
//! breath after the last sentence, the name of a sound for a click. Told
//! which language to hear it does so most readily, and its own measure of how
//! likely the audio is to hold no speech is then of no use: it is next to
//! zero whatever the audio. So a stretch that may not be speech ([`Care`])
//! is first recognized with the language left open, when the measure works
//! and noise mostly comes back as nothing. Only what passes that is
//! recognized in the language of the dictation. Such a stretch also goes
//! without the prompt, which the engine tends to go on with or say again
//! when it hears nothing, and what it comes to is not typed if the engine
//! was unsure of it, if it only repeats the end of what is typed already or
//! one phrase over and over, or, where nothing was as loud as the voice, if
//! it is one of the phrases the engine is known for making up.

use std::sync::Arc;

use serde_json::Value;

use super::clean;
use super::languages::{code_of, spaced};
use super::phantoms;
use crate::Result;
use crate::audio::{SAMPLE_RATE_HZ, wav_bytes};

/// This much audio on end detected as one language settles the language of
/// the dictation, and so do two stretches in a row however short.
const SETTLING_SAMPLES: usize = 8 * SAMPLE_RATE_HZ as usize;
/// Likelihoods settle the language when they favour one configured language
/// over every other by this factor.
const CLEARLY: f64 = 10.0;
/// About how many bytes of the text so far go along as the prompt. The
/// engine reads only the end of a long prompt, and a sentence or two are
/// what carries names, spelling and capital letters across a cut.
const PROMPT_LENGTH: usize = 200;
/// A part of an answer is left out when the engine finds it likelier than
/// this that the audio holds no speech: the engine's own bar.
const NO_SPEECH: f64 = 0.6;
/// In a stretch that may not be speech, a part of an answer is left out when
/// the mean log likelihood of its tokens is under this. Words made up for
/// noise in a named language were measured at -1.9 and under, what was said
/// softly at -1.1 and over.
const UNSURE: f64 = -1.5;

/// The care a stretch is recognized with.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct Care {
    /// It may not be speech: short, soft, even, or what was left after the
    /// last pause with little sound in it.
    pub(super) doubtful: bool,
    /// Nothing in it is as loud as the voice was, or as a voice is.
    pub(super) quiet: bool,
}

/// What answers recognition requests: the engine's server, or a stand-in in
/// tests.
pub(super) trait Inference: Send + Sync {
    /// Makes one request with the form `fields` and the recording `wav`, and
    /// returns the engine's answer, which has a `text`.
    fn infer(&self, fields: &[(&str, &str)], wav: &[u8]) -> Result<Value>;
}

/// How much the engine is asked to say besides the text.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Detail {
    Text,
    /// Which language it recognized.
    Language,
    /// That, and how likely it finds each language: a second pass.
    Likelihoods,
}

/// One dictation: what has been recognized of it, and in which language.
pub(super) struct Dictation {
    engine: Arc<dyn Inference>,
    /// The configured languages, as the engine's codes.
    languages: Vec<String>,
    /// The language of this dictation, once it is known.
    settled: Option<String>,
    /// The language the stretch before was recognized in, or at first the
    /// dictation before.
    last: Option<String>,
    /// The language the last stretches were clearly in, how many of them in
    /// a row, and how much audio they came to.
    agreed: Option<(String, usize, usize)>,
    /// Whether the stretch being recognized may not be speech.
    doubtful: bool,
    /// Everything typed so far.
    said: String,
}

impl Dictation {
    /// A dictation in one of `languages`, the configured ones. `guess` is
    /// the language the user spoke last, if that is known.
    pub(super) fn new(
        engine: Arc<dyn Inference>,
        languages: Vec<String>,
        guess: Option<String>,
    ) -> Self {
        let settled = match languages.as_slice() {
            [only] => Some(only.clone()),
            _ => None,
        };
        Self {
            last: guess.filter(|guess| languages.contains(guess)),
            engine,
            languages,
            settled,
            agreed: None,
            doubtful: false,
            said: String::new(),
        }
    }

    /// The language of the last stretch that had speech in it.
    pub(super) fn language(&self) -> Option<&str> {
        self.last.as_deref()
    }

    /// Recognizes the next stretch and returns what is to be typed for it,
    /// with the space that sets it apart from the text before. Empty if
    /// nothing was said in it.
    pub(super) fn recognize(&mut self, samples: &[i16], care: Care) -> Result<String> {
        self.doubtful = care.doubtful;
        let wav = wav_bytes(samples);
        let (reply, language) = match self.settled.clone() {
            Some(language) if care.doubtful => match self.checked(&wav, &language)? {
                Some(reply) => (reply, Some(language)),
                None => return Ok(String::new()),
            },
            Some(language) => (self.ask(&wav, &language, Detail::Text)?, Some(language)),
            None => {
                let (reply, language, clear) = self.detect(&wav)?;
                self.heard(language.as_deref().filter(|_| clear), samples.len());
                (reply, language)
            }
        };
        let text = spoken(&reply, care.doubtful);
        if text.is_empty()
            || (care.doubtful && (repeats(&self.said, &text) || phantoms::stutters(&text)))
            || (care.quiet && phantoms::made_up(&text))
        {
            return Ok(String::new());
        }
        let apart = !self.said.is_empty() && language.as_deref().is_none_or(spaced);
        self.last = language;
        let typed = self.said.len();
        if apart {
            self.said.push(' ');
        }
        self.said.push_str(&text);
        Ok(self.said[typed..].to_owned())
    }

    /// Recognizes, in `language`, a recording that may not be speech. `None`
    /// if the engine hears no speech in it when it is not told what to hear.
    fn checked(&self, wav: &[u8], language: &str) -> Result<Option<Value>> {
        let open = self.ask(wav, "auto", Detail::Language)?;
        if spoken(&open, true).is_empty() {
            return Ok(None);
        }
        if open["language"].as_str().and_then(code_of) == Some(language) {
            return Ok(Some(open));
        }
        // Speech, taken for another language, as a word or two often are.
        self.ask(wav, language, Detail::Language).map(Some)
    }

    /// Notes the language a stretch of `samples` was clearly in, if any,
    /// and settles the language of the dictation once that is safe.
    fn heard(&mut self, language: Option<&str>, samples: usize) {
        self.agreed = language.map(|language| match self.agreed.take() {
            Some((before, stretches, heard)) if before == language => {
                (before, stretches + 1, heard + samples)
            }
            _ => (language.to_owned(), 1, samples),
        });
        if let Some((language, stretches, heard)) = &self.agreed
            && (*stretches >= 2 || *heard >= SETTLING_SAMPLES)
        {
            self.settled = Some(language.clone());
        }
    }

    /// Recognizes a recording whose language is not known yet. Returns the
    /// answer, the language it is in, and whether that language is clear
    /// enough to hold for the rest of the dictation.
    fn detect(&self, wav: &[u8]) -> Result<(Value, Option<String>, bool)> {
        let reply = self.ask(wav, "auto", Detail::Language)?;
        if spoken(&reply, self.doubtful).is_empty() {
            // No speech. Asked again in a language of the user's, the engine
            // would find words for the noise.
            return Ok((reply, None, false));
        }
        let detected = reply["language"].as_str().and_then(code_of);
        let configured = |code: &str| self.languages.iter().any(|language| language == code);
        if self.languages.is_empty() || detected.is_some_and(configured) {
            return Ok((reply, detected.map(str::to_owned), detected.is_some()));
        }
        // Not a language the user speaks. The best guess is the one they
        // spoke a moment ago.
        let guess = self.last.as_ref().or(self.languages.first());
        let Some(guess) = guess else {
            return Ok((reply, None, false));
        };
        let reply = self.ask(wav, guess, Detail::Likelihoods)?;
        let (likeliest, clear) = likeliest(&self.languages, &reply["language_probabilities"]);
        match likeliest {
            Some(language) if language != guess => {
                let reply = self.ask(wav, language, Detail::Text)?;
                Ok((reply, Some(language.to_owned()), clear))
            }
            _ => Ok((reply, Some(guess.clone()), clear)),
        }
    }

    fn ask(&self, wav: &[u8], language: &str, detail: Detail) -> Result<Value> {
        let mut fields = vec![
            (
                "response_format",
                if detail == Detail::Text {
                    "json"
                } else {
                    "verbose_json"
                },
            ),
            ("temperature", "0.0"),
            ("language", language),
        ];
        if detail == Detail::Language {
            fields.push(("no_language_probabilities", "true"));
        }
        let prompt = if self.doubtful {
            ""
        } else {
            prompt(&self.said)
        };
        if !prompt.is_empty() {
            fields.push(("prompt", prompt));
        }
        self.engine.infer(&fields, wav)
    }
}

/// The configured language the engine finds likeliest, if it lists any, and
/// whether it is [`CLEARLY`] likelier than every other configured one. The
/// engine leaves out the languages it finds next to impossible.
fn likeliest<'a>(languages: &'a [String], likelihoods: &Value) -> (Option<&'a str>, bool) {
    let mut ranked: Vec<(&str, f64)> = languages
        .iter()
        .map(|code| {
            let likelihood = likelihoods.get(code).and_then(Value::as_f64);
            (code.as_str(), likelihood.unwrap_or(0.0))
        })
        .collect();
    // Stable, so of equally likely languages the one configured first leads.
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    match ranked.as_slice() {
        [(first, likelihood), rest @ ..] if *likelihood > 0.0 => {
            let clear = rest.iter().all(|other| *likelihood >= other.1 * CLEARLY);
            (Some(first), clear)
        }
        _ => (None, false),
    }
}

/// The text of an answer, tidied. The engine breaks it into lines of some
/// sixty characters wherever that falls, inside a word too, and every line
/// that starts a word starts with a space of its own, so the lines are put
/// back together as they are.
///
/// An answer that lists its parts is read part by part: a part is left out
/// if the engine finds it likely that there was no speech, if it is nothing
/// but the name of a sound, or, with `doubtful`, if the engine was unsure of
/// its words.
fn spoken(reply: &Value, doubtful: bool) -> String {
    let parts = reply["segments"].as_array();
    let Some(parts) = parts.filter(|parts| !parts.is_empty()) else {
        return clean(&reply["text"].as_str().unwrap_or_default().replace('\n', ""));
    };
    let mut text = String::new();
    for part in parts {
        let no_speech = part["no_speech_prob"].as_f64().unwrap_or(0.0);
        let sure = part["avg_logprob"].as_f64().unwrap_or(0.0);
        let words = part["text"].as_str().unwrap_or_default();
        if no_speech > NO_SPEECH || (doubtful && sure < UNSURE) || clean(words).is_empty() {
            continue;
        }
        text.push_str(words);
    }
    clean(&text.replace('\n', ""))
}

/// Whether `text` says nothing but what `said` ends with, whatever the
/// capitals and the punctuation.
fn repeats(said: &str, text: &str) -> bool {
    let words = |text: &str| -> Vec<String> {
        text.split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .map(str::to_lowercase)
            .collect()
    };
    let (said, text) = (words(said), words(text));
    !text.is_empty() && said.ends_with(&text)
}

/// The end of the text so far, from the start of a word.
fn prompt(said: &str) -> &str {
    let Some(mut start) = said.len().checked_sub(PROMPT_LENGTH) else {
        return said;
    };
    while !said.is_char_boundary(start) {
        start += 1;
    }
    let tail = &said[start..];
    match tail.find(char::is_whitespace) {
        Some(space) if start > 0 => tail[space..].trim_start(),
        _ => tail,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;

    /// An engine that gives the answers it was handed, in order, and keeps
    /// the form of every request.
    #[derive(Default)]
    struct Scripted {
        answers: Mutex<VecDeque<Value>>,
        asked: Mutex<Vec<Vec<(String, String)>>>,
    }

    impl Inference for Scripted {
        fn infer(&self, fields: &[(&str, &str)], _wav: &[u8]) -> Result<Value> {
            let form = fields
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect();
            if let Ok(mut asked) = self.asked.lock() {
                asked.push(form);
            }
            let answer = self.answers.lock().ok().and_then(|mut all| all.pop_front());
            answer.ok_or_else(|| crate::Error::Transcription("asked once too often".into()))
        }
    }

    /// A dictation in `languages` with an engine that gives `answers`.
    fn dictation(languages: &[&str], answers: Vec<Value>) -> (Dictation, Arc<Scripted>) {
        let engine = Arc::new(Scripted {
            answers: Mutex::new(answers.into()),
            ..Scripted::default()
        });
        let languages = languages.iter().map(|code| (*code).to_owned()).collect();
        (Dictation::new(engine.clone(), languages, None), engine)
    }

    /// What request number `index` had in its field `name`.
    fn asked(engine: &Scripted, index: usize, name: &str) -> Option<String> {
        let asked = engine.asked.lock().ok()?;
        let form = asked.get(index)?;
        let field = form.iter().find(|(field, _)| field == name)?;
        Some(field.1.clone())
    }

    fn requests(engine: &Scripted) -> usize {
        engine.asked.lock().map_or(0, |asked| asked.len())
    }

    const LONG: &[i16] = &[0; SETTLING_SAMPLES];
    const SHORT: &[i16] = &[0; SETTLING_SAMPLES / 4];

    impl Dictation {
        /// Recognizes a stretch that is plainly speech.
        fn stretch(&mut self, samples: &[i16]) -> Result<String> {
            self.recognize(samples, Care::default())
        }
    }

    #[test]
    fn one_language_is_named_and_nothing_else_asked_for() {
        let (mut dictation, engine) = dictation(&["sl"], vec![json!({"text": " Dober dan.\n"})]);
        assert_eq!(dictation.stretch(LONG).ok(), Some("Dober dan.".into()));
        assert_eq!(asked(&engine, 0, "language").as_deref(), Some("sl"));
        assert_eq!(
            asked(&engine, 0, "response_format").as_deref(),
            Some("json")
        );
        assert_eq!(asked(&engine, 0, "prompt"), None);
        assert_eq!(requests(&engine), 1);
    }

    #[test]
    fn a_detected_language_that_is_configured_stands_for_the_dictation() {
        let (mut dictation, engine) = dictation(
            &["en", "sl"],
            vec![
                json!({"text": " Dober dan.", "language": "slovenian"}),
                json!({"text": " Kako ste?"}),
            ],
        );
        assert_eq!(dictation.stretch(LONG).ok(), Some("Dober dan.".into()));
        assert_eq!(asked(&engine, 0, "language").as_deref(), Some("auto"));
        assert_eq!(
            asked(&engine, 0, "response_format").as_deref(),
            Some("verbose_json")
        );
        // The likelihoods, a second pass over the audio, are not asked for.
        assert_eq!(
            asked(&engine, 0, "no_language_probabilities").as_deref(),
            Some("true")
        );
        // The next stretch is not detected again, however short it is.
        assert_eq!(dictation.stretch(SHORT).ok(), Some(" Kako ste?".into()));
        assert_eq!(asked(&engine, 1, "language").as_deref(), Some("sl"));
        assert_eq!(
            asked(&engine, 1, "response_format").as_deref(),
            Some("json")
        );
        assert_eq!(requests(&engine), 2);
    }

    #[test]
    fn one_short_stretch_does_not_settle_the_language_but_two_that_agree_do() {
        // A Slovenian opening taken for English does not drag the rest of
        // the dictation into English.
        let (mut dictation, engine) = dictation(
            &["en", "sl"],
            vec![
                json!({"text": " The bar done.", "language": "english"}),
                json!({"text": " Danes je lep dan.", "language": "slovenian"}),
                json!({"text": " Gremo ven.", "language": "slovenian"}),
                json!({"text": " Takoj."}),
            ],
        );
        assert!(dictation.stretch(SHORT).is_ok());
        assert!(dictation.stretch(SHORT).is_ok());
        assert_eq!(asked(&engine, 1, "language").as_deref(), Some("auto"));
        // English once, Slovenian once: still open.
        assert!(dictation.stretch(SHORT).is_ok());
        assert_eq!(asked(&engine, 2, "language").as_deref(), Some("auto"));
        // Slovenian twice in a row: settled.
        assert_eq!(dictation.stretch(SHORT).ok(), Some(" Takoj.".into()));
        assert_eq!(asked(&engine, 3, "language").as_deref(), Some("sl"));
    }

    const DOUBTFUL: Care = Care {
        doubtful: true,
        quiet: false,
    };
    const QUIET: Care = Care {
        doubtful: true,
        quiet: true,
    };

    /// An answer in one part, with the engine's measures of it.
    fn part(text: &str, language: &str, no_speech: f64, sure: f64) -> Value {
        json!({"text": text, "language": language, "segments": [
            {"text": text, "no_speech_prob": no_speech, "avg_logprob": sure},
        ]})
    }

    /// What a stretch recognized with `care` comes to.
    fn careful(dictation: &mut Dictation, care: Care) -> Option<String> {
        dictation.recognize(SHORT, care).ok()
    }

    #[test]
    fn a_doubtful_stretch_goes_without_the_prompt_and_may_not_repeat_the_text() {
        let (mut dictation, engine) = dictation(
            &["en"],
            vec![
                json!({"text": " Send it by Friday. Thank you."}),
                part(" Thank you.", "english", 0.1, -0.2),
                part(" Thanks.", "english", 0.1, -0.2),
                part(" You guys, you guys, you guys.", "english", 0.5, -0.4),
            ],
        );
        assert!(dictation.stretch(LONG).is_ok());
        // A breath, and the engine says the last words again: not typed.
        assert_eq!(careful(&mut dictation, DOUBTFUL), Some(String::new()));
        // The language is left open, so that the engine's measure of speech
        // is worth something, and the prompt is left out.
        assert_eq!(asked(&engine, 1, "language").as_deref(), Some("auto"));
        assert_eq!(asked(&engine, 1, "prompt"), None);
        // A soft last word is typed like any other, and takes one request.
        assert_eq!(careful(&mut dictation, DOUBTFUL), Some(" Thanks.".into()));
        // One phrase over and over is what the engine makes of a hum.
        assert_eq!(careful(&mut dictation, DOUBTFUL), Some(String::new()));
        assert_eq!(requests(&engine), 4);
        assert!(repeats("We meet at ten.", "at TEN"));
        assert!(!repeats("We meet at ten.", "We meet"));
        assert!(!repeats("", "Yes."));
    }

    #[test]
    fn noise_is_not_recognized_in_the_language_of_the_dictation() {
        // What the engine answers for the room and a click with the language
        // left open: nothing, the name of a sound, or likely no speech.
        for noise in [
            json!({"text": "", "language": "nynorsk", "segments": []}),
            part(" [BLANK_AUDIO]", "english", 0.85, -0.2),
            part(" (upbeat music)", "english", 0.2, -0.3),
            part(" *phone rings*", "english", 0.1, -0.3),
            part(" Thank you.", "english", 0.72, -0.3),
        ] {
            // One language: asked once, and not again in Slovenian, in which
            // the engine would find words for it.
            let (mut one, engine) = dictation(&["sl"], vec![noise.clone()]);
            assert_eq!(careful(&mut one, DOUBTFUL), Some(String::new()));
            assert_eq!(requests(&engine), 1);
            // Several languages: the same, whether or not the stretch looked
            // like speech, and the language is none the more settled for it.
            let (mut several, engine) = dictation(&["en", "sl"], vec![noise]);
            assert_eq!(several.stretch(LONG).ok(), Some(String::new()));
            assert_eq!(requests(&engine), 1);
            assert_eq!(several.language(), None);
        }
    }

    #[test]
    fn a_short_word_taken_for_another_language_is_recognized_in_the_right_one() {
        let (mut dictation, engine) = dictation(
            &["sl"],
            vec![
                part(" Da.", "russian", 0.05, -0.4),
                part(" Da.", "slovenian", 0.0, -0.3),
                // Speech to the open ear, but in Slovenian the engine can
                // only guess at it.
                part(" Here.", "english", 0.1, -0.5),
                part(" Jeste?", "slovenian", 0.0, -1.9),
            ],
        );
        assert_eq!(careful(&mut dictation, DOUBTFUL), Some("Da.".into()));
        assert_eq!(asked(&engine, 0, "language").as_deref(), Some("auto"));
        assert_eq!(asked(&engine, 1, "language").as_deref(), Some("sl"));
        assert_eq!(careful(&mut dictation, DOUBTFUL), Some(String::new()));
        assert_eq!(requests(&engine), 4);
    }

    #[test]
    fn a_made_up_phrase_is_dropped_only_where_nothing_was_as_loud_as_the_voice() {
        let thanks = || part(" Thank you.", "english", 0.1, -0.1);
        let (mut dictation, _) = dictation(
            &["en", "sl"],
            vec![
                json!({"text": " Send it by Friday.", "language": "english"}),
                thanks(),
                // Taken for Slovenian, and asked for again in English.
                part(" Hvala za ogled.", "slovenian", 0.1, -0.2),
                thanks(),
                thanks(),
                part(" Thank you for the flowers.", "english", 0.1, -0.2),
            ],
        );
        assert!(dictation.stretch(LONG).is_ok());
        // After the last pause, with the room and the key in it: made up.
        assert_eq!(careful(&mut dictation, QUIET), Some(String::new()));
        assert_eq!(careful(&mut dictation, QUIET), Some(String::new()));
        // Said aloud, if briefly: typed.
        assert_eq!(
            careful(&mut dictation, DOUBTFUL),
            Some(" Thank you.".into())
        );
        // More than the phrase is not the phrase.
        assert_eq!(
            careful(&mut dictation, QUIET),
            Some(" Thank you for the flowers.".into())
        );
    }

    #[test]
    fn the_parts_of_an_answer_without_speech_are_left_out() {
        let answer = json!({"text": " We meet at ten. [Music] Thank you.", "segments": [
            {"text": " We meet at", "no_speech_prob": 0.01, "avg_logprob": -0.2},
            {"text": " ten.", "no_speech_prob": 0.01, "avg_logprob": -1.2},
            {"text": " [Music]", "no_speech_prob": 0.02, "avg_logprob": -0.3},
            {"text": " (applause)", "no_speech_prob": 0.02, "avg_logprob": -0.3},
            {"text": " Thank you.", "no_speech_prob": 0.8, "avg_logprob": -0.4},
            {"text": " li ste", "no_speech_prob": 0.0, "avg_logprob": -5.3},
        ]});
        assert_eq!(spoken(&answer, true), "We meet at ten.");
        // Where there plainly was speech, words the engine was unsure of
        // are still words.
        assert_eq!(spoken(&answer, false), "We meet at ten. li ste");
        // An answer that lists no parts is taken whole.
        assert_eq!(spoken(&json!({"text": " Yes.\n"}), true), "Yes.");
    }

    #[test]
    fn a_language_that_is_not_configured_is_never_accepted() {
        // Taken for Croatian; of the configured ones Slovenian is likelier.
        let (mut dictation, engine) = dictation(
            &["en", "sl"],
            vec![
                json!({"text": " Dobar dan.", "language": "croatian"}),
                json!({"text": " The bar done.", "language": "english",
                    "language_probabilities": {"hr": 0.5, "sl": 0.3, "en": 0.01}}),
                json!({"text": " Dober dan."}),
                json!({"text": " Hvala."}),
            ],
        );
        assert_eq!(dictation.stretch(LONG).ok(), Some("Dober dan.".into()));
        assert_eq!(asked(&engine, 1, "language").as_deref(), Some("en"));
        assert_eq!(asked(&engine, 1, "no_language_probabilities"), None);
        assert_eq!(asked(&engine, 2, "language").as_deref(), Some("sl"));
        // Thirty times likelier than English: that holds for what follows.
        assert_eq!(dictation.stretch(SHORT).ok(), Some(" Hvala.".into()));
        assert_eq!(asked(&engine, 3, "language").as_deref(), Some("sl"));
        assert_eq!(requests(&engine), 4);
    }

    #[test]
    fn the_first_configured_language_is_the_guess_and_may_be_right() {
        // Taken for Welsh; English is the likelier configured language, so
        // the second answer is the one, but not by enough to hold.
        let (mut dictation, engine) = dictation(
            &["en", "sl"],
            vec![
                json!({"text": " Diolch.", "language": "welsh"}),
                json!({"text": " The oak.", "language": "english",
                    "language_probabilities": {"cy": 0.5, "en": 0.2, "sl": 0.1}}),
                json!({"text": " Next.", "language": "english"}),
            ],
        );
        assert_eq!(dictation.stretch(LONG).ok(), Some("The oak.".into()));
        assert_eq!(requests(&engine), 2);
        assert!(dictation.stretch(LONG).is_ok());
        assert_eq!(asked(&engine, 2, "language").as_deref(), Some("auto"));
        // A language the engine does not even list is not likely at all.
        let listed = json!({"pl": 0.6});
        assert_eq!(
            likeliest(&["en".into(), "sl".into()], &listed),
            (None, false)
        );
    }

    #[test]
    fn the_language_spoken_last_is_the_better_guess() {
        let engine = Arc::new(Scripted {
            answers: Mutex::new(
                vec![
                    json!({"text": " Dobar dan.", "language": "croatian"}),
                    json!({"text": " Dober dan.", "language": "slovenian",
                        "language_probabilities": {"hr": 0.5, "sl": 0.3, "en": 0.01}}),
                ]
                .into(),
            ),
            ..Scripted::default()
        });
        let configured = vec!["en".to_owned(), "sl".to_owned()];
        let mut dictation = Dictation::new(engine.clone(), configured.clone(), Some("sl".into()));
        assert_eq!(dictation.stretch(LONG).ok(), Some("Dober dan.".into()));
        assert_eq!(asked(&engine, 1, "language").as_deref(), Some("sl"));
        assert_eq!(requests(&engine), 2);
        assert_eq!(dictation.language(), Some("sl"));
        // A language that is no longer configured is no guess.
        let dictation = Dictation::new(engine, configured, Some("de".into()));
        assert_eq!(dictation.language(), None);
    }

    #[test]
    fn without_configured_languages_any_language_is_taken() {
        let (mut dictation, engine) = dictation(
            &[],
            vec![json!({"text": " Dobar dan.", "language": "croatian"})],
        );
        assert_eq!(dictation.stretch(LONG).ok(), Some("Dobar dan.".into()));
        assert_eq!(requests(&engine), 1);
    }

    #[test]
    fn the_text_so_far_is_the_prompt_and_stretches_are_set_apart() {
        let (mut dictation, engine) = dictation(
            &["en"],
            vec![
                json!({"text": " We meet on Thursday,\n"}),
                json!({"text": " [BLANK_AUDIO]\n"}),
                json!({"text": " at ten in the morning.\n"}),
            ],
        );
        assert_eq!(
            dictation.stretch(LONG).ok(),
            Some("We meet on Thursday,".into())
        );
        // Nothing said: nothing typed, and no space left behind.
        assert_eq!(dictation.stretch(LONG).ok(), Some(String::new()));
        assert_eq!(
            dictation.stretch(LONG).ok(),
            Some(" at ten in the morning.".into())
        );
        assert_eq!(asked(&engine, 0, "prompt"), None);
        assert_eq!(
            asked(&engine, 2, "prompt").as_deref(),
            Some("We meet on Thursday,")
        );
    }

    #[test]
    fn a_language_without_spaces_is_joined_without_one() {
        let (mut dictation, _) = dictation(
            &["ja"],
            vec![
                json!({"text": "こんにちは。"}),
                json!({"text": "元気ですか。"}),
            ],
        );
        assert!(dictation.stretch(LONG).is_ok());
        assert_eq!(dictation.stretch(LONG).ok(), Some("元気ですか。".into()));
    }

    #[test]
    fn the_lines_of_an_answer_are_joined_as_they_are() {
        let broken = json!({"text": " It was moved to ten in the morning\n.\n Please bring the bud\nget.\n"});
        assert_eq!(
            spoken(&broken, false),
            "It was moved to ten in the morning. Please bring the budget."
        );
    }

    #[test]
    fn a_long_text_is_prompted_with_its_end_from_a_whole_word() {
        assert_eq!(prompt("Kratko."), "Kratko.");
        let long = format!("{} čaša šuma žaba", "beseda ".repeat(40));
        let end = prompt(&long);
        assert!(end.len() <= PROMPT_LENGTH);
        assert!(end.starts_with("beseda ") && end.ends_with("čaša šuma žaba"));
        // No spaces to start from: cut between two characters, never in one.
        let unspaced = "言".repeat(100);
        assert_eq!(prompt(&unspaced).chars().count(), PROMPT_LENGTH / 3);
    }
}
