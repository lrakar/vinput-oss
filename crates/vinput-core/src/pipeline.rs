//! Orchestration of one dictation session.

use std::time::Instant;

use crate::Result;
use crate::audio::{AudioSource, SAMPLE_RATE_HZ};
use crate::inject::TextSink;
use crate::inject::live::Typist;
use crate::stt::{Transcriber, Transcript};

/// The most samples the recognizer is sent at a time: 100 ms of audio.
///
/// It is a ceiling, not the usual size. A microphone hands over what it has
/// as soon as it has it, which is one 10 ms packet at a time, and each goes
/// to the recognizer at once: about a hundred small messages a second, so
/// that no word waits here for a chunk to fill. Gathering 100 ms first would
/// put every word up to 90 ms later on the screen to save messages that cost
/// next to nothing. Only a session that has fallen behind (while typing into
/// a slow program, say) gets more per read, up to this, and so catches up.
/// Sources that are not live, such as a file, fill the whole chunk.
const CHUNK_SAMPLES: usize = (SAMPLE_RATE_HZ / 10) as usize;

/// Runs one dictation session: reads `audio` until it ends, feeds it to
/// `transcriber`, and types every final transcript into `sink`.
///
/// # Errors
///
/// Returns the first error raised by any stage.
pub fn run_session(
    audio: &mut impl AudioSource,
    transcriber: &mut impl Transcriber,
    sink: &mut impl TextSink,
) -> Result<()> {
    let mut buf = [0i16; CHUNK_SAMPLES];
    loop {
        let read = audio.read(&mut buf)?;
        if read == 0 {
            break;
        }
        if let Some(transcript) = transcriber.feed(&buf[..read])? {
            emit(&transcript, sink)?;
        }
    }
    transcriber.finish(&mut |transcript| emit(&transcript, sink))
}

/// Runs one dictation session as [`run_session`] does, and types the
/// recognizer's guesses as well, as they come: a word is on the screen when
/// it was heard and not when it was settled, and what the recognizer changes
/// its mind about is taken back and typed anew (`inject/live.rs`).
///
/// Into a sink that cannot take text back, only settled text is typed, as
/// by [`run_session`]; so it is with a recognizer that makes no guesses.
///
/// # Errors
///
/// Returns the first error raised by any stage. What was settled before it
/// has been typed by then, and the guess that was on the screen is left
/// there: words the speaker has seen appear are better kept than lost.
pub fn run_live_session(
    audio: &mut impl AudioSource,
    transcriber: &mut impl Transcriber,
    sink: &mut impl TextSink,
) -> Result<()> {
    if !sink.rewrites() {
        return run_session(audio, transcriber, sink);
    }
    let mut typist = Typist::new(sink);
    let listened = listen_live(audio, transcriber, &mut typist);
    // The key was let go, or the session has failed: from here on nothing
    // is spread out.
    let caught_up = typist.flush();
    listened.and(caught_up)?;
    transcriber.finish(&mut |transcript| {
        if transcript.is_final {
            typist.settle(&transcript.text, Instant::now());
            typist.flush()?;
        }
        Ok(())
    })?;
    typist.end()
}

/// Feeds `audio` to `transcriber` until it ends, and has `typist` type what
/// comes back as it comes.
fn listen_live<S: TextSink>(
    audio: &mut impl AudioSource,
    transcriber: &mut impl Transcriber,
    typist: &mut Typist<'_, S>,
) -> Result<()> {
    let mut buf = [0i16; CHUNK_SAMPLES];
    loop {
        let read = audio.read(&mut buf)?;
        if read == 0 {
            return Ok(());
        }
        let heard = transcriber.feed(&buf[..read])?;
        let now = Instant::now();
        if let Some(transcript) = heard.filter(|transcript| transcript.is_final) {
            typist.settle(&transcript.text, now);
        }
        if let Some(guess) = transcriber.guess() {
            typist.guess(&guess);
        }
        // A microphone is read ten milliseconds at a time, which is the
        // step the screen is brought up to date in.
        typist.step(now)?;
    }
}

fn emit(transcript: &Transcript, sink: &mut impl TextSink) -> Result<()> {
    if transcript.is_final {
        sink.type_text(&transcript.text)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;
    use crate::stt::Emit;

    /// Yields a fixed number of full chunks of silence.
    struct Silence {
        chunks_left: usize,
    }

    impl AudioSource for Silence {
        fn read(&mut self, buf: &mut [i16]) -> Result<usize> {
            if self.chunks_left == 0 {
                return Ok(0);
            }
            self.chunks_left -= 1;
            buf.fill(0);
            Ok(buf.len())
        }
    }

    /// Emits an interim transcript per chunk and a final one at the end.
    #[derive(Default)]
    struct CountingTranscriber {
        chunks: usize,
    }

    impl Transcriber for CountingTranscriber {
        fn feed(&mut self, _pcm: &[i16]) -> Result<Option<Transcript>> {
            self.chunks += 1;
            Ok(Some(Transcript {
                text: "interim".into(),
                is_final: false,
            }))
        }

        fn finish(&mut self, emit: &mut Emit<'_>) -> Result<()> {
            emit(Transcript {
                text: "revised".into(),
                is_final: false,
            })?;
            emit(Transcript {
                text: format!("{} chunks", self.chunks),
                is_final: true,
            })?;
            emit(Transcript {
                text: " and the rest".into(),
                is_final: true,
            })
        }
    }

    #[derive(Default)]
    struct Recorder {
        typed: Vec<String>,
    }

    impl TextSink for Recorder {
        fn type_text(&mut self, text: &str) -> Result<()> {
            self.typed.push(text.to_owned());
            Ok(())
        }
    }

    struct BrokenSink;

    impl TextSink for BrokenSink {
        fn type_text(&mut self, _text: &str) -> Result<()> {
            Err(Error::Injection("no focused window".into()))
        }
    }

    #[test]
    fn types_only_final_transcripts() {
        let mut sink = Recorder::default();
        let result = run_session(
            &mut Silence { chunks_left: 3 },
            &mut CountingTranscriber::default(),
            &mut sink,
        );
        assert!(result.is_ok());
        assert_eq!(sink.typed, ["3 chunks", " and the rest"]);
    }

    /// A text field, how much was ever taken back from it at once, and
    /// what was first typed into it.
    #[derive(Default)]
    struct Field {
        text: String,
        most_erased: usize,
        first: Option<String>,
        rewrites: bool,
    }

    impl TextSink for Field {
        fn type_text(&mut self, text: &str) -> Result<()> {
            self.text.push_str(text);
            Ok(())
        }

        fn rewrites(&self) -> bool {
            self.rewrites
        }

        fn rewrite(&mut self, erase: usize, text: &str) -> Result<()> {
            for _ in 0..erase {
                self.text.pop();
            }
            self.most_erased = self.most_erased.max(erase);
            self.first.get_or_insert_with(|| text.to_owned());
            self.type_text(text)
        }
    }

    /// Says what a script has it say: for each chunk of audio, the text it
    /// settles and its guess at what follows; what is left of the script
    /// when the audio ends is settled then.
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

    fn script() -> Vec<(&'static str, Option<&'static str>)> {
        vec![
            ("", Some("I")),
            ("", Some("I scream")),
            ("", Some("Ice cream is")),
            ("Ice", Some(" cream is nice")),
            ("", None),
            (" cream is", Some(" nice to")),
            // Settled when the key is let go.
            (" nice today.", None),
        ]
    }

    #[test]
    fn guesses_are_typed_as_they_come_and_the_text_ends_as_it_was_settled() {
        let mut sink = Field {
            rewrites: true,
            ..Field::default()
        };
        let result = run_live_session(
            &mut Silence { chunks_left: 6 },
            &mut Scripted::new(script()),
            &mut sink,
        );
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(sink.text, "Ice cream is nice today.");
        // The first guess was on the screen with the first chunk of audio,
        assert_eq!(sink.first.as_deref(), Some("I"));
        // and "I scream" became "Ice cream": seven characters at the most,
        // however much of it had been typed by then.
        assert!(sink.most_erased <= 7);
    }

    /// Settles some text, guesses on, and then fails.
    struct Failing(u32);

    impl Transcriber for Failing {
        fn feed(&mut self, _pcm: &[i16]) -> Result<Option<Transcript>> {
            self.0 += 1;
            match self.0 {
                1 => Ok(Some(Transcript {
                    text: "So far, so good.".into(),
                    is_final: true,
                })),
                2 => Ok(None),
                _ => Err(Error::Transcription("it broke".into())),
            }
        }

        fn finish(&mut self, _emit: &mut Emit<'_>) -> Result<()> {
            Ok(())
        }

        fn guess(&mut self) -> Option<String> {
            (self.0 == 2).then(|| " And then".to_owned())
        }
    }

    #[test]
    fn what_was_settled_and_guessed_before_a_failure_is_on_the_screen() {
        let mut sink = Field {
            rewrites: true,
            ..Field::default()
        };
        let result = run_live_session(&mut Silence { chunks_left: 9 }, &mut Failing(0), &mut sink);
        assert!(matches!(result, Err(Error::Transcription(_))), "{result:?}");
        // All of it, though the chunks came faster than text is typed.
        assert_eq!(sink.text, "So far, so good. And then");
    }

    #[test]
    fn a_sink_that_cannot_take_text_back_is_typed_settled_text_alone() {
        let mut sink = Field::default();
        let result = run_live_session(
            &mut Silence { chunks_left: 6 },
            &mut Scripted::new(script()),
            &mut sink,
        );
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(sink.text, "Ice cream is nice today.");
        assert_eq!(sink.most_erased, 0);
    }

    #[test]
    fn propagates_sink_errors() {
        let result = run_session(
            &mut Silence { chunks_left: 1 },
            &mut CountingTranscriber::default(),
            &mut BrokenSink,
        );
        assert!(matches!(result, Err(Error::Injection(_))));
    }
}
