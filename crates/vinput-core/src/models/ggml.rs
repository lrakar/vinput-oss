//! Telling a Whisper model in the ggml format, the one the engine runs, from
//! every other kind of model file by its first bytes. A file that is not one
//! is refused with the reason before it is downloaded or used, instead of
//! failing later inside the engine.

use std::fmt;
use std::io::Read;
use std::path::Path;

/// How many bytes from the start of a file [`inspect`] looks at.
pub const HEADER_BYTES: usize = 48;

/// `ggml` as the little-endian number that opens a ggml file.
const GGML: [u8; 4] = *b"lmgg";
/// The magic numbers of the later ggml formats used by language models.
const OTHER_GGML: [[u8; 4]; 2] = [*b"fmgg", *b"tjgg"];

/// What the header of a Whisper model says about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Whisper {
    /// Words the model knows: 51864 for the English-only models.
    pub vocabulary: u32,
    /// Width of its audio encoder, which tells the size apart.
    pub audio_width: u32,
    /// Layers of its text decoder: 4 for the turbo models, 2 for distilled ones.
    pub text_layers: u32,
}

impl Whisper {
    /// Whether it only recognizes English, like the `.en` models.
    #[must_use]
    pub fn english_only(&self) -> bool {
        self.vocabulary == 51_864
    }

    /// The size it was trained at, as Whisper names them.
    #[must_use]
    pub fn size(&self) -> Option<&'static str> {
        Some(match self.audio_width {
            384 => "tiny",
            512 => "base",
            768 => "small",
            1024 => "medium",
            1280 if self.text_layers <= 4 => "large turbo",
            1280 => "large",
            _ => return None,
        })
    }

    /// What the list of models says about it: its size and languages.
    #[must_use]
    pub fn describe(&self) -> String {
        let languages = if self.english_only() {
            "English only"
        } else {
            "99 languages"
        };
        match self.size() {
            Some(size) => format!("Whisper {size}, {languages}"),
            None => format!("Whisper, {languages}"),
        }
    }
}

/// Why a file is not a model Vinput can run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsupported {
    /// A GGUF file, the format of language models and of newer conversions.
    Gguf,
    /// A checkpoint of `PyTorch`.
    PyTorch,
    /// A safetensors file.
    Safetensors,
    /// A ggml file of something other than Whisper.
    OtherGgml,
    /// Anything else.
    Unknown,
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Gguf => "This is a GGUF file. VinPUT runs Whisper models in ggml (.bin).",
            Self::PyTorch => "This is a PyTorch model. VinPUT needs it converted to ggml (.bin).",
            Self::Safetensors => "This is a safetensors model. VinPUT needs it in ggml (.bin).",
            Self::OtherGgml => "This ggml file is not a Whisper speech model.",
            Self::Unknown => "This is not a Whisper model in ggml format (.bin).",
        })
    }
}

/// Reads what the first bytes of a file say about it.
///
/// # Errors
///
/// Returns why the file is not a Whisper model in the ggml format.
pub fn inspect(header: &[u8]) -> Result<Whisper, Unsupported> {
    let magic = header.get(..4).ok_or(Unsupported::Unknown)?;
    if magic == b"GGUF" {
        return Err(Unsupported::Gguf);
    }
    // A PyTorch checkpoint is a zip archive, or a pickle in older versions.
    if magic == b"PK\x03\x04" || magic.starts_with(&[0x80, 0x02]) {
        return Err(Unsupported::PyTorch);
    }
    // A safetensors file opens with the length of its JSON header, then the header.
    if header.len() > 9 && header[8] == b'{' && header[..8].iter().skip(5).all(|byte| *byte == 0) {
        return Err(Unsupported::Safetensors);
    }
    if OTHER_GGML.iter().any(|other| magic == other) {
        return Err(Unsupported::OtherGgml);
    }
    if magic != GGML {
        return Err(Unsupported::Unknown);
    }
    // Eleven numbers follow: vocabulary, audio context, width, heads and
    // layers, text context, width, heads and layers, mel bands, number type.
    let field = |index: usize| {
        let at = 4 + 4 * index;
        header
            .get(at..at + 4)
            .and_then(|bytes| bytes.try_into().ok())
            .map(u32::from_le_bytes)
    };
    let fields: Option<Vec<u32>> = (0..11).map(field).collect();
    let fields = fields.ok_or(Unsupported::Unknown)?;
    let [
        vocabulary,
        audio_context,
        audio_width,
        _,
        audio_layers,
        text_context,
        _,
        _,
        text_layers,
        mels,
        _,
    ] = fields[..]
    else {
        return Err(Unsupported::Unknown);
    };
    let whisper = (51_864..=51_866).contains(&vocabulary)
        && audio_context == 1500
        && text_context == 448
        && (mels == 80 || mels == 128)
        && (1..=64).contains(&audio_layers)
        && (1..=64).contains(&text_layers);
    if !whisper {
        return Err(Unsupported::OtherGgml);
    }
    Ok(Whisper {
        vocabulary,
        audio_width,
        text_layers,
    })
}

/// Reads the start of the file at `path` and inspects it.
///
/// # Errors
///
/// Returns why the file is not a Whisper model, or [`Unsupported::Unknown`]
/// if it cannot be read.
pub fn inspect_file(path: &Path) -> Result<Whisper, Unsupported> {
    let mut header = Vec::with_capacity(HEADER_BYTES);
    std::fs::File::open(path)
        .and_then(|file| file.take(HEADER_BYTES as u64).read_to_end(&mut header))
        .map_err(|_| Unsupported::Unknown)?;
    inspect(&header)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The header of a ggml Whisper model with these numbers.
    pub(crate) fn whisper_header(vocabulary: u32, width: u32, text_layers: u32) -> Vec<u8> {
        let numbers = [
            vocabulary,
            1500,
            width,
            8,
            12,
            448,
            width,
            8,
            text_layers,
            80,
            1,
        ];
        let mut header = GGML.to_vec();
        for number in numbers {
            header.extend_from_slice(&number.to_le_bytes());
        }
        header
    }

    #[test]
    fn whisper_models_are_recognized_with_their_size_and_languages() {
        let base = inspect(&whisper_header(51_865, 512, 6));
        assert_eq!(
            base.map(|model| model.describe()).as_deref(),
            Ok("Whisper base, 99 languages")
        );
        let english = inspect(&whisper_header(51_864, 1024, 24));
        assert_eq!(
            english.map(|model| model.describe()).as_deref(),
            Ok("Whisper medium, English only")
        );
        let turbo = inspect(&whisper_header(51_866, 1280, 4));
        assert_eq!(
            turbo.ok().and_then(|model| model.size()),
            Some("large turbo")
        );
        assert_eq!(whisper_header(51_865, 512, 6).len(), HEADER_BYTES);
    }

    #[test]
    fn other_formats_are_refused_with_their_name() {
        let mut gguf = b"GGUF".to_vec();
        gguf.extend_from_slice(&[3, 0, 0, 0]);
        assert_eq!(inspect(&gguf), Err(Unsupported::Gguf));
        assert_eq!(inspect(b"PK\x03\x04archive"), Err(Unsupported::PyTorch));
        assert_eq!(
            inspect(&[0x80, 0x02, 0x8a, 0x0a]),
            Err(Unsupported::PyTorch)
        );
        let mut safetensors = 1234u64.to_le_bytes().to_vec();
        safetensors.extend_from_slice(br#"{"__metadata__":{}}"#);
        assert_eq!(inspect(&safetensors), Err(Unsupported::Safetensors));
        assert_eq!(inspect(b"tjggllama"), Err(Unsupported::OtherGgml));
        assert_eq!(inspect(b"<!DOCTYPE html>"), Err(Unsupported::Unknown));
        assert_eq!(inspect(b"lm"), Err(Unsupported::Unknown));
        // The right magic but cut short.
        assert_eq!(
            inspect(&whisper_header(51_865, 512, 6)[..20]),
            Err(Unsupported::Unknown)
        );
        // The right magic, but not the numbers of a Whisper model.
        assert_eq!(
            inspect(&whisper_header(32_000, 4096, 32)),
            Err(Unsupported::OtherGgml)
        );
    }

    #[test]
    fn every_refusal_says_what_to_do_in_one_line() {
        for reason in [
            Unsupported::Gguf,
            Unsupported::PyTorch,
            Unsupported::Safetensors,
            Unsupported::OtherGgml,
            Unsupported::Unknown,
        ] {
            let text = reason.to_string();
            assert!(!text.contains('\n') && text.len() <= 70, "{text}");
        }
    }
}
