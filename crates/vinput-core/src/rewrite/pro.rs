//! Rewriting with the model of `VinPUT` Pro, at Vinput's own server.
//!
//! One request a job: the text and what is to be done with it go out, and
//! the result comes back as it is written, an event for every few words
//! (`cloud/docs/api.md`, "Rewriting"). The server decides what its model is
//! told; nothing here sends instructions for it. It comes with either tier
//! of the subscription and is not counted against the month's hours.
//!
//! The request is made on the thread of the dictation, which has nothing
//! else to do by then: the microphone is closed and the recognizer has
//! answered.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use putlab_core::llm::events;
use serde_json::{Value, json};

use super::{Job, Rewriter};
use crate::cloud::{Account, Code, Failure};
use crate::{Error, Result, log};

/// How long the server may send nothing, before its first words or between
/// any two pieces. It gives up on its own model sooner than this.
const QUIET: Duration = Duration::from_secs(20);

/// What is said when the server's model cannot be had for want of a
/// subscription, or of anyone signed in.
const NEEDS_PLAN: &str =
    "This needs VinPUT Plus or Pro, or a text model on this PC. Choose one under Cleanup.";

/// The model of `VinPUT` Pro.
#[derive(Debug)]
pub(super) struct Pro {
    account: Arc<Account>,
}

impl Pro {
    pub(super) fn new(account: Arc<Account>) -> Self {
        Self { account }
    }
}

/// The request that asks for `job`.
fn request(job: &Job<'_>) -> Value {
    let mut request = json!({
        "mode": job.kind.name(),
        "text": job.text,
        "languages": job.languages,
    });
    if !job.instruction.is_empty() {
        request["instruction"] = job.instruction.into();
    }
    request
}

/// Why the server did not rewrite, in words for the user.
fn failed(failure: &Failure) -> Error {
    match failure {
        Failure::SignedOut
        | Failure::Refused {
            code: Code::SubscriptionRequired | Code::Unauthorized,
            ..
        } => Error::Setup(NEEDS_PLAN.to_owned()),
        Failure::Refused {
            code: Code::TooManyRequests,
            ..
        } => Error::Rewrite("VinPUT is rewriting too much at once. Try again in a moment.".into()),
        Failure::Refused {
            code: Code::BadRequest,
            ..
        } => Error::Rewrite("The text is too long to rewrite.".into()),
        Failure::Unreachable(_) => {
            Error::Rewrite("The VinPUT server could not be reached. Check the connection.".into())
        }
        _ => Error::Rewrite("The VinPUT server could not rewrite this just now.".into()),
    }
}

impl Rewriter for Pro {
    fn rewrite(&self, job: &Job<'_>, stop: &AtomicBool, wrote: &mut dyn FnMut(&str)) -> Result<()> {
        let body = request(job).to_string();
        let mut response = self
            .account
            .open("/v1/rewrite", &body, QUIET)
            .map_err(|failure| {
                // The failure's own words may name the server's; the log
                // has them, the user gets ours.
                log::write(&format!("rewriting ({}): {failure}", job.kind.name()));
                failed(&failure)
            })?;
        let cut_off = || Error::Rewrite("The VinPUT server stopped in the middle.".to_owned());
        let mut done = false;
        let mut broke = false;
        let read = events(&mut response, stop, &mut |data| {
            let Ok(event) = serde_json::from_str::<Value>(data) else {
                return true;
            };
            if let Some(text) = event["text"].as_str() {
                if !text.is_empty() {
                    wrote(text);
                }
                true
            } else if event["done"].as_bool() == Some(true) {
                done = true;
                false
            } else if event.get("error").is_some() {
                broke = true;
                false
            } else {
                // An event this version does not know.
                true
            }
        });
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        if read.is_err() || broke || !done {
            log::write(&format!(
                "rewriting ({}): the answer was cut off",
                job.kind.name()
            ));
            return Err(cut_off());
        }
        Ok(())
    }

    fn make_ready(&self) {
        // A token that would have to be traded first costs a round trip
        // after the key is let go; it is traded now, while the user speaks.
        if self.account.is_ready() {
            return;
        }
        let account = Arc::clone(&self.account);
        let _ = std::thread::Builder::new()
            .name("vinput-pro".into())
            .spawn(move || {
                let _ = account.access_token();
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::fake::Fake;
    use crate::rewrite::Kind;

    type Outcome = std::io::Result<()>;

    fn written(pro: &Pro, job: &Job<'_>) -> (Result<()>, String) {
        let mut whole = String::new();
        let stop = AtomicBool::new(false);
        let result = pro.rewrite(job, &stop, &mut |more| whole.push_str(more));
        (result, whole)
    }

    /// The model of a stand-in server, with someone signed in.
    fn signed_in(fake: &Fake) -> Pro {
        let (account, memory) = fake.account();
        memory.sign_in(fake);
        Pro::new(account)
    }

    #[test]
    fn a_dictation_is_sent_with_its_mode_and_comes_back_as_it_is_written() -> Outcome {
        let fake = Fake::start()?;
        let pro = signed_in(&fake);
        let languages = ["sl-SI".to_owned()];
        let job = Job::tidy(Kind::Clean, "um hello world", &languages);
        let (result, whole) = written(&pro, &job);
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(whole, "Hello, world.");
        let asked = fake.read(|state| state.rewrites.clone());
        assert_eq!(
            asked,
            [json!({ "mode": "clean", "text": "um hello world", "languages": ["sl-SI"] })]
        );
        assert_eq!(fake.asked("POST /v1/rewrite"), 1);
        Ok(())
    }

    #[test]
    fn an_edit_is_sent_with_its_instruction() -> Outcome {
        let fake = Fake::start()?;
        let pro = signed_in(&fake);
        let job = Job::command("A paragraph.", "make it shorter", &[]);
        assert!(written(&pro, &job).0.is_ok());
        let asked = fake.read(|state| state.rewrites.clone());
        assert_eq!(asked[0]["mode"], "command");
        assert_eq!(asked[0]["text"], "A paragraph.");
        assert_eq!(asked[0]["instruction"], "make it shorter");
        Ok(())
    }

    #[test]
    fn without_a_subscription_or_a_sign_in_it_says_what_to_choose() -> Outcome {
        let fake = Fake::start()?;
        // Nobody is signed in.
        let (account, _memory) = fake.account();
        let job = Job::tidy(Kind::Clean, "hello", &[]);
        let (result, whole) = written(&Pro::new(account), &job);
        assert!(matches!(&result, Err(Error::Setup(why)) if why == NEEDS_PLAN));
        assert!(whole.is_empty());
        assert_eq!(fake.asked("POST /v1/rewrite"), 0);
        // Signed in, and not subscribed.
        let pro = signed_in(&fake);
        fake.change(|state| {
            state
                .faults
                .push(("POST /v1/rewrite", 402, "subscription_required"));
        });
        let (result, _) = written(&pro, &job);
        assert!(matches!(&result, Err(Error::Setup(why)) if why == NEEDS_PLAN));
        Ok(())
    }

    #[test]
    fn a_server_that_cannot_rewrite_is_an_error_in_words_and_never_its_own() -> Outcome {
        let fake = Fake::start()?;
        let pro = signed_in(&fake);
        let job = Job::tidy(Kind::Clean, "hello", &[]);
        for (status, code) in [
            (502, "upstream_failed"),
            (503, "unavailable"),
            (429, "too_many_requests"),
            (400, "bad_request"),
        ] {
            fake.change(|state| state.faults.push(("POST /v1/rewrite", status, code)));
            let (result, whole) = written(&pro, &job);
            let Err(Error::Rewrite(why)) = result else {
                return Err(std::io::Error::other(format!("{code}: not a failure")));
            };
            assert!(!why.contains("not now") && !why.contains(code), "{why}");
            assert!(whole.is_empty());
        }
        Ok(())
    }

    #[test]
    fn an_answer_that_breaks_off_or_ends_without_saying_so_is_not_taken_for_whole() -> Outcome {
        let fake = Fake::start()?;
        let pro = signed_in(&fake);
        let job = Job::tidy(Kind::Clean, "hello", &[]);
        let failure = json!({"error": {"code": "upstream_failed", "message": "failed"}});
        for cut in [
            vec![json!({"text": "So far "}), failure],
            vec![json!({"text": "So far "})],
        ] {
            fake.change(|state| state.rewritten = cut.clone());
            let (result, whole) = written(&pro, &job);
            assert!(matches!(result, Err(Error::Rewrite(_))), "{cut:?}");
            // What arrived was handed on; whoever asked knows it is not all.
            assert_eq!(whole, "So far ");
        }
        // An event this version does not know is passed over.
        fake.change(|state| {
            state.rewritten = vec![
                json!({"usage": 12}),
                json!({"text": "All of it."}),
                json!({"done": true}),
            ];
        });
        let (result, whole) = written(&pro, &job);
        assert!(result.is_ok());
        assert_eq!(whole, "All of it.");
        Ok(())
    }

    #[test]
    fn a_token_the_server_no_longer_takes_is_traded_once_and_the_job_done() -> Outcome {
        let fake = Fake::start()?;
        let pro = signed_in(&fake);
        let job = Job::tidy(Kind::Clean, "hello", &[]);
        assert!(written(&pro, &job).0.is_ok());
        fake.change(|state| {
            state.faults.push(("POST /v1/rewrite", 401, "unauthorized"));
        });
        let (result, whole) = written(&pro, &job);
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(whole, "Hello, world.");
        assert_eq!(fake.asked("POST /v1/auth/token"), 2);
        Ok(())
    }

    #[test]
    fn a_job_that_is_called_off_ends_without_an_error() -> Outcome {
        let fake = Fake::start()?;
        let pro = signed_in(&fake);
        let job = Job::tidy(Kind::Clean, "hello", &[]);
        let stop = AtomicBool::new(true);
        let mut count = 0;
        let result = pro.rewrite(&job, &stop, &mut |_| count += 1);
        assert!(result.is_ok());
        assert_eq!(count, 0);
        Ok(())
    }
}
