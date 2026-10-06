//! Asking Google whether what someone entered for a Google service of their
//! own works, before it is put to use.
//!
//! Each check is one small request that costs nothing: the list of models
//! for a Gemini API key, and for Google Cloud Speech-to-Text an access token
//! and then the list of recognizers of the project in its location. What
//! Google answers is sorted into a [`Verdict`], which says what is wrong in
//! a way Settings can put in words with what to do next.
//!
//! A check waits for the network, so it runs on a thread of the caller's
//! own. Nothing here writes a key anywhere, and no key is part of a
//! [`Verdict`].

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use super::google::{endpoint, token};
use crate::net::{self, Request};
use crate::{Error, Result, secrets};

/// Where the models a Gemini API key may use are listed.
const GEMINI_MODELS: &str = "https://generativelanguage.googleapis.com/v1beta/models?pageSize=1";
/// How long a step of a check may take. Someone is waiting for it.
const TIMEOUT: Duration = Duration::from_secs(10);
/// Either answer is a few kilobytes at most.
const MAX_ANSWER_BYTES: usize = 256 * 1024;
/// The largest key file read: a service account key is about 2 kB.
const MAX_FILE_BYTES: u64 = 64 * 1024;
/// How much of what Google said is passed on when it fits none of the kinds.
const MAX_SAID: usize = 200;

/// What Google made of a key, or of a project and its credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// It works.
    Works,
    /// Google does not know the key, or the sign-in is no longer good.
    WrongKey,
    /// The key or the account is known but may not use this service: the
    /// API is not switched on, the key is restricted, a permission is missing.
    NoAccess,
    /// The project has no billing.
    Billing,
    /// It would work, but the quota is used up for now.
    Quota,
    /// Google does not know the project, or not in this location.
    NotFound,
    /// There is no Google sign-in on this PC and no key was chosen.
    NoSignIn,
    /// Google could not be reached.
    Offline,
    /// Something else, in Google's words or the network's.
    Other(String),
}

/// A service account key read from a file, ready to be kept.
#[derive(Clone, PartialEq, Eq)]
pub struct ServiceAccount {
    /// The address of the account, such as `name@project.iam.gserviceaccount.com`.
    pub email: String,
    /// The project the file names.
    pub project_id: Option<String>,
    /// What goes into the credential store under
    /// [`secrets::GOOGLE_CLOUD_KEY`]. A secret.
    pub key: String,
}

impl std::fmt::Debug for ServiceAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the key itself.
        f.debug_struct("ServiceAccount")
            .field("email", &self.email)
            .finish_non_exhaustive()
    }
}

/// Reads a service account key file.
///
/// # Errors
///
/// Returns [`Error::Transcription`] if the file cannot be read or is not a
/// service account key.
pub fn service_account(path: &Path) -> Result<ServiceAccount> {
    let unreadable = || Error::Transcription(format!("{} could not be read", path.display()));
    let size = std::fs::metadata(path).map_err(|_| unreadable())?.len();
    if size > MAX_FILE_BYTES {
        return Err(Error::Transcription(
            "the file is too large to be a service account key".into(),
        ));
    }
    let text = std::fs::read_to_string(path).map_err(|_| unreadable())?;
    let (key, email, project_id) = token::compact(&text)?;
    Ok(ServiceAccount {
        email,
        project_id,
        key,
    })
}

/// The address of the service account whose key is in the credential store.
#[must_use]
pub fn stored_account() -> Option<String> {
    token::account_of(&secrets::get(secrets::GOOGLE_CLOUD_KEY)?)
}

/// Whether this PC has a Google Cloud sign-in: what the Google Cloud CLI
/// leaves behind, or the file `GOOGLE_APPLICATION_CREDENTIALS` names.
#[must_use]
pub fn sign_in_found() -> bool {
    token::credentials_file().is_some_and(|path| path.is_file())
}

/// Whether `text` can be a Google Cloud project id: nothing in it could
/// turn the address it becomes part of into another one.
#[must_use]
pub fn is_project(text: &str) -> bool {
    let fits = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':');
    !text.is_empty() && text.len() <= 100 && text.chars().all(fits)
}

/// Whether `text` can be a Speech-to-Text location, such as `eu` or
/// `europe-west3`. It becomes part of the name of the server.
#[must_use]
pub fn is_location(text: &str) -> bool {
    let fits = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-';
    !text.is_empty() && text.len() <= 40 && text.chars().all(fits)
}

/// Asks Google whether a Gemini API key is good. Waits for the answer.
#[must_use]
pub fn gemini_key(key: &str) -> Verdict {
    // A key is letters, digits, dashes and underscores; anything else was
    // never one, and must not end up in a header.
    if key.is_empty() || !key.chars().all(|c| c.is_ascii_graphic()) {
        return Verdict::WrongKey;
    }
    match answer(&[("x-goog-api-key", key)], GEMINI_MODELS) {
        // Should Google ever say the key back, it is not passed on.
        Verdict::Other(said) => Verdict::Other(said.replace(key, "(key)")),
        verdict => verdict,
    }
}

/// Asks Google whether `project` can use Speech-to-Text in `location` with
/// the service account `key`, or without one with the sign-in of this PC,
/// whatever key the credential store holds. Waits for the answers.
#[must_use]
pub fn google_cloud(project: &str, location: &str, key: Option<&str>) -> Verdict {
    if !is_project(project) || !is_location(location) {
        return Verdict::NotFound;
    }
    let credentials = match key {
        Some(key) => token::Key::parse(key),
        None if !sign_in_found() => return Verdict::NoSignIn,
        None => token::Key::from_file(),
    };
    let access = match credentials.and_then(|credentials| access_token(&credentials)) {
        Ok(Ok(access)) => access,
        Ok(Err(verdict)) => return verdict,
        Err(err) => return Verdict::Other(said(&err.to_string())),
    };
    let url = format!(
        "{}/v2/projects/{project}/locations/{location}/recognizers?pageSize=1",
        endpoint(location)
    );
    let bearer = format!("Bearer {access}");
    answer(
        &[("Authorization", &bearer), ("x-goog-user-project", project)],
        &url,
    )
}

/// Trades credentials for an access token. The inner error is the verdict
/// when Google or the network said no.
fn access_token(credentials: &token::Key) -> Result<std::result::Result<String, Verdict>> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let (url, form) = credentials.request(now)?;
    let Ok((status, body)) = net::post_form(&url, &form, TIMEOUT, token::MAX_ANSWER_BYTES) else {
        return Ok(Err(Verdict::Offline));
    };
    Ok(match token::read_answer(status, &body) {
        Ok((access, _)) => Ok(access),
        // The sign-in has expired or was taken back, or the key was deleted.
        Err(_) if matches!(status, 400 | 401) => Err(Verdict::WrongKey),
        Err(err) => Err(Verdict::Other(said(&err.to_string()))),
    })
}

/// Makes the request of a check and judges what comes back.
fn answer(headers: &[(&str, &str)], url: &str) -> Verdict {
    let sent = Request {
        headers,
        ..Request::get(url, TIMEOUT)
    }
    .send();
    let Ok(mut response) = sent else {
        return Verdict::Offline;
    };
    let status = response.status();
    match response.body(MAX_ANSWER_BYTES, TIMEOUT) {
        Ok(body) => judge(status, &body),
        Err(_) if status == 200 => Verdict::Works,
        Err(_) => Verdict::Offline,
    }
}

/// At most [`MAX_SAID`] characters of `text`, on one line.
fn said(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    line.chars().take(MAX_SAID).collect()
}

/// Sorts an answer of a Google API by its status and, where Google gives
/// one, by the reason in its error: the status alone says too little, since
/// a key that does not exist and a region that is not served are both 400.
fn judge(status: u16, body: &[u8]) -> Verdict {
    if status == 200 {
        return Verdict::Works;
    }
    let answer: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let error = &answer["error"];
    let message = error["message"].as_str().unwrap_or_default();
    let reason = error["details"]
        .as_array()
        .into_iter()
        .flatten()
        .find_map(|detail| detail["reason"].as_str())
        .unwrap_or_default();
    let billing = || message.to_ascii_lowercase().contains("billing");
    match (reason, status) {
        ("API_KEY_INVALID" | "ACCESS_TOKEN_EXPIRED" | "ACCOUNT_STATE_INVALID", _) | (_, 401) => {
            Verdict::WrongKey
        }
        ("BILLING_DISABLED", _) => Verdict::Billing,
        ("RATE_LIMIT_EXCEEDED" | "RESOURCE_QUOTA_EXCEEDED", _) | (_, 429) => Verdict::Quota,
        (_, 400 | 403) if billing() => Verdict::Billing,
        (_, 400) if message.contains("API key not valid") => Verdict::WrongKey,
        (_, 403) => Verdict::NoAccess,
        (_, 404) => Verdict::NotFound,
        _ if message.is_empty() => Verdict::Other(format!("HTTP {status}")),
        _ => Verdict::Other(said(message)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An error the way Google's APIs write one.
    fn error(code: u16, message: &str, reason: &str) -> Vec<u8> {
        serde_json::json!({ "error": {
            "code": code,
            "message": message,
            "status": "X",
            "details": [
                { "@type": "type.googleapis.com/google.rpc.Help" },
                { "@type": "type.googleapis.com/google.rpc.ErrorInfo", "reason": reason },
            ],
        }})
        .to_string()
        .into_bytes()
    }

    #[test]
    fn an_answer_is_sorted_by_what_is_to_be_done_about_it() {
        assert_eq!(judge(200, b"{\"models\":[]}"), Verdict::Works);
        for (status, message, reason, verdict) in [
            (
                400,
                "API key not valid. Please pass a valid API key.",
                "API_KEY_INVALID",
                Verdict::WrongKey,
            ),
            (400, "API key not valid.", "", Verdict::WrongKey),
            (
                401,
                "Request had invalid authentication credentials.",
                "",
                Verdict::WrongKey,
            ),
            (
                403,
                "Generative Language API has not been used in project 1 before or it is disabled.",
                "SERVICE_DISABLED",
                Verdict::NoAccess,
            ),
            (
                403,
                "Requests to this API are blocked.",
                "API_KEY_SERVICE_BLOCKED",
                Verdict::NoAccess,
            ),
            (
                403,
                "Permission 'speech.recognizers.list' denied",
                "IAM_PERMISSION_DENIED",
                Verdict::NoAccess,
            ),
            (
                403,
                "This API method requires billing to be enabled.",
                "BILLING_DISABLED",
                Verdict::Billing,
            ),
            (
                400,
                "User location is not supported for the API use without a billing account.",
                "",
                Verdict::Billing,
            ),
            (
                429,
                "Quota exceeded.",
                "RATE_LIMIT_EXCEEDED",
                Verdict::Quota,
            ),
            (
                404,
                "Requested entity was not found.",
                "",
                Verdict::NotFound,
            ),
        ] {
            assert_eq!(
                judge(status, &error(status, message, reason)),
                verdict,
                "{message}"
            );
        }
        assert_eq!(
            judge(400, &error(400, "The  region\nis not served.", "")),
            Verdict::Other("The region is not served.".to_owned())
        );
        assert_eq!(judge(503, b"<html>"), Verdict::Other("HTTP 503".to_owned()));
    }

    #[test]
    fn nothing_that_could_change_the_address_is_asked_about() {
        assert!(is_project("my-project-123") && is_project("example.com:my-project"));
        for odd in ["", "a/b", "a b", "a?b=c", "a#b", "pro\u{10d}"] {
            assert!(!is_project(odd), "{odd}");
        }
        assert!(is_location("eu") && is_location("europe-west3") && is_location("global"));
        for odd in ["", "EU", "eu.evil.example", "eu/x", "e u", "eu@x"] {
            assert!(!is_location(odd), "{odd}");
        }
        // Neither is asked about: there is no answer to wait for.
        assert_eq!(google_cloud("a/b", "eu", None), Verdict::NotFound);
        assert_eq!(
            google_cloud("demo", "eu.evil.example", None),
            Verdict::NotFound
        );
        assert_eq!(gemini_key(""), Verdict::WrongKey);
        assert_eq!(gemini_key("key\r\nHost: x"), Verdict::WrongKey);
    }

    #[test]
    fn a_file_that_is_no_key_is_refused_before_anything_is_kept() {
        let folder = std::env::temp_dir().join(format!("vinput-check-{}", std::process::id()));
        assert!(std::fs::create_dir_all(&folder).is_ok());
        let file = folder.join("key.json");
        assert!(service_account(&file).is_err(), "there is no such file");
        assert!(std::fs::write(&file, "{\"type\":\"authorized_user\"}").is_ok());
        assert!(service_account(&file).is_err());
        let _ = std::fs::remove_dir_all(&folder);
        let shown = format!(
            "{:?}",
            ServiceAccount {
                email: "a@b".into(),
                project_id: None,
                key: "hush".into(),
            }
        );
        assert!(shown.contains("a@b") && !shown.contains("hush"));
    }
}
