//! MP3 encoding with the encoder that is part of Windows (Media Foundation).
//!
//! Using the system encoder keeps an MP3 library out of the Vinput executable
//! and leaves its licensing with Windows.
//!
//! Windows puts a tag in front of the audio that says, for players that join
//! tracks without a gap, how many samples at the start and at the end are
//! not part of the recording. Its count for the end is wrong whenever the
//! encoder changed the sample rate, as it does for a microphone at 16 kHz:
//! a number near four thousand million, where some two thousand are meant.
//! A player that goes by the tag has nothing left to play. The players of
//! Windows do not go by it (a test here plays a memo with their decoder),
//! but a memo is made to be sent to someone else. So the tag is taken off
//! again ([`without_tag`]): an MP3 needs none, and without it every player
//! plays what is in the file.
#![allow(unsafe_code)]

use std::path::Path;

use windows::Win32::Media::MediaFoundation::{
    IMFMediaType, IMFSinkWriter, MF_MT_ALL_SAMPLES_INDEPENDENT, MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
    MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_BLOCK_ALIGNMENT, MF_MT_AUDIO_NUM_CHANNELS,
    MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_VERSION, MFAudioFormat_MP3,
    MFAudioFormat_PCM, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
    MFCreateSinkWriterFromURL, MFMediaType_Audio, MFSTARTUP_LITE, MFShutdown, MFStartup,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::core::HSTRING;
use windows_sys::Win32::System::LibraryLoader::{LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};

use crate::{Error, Result};

/// The sample rates of MPEG-1 layer III, the ones the system encoder takes.
const MP3_RATES_HZ: [u32; 3] = [32_000, 44_100, 48_000];
/// The bit rates a memo is encoded at, in bytes per second, the best that
/// the encoder takes. 128 kbit/s is the most the encoder of Windows does for
/// one channel, at 32, 44.1 and 48 kHz alike, and a voice recorded at 48 kHz
/// sounds at it as it did. 64 kbit/s, which memos used to have, is there for
/// an encoder that will not.
const BYTES_PER_SECOND: [u32; 2] = [16_000, 8_000];
/// Media Foundation counts time in units of 100 nanoseconds.
const TICKS_PER_SECOND: i64 = 10_000_000;

/// The sample rate of the MP3 of a recording at `rate`: the same if MP3 has
/// it, otherwise the next one up, to which Windows converts the recording.
pub(super) fn mp3_rate(rate: u32) -> u32 {
    MP3_RATES_HZ
        .into_iter()
        .find(|&mp3| mp3 >= rate)
        .unwrap_or(MP3_RATES_HZ[2])
}

/// Encodes mono 16-bit samples at `rate` into an MP3 file at `path`.
/// `next` fills the buffer it is given with the next samples and says how
/// many; 0 is the end.
///
/// # Errors
///
/// Returns [`Error::Memo`] if the encoder is not available (Windows editions
/// without the media features) or the file cannot be written. A partial file
/// is removed.
pub(super) fn encode(
    rate: u32,
    path: &Path,
    next: &mut dyn FnMut(&mut [i16]) -> usize,
) -> Result<()> {
    let tidied = encode_tagged(rate, path, next).and_then(|()| {
        let file = std::fs::read(path).map_err(|err| err.to_string())?;
        let audio = without_tag(&file);
        if audio.is_empty() {
            return Err("encoding the MP3 failed: no audio came out".to_owned());
        }
        if audio.len() < file.len() {
            std::fs::write(path, audio).map_err(|err| err.to_string())?;
        }
        Ok(())
    });
    tidied.map_err(|err| {
        let _ = std::fs::remove_file(path);
        Error::Memo(err)
    })
}

/// Has Windows encode what `next` gives into an MP3 file at `path`, tag
/// and all.
fn encode_tagged(
    rate: u32,
    path: &Path,
    next: &mut dyn FnMut(&mut [i16]) -> usize,
) -> std::result::Result<(), String> {
    if !encoder_present() {
        return Err(
            "this edition of Windows has no MP3 encoder; install the Media Feature Pack".into(),
        );
    }
    // SAFETY: COM and Media Foundation are started for this call and shut down
    // again in reverse order, each only if its start succeeded. Every COM
    // object created in between is released before the shutdown.
    let written = unsafe {
        let com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let result = MFStartup(MF_VERSION, MFSTARTUP_LITE).and_then(|()| {
            let result = write(rate, path, next);
            let _ = MFShutdown();
            result
        });
        if com {
            CoUninitialize();
        }
        result
    };
    written.map_err(|err| format!("encoding the MP3 failed: {}", err.message()))
}

/// The audio of an MP3 file: `file` without the `ID3v2` tag in front of it,
/// if it has one.
pub(super) fn without_tag(file: &[u8]) -> &[u8] {
    let [b'I', b'D', b'3', _, _, flags, size @ ..] = file else {
        return file;
    };
    let Some(size) = size.get(..4) else {
        return file;
    };
    // Four bytes of seven bits each, and ten more if the tag has a footer.
    let length = size.iter().fold(0usize, |length, byte| {
        (length << 7) | usize::from(byte & 0x7F)
    });
    let footer = if flags & 0x10 == 0 { 0 } else { 10 };
    file.get(10 + length + footer..).unwrap_or_default()
}

/// Whether the Media Foundation libraries are on this PC. The N and KN
/// editions of Windows come without them.
///
/// The executable loads them at the first call instead of at start, and that
/// first call ends the process if one is missing, so this looks first.
fn encoder_present() -> bool {
    ["mfplat.dll", "mfreadwrite.dll"].iter().all(|library| {
        let name: Vec<u16> = library.encode_utf16().chain([0]).collect();
        // SAFETY: `name` is a NUL-terminated string that outlives the call. The
        // library is looked for in the system folder only, and stays loaded for
        // the calls that follow.
        let loaded = unsafe {
            LoadLibraryExW(
                name.as_ptr(),
                std::ptr::null_mut(),
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        };
        !loaded.is_null()
    })
}

/// An audio type with the fields PCM and MP3 share.
unsafe fn audio_type(
    format: &windows::core::GUID,
    rate: u32,
    bytes_per_second: u32,
) -> windows::core::Result<IMFMediaType> {
    // SAFETY: plain attribute setters on an object created here.
    unsafe {
        let media_type = MFCreateMediaType()?;
        media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        media_type.SetGUID(&MF_MT_SUBTYPE, format)?;
        media_type.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 1)?;
        media_type.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate)?;
        media_type.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, bytes_per_second)?;
        Ok(media_type)
    }
}

/// A writer of an MP3 file at `path` from mono 16-bit PCM at `rate`, at
/// the best of [`BYTES_PER_SECOND`] the encoder takes, and its stream.
///
/// # Safety
///
/// COM and Media Foundation must be started.
unsafe fn writer(rate: u32, path: &Path) -> windows::core::Result<(IMFSinkWriter, u32)> {
    // SAFETY: as the caller promises.
    unsafe {
        let input = audio_type(&MFAudioFormat_PCM, rate, rate * 2)?;
        input.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        input.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 2)?;
        input.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
        let mut refused = None;
        for bytes_per_second in BYTES_PER_SECOND {
            let tried = (|| {
                let writer: IMFSinkWriter =
                    MFCreateSinkWriterFromURL(&HSTRING::from(path.as_os_str()), None, None)?;
                let output = audio_type(&MFAudioFormat_MP3, mp3_rate(rate), bytes_per_second)?;
                let stream = writer.AddStream(&output)?;
                writer.SetInputMediaType(stream, &input, None)?;
                Ok((writer, stream))
            })();
            match tried {
                Ok(writer) => return Ok(writer),
                Err(err) => refused = Some(err),
            }
        }
        Err(refused.unwrap_or_else(windows::core::Error::empty))
    }
}

unsafe fn write(
    rate: u32,
    path: &Path,
    next: &mut dyn FnMut(&mut [i16]) -> usize,
) -> windows::core::Result<()> {
    // SAFETY: the caller has started COM and Media Foundation. Buffers are
    // locked only for the copy, which stays within the length they were
    // created with.
    unsafe {
        let (writer, stream) = writer(rate, path)?;
        writer.BeginWriting()?;

        // Handed to the encoder a second at a time.
        let mut samples = vec![0i16; rate as usize];
        let mut position = 0i64;
        loop {
            let read = next(&mut samples).min(samples.len());
            if read == 0 {
                break;
            }
            let chunk = &samples[..read];
            let bytes = u32::try_from(chunk.len() * 2).unwrap_or(0);
            let buffer = MFCreateMemoryBuffer(bytes)?;
            let mut data = std::ptr::null_mut();
            buffer.Lock(&raw mut data, None, None)?;
            std::ptr::copy_nonoverlapping(chunk.as_ptr().cast::<u8>(), data, bytes as usize);
            buffer.Unlock()?;
            buffer.SetCurrentLength(bytes)?;

            let duration =
                i64::try_from(chunk.len()).unwrap_or(0) * TICKS_PER_SECOND / i64::from(rate);
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(position)?;
            sample.SetSampleDuration(duration)?;
            writer.WriteSample(stream, &sample)?;
            position += duration;
        }
        writer.Finalize()
    }
}

#[cfg(test)]
mod tests {
    use windows::Win32::Media::MediaFoundation::{
        IMFSourceReader, MF_SOURCE_READER_FIRST_AUDIO_STREAM, MF_SOURCE_READERF_ENDOFSTREAM,
        MFCreateSourceReaderFromURL,
    };

    use super::*;

    use crate::audio::SAMPLE_RATE_HZ;

    /// Decodes an MP3 file with the decoder of Windows, the one its own
    /// players use, to mono 16-bit samples at `rate`, the rate of the file.
    fn decode(path: &Path, rate: u32) -> windows::core::Result<Vec<i16>> {
        // The reader's name for the first audio stream, as the bits it takes.
        let stream = u32::from_ne_bytes(MF_SOURCE_READER_FIRST_AUDIO_STREAM.0.to_ne_bytes());
        let ended = u32::from_ne_bytes(MF_SOURCE_READERF_ENDOFSTREAM.0.to_ne_bytes());
        // SAFETY: as in `encode`: COM and Media Foundation are started for
        // the call and every object is released before they are shut down;
        // a buffer is read only while locked, for the length it reports.
        unsafe {
            let com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
            MFStartup(MF_VERSION, MFSTARTUP_LITE)?;
            let decoded = (|| {
                let reader: IMFSourceReader =
                    MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), None)?;
                let pcm = MFCreateMediaType()?;
                pcm.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
                pcm.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
                pcm.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
                pcm.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 1)?;
                pcm.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate)?;
                reader.SetCurrentMediaType(stream, None, &pcm)?;
                let mut samples = Vec::new();
                loop {
                    let mut flags = 0;
                    let mut sample = None;
                    reader.ReadSample(
                        stream,
                        0,
                        None,
                        Some(&raw mut flags),
                        None,
                        Some(&raw mut sample),
                    )?;
                    if let Some(sample) = sample {
                        let buffer = sample.ConvertToContiguousBuffer()?;
                        let mut data = std::ptr::null_mut();
                        let mut length = 0;
                        buffer.Lock(&raw mut data, None, Some(&raw mut length))?;
                        let bytes = std::slice::from_raw_parts(data, length as usize);
                        samples.extend(
                            bytes
                                .chunks_exact(2)
                                .map(|pair| i16::from_le_bytes([pair[0], pair[1]])),
                        );
                        buffer.Unlock()?;
                    }
                    if flags & ended != 0 {
                        return Ok(samples);
                    }
                }
            })();
            let _ = MFShutdown();
            if com {
                CoUninitialize();
            }
            decoded
        }
    }

    /// How loud `samples` are: the root of the mean square.
    #[expect(clippy::cast_precision_loss, reason = "a level, to compare")]
    fn level(samples: &[i16]) -> f64 {
        let energy: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
        (energy / samples.len().max(1) as f64).sqrt()
    }

    /// A recorded sentence, as a memo would hold it.
    fn recording() -> Vec<i16> {
        let wav: &[u8] = include_bytes!("../stt/local/clips/en.wav");
        wav.get(44..)
            .unwrap_or_default()
            .chunks_exact(2)
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
            .collect()
    }

    #[test]
    fn a_memo_is_an_mp3_at_the_rate_of_its_microphone_or_the_next_one_up() {
        assert_eq!(mp3_rate(48_000), 48_000);
        assert_eq!(mp3_rate(44_100), 44_100);
        assert_eq!(mp3_rate(32_000), 32_000);
        // Headsets and the speech rate.
        assert_eq!(mp3_rate(SAMPLE_RATE_HZ), 32_000);
        assert_eq!(mp3_rate(8_000), 32_000);
        // Nothing records a memo above 48 kHz, but the MP3 would hold it.
        assert_eq!(mp3_rate(96_000), 48_000);
    }

    #[test]
    fn a_memo_plays_in_full_in_the_players_of_windows() -> std::result::Result<(), String> {
        let dir = std::env::temp_dir().join(format!("vinput-mp3-play-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
        let recording = recording();
        let path = dir.join("memo.mp3");
        let mut rest = &recording[..];
        let encoded = encode(SAMPLE_RATE_HZ, &path, &mut |buf| {
            let count = buf.len().min(rest.len());
            buf[..count].copy_from_slice(&rest[..count]);
            rest = &rest[count..];
            count
        });
        if encoded.is_err() && std::env::var_os("CI").is_some() {
            // Server editions of Windows can come without the media features.
            eprintln!("skipped: {encoded:?}");
            return std::fs::remove_dir_all(dir).map_err(|err| err.to_string());
        }
        encoded.map_err(|err| err.to_string())?;
        let mp3 = mp3_rate(SAMPLE_RATE_HZ);
        let played = decode(&path, mp3).map_err(|err| err.message())?;

        // As long as the recording, give or take the frame the encoder
        // needs to start and the one it ends in, and as loud.
        let rate = f64::from(mp3 / SAMPLE_RATE_HZ);
        #[expect(clippy::cast_precision_loss, reason = "a length, to compare")]
        let (length, expected) = (played.len() as f64, recording.len() as f64 * rate);
        assert!(
            (expected..expected + 3.0 * 1152.0).contains(&length),
            "{length} samples for {expected}"
        );
        let (loud, expected) = (level(&played), level(&recording));
        assert!(
            (expected * 0.8..expected * 1.2).contains(&loud),
            "level {loud} for {expected}"
        );
        std::fs::remove_dir_all(dir).map_err(|err| err.to_string())
    }
}
