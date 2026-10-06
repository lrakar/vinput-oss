//! Where a dictation can be cut while it is still being spoken: at a pause.
//!
//! A pause is 0.6 s in which nothing comes near the level of the speaker's
//! voice. That level is measured from the dictation itself, so a quiet voice
//! far from the microphone and a loud one close to it are cut alike. A room
//! so noisy that its noise comes near the voice has no pauses by this
//! measure, and a dictation in it is not cut at all, which is what happened
//! to every dictation before there was any cutting.
//!
//! The level of the voice decides only where a dictation is cut, never what
//! is recognized: a cut hands every sample to one side or the other. No audio
//! that might be speech is left out. What is dropped is audio after a cut in
//! which nothing lasting comes within 26 dB of the voice: the room, a breath,
//! the click of the key. The engine makes words up for that, "Thank you."
//! above all, and it used to be typed after the last sentence.

use super::sound::{FRAME, FRAMES_PER_SECOND, HUSHED_PART, SILENCE, VOICE_PART, level, sustained};

/// This long without speech is a pause.
const PAUSE_FRAMES: usize = 6 * FRAMES_PER_SECOND / 10;
/// A stretch is cut off only when this much time lies between its first
/// sound and the pause, so the engine is not handed fragments: it recognizes
/// a few words alone worse than a sentence, and is least sure of their
/// language.
const MIN_SPEECH_FRAMES: usize = 2 * FRAMES_PER_SECOND;
/// What is left at the end with less sound in it than this is more likely a
/// breath or the click of the key than a sentence; see [`Rest::faint`].
const FAINT_FRAMES: usize = FRAMES_PER_SECOND;
/// What is left at the end with less of the voice in it than this was not
/// said aloud; see [`Rest::quiet`].
const QUIET_FRAMES: usize = FRAMES_PER_SECOND / 5;
/// The silence kept before the first word of a stretch that follows a cut.
const LEAD_FRAMES: usize = FRAMES_PER_SECOND / 5;
/// How much of the level of the voice is left a frame later, so that one
/// loud moment does not set the bar for the rest of the dictation, and a
/// speaker who goes on more softly is followed: half of it is gone after
/// five seconds.
const VOICE_DECAY: f32 = 0.997_23;

/// What is left of a dictation when it ends.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Rest {
    pub(super) samples: Vec<i16>,
    /// Whether this follows a cut and has little sound in it. It is still
    /// recognized, but with the care a breath after the last sentence needs.
    pub(super) faint: bool,
    /// Whether this follows a cut and has next to nothing in it that is as
    /// loud as the voice was. A phrase the engine is known to make up is not
    /// taken from it.
    pub(super) quiet: bool,
}

/// Collects the audio of a dictation and hands it out a stretch at a time.
#[derive(Debug, Default)]
pub(super) struct Pauses {
    /// The audio of the stretch being spoken.
    stretch: Vec<i16>,
    /// The level of each whole frame of `stretch`.
    levels: Vec<f32>,
    /// How many frames at the end of `levels` have been quiet.
    quiet: usize,
    /// The level of the voice: the loudest of the recent frames, smoothed
    /// so that a click does not count.
    voice: f32,
    smooth: f32,
    /// Whether a stretch has been handed out.
    cut: bool,
}

impl Pauses {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Takes in audio. Returns a finished stretch if the speaker has paused
    /// and `may_cut` allows it; the audio after it stays for the next one.
    pub(super) fn feed(&mut self, pcm: &[i16], may_cut: bool) -> Option<Vec<i16>> {
        self.stretch.extend_from_slice(pcm);
        let mut measured = self.levels.len() * FRAME;
        while let Some(frame) = self.stretch.get(measured..measured + FRAME) {
            let level = level(frame);
            measured += FRAME;
            self.levels.push(level);
            if level < self.threshold() {
                self.quiet += 1;
            } else {
                self.quiet = 0;
            }
            // A click is over in a frame or two; a voice is not.
            self.smooth += (level - self.smooth) * 0.2;
            self.voice = (self.voice * VOICE_DECAY).max(self.smooth);
        }
        if self.quiet < PAUSE_FRAMES {
            return None;
        }
        let speech = self.speech();
        if may_cut && speech >= MIN_SPEECH_FRAMES {
            return Some(self.cut_off());
        }
        if self.cut && self.levels.len() > 2 * LEAD_FRAMES && self.silent() {
            // The microphone has heard nothing at all since the last cut:
            // that silence is not kept.
            let surplus = self.levels.len() - LEAD_FRAMES;
            self.levels.drain(..surplus);
            self.stretch.drain(..surplus * FRAME);
        }
        None
    }

    /// What is left when the dictation ends: all of it, unless it follows a
    /// cut and nothing in it was said, however softly.
    pub(super) fn rest(&mut self) -> Option<Rest> {
        let heard = !self.cut || self.said();
        let sound = self.sound();
        let faint = self.cut && sound < FAINT_FRAMES;
        let quiet = self.cut && sound < QUIET_FRAMES;
        let samples = std::mem::take(&mut self.stretch);
        *self = Self::new();
        heard.then_some(Rest {
            samples,
            faint,
            quiet,
        })
    }

    /// Whether anything in the stretch lasts as a word does and comes
    /// within [`HUSHED_PART`] of the level of the voice. The room does not,
    /// and neither does the click of the key, which is loud but over at once.
    fn said(&self) -> bool {
        sustained(&self.levels) >= SILENCE.max(self.voice * HUSHED_PART)
    }

    /// Whether every frame of the stretch is under [`SILENCE`]. This, and
    /// never the level of the voice, is what audio is dropped by while the
    /// dictation goes on.
    fn silent(&self) -> bool {
        self.levels.iter().all(|level| *level < SILENCE)
    }

    /// Under this level a frame is part of a pause.
    fn threshold(&self) -> f32 {
        SILENCE.max(self.voice * VOICE_PART)
    }

    /// How many frames of the stretch are not quiet, by what is known of the
    /// voice now. Judging them afresh keeps the noise of the room before
    /// the first word from passing for speech.
    fn sound(&self) -> usize {
        let threshold = self.threshold();
        self.levels
            .iter()
            .filter(|level| **level >= threshold)
            .count()
    }

    /// How many frames lie between the first sound of the stretch and the
    /// pause it ends with.
    fn speech(&self) -> usize {
        let threshold = self.threshold();
        let spoken = self.levels.len().saturating_sub(self.quiet);
        self.levels
            .iter()
            .position(|level| *level >= threshold)
            .map_or(0, |first| spoken.saturating_sub(first))
    }

    /// Cuts the stretch off in the pause it ends with. The next one starts
    /// with the last [`LEAD_FRAMES`] of the pause.
    fn cut_off(&mut self) -> Vec<i16> {
        let next = self.levels.len().saturating_sub(LEAD_FRAMES);
        self.levels.drain(..next);
        let rest = self.stretch.split_off(next * FRAME);
        self.cut = true;
        std::mem::replace(&mut self.stretch, rest)
    }
}

/// Made-up audio for tests: a voice and a room, as noise of a given level.
#[cfg(test)]
pub(super) mod testing {
    use crate::audio::SAMPLE_RATE_HZ;

    /// The microphone hands audio over 10 ms at a time.
    pub(in super::super) const PACKET: usize = SAMPLE_RATE_HZ as usize / 100;

    /// `millis` of noise that swings between `-level` and `level`. The same
    /// every time, so a test that passes once passes always.
    pub(in super::super) fn noise(millis: usize, level: i16) -> Vec<i16> {
        let mut state = 0x2545_f491_u32;
        (0..millis * SAMPLE_RATE_HZ as usize / 1000)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let unit = f32::from(i16::try_from(state >> 17).unwrap_or(0)) / 16_384.0 - 1.0;
                #[expect(clippy::cast_possible_truncation, reason = "within the level")]
                let sample = (unit * f32::from(level)) as i16;
                sample
            })
            .collect()
    }

    /// Someone speaking for `millis`.
    pub(in super::super) fn voice(millis: usize) -> Vec<i16> {
        noise(millis, 6000)
    }

    /// The room on its own for `millis`.
    pub(in super::super) fn room(millis: usize) -> Vec<i16> {
        noise(millis, 60)
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{PACKET, noise, room, voice};
    use super::*;
    use crate::audio::SAMPLE_RATE_HZ;

    /// Feeds `audio` the way the microphone does and returns the stretches
    /// that were cut off, each with the place in the audio where it happened.
    fn feed(pauses: &mut Pauses, audio: &[i16], may_cut: bool) -> Vec<(usize, Vec<i16>)> {
        let mut stretches = Vec::new();
        for (index, packet) in audio.chunks(PACKET).enumerate() {
            if let Some(stretch) = pauses.feed(packet, may_cut) {
                stretches.push(((index + 1) * PACKET, stretch));
            }
        }
        stretches
    }

    /// Runs a whole dictation through. Returns how many stretches were cut
    /// off, whether what was left counts as faint, and everything that would
    /// be sent to the engine, on end.
    fn dictate(audio: &[i16]) -> (usize, bool, Vec<i16>) {
        let mut pauses = Pauses::new();
        let stretches = feed(&mut pauses, audio, true);
        let cuts = stretches.len();
        let mut sent: Vec<i16> = stretches
            .into_iter()
            .flat_map(|(_, stretch)| stretch)
            .collect();
        let rest = pauses.rest();
        let faint = rest.as_ref().is_some_and(|rest| rest.faint);
        sent.extend(rest.map(|rest| rest.samples).unwrap_or_default());
        (cuts, faint, sent)
    }

    const SECOND: usize = SAMPLE_RATE_HZ as usize;

    #[test]
    fn a_dictation_without_a_pause_stays_whole() {
        let audio = [room(300), voice(5000), room(400)].concat();
        let (cuts, faint, sent) = dictate(&audio);
        assert_eq!((cuts, faint), (0, false));
        assert!(sent == audio);
    }

    #[test]
    fn a_pause_ends_a_stretch_and_no_sample_is_lost() {
        let audio = [room(300), voice(3000), room(1500), voice(2500)].concat();
        let mut pauses = Pauses::new();
        let mut stretches = feed(&mut pauses, &audio, true);
        assert_eq!(stretches.len(), 1);
        let (at, first) = stretches.pop().unwrap_or_default();
        // Cut 0.6 s into the pause, and the last 0.2 s of it start the next.
        assert_eq!(at, SECOND * 39 / 10);
        assert_eq!(first.len(), SECOND * 37 / 10);
        let rest = pauses.rest().map(|rest| rest.samples).unwrap_or_default();
        assert!([first, rest].concat() == audio);
    }

    #[test]
    fn a_breath_or_a_few_words_do_not_end_a_stretch() {
        // A pause too short to be one.
        let audio = [voice(3000), room(500), voice(3000)].concat();
        assert_eq!(dictate(&audio).0, 0);
        // Too little said before the pause; the room before the first word
        // does not count as speech.
        let audio = [room(3000), voice(1500), room(2000), voice(1000)].concat();
        let (cuts, _, sent) = dictate(&audio);
        assert_eq!(cuts, 0);
        assert!(sent == audio);
    }

    #[test]
    fn a_stretch_waits_for_the_engine_to_be_free() {
        let mut pauses = Pauses::new();
        let spoken = [voice(3000), room(2000)].concat();
        assert!(feed(&mut pauses, &spoken, false).is_empty());
        // Still in the same pause when the engine is free: cut at once.
        let stretches = feed(&mut pauses, &room(100), true);
        assert_eq!(stretches.len(), 1);
        // Speech has gone on by the time the engine is free: no cut.
        let mut pauses = Pauses::new();
        let spoken = [voice(3000), room(2000), voice(200)].concat();
        assert!(feed(&mut pauses, &spoken, false).is_empty());
        assert!(feed(&mut pauses, &voice(500), true).is_empty());
    }

    #[test]
    fn only_a_microphone_that_hears_nothing_is_left_out() {
        // Dead silence after the last sentence: nothing more to recognize.
        let nothing = vec![0i16; 4 * SECOND];
        let audio = [voice(3000), nothing.clone()].concat();
        let mut pauses = Pauses::new();
        assert_eq!(feed(&mut pauses, &audio, true).len(), 1);
        assert!(pauses.rest().is_none());
        // Dead silence between two sentences is cut down to its end, and the
        // second sentence is whole.
        let audio = [voice(3000), nothing, voice(2000)].concat();
        let (cuts, faint, sent) = dictate(&audio);
        assert_eq!((cuts, faint), (1, false));
        assert!(sent.len() < audio.len() - 3 * SECOND);
        assert!(sent.ends_with(&audio[audio.len() - 2 * SECOND - LEAD_FRAMES * FRAME..]));
    }

    #[test]
    fn the_room_and_the_click_of_the_key_are_not_a_last_stretch() {
        // The room after the last sentence, and the key let go: nothing was
        // said, so there is nothing for the engine to make words up for.
        let click = noise(40, 6000);
        let audio = [voice(3000), room(4000), click, room(300)].concat();
        let mut pauses = Pauses::new();
        assert_eq!(feed(&mut pauses, &audio, true).len(), 1);
        assert!(pauses.rest().is_none());
        // A noisier room, a tenth as loud as the voice was, might hold a
        // whisper. It is sent, marked as having next to nothing in it.
        let audio = [voice(3000), noise(2000, 600)].concat();
        let mut pauses = Pauses::new();
        assert_eq!(feed(&mut pauses, &audio, true).len(), 1);
        let rest = pauses.rest();
        assert!(rest.is_some_and(|rest| rest.faint && rest.quiet));
        // Without a cut there is no voice to measure by: all of it is left.
        let mut pauses = Pauses::new();
        assert!(feed(&mut pauses, &room(2000), true).is_empty());
        let rest = pauses.rest();
        assert!(rest.is_some_and(|rest| !rest.faint && !rest.quiet));
    }

    #[test]
    fn what_is_said_after_a_cut_is_never_dropped_for_being_soft() {
        // A cough far louder than the voice, and then speech at two thirds
        // of the level before.
        let cough = noise(300, 30_000);
        let softer = noise(8000, 4000);
        let audio = [voice(3000), room(1000), cough, softer].concat();
        let (cuts, faint, sent) = dictate(&audio);
        assert_eq!((cuts, faint), (1, false));
        assert!(sent == audio);
        // Eight seconds spoken some 17 dB more softly than the first three.
        let audio = [voice(3000), room(1000), noise(8000, 850)].concat();
        let (cuts, _, sent) = dictate(&audio);
        assert!(cuts >= 1);
        assert!(sent == audio);
        // One short soft word before the key is let go.
        let audio = [voice(3000), room(1000), noise(100, 850), room(200)].concat();
        let (cuts, faint, sent) = dictate(&audio);
        assert_eq!((cuts, faint), (1, true));
        assert!(sent == audio);
        // A soft afterthought of a second and a half.
        let audio = [voice(3000), room(1000), noise(1500, 700), room(200)].concat();
        let (cuts, _, sent) = dictate(&audio);
        assert_eq!(cuts, 1);
        assert!(sent == audio);
    }

    #[test]
    fn the_bar_comes_down_when_the_voice_does() {
        // Half a minute on, a voice a tenth as loud as at the start has
        // pauses of its own again.
        let soft = [noise(3000, 600), noise(1000, 20), noise(3000, 600)].concat();
        let audio = [voice(3000), room(30_000), soft].concat();
        assert_eq!(dictate(&audio).0, 2);
    }

    #[test]
    fn a_quiet_voice_is_cut_like_a_loud_one_and_a_noisy_room_not_at_all() {
        let quiet = [noise(3000, 300), noise(1000, 15), noise(1000, 300)].concat();
        assert_eq!(dictate(&quiet).0, 1);
        let loud = [noise(3000, 20_000), noise(1000, 1000), noise(1000, 20_000)].concat();
        assert_eq!(dictate(&loud).0, 1);
        // The room is a quarter as loud as the voice: no telling a pause.
        let noisy = [noise(3000, 6000), noise(2000, 1500), noise(1000, 6000)].concat();
        assert_eq!(dictate(&noisy).0, 0);
    }
}
