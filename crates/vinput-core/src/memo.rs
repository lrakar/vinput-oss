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
pub(crate) mod clipboard;
#[cfg(windows)]
mod mp3;

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// The longest memo that is kept: one hour. A recording that is still
/// running then is ended, so a forgotten memo cannot fill the disk.
pub const MAX_SECONDS: u32 = 60 * 60;
/// A recording shorter than this, in milliseconds, is a slip of the finger,
/// not a memo.
const MIN_MILLIS: u32 = 300;
/// The level a memo is brought to: its louder moments at about -20 dB of
/// full scale, where speech recorded for listening is.
const VOICE_LEVEL: f32 = 3277.0;
/// A memo is made at most this much louder: 30 dB. More would only turn the
/// hiss of a microphone that heard nothing into a roar.
const MOST_GAIN: f32 = 31.6;
/// Above this a sample is eased towards full scale instead of cut off.
const KNEE: f32 = 26_000.0;
/// The level is measured over a fiftieth of a second at a time: 20 ms.
const FRAMES_PER_SECOND: u32 = 50;

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

/// A memo being recorded: mono 16-bit samples at the rate of the
/// microphone, up to [`MAX_SECONDS`] of them.
///
/// The samples are not held in memory, where an hour at 48 kHz would take
/// 350 MB, but written to a file of their own in the temporary folder, and
/// measured on the way for [`leveled`]. Windows deletes that file when it
/// is closed, which it is when the recording is saved or dropped and also
/// when Vinput ends in any other way, so no recording outlives its memo.
pub struct Recording {
    rate: u32,
    file: BufWriter<File>,
    /// How many samples have been written.
    samples: usize,
    levels: Levels,
}

impl std::fmt::Debug for Recording {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recording")
            .field("rate", &self.rate)
            .field("samples", &self.samples)
            .finish_non_exhaustive()
    }
}

impl Recording {
    /// Starts a recording of samples at `rate` hertz.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Memo`] if there is no room for it in the temporary
    /// folder.
    pub fn new(rate: u32) -> Result<Self> {
        let file = scratch_file().map_err(|err| keeping_failed(&err))?;
        Ok(Self {
            rate,
            file: BufWriter::with_capacity(1 << 16, file),
            samples: 0,
            levels: Levels::new(rate),
        })
    }

    /// Samples per second.
    #[must_use]
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// The most samples it takes: an hour of them.
    fn capacity(&self) -> usize {
        self.rate as usize * MAX_SECONDS as usize
    }

    /// Whether it has reached [`MAX_SECONDS`] and takes no more.
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.samples >= self.capacity()
    }

    /// Whether it is too short to be a memo.
    #[must_use]
    pub fn is_too_short(&self) -> bool {
        self.samples < self.rate as usize * MIN_MILLIS as usize / 1000
    }

    /// How long it is.
    #[must_use]
    #[expect(
        clippy::cast_precision_loss,
        reason = "a duration in seconds, shown to one decimal place"
    )]
    pub fn seconds(&self) -> f32 {
        self.samples as f32 / self.rate.max(1) as f32
    }

    /// Adds `samples` at the end, as far as there is room.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Memo`] if they cannot be written, as when the disk
    /// is full.
    pub fn push(&mut self, samples: &[i16]) -> Result<()> {
        let room = self.capacity().saturating_sub(self.samples);
        let samples = &samples[..samples.len().min(room)];
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        self.file
            .write_all(&bytes)
            .map_err(|err| keeping_failed(&err))?;
        self.levels.add(samples);
        self.samples += samples.len();
        Ok(())
    }

    /// Levels and encodes the recording, and saves it as a new MP3 file in
    /// the memo folder.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Memo`] if the folder cannot be created, the
    /// recording cannot be read back or the encoder fails.
    pub fn save(self, configured_folder: &str) -> Result<Memo> {
        let dir = folder(configured_folder);
        std::fs::create_dir_all(&dir)
            .map_err(|err| Error::Memo(format!("creating {}: {err}", dir.display())))?;
        let path = unused(&dir.join(file_name(platform::local_time())));
        let seconds = self.seconds();
        self.encode(&path)?;
        Ok(Memo { path, seconds })
    }

    /// Levels and encodes the recording into an MP3 file at `path`.
    fn encode(self, path: &Path) -> Result<()> {
        self.encode_with(path, platform::encode)
    }

    /// Hands the recording, leveled, to the encoder `mp3`.
    fn encode_with(
        self,
        path: &Path,
        mp3: impl FnOnce(u32, &Path, &mut dyn FnMut(&mut [i16]) -> usize) -> Result<()>,
    ) -> Result<()> {
        let Self {
            rate, file, levels, ..
        } = self;
        let gain = levels.gain();
        let mut file = file
            .into_inner()
            .map_err(|err| keeping_failed(err.error()))?;
        file.seek(SeekFrom::Start(0))
            .map_err(|err| keeping_failed(&err))?;
        let mut file = BufReader::with_capacity(1 << 16, file);
        let mut bytes = Vec::new();
        let mut failed = None;
        let encoded = mp3(rate, path, &mut |buf| {
            bytes.resize(buf.len() * 2, 0);
            match fill(&mut file, &mut bytes) {
                Ok(read) => {
                    for (slot, pair) in buf.iter_mut().zip(bytes[..read].chunks_exact(2)) {
                        *slot = level(i16::from_le_bytes([pair[0], pair[1]]), gain);
                    }
                    read / 2
                }
                Err(err) => {
                    failed = Some(err);
                    0
                }
            }
        });
        match failed {
            Some(err) => {
                let _ = std::fs::remove_file(path);
                Err(keeping_failed(&err))
            }
            None => encoded,
        }
    }
}

/// The error of a recording that could not be kept on disk.
fn keeping_failed(err: &std::io::Error) -> Error {
    Error::Memo(format!(
        "the recording could not be kept in the temporary folder: {err}"
    ))
}

/// A new file for a recording in the temporary folder, open for writing it
/// and reading it back, and deleted when it is closed.
fn scratch_file() -> std::io::Result<File> {
    let dir = std::env::temp_dir();
    let mut number = 0u32;
    loop {
        let path = dir.join(format!(
            "Vinput recording {} {number}.pcm",
            std::process::id()
        ));
        match platform::open_scratch(&path) {
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists && number < 100 => {
                number += 1;
            }
            opened => return opened,
        }
    }
}

/// Reads into `bytes` until it is full or the file has ended, and returns
/// how many bytes were read.
fn fill(file: &mut impl Read, bytes: &mut [u8]) -> std::io::Result<usize> {
    let mut read = 0;
    while read < bytes.len() {
        match file.read(&mut bytes[read..]) {
            Ok(0) => break,
            Ok(count) => read += count,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(read)
}

/// The level of a recording, measured frame by frame as it comes.
struct Levels {
    /// Samples in a frame.
    frame: usize,
    /// The energy of the frame being measured, and how many samples it has.
    energy: f32,
    count: usize,
    /// The level of each whole frame so far.
    frames: Vec<f32>,
}

impl Levels {
    fn new(rate: u32) -> Self {
        Self {
            frame: (rate / FRAMES_PER_SECOND).max(1) as usize,
            energy: 0.0,
            count: 0,
            frames: Vec::new(),
        }
    }

    fn add(&mut self, samples: &[i16]) {
        for &sample in samples {
            self.energy += f32::from(sample) * f32::from(sample);
            self.count += 1;
            if self.count == self.frame {
                #[expect(clippy::cast_precision_loss, reason = "a frame is under 1000 samples")]
                let mean = self.energy / self.frame as f32;
                self.frames.push(mean.sqrt());
                self.energy = 0.0;
                self.count = 0;
            }
        }
    }

    /// How much louder the recording is to be made: never softer, and as
    /// much as brings its louder moments to [`VOICE_LEVEL`].
    ///
    /// The measure is the level that one frame in ten is above, which in
    /// speech is the level of the voice and not that of one loud click.
    fn gain(mut self) -> f32 {
        self.frames.sort_by(f32::total_cmp);
        let voice = self
            .frames
            .get(self.frames.len() * 9 / 10)
            .copied()
            .unwrap_or(0.0);
        (VOICE_LEVEL / voice.max(1.0)).clamp(1.0, MOST_GAIN)
    }
}

/// `sample` made `gain` times louder, and what that pushes past [`KNEE`],
/// such as a click, eased in under full scale.
fn level(sample: i16, gain: f32) -> i16 {
    let room = f32::from(i16::MAX) - KNEE;
    let loud = f32::from(sample) * gain;
    let eased = if loud.abs() > KNEE {
        (KNEE + room * ((loud.abs() - KNEE) / room).tanh()).copysign(loud)
    } else {
        loud
    };
    #[expect(clippy::cast_possible_truncation, reason = "eased in under full scale")]
    let sample = eased.round() as i16;
    sample
}

/// `samples` at `rate` as loud as a voice memo should be: made louder,
/// never softer, until the louder moments of the recording are at
/// [`VOICE_LEVEL`]. A recording is leveled this way as it is saved.
#[must_use]
pub fn leveled(samples: &[i16], rate: u32) -> Vec<i16> {
    let mut levels = Levels::new(rate);
    levels.add(samples);
    let gain = levels.gain();
    samples.iter().map(|&sample| level(sample, gain)).collect()
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

/// Encodes mono 16-bit samples at `rate` to an MP3 file at an exact path
/// as a memo is saved, for trying out what a recording comes to.
///
/// # Errors
///
/// Returns [`Error::Memo`] if the encoder fails.
pub fn encode_to(samples: &[i16], rate: u32, path: &Path) -> Result<()> {
    let mut rest = &leveled(samples, rate)[..];
    platform::encode(rate, path, &mut |buf| {
        let count = buf.len().min(rest.len());
        buf[..count].copy_from_slice(&rest[..count]);
        rest = &rest[count..];
        count
    })
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod platform {
    use std::path::{Path, PathBuf};
    use std::ptr::null_mut;

    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    use windows_sys::Win32::UI::Shell::{FOLDERID_Documents, SHGetKnownFolderPath};

    pub(super) use super::clipboard::put_file;
    pub(super) use super::mp3::encode;

    /// Opens a new file that Windows deletes when it is closed, however the
    /// process ends, and that it keeps in memory while it can.
    pub(super) fn open_scratch(path: &Path) -> std::io::Result<std::fs::File> {
        use std::os::windows::fs::OpenOptionsExt;
        /// `FILE_ATTRIBUTE_TEMPORARY` and `FILE_FLAG_DELETE_ON_CLOSE`.
        const FILE_ATTRIBUTE_TEMPORARY: u32 = 0x100;
        const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .attributes(FILE_ATTRIBUTE_TEMPORARY)
            .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
            .open(path)
    }

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

    pub(super) fn encode(
        _rate: u32,
        _path: &Path,
        _next: &mut dyn FnMut(&mut [i16]) -> usize,
    ) -> Result<()> {
        Err(Error::Memo("voice memos need Windows".into()))
    }

    /// Opens a new file that is gone once it is closed.
    pub(super) fn open_scratch(path: &Path) -> std::io::Result<std::fs::File> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)?;
        std::fs::remove_file(path)?;
        Ok(file)
    }

    pub(super) fn put_file(_path: &Path) -> Result<()> {
        Err(Error::Memo("voice memos need Windows".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::SAMPLE_RATE_HZ;

    #[test]
    fn memos_are_named_after_the_moment_they_were_recorded() {
        assert_eq!(
            file_name([2026, 10, 5, 8, 4, 9]),
            "Voice memo 2026-10-05 08.04.09.mp3"
        );
    }

    /// A 440 Hz tone of `millis` at `rate` whose level, the root of its
    /// mean square, is `level`.
    fn tone(rate: u32, millis: u32, level: f32) -> Vec<i16> {
        #[expect(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
        (0..rate * millis / 1000)
            .map(|n| {
                let phase = n as f32 * 440.0 * std::f32::consts::TAU / rate as f32;
                (phase.sin() * level * std::f32::consts::SQRT_2) as i16
            })
            .collect()
    }

    fn loudness(samples: &[i16]) -> f32 {
        let energy: f32 = samples.iter().map(|&s| f32::from(s) * f32::from(s)).sum();
        #[expect(clippy::cast_precision_loss, reason = "a level, to compare")]
        let mean = energy / samples.len().max(1) as f32;
        mean.sqrt()
    }

    #[test]
    fn a_memo_from_a_soft_microphone_is_as_loud_as_any() {
        for rate in [SAMPLE_RATE_HZ, 44_100, 48_000] {
            // A voice at -38 dB, as a microphone set low records it, between
            // two stretches of the room, with the ready sound at the start.
            let soft = [
                tone(rate, 100, 1300.0),
                tone(rate, 500, 5.0),
                tone(rate, 2000, 400.0),
                tone(rate, 400, 5.0),
            ]
            .concat();
            let memo = leveled(&soft, rate);
            assert_eq!(memo.len(), soft.len());
            let second = rate as usize;
            let voice = loudness(&memo[second..2 * second]);
            assert!((3000.0..3500.0).contains(&voice), "{voice} at {rate} Hz");
            // A voice that is loud enough is left as it is, and so is silence.
            let loud = tone(rate, 2000, 6000.0);
            assert!(leveled(&loud, rate) == loud);
            let silence = vec![0; second];
            assert!(leveled(&silence, rate) == silence);
            // The hiss of a microphone that heard nothing is not made a roar.
            let hiss = loudness(&leveled(&tone(rate, 2000, 10.0), rate));
            assert!(hiss < 400.0, "{hiss}");
            // What the gain pushes over the top is eased in, not wrapped around.
            let click = [
                tone(rate, 2000, 400.0),
                vec![i16::MAX, i16::MIN, 20_000, -20_000],
            ]
            .concat();
            let memo = leveled(&click, rate);
            let end = &memo[memo.len() - 4..];
            assert!(end[0] > 30_000 && end[1] < -30_000 && end[2] > 30_000 && end[3] < -30_000);
        }
    }

    /// What a recording of `samples` at `rate` hands its encoder, `chunk`
    /// samples at a time.
    fn recorded(samples: &[i16], rate: u32, chunk: usize) -> Result<Vec<i16>> {
        let mut recording = Recording::new(rate)?;
        for piece in samples.chunks(1234) {
            recording.push(piece)?;
        }
        let mut encoded = Vec::new();
        recording.encode_with(Path::new("memo.mp3"), |at, _, next| {
            assert_eq!(at, rate);
            let mut buf = vec![0; chunk];
            loop {
                let read = next(&mut buf);
                if read == 0 {
                    return Ok(());
                }
                encoded.extend_from_slice(&buf[..read]);
            }
        })?;
        Ok(encoded)
    }

    #[test]
    fn a_recording_is_kept_on_disk_and_comes_back_leveled() -> Result<()> {
        let rate = 48_000;
        let soft = [tone(rate, 500, 5.0), tone(rate, 2000, 400.0)].concat();
        let encoded = recorded(&soft, rate, rate as usize)?;
        assert!(encoded == leveled(&soft, rate));
        // However it is read.
        assert!(recorded(&soft, rate, 7)? == encoded);
        Ok(())
    }

    #[test]
    fn a_recording_ends_after_an_hour_and_a_slip_is_no_memo() -> Result<()> {
        // At ten samples a second an hour is 36 000 of them.
        let mut recording = Recording::new(10)?;
        assert!(recording.is_too_short());
        recording.push(&[1; 3])?;
        assert!(!recording.is_too_short());
        recording.push(&vec![1; 40_000])?;
        assert!(recording.is_full());
        assert!((recording.seconds() - 3600.0).abs() < 0.01);
        let mut ended = 0;
        recording.encode_with(Path::new("memo.mp3"), |_, _, next| {
            let mut buf = [0; 1000];
            loop {
                match next(&mut buf) {
                    0 => return Ok(()),
                    read => ended += read,
                }
            }
        })?;
        assert_eq!(ended, 36_000);
        Ok(())
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

    /// The frames of `mp3`, if it is frames of MPEG-1 layer III from its
    /// first byte to its last: how many, and the bit rate and sample rate
    /// of the first.
    #[cfg(windows)]
    fn frames(mut mp3: &[u8]) -> Option<(usize, u32, u32)> {
        const KBITS: [u32; 15] = [
            0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
        ];
        const RATES: [u32; 3] = [44_100, 48_000, 32_000];
        let mut count = 0;
        let mut first = None;
        while let [0xFF, 0xFA | 0xFB, rates, ..] = mp3 {
            let kbits = *KBITS.get(usize::from(rates >> 4))?;
            let rate = *RATES.get(usize::from(rates >> 2 & 3))?;
            if kbits == 0 {
                return None;
            }
            let length = 144 * kbits as usize * 1000 / rate as usize + usize::from(rates >> 1 & 1);
            mp3 = mp3.get(length..)?;
            count += 1;
            first.get_or_insert((kbits * 1000, rate));
        }
        let (bits, rate) = first?;
        mp3.is_empty().then_some((count, bits, rate))
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
    fn a_recording_becomes_a_playable_mp3_at_the_rate_it_was_made() -> std::io::Result<()> {
        let dir = std::env::temp_dir().join(format!("vinput-mp3-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        // The rates of microphones, and the MP3 each becomes.
        for (rate, mp3_rate) in [(SAMPLE_RATE_HZ, 32_000), (44_100, 44_100), (48_000, 48_000)] {
            let path = dir.join(format!("tone {rate}.mp3"));
            // Two seconds of a 440 Hz tone.
            let samples = tone(rate, 2000, 5600.0);
            let encoded = encode_to(&samples, rate, &path);
            if encoded.is_err() && std::env::var_os("CI").is_some() {
                // Server editions of Windows can come without the media features.
                eprintln!("skipped: {encoded:?}");
                return std::fs::remove_dir_all(dir);
            }
            assert!(encoded.is_ok(), "{encoded:?}");
            let bytes = std::fs::read(&path)?;
            // Nothing but audio: frames from the first byte to the last, two
            // seconds of them and what the encoder needs to start and to
            // end. No tag in front, which Windows fills in so that some
            // players play nothing at all.
            let frames = frames(&bytes);
            assert!(frames.is_some(), "not frames throughout at {rate} Hz");
            let (count, bits, at) = frames.unwrap_or_default();
            assert_eq!(at, mp3_rate);
            assert!(bits >= 128_000, "{bits} bit/s at {rate} Hz");
            let millis = count * 1152 * 1000 / at as usize;
            assert!((2000..2200).contains(&millis), "{millis} ms at {rate} Hz");
            assert!(!bytes.windows(8).any(|tag| tag == b"iTunSMPB"));
        }
        std::fs::remove_dir_all(dir)
    }
}
