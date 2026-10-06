//! Finding a model on Hugging Face: reading the link someone pastes, listing
//! the files of the repository it names, and reading the first bytes of one.
//!
//! Only public models are reachable; Vinput signs in nowhere. Every address
//! it downloads from is pinned to the commit the listing came from, like the
//! built-in models, so the bytes behind it cannot change under the checksum.

use std::time::Duration;

use serde_json::Value;

use super::ggml::HEADER_BYTES;
use crate::net::Request;
use crate::{Error, Result};

const SITE: &str = "https://huggingface.co";
const TIMEOUT: Duration = Duration::from_secs(20);
/// A listing larger than this is not one Vinput needs to read.
const MAX_LISTING_BYTES: usize = 4 << 20;
/// First path segments of the site that are not an organization or user.
const NOT_MODELS: [&str; 9] = [
    "datasets",
    "spaces",
    "docs",
    "api",
    "blog",
    "models",
    "papers",
    "collections",
    "settings",
];
/// Names of `.bin` files that hold something other than a ggml model.
const NOT_GGML: [&str; 6] = [
    "pytorch_model",
    "training_args",
    "optimizer",
    "scheduler",
    "rng_state",
    "adapter_model",
];

/// What a pasted link points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// `organization/name`.
    pub repo: String,
    /// The branch, tag or commit in the link; the main branch if none.
    pub revision: Option<String>,
    /// The path of one file in the repository, if the link names one.
    pub file: Option<String>,
}

/// Reads a link to a model on Hugging Face: the page of a repository, of a
/// file in it, a download address, or just `organization/name`.
///
/// # Errors
///
/// Returns a one-line reason, for the user, if it is none of these.
pub fn parse_link(text: &str) -> std::result::Result<Link, &'static str> {
    const NOT_A_LINK: &str = "Paste a link to a model on huggingface.co, or pick a file.";
    let text = text.trim();
    let text = text.split(['?', '#']).next().unwrap_or_default();
    let bare = text
        .strip_prefix("https://")
        .or_else(|| text.strip_prefix("http://"))
        .unwrap_or(text);
    let bare = bare.strip_prefix("www.").unwrap_or(bare);
    let path = match bare.split_once('/') {
        Some(("huggingface.co" | "hf.co", path)) => path,
        // A repository named on its own, as Hugging Face writes them.
        _ if bare.matches('/').count() == 1 && !bare.contains(':') => bare,
        _ => return Err(NOT_A_LINK),
    };
    let segments: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    let [owner, name, rest @ ..] = segments.as_slice() else {
        return Err(NOT_A_LINK);
    };
    let (owner, name) = (*owner, *name);
    if NOT_MODELS.contains(&owner) {
        return Err("That link is not a model. Paste the link of a model on huggingface.co.");
    }
    let plain = |part: &str| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.%".contains(c))
    };
    if !plain(owner) || !plain(name) || !rest.iter().all(|part| plain(part)) {
        return Err(NOT_A_LINK);
    }
    let repo = format!("{owner}/{name}");
    let (revision, file) = match rest {
        ["blob" | "resolve", revision, file @ ..] if !file.is_empty() => {
            (Some(*revision), Some(file.join("/")))
        }
        ["tree", revision, ..] => (Some(*revision), None),
        _ => (None, None),
    };
    Ok(Link {
        repo,
        revision: revision.map(str::to_owned),
        file,
    })
}

/// A file in a repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoFile {
    /// Its path in the repository.
    pub path: String,
    /// Its size in bytes.
    pub bytes: u64,
    /// Its SHA-256, which Hugging Face knows for every large file.
    pub sha256: Option<String>,
}

impl RepoFile {
    /// Its name without the folders.
    #[must_use]
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

/// The files of a repository at one commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    /// `organization/name`.
    pub repo: String,
    /// The commit the files were listed at.
    pub commit: String,
    /// Every file.
    pub files: Vec<RepoFile>,
}

impl Listing {
    /// The files that may be ggml models, largest first. Which of them really
    /// are is only known from their first bytes.
    #[must_use]
    pub fn candidates(&self) -> Vec<&RepoFile> {
        let mut found: Vec<&RepoFile> = self
            .files
            .iter()
            .filter(|file| {
                let name = file.name().to_ascii_lowercase();
                let Some(stem) = name.strip_suffix(".bin") else {
                    return false;
                };
                file.bytes > 1_000_000 && !NOT_GGML.iter().any(|other| stem.starts_with(other))
            })
            .collect();
        found.sort_by(|a, b| a.path.cmp(&b.path));
        found
    }

    /// Why a repository without candidates has nothing Vinput can run, by
    /// what it does have.
    #[must_use]
    pub fn why_none(&self) -> &'static str {
        let has = |extension: &str| {
            self.files
                .iter()
                .any(|file| file.path.to_ascii_lowercase().ends_with(extension))
        };
        if has(".gguf") {
            "This model is in GGUF format. VinPUT runs Whisper models in ggml (.bin)."
        } else if has(".safetensors") || has(".bin") || has(".pt") {
            "This model is not converted to ggml. Look for a version with a .bin file."
        } else {
            "This repository has no Whisper model in ggml format (.bin)."
        }
    }

    /// The download address of one of its files, pinned to the commit.
    #[must_use]
    pub fn address(&self, file: &RepoFile) -> String {
        format!("{SITE}/{}/resolve/{}/{}", self.repo, self.commit, file.path)
    }

    /// The file a link names, if it is in this listing.
    #[must_use]
    pub fn find(&self, path: &str) -> Option<&RepoFile> {
        self.files.iter().find(|file| file.path == path)
    }
}

/// Reads the answer of the Hugging Face API about a repository.
///
/// # Errors
///
/// Returns [`Error::Install`] if it is not the answer expected.
pub fn parse_listing(repo: &str, json: &str) -> Result<Listing> {
    let unreadable = || Error::Install("Hugging Face sent an answer VinPUT cannot read".into());
    let answer: Value = serde_json::from_str(json).map_err(|_| unreadable())?;
    let commit = answer["sha"]
        .as_str()
        .filter(|sha| sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()))
        .ok_or_else(unreadable)?
        .to_owned();
    let files = answer["siblings"]
        .as_array()
        .ok_or_else(unreadable)?
        .iter()
        .filter_map(|sibling| {
            let path = sibling["rfilename"].as_str()?.to_owned();
            let lfs = &sibling["lfs"];
            let bytes = lfs["size"].as_u64().or_else(|| sibling["size"].as_u64())?;
            let sha256 = lfs["sha256"]
                .as_str()
                .filter(|sha| sha.len() == 64 && sha.chars().all(|c| c.is_ascii_hexdigit()))
                .map(str::to_ascii_lowercase);
            Some(RepoFile {
                path,
                bytes,
                sha256,
            })
        })
        .collect();
    Ok(Listing {
        repo: repo.to_owned(),
        commit,
        files,
    })
}

#[expect(clippy::needless_pass_by_value, reason = "fits `map_err` as it is")]
fn unreachable(err: String) -> Error {
    crate::log::write(&format!("reaching Hugging Face: {err}"));
    Error::Install("Hugging Face could not be reached. Check the connection and try again.".into())
}

/// Lists the files of the repository a link names, at its revision.
///
/// # Errors
///
/// Returns [`Error::Install`] with a reason for the user if the repository
/// is not a public model or Hugging Face cannot be reached.
pub fn list(link: &Link) -> Result<Listing> {
    let revision = link.revision.as_deref().unwrap_or("main");
    let url = format!(
        "{SITE}/api/models/{}/revision/{revision}?blobs=true",
        link.repo
    );
    let mut response = Request::get(&url, TIMEOUT).send().map_err(unreachable)?;
    match response.status() {
        200 => {}
        // Hugging Face answers 401 for a repository it will not show,
        // whether it is private or does not exist.
        401 | 404 => {
            return Err(Error::Install(format!(
                "Hugging Face has no public model {}.",
                link.repo
            )));
        }
        403 => {
            return Err(Error::Install(format!(
                "{} needs a Hugging Face sign-in. VinPUT can use public models only.",
                link.repo
            )));
        }
        status => {
            return Err(Error::Install(format!(
                "Hugging Face answered HTTP {status}. Try again later."
            )));
        }
    }
    let body = response.body(MAX_LISTING_BYTES, TIMEOUT).map_err(|err| {
        if err.contains("too large") {
            Error::Install("the repository is too large to list".into())
        } else {
            unreachable(err)
        }
    })?;
    parse_listing(&link.repo, &String::from_utf8_lossy(&body))
}

/// The first bytes of the file at `url`, enough to tell what it is.
///
/// # Errors
///
/// Returns [`Error::Install`] if they cannot be downloaded.
pub fn header(url: &str) -> Result<Vec<u8>> {
    let range = format!("bytes=0-{}", HEADER_BYTES - 1);
    let mut response = Request {
        headers: &[("Range", &range)],
        ..Request::get(url, TIMEOUT)
    }
    .send()
    .map_err(unreachable)?;
    let status = response.status();
    if status != 200 && status != 206 {
        return Err(Error::Install(format!(
            "Hugging Face answered HTTP {status} for the file. Try again later."
        )));
    }
    // A server that ignores the range sends the whole file; the start is
    // enough, and the rest is left unread.
    let mut header = vec![0u8; HEADER_BYTES];
    let mut have = 0;
    while have < HEADER_BYTES {
        let read = response.read(&mut header[have..]).map_err(unreachable)?;
        if read == 0 {
            break;
        }
        have += read;
    }
    header.truncate(have);
    Ok(header)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(repo: &str, revision: Option<&str>, file: Option<&str>) -> Link {
        Link {
            repo: repo.to_owned(),
            revision: revision.map(str::to_owned),
            file: file.map(str::to_owned),
        }
    }

    #[test]
    fn links_to_a_repository_or_a_file_are_read() {
        let cases = [
            (
                "https://huggingface.co/ggerganov/whisper.cpp",
                link("ggerganov/whisper.cpp", None, None),
            ),
            (
                "huggingface.co/ggerganov/whisper.cpp/",
                link("ggerganov/whisper.cpp", None, None),
            ),
            (
                "  hf.co/org/model?not=this  ",
                link("org/model", None, None),
            ),
            ("org/model", link("org/model", None, None)),
            (
                "https://huggingface.co/org/model/tree/v2",
                link("org/model", Some("v2"), None),
            ),
            (
                "https://huggingface.co/org/model/blob/main/ggml-medium.bin",
                link("org/model", Some("main"), Some("ggml-medium.bin")),
            ),
            (
                "https://huggingface.co/org/model/resolve/abc123/models/ggml-small.bin?download=true",
                link("org/model", Some("abc123"), Some("models/ggml-small.bin")),
            ),
            (
                "https://www.huggingface.co/org/model/discussions",
                link("org/model", None, None),
            ),
        ];
        for (text, expected) in cases {
            assert_eq!(parse_link(text), Ok(expected), "{text}");
        }
    }

    #[test]
    fn anything_else_is_refused() {
        for text in [
            "",
            "whisper",
            "https://example.com/org/model",
            "https://huggingface.co/datasets/org/data",
            "https://huggingface.co/spaces/org/app",
            "https://huggingface.co/org",
            "https://huggingface.co/org/model/blob/main/../../etc",
            r"C:\models\ggml.bin",
            "a b/c",
        ] {
            assert!(parse_link(text).is_err(), "{text}");
        }
    }

    const ANSWER: &str = r#"{
        "id": "org/model",
        "sha": "5359861c739e955e79d9a303bcbc70fb988958b1",
        "siblings": [
            {"rfilename": "README.md", "size": 2000},
            {"rfilename": "ggml-medium.bin", "size": 1533763059,
             "lfs": {"sha256": "6C14D5ADEE5F86394037B4E4E8B59F1673B6CEE10E3CF0B11BBDBEE79C156208", "size": 1533763059}},
            {"rfilename": "q/ggml-medium-q5_0.bin", "size": 539212467,
             "lfs": {"sha256": "19fea4b380c3a618ec4723c3eef2eb785ffba0d0538cf43f8f235e7b3b34220f", "size": 539212467}},
            {"rfilename": "pytorch_model.bin", "size": 3055754841},
            {"rfilename": "model.safetensors", "size": 3055544304}
        ]
    }"#;

    #[test]
    fn a_listing_is_read_and_only_ggml_candidates_are_offered() {
        let listing = parse_listing("org/model", ANSWER);
        assert!(listing.is_ok(), "{listing:?}");
        let Ok(listing) = listing else {
            return;
        };
        assert_eq!(listing.files.len(), 5);
        let names: Vec<&str> = listing
            .candidates()
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        assert_eq!(names, ["ggml-medium.bin", "q/ggml-medium-q5_0.bin"]);
        let medium = listing.find("ggml-medium.bin");
        assert_eq!(
            medium.and_then(|file| file.sha256.as_deref()),
            Some("6c14d5adee5f86394037b4e4e8b59f1673b6cee10e3cf0b11bbdbee79c156208")
        );
        assert_eq!(
            medium.map(|file| listing.address(file)).as_deref(),
            Some(
                "https://huggingface.co/org/model/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-medium.bin"
            )
        );
        assert_eq!(listing.candidates()[1].name(), "ggml-medium-q5_0.bin");
    }

    #[test]
    fn a_repository_without_ggml_says_what_it_has() {
        let listing = |names: &[&str]| Listing {
            repo: "org/model".into(),
            commit: "0".repeat(40),
            files: names
                .iter()
                .map(|name| RepoFile {
                    path: (*name).to_owned(),
                    bytes: 5_000_000,
                    sha256: None,
                })
                .collect(),
        };
        let safetensors = listing(&["model.safetensors", "pytorch_model.bin"]);
        assert!(safetensors.candidates().is_empty());
        assert!(safetensors.why_none().contains("not converted to ggml"));
        assert!(listing(&["model-q4.gguf"]).why_none().contains("GGUF"));
        assert!(
            listing(&["README.md"])
                .why_none()
                .contains("no Whisper model")
        );
    }

    #[test]
    fn a_strange_answer_is_an_error() {
        assert!(parse_listing("org/model", "<html>").is_err());
        assert!(parse_listing("org/model", r#"{"sha": "short", "siblings": []}"#).is_err());
    }
}
