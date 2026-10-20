//! Finding and fetching newer versions of Vinput.
//!
//! A small manifest on the Vinput site names the newest release, its setup
//! program, and that program's size and SHA-256. Vinput reads the manifest now
//! and then; when the user accepts an update, the setup program is downloaded,
//! checked against the manifest and run. It replaces the installed program and
//! starts it again. Settings are not part of the program and are left alone.
//!
//! # Whose word a release is
//!
//! The manifest and the setup program come from the same site, so a checksum
//! in the one says nothing about who put the other there: whoever can publish
//! to the site could publish both. A manifest therefore carries a signature,
//! made when the release is built with a key that is kept off the site and
//! out of the repository, and the half of that key that checks signatures is
//! in the program ([`PUBLISHER`]). What is signed is [`signed_text`]: whose
//! setup program it is, its version, its checksum and its size. A release
//! whose signature does not hold is not offered and not downloaded, so
//! nothing is run that was not downloaded against a signed checksum. The
//! setup program has to be on the site ([`SITE`]) as well.
//!
//! A signed release stays signed, so an old manifest put back is still
//! believed; it is not offered, because only a version newer than the one
//! that runs is ([`is_newer`]). `scripts/release.ps1` signs, with
//! `cargo run -p putlab-core --example sign`.
//!
//! Versions from before there were signatures read the `signature` of a
//! manifest as they read any field they do not know, not at all, and update
//! by the checksum as they always did. The first signed release reaches
//! them that way, and checks every release after it.

use std::path::PathBuf;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use ring::signature::{ED25519, UnparsedPublicKey};
use serde_json::Value;

use crate::download::{self, Source};
use crate::folders::local_dir;
use crate::net::Request;
use crate::product;
use crate::{Error, Result};

/// Where the manifest of the newest release of Vinput is published.
pub const MANIFEST_URL: &str = product::VINPUT.manifest;
/// Environment variable that points a build for development at another
/// manifest, for trying an update out before it is published. A build that
/// ships does not read it ([`LISTENS`]).
pub const ENV_MANIFEST_URL: &str = "VINPUT_UPDATE_URL";
/// The site releases are published on. A setup program is downloaded from
/// here and from nowhere else.
pub const SITE: &str = "https://vinput-dasvcr.web.app/";
/// Whether this build reads [`ENV_MANIFEST_URL`] and takes a release from
/// this computer: a build for development and the tests do, a build that
/// ships does not.
const LISTENS: bool = cfg!(any(debug_assertions, test));

/// The key releases are signed with: the half of it that checks a signature
/// (Ed25519). The half that signs is with whoever publishes releases and
/// nowhere else; `cargo run -p putlab-core --example sign -- public <key
/// file>` prints this line for it.
pub const PUBLISHER: [u8; 32] = [
    0x90, 0x0b, 0x8a, 0x92, 0x8b, 0x0c, 0xea, 0x24, 0xc2, 0x9b, 0x77, 0x75, 0x1c, 0xc4, 0x34, 0xbf,
    0x8c, 0x03, 0xb8, 0xb9, 0xb2, 0x2b, 0x6f, 0x98, 0xb1, 0x79, 0xae, 0xbf, 0xdd, 0x9f, 0xf1, 0x17,
];

/// The key a signature is checked against: the publisher's, and in tests
/// the key of [`testing`], with which a test signs the releases it makes up.
fn publisher() -> [u8; 32] {
    #[cfg(any(test, feature = "test-instance"))]
    {
        testing::publisher()
    }
    #[cfg(not(any(test, feature = "test-instance")))]
    {
        PUBLISHER
    }
}
/// The version of Vinput this was built as.
pub const CURRENT_VERSION: &str = product::VINPUT.version;

const TIMEOUT: Duration = Duration::from_secs(20);
/// A manifest is a few hundred bytes; anything much larger is not one.
const MAX_MANIFEST_BYTES: usize = 64 * 1024;

/// A release that can be installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Its version, such as `0.3.0`.
    pub version: String,
    /// What is new, one short line each.
    pub notes: Vec<String>,
    /// Address of the setup program.
    pub url: String,
    /// SHA-256 of the setup program.
    pub sha256: String,
    /// Size of the setup program in bytes.
    pub bytes: u64,
    /// The publisher's signature under [`signed_text`], in base64; empty if
    /// the manifest has none.
    pub signature: String,
}

/// What the signature of a release is made over: whose setup program it is
/// (what its name begins with, such as `VinputSetup`), its version, the
/// SHA-256 of the setup program in lower case, and its size in bytes, a line
/// each. The name is part of it so that the release of one program is not
/// taken for that of another.
#[must_use]
pub fn signed_text(setup: &str, version: &str, sha256: &str, bytes: u64) -> String {
    format!(
        "{setup}\n{version}\n{}\n{bytes}\n",
        sha256.to_ascii_lowercase()
    )
}

impl Release {
    /// Reads a manifest.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Install`] if a field is missing or the download would
    /// not come over an encrypted connection.
    pub fn parse(manifest: &str) -> Result<Self> {
        let invalid = |what: &str| Error::Install(format!("the update manifest {what}"));
        let value: Value =
            serde_json::from_str(manifest).map_err(|_| invalid("is not valid JSON"))?;
        let text = |name: &str| {
            value
                .get(name)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| invalid(&format!("has no \"{name}\"")))
        };
        let release = Self {
            version: text("version")?,
            notes: value
                .get("notes")
                .and_then(Value::as_array)
                .map(|notes| {
                    notes
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            url: text("url")?,
            sha256: text("sha256")?,
            bytes: value
                .get("bytes")
                .and_then(Value::as_u64)
                .ok_or_else(|| invalid("has no \"bytes\""))?,
            signature: value
                .get("signature")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        };
        if version_of(&release.version).is_none() {
            return Err(invalid("names a version that is not three numbers"));
        }
        if !is_on_the_site(&release.url) {
            return Err(invalid(
                "points to a download that is not on the Vinput site",
            ));
        }
        if release.sha256.len() != 64 || !release.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(invalid("has a malformed checksum"));
        }
        Ok(release)
    }

    /// Checks that the publisher signed this release as one of the program
    /// whose setup program's name begins with `setup`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Install`] if the manifest carried no signature, or
    /// one that does not hold for what the manifest says.
    pub fn verify(&self, setup: &str) -> Result<()> {
        let unsigned = || Error::Install("the update is not signed by its publisher".into());
        let signature = BASE64.decode(&self.signature).map_err(|_| unsigned())?;
        let text = signed_text(setup, &self.version, &self.sha256, self.bytes);
        UnparsedPublicKey::new(&ED25519, publisher())
            .verify(text.as_bytes(), &signature)
            .map_err(|_| unsigned())
    }

    /// Whether this release is newer than the running program.
    #[must_use]
    pub fn is_newer(&self) -> bool {
        is_newer(&self.version, product::current().version)
    }

    /// Where the downloaded setup program is kept. `setup` is what its name
    /// begins with.
    fn setup_path(&self, setup: &str) -> Option<PathBuf> {
        Some(
            local_dir()?
                .join("updates")
                .join(format!("{setup}-{}.exe", self.version)),
        )
    }

    /// Downloads the setup program and checks it against the manifest.
    /// `progress` is called with the bytes received so far; returning `false`
    /// stops the download.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Install`] if the download fails, is stopped, or does
    /// not match the manifest.
    pub fn download(&self, progress: &mut dyn FnMut(u64) -> bool) -> Result<PathBuf> {
        self.download_as(product::current().setup, progress)
    }

    /// Like [`download`](Self::download), for the release of another program
    /// of the family: `setup` is what the name of its setup program begins
    /// with, such as `VinputSetup`. That name is what makes the program
    /// install instead of run.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Install`] if the release is not signed by its
    /// publisher, or the download fails, is stopped, or does not match the
    /// manifest.
    pub fn download_as(
        &self,
        setup: &str,
        progress: &mut dyn FnMut(u64) -> bool,
    ) -> Result<PathBuf> {
        // Checked here whatever was checked before: this is the last step
        // before a program is fetched to be run.
        self.verify(setup)?;
        let path = self
            .setup_path(setup)
            .ok_or_else(|| Error::Install("LOCALAPPDATA is not set".into()))?;
        let source = Source {
            url: &self.url,
            sha256: &self.sha256,
            bytes: self.bytes,
        };
        download::fetch(source, &path, progress)?;
        Ok(path)
    }
}

/// On this computer, which is how tests and trial runs serve a release.
fn is_here(url: &str) -> bool {
    url.starts_with("http://127.0.0.1:") || url.starts_with("http://localhost:")
}

/// Whether a manifest may be asked for at `url`, or a setup program
/// downloaded from it: on the site or, in a build that listens, on this
/// computer. The site's address ends in a slash, so no other host begins
/// with it.
#[must_use]
pub fn is_on_the_site(url: &str) -> bool {
    url.starts_with(SITE) || LISTENS && is_here(url)
}

/// The three numbers of a version such as `1.2.3`.
fn version_of(version: &str) -> Option<[u32; 3]> {
    let mut parts = version.trim().trim_start_matches('v').split('.');
    let mut numbers = [0u32; 3];
    for number in &mut numbers {
        *number = parts.next()?.parse().ok()?;
    }
    parts.next().is_none().then_some(numbers)
}

/// Whether version `candidate` is newer than version `current`.
#[must_use]
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (version_of(candidate), version_of(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

/// The manifest address to use: the published one, unless a build that
/// listens was told another, on the site or on this computer.
#[must_use]
pub fn manifest_url() -> String {
    std::env::var(ENV_MANIFEST_URL)
        .ok()
        .filter(|url| LISTENS && is_on_the_site(url))
        .unwrap_or_else(|| product::current().manifest.to_owned())
}

/// Asks the site for the newest release. `Ok(None)` means the running program
/// is up to date, or nothing has been published yet.
///
/// # Errors
///
/// Returns [`Error::Install`] if the site cannot be reached or its manifest
/// cannot be read.
pub fn check() -> Result<Option<Release>> {
    check_at(&manifest_url())
}

/// Like [`check`], at a given manifest address.
///
/// # Errors
///
/// Returns [`Error::Install`] if the site cannot be reached, its manifest
/// cannot be read, or a newer release in it is not signed by its publisher.
pub fn check_at(url: &str) -> Result<Option<Release>> {
    let Some(release) = newest_at(url)?.filter(Release::is_newer) else {
        return Ok(None);
    };
    release.verify(product::current().setup)?;
    Ok(Some(release))
}

/// The newest release the manifest at `url` names, whether or not it is
/// newer than anything: for a program that looks after another one, whose
/// version is not its own. `Ok(None)` means nothing has been published.
/// Whose release it is, is for the caller to check ([`Release::verify`]).
///
/// # Errors
///
/// Returns [`Error::Install`] if the site cannot be reached or its manifest
/// cannot be read.
pub fn newest_at(url: &str) -> Result<Option<Release>> {
    let failed = |what: String| Error::Install(format!("checking for updates failed: {what}"));
    let mut response = Request::get(url, TIMEOUT).send().map_err(failed)?;
    let manifest = match response.status() {
        200 => Some(
            response
                .body(MAX_MANIFEST_BYTES, TIMEOUT)
                .map(|body| String::from_utf8_lossy(&body).into_owned())
                .map_err(failed)?,
        ),
        // Nothing published is not an error.
        404 => None,
        status => return Err(failed(format!("the site answered HTTP {status}"))),
    };
    let Some(manifest) = manifest else {
        return Ok(None);
    };
    Release::parse(&manifest).map(Some)
}

/// Deletes setup programs left over from earlier updates.
pub fn clean_up() {
    if let Some(dir) = local_dir().map(|dir| dir.join("updates")) {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// For tests, in this crate and in the ones that ask for its feature
/// `test-instance`: a key of their own in the place of the publisher's, so
/// that a test can sign the releases it makes up. Its signing half is right
/// here, which is why no build that ships has it.
#[cfg(any(test, feature = "test-instance"))]
pub mod testing {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    /// What the key of the tests is made from.
    const SEED: [u8; 32] = *b"a key for tests, known to anyone";

    fn key() -> Option<Ed25519KeyPair> {
        Ed25519KeyPair::from_seed_unchecked(&SEED).ok()
    }

    /// The half of the key of the tests that checks a signature.
    #[must_use]
    pub fn publisher() -> [u8; 32] {
        key()
            .and_then(|key| key.public_key().as_ref().try_into().ok())
            .unwrap_or_default()
    }

    /// The signature of a release as the tests' publisher makes it, for the
    /// `signature` of a manifest.
    #[must_use]
    pub fn sign(setup: &str, version: &str, sha256: &str, bytes: u64) -> String {
        let text = super::signed_text(setup, version, sha256, bytes);
        key().map_or_else(String::new, |key| BASE64.encode(key.sign(text.as_bytes())))
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    const CHECKSUM: &str = "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe";

    const ON_THE_SITE: &str = "https://vinput-dasvcr.web.app/releases/VinputSetup.exe";

    /// A manifest signed as a release of Vinput.
    fn manifest(version: &str, url: &str) -> String {
        let signature = testing::sign("VinputSetup", version, CHECKSUM, 7_000_000);
        format!(
            r#"{{"version":"{version}","notes":["Faster start","Voice memos"],
                "url":"{url}","sha256":"{CHECKSUM}","bytes":7000000,"published":"2026-10-05",
                "signature":"{signature}"}}"#
        )
    }

    /// The same without a signature, as every manifest was before there
    /// were any.
    fn unsigned(version: &str, url: &str) -> String {
        format!(
            r#"{{"version":"{version}","notes":[],
                "url":"{url}","sha256":"{CHECKSUM}","bytes":7000000,"published":"2026-10-05"}}"#
        )
    }

    #[test]
    fn versions_compare_by_number_not_by_text() {
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("v1.0.0", "0.99.99"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.1.9", "0.2.0"));
        assert!(!is_newer("banana", "0.2.0"));
        assert!(!is_newer("1.2", "0.2.0"));
    }

    #[test]
    fn a_manifest_is_read() {
        let release = Release::parse(&manifest("9.0.0", ON_THE_SITE));
        assert_eq!(
            release.ok(),
            Some(Release {
                version: "9.0.0".into(),
                notes: vec!["Faster start".into(), "Voice memos".into()],
                url: ON_THE_SITE.into(),
                sha256: CHECKSUM.into(),
                bytes: 7_000_000,
                signature: testing::sign("VinputSetup", "9.0.0", CHECKSUM, 7_000_000),
            })
        );
    }

    #[test]
    fn a_release_is_believed_only_with_its_publishers_signature() -> Result<()> {
        let signed = Release::parse(&manifest("9.0.0", ON_THE_SITE))?;
        signed.verify("VinputSetup")?;

        // None at all, as whoever got onto the site would have to leave it,
        assert!(
            Release::parse(&unsigned("9.0.0", ON_THE_SITE))?
                .verify("VinputSetup")
                .is_err()
        );
        // or one that is not a signature,
        let mut garbled = signed.clone();
        garbled.signature = "bm90IGEgc2lnbmF0dXJl".into();
        assert!(garbled.verify("VinputSetup").is_err());
        garbled.signature = "not base64 at all!".into();
        assert!(garbled.verify("VinputSetup").is_err());

        // A signature holds for one setup program and no other: another
        // checksum, size or version under it is not what was signed,
        let other = "a".repeat(64);
        for changed in [
            Release {
                sha256: other,
                ..signed.clone()
            },
            Release {
                bytes: 7_000_001,
                ..signed.clone()
            },
            Release {
                version: "9.0.1".into(),
                ..signed.clone()
            },
        ] {
            assert!(changed.verify("VinputSetup").is_err(), "{changed:?}");
        }
        // nor is the release of another program of the family,
        assert!(signed.verify("VoutputSetup").is_err());
        // and where the file is fetched from is free within the site, the
        // checksum being what ties it down.
        Release {
            url: format!("{SITE}elsewhere/VinputSetup.exe"),
            ..signed
        }
        .verify("VinputSetup")
    }

    #[test]
    fn a_checksum_in_capitals_is_the_same_checksum() -> Result<()> {
        let shouted = manifest("9.0.0", ON_THE_SITE).replace(CHECKSUM, &CHECKSUM.to_uppercase());
        Release::parse(&shouted)?.verify("VinputSetup")
    }

    #[test]
    fn a_release_that_is_not_signed_is_not_downloaded() -> Result<()> {
        let release = Release::parse(&unsigned("9.0.0", ON_THE_SITE))?;
        let mut asked = false;
        let fetched = release.download(&mut |_| {
            asked = true;
            true
        });
        assert!(matches!(fetched, Err(Error::Install(why)) if why.contains("not signed")));
        assert!(!asked);
        Ok(())
    }

    /// What a version from before there were signatures does with the
    /// manifest of a signed release: it reads the fields it knows, as
    /// `parse` still does first, and is not put off by the one it does not.
    /// (Those versions are not built here; this holds the manifest to the
    /// form they read, with every field they ask for in its place.)
    #[test]
    fn a_signed_manifest_is_still_one_that_earlier_versions_read() {
        let signed = manifest("9.0.0", ON_THE_SITE);
        let value: Value = serde_json::from_str(&signed).unwrap_or_default();
        for field in ["version", "url", "sha256"] {
            assert!(value[field].is_string(), "{field}");
        }
        assert!(value["notes"].is_array());
        assert_eq!(value["bytes"].as_u64(), Some(7_000_000));
        assert!(
            value["url"]
                .as_str()
                .is_some_and(|url| url.starts_with("https://"))
        );
        assert_eq!(value["sha256"].as_str().map(str::len), Some(64));
    }

    #[test]
    fn a_setup_program_comes_from_the_site_and_nowhere_else() {
        assert!(is_on_the_site(ON_THE_SITE));
        for elsewhere in [
            "https://example.com/VinputSetup.exe",
            "https://vinput-dasvcr.web.app.example.com/VinputSetup.exe",
            "https://vinput-dasvcr.web.app@example.com/VinputSetup.exe",
            "http://vinput-dasvcr.web.app/releases/VinputSetup.exe",
            "https://vinput-dasvcr.web.app",
        ] {
            assert!(!is_on_the_site(elsewhere), "{elsewhere}");
            assert!(Release::parse(&manifest("9.0.0", elsewhere)).is_err());
        }
    }

    #[test]
    fn a_manifest_that_cannot_be_trusted_is_refused() {
        for bad in [
            "not json".to_owned(),
            "{}".to_owned(),
            manifest("9.0.0", "http://example.com/VinputSetup.exe"),
            manifest("9.0.0", "https://example.com/VinputSetup.exe"),
            manifest("nine", ON_THE_SITE),
            manifest("9.0.0", ON_THE_SITE).replace(CHECKSUM, "abc"),
        ] {
            assert!(Release::parse(&bad).is_err(), "{bad}");
        }
    }

    /// Serves one reply to one request on a loopback port.
    fn serve(status: &'static str, body: String) -> std::io::Result<String> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let url = format!("http://{}/releases/latest.json", listener.local_addr()?);
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut request = [0u8; 2048];
                let _ = stream.read(&mut request);
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        Ok(url)
    }

    #[test]
    fn a_newer_release_is_offered_and_an_older_one_is_not() -> std::io::Result<()> {
        let newer = serve("200 OK", manifest("99.0.0", ON_THE_SITE))?;
        let found = check_at(&newer);
        assert_eq!(
            found.ok().flatten().map(|release| release.version),
            Some("99.0.0".into())
        );

        let older = serve("200 OK", manifest("0.0.1", ON_THE_SITE))?;
        assert_eq!(check_at(&older).ok(), Some(None));

        // A newer one nobody signed is an error, not an offer, and an older
        // one is no more of either for being unsigned.
        let forged = serve("200 OK", unsigned("99.0.0", ON_THE_SITE))?;
        assert!(
            matches!(check_at(&forged), Err(Error::Install(why)) if why.contains("not signed"))
        );
        let stale = serve("200 OK", unsigned("0.0.1", ON_THE_SITE))?;
        assert_eq!(check_at(&stale).ok(), Some(None));

        let unpublished = serve("404 Not Found", String::new())?;
        assert_eq!(check_at(&unpublished).ok(), Some(None));

        let broken = serve("500 Internal Server Error", String::new())?;
        assert!(check_at(&broken).is_err());
        Ok(())
    }
}
