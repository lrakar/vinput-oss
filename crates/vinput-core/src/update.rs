//! Finding and fetching newer versions of Vinput.
//!
//! A small manifest on the Vinput site names the newest release, its setup
//! program, and that program's size and SHA-256. Vinput reads the manifest now
//! and then; when the user accepts an update, the setup program is downloaded,
//! checked against the manifest and run. It replaces the installed program and
//! starts it again. Settings are not part of the program and are left alone.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

use crate::download::{self, Source};
use crate::net::Request;
use crate::settings::local_dir;
use crate::{Error, Result};

/// Where the manifest of the newest release is published.
pub const MANIFEST_URL: &str = "https://vinput-dasvcr.web.app/releases/latest.json";
/// Environment variable that points Vinput at another manifest, for testing a
/// release before it is published.
pub const ENV_MANIFEST_URL: &str = "VINPUT_UPDATE_URL";
/// The version of the running program.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

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
        };
        if version_of(&release.version).is_none() {
            return Err(invalid("names a version that is not three numbers"));
        }
        if !is_trusted_address(&release.url) {
            return Err(invalid("points to a download that is not https"));
        }
        if release.sha256.len() != 64 || !release.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(invalid("has a malformed checksum"));
        }
        Ok(release)
    }

    /// Whether this release is newer than the running program.
    #[must_use]
    pub fn is_newer(&self) -> bool {
        is_newer(&self.version, CURRENT_VERSION)
    }

    /// Where the downloaded setup program is kept.
    fn setup_path(&self) -> Option<PathBuf> {
        Some(
            local_dir()?
                .join("updates")
                .join(format!("VinputSetup-{}.exe", self.version)),
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
        let path = self
            .setup_path()
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

/// Encrypted, or on this computer (which is how tests and trial runs serve it).
fn is_trusted_address(url: &str) -> bool {
    url.starts_with("https://")
        || url.starts_with("http://127.0.0.1:")
        || url.starts_with("http://localhost:")
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

/// The manifest address to use: the published one unless overridden.
#[must_use]
pub fn manifest_url() -> String {
    std::env::var(ENV_MANIFEST_URL)
        .ok()
        .filter(|url| is_trusted_address(url))
        .unwrap_or_else(|| MANIFEST_URL.to_owned())
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
/// Returns [`Error::Install`] if the site cannot be reached or its manifest
/// cannot be read.
pub fn check_at(url: &str) -> Result<Option<Release>> {
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
    let release = Release::parse(&manifest)?;
    Ok(release.is_newer().then_some(release))
}

/// Deletes setup programs left over from earlier updates.
pub fn clean_up() {
    if let Some(dir) = local_dir().map(|dir| dir.join("updates")) {
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    const CHECKSUM: &str = "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe";

    fn manifest(version: &str, url: &str) -> String {
        format!(
            r#"{{"version":"{version}","notes":["Faster start","Voice memos"],
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
        let release = Release::parse(&manifest("9.0.0", "https://example.com/VinputSetup.exe"));
        assert_eq!(
            release.ok(),
            Some(Release {
                version: "9.0.0".into(),
                notes: vec!["Faster start".into(), "Voice memos".into()],
                url: "https://example.com/VinputSetup.exe".into(),
                sha256: CHECKSUM.into(),
                bytes: 7_000_000,
            })
        );
    }

    #[test]
    fn a_manifest_that_cannot_be_trusted_is_refused() {
        for bad in [
            "not json".to_owned(),
            "{}".to_owned(),
            manifest("9.0.0", "http://example.com/VinputSetup.exe"),
            manifest("nine", "https://example.com/VinputSetup.exe"),
            manifest("9.0.0", "https://example.com/a.exe").replace(CHECKSUM, "abc"),
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
        let newer = serve("200 OK", manifest("99.0.0", "https://example.com/s.exe"))?;
        let found = check_at(&newer);
        assert_eq!(
            found.ok().flatten().map(|release| release.version),
            Some("99.0.0".into())
        );

        let older = serve("200 OK", manifest("0.0.1", "https://example.com/s.exe"))?;
        assert_eq!(check_at(&older).ok(), Some(None));

        let unpublished = serve("404 Not Found", String::new())?;
        assert_eq!(check_at(&unpublished).ok(), Some(None));

        let broken = serve("500 Internal Server Error", String::new())?;
        assert!(check_at(&broken).is_err());
        Ok(())
    }
}
