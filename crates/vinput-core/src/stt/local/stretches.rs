//! The stretches of one dictation on their way through the engine.
//!
//! Recognizing takes the engine a moment, and the thread that captures the
//! microphone and types must not wait for it. So a thread of its own takes
//! the stretches one after the other, which is also all the engine can do,
//! and hands back the text of each in the order they were spoken.
//!
//! Cutting only pays with an engine that is done with a stretch before the
//! speaker has said much more. A slow one, a large model on the processor,
//! would still be busy with the stretch before when the key is let go, and
//! the user would wait for two requests where there used to be one. So the
//! thread times the engine ([`Experience`]), and a slow engine is handed the
//! dictation whole, as it always was.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use super::dictation::{Care, Dictation};
use crate::audio::SAMPLE_RATE_HZ;
use crate::{Error, Result};

/// An engine that takes longer than this for a stretch is not handed
/// stretches while the user speaks. A stretch is cut off 0.6 s into a pause,
/// and whoever speaks on after it needs about a second for a few more words,
/// so an engine this quick is free again by the time the key is let go.
const KEEPS_UP: Duration = Duration::from_millis(1500);
/// The engine works through a recording half a minute at a time, so only a
/// stretch shorter than that says how long it takes for one.
const TIMED_SAMPLES: usize = 30 * SAMPLE_RATE_HZ as usize;

/// What one dictation leaves for the next: how long the engine took for the
/// last stretch, which language that was in, and how loud the speaker is.
#[derive(Debug, Default)]
pub(super) struct Experience {
    /// The level at which the user speaks into this microphone, as far as
    /// the stretches that had speech in them show; zero until one has.
    voice: AtomicU32,
    /// In milliseconds. Every dictation times the engine again, at its end
    /// if nowhere else, so an engine that was slow once, while it was
    /// loading, is not held to it.
    took: AtomicU32,
    language: Mutex<Option<String>>,
}

impl Experience {
    /// Whether the engine is quick enough to be handed stretches while the
    /// user speaks. It is, until it has shown otherwise.
    pub(super) fn keeps_up(&self) -> bool {
        Duration::from_millis(self.took.load(Ordering::Relaxed).into()) <= KEEPS_UP
    }

    pub(super) fn took(&self, time: Duration) {
        let millis = u32::try_from(time.as_millis()).unwrap_or(u32::MAX);
        self.took.store(millis, Ordering::Relaxed);
    }

    /// The language of the last stretch that was recognized, which is the
    /// best guess at the language of the next dictation.
    pub(super) fn language(&self) -> Option<String> {
        self.language
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The level of the user's voice in the dictations so far, once there
    /// has been one.
    pub(super) fn voice(&self) -> Option<f32> {
        let voice = self.voice.load(Ordering::Relaxed);
        #[expect(clippy::cast_precision_loss, reason = "a level is under 32768")]
        (voice > 0).then_some(voice as f32)
    }

    /// Notes that speech was recognized in a stretch whose loudest was
    /// `level`. The level kept follows the speaker by a quarter each time,
    /// so one shout or one whisper does not become the measure.
    fn voiced(&self, level: f32) {
        let before = self.voice().unwrap_or(level);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a level is between 0 and 32768"
        )]
        let voice = (before + (level - before) / 4.0).round().max(1.0) as u32;
        self.voice.store(voice, Ordering::Relaxed);
    }

    fn spoke(&self, language: &str) {
        let mut last = self.language.lock().unwrap_or_else(PoisonError::into_inner);
        if last.as_deref() != Some(language) {
            *last = Some(language.to_owned());
        }
    }
}

/// The line to the thread that recognizes a dictation.
pub(super) struct Stretches {
    /// Each stretch, the care it is to be recognized with, and its level.
    spoken: Sender<(Vec<i16>, Care, f32)>,
    texts: Receiver<Result<String>>,
    /// How many stretches have been sent whose text has not come back.
    outstanding: usize,
}

impl Stretches {
    /// Starts the thread that recognizes the stretches of `dictation` and
    /// notes what it learns in `experience`. It ends when this is dropped,
    /// after the stretch it is busy with.
    pub(super) fn start(mut dictation: Dictation, experience: Arc<Experience>) -> Result<Self> {
        let (spoken, waiting) = mpsc::channel::<(Vec<i16>, Care, f32)>();
        let (answer, texts) = mpsc::channel();
        thread::Builder::new()
            .name("vinput-local-stt".into())
            .spawn(move || {
                for (stretch, care, level) in waiting {
                    let started = Instant::now();
                    let text = dictation.recognize(&stretch, care);
                    if text.is_ok() && stretch.len() < TIMED_SAMPLES {
                        experience.took(started.elapsed());
                    }
                    if let Some(language) = dictation.language() {
                        experience.spoke(language);
                    }
                    // Only what plainly was speech says how loud speech is.
                    if !care.doubtful && text.as_ref().is_ok_and(|text| !text.is_empty()) {
                        experience.voiced(level);
                    }
                    // After a failure the text has a hole in it; the
                    // dictation ends there instead of going on around it.
                    let failed = text.is_err();
                    if answer.send(text).is_err() || failed {
                        return;
                    }
                }
            })
            .map_err(|err| {
                Error::Transcription(format!("starting the recognition thread failed: {err}"))
            })?;
        Ok(Self {
            spoken,
            texts,
            outstanding: 0,
        })
    }

    /// Whether the engine has nothing to do.
    pub(super) fn idle(&self) -> bool {
        self.outstanding == 0
    }

    /// Queues a stretch for recognition with the care it calls for.
    /// `level` is how loud it is.
    pub(super) fn send(&mut self, stretch: Vec<i16>, care: Care, level: f32) {
        // Counted even if the thread has stopped, so that the text is still
        // waited for and the reason it stopped comes out there.
        self.outstanding += 1;
        let _ = self.spoken.send((stretch, care, level));
    }

    /// The text of the next stretch if it has been recognized by now.
    pub(super) fn ready(&mut self) -> Result<Option<String>> {
        if self.idle() {
            return Ok(None);
        }
        match self.texts.try_recv() {
            Ok(text) => self.received(text),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(stopped()),
        }
    }

    /// Waits for the text of the next stretch. `None` once every stretch
    /// that was sent has been answered.
    pub(super) fn wait(&mut self) -> Result<Option<String>> {
        if self.idle() {
            return Ok(None);
        }
        let text = self.texts.recv().map_err(|_| stopped())?;
        self.received(text)
    }

    fn received(&mut self, text: Result<String>) -> Result<Option<String>> {
        self.outstanding -= 1;
        text.map(Some)
    }
}

fn stopped() -> Error {
    Error::Transcription("the local engine failed: recognition stopped early".into())
}
