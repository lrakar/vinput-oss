//! Audio capture stage.

mod armed;
mod quiet;
#[cfg(windows)]
mod wasapi;
mod wav;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

pub use armed::{Armed, ArmedSource, CaptureBackend, CaptureStream, Changed};
pub use quiet::UntilQuiet;
#[cfg(windows)]
pub use wasapi::{ArmedMicrophone, Wasapi, WasapiSource, WasapiStream, WasapiWatch, microphones};
#[cfg(windows)]
pub(crate) use wasapi::{apartment, device_id};
pub use wav::{WavSource, wav_bytes};

use crate::Result;

/// Sample rate the pipeline works in, in hertz. Mono, 16-bit linear PCM.
pub const SAMPLE_RATE_HZ: u32 = 16_000;

/// A microphone that is connected to this PC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Microphone {
    /// What Windows knows the device by. It stays the same when the device is
    /// unplugged and plugged in again, so it is what the settings store.
    pub id: String,
    /// The name Windows shows for it, such as "Microphone (USB Audio)".
    pub name: String,
}

/// Asks a running microphone source to end, from any thread.
#[derive(Debug, Clone, Default)]
pub struct StopHandle(Arc<AtomicBool>);

impl StopHandle {
    /// Creates a handle that has not been triggered.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes the source's next `read` return `Ok(0)`.
    pub fn stop(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// Whether [`stop`](Self::stop) has been called.
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// A source of mono 16-bit PCM audio at [`SAMPLE_RATE_HZ`].
pub trait AudioSource {
    /// Fills `buf` with samples and returns how many were written.
    ///
    /// Returns `Ok(0)` once the source has ended.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Audio`](crate::Error::Audio) if the capture device fails.
    fn read(&mut self, buf: &mut [i16]) -> Result<usize>;
}

/// The loudness of the most recent audio, shared between the capture thread that
/// writes it and the UI thread that draws it.
#[derive(Debug, Clone, Default)]
pub struct LevelMeter(Arc<AtomicU32>);

impl LevelMeter {
    /// Creates a meter reading silence.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Peak amplitude of the latest audio, from 0.0 (silence) to 1.0 (full scale).
    #[must_use]
    pub fn level(&self) -> f32 {
        let peak = u16::try_from(self.0.load(Ordering::Relaxed)).unwrap_or(u16::MAX);
        f32::from(peak) / f32::from(i16::MAX.unsigned_abs())
    }

    /// Sets the meter back to silence.
    pub fn reset(&self) {
        self.0.store(0, Ordering::Relaxed);
    }

    fn record(&self, samples: &[i16]) {
        let peak = samples
            .iter()
            .map(|sample| sample.unsigned_abs())
            .max()
            .unwrap_or(0);
        self.0.store(u32::from(peak), Ordering::Relaxed);
    }
}

/// Wraps a source and reports the level of everything read from it to a meter.
#[derive(Debug)]
pub struct Metered<S> {
    source: S,
    meter: LevelMeter,
}

impl<S> Metered<S> {
    /// Reports the level of `source` to `meter`.
    pub fn new(source: S, meter: LevelMeter) -> Self {
        Self { source, meter }
    }
}

impl<S: AudioSource> AudioSource for Metered<S> {
    fn read(&mut self, buf: &mut [i16]) -> Result<usize> {
        let read = self.source.read(buf)?;
        self.meter.record(&buf[..read]);
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(Vec<i16>);

    impl AudioSource for Fixed {
        fn read(&mut self, buf: &mut [i16]) -> Result<usize> {
            let count = buf.len().min(self.0.len());
            for (slot, sample) in buf.iter_mut().zip(self.0.drain(..count)) {
                *slot = sample;
            }
            Ok(count)
        }
    }

    #[test]
    fn meter_follows_the_peak_of_each_read() {
        let meter = LevelMeter::new();
        let mut source = Metered::new(Fixed(vec![0, -16_384, 100, i16::MIN]), meter.clone());
        let mut buf = [0i16; 3];

        assert_eq!(source.read(&mut buf).ok(), Some(3));
        assert!((meter.level() - 0.5).abs() < 0.001);
        assert_eq!(source.read(&mut buf).ok(), Some(1));
        assert!(meter.level() >= 1.0);
        assert_eq!(source.read(&mut buf).ok(), Some(0));
        assert!(meter.level() < f32::EPSILON);
    }
}
