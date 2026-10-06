//! Voice memos: a recording saved as an MP3 file.
//!
//! A memo is recorded like a dictation, but instead of being recognized it is
//! encoded and saved. The file can then be put on the clipboard, so it pastes
//! straight into a messenger or a mail, and its path is announced over the
//! local API for programs that send it somewhere themselves.
//!
//! A memo is for someone to listen to, so it is saved at the level of a
//! voice recording whatever the microphone gave ([`leveled`]). Recognition
//! does not care how softly a microphone records; a listener does: a memo
//! from a microphone set low, saved as it came, plays so softly that it
//! seems to hold nothing.

#[cfg(windows)]
mod clipboard;
#[cfg(windows)]
mod mp3;

use std::path::{Path, PathBuf};

use crate::audio::SAMPLE_RATE_HZ;
use crate::{Error, Result};

/// The longest memo that is kept, in samples: one hour. A recording that is
/// still running then is ended, so a forgotten memo cannot fill the memory.
pub const MAX_SAMPLES: usize = SAMPLE_RATE_HZ as usize * 60 * 60;
/// A recording shorter than this is a slip of the finger, not a memo.
pub const MIN_SAMPLES: usize = SAMPLE_RATE_HZ as usize * 3 / 10;
/// The level a memo is brought to: its louder moments at about -20 dB of
/// full scale, where speech recorded for listening is.
const VOICE_LEVEL: f32 = 3277.0;
/// A memo is made at most this much louder: 30 dB. More would only turn the
/// hiss of a microphone that heard nothing into a roar.
const MOST_GAIN: f32 = 31.6;
/// Above this a sample is eased towards full scale instead of cut off.
const KNEE: f32 = 26_000.0;
/// The level is measured over this many samples at a time: 20 ms.
const FRAME: usize = SAMPLE_RATE_HZ as usize / 50;

/// A saved voice memo.
#[derive(Debug, Clone, PartialEq)]
pub struct Memo {
    /// The MP3 file.
    pub path: PathBuf,
    /// How long the recording is.
    pub seconds: f32,
}

/// The folder memos are saved in: `configured` if it names one, otherwise
/// `Vinput memos` in the Documents folder of the user.
#[must_use]
pub fn folder(configured: &str) -> PathBuf {
    let configured = configured.trim();
    if configured.is_empty() {
        platform::documents().join("Vinput memos")
    } else {
        PathBuf::from(configured)
    }
}

/// Today on the clock of the user, as `YYYYMMDD`.
#[must_use]
pub fn today() -> String {
    let [year, month, day, ..] = platform::local_time();
    format!("{year:04}{month:02}{day:02}")
}

/// The file name of a memo recorded at the given local date and time.
fn file_name([year, month, day, hour, minute, second]: [u16; 6]) -> String {
    format!("Voice memo {year:04}-{month:02}-{day:02} {hour:02}.{minute:02}.{second:02}.mp3")
}

/// Encodes `samples` (mono 16-bit at the pipeline rate) and saves them as a
/// new MP3 file in the memo folder.
///
/// # Errors
///
/// Returns [`Error::Memo`] if the folder cannot be created or the encoder
/// fails.
#[expect(
    clippy::cast_precision_loss,
    reason = "a duration in seconds, shown to one decimal place"
)]
pub fn save(samples: &[i16], configured_folder: &str) -> Result<Memo> {
    let dir = folder(configured_folder);
    std::fs::create_dir_all(&dir)
        .map_err(|err| Error::Memo(format!("creating {}: {err}", dir.display())))?;
    let path = unused(&dir.join(file_name(platform::local_time())));
    platform::encode(&leveled(samples), &path)?;
    Ok(Memo {
        path,
        seconds: samples.len() as f32 / SAMPLE_RATE_HZ as f32,
    })
}

/// `samples` as loud as a voice memo should be: made louder, never softer,
/// until the louder moments of the recording are at [`VOICE_LEVEL`].
///
/// The measure is the level that one frame in ten is above, which in speech
/// is the level of the voice and not that of one loud click. What the gain
/// pushes past [`KNEE`], such a click, is eased in under full scale.
#[must_use]
pub fn leveled(samples: &[i16]) -> Vec<i16> {
    let mut levels: Vec<f32> = samples
        .chunks_exact(FRAME)
        .map(|frame| {
            let energy: f32 = frame.iter().map(|&s| f32::from(s) * f32::from(s)).sum();
            #[expect(clippy::cast_precision_loss, reason = "a frame is 320 samples")]
            let mean = energy / FRAME as f32;
            mean.sqrt()
        })
        .collect();
    levels.sort_by(f32::total_cmp);
    let voice = levels.get(levels.len() * 9 / 10).copied().unwrap_or(0.0);
    let gain = (VOICE_LEVEL / voice.max(1.0)).clamp(1.0, MOST_GAIN);
    let room = f32::from(i16::MAX) - KNEE;
    samples
        .iter()
        .map(|&sample| {
            let loud = f32::from(sample) * gain;
            let eased = if loud.abs() > KNEE {
                (KNEE + room * ((loud.abs() - KNEE) / room).tanh()).copysign(loud)
            } else {
                loud
            };
            #[expect(clippy::cast_possible_truncation, reason = "eased in under full scale")]
            let sample = eased.round() as i16;
            sample
        })
        .collect()
}

/// `path`, or a numbered variant of it if a file of that name already exists.
fn unused(path: &Path) -> PathBuf {
    let mut candidate = path.to_path_buf();
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned());
    let mut number = 2;
    while candidate.exists() {
        candidate = path.with_file_name(format!(
            "{} ({number}).mp3",
            stem.as_deref().unwrap_or("Voice memo")
        ));
        number += 1;
    }
    candidate
}

/// Puts the memo on the clipboard as a file, replacing what was there.
///
/// # Errors
///
/// Returns [`Error::Memo`] if the clipboard cannot be written.
pub fn copy_to_clipboard(memo: &Memo) -> Result<()> {
    platform::put_file(&memo.path)
}

/// Encodes samples to an MP3 file at an exact path as a memo is saved, for
/// trying out what a recording comes to.
///
/// # Errors
///
/// Returns [`Error::Memo`] if the encoder fails.
pub fn encode_to(samples: &[i16], path: &Path) -> Result<()> {
    platform::encode(&leveled(samples), path)
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod platform {
    use std::path::PathBuf;
    use std::ptr::null_mut;

    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    use windows_sys::Win32::UI::Shell::{FOLDERID_Documents, SHGetKnownFolderPath};

    pub(super) use super::clipboard::put_file;
    pub(super) use super::mp3::encode;

    /// The Documents folder, wherever the user has moved it.
    pub(super) fn documents() -> PathBuf {
        let mut path = null_mut();
        // SAFETY: on success `path` is a NUL-terminated string allocated by
        // the system, read before it is freed exactly once. It is freed on
        // failure too, as the function documents.
        let known = unsafe {
            let documents = FOLDERID_Documents;
            let found =
                SHGetKnownFolderPath(&raw const documents, 0, null_mut(), &raw mut path) >= 0;
            let known = (found && !path.is_null()).then(|| {
                let mut length = 0;
                while *path.add(length) != 0 {
                    length += 1;
                }
                PathBuf::from(String::from_utf16_lossy(std::slice::from_raw_parts(
                    path, length,
                )))
            });
            CoTaskMemFree(path.cast());
            known
        };
        known.unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default()).join("Documents")
        })
    }

    /// Year, month, day, hour, minute and second on the clock of the user.
    pub(super) fn local_time() -> [u16; 6] {
        let mut now = windows_sys::Win32::Foundation::SYSTEMTIME::default();
        // SAFETY: the call only fills in the structure.
        unsafe { GetLocalTime(&raw mut now) };
        [
            now.wYear,
            now.wMonth,
            now.wDay,
            now.wHour,
            now.wMinute,
            now.wSecond,
        ]
    }
}

#[cfg(not(windows))]
mod platform {
    use std::path::{Path, PathBuf};

    use crate::{Error, Result};

    pub(super) fn documents() -> PathBuf {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
    }

    pub(super) fn local_time() -> [u16; 6] {
        [1970, 1, 1, 0, 0, 0]
    }

    pub(super) fn encode(_samples: &[i16], _path: &Path) -> Result<()> {
        Err(Error::Memo("voice memos need Windows".into()))
    }

    pub(super) fn put_file(_path: &Path) -> Result<()> {
        Err(Error::Memo("voice memos need Windows".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memos_are_named_after_the_moment_they_were_recorded() {
        assert_eq!(
            file_name([2026, 10, 5, 8, 4, 9]),
            "Voice memo 2026-10-05 08.04.09.mp3"
        );
    }

    /// A tone of `millis` whose level, the root of its mean square, is
    /// `level`.
    fn tone(millis: u32, level: f32) -> Vec<i16> {
        #[expect(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
        (0..SAMPLE_RATE_HZ * millis / 1000)
            .map(|n| {
                let phase = n as f32 * 440.0 * std::f32::consts::TAU / SAMPLE_RATE_HZ as f32;
                (phase.sin() * level * std::f32::consts::SQRT_2) as i16
            })
            .collect()
    }

    fn level(samples: &[i16]) -> f32 {
        let energy: f32 = samples.iter().map(|&s| f32::from(s) * f32::from(s)).sum();
        #[expect(clippy::cast_precision_loss, reason = "a level, to compare")]
        let mean = energy / samples.len().max(1) as f32;
        mean.sqrt()
    }

    #[test]
    fn a_memo_from_a_soft_microphone_is_as_loud_as_any() {
        // A voice at -38 dB, as a microphone set low records it, between
        // two stretches of the room, with the ready sound at the start.
        let soft = [
            tone(100, 1300.0),
            tone(500, 5.0),
            tone(2000, 400.0),
            tone(400, 5.0),
        ]
        .concat();
        let memo = leveled(&soft);
        assert_eq!(memo.len(), soft.len());
        let voice = level(&memo[16_000..32_000]);
        assert!((3000.0..3500.0).contains(&voice), "{voice}");
        // A voice that is loud enough is left as it is, and so is silence.
        let loud = tone(2000, 6000.0);
        assert!(leveled(&loud) == loud);
        let silence = vec![0; 16_000];
        assert!(leveled(&silence) == silence);
        // The hiss of a microphone that heard nothing is not made a roar.
        let hiss = level(&leveled(&tone(2000, 10.0)));
        assert!(hiss < 400.0, "{hiss}");
        // What the gain pushes over the top is eased in, not wrapped around.
        let click = [tone(2000, 400.0), vec![i16::MAX, i16::MIN, 20_000, -20_000]].concat();
        let memo = leveled(&click);
        let end = &memo[memo.len() - 4..];
        assert!(end[0] > 30_000 && end[1] < -30_000 && end[2] > 30_000 && end[3] < -30_000);
    }

    #[test]
    fn a_configured_folder_wins_over_the_default() {
        assert_eq!(folder(r" D:\Memos "), PathBuf::from(r"D:\Memos"));
        assert!(folder("").ends_with("Vinput memos"));
    }

    #[test]
    fn an_existing_name_gets_a_number() -> std::io::Result<()> {
        let dir = std::env::temp_dir().join(format!("vinput-memo-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let first = dir.join("Voice memo.mp3");
        assert_eq!(unused(&first), first);
        std::fs::write(&first, b"x")?;
        assert_eq!(unused(&first), dir.join("Voice memo (2).mp3"));
        std::fs::remove_dir_all(dir)
    }

    /// How many frames `mp3` is, if it is frames of MPEG-1 layer III at
    /// 64 kbit/s and 32 kHz from its first byte to its last.
    #[cfg(windows)]
    fn frames(mut mp3: &[u8]) -> Option<usize> {
        let mut count = 0;
        while let [0xFF, 0xFA | 0xFB, rates, ..] = mp3 {
            // The bit rate and the sample rate, then whether this frame has
            // a byte of padding.
            if rates & 0xFC != 0x58 {
                return None;
            }
            let length = 144 * 64_000 / 32_000 + usize::from(rates >> 1 & 1);
            mp3 = mp3.get(length..)?;
            count += 1;
        }
        mp3.is_empty().then_some(count)
    }

    #[cfg(windows)]
    #[test]
    fn the_tag_windows_puts_in_front_is_taken_off() {
        use super::mp3::without_tag;
        let audio = [0xFF, 0xFB, 0x58, 0xC0, 1, 2, 3];
        // A tag of 0x1A5 bytes, as Windows writes it: 3 * 128 + 37.
        let mut tagged = b"ID3    %".to_vec();
        tagged.extend([0; 0x1A5]);
        tagged.extend(audio);
        assert_eq!(without_tag(&tagged), audio);
        // With a footer, ten bytes more.
        tagged[5] = 0x10;
        assert_eq!(without_tag(&tagged), &audio[..0]);
        // No tag, or not enough of one to tell: left as it is.
        assert_eq!(without_tag(&audio), audio);
        assert_eq!(without_tag(b"ID3"), b"ID3");
        // A tag that says it is longer than the file leaves nothing.
        assert_eq!(without_tag(b"ID3    %"), b"");
    }

    #[cfg(windows)]
    #[test]
    fn a_recording_becomes_a_playable_mp3() -> std::io::Result<()> {
        let dir = std::env::temp_dir().join(format!("vinput-mp3-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("tone.mp3");
        // Two seconds of a 440 Hz tone.
        #[expect(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
        let samples: Vec<i16> = (0..SAMPLE_RATE_HZ * 2)
            .map(|n| {
                let phase = n as f32 * 440.0 * std::f32::consts::TAU / SAMPLE_RATE_HZ as f32;
                (phase.sin() * 8000.0) as i16
            })
            .collect();
        let encoded = encode_to(&samples, &path);
        if encoded.is_err() && std::env::var_os("CI").is_some() {
            // Server editions of Windows can come without the media features.
            eprintln!("skipped: {encoded:?}");
            return std::fs::remove_dir_all(dir);
        }
        assert!(encoded.is_ok(), "{encoded:?}");
        let bytes = std::fs::read(&path)?;
        // 64 kbit/s is 8 kB per second; allow for padding.
        assert!(
            (15_000..18_000).contains(&bytes.len()),
            "{} bytes",
            bytes.len()
        );
        // Nothing but audio: frames of 1152 samples at 32 kHz from the first
        // byte to the last, two seconds of them and what the encoder needs
        // to start and to end. No tag in front, which Windows fills in so
        // that some players play nothing at all.
        let frames = frames(&bytes);
        assert!(frames.is_some(), "not frames throughout");
        let seconds = frames.unwrap_or(0) * 1152 / 32;
        assert!((2000..2200).contains(&seconds), "{seconds} ms");
        assert!(!bytes.windows(8).any(|tag| tag == b"iTunSMPB"));
        std::fs::remove_dir_all(dir)
    }
}
