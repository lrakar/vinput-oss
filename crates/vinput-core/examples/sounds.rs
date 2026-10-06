//! Writes the sounds of a dictation as WAV files, to listen to them without
//! running Vinput: `start.wav`, `stop.wav`, and `both.wav` with a second of
//! dictation between the two.
//!
//! ```text
//! cargo run -p vinput-core --example sounds -- <folder>
//! ```

use std::path::Path;

use vinput_core::sound::{self, Cue};

fn main() -> std::io::Result<()> {
    let folder = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    let folder = Path::new(&folder);
    std::fs::create_dir_all(folder)?;
    let (start, stop) = (sound::wav(Cue::Start), sound::wav(Cue::Stop));
    std::fs::write(folder.join("start.wav"), &start)?;
    std::fs::write(folder.join("stop.wav"), &stop)?;

    // One file with the header of the first and the samples of both.
    let quiet = |tenths: usize| vec![0u8; tenths * 48_000 / 10 * 2];
    let samples = [&quiet(2), &start[44..], &quiet(12), &stop[44..], &quiet(5)].concat();
    let bytes = u32::try_from(samples.len()).unwrap_or(u32::MAX);
    let mut both = start[..44].to_vec();
    both[4..8].copy_from_slice(&(36 + bytes).to_le_bytes());
    both[40..44].copy_from_slice(&bytes.to_le_bytes());
    both.extend(samples);
    std::fs::write(folder.join("both.wav"), both)
}
