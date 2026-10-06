//! Access tokens for Google Cloud, from the sign-in the Google Cloud CLI
//! leaves on the PC or from a service account key.
//!
//! This is the one thing Vinput needs from Google's sign-in machinery: read
//! the credentials file, exchange what is in it for an access token at
//! Google's token address, and keep the token until shortly before it
//! expires. A service account key chosen in Settings is kept in the Windows
//! credential store and comes first. Otherwise the file is found the way
//! every Google client finds it: the path in `GOOGLE_APPLICATION_CREDENTIALS`,
//! else what `gcloud auth application-default login` wrote.

use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::SystemRandom;
use ring::rsa::KeyPair;
use ring::signature::RSA_PKCS1_SHA256;
use serde_json::{Value, json};

use crate::{Error, Result, net, secrets};

/// Environment variable that names the credentials file.
const ENV_CREDENTIALS: &str = "GOOGLE_APPLICATION_CREDENTIALS";
/// Where the Google Cloud CLI keeps the sign-in, under `%APPDATA%`.
const CLI_CREDENTIALS: &str = r"gcloud\application_default_credentials.json";
/// Where a sign-in is exchanged for an access token.
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
/// What the token may be used for.
const SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
/// How long a signed request for a token stays valid; the longest Google takes.
const ASSERTION_LIFETIME: Duration = Duration::from_secs(3600);
/// A token this close to its end is replaced before it is used again.
pub(super) const REFRESH_MARGIN: Duration = Duration::from_secs(300);
pub(in crate::stt) const TIMEOUT: Duration = Duration::from_secs(20);
/// A token answer is a kilobyte or two.
pub(in crate::stt) const MAX_ANSWER_BYTES: usize = 64 * 1024;

/// What a credentials file holds.
pub(in crate::stt) enum Key {
    /// A person signed in with the Google Cloud CLI.
    User {
        client_id: String,
        client_secret: String,
        refresh_token: String,
    },
    /// A service account and its private key.
    ServiceAccount {
        email: String,
        key_id: String,
        key: Box<KeyPair>,
        token_url: String,
    },
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the secrets themselves.
        f.write_str(match self {
            Self::User { .. } => "Key::User",
            Self::ServiceAccount { .. } => "Key::ServiceAccount",
        })
    }
}

fn failed(what: impl std::fmt::Display) -> Error {
    Error::Transcription(format!("signing in to Google Cloud failed: {what}"))
}

impl Key {
    /// Reads the service account key kept in the credential store, or else
    /// the credentials file of this PC.
    pub(in crate::stt) fn load() -> Result<Self> {
        match secrets::get(secrets::GOOGLE_CLOUD_KEY) {
            Some(stored) => Self::parse(&stored),
            None => Self::from_file(),
        }
    }

    /// Reads the credentials file of this PC.
    pub(in crate::stt) fn from_file() -> Result<Self> {
        let path = credentials_file().ok_or_else(|| failed("no credentials file is set"))?;
        let text = std::fs::read_to_string(&path).map_err(|_| {
            failed(format!(
                "no sign-in was found at {}; run `gcloud auth login --update-adc`",
                path.display()
            ))
        })?;
        Self::parse(&text)
    }

    /// Reads the text of a credentials file.
    pub(in crate::stt) fn parse(text: &str) -> Result<Self> {
        let file: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))
            .map_err(|_| failed("the credentials file is not valid JSON"))?;
        let field = |name: &str| {
            file.get(name)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| failed(format!("the credentials file has no {name}")))
        };
        match file.get("type").and_then(Value::as_str) {
            Some("authorized_user") => Ok(Self::User {
                client_id: field("client_id")?,
                client_secret: field("client_secret")?,
                refresh_token: field("refresh_token")?,
            }),
            Some("service_account") => {
                let token_url = field("token_uri").unwrap_or_else(|_| TOKEN_URL.to_owned());
                if !token_url.starts_with("https://") {
                    return Err(failed(
                        "the credentials file names a token address that is not HTTPS",
                    ));
                }
                Ok(Self::ServiceAccount {
                    email: field("client_email")?,
                    key_id: field("private_key_id")?,
                    key: Box::new(private_key(&field("private_key")?)?),
                    token_url,
                })
            }
            Some(other) => Err(failed(format!(
                "credentials of the kind {other:?} are not supported; sign in with `gcloud auth login --update-adc` or use a service account key"
            ))),
            None => Err(failed("the credentials file does not say what kind it is")),
        }
    }

    /// The address to post to and the form to post, for a token asked for at
    /// `now` (seconds since 1970).
    pub(in crate::stt) fn request(&self, now: u64) -> Result<(String, String)> {
        match self {
            Self::User {
                client_id,
                client_secret,
                refresh_token,
            } => Ok((
                TOKEN_URL.to_owned(),
                form(&[
                    ("grant_type", "refresh_token"),
                    ("client_id", client_id),
                    ("client_secret", client_secret),
                    ("refresh_token", refresh_token),
                ]),
            )),
            Self::ServiceAccount {
                email,
                key_id,
                key,
                token_url,
            } => {
                let assertion = assertion(email, key_id, key, token_url, now)?;
                Ok((
                    token_url.clone(),
                    form(&[
                        ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                        ("assertion", &assertion),
                    ]),
                ))
            }
        }
    }
}

/// Where the credentials file of this PC is, whether or not it exists.
pub(in crate::stt) fn credentials_file() -> Option<PathBuf> {
    std::env::var_os(ENV_CREDENTIALS)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(std::env::var_os("APPDATA")?).join(CLI_CREDENTIALS)))
}

/// What of a service account key file is needed to ask for tokens, as a
/// small file of the same kind: it has to fit in the credential store. With
/// it come the address of the account and the project the file names.
///
/// # Errors
///
/// Returns [`Error::Transcription`] if `text` is not a service account key
/// that can be used.
pub(in crate::stt) fn compact(text: &str) -> Result<(String, String, Option<String>)> {
    let Key::ServiceAccount { email, .. } = Key::parse(text)? else {
        return Err(failed("the file is not a service account key"));
    };
    let file: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|_| failed("the credentials file is not valid JSON"))?;
    let mut kept = serde_json::Map::new();
    for name in [
        "type",
        "client_email",
        "private_key_id",
        "private_key",
        "token_uri",
    ] {
        if let Some(value) = file.get(name) {
            kept.insert(name.to_owned(), value.clone());
        }
    }
    let project = file
        .get("project_id")
        .and_then(Value::as_str)
        .filter(|project| !project.is_empty())
        .map(str::to_owned);
    Ok((Value::Object(kept).to_string(), email, project))
}

/// The address of the service account a key file, or what [`compact`] made
/// of one, belongs to.
pub(in crate::stt) fn account_of(text: &str) -> Option<String> {
    let file: Value = serde_json::from_str(text).ok()?;
    Some(file.get("client_email")?.as_str()?.to_owned())
}

/// The private key in a service account file: PKCS#8 in PEM.
fn private_key(pem: &str) -> Result<KeyPair> {
    let body: String = pem
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("-----"))
        .collect();
    let der = STANDARD
        .decode(body)
        .map_err(|_| failed("the private key in the credentials file is not readable"))?;
    KeyPair::from_pkcs8(&der).map_err(|err| {
        failed(format!(
            "the private key in the credentials file was refused ({err})"
        ))
    })
}

/// A request for a token, signed with the service account's key: a JSON Web
/// Token with the RS256 signature Google asks for.
fn assertion(email: &str, key_id: &str, key: &KeyPair, audience: &str, now: u64) -> Result<String> {
    let header = json!({ "alg": "RS256", "typ": "JWT", "kid": key_id });
    let claims = json!({
        "iss": email,
        "scope": SCOPE,
        "aud": audience,
        "iat": now,
        "exp": now + ASSERTION_LIFETIME.as_secs(),
    });
    let mut token = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header.to_string()),
        URL_SAFE_NO_PAD.encode(claims.to_string())
    );
    let mut signature = vec![0u8; key.public().modulus_len()];
    key.sign(
        &RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        token.as_bytes(),
        &mut signature,
    )
    .map_err(|_| failed("the token request could not be signed"))?;
    token.push('.');
    token.push_str(&URL_SAFE_NO_PAD.encode(signature));
    Ok(token)
}

/// `name=value` pairs joined for a form post, percent-encoded.
fn form(fields: &[(&str, &str)]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::new();
    for (name, value) in fields {
        if !out.is_empty() {
            out.push('&');
        }
        out.push_str(name);
        out.push('=');
        for &byte in value.as_bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                out.push(char::from(byte));
            } else {
                out.push('%');
                out.push(char::from(HEX[usize::from(byte >> 4)]));
                out.push(char::from(HEX[usize::from(byte & 15)]));
            }
        }
    }
    out
}

/// Reads what the token address answered: the token and how long it lasts.
pub(in crate::stt) fn read_answer(status: u16, body: &[u8]) -> Result<(String, Duration)> {
    let answer: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let text = |name: &str| answer.get(name).and_then(Value::as_str);
    if status != 200 {
        let why = text("error").unwrap_or("no reason given");
        let detail = text("error_description").unwrap_or_default();
        return Err(failed(format!(
            "Google answered HTTP {status} ({why}{}{detail}); if the sign-in has expired, run `gcloud auth login --update-adc` again",
            if detail.is_empty() { "" } else { ": " }
        )));
    }
    let token = text("access_token")
        .filter(|token| !token.is_empty())
        .ok_or_else(|| failed("Google's answer has no access token"))?;
    let seconds = answer
        .get("expires_in")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Ok((token.to_owned(), Duration::from_secs(seconds)))
}

/// A token and when to stop using it.
struct Held {
    token: String,
    until: Instant,
}

/// The access tokens of one sign-in: the newest is kept and handed out until
/// it is close to expiring.
pub(super) struct Tokens {
    key: Key,
    held: Mutex<Option<Held>>,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl Tokens {
    /// Reads the sign-in of this PC. Nothing is asked of Google yet.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transcription`] if there is no usable credentials file.
    pub(super) fn load() -> Result<Self> {
        Ok(Self {
            key: Key::load()?,
            held: Mutex::default(),
        })
    }

    /// A token that is good for at least [`REFRESH_MARGIN`], from memory if
    /// there is one. Otherwise this asks Google and waits for the answer, so
    /// it must not run on a thread that other work is waiting for.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transcription`] if Google cannot be reached or
    /// refuses the sign-in.
    pub(super) fn token(&self) -> Result<String> {
        // Held while asking, so two sessions that start together ask once.
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(held) = held.as_ref()
            && held.until > Instant::now() + REFRESH_MARGIN
        {
            return Ok(held.token.clone());
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        let (url, body) = self.key.request(now)?;
        let asked = Instant::now();
        let (status, answer) =
            net::post_form(&url, &body, TIMEOUT, MAX_ANSWER_BYTES).map_err(failed)?;
        let (token, lifetime) = read_answer(status, &answer)?;
        *held = Some(Held {
            token: token.clone(),
            until: asked + lifetime,
        });
        Ok(token)
    }

    /// How long the token in memory can still be handed out; zero if there
    /// is none or it is due for replacement.
    pub(super) fn fresh_for(&self) -> Duration {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map_or(Duration::ZERO, |held| {
                held.until
                    .saturating_duration_since(Instant::now() + REFRESH_MARGIN)
            })
    }
}

#[cfg(test)]
mod tests {
    use ring::signature::{RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};

    use super::*;

    /// A key made for these tests and used nowhere else.
    const TEST_KEY: &str = "-----BEGIN PRIVATE KEY-----\n\
MIIEvwIBADANBgkqhkiG9w0BAQEFAASCBKkwggSlAgEAAoIBAQC6wRwPYzTg61Xm\n\
PA42s33+blNeKiyizLQjDo+b07f4XujHyaDALp3j/BAWLDljc13wG1at2ArFZddY\n\
5V9LGlc9N+YvgcJ30zonM/beVnyIHX4gGhA7dLgsDgW9nKjv1oh7Q+F/ClSM0gFd\n\
JIYUjNF+IMtRaWyX0N832n748RZzVCoySv7wnycr6aGVAeunCfX5y1IFfSn5mmx2\n\
aPliTQwlb6kRK8UUA675TUFppoPuW1tqJO5NfHY47J/JgJhW/akEPuSZasf6PRwE\n\
gg/RhArmjWvnBtTInnl79Pj0/nqavKIQB+svInejq9bcZc27dcP6PVnV5nQk+NEn\n\
N1+1eg2LAgMBAAECggEAPMvW7a93LG2QZkZQvkAKmUSjQTlrlIgdcHgyCiGPd3y2\n\
PSwHd+j1H54UQpOdDyIkh6Duw+NcBk0UIO1y60kk81n6Rqbwfv+1o8OH73OqUMmB\n\
IMQ7Wo+x0S2zcrnISWPGxJIhm7DqO3m2VfMo7xG0tWHJrrQqVPX7ZPxtb29UxrAl\n\
eYbkEQueICr0cNi7sdXXnm/7c6c2Mp17Y5ZL/gU3TNRMN7iaLW7ASiX7bh7QEc46\n\
3B13hAyai0TXRuUG9MlhfGp0RRBuF5ukYWdc6OgU6g3gjpZy06mvZ2InwB0mPqdW\n\
j7UjdVYwh9hUDOMW/zS6eXZ6QB1Lxk+3Bx6aq2rt4QKBgQDyZ8+ZZtaD4cvg7/vt\n\
smyqvaK5AzU2giKfKZw9//femIqHEECWuqVsjO5PMSqfosqwpCuer9XhdNjxgoF4\n\
kAWVsc51hJ6M7W/NnqDiN6p4dNAws7VW/6ry2l1VDftk4YVN7iyqSOZi+0Ws40q4\n\
ndyDN4ZifRAsvB0+QrDTjIr00QKBgQDFOlImE3DWsjHznnhGzphPDzDdyrq8YYyu\n\
JkxrRknVwLAVm7uU/Rik7MHxiRQoWLBArGl1ay5kMfvR8RRjZOieDsDRxG6BDVVH\n\
njG8b0DZohLr1UUE8/xbPeW9k8BzI3kLgFpJHO4+juD2r73pQWg6h+JGNbJ8sK2g\n\
FOXxTLtjmwKBgQCm8ng33mu/SLNxp9aKTmgo75IyedMcR2Qw2ieRS8katVQy0S6q\n\
oMFtkWJPiD0i1KtkHK0uDUvwitke/BLPReGSx3K53SyVQrHNm4KSSmRNsaXw5NPv\n\
pwXFXD8LZSXOxMNaeL6qSKrjYMsL1z6PB6nndUZIbnnFVYmOZ8AA8rqBAQKBgQCX\n\
0BKXuHMs77E/gwW2CyR2Bo1oy7n9c2EkU9qS4GHJJS7lEyDKyB8GJDANSaS4gbv5\n\
rDzo4v2spkTeDyHQeEYOE/EcBoXA1JCGNiC2mTRj0TubSGh+LYhce0KyMF78wZtU\n\
3RGiIaDJipVjjVcziuM92oaz4HVpQV4EUNHGcXf0bQKBgQDILZ3kBF/USNVVk8Z7\n\
iknX3ZU+kuM0AcsVvnumUrq/hYX6Y8GFIiTJun2uYmJIuv59nuEpC8p1BZEcydhz\n\
fvIBjhdC/2ie9XJO258uUzJnjPiuw+Ez+Cvlg/uGclTvTo8OkMkR2PUpQ0aDahEt\n\
w57MHX/CKB7vjH/tcHgNd02mfQ==\n\
-----END PRIVATE KEY-----\n";

    fn service_account_file() -> String {
        json!({
            "type": "service_account",
            "project_id": "demo",
            "private_key_id": "key-1",
            "private_key": TEST_KEY,
            "client_email": "vinput@demo.iam.gserviceaccount.com",
            "token_uri": "https://oauth2.googleapis.com/token",
        })
        .to_string()
    }

    fn decoded(part: &str) -> Value {
        URL_SAFE_NO_PAD
            .decode(part)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or(Value::Null)
    }

    #[test]
    fn a_cli_sign_in_asks_with_its_refresh_token() {
        let key = Key::parse(
            r#"{"type":"authorized_user","client_id":"id.apps","client_secret":"s/e+c",
                "refresh_token":"1//r&t=x","quota_project_id":"demo"}"#,
        );
        assert!(key.is_ok(), "{key:?}");
        let Ok(key) = key else { return };
        assert_eq!(
            key.request(1_800_000_000).ok(),
            Some((
                "https://oauth2.googleapis.com/token".to_owned(),
                "grant_type=refresh_token&client_id=id.apps&client_secret=s%2Fe%2Bc\
                 &refresh_token=1%2F%2Fr%26t%3Dx"
                    .to_owned()
            ))
        );
    }

    #[test]
    fn a_service_account_asks_with_a_token_it_signed() {
        let key = Key::parse(&service_account_file());
        assert!(key.is_ok(), "{key:?}");
        let Ok(key) = key else { return };
        let request = key.request(1_800_000_000);
        assert!(request.is_ok(), "{request:?}");
        let Ok((url, body)) = request else { return };
        assert_eq!(url, "https://oauth2.googleapis.com/token");
        let assertion = body
            .strip_prefix(
                "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion=",
            )
            .unwrap_or_default();
        // The same bytes `openssl dgst -sha256 -sign` makes with this key:
        // RS256 has no randomness in it.
        assert_eq!(assertion, EXPECTED_ASSERTION);

        let parts: Vec<&str> = assertion.split('.').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(
            decoded(parts[0]),
            json!({ "alg": "RS256", "typ": "JWT", "kid": "key-1" })
        );
        assert_eq!(
            decoded(parts[1]),
            json!({
                "iss": "vinput@demo.iam.gserviceaccount.com",
                "scope": "https://www.googleapis.com/auth/cloud-platform",
                "aud": "https://oauth2.googleapis.com/token",
                "iat": 1_800_000_000u64,
                "exp": 1_800_003_600u64,
            })
        );
        // And the signature is the key's own, over the first two parts.
        let Key::ServiceAccount { key, .. } = &key else {
            return;
        };
        let signed = format!("{}.{}", parts[0], parts[1]);
        let signature = URL_SAFE_NO_PAD.decode(parts[2]).unwrap_or_default();
        let public = UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, key.public().as_ref());
        assert!(public.verify(signed.as_bytes(), &signature).is_ok());
        assert!(public.verify(b"something else", &signature).is_err());
    }

    const EXPECTED_ASSERTION: &str = "eyJhbGciOiJSUzI1NiIsImtpZCI6ImtleS0xIiwidHlwIjoiSldUIn0.eyJhdWQiOiJodHRwczovL29hdXRoMi5nb29nbGVhcGlzLmNvbS90b2tlbiIsImV4cCI6MTgwMDAwMzYwMCwiaWF0IjoxODAwMDAwMDAwLCJpc3MiOiJ2aW5wdXRAZGVtby5pYW0uZ3NlcnZpY2VhY2NvdW50LmNvbSIsInNjb3BlIjoiaHR0cHM6Ly93d3cuZ29vZ2xlYXBpcy5jb20vYXV0aC9jbG91ZC1wbGF0Zm9ybSJ9.TekqijKWDpcSN1veTQK5GWVcKWv-q6ifGKAk4E8a1syCFlxaevzCH-DQ0_5smuep3_W2YpVwscBF26q7Ska-OUnx8bp7Nb6_8QYl9NC9knvqhhPSeKEBOSCabjZ9Rkm7oP58pIsM92ZCZ57S45Y9w8Zch-MpPIE91iRJCeh06S-pqzuAKVgNpSJrnhPOUiG1XfupPZkVi0JKEmGwgxylrxDz9aIVvhsDhpjU5tqUIT5gBdS6JiIgGgCKmsxKWwRhBZY-Rgm8eImizu5EF_FuR3wYStMFva8l4QfbXYVDqX9MBwT3Zj2JPAzOUpNp_Hhy5ob68gAxwVm3yqieBoS3sg";

    #[test]
    fn a_key_file_is_cut_down_to_what_asking_for_tokens_needs() {
        let mut file: Value = serde_json::from_str(&service_account_file()).unwrap_or_default();
        file["client_id"] = json!("1234567890");
        file["universe_domain"] = json!("googleapis.com");
        let kept = compact(&file.to_string());
        assert!(kept.is_ok(), "{kept:?}");
        let Ok((kept, email, project)) = kept else {
            return;
        };
        assert_eq!(email, "vinput@demo.iam.gserviceaccount.com");
        assert_eq!(project.as_deref(), Some("demo"));
        assert!(!kept.contains("client_id") && !kept.contains("universe"));
        assert_eq!(account_of(&kept), Some(email));
        // What is kept asks for the same token the whole file would.
        let whole = Key::parse(&service_account_file()).and_then(|key| key.request(1));
        let small = Key::parse(&kept).and_then(|key| key.request(1));
        assert!(whole.is_ok() && whole.ok() == small.ok());
        // A sign-in made with gcloud is not a key to keep.
        let user =
            r#"{"type":"authorized_user","client_id":"a","client_secret":"b","refresh_token":"c"}"#;
        assert!(compact(user).is_err() && compact("nonsense").is_err());
    }

    #[test]
    fn files_that_cannot_be_used_say_why() {
        for (file, why) in [
            ("not json", "not valid JSON"),
            ("{}", "what kind"),
            (r#"{"type":"external_account"}"#, "not supported"),
            (
                r#"{"type":"authorized_user","client_id":"a"}"#,
                "client_secret",
            ),
            (
                r#"{"type":"service_account","client_email":"a@b","private_key_id":"k","private_key":"nonsense"}"#,
                "private key",
            ),
        ] {
            let refused = Key::parse(file).map(|_| ()).map_err(|err| err.to_string());
            assert!(
                matches!(&refused, Err(said) if said.contains(why)),
                "{file}: {refused:?}"
            );
        }
        let plain = service_account_file().replace("https://oauth2", "http://oauth2");
        assert!(Key::parse(&plain).is_err());
    }

    #[test]
    fn the_answer_gives_the_token_or_the_reason() {
        let good = read_answer(
            200,
            br#"{"access_token":"ya29.abc","expires_in":3599,"token_type":"Bearer"}"#,
        );
        assert_eq!(
            good.ok(),
            Some(("ya29.abc".to_owned(), Duration::from_secs(3599)))
        );
        let expired = read_answer(
            400,
            br#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#,
        )
        .map_err(|err| err.to_string());
        assert!(
            matches!(&expired, Err(said) if said.contains("invalid_grant") && said.contains("revoked")),
            "{expired:?}"
        );
        assert!(read_answer(200, b"{}").is_err());
        assert!(read_answer(502, b"<html>").is_err());
    }

    #[test]
    fn secrets_stay_out_of_debug_output() {
        let key = Key::parse(
            r#"{"type":"authorized_user","client_id":"id","client_secret":"hush","refresh_token":"quiet"}"#,
        );
        let shown = format!("{key:?}");
        assert!(
            !shown.contains("hush") && !shown.contains("quiet"),
            "{shown}"
        );
    }
}
