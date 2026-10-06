//! Downloading a file whose size and SHA-256 are known in advance.
//!
//! Used for speech models, their engine and Vinput's own updates. A file only
//! appears under its final name once every byte has been checked, so nothing
//! else in Vinput ever sees a partial or tampered download. An interrupted
//! download leaves a `.part` file behind and continues from it next time.
//!
//! A source can also be a file on this PC, written `file:` and its path: it
//! is copied the same way, so a model someone picks from disk is installed
//! like any other.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use ring::digest::{Context, SHA256};

use crate::net::Request;
use crate::{Error, Result};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long the server may send nothing before the download is given up.
const STALL_TIMEOUT: Duration = Duration::from_secs(60);

/// A file to download and what it must turn out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Source<'a> {
    /// Where to get it. Redirects are followed. `file:` and a path is a file
    /// on this PC.
    pub url: &'a str,
    /// Its SHA-256, as 64 hexadecimal digits, or empty for a file whose
    /// checksum nobody published, which then only has to have its size.
    pub sha256: &'a str,
    /// Its size in bytes.
    pub bytes: u64,
}

/// Downloads `source` to `dest`, creating the folder if needed.
///
/// `progress` is called with the number of bytes on disk so far, starting with
/// what an earlier attempt left behind; returning `false` from it stops the
/// download, keeping the partial file for next time.
///
/// # Errors
///
/// Returns [`Error::Install`] if the download fails or is stopped, or if the
/// file does not match its expected size and checksum, in which case it is
/// deleted.
pub fn fetch(source: Source<'_>, dest: &Path, progress: &mut dyn FnMut(u64) -> bool) -> Result<()> {
    let part = partial(dest);
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(failed("creating the download folder"))?;
    }
    if let Some(path) = source.url.strip_prefix(LOCAL) {
        copy(Path::new(path), source.bytes, &part, progress)?;
    } else {
        transfer(source, &part, progress)?;
    }

    let actual = if source.sha256.is_empty() {
        String::new()
    } else {
        sha256_of(&part)?
    };
    if !actual.eq_ignore_ascii_case(source.sha256) {
        let _ = std::fs::remove_file(&part);
        return Err(Error::Install(format!(
            "{} failed its integrity check and was deleted (expected SHA-256 {}, got {actual})",
            file_name(dest),
            source.sha256
        )));
    }
    // Replacing an older copy must not fail just because it is there.
    let _ = std::fs::remove_file(dest);
    std::fs::rename(&part, dest).map_err(failed("moving the download into place"))
}

/// What the address of a file on this PC starts with.
const LOCAL: &str = "file:";

/// Copies the file at `from`, which must still have its size of `bytes`, to
/// `part`, starting over each time.
fn copy(from: &Path, bytes: u64, part: &Path, progress: &mut dyn FnMut(u64) -> bool) -> Result<()> {
    let mut source = File::open(from)
        .map_err(|err| Error::Install(format!("{} cannot be read: {err}", from.display())))?;
    let size = source.metadata().map_or(0, |meta| meta.len());
    if size != bytes {
        return Err(Error::Install(format!(
            "{} has changed since it was added; remove it and add it again",
            file_name(from)
        )));
    }
    let mut file = File::create(part).map_err(failed("opening the download file"))?;
    let mut buffer = vec![0u8; 1 << 20];
    let mut have = 0u64;
    loop {
        if !progress(have) {
            return Err(stopped());
        }
        let read = source
            .read(&mut buffer)
            .map_err(failed("reading the file"))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .map_err(failed("writing the copy to disk"))?;
        have += read as u64;
    }
    file.flush().map_err(failed("writing the copy to disk"))?;
    if have == bytes {
        Ok(())
    } else {
        Err(Error::Install(format!(
            "{} changed while it was copied; try again",
            file_name(from)
        )))
    }
}

/// Where an unfinished download of `dest` is kept.
#[must_use]
pub fn partial(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_owned();
    name.push(".part");
    PathBuf::from(name)
}

fn transfer(source: Source<'_>, part: &Path, progress: &mut dyn FnMut(u64) -> bool) -> Result<()> {
    let mut have = std::fs::metadata(part).map_or(0, |meta| meta.len());
    if have > source.bytes {
        have = 0;
    }
    if have == source.bytes && have > 0 {
        progress(have);
        return Ok(());
    }
    let range = format!("bytes={have}-");
    let headers = [("Range", range.as_str())];
    let mut response = Request {
        url: source.url,
        form: None,
        json: None,
        headers: if have > 0 { &headers } else { &[] },
        connect_timeout: CONNECT_TIMEOUT,
        stall_timeout: STALL_TIMEOUT,
    }
    .send()
    .map_err(failed("connecting to the download server"))?;
    let status = response.status();
    let mut file = match status {
        206 if have > 0 => OpenOptions::new().append(true).open(part),
        // The server ignored the range and is sending the whole file.
        200 => {
            have = 0;
            File::create(part)
        }
        _ => {
            return Err(Error::Install(format!(
                "the download server answered HTTP {status} for {}",
                source.url
            )));
        }
    }
    .map_err(failed("opening the download file"))?;

    if !progress(have) {
        return Err(stopped());
    }
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let read = response
            .read(&mut buffer)
            .map_err(failed("receiving the download"))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .map_err(failed("writing the download to disk"))?;
        have += read as u64;
        if have > source.bytes {
            drop(file);
            let _ = std::fs::remove_file(part);
            return Err(Error::Install(format!(
                "the download is larger than the {} bytes expected",
                source.bytes
            )));
        }
        if !progress(have) {
            return Err(stopped());
        }
    }
    file.flush()
        .map_err(failed("writing the download to disk"))?;
    if have == source.bytes {
        Ok(())
    } else {
        Err(Error::Install(format!(
            "the download ended early, at {have} of {} bytes; try again to continue it",
            source.bytes
        )))
    }
}

/// The SHA-256 of the file at `path`, as lower-case hexadecimal.
///
/// # Errors
///
/// Returns [`Error::Install`] if the file cannot be read.
pub fn sha256_of(path: &Path) -> Result<String> {
    let mut file = File::open(path).map_err(failed("reading the download back"))?;
    let mut context = Context::new(&SHA256);
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(failed("reading the download back"))?;
        if read == 0 {
            break;
        }
        context.update(&buffer[..read]);
    }
    Ok(hex(context.finish().as_ref()))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

fn stopped() -> Error {
    Error::Install("the download was stopped".into())
}

fn failed<E: std::fmt::Display>(action: &'static str) -> impl Fn(E) -> Error {
    move |err| Error::Install(format!("{action} failed: {err}"))
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    const BODY: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    const BODY_SHA256: &str = "74e7e5bb9d22d6db26bf76946d40fff3ea9f0346b884fd0694920fccfad15e33";

    /// Serves `BODY` to each of `requests` connections, honoring a range, and
    /// returns the range headers it saw.
    fn serve(requests: usize) -> std::io::Result<(String, thread::JoinHandle<Vec<String>>)> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let url = format!("http://{}/file.bin", listener.local_addr()?);
        let server = thread::spawn(move || {
            let mut ranges = Vec::new();
            for _ in 0..requests {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let Ok(reading) = stream.try_clone() else {
                    break;
                };
                let mut reader = BufReader::new(reading);
                let mut from = 0usize;
                let mut range = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                        range = value.trim().to_owned();
                        from = range.trim_end_matches('-').parse().unwrap_or(0);
                    }
                }
                let status = if range.is_empty() {
                    "200 OK"
                } else {
                    "206 Partial Content"
                };
                ranges.push(range);
                let body = &BODY[from..];
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(body);
            }
            ranges
        });
        Ok((url, server))
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vinput-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn hashing_matches_a_known_value() -> std::io::Result<()> {
        let dir = scratch("hash");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("abc");
        std::fs::write(&path, b"abc")?;
        assert_eq!(
            sha256_of(&path).ok().as_deref(),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
        std::fs::remove_dir_all(dir)
    }

    #[test]
    fn a_good_download_lands_under_its_name_and_reports_progress() -> std::io::Result<()> {
        let (url, server) = serve(1)?;
        let dir = scratch("good");
        let dest = dir.join("file.bin");
        let source = Source {
            url: &url,
            sha256: BODY_SHA256,
            bytes: BODY.len() as u64,
        };
        let mut seen = Vec::new();
        let result = fetch(source, &dest, &mut |bytes| {
            seen.push(bytes);
            true
        });
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(std::fs::read(&dest)?, BODY);
        assert_eq!(seen.last().copied(), Some(BODY.len() as u64));
        assert!(!partial(&dest).exists());
        let _ = server.join();
        std::fs::remove_dir_all(dir)
    }

    #[test]
    fn a_wrong_checksum_is_refused_and_leaves_nothing_behind() -> std::io::Result<()> {
        let (url, server) = serve(1)?;
        let dir = scratch("bad");
        let dest = dir.join("file.bin");
        let source = Source {
            url: &url,
            sha256: "00",
            bytes: BODY.len() as u64,
        };
        let result = fetch(source, &dest, &mut |_| true);
        assert!(
            matches!(&result, Err(Error::Install(why)) if why.contains("integrity")),
            "{result:?}"
        );
        assert!(!dest.exists() && !partial(&dest).exists());
        let _ = server.join();
        Ok(())
    }

    #[test]
    fn an_interrupted_download_continues_where_it_stopped() -> std::io::Result<()> {
        let (url, server) = serve(1)?;
        let dir = scratch("resume");
        let dest = dir.join("file.bin");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(partial(&dest), &BODY[..10])?;
        let source = Source {
            url: &url,
            sha256: BODY_SHA256,
            bytes: BODY.len() as u64,
        };
        let result = fetch(source, &dest, &mut |_| true);
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(std::fs::read(&dest)?, BODY);
        assert_eq!(server.join().unwrap_or_default(), ["10-"]);
        std::fs::remove_dir_all(dir)
    }

    #[test]
    fn a_file_on_this_pc_is_copied_and_checked() -> std::io::Result<()> {
        let dir = scratch("local");
        std::fs::create_dir_all(&dir)?;
        let from = dir.join("source.bin");
        std::fs::write(&from, BODY)?;
        let url = format!("{LOCAL}{}", from.display());
        let dest = dir.join("sub").join("copy.bin");
        for sha256 in [BODY_SHA256, ""] {
            let source = Source {
                url: &url,
                sha256,
                bytes: BODY.len() as u64,
            };
            let result = fetch(source, &dest, &mut |_| true);
            assert!(result.is_ok(), "{result:?}");
            assert_eq!(std::fs::read(&dest)?, BODY);
        }
        let changed = Source {
            url: &url,
            sha256: "",
            bytes: 3,
        };
        let result = fetch(changed, &dest, &mut |_| true);
        assert!(
            matches!(&result, Err(Error::Install(why)) if why.contains("changed")),
            "{result:?}"
        );
        std::fs::remove_dir_all(dir)
    }

    #[test]
    fn stopping_keeps_the_partial_file() -> std::io::Result<()> {
        let (url, server) = serve(1)?;
        let dir = scratch("stop");
        let dest = dir.join("file.bin");
        let source = Source {
            url: &url,
            sha256: BODY_SHA256,
            bytes: BODY.len() as u64,
        };
        let result = fetch(source, &dest, &mut |bytes| bytes == 0);
        assert!(
            matches!(&result, Err(Error::Install(why)) if why.contains("stopped")),
            "{result:?}"
        );
        assert!(!dest.exists() && partial(&dest).exists());
        let _ = server.join();
        std::fs::remove_dir_all(dir)
    }
}
