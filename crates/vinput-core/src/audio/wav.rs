//! A WAV file as an audio source, for testing recognition without a microphone.

use std::path::Path;
use std::time::{Duration, Instant};

use super::{AudioSource, SAMPLE_RATE_HZ};
use crate::{Error, Result};

/// Plays back a mono 16-bit PCM WAV file recorded at
/// [`SAMPLE_RATE_HZ`](super::SAMPLE_RATE_HZ).
///
/// Samples are released at the speed a microphone would deliver them, because
/// streaming recognizers expect audio in real time.
#[derive(Debug)]
pub struct WavSource {
    samples: Vec<i16>,
    position: usize,
    started: Option<Instant>,
}

impl WavSource {
    /// Reads and validates the file at `path`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Audio`] if the file cannot be read or is not mono 16-bit
    /// PCM at the pipeline sample rate.
    pub fn open(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)
            .map_err(|err| Error::Audio(format!("reading {}: {err}", path.display())))?;
        Self::parse(&bytes)
    }

    fn parse(bytes: &[u8]) -> Result<Self> {
        let invalid = |what: &str| Error::Audio(format!("unsupported WAV file: {what}"));
        if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
            return Err(invalid("not a RIFF/WAVE file"));
        }
        let mut format_ok = false;
        let mut rest = &bytes[12..];
        while rest.len() >= 8 {
            let size = u32::from_le_bytes([rest[4], rest[5], rest[6], rest[7]]) as usize;
            let body = rest
                .get(8..8 + size)
                .ok_or_else(|| invalid("truncated chunk"))?;
            match &rest[..4] {
                b"fmt " => {
                    let field = |at: usize| body.get(at..at + 2).map(|b| [b[0], b[1]]);
                    let u16_at = |at| field(at).map(u16::from_le_bytes);
                    let rate = body
                        .get(4..8)
                        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
                    format_ok = u16_at(0) == Some(1)
                        && u16_at(2) == Some(1)
                        && rate == Some(SAMPLE_RATE_HZ)
                        && u16_at(14) == Some(16);
                    if !format_ok {
                        return Err(invalid("expected mono 16-bit PCM at 16 kHz"));
                    }
                }
                b"data" if format_ok => {
                    let samples = body
                        .chunks_exact(2)
                        .map(|b| i16::from_le_bytes([b[0], b[1]]))
                        .collect();
                    return Ok(Self {
                        samples,
                        position: 0,
                        started: None,
                    });
                }
                _ => {}
            }
            // Chunks are padded to an even length.
            rest = rest.get(8 + size + size % 2..).unwrap_or_default();
        }
        Err(invalid("no audio data found"))
    }
}

/// Encodes samples as a mono 16-bit PCM WAV file at
/// [`SAMPLE_RATE_HZ`](super::SAMPLE_RATE_HZ), the format [`WavSource`] reads.
#[must_use]
pub fn wav_bytes(samples: &[i16]) -> Vec<u8> {
    let len = u32::try_from(samples.len() * 2).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&len.saturating_add(36).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    // PCM, one channel.
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&SAMPLE_RATE_HZ.to_le_bytes());
    out.extend_from_slice(&(SAMPLE_RATE_HZ * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&len.to_le_bytes());
    out.extend(samples.iter().flat_map(|sample| sample.to_le_bytes()));
    out
}

impl WavSource {
    /// Every sample of the file at once, for uses that do not need real time.
    #[must_use]
    pub fn into_samples(self) -> Vec<i16> {
        self.samples
    }
}

impl AudioSource for WavSource {
    fn read(&mut self, buf: &mut [i16]) -> Result<usize> {
        let remaining = &self.samples[self.position..];
        let count = buf.len().min(remaining.len());
        if count == 0 {
            return Ok(0);
        }
        buf[..count].copy_from_slice(&remaining[..count]);
        self.position += count;

        // Wait until a live source would have produced the samples handed out so far.
        let started = *self.started.get_or_insert_with(Instant::now);
        let played = u64::try_from(self.position).unwrap_or(u64::MAX);
        let due =
            Duration::from_micros(played.saturating_mul(1_000_000) / u64::from(SAMPLE_RATE_HZ));
        std::thread::sleep(due.saturating_sub(started.elapsed()));
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(rate: u32, samples: &[i16]) -> Vec<u8> {
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let len = u32::try_from(data.len()).unwrap_or(u32::MAX);
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&data);
        out
    }

    #[test]
    fn encoded_samples_read_back() {
        let source = WavSource::parse(&wav_bytes(&[5, -6, 7]));
        assert_eq!(
            source.ok().map(WavSource::into_samples),
            Some(vec![5, -6, 7])
        );
    }

    #[test]
    fn reads_samples_then_ends() {
        let mut source = WavSource::parse(&wav(16_000, &[1, -2, 3])).ok();
        let mut buf = [0i16; 2];
        let mut read = |buf: &mut [i16]| source.as_mut().and_then(|s| s.read(buf).ok());
        assert_eq!(read(&mut buf), Some(2));
        assert_eq!(buf, [1, -2]);
        assert_eq!(read(&mut buf), Some(1));
        assert_eq!(buf[0], 3);
        assert_eq!(read(&mut buf), Some(0));
    }

    #[test]
    fn rejects_other_sample_rates_and_garbage() {
        assert!(WavSource::parse(&wav(44_100, &[0])).is_err());
        assert!(WavSource::parse(b"not a wav file").is_err());
    }
}
