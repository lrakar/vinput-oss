//! Ending a dictation that nobody is speaking into.
//!
//! A dictation started with a tap listens until it is tapped again. One
//! that is forgotten would listen for good: the microphone stays on, and an
//! online service is sent the room for as long, which it counts like speech.
//! So a source wrapped in [`UntilQuiet`] ends by itself once a time has
//! passed without a sound, and whoever wants to go on presses the key again.
//! The time is the user's to choose ([`crate::settings::STOP_AFTER_MINUTES`])
//! and half an hour unless they do, because someone who dictates may well
//! stop to read or to think for minutes, and must find the dictation as
//! they left it.
//!
//! What a sound is, is judged with a wide margin on the side of going on.
//! Ending a dictation someone is still speaking into would lose words;
//! listening a while longer to a room that is not quite still loses nothing.
//! A level counts as sound when it stands clear of the quietest the
//! microphone has lately heard, which is the room, so a fan or a hum is
//! quiet and a soft voice over it is not. Time is counted in samples read,
//! not on a clock, so a session that falls behind is not ended for it.

use super::{AudioSource, SAMPLE_RATE_HZ, StopHandle};
use crate::Result;

/// The level is measured over this many samples at a time: 20 ms.
const FRAME: usize = SAMPLE_RATE_HZ as usize / 50;
const FRAMES_PER_SECOND: usize = SAMPLE_RATE_HZ as usize / FRAME;
const FRAMES_PER_MINUTE: usize = 60 * FRAMES_PER_SECOND;
/// A level under this is silence whatever the room: about -68 dB of full
/// scale, under the hiss of any microphone that is switched on.
const SILENCE: f32 = 12.0;
/// A frame is a sound when it is this many times as loud as the room: 3.5 dB
/// over it. The level of an even noise stays within a tenth of itself from
/// one frame to the next, and speech that close to the noise around it is
/// not recognized by anything.
const CLEAR: f32 = 1.5;
/// Sound has to last this many frames to count: 80 ms, which the shortest
/// word does and the click of a key or a mouse does not.
const RUN: usize = 4;
/// How much of the way to a louder level the room is taken to have gone
/// with each frame. It follows a quieter one at once, so the level of the
/// room is found in the gaps between words, and a room that has become
/// louder for good is known as such within some ten seconds.
const CREEP: f32 = 0.002;

/// Listens for sound and counts how long there has been none.
#[derive(Debug)]
struct Watch {
    /// The sum of the squares of the samples of the frame being filled.
    energy: u64,
    filled: usize,
    /// The level of the room; none before the first frame.
    room: Option<f32>,
    /// How many frames on end were louder than the room.
    run: usize,
    /// Frames since the last sound.
    quiet: usize,
    /// How many of those there may be.
    limit: usize,
}

impl Watch {
    /// A watch that allows `limit` frames without a sound.
    const fn new(limit: usize) -> Self {
        Self {
            energy: 0,
            filled: 0,
            room: None,
            run: 0,
            quiet: 0,
            limit,
        }
    }

    /// Takes in what was read. Says whether nothing has been heard for as
    /// long as the watch allows.
    fn hear(&mut self, samples: &[i16]) -> bool {
        for &sample in samples {
            let sample = u64::from(sample.unsigned_abs());
            self.energy += sample * sample;
            self.filled += 1;
            if self.filled == FRAME {
                // A frame of full scale is 2^39: exact as an `f32` would
                // not be, and nothing here needs it to be.
                #[allow(clippy::cast_precision_loss)]
                let level = (self.energy as f32 / FRAME as f32).sqrt();
                self.energy = 0;
                self.filled = 0;
                self.frame(level);
            }
        }
        self.quiet >= self.limit
    }

    fn frame(&mut self, level: f32) {
        let room = match self.room {
            Some(room) if level > room => room + (level - room) * CREEP,
            _ => level,
        };
        self.room = Some(room);
        self.run = if level > SILENCE.max(room * CLEAR) {
            self.run + 1
        } else {
            0
        };
        self.quiet = if self.run >= RUN { 0 } else { self.quiet + 1 };
    }
}

/// Wraps a source and ends it when nothing has been heard from it for a
/// number of minutes: `stop`, which ends the source, is then triggered, and
/// the source goes on being read, so that it hands on what it still holds
/// and lets go of its device at once. `ended` is called once, when the
/// source has ended that way, before the read that says so returns.
///
/// While `held` says that the key is being held down nothing is counted:
/// whoever holds a key is there.
pub struct UntilQuiet<S, H, F> {
    source: S,
    watch: Watch,
    stop: StopHandle,
    held: H,
    /// Whether it was this that triggered `stop`.
    fired: bool,
    ended: Option<F>,
}

impl<S, H, F> std::fmt::Debug for UntilQuiet<S, H, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UntilQuiet")
            .field("watch", &self.watch)
            .finish_non_exhaustive()
    }
}

impl<S, H: Fn() -> bool, F: FnOnce()> UntilQuiet<S, H, F> {
    /// Ends `source`, which `stop` ends, after `minutes` without a sound
    /// and with the key not `held`, and calls `ended` then.
    pub fn new(source: S, minutes: u32, stop: StopHandle, held: H, ended: F) -> Self {
        Self {
            source,
            watch: Watch::new((minutes as usize).saturating_mul(FRAMES_PER_MINUTE)),
            stop,
            held,
            fired: false,
            ended: Some(ended),
        }
    }
}

impl<S: AudioSource, H: Fn() -> bool, F: FnOnce()> AudioSource for UntilQuiet<S, H, F> {
    fn read(&mut self, buf: &mut [i16]) -> Result<usize> {
        let read = self.source.read(buf)?;
        if self.fired {
            if read == 0
                && let Some(ended) = self.ended.take()
            {
                ended();
            }
        } else {
            let quiet = self.watch.hear(&buf[..read]);
            if (self.held)() {
                // The time starts when the key is let go.
                self.watch.quiet = 0;
            } else if quiet && !self.stop.is_stopped() {
                // Not if it was stopped at this very moment: whoever did
                // that ends it. What was read is passed on, and so is what
                // the source still hands over on its way out.
                self.fired = true;
                self.stop.stop();
            }
        }
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    const SECOND: usize = SAMPLE_RATE_HZ as usize;

    /// A watch that allows a minute, so that the tests need not make up
    /// half an hour of sound each. What it takes for a sound is the same.
    fn watch() -> Watch {
        Watch::new(FRAMES_PER_MINUTE)
    }

    /// A noise that is the same from one call to the next.
    struct Noise(u32);

    impl Noise {
        /// The next sample, between `-peak` and `peak`.
        fn next(&mut self, peak: i32) -> i16 {
            self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let unit = i32::try_from(self.0 >> 16).unwrap_or(0) - 32_768;
            i16::try_from(unit * peak / 32_768).unwrap_or(0)
        }

        fn of(&mut self, samples: usize, peak: i32) -> Vec<i16> {
            (0..samples).map(|_| self.next(peak)).collect()
        }
    }

    /// One of the recorded sentences, with the room before and after it.
    fn recording(wav: &[u8]) -> Vec<i16> {
        wav.get(44..)
            .unwrap_or_default()
            .chunks_exact(2)
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
            .collect()
    }

    fn recordings() -> [Vec<i16>; 2] {
        [
            recording(include_bytes!("../stt/local/clips/en.wav")),
            recording(include_bytes!("../stt/local/clips/sl.wav")),
        ]
    }

    /// Seconds from the start of `samples`, heard 10 ms at a time as from a
    /// microphone, until the watch says it has been quiet for too long.
    fn quiet_after(watch: &mut Watch, samples: &[i16]) -> Option<usize> {
        samples
            .chunks(SECOND / 100)
            .position(|packet| watch.hear(packet))
            .map(|packet| packet / 100)
    }

    /// The same sentence said softer: `part` of its level.
    fn softer(samples: &[i16], part: f32) -> Vec<i16> {
        #[allow(clippy::cast_possible_truncation)]
        samples
            .iter()
            .map(|&sample| (f32::from(sample) * part) as i16)
            .collect()
    }

    #[test]
    fn a_minute_of_a_still_room_is_quiet() {
        for peak in [0, 20, 300, 3000] {
            let room = Noise(1).of(61 * SECOND, peak);
            let after = quiet_after(&mut watch(), &room);
            assert!(matches!(after, Some(59..=60)), "peak {peak}: {after:?}");
        }
    }

    #[test]
    fn a_sentence_is_heard_and_the_minute_starts_again_after_it() {
        for sentence in recordings() {
            let mut watch = watch();
            let room = Noise(2).of(50 * SECOND, 40);
            assert_eq!(quiet_after(&mut watch, &room), None);
            assert_eq!(quiet_after(&mut watch, &sentence), None);
            // Without the sentence this would be the second minute.
            assert_eq!(quiet_after(&mut watch, &room), None);
            // The minute starts where the sentence ends, or some seconds
            // later: a room louder than the one of the recording is a sound
            // until it has been heard for a while.
            let after = quiet_after(&mut watch, &Noise(3).of(30 * SECOND, 40));
            assert!(matches!(after, Some(7..=20)), "{after:?}");
        }
    }

    #[test]
    fn a_soft_voice_is_heard_as_a_loud_one_is() {
        for sentence in recordings() {
            for part in [0.3, 0.1, 0.03] {
                let mut watch = watch();
                let room = Noise(4).of(50 * SECOND, 6);
                assert_eq!(quiet_after(&mut watch, &room), None);
                assert_eq!(quiet_after(&mut watch, &softer(&sentence, part)), None);
                assert_eq!(quiet_after(&mut watch, &room), None, "at {part}");
            }
        }
    }

    #[test]
    fn a_voice_over_a_loud_room_is_heard() {
        for sentence in recordings() {
            let mut noise = Noise(5);
            // A fan as loud as a tenth of full scale at its peaks, and the
            // sentence said over it.
            let over: Vec<i16> = sentence
                .iter()
                .map(|&sample| sample.saturating_add(noise.next(1500)))
                .collect();
            let mut watch = watch();
            let room = noise.of(50 * SECOND, 1500);
            assert_eq!(quiet_after(&mut watch, &room), None);
            assert_eq!(quiet_after(&mut watch, &over), None);
            assert_eq!(quiet_after(&mut watch, &room), None);
        }
    }

    #[test]
    fn the_click_of_a_key_is_not_a_sound() {
        let mut noise = Noise(6);
        let mut room = Vec::new();
        for _ in 0..61 {
            // A click of 30 ms every second.
            room.extend(noise.of(SECOND - 480, 30));
            room.extend(noise.of(480, 8000));
        }
        let after = quiet_after(&mut watch(), &room);
        assert!(matches!(after, Some(59..=60)), "{after:?}");
    }

    #[test]
    fn someone_who_keeps_speaking_is_never_cut_off() {
        let [english, slovenian] = recordings();
        let mut watch = watch();
        // Ten minutes of one sentence after the other.
        for _ in 0..40 {
            assert_eq!(quiet_after(&mut watch, &english), None);
            assert_eq!(quiet_after(&mut watch, &slovenian), None);
        }
    }

    struct Fixed(Vec<i16>, usize);

    impl AudioSource for Fixed {
        fn read(&mut self, buf: &mut [i16]) -> Result<usize> {
            let rest = &self.0[self.1..];
            let count = buf.len().min(rest.len());
            buf[..count].copy_from_slice(&rest[..count]);
            self.1 += count;
            Ok(count)
        }
    }

    /// A room that is heard until it is stopped. It then hands on the
    /// tenth of a second it still holds, and lets go of the microphone.
    struct Room {
        noise: Noise,
        stop: StopHandle,
        left: usize,
        released: std::rc::Rc<Cell<bool>>,
    }

    impl Room {
        fn new(stop: &StopHandle) -> (Self, std::rc::Rc<Cell<bool>>) {
            let released = std::rc::Rc::new(Cell::new(false));
            let room = Self {
                noise: Noise(7),
                stop: stop.clone(),
                left: SECOND / 10,
                released: std::rc::Rc::clone(&released),
            };
            (room, released)
        }
    }

    impl AudioSource for Room {
        fn read(&mut self, buf: &mut [i16]) -> Result<usize> {
            let count = if self.stop.is_stopped() {
                buf.len().min(self.left)
            } else {
                buf.len()
            };
            if self.stop.is_stopped() {
                self.left -= count;
                self.released.set(count == 0);
            }
            for slot in &mut buf[..count] {
                *slot = self.noise.next(40);
            }
            Ok(count)
        }
    }

    /// Reads `source` to its end and returns how much came.
    fn read_out(source: &mut impl AudioSource) -> usize {
        let mut buf = [0i16; SECOND / 10];
        let mut read = 0;
        while let Ok(count @ 1..) = source.read(&mut buf) {
            read += count;
        }
        read
    }

    #[test]
    fn the_source_ends_after_the_minutes_it_was_given_and_says_so_once() {
        let stop = StopHandle::new();
        let (room, released) = Room::new(&stop);
        let said = Cell::new(0);
        let mut source = UntilQuiet::new(
            room,
            30,
            stop.clone(),
            || false,
            || {
                // Said when the microphone has been let go of, not before.
                assert!(released.get());
                said.set(said.get() + 1);
            },
        );
        // What the source still held when it was stopped is handed on.
        assert_eq!(read_out(&mut source), 30 * 60 * SECOND + SECOND / 10);
        assert!(stop.is_stopped());
        assert_eq!(said.get(), 1);
        assert_eq!(source.read(&mut [0; 160]).ok(), Some(0));
        assert_eq!(said.get(), 1);
    }

    #[test]
    fn the_microphone_is_let_go_of_by_the_read_after_the_last_minute() {
        let stop = StopHandle::new();
        let (room, released) = Room::new(&stop);
        let mut source = UntilQuiet::new(room, 1, stop.clone(), || false, || {});
        let mut buf = [0i16; SECOND / 10];
        while !stop.is_stopped() {
            assert_eq!(source.read(&mut buf).ok(), Some(buf.len()));
        }
        // Nothing has to end first: not the recognizer, not the session.
        assert!(!released.get());
        assert_eq!(source.read(&mut buf).ok(), Some(buf.len()));
        assert_eq!(source.read(&mut buf).ok(), Some(0));
        assert!(released.get());
    }

    #[test]
    fn a_key_that_is_held_is_never_cut_off_and_the_time_starts_when_it_is_let_go() {
        let stop = StopHandle::new();
        let (room, _) = Room::new(&stop);
        let held = Cell::new(true);
        let said = Cell::new(false);
        let mut source = UntilQuiet::new(room, 1, stop.clone(), || held.get(), || said.set(true));
        let mut buf = [0i16; SECOND / 10];
        // Five minutes of nothing with the key down.
        for _ in 0..5 * 600 {
            assert_eq!(source.read(&mut buf).ok(), Some(buf.len()));
        }
        assert!(!stop.is_stopped());
        // Let go after a tap, it is a dictation that can be forgotten.
        held.set(false);
        assert_eq!(read_out(&mut source), 60 * SECOND + SECOND / 10);
        assert!(stop.is_stopped() && said.get());
    }

    #[test]
    fn a_source_someone_stops_has_not_ended_by_itself() {
        let stop = StopHandle::new();
        let (room, _) = Room::new(&stop);
        let said = Cell::new(false);
        let mut source = UntilQuiet::new(room, 1, stop.clone(), || false, || said.set(true));
        let mut buf = [0i16; SECOND / 10];
        assert_eq!(source.read(&mut buf).ok(), Some(buf.len()));
        stop.stop();
        assert_eq!(read_out(&mut source), SECOND / 10);
        assert!(!said.get());
    }

    #[test]
    fn a_source_that_ends_first_is_passed_on_as_it_is() {
        let said = Cell::new(false);
        let stop = StopHandle::new();
        let mut source = UntilQuiet::new(
            Fixed(vec![0; 400], 0),
            1,
            stop.clone(),
            || false,
            || said.set(true),
        );
        let mut buf = [0i16; 320];
        assert_eq!(source.read(&mut buf).ok(), Some(320));
        assert_eq!(source.read(&mut buf).ok(), Some(80));
        assert_eq!(source.read(&mut buf).ok(), Some(0));
        assert!(!said.get() && !stop.is_stopped());
    }
}
