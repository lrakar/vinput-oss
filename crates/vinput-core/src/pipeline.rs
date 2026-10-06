//! Orchestration of one dictation session.

use crate::Result;
use crate::audio::{AudioSource, SAMPLE_RATE_HZ};
use crate::inject::TextSink;
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
