//! Models people add themselves: a Whisper model in ggml format from Hugging
//! Face, or a file of one on this PC.
//!
//! Adding one takes two steps. [`choices`] finds the files a link or a path
//! offers, and [`add`] makes sure the one picked really is a Whisper model,
//! by its first bytes, before it joins the list. From then on it is a
//! [`Model`] like the built-in ones: installed by the same installer, into
//! `%LOCALAPPDATA%\Vinput\models\custom`, and run by the same engine.
//!
//! The list is kept in `%APPDATA%\Vinput\models.ini`. Each model on it is
//! made `'static` once, the first time this process sees it, so it can be
//! handed around like an entry of the built-in catalog; a model that is
//! removed keeps those few bytes until Vinput exits.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::ggml::{self, Whisper};
use super::hub::{self, Listing, RepoFile};
use super::{Download, Model, Runner};
use crate::download::Source;
use crate::settings::data_dir;
use crate::stt::ProviderKind;
use crate::{Error, Result, log};

/// What the id of every added model starts with; no built-in one does.
pub const PREFIX: &str = "custom-";
/// What the address of a file on this PC starts with.
pub const LOCAL: &str = "file:";
/// The folder under the models folder where added models are kept.
const FOLDER: &str = "custom";
/// File names that say nothing about the model, for which its repository
/// name is shown instead.
const PLAIN_NAMES: [&str; 3] = ["model", "ggml-model", "ggml_model"];

/// Where a file that can be added comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// A repository on Hugging Face, as `organization/name`.
    Hub(String),
    /// A file on this PC.
    Disk,
}

/// A file a link or a path offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// Its path in its repository, or its full path on this PC.
    pub path: String,
    /// Where it is downloaded from: pinned to a commit, or `file:` and a path.
    pub url: String,
    /// Its SHA-256, when Hugging Face knows it.
    pub sha256: Option<String>,
    /// Its size in bytes.
    pub bytes: u64,
    /// Where it comes from.
    pub origin: Origin,
    /// What its first bytes say, once they have been read.
    pub whisper: Option<Whisper>,
}

impl Choice {
    fn from_hub(listing: &Listing, file: &RepoFile) -> Self {
        Self {
            path: file.path.clone(),
            url: listing.address(file),
            sha256: file.sha256.clone(),
            bytes: file.bytes,
            origin: Origin::Hub(listing.repo.clone()),
            whisper: None,
        }
    }

    /// The name the list of models shows for it.
    #[must_use]
    pub fn name(&self) -> String {
        let file = self.path.rsplit(['/', '\\']).next().unwrap_or(&self.path);
        let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem).trim();
        let plain = PLAIN_NAMES.iter().any(|name| {
            stem.eq_ignore_ascii_case(name)
                || stem.to_ascii_lowercase().starts_with(&format!("{name}-"))
        });
        match &self.origin {
            Origin::Hub(repo) if plain || stem.is_empty() => {
                repo.rsplit('/').next().unwrap_or(repo).to_owned()
            }
            _ if stem.is_empty() => "Custom model".to_owned(),
            _ => stem.to_owned(),
        }
    }

    /// Whether this file is on the list already.
    #[must_use]
    pub fn is_added(&self) -> bool {
        locked()
            .entries
            .iter()
            .any(|(entry, _)| entry.url == self.url)
    }
}

/// The files that the text someone typed offers: one file on this PC, one
/// file of a repository, or every file of a repository that may be a ggml
/// model. Looks the repository up on Hugging Face, so it takes a moment.
///
/// # Errors
///
/// Returns [`Error::Install`] with a reason for the user if the text is no
/// such link or path, the file is not a Whisper model in ggml format, or the
/// repository offers none.
pub fn choices(text: &str) -> Result<Vec<Choice>> {
    let text = text.trim().trim_matches('"');
    let path = Path::new(text);
    if path.is_absolute() || path.is_file() {
        return on_disk(path).map(|choice| vec![choice]);
    }
    let link = hub::parse_link(text).map_err(|why| Error::Install(why.to_owned()))?;
    let listing = hub::list(&link)?;
    if let Some(wanted) = &link.file {
        let file = listing
            .find(wanted)
            .ok_or_else(|| Error::Install(format!("{} has no file {wanted}.", listing.repo)))?;
        return Ok(vec![Choice::from_hub(&listing, file)]);
    }
    let found: Vec<Choice> = listing
        .candidates()
        .into_iter()
        .map(|file| Choice::from_hub(&listing, file))
        .collect();
    if found.is_empty() {
        return Err(Error::Install(listing.why_none().to_owned()));
    }
    Ok(found)
}

/// A file on this PC, if it is a Whisper model in ggml format.
fn on_disk(path: &Path) -> Result<Choice> {
    let meta = std::fs::metadata(path)
        .ok()
        .filter(std::fs::Metadata::is_file)
        .ok_or_else(|| Error::Install("There is no such file on this PC.".into()))?;
    let whisper = ggml::inspect_file(path).map_err(|why| Error::Install(why.to_string()))?;
    let full = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let shown = full.to_string_lossy().into_owned();
    Ok(Choice {
        url: format!("{LOCAL}{shown}"),
        path: shown,
        sha256: None,
        bytes: meta.len(),
        origin: Origin::Disk,
        whisper: Some(whisper),
    })
}

/// Adds `choice` to the list of models, once its first bytes show that it is
/// a Whisper model in ggml format; a file that is on the list already is not
/// added twice. Reads those bytes from Hugging Face if they are not known
/// yet. Returns the model, ready to be installed.
///
/// # Errors
///
/// Returns [`Error::Install`] with a reason for the user if the file is not
/// a Whisper model, cannot be read, or the list cannot be saved.
pub fn add(choice: &Choice) -> Result<&'static Model> {
    if let Some((_, model)) = locked()
        .entries
        .iter()
        .find(|(entry, _)| entry.url == choice.url)
    {
        return Ok(model);
    }
    let whisper = if let Some(whisper) = choice.whisper {
        whisper
    } else {
        let header = hub::header(&choice.url)?;
        ggml::inspect(&header).map_err(|why| Error::Install(why.to_string()))?
    };
    let mut registry = locked();
    if let Some((_, model)) = registry
        .entries
        .iter()
        .find(|(entry, _)| entry.url == choice.url)
    {
        return Ok(model);
    }
    let name = choice.name();
    let slug = unique_slug(&slug_of(&name), |taken| {
        registry
            .entries
            .iter()
            .any(|(entry, _)| entry.id == format!("{PREFIX}{taken}"))
    });
    let entry = Entry {
        id: format!("{PREFIX}{slug}"),
        name,
        file: format!("{FOLDER}/{slug}.bin"),
        url: choice.url.clone(),
        sha256: choice.sha256.clone().unwrap_or_default(),
        bytes: choice.bytes,
        from: match &choice.origin {
            Origin::Hub(repo) => repo.clone(),
            Origin::Disk => String::new(),
        },
        english_only: whisper.english_only(),
        about: whisper.describe(),
    };
    let model = entry.intern();
    registry.entries.push((entry, model));
    if let Err(err) = registry.save() {
        registry.entries.pop();
        return Err(err);
    }
    Ok(model)
}

/// Every model on the list, in the order they were added.
#[must_use]
pub fn models() -> Vec<&'static Model> {
    locked().entries.iter().map(|(_, model)| *model).collect()
}

/// Takes a model off the list. Its file is the caller's to delete.
///
/// # Errors
///
/// Returns [`Error::Install`] if the list cannot be saved.
pub fn forget(id: &str) -> Result<()> {
    let mut registry = locked();
    let before = registry.entries.len();
    registry.entries.retain(|(entry, _)| entry.id != id);
    if registry.entries.len() == before {
        return Ok(());
    }
    registry.save()
}

/// One model on the list, as the file keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    id: String,
    name: String,
    /// Its file under the models folder.
    file: String,
    url: String,
    /// Empty when Hugging Face did not say.
    sha256: String,
    bytes: u64,
    /// The repository it came from; empty for a file from this PC.
    from: String,
    english_only: bool,
    /// Its size and languages.
    about: String,
}

impl Entry {
    /// The model this entry describes, made to last as long as Vinput runs.
    fn intern(&self) -> &'static Model {
        let leak = |text: String| -> &'static str { Box::leak(text.into_boxed_str()) };
        let summary = if self.from.is_empty() {
            format!("Added by you from this PC. {}.", self.about)
        } else {
            format!(
                "Added by you from huggingface.co/{}. {}.",
                self.from, self.about
            )
        };
        let detail = if self.english_only {
            "Custom, English only"
        } else {
            "Custom"
        };
        let file = leak(self.file.clone());
        Box::leak(Box::new(Model {
            id: leak(self.id.clone()),
            provider: ProviderKind::Local,
            name: leak(self.name.clone()),
            summary: leak(summary),
            detail,
            languages: if self.english_only { "en" } else { "" },
            local: Some((
                Download {
                    file,
                    source: Source {
                        url: leak(self.url.clone()),
                        sha256: leak(self.sha256.clone()),
                        bytes: self.bytes,
                    },
                },
                Runner::Server,
            )),
        }))
    }
}

/// Reads the list from the text of its file. Entries that are not whole are
/// left out, so a damaged file loses only them.
fn parse(text: &str) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut current: Option<Entry> = None;
    let finish = |entry: Option<Entry>, entries: &mut Vec<Entry>| {
        if let Some(entry) = entry.filter(Entry::is_whole) {
            entries.push(entry);
        }
    };
    for line in text.lines().map(str::trim) {
        if let Some(id) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            finish(current.take(), &mut entries);
            current = Some(Entry {
                id: id.trim().to_owned(),
                name: String::new(),
                file: String::new(),
                url: String::new(),
                sha256: String::new(),
                bytes: 0,
                from: String::new(),
                english_only: false,
                about: String::new(),
            });
            continue;
        }
        let (Some(entry), Some((key, value))) = (current.as_mut(), line.split_once('=')) else {
            continue;
        };
        let value = value.trim().to_owned();
        match key.trim() {
            "name" => entry.name = value,
            "file" => entry.file = value,
            "url" => entry.url = value,
            "sha256" => entry.sha256 = value,
            "bytes" => entry.bytes = value.parse().unwrap_or(0),
            "from" => entry.from = value,
            "english_only" => entry.english_only = value == "true",
            "about" => entry.about = value,
            _ => {}
        }
    }
    finish(current, &mut entries);
    entries
}

impl Entry {
    /// Whether it has everything a model needs, and nothing that could
    /// reach outside the models folder or come over plain HTTP.
    fn is_whole(&self) -> bool {
        let safe_file = self
            .file
            .strip_prefix(&format!("{FOLDER}/"))
            .is_some_and(|name| {
                !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
                    && !name.contains("..")
            });
        self.id.starts_with(PREFIX)
            && self.id.len() > PREFIX.len()
            && !self.name.is_empty()
            && safe_file
            && (self.url.starts_with("https://") || self.url.starts_with(LOCAL))
            && (self.sha256.is_empty() || self.sha256.len() == 64)
            && self.bytes > 0
    }
}

/// Renders the list in the format [`parse`] reads.
fn serialize(entries: &[Entry]) -> String {
    use std::fmt::Write as _;
    let mut text = String::from("# Speech models added in VinPUT settings.\n");
    for entry in entries {
        let _ = write!(
            text,
            "\n[{}]\nname = {}\nfile = {}\nurl = {}\nsha256 = {}\nbytes = {}\nfrom = {}\nenglish_only = {}\nabout = {}\n",
            entry.id,
            entry.name,
            entry.file,
            entry.url,
            entry.sha256,
            entry.bytes,
            entry.from,
            entry.english_only,
            entry.about,
        );
    }
    text
}

/// `name` as letters, digits and dashes, for an id and a file name.
fn slug_of(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches(['-', '.']).replace("..", ".");
    if slug.is_empty() {
        "model".to_owned()
    } else {
        slug.chars().take(48).collect()
    }
}

/// `slug`, or `slug-2`, `slug-3` and on if it is taken.
fn unique_slug(slug: &str, taken: impl Fn(&str) -> bool) -> String {
    let mut candidate = slug.to_owned();
    let mut number = 1u32;
    while taken(&candidate) {
        number += 1;
        candidate = format!("{slug}-{number}");
    }
    candidate
}

/// The list, read from its file the first time it is needed.
#[derive(Default)]
struct Registry {
    loaded: bool,
    entries: Vec<(Entry, &'static Model)>,
}

static REGISTRY: Mutex<Registry> = Mutex::new(Registry {
    loaded: false,
    entries: Vec::new(),
});

fn path() -> Option<PathBuf> {
    Some(data_dir()?.join("models.ini"))
}

fn locked() -> MutexGuard<'static, Registry> {
    let mut registry = REGISTRY.lock().unwrap_or_else(PoisonError::into_inner);
    if !registry.loaded {
        registry.loaded = true;
        let text = path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .unwrap_or_default();
        registry.entries = parse(&text)
            .into_iter()
            .map(|entry| {
                let model = entry.intern();
                (entry, model)
            })
            .collect();
    }
    registry
}

impl Registry {
    /// Writes the list to its file, replacing the old one only once the new
    /// one is complete.
    fn save(&self) -> Result<()> {
        let failed = |err: std::io::Error| {
            log::write(&format!("saving the list of added models: {err}"));
            Error::Install(format!("The list of models could not be saved: {err}"))
        };
        let path = path().ok_or_else(|| Error::Install("APPDATA is not set".into()))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(failed)?;
        }
        let entries: Vec<Entry> = self
            .entries
            .iter()
            .map(|(entry, _)| entry.clone())
            .collect();
        let fresh = path.with_extension("ini.new");
        std::fs::write(&fresh, serialize(&entries)).map_err(failed)?;
        std::fs::rename(&fresh, &path).map_err(failed)
    }
}

#[cfg(test)]
mod tests {
    use super::super::ggml::tests::whisper_header;
    use super::*;

    fn entry(id: &str) -> Entry {
        Entry {
            id: format!("{PREFIX}{id}"),
            name: "ggml-medium-sl".into(),
            file: format!("{FOLDER}/{id}.bin"),
            url: "https://huggingface.co/org/model/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-medium-sl.bin".into(),
            sha256: "6c14d5adee5f86394037b4e4e8b59f1673b6cee10e3cf0b11bbdbee79c156208".into(),
            bytes: 1_533_763_059,
            from: "org/model".into(),
            english_only: false,
            about: "Whisper medium, 99 languages".into(),
        }
    }

    #[test]
    fn the_list_survives_a_round_trip() {
        let entries = vec![
            entry("ggml-medium-sl"),
            Entry {
                url: r"file:C:\Users\me\Downloads\ggml-small.en.bin".into(),
                sha256: String::new(),
                from: String::new(),
                english_only: true,
                ..entry("ggml-small.en")
            },
        ];
        assert_eq!(parse(&serialize(&entries)), entries);
    }

    #[test]
    fn a_damaged_or_unsafe_entry_is_left_out() {
        let good = entry("good");
        let bad = [
            Entry {
                file: "../../evil.exe".into(),
                ..entry("a")
            },
            Entry {
                file: format!("{FOLDER}/..\\evil.bin"),
                ..entry("b")
            },
            Entry {
                url: "http://example.com/model.bin".into(),
                ..entry("c")
            },
            Entry {
                id: "whisper-base".into(),
                ..entry("d")
            },
            Entry {
                sha256: "abc".into(),
                ..entry("e")
            },
            Entry {
                bytes: 0,
                ..entry("f")
            },
        ];
        let mut all = vec![good.clone()];
        all.extend(bad);
        assert_eq!(parse(&serialize(&all)), [good]);
        assert!(parse("garbage\n[\nname = x").is_empty());
    }

    #[test]
    fn an_added_model_is_a_local_model_like_the_others() {
        let model = entry("ggml-medium-sl").intern();
        assert!(model.id.starts_with(PREFIX));
        assert_eq!(model.provider, ProviderKind::Local);
        assert_eq!(model.detail, "Custom");
        assert!(model.summary.contains("huggingface.co/org/model"));
        assert!(matches!(model.local, Some((_, Runner::Server))));
        if let Some((file, _)) = model.local {
            assert_eq!(file.file, "custom/ggml-medium-sl.bin");
            assert_eq!(file.source.bytes, 1_533_763_059);
        }
    }

    #[test]
    fn names_ids_and_files_are_tidy_and_unique() {
        let choice = |path: &str, origin: Origin| Choice {
            path: path.into(),
            url: String::new(),
            sha256: None,
            bytes: 1,
            origin,
            whisper: None,
        };
        let hub = || Origin::Hub("someone/whisper-large-v3-slovenian".into());
        assert_eq!(
            choice("ggml-model.bin", hub()).name(),
            "whisper-large-v3-slovenian"
        );
        assert_eq!(
            choice("ggml-model-q5_0.bin", hub()).name(),
            "whisper-large-v3-slovenian"
        );
        assert_eq!(
            choice("q/ggml-medium-q5_0.bin", hub()).name(),
            "ggml-medium-q5_0"
        );
        assert_eq!(
            choice(r"C:\Models\ggml-small.en.bin", Origin::Disk).name(),
            "ggml-small.en"
        );
        assert_eq!(slug_of("Whisper Large (SL) v3!"), "whisper-large-sl-v3");
        assert_eq!(slug_of("../.."), "model");
        assert_eq!(slug_of("ggml-small.en"), "ggml-small.en");
        let taken = ["base", "base-2"];
        assert_eq!(unique_slug("base", |slug| taken.contains(&slug)), "base-3");
        assert_eq!(unique_slug("tiny", |slug| taken.contains(&slug)), "tiny");
    }

    #[test]
    fn a_file_on_this_pc_is_offered_only_if_it_is_a_whisper_model() -> std::io::Result<()> {
        let dir = std::env::temp_dir().join(format!("vinput-custom-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let good = dir.join("ggml-base.bin");
        let mut bytes = whisper_header(51_865, 512, 6);
        bytes.resize(4096, 0);
        std::fs::write(&good, &bytes)?;
        let found = choices(&good.to_string_lossy());
        assert!(matches!(found.as_deref(), Ok([_])), "{found:?}");
        let Ok([choice]) = found.as_deref() else {
            return Ok(());
        };
        assert_eq!(choice.origin, Origin::Disk);
        assert_eq!(choice.bytes, 4096);
        assert!(choice.url.starts_with(LOCAL));
        assert_eq!(
            choice.whisper.map(|model| model.describe()).as_deref(),
            Some("Whisper base, 99 languages")
        );

        let gguf = dir.join("model.gguf");
        std::fs::write(&gguf, b"GGUF\x03\0\0\0rest of it")?;
        let refused = choices(&format!("\"{}\"", gguf.display()));
        assert!(
            matches!(&refused, Err(Error::Install(why)) if why.contains("GGUF")),
            "{refused:?}"
        );
        let missing = choices(&dir.join("nothing.bin").to_string_lossy());
        assert!(missing.is_err());
        std::fs::remove_dir_all(dir)
    }
}
