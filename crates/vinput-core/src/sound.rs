//! The short "ready to listen" sound.
//!
//! The tone is synthesized once at first use, so there is no sound file to ship
//! or lose.

const SAMPLE_RATE: u32 = 44_100;
const DURATION_MS: u32 = 170;
/// D5, with a quiet octave above it for warmth.
const FUNDAMENTAL_HZ: f32 = 587.33;
const PEAK: f32 = 0.16;
const ATTACK_SECONDS: f32 = 0.012;
const DECAY_SECONDS: f32 = 0.045;

/// Builds the ready sound as a complete WAV file in memory: a soft sine with a
/// fast fade-in and a smooth decay to silence.
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "sample indexes are small and samples are scaled into the i16 range"
)]
pub fn ready_wav() -> Vec<u8> {
    let count = SAMPLE_RATE * DURATION_MS / 1000;
    let data_bytes = count * 2;
    let mut wav = Vec::with_capacity(44 + data_bytes as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_bytes.to_le_bytes());

    let total = count as f32 / SAMPLE_RATE as f32;
    for index in 0..count {
        let t = index as f32 / SAMPLE_RATE as f32;
        let phase = std::f32::consts::TAU * FUNDAMENTAL_HZ * t;
        let tone = phase.sin() + 0.22 * (2.0 * phase).sin();
        let attack = (t / ATTACK_SECONDS).min(1.0);
        let decay = (-(t - ATTACK_SECONDS).max(0.0) / DECAY_SECONDS).exp();
        // A final linear fade guarantees the sound ends exactly at zero, with no click.
        let tail = ((total - t) / 0.02).clamp(0.0, 1.0);
        let sample = tone * attack * decay * tail * PEAK / 1.22;
        wav.extend_from_slice(&((sample * f32::from(i16::MAX)) as i16).to_le_bytes());
    }
    wav
}

/// Plays the ready sound without waiting for it to finish. A copy that
/// keeps to itself ([`crate::instance::is_quiet`]) plays nothing.
#[cfg(windows)]
#[allow(unsafe_code)]
pub fn play_ready() {
    use std::sync::OnceLock;

    use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};

    // Asynchronous playback reads the buffer after this function returns, so the
    // buffer has to live for the rest of the process.
    static WAV: OnceLock<Vec<u8>> = OnceLock::new();
    if crate::instance::is_quiet() {
        return;
    }
    let wav = WAV.get_or_init(ready_wav);
    // SAFETY: with SND_MEMORY the first argument is a pointer to a complete WAV
    // image, which `WAV` keeps alive and unchanged forever.
    unsafe {
        PlaySoundW(
            wav.as_ptr().cast(),
            std::ptr::null_mut(),
            SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples(wav: &[u8]) -> Vec<i16> {
        wav[44..]
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect()
    }

    #[test]
    fn wav_header_matches_its_data() {
        let wav = ready_wav();
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        let riff_size = u32::from_le_bytes([wav[4], wav[5], wav[6], wav[7]]) as usize;
        let data_size = u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]) as usize;
        assert_eq!(riff_size + 8, wav.len());
        assert_eq!(data_size + 44, wav.len());
    }

    #[test]
    fn sound_is_short_soft_and_ends_in_silence() {
        let samples = samples(&ready_wav());
        assert!(samples.len() < 44_100 / 4, "under a quarter of a second");
        let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
        assert!(peak > 1_000, "audible");
        assert!(peak < 6_600, "well below a fifth of full scale");
        assert_eq!(samples.first().copied(), Some(0));
        assert!(samples.last().is_some_and(|last| last.unsigned_abs() < 8));
    }
}
