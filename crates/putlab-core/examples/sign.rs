//! Signs a release, with the key only its publisher has.
//!
//! ```text
//! cargo run -p putlab-core --example sign -- new-key <key file>
//! cargo run -p putlab-core --example sign -- public <key file>
//! cargo run -p putlab-core --example sign -- release <key file> <setup> <version> <sha256> <bytes>
//! ```
//!
//! `new-key` makes a key and writes it to a file that must not exist yet;
//! it is made once, and whoever has that file can publish updates, so it is
//! kept out of the repository and off the site. `public` prints the half of
//! it that checks signatures, as the line that goes into
//! `crates/putlab-core/src/update.rs`. `release` prints the signature of one
//! release, in base64, for the `signature` of its manifest; `setup` is what
//! the name of the setup program begins with, such as `VinputSetup`
//! (`scripts/release.ps1` does this). The key itself is never printed.
#![expect(
    clippy::print_stdout,
    reason = "a command-line tool: what it prints is its result"
)]

use std::path::Path;
use std::process::ExitCode;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use putlab_core::update::signed_text;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let done = match args[..] {
        ["new-key", file] => new_key(Path::new(file)),
        ["public", file] => key(Path::new(file)).map(|key| public(&key)),
        ["release", file, setup, version, sha256, bytes] => {
            release(Path::new(file), setup, version, sha256, bytes)
        }
        _ => Err("usage: sign new-key <key file> | public <key file> | \
                  release <key file> <setup> <version> <sha256> <bytes>"
            .to_owned()),
    };
    match done {
        Ok(printed) => {
            println!("{printed}");
            ExitCode::SUCCESS
        }
        Err(why) => {
            eprintln!("error: {why}");
            ExitCode::FAILURE
        }
    }
}

/// Makes a key and keeps it in `file`, which must not be there yet.
fn new_key(file: &Path) -> Result<String, String> {
    let made = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .map_err(|_| "no key could be made".to_owned())?;
    let mut kept = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(file)
        .map_err(|err| format!("{}: {err}", file.display()))?;
    std::io::Write::write_all(&mut kept, made.as_ref())
        .and_then(|()| kept.sync_all())
        .map_err(|err| format!("{}: {err}", file.display()))?;
    key(file).map(|key| public(&key))
}

/// The key in `file`.
fn key(file: &Path) -> Result<Ed25519KeyPair, String> {
    let kept = std::fs::read(file).map_err(|err| format!("{}: {err}", file.display()))?;
    Ed25519KeyPair::from_pkcs8(&kept).map_err(|_| format!("{} holds no key", file.display()))
}

/// The half of `key` that checks signatures, as a line of Rust.
fn public(key: &Ed25519KeyPair) -> String {
    let bytes: Vec<String> = key
        .public_key()
        .as_ref()
        .iter()
        .map(|byte| format!("0x{byte:02x}"))
        .collect();
    format!("[{}]", bytes.join(", "))
}

/// The signature of one release, in base64.
fn release(
    file: &Path,
    setup: &str,
    version: &str,
    sha256: &str,
    bytes: &str,
) -> Result<String, String> {
    let bytes: u64 = bytes
        .parse()
        .map_err(|_| format!("{bytes} is not a number of bytes"))?;
    if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("{sha256} is not a SHA-256"));
    }
    let text = signed_text(setup, version, sha256, bytes);
    Ok(BASE64.encode(key(file)?.sign(text.as_bytes())))
}
