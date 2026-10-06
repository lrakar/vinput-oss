//! What of a stretch is worth the engine's time: the part with sound in it.
//!
//! The engine makes words up for audio without speech. Handed the room and
//! the click of the key that follow the last sentence it answers "Thank you."
//! as readily as with nothing, so what is quiet at either end of a stretch is
//! not sent, a stretch with no sound in it is not sent at all, and one with
//! so little that it may not be speech is marked for the care
//! [`super::dictation`] takes with it.
//!
//! Sound is judged against the loudest of the stretch itself, never against
//! a fixed level, so a soft voice far from the microphone is kept like a loud
//! one close to it. And it has to last: the click of a key is over in a frame
//! or two, a syllable is not.

use std::ops::Range;

use crate::audio::SAMPLE_RATE_HZ;

/// The level is measured over this many samples at a time: 20 ms.
pub(super) const FRAME: usize = SAMPLE_RATE_HZ as usize / 50;
pub(super) const FRAMES_PER_SECOND: usize = SAMPLE_RATE_HZ as usize / FRAME;
/// A level under this is silence whatever the voice: about -68 dB of full
/// scale, under the hiss of any microphone that is switched on.
pub(super) const SILENCE: f32 = 12.0;
/// A frame counts as speech when it reaches this part of the level of the
/// voice: some 16 dB under it, where the gaps between words are but no
/// syllable is, short of a whisper.
pub(super) const VOICE_PART: f32 = 0.15;
/// What does not reach this part of the level of the voice for [`RUN`]
/// frames on end is not speech, however softly spoken: 26 dB under it.
pub(super) const HUSHED_PART: f32 = 0.05;
/// Sound has to last this many frames to count: 80 ms, which the shortest
/// word does and the click of a key does not.
pub(super) const RUN: usize = 4;
/// The quiet kept before the first sound of a stretch and after its last.
/// The engine recognizes a word that starts or ends on the edge of a
/// recording worse than one with a moment around it.
const LEAD_FRAMES: usize = FRAMES_PER_SECOND / 5;
const TAIL_FRAMES: usize = 3 * FRAMES_PER_SECOND / 10;
/// A stretch with less sound in it than this may be a word, or a tap on the
/// key and the noise of it.
const SHORT_FRAMES: usize = FRAMES_PER_SECOND / 2;
/// A stretch whose loudest moment is under this is more likely a room than
/// a voice: about -50 dB of full scale.
const HUSH: f32 = 100.0;
/// A stretch whose loudest moment is less than this many times its quietest
/// tenth is one even noise, a fan or a hum, and not the ups and downs of
/// speech.
const EVEN: f32 = 3.0;

/// What there is to hear in a stretch.
#[derive(Debug, PartialEq)]
pub(super) struct Sound {
    /// The samples from a moment before the first sound to a moment after
    /// the last.
    pub(super) heard: Range<usize>,
    /// Whether it may well not be speech: short, soft or even.
    pub(super) doubtful: bool,
    /// Whether nothing in it is as loud as a voice near a microphone.
    pub(super) hushed: bool,
    /// The loudest level in it that lasts as a word does.
    pub(super) loudest: f32,
}

impl Sound {
    /// Whether nothing in the stretch comes near `voice`, the level at which
    /// the user was heard to speak before. Then it is the room that was
    /// recorded, as when the key is pressed by mistake.
    pub(super) fn far_under(&self, voice: f32) -> bool {
        self.loudest < voice * VOICE_PART
    }
}

/// Listens to a stretch. `None` if there is nothing in it but silence.
pub(super) fn listen(samples: &[i16]) -> Option<Sound> {
    let levels = levels(samples);
    let loudest = sustained(&levels);
    if loudest < SILENCE {
        return None;
    }
    let edge = SILENCE.max(loudest * HUSHED_PART);
    let mut runs = levels
        .windows(RUN)
        .map(|run| run.iter().all(|level| *level >= edge));
    let first = runs.position(|sound| sound)?;
    let last = runs
        .rposition(|sound| sound)
        .map_or(first, |last| first + 1 + last);
    let end = last + RUN + TAIL_FRAMES;
    let heard = first.saturating_sub(LEAD_FRAMES) * FRAME..if end >= levels.len() {
        // To the last sample, which need not be the end of a frame.
        samples.len()
    } else {
        end * FRAME
    };

    let bar = SILENCE.max(loudest * VOICE_PART);
    let sound = levels.iter().filter(|level| **level >= bar).count();
    let mut sorted = levels;
    sorted.sort_by(f32::total_cmp);
    let floor = sorted.get(sorted.len() / 10).copied().unwrap_or_default();
    Some(Sound {
        heard,
        doubtful: sound < SHORT_FRAMES || loudest < HUSH || loudest < EVEN * floor,
        hushed: loudest < HUSH,
        loudest,
    })
}

/// The level of each whole frame of `samples`.
pub(super) fn levels(samples: &[i16]) -> Vec<f32> {
    samples.chunks_exact(FRAME).map(level).collect()
}

/// The loudest level that is kept up for [`RUN`] frames on end.
pub(super) fn sustained(levels: &[f32]) -> f32 {
    levels
        .windows(RUN)
        .map(|run| run.iter().copied().fold(f32::INFINITY, f32::min))
        .fold(0.0, f32::max)
}

/// How loud a frame is: the root of the mean square of its samples.
pub(super) fn level(frame: &[i16]) -> f32 {
    let energy: f32 = frame
        .iter()
        .map(|&sample| f32::from(sample) * f32::from(sample))
        .sum();
    #[expect(
        clippy::cast_precision_loss,
        reason = "a frame has a few hundred samples"
    )]
    let mean = energy / frame.len().max(1) as f32;
    mean.sqrt()
}

#[cfg(test)]
mod tests {
    use super::super::pauses::testing::{noise, room, voice};
    use super::*;

    const SECOND: usize = SAMPLE_RATE_HZ as usize;

    /// A room with a noise that comes and goes, at some 230 at its loudest.
    fn loud_enough() -> Vec<i16> {
        [noise(500, 100), noise(700, 400), noise(500, 100)].concat()
    }

    #[test]
    fn the_quiet_at_either_end_is_not_sent() {
        let audio = [room(2000), voice(3000), room(4000)].concat();
        let sound = listen(&audio).unwrap_or_else(|| unreachable!());
        // A fifth of a second before the voice, three tenths after it.
        assert_eq!(sound.heard, SECOND * 18 / 10..SECOND * 53 / 10);
        assert!(!sound.doubtful && !sound.hushed);
        // Nothing to leave out: all of it, to the last sample.
        let audio = [room(100), voice(3000), room(210), vec![0; 7]].concat();
        assert_eq!(
            listen(&audio).map(|sound| sound.heard),
            Some(0..audio.len())
        );
    }

    #[test]
    fn the_click_of_the_key_is_not_what_a_stretch_ends_with() {
        let click = noise(40, 6000);
        let audio = [room(200), voice(3000), room(3000), click, room(100)].concat();
        let sound = listen(&audio).unwrap_or_else(|| unreachable!());
        assert_eq!(sound.heard.end, SECOND * 35 / 10);
    }

    #[test]
    fn a_soft_word_at_the_end_is_kept() {
        // A seventh as loud as the sentence before it.
        let audio = [voice(3000), room(1000), noise(100, 850), room(1000)].concat();
        let sound = listen(&audio).unwrap_or_else(|| unreachable!());
        assert_eq!(sound.heard, 0..SECOND * 44 / 10);
    }

    #[test]
    fn silence_is_nothing_and_a_room_or_a_tap_is_doubtful() {
        assert_eq!(listen(&vec![0; 3 * SECOND]), None);
        assert_eq!(listen(&[]), None);
        // The room alone: soft and even.
        let sound = listen(&room(3000)).unwrap_or_else(|| unreachable!());
        assert!(sound.doubtful && sound.hushed);
        // A fan close by: loud, but even.
        let sound = listen(&noise(3000, 2000)).unwrap_or_else(|| unreachable!());
        assert!(sound.doubtful && !sound.hushed);
        // A tap on the key: a third of a second of anything.
        let tap = [room(300), voice(300), room(300)].concat();
        let sound = listen(&tap).unwrap_or_else(|| unreachable!());
        assert!(sound.doubtful && !sound.hushed);
        // Next to the voice of the dictations before, a room is a room
        // however uneven its noise.
        let sound = listen(&loud_enough()).unwrap_or_else(|| unreachable!());
        assert!(sound.far_under(3000.0) && !sound.far_under(1000.0));
        // A sentence is not doubtful, whether spoken loudly or softly.
        let loud = [room(300), voice(2000), room(300)].concat();
        assert_eq!(listen(&loud).map(|sound| sound.doubtful), Some(false));
        let soft = [noise(300, 15), noise(2000, 300), noise(300, 15)].concat();
        assert_eq!(listen(&soft).map(|sound| sound.doubtful), Some(false));
    }
}
