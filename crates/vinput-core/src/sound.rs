//! The two sounds of a dictation: one the moment listening starts, one the
//! moment it stops.
//!
//! Both are synthesized, so there is no sound file to ship or lose: a short,
//! soft, woody click with a pitch, the one for the start a fifth above the
//! one for the stop, so that the pair falls and comes to rest.
//!
//! They answer a key, so they have to be heard as soon as a key of a
//! keyboard is: [`play`] hands the sound to a thread that keeps the way to
//! the speakers open (`sound/player.rs`) and returns at once. Nothing about
//! a session waits for a sound.
//!
//! And they are over like a click, not like a word. The microphone may well
//! hear the speakers, and the recognizer makes words up for sounds that are
//! not speech; what tells a click from a syllable there is that it does not
//! last (`stt/local/sound.rs`, `RUN`). A test below holds the sounds to that.

#[cfg(windows)]
mod player;

/// A sound that says what a key press has done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cue {
    /// Listening has started.
    Start,
    /// Listening has stopped.
    Stop,
}

/// The sample rate of [`wav`], which is the rate most speakers run at.
const WAV_RATE: u32 = 48_000;
/// How long a sound is, with the fade that ends it.
const LENGTH_SECONDS: f32 = 0.11;
/// The fade-in. Short enough to be heard at once, long enough not to crack.
const ATTACK_SECONDS: f32 = 0.0012;
/// The body of the sound is down to a third after this long.
const DECAY_SECONDS: f32 = 0.018;
/// A struck thing sounds a little sharp at first: by this much, for this long.
const STRIKE: f32 = 0.035;
const STRIKE_SECONDS: f32 = 0.006;
/// The crisp edge of the click: a high partial that is gone in a few
/// milliseconds.
const EDGE_HZ: f32 = 3_100.0;
const EDGE: f32 = 0.10;
const EDGE_SECONDS: f32 = 0.0022;
/// The linear fade at the very end, so that the sound ends at zero exactly.
const TAIL_SECONDS: f32 = 0.02;

impl Cue {
    /// The pitch of the sound and its loudest sample, as a part of full
    /// scale: G5 for the start, and C5, a little softer, for the stop.
    const fn voice(self) -> (f32, f32) {
        match self {
            Self::Start => (783.99, 0.22),
            Self::Stop => (523.25, 0.19),
        }
    }
}

/// The sound as mono samples at `rate`. Synthesized at the rate of the
/// device it is played on, it needs no conversion on the way there.
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "sample indexes are small and samples are scaled into the i16 range"
)]
pub fn samples(cue: Cue, rate: u32) -> Vec<i16> {
    use std::f32::consts::{PI, TAU};

    let (hz, peak) = cue.voice();
    let rate = rate.max(8_000) as f32;
    let count = (rate * LENGTH_SECONDS) as usize;
    let mut phase = 0.0f32;
    let mut sound = Vec::with_capacity(count);
    for index in 0..count {
        let t = index as f32 / rate;
        phase += TAU * hz * (1.0 + STRIKE * (-t / STRIKE_SECONDS).exp()) / rate;
        // The overtones give the click its wood, and are gone first.
        let body = phase.sin() * (-t / DECAY_SECONDS).exp()
            + 0.22 * (2.0 * phase).sin() * (-2.0 * t / DECAY_SECONDS).exp()
            + 0.07 * (3.0 * phase).sin() * (-3.0 * t / DECAY_SECONDS).exp();
        let edge = EDGE * (TAU * EDGE_HZ * t).sin() * (-t / EDGE_SECONDS).exp();
        let attack = 0.5 - 0.5 * (PI * (t / ATTACK_SECONDS).min(1.0)).cos();
        let tail = ((LENGTH_SECONDS - t) / TAIL_SECONDS).clamp(0.0, 1.0);
        sound.push((body + edge) * attack * tail);
    }
    let loudest = sound.iter().fold(f32::MIN_POSITIVE, |a, s| a.max(s.abs()));
    let scale = peak / loudest * f32::from(i16::MAX);
    sound.iter().map(|s| (s * scale) as i16).collect()
}

/// The sound as a complete WAV file, to listen to outside Vinput.
#[must_use]
pub fn wav(cue: Cue) -> Vec<u8> {
    let samples = samples(cue, WAV_RATE);
    let data_bytes = u32::try_from(samples.len() * 2).unwrap_or(u32::MAX);
    let mut wav = Vec::with_capacity(44 + samples.len() * 2);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&WAV_RATE.to_le_bytes());
    wav.extend_from_slice(&(WAV_RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_bytes.to_le_bytes());
    wav.extend(samples.iter().flat_map(|sample| sample.to_le_bytes()));
    wav
}

/// Plays the sound without waiting for anything. A copy that keeps to itself
/// ([`crate::instance::is_quiet`]) plays nothing.
pub fn play(cue: Cue) {
    #[cfg(windows)]
    if !crate::instance::is_quiet() {
        player::ask(player::Ask::Play(cue));
    }
    #[cfg(not(windows))]
    let _ = cue;
}

/// Makes the way to the speakers ready for a sound that is about to be
/// asked for, without playing one: a session that starts in silence still
/// has its stop to sound.
pub fn warm() {
    #[cfg(windows)]
    if !crate::instance::is_quiet() {
        player::ask(player::Ask::Warm);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CUES: [Cue; 2] = [Cue::Start, Cue::Stop];

    /// How loud each 20 ms of a sound is, as the recognizer's own measure
    /// of sound takes it in.
    fn levels(samples: &[i16], rate: u32) -> Vec<f32> {
        samples
            .chunks_exact(rate as usize / 50)
            .map(|frame| {
                let squares: f32 = frame.iter().map(|s| f32::from(*s).powi(2)).sum();
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "a frame is a few hundred samples"
                )]
                (squares / frame.len() as f32).sqrt()
            })
            .collect()
    }

    #[test]
    fn wav_header_matches_its_data() {
        for cue in CUES {
            let wav = wav(cue);
            assert_eq!(&wav[..4], b"RIFF");
            assert_eq!(&wav[8..16], b"WAVEfmt ");
            let riff_size = u32::from_le_bytes([wav[4], wav[5], wav[6], wav[7]]) as usize;
            let data_size = u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]) as usize;
            assert_eq!(riff_size + 8, wav.len());
            assert_eq!(data_size + 44, wav.len());
        }
    }

    #[test]
    fn a_sound_is_short_soft_and_ends_in_silence() {
        for cue in CUES {
            for rate in [16_000, 44_100, 48_000, 96_000, 192_000] {
                let samples = samples(cue, rate);
                assert!(samples.len() < rate as usize / 8, "an eighth of a second");
                let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
                assert!(peak > 4_000, "audible: {peak}");
                assert!(peak < 7_600, "under a quarter of full scale: {peak}");
                assert_eq!(samples.first().copied(), Some(0));
                assert!(samples.last().is_some_and(|last| last.unsigned_abs() < 8));
            }
        }
    }

    #[test]
    fn a_sound_is_there_at_once() {
        // A tenth of its loudest within half a millisecond, most of it
        // within three: nothing of the delay of a key is in the sound.
        for cue in CUES {
            let samples = samples(cue, 48_000);
            let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
            let reaches = |part: u16| samples.iter().position(|s| s.unsigned_abs() >= part);
            assert!(reaches(peak / 10).is_some_and(|at| at <= 24));
            assert!(reaches(peak / 10 * 7).is_some_and(|at| at <= 144));
        }
    }

    #[test]
    fn the_stop_is_lower_and_softer_than_the_start() {
        let (start, stop) = (Cue::Start.voice(), Cue::Stop.voice());
        assert!(stop.0 < start.0 && stop.1 < start.1);
    }

    #[test]
    fn a_sound_is_over_like_a_click_and_not_like_a_word() {
        // What the recognizer takes for sound has to hold its level for
        // 80 ms, four frames of 20 ms; that is how it tells a syllable from
        // the click of a key. Wherever the frames happen to fall on one of
        // these sounds, what it holds for that long is under an eighth of
        // its loudest frame, 18 dB down. The tone this took the place of
        // held nearly half.
        for cue in CUES {
            for offset in (0..320).step_by(40) {
                let mut heard = vec![0; offset];
                heard.extend(samples(cue, 16_000));
                heard.extend([0; 640]);
                let levels = levels(&heard, 16_000);
                let loudest = levels.iter().copied().fold(0.0, f32::max);
                let lasting = levels
                    .windows(4)
                    .map(|run| run.iter().copied().fold(f32::MAX, f32::min))
                    .fold(0.0, f32::max);
                assert!(
                    lasting < loudest * 0.125,
                    "{cue:?} {offset}: {lasting} of {loudest}"
                );
            }
        }
    }
}
