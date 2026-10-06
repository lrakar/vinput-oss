//! Plays the sounds of a dictation with as little delay as Windows allows.
//!
//! One thread, `vinput-sound`, owns a stream to the default speakers
//! (WASAPI, shared mode) and is woken for each sound asked for. What makes
//! a sound late is everything that happens before Windows mixes its first
//! sample, and that is, measured on a desktop PC with nothing else playing:
//!
//! - waking the speakers, which Windows lets go to rest a few seconds after
//!   the last stream on them has stopped: 90 ms, spent in starting a stream;
//! - opening a stream: 20 to 30 ms;
//! - asking which speakers are the default ones: 2 ms;
//! - writing a sound into a running stream: a hundredth of a millisecond,
//!   after which Windows takes it at its next mixing pass, which was 4 to
//!   7 ms later and is 10 ms later at most.
//!
//! So the stream is kept open and running, playing nothing, from the first
//! sound of a dictation until two minutes after the last, and through a
//! dictation however long it is. In that time a sound costs the last of
//! those lines alone. The whole sound is written in one go, so there is no
//! thread of ours for a busy PC to hold up and no crackle to come of it,
//! and it is synthesized at the rate of the speakers, so Windows has nothing
//! to convert.
//!
//! The first sound after more than two minutes without a dictation pays for
//! the rest, about a tenth of a second where nothing else is playing. That
//! is the price of holding nothing for good: a stream that runs keeps the
//! speakers and the mixer of Windows awake and may keep the PC from going
//! to sleep, as the microphone would (`audio/armed.rs`). When the two
//! minutes have passed the stream is closed, the thread waits for nothing
//! but the next request, and an idle Vinput is not woken by it.
//!
//! Before the sound of a start, the stream is checked against what Windows
//! now uses as the speakers, so that headphones put on between two
//! dictations get the sound; the sound of a stop goes where the start went.
//! Any failure of a stream (the device went away, Windows reconfigured it)
//! is answered by opening a new one and trying once more. A sound that
//! cannot be played is left out and noted in the log; it is never a reason
//! for a session to fail.
#![allow(unsafe_code)]

use std::sync::{Condvar, Mutex, OnceLock, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Media::Audio::{
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, IAudioClient, IAudioRenderClient, IMMDeviceEnumerator,
    MMDeviceEnumerator, WAVE_FORMAT_PCM, WAVEFORMATEX, eConsole, eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree};

use super::Cue;
use crate::audio::{apartment, device_id};
use crate::{Error, Result, log};

/// Room in the stream for audio that has not been played yet, in
/// 100-nanosecond units: half a second, several times a sound. It adds no
/// delay; Windows mixes what is there every 10 ms whatever the size.
const BUFFER_DURATION_HNS: i64 = 5_000_000;
/// How long the stream is kept running after the last sound, for the next
/// dictation: as long as the microphone is kept.
const AWAKE: Duration = Duration::from_secs(120);
/// How long the stream is kept running for the stop of a dictation that has
/// started. Longer than a dictation is; one that ends without a stop (it
/// broke, or its stop makes no sound) leaves the stream running this long
/// at most.
const DICTATING: Duration = Duration::from_secs(600);
/// Left and right.
const CHANNELS: u16 = 2;
const BYTES_PER_FRAME: u16 = CHANNELS * 2;
#[expect(clippy::cast_possible_truncation)]
const FORMAT_TAG_PCM: u16 = WAVE_FORMAT_PCM as u16;

/// What the thread is asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Ask {
    /// Play this sound now.
    Play(Cue),
    /// A dictation has started without a sound: have the stream running
    /// for the sound of its stop.
    Warm,
}

/// The request that waits to be served. One at most: a sound asked for
/// before the one before it was played takes its place, as it would have a
/// moment later anyway.
static WAITING: Mutex<Option<Ask>> = Mutex::new(None);
/// Wakes the thread for what is [`WAITING`].
static ASKED: Condvar = Condvar::new();

/// Hands `ask` to the thread, which is started the first time. Never waits.
pub(super) fn ask(ask: Ask) {
    static STARTED: OnceLock<bool> = OnceLock::new();
    let started = *STARTED.get_or_init(|| {
        thread::Builder::new()
            .name("vinput-sound".into())
            .spawn(run)
            .map_err(|err| log::write(&format!("no thread for sounds: {err}")))
            .is_ok()
    });
    if started {
        *WAITING.lock().unwrap_or_else(PoisonError::into_inner) = Some(ask);
        ASKED.notify_one();
    }
}

/// The life of the thread: serve a request, then wait for the next one, or
/// for the time at which the stream is to be closed.
fn run() {
    let mut output: Option<Output> = None;
    // Whether the last request failed, so that a PC without speakers is
    // noted in the log once and not at every key press.
    let mut failing = false;
    loop {
        let Some(ask) = next(output.as_ref().map(|open| open.until)) else {
            output = None;
            continue;
        };
        match serve(&mut output, ask) {
            Ok(()) => failing = false,
            Err(err) => {
                if !failing {
                    log::write(&format!("could not play a sound: {err}"));
                }
                failing = true;
            }
        }
    }
}

/// Waits for the next request and takes it. `None` when `until`, the time
/// at which the open stream is to be closed, comes first.
fn next(until: Option<Instant>) -> Option<Ask> {
    let mut waiting = WAITING.lock().unwrap_or_else(PoisonError::into_inner);
    loop {
        if let Some(ask) = waiting.take() {
            return Some(ask);
        }
        waiting = match until {
            None => ASKED.wait(waiting).unwrap_or_else(PoisonError::into_inner),
            Some(until) => {
                let left = until.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return None;
                }
                let woken = ASKED.wait_timeout(waiting, left);
                woken.unwrap_or_else(PoisonError::into_inner).0
            }
        };
    }
}

/// Carries out one request on the stream that is open, or on a new one.
fn serve(output: &mut Option<Output>, ask: Ask) -> Result<()> {
    // A stream kept from the dictation before may be on what are no longer
    // the speakers.
    if ask != Ask::Play(Cue::Stop) && output.as_ref().is_some_and(|open| !open.is_current()) {
        *output = None;
    }
    let kept = output.is_some();
    let mut result = match output {
        Some(open) => open.serve(ask),
        None => Output::open().and_then(|open| output.insert(open).serve(ask)),
    };
    if result.is_err() && kept {
        // The stream that was kept has gone bad: once more on a new one.
        result = Output::open().and_then(|open| output.insert(open).serve(ask));
    }
    if result.is_err() {
        *output = None;
    }
    result
}

/// A stream to the default speakers. Made and used on the one thread.
struct Output {
    enumerator: IMMDeviceEnumerator,
    client: IAudioClient,
    render: IAudioRenderClient,
    /// The speakers the stream is on, as Windows identifies them.
    device: Option<String>,
    /// How many frames the stream has room for.
    room: u32,
    /// The sounds at the rate of the stream, [`Cue::Start`] first.
    sounds: [Vec<i16>; 2],
    /// Frames a second.
    rate: u32,
    running: bool,
    /// When the sound written last is over.
    over: Instant,
    /// When the stream is to be closed.
    until: Instant,
}

impl Output {
    /// Opens a stream on the default speakers, stopped and empty.
    fn open() -> Result<Self> {
        apartment()?;
        // SAFETY: COM is available. The mix format Windows hands out is read
        // and then freed with the allocator that made it; the format handed
        // in outlives the call it is handed to.
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(failed("creating the device enumerator"))?;
            let speakers = enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .map_err(failed("finding the speakers"))?;
            let client: IAudioClient = speakers
                .Activate(CLSCTX_ALL, None)
                .map_err(failed("opening the speakers"))?;
            let mix = client
                .GetMixFormat()
                .map_err(failed("asking for the format of the speakers"))?;
            let rate = (*mix).nSamplesPerSec;
            CoTaskMemFree(Some(mix.cast_const().cast()));
            // 16 bits in stereo at the rate Windows mixes at: nothing is
            // resampled, and Windows spreads the two channels over however
            // many speakers there are.
            let format = WAVEFORMATEX {
                wFormatTag: FORMAT_TAG_PCM,
                nChannels: CHANNELS,
                nSamplesPerSec: rate,
                nAvgBytesPerSec: rate * u32::from(BYTES_PER_FRAME),
                nBlockAlign: BYTES_PER_FRAME,
                wBitsPerSample: 16,
                cbSize: 0,
            };
            client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                    BUFFER_DURATION_HNS,
                    0,
                    &raw const format,
                    None,
                )
                .map_err(failed("configuring the speakers"))?;
            let room = client
                .GetBufferSize()
                .map_err(failed("sizing the stream to the speakers"))?;
            let render: IAudioRenderClient = client
                .GetService()
                .map_err(failed("setting up the speakers"))?;
            let now = Instant::now();
            Ok(Self {
                device: device_id(&speakers),
                enumerator,
                client,
                render,
                room,
                sounds: [
                    super::samples(Cue::Start, rate),
                    super::samples(Cue::Stop, rate),
                ],
                rate,
                running: false,
                over: now,
                until: now,
            })
        }
    }

    /// Whether the stream is on what Windows now uses as the speakers.
    fn is_current(&self) -> bool {
        // SAFETY: COM is available on this thread, which opened the stream.
        let speakers = unsafe {
            self.enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .ok()
                .and_then(|speakers| device_id(&speakers))
        };
        speakers.is_some() && speakers == self.device
    }

    /// Plays a sound, or only has the stream running for one.
    fn serve(&mut self, ask: Ask) -> Result<()> {
        let now = Instant::now();
        // Whatever comes of this, the stream is not kept for longer than a
        // sound was last asked of it.
        self.until = now;
        if let Ask::Play(cue) = ask {
            // A sound still playing gives way to the new one: a key let go
            // of at once is answered at once, not when the start has
            // finished sounding.
            if self.running && now < self.over {
                self.halt()?;
            }
            let length = self.write(cue)?;
            self.over = now + length;
        }
        if !self.running {
            // SAFETY: the client is live and initialized.
            unsafe { self.client.Start() }.map_err(failed("starting the speakers"))?;
            self.running = true;
        }
        self.until = now
            + match ask {
                Ask::Play(Cue::Stop) => AWAKE,
                Ask::Play(Cue::Start) | Ask::Warm => DICTATING,
            };
        Ok(())
    }

    /// Writes the whole sound into the stream and says how long it lasts.
    fn write(&mut self, cue: Cue) -> Result<Duration> {
        let sound = &self.sounds[usize::from(cue == Cue::Stop)];
        // SAFETY: the client is live. Windows hands out room for `frames`
        // frames of two 16-bit samples each, and all of that room is written
        // to before it is handed back.
        unsafe {
            let waiting = self
                .client
                .GetCurrentPadding()
                .map_err(failed("asking how much the speakers have left"))?;
            let frames = u32::try_from(sound.len())
                .unwrap_or(u32::MAX)
                .min(self.room.saturating_sub(waiting));
            if frames == 0 {
                return Ok(Duration::ZERO);
            }
            let buffer = self
                .render
                .GetBuffer(frames)
                .map_err(failed("getting room for a sound"))?;
            let room = std::slice::from_raw_parts_mut(
                buffer,
                frames as usize * usize::from(BYTES_PER_FRAME),
            );
            for (frame, sample) in room
                .chunks_exact_mut(usize::from(BYTES_PER_FRAME))
                .zip(sound)
            {
                let [low, high] = sample.to_le_bytes();
                frame.copy_from_slice(&[low, high, low, high]);
            }
            self.render
                .ReleaseBuffer(frames, 0)
                .map_err(failed("handing a sound to the speakers"))?;
            Ok(Duration::from_secs_f64(
                f64::from(frames) / f64::from(self.rate.max(1)),
            ))
        }
    }

    /// Stops the stream and empties it.
    fn halt(&mut self) -> Result<()> {
        self.running = false;
        // SAFETY: the client is live; a stream is emptied only once stopped.
        unsafe {
            self.client
                .Stop()
                .map_err(failed("stopping the speakers"))?;
            self.client.Reset().map_err(failed("emptying the speakers"))
        }
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        if self.running {
            // SAFETY: the client is live.
            unsafe {
                let _ = self.client.Stop();
            }
        }
    }
}

fn failed(action: &'static str) -> impl Fn(windows::core::Error) -> Error {
    move |err| Error::Audio(format!("{action} failed: {}", err.message()))
}

#[cfg(test)]
mod tests {
    //! These run a real stream on the speakers of the PC they run on, so
    //! they are ignored unless asked for by name. Nothing is ever heard: the
    //! sounds are replaced with silence before anything is played.
    //!
    //! `cargo test -p vinput-core -- --ignored sound::player --nocapture`

    use windows::Win32::Media::Audio::{AUDCLNT_STREAMFLAGS_LOOPBACK, IAudioCaptureClient};

    use super::*;

    #[test]
    fn the_thread_is_handed_the_sound_asked_for_last_or_told_that_time_is_up() {
        // Nothing else puts a request here: a test never plays a sound.
        let put = |ask| *WAITING.lock().unwrap_or_else(PoisonError::into_inner) = Some(ask);
        put(Ask::Play(Cue::Start));
        put(Ask::Play(Cue::Stop));
        assert_eq!(next(None), Some(Ask::Play(Cue::Stop)));
        // With nothing asked for, it waits until the stream is to be closed.
        let started = Instant::now();
        assert_eq!(next(Some(started + Duration::from_millis(30))), None);
        assert!(started.elapsed() >= Duration::from_millis(30));
        assert_eq!(next(Some(started)), None);
        // A request made while it waits wakes it.
        let asking = thread::spawn(move || {
            thread::sleep(Duration::from_millis(30));
            put(Ask::Warm);
            ASKED.notify_one();
        });
        assert_eq!(
            next(Some(started + Duration::from_secs(60))),
            Some(Ask::Warm)
        );
        assert!(started.elapsed() < Duration::from_secs(30));
        assert!(asking.join().is_ok());
    }

    fn silent() -> Result<Output> {
        let mut output = Output::open()?;
        for sound in &mut output.sounds {
            sound.fill(0);
        }
        Ok(output)
    }

    /// How much of what was written has not been played yet, in frames.
    fn waiting(output: &Output) -> Result<u32> {
        // SAFETY: the client is live.
        unsafe { output.client.GetCurrentPadding() }.map_err(failed("asking"))
    }

    /// What the speakers are being sent, to see when a sound gets there.
    struct Heard {
        client: IAudioClient,
        capture: IAudioCaptureClient,
        channels: usize,
    }

    impl Heard {
        fn listen() -> Result<Self> {
            apartment()?;
            // SAFETY: COM is available; the mix format is read and freed.
            unsafe {
                let enumerator: IMMDeviceEnumerator =
                    CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                        .map_err(failed("creating the device enumerator"))?;
                let client: IAudioClient = enumerator
                    .GetDefaultAudioEndpoint(eRender, eConsole)
                    .and_then(|speakers| speakers.Activate(CLSCTX_ALL, None))
                    .map_err(failed("opening the speakers"))?;
                let mix = client.GetMixFormat().map_err(failed("asking"))?;
                let channels = (*mix).nChannels;
                // What Windows mixes in is 32-bit floating point.
                let bits = (*mix).wBitsPerSample;
                assert_eq!(bits, 32);
                let listening = client.Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_LOOPBACK,
                    BUFFER_DURATION_HNS,
                    0,
                    mix,
                    None,
                );
                CoTaskMemFree(Some(mix.cast_const().cast()));
                listening.map_err(failed("listening to the speakers"))?;
                let capture: IAudioCaptureClient =
                    client.GetService().map_err(failed("setting up"))?;
                client.Start().map_err(failed("starting to listen"))?;
                Ok(Self {
                    client,
                    capture,
                    channels: usize::from(channels),
                })
            }
        }

        /// Waits up to `patience` for something that is not silence and
        /// says when the packet it came in arrived, which is when Windows had
        /// mixed it at the latest.
        fn first_sound(&self, patience: Duration) -> Result<Option<Instant>> {
            let deadline = Instant::now() + patience;
            while Instant::now() < deadline {
                // SAFETY: the client is live; each packet is read within
                // the bounds Windows gives and handed back.
                unsafe {
                    while self.capture.GetNextPacketSize().map_err(failed("asking"))? > 0 {
                        let arrived = Instant::now();
                        let (mut data, mut frames, mut flags) = (std::ptr::null_mut(), 0, 0);
                        self.capture
                            .GetBuffer(&raw mut data, &raw mut frames, &raw mut flags, None, None)
                            .map_err(failed("reading"))?;
                        let bytes =
                            std::slice::from_raw_parts(data, frames as usize * self.channels * 4);
                        let first = bytes
                            .chunks_exact(4)
                            .position(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]) != 0.0);
                        self.capture
                            .ReleaseBuffer(frames)
                            .map_err(failed("reading"))?;
                        if first.is_some() {
                            return Ok(Some(arrived));
                        }
                    }
                }
                // Not a sleep, which Windows rounds up to its clock tick.
                thread::yield_now();
            }
            Ok(None)
        }
    }

    impl Drop for Heard {
        fn drop(&mut self) {
            // SAFETY: the client is live.
            unsafe {
                let _ = self.client.Stop();
            }
        }
    }

    #[test]
    #[ignore = "plays on the real speakers, 80 dB under full scale, which nobody can hear"]
    #[expect(
        clippy::print_stdout,
        reason = "the timings are what the test is run for"
    )]
    fn a_sound_is_on_its_way_to_the_speakers_within_a_mixing_pass() -> Result<()> {
        let mut output = Output::open()?;
        for sound in &mut output.sounds {
            // The shape of the sound, at three steps of the 65 536.
            for sample in sound.iter_mut() {
                *sample = sample.signum() * 3;
            }
        }
        output.serve(Ask::Warm)?;
        let heard = Heard::listen()?;
        if heard.first_sound(Duration::from_millis(500))?.is_some() {
            println!("something else is playing on this PC: nothing to measure against");
            return Ok(());
        }
        let mut delays = Vec::new();
        for cue in [Cue::Start, Cue::Stop, Cue::Start, Cue::Stop, Cue::Start] {
            let asked = Instant::now();
            output.serve(Ask::Play(cue))?;
            let Some(sent) = heard.first_sound(Duration::from_millis(500))? else {
                return Err(Error::Audio("the sound never reached the speakers".into()));
            };
            delays.push(sent.saturating_duration_since(asked));
            // Past the end of the sound, to another point of the pass, with
            // nothing of the sound left to be taken for the next one.
            thread::sleep(Duration::from_millis(203));
            while heard.first_sound(Duration::from_millis(50))?.is_some() {}
        }
        println!("from the request to the mix of Windows: {delays:?}");
        let slowest = delays.iter().max().copied().unwrap_or_default();
        assert!(slowest < Duration::from_millis(15), "{slowest:?}");
        Ok(())
    }

    #[test]
    #[ignore = "plays silence on the real speakers"]
    #[expect(
        clippy::print_stdout,
        reason = "the timings are what the test is run for"
    )]
    fn a_sound_on_a_running_stream_is_handed_over_in_under_a_millisecond() -> Result<()> {
        let started = Instant::now();
        let mut output = silent()?;
        let opening = started.elapsed();
        assert!(output.is_current() && !output.running);

        let started = Instant::now();
        output.serve(Ask::Warm)?;
        let waking = started.elapsed();
        assert!(output.running);
        assert!(output.until > Instant::now() + AWAKE);

        let mut handing = Vec::new();
        for cue in [Cue::Start, Cue::Stop, Cue::Start, Cue::Stop] {
            // The stream is running: Windows takes a sound without being
            // fed, and has played all of it well within the time.
            thread::sleep(Duration::from_millis(300));
            assert_eq!(waiting(&output)?, 0, "all of it was played");
            let started = Instant::now();
            output.serve(Ask::Play(cue))?;
            handing.push(started.elapsed());
        }
        assert!(output.until < Instant::now() + AWAKE);
        // A sound asked for while another plays takes its place.
        output.serve(Ask::Play(Cue::Start))?;
        output.serve(Ask::Play(Cue::Stop))?;
        let whole = u32::try_from(output.sounds[1].len()).unwrap_or(0);
        assert!(waiting(&output)? <= whole);
        assert!(output.running);

        let started = Instant::now();
        let current = output.is_current();
        let checking = started.elapsed();
        assert!(current);

        println!(
            "opening {opening:?}, waking the speakers {waking:?}, a sound on the running \
             stream {handing:?}, checking the speakers {checking:?}, at {} Hz",
            output.rate
        );
        let slowest = handing.iter().max().copied().unwrap_or_default();
        assert!(slowest < Duration::from_millis(1), "{slowest:?}");
        Ok(())
    }
}
