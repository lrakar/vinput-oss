//! `VinPUT` Pro: the account at Vinput's own server, and the session with it.
//!
//! Pro is speech recognition as a subscription. The person signs in with
//! Google in the browser, never in the app. The app listens on a port of
//! this computer, opens the server's sign-in page, and the browser comes
//! back to that port with a one-time code, which the app trades for a
//! session together with a secret it never let go of (`cloud/loopback.rs`).
//! Only if no port can be listened on does it sign in the older way: it asks
//! the server to start a sign-in, shows the short code the page asks for,
//! and polls until the server hands it a session. A session is two tokens. The refresh token is kept in the Windows credential store and
//! nowhere else: not in the settings file, not in the log. The access token
//! lives in memory for the hour it is good for and is traded for a new one
//! shortly before that hour is up.
//!
//! A refresh token is replaced every time it is used, so there is exactly
//! one [`Account`] in a process ([`shared`]), every refresh goes through one
//! lock, and the next token is in the credential store before the session it
//! came with is used. Every Vinput process that shares the credential store
//! (the resident one and a `vinput transcribe` beside it) takes turns at
//! that under one name (`cloud/turn.rs`). The server keeps the old token
//! working for a minute, so a refresh whose answer was lost is made again,
//! at once and a few times, before that minute is over.
//!
//! Every request here is a plain HTTPS request on the thread that makes it
//! ([`crate::net`]); none is made on the thread of the message loop. The
//! audio itself goes over a WebSocket, in `stt/cloud.rs`.
//!
//! [`Failure`] says what went wrong in terms the app acts on. The words of
//! a failure are for people; no token is ever part of them.

#[cfg(test)]
pub(crate) mod fake;
mod loopback;
mod turn;

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::digest;
use ring::rand::{SecureRandom as _, SystemRandom};
use serde_json::{Value, json};

use crate::net::{Request, Target};
use crate::{log, secrets};

/// Where the server is. A build that ships is given the address in
/// `VINPUT_SERVER` when it is compiled; this source names none, and a build
/// without it has no server to sign in to.
const SERVER: &str = match option_env!("VINPUT_SERVER") {
    Some(address) => address,
    None => "https://vinput-server.invalid",
};

/// Environment variable that names another server, for testing: an
/// `https://` address, or `http://127.0.0.1` with a port. Plain HTTP is
/// taken from nowhere else. Only a build for development or for tests
/// listens to it ([`LISTENS`]): a Vinput that ships talks to [`SERVER`] and
/// to nothing else, whatever its environment says.
pub const ENV_URL: &str = "VINPUT_CLOUD_URL";

/// Whether this build listens to [`ENV_URL`].
const LISTENS: bool = cfg!(any(debug_assertions, test, feature = "test-instance"));

/// The built-in tiers, for as long as the server has not said: Plus at two
/// euros fifty a month or twenty-four a year with eight hours, Pro at five
/// or forty-nine with twenty-five. In cents, and in the order they are
/// offered. No price of a first month is among them: that is the server's
/// alone to promise.
const BUILT_IN: [Offer; 2] = [
    Offer {
        tier: Tier::Plus,
        monthly: 250,
        first: 0,
        yearly: 2400,
        cap_seconds: 8 * 3600,
    },
    Offer {
        tier: Tier::Pro,
        monthly: 500,
        first: 0,
        yearly: 4900,
        cap_seconds: 25 * 3600,
    },
];

/// The id of the model, and the name of the provider, in the settings file.
pub const MODEL_ID: &str = "vinput-pro";

/// How long a request may take to connect, and to answer.
const TIMEOUT: Duration = Duration::from_secs(10);
/// The longest answer the server gives is a few hundred bytes.
const ANSWER_LIMIT: usize = 64 * 1024;
/// An access token with less than this left is not used for a new request.
const EXPIRY_MARGIN: Duration = Duration::from_secs(60);
/// An access token with less than this left is traded for a new one when
/// there is a quiet moment for it: after a dictation, not before one.
const FRESH_FOR: Duration = Duration::from_secs(20 * 60);
/// The shortest wait between two looks at a sign-in, whatever the server asks.
const POLL_FLOOR: Duration = Duration::from_secs(1);
/// How often a wait looks at whether it was called off.
const STOP_STEP: Duration = Duration::from_millis(50);
/// How long a refresh that got no answer waits before it is made again, each
/// time. The server takes the same token again for a minute, so all of them
/// together stay well inside that.
const RETRY_WAITS: [Duration; 3] = [
    Duration::from_millis(500),
    Duration::from_secs(2),
    Duration::from_secs(5),
];
/// How long a refresh waits for another Vinput process that is trading the
/// token before it goes ahead anyway.
const TURN_WAIT: Duration = Duration::from_secs(20);
/// The longest name of this PC the server is told.
const DEVICE_NAME_MAX: usize = 64;
/// How long the person has to sign in with Google once the browser was
/// opened for it: as long as the server keeps the sign-in.
const BROWSER_WAIT: Duration = Duration::from_secs(10 * 60);
/// What is said when the browser came back without anyone signed in.
const NOT_THROUGH: &str =
    "The sign-in did not go through in the browser. Start it again when you are ready.";
/// What is said when Google will not vouch for the account's address.
const UNVERIFIED: &str = "Google has not verified this account's e-mail address. Verify it with \
                          Google, then sign in again.";

/// The address of the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    /// `https://host`, without a slash at the end.
    base: String,
}

impl Server {
    /// The server this copy talks to: the real one, or in a build for
    /// development or tests the one named by [`ENV_URL`] if that names a
    /// usable one.
    #[must_use]
    pub fn from_env() -> Self {
        Self::chosen(std::env::var(ENV_URL).ok().as_deref(), LISTENS)
    }

    /// The server for a copy whose environment names `named`, and that
    /// listens to it or not.
    fn chosen(named: Option<&str>, listens: bool) -> Self {
        named
            .filter(|_| listens)
            .and_then(Self::at)
            .unwrap_or_else(|| Self {
                base: SERVER.to_owned(),
            })
    }

    /// The server at `url`, if that is an address requests are made to:
    /// HTTPS, or plain HTTP on this computer.
    #[must_use]
    pub fn at(url: &str) -> Option<Self> {
        let base = url.trim().trim_end_matches('/');
        let target = Target::parse(base).ok()?;
        // Only the server's own address: a path would be lost, a query
        // would end up in the middle of every address.
        (target.path == "/").then(|| Self {
            base: base.to_owned(),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// The address of the audio stream: the same server, as a WebSocket.
    pub(crate) fn stream_url(&self) -> String {
        let address = match self.base.strip_prefix("https://") {
            Some(rest) => format!("wss://{rest}"),
            None => self.base.replacen("http://", "ws://", 1),
        };
        format!("{address}/v1/stream")
    }

    /// Whether `url` is a page of the server itself.
    fn serves(&self, url: &str) -> bool {
        let rest = url.strip_prefix(self.base.as_str());
        rest.is_some_and(|rest| rest.starts_with('/')) && Target::parse(url).is_ok()
    }

    /// Whether the address the server handed out for a sign-in may be
    /// opened in the browser: only a page of the server itself.
    #[must_use]
    pub fn is_sign_in_page(&self, url: &str) -> bool {
        self.serves(url)
    }

    /// Whether the address the server handed out for paying may be opened
    /// in the browser: an HTTPS page of the payment service, or a page of
    /// the server itself when that is a test server on this computer.
    #[must_use]
    pub fn is_payment_page(&self, url: &str) -> bool {
        let Ok(target) = Target::parse(url) else {
            return false;
        };
        let host = target.host.to_ascii_lowercase();
        let pays = target.secure
            && target.port == 443
            && (host == "stripe.com" || host.ends_with(".stripe.com"));
        pays || (!self.base.starts_with("https://") && self.serves(url))
    }

    /// Whether an address the server handed out may be opened in the
    /// browser at all: for a sign-in or for paying.
    #[must_use]
    pub fn may_open(&self, url: &str) -> bool {
        self.is_sign_in_page(url) || self.is_payment_page(url)
    }
}

/// What the server means by a refusal. The names are the server's, and
/// never change meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    /// No valid session.
    Unauthorized,
    /// The request was malformed, or cannot be done as things stand.
    BadRequest,
    /// No such thing.
    NotFound,
    /// Signed in, but without a subscription that is paid for.
    SubscriptionRequired,
    /// The month's allowance is used up.
    QuotaExceeded,
    /// Too many requests, or too many streams at once.
    TooManyRequests,
    /// The server is not set up for this.
    Unavailable,
    /// A service behind the server failed.
    UpstreamFailed,
    /// A fault in the server, or a code this version does not know.
    Internal,
}

impl Code {
    pub(crate) fn named(name: &str) -> Option<Self> {
        Some(match name {
            "unauthorized" => Self::Unauthorized,
            "bad_request" => Self::BadRequest,
            "not_found" => Self::NotFound,
            "subscription_required" => Self::SubscriptionRequired,
            "quota_exceeded" => Self::QuotaExceeded,
            "too_many_requests" => Self::TooManyRequests,
            "unavailable" => Self::Unavailable,
            "upstream_failed" => Self::UpstreamFailed,
            "internal" => Self::Internal,
            _ => return None,
        })
    }

    /// What a status means when the answer does not say. `429` stands for
    /// two things; without the answer it is taken for the milder one.
    fn of_status(status: u16) -> Self {
        match status {
            400 => Self::BadRequest,
            401 => Self::Unauthorized,
            402 => Self::SubscriptionRequired,
            404 => Self::NotFound,
            429 => Self::TooManyRequests,
            502 => Self::UpstreamFailed,
            503 => Self::Unavailable,
            _ => Self::Internal,
        }
    }
}

/// What a refusal is about, where the app does something other than say
/// so. The names are the server's `reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Reason {
    /// Nothing the app acts on.
    #[default]
    Other,
    /// A sign-in has not been finished in the browser yet.
    Pending,
    /// There is a subscription already; it is managed, not bought again.
    AlreadySubscribed,
    /// There is no subscription to manage yet.
    NothingToManage,
}

impl Reason {
    /// What an error of the server is about. Servers from before there was
    /// a `reason` said the same in their message.
    fn of(error: &Value) -> Self {
        let said = |name: &str| error[name].as_str();
        match said("reason").or_else(|| said("message")) {
            Some("authorization_pending") => Self::Pending,
            Some("already_subscribed" | "already subscribed") => Self::AlreadySubscribed,
            Some("nothing_to_manage") => Self::NothingToManage,
            _ => Self::Other,
        }
    }
}

/// Why something asked of the account did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Nobody is signed in, or the server no longer accepts the session and
    /// it was let go of.
    SignedOut,
    /// The server said no.
    Refused {
        /// What it means.
        code: Code,
        /// What it is about, where the app acts on that.
        reason: Reason,
        /// The server's words, for a person.
        message: String,
    },
    /// No answer: offline, or the server cannot be reached. In words.
    Unreachable(String),
    /// A sign-in was not finished in the browser in time.
    Expired,
    /// A sign-in ended in the browser without anyone signed in, for the
    /// reason these words give the person.
    Browser(&'static str),
    /// A wait was called off.
    Stopped,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SignedOut => f.write_str("not signed in to VinPUT Pro"),
            Self::Refused { message, .. } => write!(f, "VinPUT Pro said: {message}"),
            Self::Unreachable(why) => write!(f, "VinPUT Pro could not be reached: {why}"),
            Self::Expired => f.write_str("the sign-in was not finished in time"),
            Self::Browser(why) => f.write_str(why),
            Self::Stopped => f.write_str("stopped"),
        }
    }
}

impl Failure {
    /// A refusal that says nothing but `code`.
    #[must_use]
    pub fn refused(code: Code, message: &str) -> Self {
        Self::Refused {
            code,
            reason: Reason::Other,
            message: message.to_owned(),
        }
    }

    /// Whether the same request may well be answered a moment later: no
    /// connection, or a server that is busy or has a fault.
    #[must_use]
    pub fn is_passing(&self) -> bool {
        match self {
            Self::Unreachable(_) => true,
            Self::Refused { code, .. } => matches!(
                code,
                Code::TooManyRequests | Code::Internal | Code::UpstreamFailed | Code::Unavailable
            ),
            Self::SignedOut | Self::Expired | Self::Browser(_) | Self::Stopped => false,
        }
    }
}

/// What the server's answer to a failed request means.
pub(crate) fn refusal(status: u16, body: &[u8]) -> Failure {
    let answer: Value = serde_json::from_slice(body).unwrap_or_default();
    let error = &answer["error"];
    let code = error["code"]
        .as_str()
        .and_then(Code::named)
        .unwrap_or_else(|| Code::of_status(status));
    let message = error["message"]
        .as_str()
        .map_or_else(|| format!("HTTP {status}"), str::to_owned);
    Failure::Refused {
        code,
        reason: Reason::of(error),
        message,
    }
}

/// The plans there are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// Paid by the month.
    Monthly,
    /// Paid by the year.
    Yearly,
}

impl Plan {
    /// The server's name for it.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Monthly => "monthly",
            Self::Yearly => "yearly",
        }
    }

    fn named(name: &str) -> Option<Self> {
        match name {
            "monthly" => Some(Self::Monthly),
            "yearly" => Some(Self::Yearly),
            _ => None,
        }
    }
}

/// The tiers there are: the same recognition, with more or fewer hours a
/// month.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tier {
    /// The larger allowance. What an answer that names no tier means.
    #[default]
    Pro,
    /// The smaller one.
    Plus,
}

impl Tier {
    /// The server's name for it.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Pro => "pro",
            Self::Plus => "plus",
        }
    }

    /// The tier `said` names; one that names none is Pro, and one this
    /// version does not know is none.
    fn said(said: &Value) -> Option<Self> {
        match said.as_str() {
            None | Some("pro") => Some(Self::Pro),
            Some("plus") => Some(Self::Plus),
            Some(_) => None,
        }
    }
}

/// A subscription, which may be one that has ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    /// Where it stands, in the payment service's word: `active`,
    /// `past_due`, `canceled` and so on.
    pub status: String,
    /// The plan, if it is one this version knows.
    pub plan: Option<Plan>,
    /// When the period that is paid for ends, in seconds since 1970.
    pub period_end: u64,
    /// Whether it ends then instead of renewing.
    pub ends: bool,
    /// Until when Pro keeps working although a renewal failed, in seconds
    /// since 1970; zero unless a payment is overdue.
    pub grace_until: u64,
    /// When it ends, if it is set to end; zero otherwise.
    pub ends_at: u64,
    /// When it ended, if it has; zero otherwise.
    pub ended_at: u64,
}

/// What the server knows about the account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountInfo {
    /// The Google address that is signed in.
    pub email: String,
    /// Whether Pro can be used right now. Not the same as a subscription
    /// being active: the server decides, and this is its word.
    pub entitled: bool,
    /// The subscription, for someone who ever had one.
    pub subscription: Option<Subscription>,
    /// Seconds of audio recognized this month.
    pub used_seconds: u64,
    /// Seconds a month comes with, on the tier in effect.
    pub cap_seconds: u64,
    /// The tier in effect, or the one of a subscription that is not: Pro
    /// for an answer that names none, and for an account that is given it.
    pub tier: Tier,
    /// When the count starts again, in seconds since 1970; zero if the
    /// server did not say.
    pub resets_at: u64,
    /// Whether the server gives this account Pro without a subscription.
    /// An answer that does not say is one that does not.
    pub complimentary: bool,
    /// Whether the account would pay the price of a first month, as someone
    /// new does. An answer that does not say is one that would not.
    pub intro_eligible: bool,
}

impl AccountInfo {
    /// Whether Pro is on for this account at no charge: the server gives it,
    /// and no subscription of the account's own is paying for it.
    #[must_use]
    pub fn at_no_charge(&self) -> bool {
        let paying = self.subscription.as_ref().is_some_and(|subscription| {
            matches!(subscription.status.as_str(), "active" | "trialing")
        });
        self.complimentary && !paying
    }

    /// Whether the month's allowance is used up.
    #[must_use]
    pub fn over_allowance(&self) -> bool {
        self.cap_seconds > 0 && self.used_seconds >= self.cap_seconds
    }
}

fn parse_account(answer: &Value) -> Option<AccountInfo> {
    let subscription = &answer["subscription"];
    let usage = &answer["usage"];
    let when = |name: &str| subscription[name].as_u64().unwrap_or(0);
    Some(AccountInfo {
        email: answer["email"].as_str()?.to_owned(),
        entitled: answer["entitled"].as_bool()?,
        subscription: subscription.as_object().map(|_| Subscription {
            status: subscription["status"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            plan: subscription["plan"].as_str().and_then(Plan::named),
            period_end: when("current_period_end"),
            ends: subscription["cancel_at_period_end"]
                .as_bool()
                .unwrap_or(false),
            grace_until: when("grace_until"),
            ends_at: when("ends_at"),
            ended_at: when("ended_at"),
        }),
        used_seconds: usage["seconds"].as_u64().unwrap_or(0),
        cap_seconds: usage["cap_seconds"].as_u64().unwrap_or(0),
        tier: [&answer["tier"], &subscription["tier"]]
            .into_iter()
            .find(|tier| tier.is_string())
            .and_then(Tier::said)
            .unwrap_or_default(),
        resets_at: usage["resets_at"].as_u64().unwrap_or(0),
        complimentary: answer["complimentary"].as_bool().unwrap_or(false),
        intro_eligible: answer["intro_eligible"].as_bool().unwrap_or(false),
    })
}

/// One tier on offer: what it costs and what a month of it comes with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Offer {
    /// Which tier.
    pub tier: Tier,
    /// What a month costs, in cents.
    pub monthly: u32,
    /// What the first month costs someone new, in cents; zero where it is
    /// a month like any other.
    pub first: u32,
    /// What a year costs, in cents.
    pub yearly: u32,
    /// Seconds of speech a month comes with.
    pub cap_seconds: u64,
}

/// What the tiers cost and what a month of each comes with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prices {
    /// The tiers that can be subscribed to, the smaller first.
    pub offers: Vec<Offer>,
    /// The currency, as its three letters in lower case: `eur`.
    pub currency: String,
    /// Whether a plan can be subscribed to yet. An answer that does not say
    /// is one from a server where it can.
    pub open: bool,
}

impl Default for Prices {
    /// What is shown until the server has said.
    fn default() -> Self {
        Self {
            offers: BUILT_IN.to_vec(),
            currency: "eur".to_owned(),
            open: true,
        }
    }
}

/// Reads the plans. One that names no tier is of Pro, and one that names no
/// allowance has the one the answer names for all; a tier is on offer when
/// both its plans are. A server from before there were tiers offers Pro.
fn parse_prices(answer: &Value) -> Option<Prices> {
    let plans = answer["plans"].as_array()?;
    let currency = plans.first()?["currency"].as_str()?;
    // Plans in two currencies are not something to show side by side.
    let one = plans.iter().all(|plan| plan["currency"] == currency);
    (one && currency.len() == 3).then_some(())?;
    let mut offers = Vec::new();
    for built_in in BUILT_IN {
        let tier = built_in.tier;
        let plan = |plan: Plan| {
            let mut of_tier = plans
                .iter()
                .filter(|entry| Tier::said(&entry["tier"]) == Some(tier));
            of_tier.find(|entry| entry["plan"] == plan.id())
        };
        let amount = |plan: &Value| u32::try_from(plan["amount"].as_u64()?).ok();
        let (Some(monthly), Some(yearly)) = (plan(Plan::Monthly), plan(Plan::Yearly)) else {
            continue;
        };
        let cap = [&monthly["cap_seconds"], &answer["cap_seconds"]]
            .into_iter()
            .find_map(Value::as_u64);
        // A price for the first month alone, and below that of a month:
        // anything else is not what the app says about it.
        let intro = &monthly["intro"];
        let regular = amount(monthly)?;
        let first = amount(intro).filter(|first| intro["months"] == 1 && *first < regular);
        offers.push(Offer {
            tier,
            monthly: regular,
            first: first.unwrap_or(0),
            yearly: amount(yearly)?,
            cap_seconds: cap.unwrap_or(built_in.cap_seconds),
        });
    }
    (!offers.is_empty()).then(|| Prices {
        offers,
        currency: currency.to_ascii_lowercase(),
        open: answer["checkout_open"] != false,
    })
}

/// The name of this PC, for the list of the account's sessions on the
/// website: what Windows calls it, without anything that is not for showing.
fn device_name(computer: Option<String>) -> Option<String> {
    let name: String = computer?
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(DEVICE_NAME_MAX)
        .collect();
    Some(name).filter(|name| !name.is_empty())
}

/// What a sign-in is started with: the name of this PC, or nothing.
fn sign_in_request(device: Option<&str>) -> String {
    device.map_or_else(String::new, |name| {
        json!({ "device_name": name }).to_string()
    })
}

/// A sign-in that was started and waits to be finished in the browser.
pub struct SignIn {
    /// The page to open in the browser.
    pub verification_url: String,
    /// The code that page asks for, which the app shows. Empty for a
    /// sign-in the browser brings back by itself: there is nothing to type.
    pub confirm_code: String,
    /// How the session comes to this process.
    way: Way,
    /// How long the person has.
    expires_in: Duration,
}

/// How a sign-in that was started ends in a session.
enum Way {
    /// The browser comes back to a port of this computer with a code.
    Listening {
        /// Where it comes back to.
        listener: loopback::Listener,
        /// The secret the code is good for nothing without.
        verifier: String,
        /// What the browser must bring back along with the code.
        state: String,
    },
    /// The person types a code in the browser, and the server is asked
    /// until it hands the session out.
    Asking {
        /// What the app trades for a session once the person has signed in.
        device_code: String,
        /// How long to wait between two looks.
        interval: Duration,
    },
}

impl SignIn {
    /// How many minutes the person has to finish it.
    #[must_use]
    pub fn minutes(&self) -> u32 {
        u32::try_from(self.expires_in.as_secs() / 60).unwrap_or(u32::MAX)
    }
}

impl fmt::Debug for SignIn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The device code is as good as a session, and so is the secret of
        // a sign-in that listens; they stay out of logs.
        f.debug_struct("SignIn")
            .field("confirm_code", &self.confirm_code)
            .finish_non_exhaustive()
    }
}

fn parse_sign_in(answer: &Value) -> Option<SignIn> {
    Some(SignIn {
        verification_url: answer["verification_url"].as_str()?.to_owned(),
        confirm_code: answer["confirm_code"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        way: Way::Asking {
            device_code: answer["device_code"].as_str()?.to_owned(),
            interval: Duration::from_secs(answer["interval"].as_u64().unwrap_or(2)),
        },
        expires_in: Duration::from_secs(answer["expires_in"].as_u64().unwrap_or(600)),
    })
}

/// A fresh secret: 256 random bits in base64url, which is 43 characters of
/// what goes into an address as it is.
fn random_token() -> Option<String> {
    let mut bytes = [0_u8; 32];
    SystemRandom::new().fill(&mut bytes).ok()?;
    Some(URL_SAFE_NO_PAD.encode(bytes))
}

/// The challenge that `verifier` answers: its SHA-256 in base64url, as
/// PKCE's `S256` makes it.
fn challenge_of(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(digest::digest(&digest::SHA256, verifier.as_bytes()))
}

/// What the code the browser brought back is traded with: the secret it
/// was asked for with, and the name of this PC.
fn app_code_request(code: &str, verifier: &str, device: Option<&str>) -> String {
    let mut request = json!({
        "grant_type": "app_code", "code": code, "code_verifier": verifier,
    });
    if let Some(name) = device {
        request["device_name"] = json!(name);
    }
    request.to_string()
}

/// A session as the server grants it.
#[derive(Clone, PartialEq, Eq)]
struct Granted {
    access_token: String,
    expires_in: Duration,
    refresh_token: String,
    email: String,
}

fn parse_granted(answer: &Value) -> Option<Granted> {
    Some(Granted {
        access_token: answer["access_token"].as_str()?.to_owned(),
        expires_in: Duration::from_secs(answer["expires_in"].as_u64()?),
        refresh_token: answer["refresh_token"].as_str()?.to_owned(),
        email: answer["email"].as_str().unwrap_or_default().to_owned(),
    })
}

fn device_request(device_code: &str) -> String {
    json!({ "grant_type": "device_code", "device_code": device_code }).to_string()
}

fn refresh_request(refresh_token: &str) -> String {
    json!({ "grant_type": "refresh_token", "refresh_token": refresh_token }).to_string()
}

/// An access token and when it stops working.
#[derive(Clone)]
struct Access {
    token: String,
    expires: Instant,
}

impl Access {
    /// Whether it still works `margin` from `now`.
    fn good_for(&self, margin: Duration, now: Instant) -> bool {
        self.expires.checked_duration_since(now) >= Some(margin)
    }
}

/// What is kept between runs: the refresh token and whose it is.
#[derive(Clone, PartialEq, Eq)]
pub struct Kept {
    /// Trades for the next session. A secret.
    pub refresh_token: String,
    /// The Google address that signed in, to show while offline.
    pub email: String,
}

impl Kept {
    /// One line each; neither holds a line break.
    fn to_text(&self) -> String {
        format!("{}\n{}", self.refresh_token, self.email)
    }

    fn from_text(text: &str) -> Option<Self> {
        let (refresh_token, email) = text.split_once('\n').unwrap_or((text, ""));
        (!refresh_token.trim().is_empty()).then(|| Self {
            refresh_token: refresh_token.trim().to_owned(),
            email: email.trim().to_owned(),
        })
    }
}

/// Where the refresh token is kept. The app keeps it in the Windows
/// credential store; tests keep it in memory.
pub trait Vault: Send + Sync {
    /// What is kept, if anyone is signed in.
    fn get(&self) -> Option<Kept>;
    /// Keeps `kept` in place of what was there. Says whether it is kept.
    fn set(&self, kept: &Kept) -> bool;
    /// Forgets what is kept.
    fn clear(&self);
    /// The name under which the processes that share what is kept take
    /// turns at trading the token; `None` if no other process shares it.
    fn turn(&self) -> Option<String> {
        None
    }
}

/// Keeps nothing: for a session that was handed over as an access token.
struct Nothing;

impl Vault for Nothing {
    fn get(&self) -> Option<Kept> {
        None
    }

    fn set(&self, _kept: &Kept) -> bool {
        false
    }

    fn clear(&self) {}
}

/// The Windows credential store, under this copy's own name.
struct CredentialStore;

impl Vault for CredentialStore {
    fn get(&self) -> Option<Kept> {
        Kept::from_text(&secrets::get(secrets::PRO_SESSION)?)
    }

    fn set(&self, kept: &Kept) -> bool {
        secrets::set(secrets::PRO_SESSION, &kept.to_text())
            .map_err(|err| log::write(&err.to_string()))
            .is_ok()
    }

    fn clear(&self) {
        secrets::delete(secrets::PRO_SESSION);
    }

    fn turn(&self) -> Option<String> {
        Some(crate::instance::named("Local\\VinputProSession"))
    }
}

/// The session with the server, and everything that is asked with it.
pub struct Account {
    server: Server,
    vault: Box<dyn Vault>,
    /// The access token in use.
    access: Mutex<Option<Access>>,
    /// Held while a refresh token is traded, so that it never is twice.
    refreshing: Mutex<()>,
    poll_floor: Duration,
    /// See [`RETRY_WAITS`]; tests shorten them.
    retry_waits: &'static [Duration],
}

impl fmt::Debug for Account {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Account")
            .field("server", &self.server)
            .finish_non_exhaustive()
    }
}

/// The one account of this process, with the session in the Windows
/// credential store.
#[must_use]
pub fn shared() -> Arc<Account> {
    static SHARED: OnceLock<Arc<Account>> = OnceLock::new();
    Arc::clone(
        SHARED
            .get_or_init(|| Arc::new(Account::new(Server::from_env(), Box::new(CredentialStore)))),
    )
}

/// How a request is made: what is posted, and as whom.
#[derive(Clone, Copy)]
struct Ask<'a> {
    path: &'a str,
    /// `None` is a `GET`; an empty one a post without a body.
    json: Option<&'a str>,
    bearer: Option<&'a str>,
    /// How long connecting, and answering, may take.
    timeout: Duration,
}

impl<'a> Ask<'a> {
    /// A request nobody is signed in for.
    fn open(path: &'a str, json: Option<&'a str>) -> Self {
        Self {
            path,
            json,
            bearer: None,
            timeout: TIMEOUT,
        }
    }
}

/// How hard a refresh that gets no answer is tried again.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Patience {
    /// Not at all: someone is holding a key down.
    None,
    /// Twice: someone is looking at a page.
    Some,
    /// As often as there are waits: nobody is waiting.
    All,
}

impl Account {
    /// An account at `server` whose session is kept in `vault`.
    #[must_use]
    pub fn new(server: Server, vault: Box<dyn Vault>) -> Self {
        Self {
            server,
            vault,
            access: Mutex::default(),
            refreshing: Mutex::default(),
            poll_floor: POLL_FLOOR,
            retry_waits: &RETRY_WAITS,
        }
    }

    /// An account at `server` that has nothing but `access_token`, which is
    /// good for at most an hour: for measuring, with a token someone was
    /// handed. It cannot be traded for another, and nothing is kept.
    #[must_use]
    pub fn with_access_token(server: Server, access_token: &str) -> Self {
        let account = Self::new(server, Box::new(Nothing));
        *account.access() = Some(Access {
            token: access_token.trim().to_owned(),
            expires: Instant::now() + Duration::from_secs(3600),
        });
        account
    }

    /// The server this account is at.
    #[must_use]
    pub fn server(&self) -> &Server {
        &self.server
    }

    /// The address that is signed in, if anyone is. Looks at the credential
    /// store, which takes a moment.
    #[must_use]
    pub fn signed_in_as(&self) -> Option<String> {
        self.vault.get().map(|kept| kept.email)
    }

    fn access(&self) -> MutexGuard<'_, Option<Access>> {
        self.access.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Makes a request and reads the answer, which is JSON or nothing.
    fn ask(&self, ask: Ask<'_>) -> Result<Value, Failure> {
        let url = self.server.url(ask.path);
        let authorization = ask.bearer.map(|token| format!("Bearer {token}"));
        let headers: Vec<(&str, &str)> = authorization
            .iter()
            .map(|value| ("Authorization", value.as_str()))
            .collect();
        let mut response = Request {
            json: ask.json,
            headers: &headers,
            ..Request::get(&url, ask.timeout)
        }
        .send()
        .map_err(Failure::Unreachable)?;
        let status = response.status();
        let body = response
            .body(ANSWER_LIMIT, ask.timeout)
            .map_err(Failure::Unreachable)?;
        if !(200..300).contains(&status) {
            return Err(refusal(status, &body));
        }
        if body.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&body)
            .map_err(|_| Failure::Unreachable("the server's answer could not be read".to_owned()))
    }

    /// Starts a sign-in. The caller opens [`SignIn::verification_url`] in
    /// the browser, shows [`SignIn::confirm_code`] if there is one, and
    /// waits with [`finish_sign_in`](Self::finish_sign_in).
    ///
    /// The browser comes back to a port of this computer by itself. Only
    /// when none can be listened on is the server asked for a sign-in with
    /// a code to type.
    ///
    /// # Errors
    ///
    /// Returns why the server did not start one, where it was asked.
    pub fn start_sign_in(&self) -> Result<SignIn, Failure> {
        match self.listen_for_sign_in() {
            Ok(sign_in) => Ok(sign_in),
            Err(why) => {
                log::write(&format!(
                    "no port to sign in to VinPUT Pro through ({why}); signing in with a code"
                ));
                self.start_sign_in_with_code()
            }
        }
    }

    /// A sign-in the browser brings back to a port of this computer. Asks
    /// the server nothing: the page it opens does.
    fn listen_for_sign_in(&self) -> Result<SignIn, String> {
        let listener = loopback::Listener::bind().map_err(|err| err.to_string())?;
        let no_randomness = || "Windows has no randomness to give".to_owned();
        let verifier = random_token().ok_or_else(no_randomness)?;
        let state = random_token().ok_or_else(no_randomness)?;
        let page = format!(
            "/v1/auth/app/start?port={}&code_challenge={}&state={state}",
            listener.port(),
            challenge_of(&verifier),
        );
        Ok(SignIn {
            verification_url: self.server.url(&page),
            confirm_code: String::new(),
            way: Way::Listening {
                listener,
                verifier,
                state,
            },
            expires_in: BROWSER_WAIT,
        })
    }

    /// A sign-in with a code to type, which the server starts.
    fn start_sign_in_with_code(&self) -> Result<SignIn, Failure> {
        let device = device_name(std::env::var("COMPUTERNAME").ok());
        let request = sign_in_request(device.as_deref());
        let answer = self.ask(Ask::open("/v1/auth/device", Some(&request)))?;
        parse_sign_in(&answer)
            .filter(|sign_in| self.server.is_sign_in_page(&sign_in.verification_url))
            .ok_or_else(|| Failure::Unreachable("the server's answer could not be read".into()))
    }

    /// What the plans cost. Asked without a session: anyone may know.
    ///
    /// # Errors
    ///
    /// Returns why the server did not say; the caller then shows
    /// [`Prices::default`].
    pub fn prices(&self) -> Result<Prices, Failure> {
        let answer = self.ask(Ask::open("/v1/plans", None))?;
        parse_prices(&answer)
            .ok_or_else(|| Failure::Unreachable("the server's answer could not be read".into()))
    }

    /// Waits until the person has finished `sign_in` in the browser, then
    /// keeps the session. Returns the address that signed in.
    ///
    /// # Errors
    ///
    /// Returns [`Failure::Stopped`] once `stop` is set, or when the person
    /// declined in the browser; [`Failure::Expired`] when the time for it is
    /// up or the server has forgotten it; [`Failure::Browser`] when the
    /// browser came back without anyone signed in; and otherwise why it
    /// could not be finished. A request that fails for want of a
    /// connection, or because the server is busy or has a fault, is made
    /// again: the person is in the browser meanwhile, and finishes there
    /// whatever happens here.
    pub fn finish_sign_in(&self, sign_in: &SignIn, stop: &AtomicBool) -> Result<String, Failure> {
        match &sign_in.way {
            Way::Listening {
                listener,
                verifier,
                state,
            } => {
                let (arrived, browser) = listener.wait(state, sign_in.expires_in, stop)?;
                let ended = match arrived {
                    loopback::Arrived::Code(code) => self.trade_app_code(&code, verifier, stop),
                    loopback::Arrived::Error(word) => Err(match word.as_str() {
                        // Their own doing, and they know: nothing to say.
                        "cancelled" => Failure::Stopped,
                        "unverified" => Failure::Browser(UNVERIFIED),
                        _ => Failure::Browser(NOT_THROUGH),
                    }),
                };
                // The browser is told last, so that what its page says is
                // what became of the sign-in here.
                let result = match &ended {
                    Ok(_) => "signed_in",
                    Err(Failure::Stopped) => "cancelled",
                    Err(Failure::Browser(UNVERIFIED)) => "unverified",
                    Err(_) => "failed",
                };
                browser.send_to(
                    &self
                        .server
                        .url(&format!("/v1/auth/app/done?result={result}")),
                );
                ended
            }
            Way::Asking {
                device_code,
                interval,
            } => self.ask_until_signed_in(device_code, *interval, sign_in.expires_in, stop),
        }
    }

    /// Trades the code the browser brought back for a session, and keeps
    /// it. Returns the address that signed in.
    fn trade_app_code(
        &self,
        code: &str,
        verifier: &str,
        stop: &AtomicBool,
    ) -> Result<String, Failure> {
        let device = device_name(std::env::var("COMPUTERNAME").ok());
        let request = app_code_request(code, verifier, device.as_deref());
        // The browser is waiting for its page meanwhile, so not for long.
        let mut waits = self.retry_waits.iter().take(2);
        let answer = loop {
            match self.ask(Ask::open("/v1/auth/token", Some(&request))) {
                Err(failure) if failure.is_passing() => match waits.next() {
                    Some(wait) => std::thread::sleep(*wait),
                    None => return Err(failure),
                },
                // The code works once and for a minute. A code the server
                // does not take was used by a request whose answer was
                // lost, or came too late.
                Err(Failure::Refused {
                    code: Code::Unauthorized,
                    ..
                }) => return Err(Failure::Expired),
                other => break other?,
            }
        };
        let granted = parse_granted(&answer)
            .ok_or_else(|| Failure::Unreachable("the server's answer could not be read".into()))?;
        // Called off while the answer was on its way: nobody is signed in,
        // here or there.
        if stop.load(Ordering::Relaxed) {
            self.tell_goodbye(&granted.refresh_token, TIMEOUT);
            return Err(Failure::Stopped);
        }
        let email = granted.email.clone();
        self.keep(granted)?;
        Ok(email)
    }

    /// Asks the server whether the person has typed the code in the
    /// browser, until it hands the session out, and keeps it.
    fn ask_until_signed_in(
        &self,
        device_code: &str,
        interval: Duration,
        expires_in: Duration,
        stop: &AtomicBool,
    ) -> Result<String, Failure> {
        let started = Instant::now();
        let request = device_request(device_code);
        let interval = interval.max(self.poll_floor);
        let mut passing = None;
        loop {
            let waited = Instant::now();
            while waited.elapsed() < interval {
                if stop.load(Ordering::Relaxed) {
                    return Err(Failure::Stopped);
                }
                std::thread::sleep(STOP_STEP.min(interval));
            }
            if started.elapsed() > expires_in {
                return Err(passing.unwrap_or(Failure::Expired));
            }
            let answer = self.ask(Ask::open("/v1/auth/token", Some(&request)));
            match answer {
                Ok(answer) => {
                    let granted = parse_granted(&answer).ok_or_else(|| {
                        Failure::Unreachable("the server's answer could not be read".into())
                    })?;
                    // Called off while the answer was on its way: nobody
                    // is signed in, here or there.
                    if stop.load(Ordering::Relaxed) {
                        self.tell_goodbye(&granted.refresh_token, TIMEOUT);
                        return Err(Failure::Stopped);
                    }
                    let email = granted.email.clone();
                    self.keep(granted)?;
                    return Ok(email);
                }
                Err(Failure::Refused {
                    reason: Reason::Pending,
                    ..
                }) => passing = None,
                Err(Failure::Refused {
                    code: Code::Unauthorized,
                    ..
                }) => return Err(Failure::Expired),
                Err(failure) if failure.is_passing() => passing = Some(failure),
                Err(other) => return Err(other),
            }
        }
    }

    /// Keeps a session the server granted: the refresh token first, in the
    /// vault, and only then the access token for use.
    fn keep(&self, granted: Granted) -> Result<String, Failure> {
        let email = if granted.email.is_empty() {
            self.signed_in_as().unwrap_or_default()
        } else {
            granted.email
        };
        let kept = Kept {
            refresh_token: granted.refresh_token,
            email,
        };
        if !self.vault.set(&kept) {
            return Err(Failure::Unreachable(
                "Windows did not keep the session in its credential store".to_owned(),
            ));
        }
        *self.access() = Some(Access {
            token: granted.access_token.clone(),
            expires: Instant::now() + granted.expires_in,
        });
        Ok(granted.access_token)
    }

    /// An access token that is good for at least `margin`: the one in
    /// memory, or a new one traded for the refresh token. A trade that gets
    /// no answer may have reached the server all the same, which then
    /// takes the old token only for a minute more: so it is made again as
    /// `patience` allows before this gives up.
    fn token(&self, margin: Duration, patience: Patience) -> Result<String, Failure> {
        let fresh = |account: &Self| {
            account
                .access()
                .as_ref()
                .filter(|access| access.good_for(margin, Instant::now()))
                .map(|access| access.token.clone())
        };
        if let Some(token) = fresh(self) {
            return Ok(token);
        }
        let _one_at_a_time = self
            .refreshing
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // Another thread may have traded it while this one waited.
        if let Some(token) = fresh(self) {
            return Ok(token);
        }
        // Another process may be trading it right now; what it keeps is
        // read only once it is done.
        let _turn = self
            .vault
            .turn()
            .map(|name| turn::Turn::take(&name, TURN_WAIT));
        let mut kept = self.vault.get().ok_or(Failure::SignedOut)?;
        let tries = match patience {
            Patience::None => 0,
            Patience::Some => 2,
            Patience::All => usize::MAX,
        };
        let mut waits = self.retry_waits.iter().take(tries);
        loop {
            let request = refresh_request(&kept.refresh_token);
            let answer = self.ask(Ask::open("/v1/auth/token", Some(&request)));
            match answer {
                Ok(answer) => {
                    let granted = parse_granted(&answer).ok_or_else(|| {
                        Failure::Unreachable("the server's answer could not be read".into())
                    })?;
                    return self.keep(granted);
                }
                Err(Failure::Refused {
                    code: Code::Unauthorized,
                    ..
                }) => {
                    // Another Vinput process may have traded it in the
                    // meantime and kept its successor; that one is tried.
                    // Otherwise the session is over.
                    match self.vault.get() {
                        Some(newer) if newer.refresh_token != kept.refresh_token => kept = newer,
                        _ => {
                            self.forget();
                            return Err(Failure::SignedOut);
                        }
                    }
                }
                Err(failure @ Failure::Unreachable(_)) => match waits.next() {
                    Some(wait) => std::thread::sleep(*wait),
                    None => return Err(failure),
                },
                Err(other) => return Err(other),
            }
        }
    }

    /// Lets go of the session on this PC without telling the server.
    fn forget(&self) {
        self.vault.clear();
        *self.access() = None;
    }

    /// The server has refused the session for good, after a new token was
    /// tried: the account is gone, or this PC was signed out elsewhere. It
    /// is let go of here too.
    pub fn signed_out_elsewhere(&self) {
        self.forget();
    }

    /// An access token for a request made now.
    ///
    /// # Errors
    ///
    /// Returns [`Failure::SignedOut`] if nobody is signed in or the server
    /// no longer accepts the session, and otherwise why no token was had.
    pub fn access_token(&self) -> Result<String, Failure> {
        self.token(EXPIRY_MARGIN, Patience::None)
    }

    /// Whether a token is in memory that a request made now can use, so
    /// that a dictation does not have to wait for one.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.access()
            .as_ref()
            .is_some_and(|access| access.good_for(EXPIRY_MARGIN, Instant::now()))
    }

    /// Trades the access token for a new one if it would run out soon, so
    /// that the next dictation finds one ready. For a quiet moment: when
    /// the provider starts, and after a dictation.
    ///
    /// # Errors
    ///
    /// As [`access_token`](Self::access_token).
    pub fn keep_fresh(&self) -> Result<(), Failure> {
        self.token(FRESH_FOR, Patience::All).map(drop)
    }

    /// The server ended a stream as one of nobody's: whatever token is in
    /// memory is not used again, and the next request trades for a new one,
    /// which shows whether the session still stands.
    pub fn distrust(&self) {
        *self.access() = None;
    }

    /// The server refused `token`: it is not used again.
    pub fn refused(&self, token: &str) {
        let mut access = self.access();
        if access.as_ref().is_some_and(|access| access.token == token) {
            *access = None;
        }
    }

    /// Makes a request as the signed-in person. A token the server refuses
    /// is traded for a new one once; if that is refused too, the session is
    /// over.
    fn authorized(&self, path: &str, json: Option<&str>) -> Result<Value, Failure> {
        let mut retried = false;
        loop {
            let token = self.token(EXPIRY_MARGIN, Patience::Some)?;
            let answer = self.ask(Ask {
                bearer: Some(&token),
                ..Ask::open(path, json)
            });
            match answer {
                Err(Failure::Refused {
                    code: Code::Unauthorized,
                    ..
                }) if !retried => {
                    self.refused(&token);
                    retried = true;
                }
                Err(Failure::Refused {
                    code: Code::Unauthorized,
                    ..
                }) => {
                    self.forget();
                    return Err(Failure::SignedOut);
                }
                other => return other,
            }
        }
    }

    /// Asks the server how the account stands.
    ///
    /// # Errors
    ///
    /// Returns why the server did not say.
    pub fn info(&self) -> Result<AccountInfo, Failure> {
        let answer = self.authorized("/v1/account", None)?;
        parse_account(&answer)
            .ok_or_else(|| Failure::Unreachable("the server's answer could not be read".into()))
    }

    /// The address named `url` in an answer, if it is one to open.
    fn page(&self, answer: &Value) -> Result<String, Failure> {
        answer["url"]
            .as_str()
            .filter(|url| self.server.is_payment_page(url))
            .map(str::to_owned)
            .ok_or_else(|| Failure::Unreachable("the server's answer could not be read".into()))
    }

    /// The page on which to subscribe to `plan` of `tier`, to open in the
    /// browser.
    ///
    /// # Errors
    ///
    /// Returns why there is none. Someone who has a subscription already is
    /// refused with [`Reason::AlreadySubscribed`]; theirs is managed in the
    /// [`portal`](Self::portal).
    pub fn checkout(&self, tier: Tier, plan: Plan) -> Result<String, Failure> {
        let body = json!({ "plan": plan.id(), "tier": tier.id() }).to_string();
        let answer = self.authorized("/v1/billing/checkout", Some(&body))?;
        self.page(&answer)
    }

    /// The page on which to change the card, switch plan or cancel, to open
    /// in the browser.
    ///
    /// # Errors
    ///
    /// Returns why there is none. Someone who never started a checkout is
    /// refused with [`Reason::NothingToManage`].
    pub fn portal(&self) -> Result<String, Failure> {
        let answer = self.authorized("/v1/billing/portal", Some(""))?;
        self.page(&answer)
    }

    /// Signs out: the session is let go of on this PC at once, and the
    /// server is told to forget it, which may fail without mattering.
    pub fn sign_out(&self) {
        let _one_at_a_time = self
            .refreshing
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let kept = self.vault.get();
        self.forget();
        if let Some(kept) = kept {
            self.tell_goodbye(&kept.refresh_token, TIMEOUT);
        }
    }

    /// Tells the server to forget `refresh_token`, which may fail without
    /// mattering: the session then runs out by itself.
    fn tell_goodbye(&self, refresh_token: &str, timeout: Duration) {
        let body = json!({ "refresh_token": refresh_token }).to_string();
        let _ = self.ask(Ask {
            timeout,
            ..Ask::open("/v1/auth/signout", Some(&body))
        });
    }

    /// Tells the server to forget the session that is kept, and leaves it
    /// kept: for removing Vinput, which deletes what is kept itself. No
    /// step of it takes longer than `timeout`.
    pub fn end_at_server(&self, timeout: Duration) {
        if let Some(kept) = self.vault.get() {
            self.tell_goodbye(&kept.refresh_token, timeout);
        }
    }
}

/// Has the server forget the session of this PC before Vinput is removed
/// with everything it kept, and waits for that for `limit` at most: removing
/// goes on whether the server heard or not.
pub fn end_session_before_removal(limit: Duration) {
    within(limit, move || shared().end_at_server(limit));
}

/// Runs `job` on a thread of its own and waits for it for `limit` at most.
/// Says whether it was done by then.
fn within(limit: Duration, job: impl FnOnce() + Send + 'static) -> bool {
    let (done, waiting) = std::sync::mpsc::channel();
    let started = std::thread::Builder::new()
        .name("vinput-pro".into())
        .spawn(move || {
            job();
            let _ = done.send(());
        });
    started.is_ok() && waiting.recv_timeout(limit).is_ok()
}

#[cfg(test)]
mod tests {
    use super::fake::Fake;
    use super::*;

    type Outcome = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn only_https_and_this_computer_can_be_the_server() {
        assert_eq!(
            Server::at("https://example.com/").map(|server| server.base),
            Some("https://example.com".to_owned())
        );
        assert!(Server::at("http://127.0.0.1:8787").is_some());
        for refused in [
            "http://example.com",
            "http://127.0.0.1.example.com",
            "https://example.com/v1",
            "https://example.com?x=1",
            "ftp://example.com",
            "",
        ] {
            assert!(Server::at(refused).is_none(), "{refused}");
        }
        assert!(Server::at(SERVER).is_some());
    }

    #[test]
    fn a_vinput_that_ships_talks_to_its_own_server_and_to_no_other() {
        // Whatever the environment names, a build that does not listen to
        // it goes to the real server.
        for named in [
            Some("https://elsewhere.example.com"),
            Some("http://127.0.0.1:8787"),
            None,
        ] {
            assert_eq!(Server::chosen(named, false).base, SERVER, "{named:?}");
        }
        // A build for development or tests may be pointed at another one,
        // if that is one requests are made to at all.
        assert_eq!(
            Server::chosen(Some("http://127.0.0.1:8787"), true).base,
            "http://127.0.0.1:8787"
        );
        assert_eq!(Server::chosen(Some("ftp://x"), true).base, SERVER);
        assert_eq!(Server::chosen(None, true).base, SERVER);
        // Tests are such a build; what ships is not, which is the point.
        const { assert!(LISTENS) };
    }

    #[test]
    fn the_stream_is_a_web_socket_on_the_same_server() {
        let real = Server::at("https://cloud.example.com");
        assert_eq!(
            real.map(|server| server.stream_url()).as_deref(),
            Some("wss://cloud.example.com/v1/stream")
        );
        let local = Server::at("http://127.0.0.1:8787");
        assert_eq!(
            local.map(|server| server.stream_url()).as_deref(),
            Some("ws://127.0.0.1:8787/v1/stream")
        );
    }

    #[test]
    fn only_web_pages_are_handed_to_the_browser() {
        let Some(real) = Server::at("https://cloud.example.com") else {
            unreachable!("an HTTPS address is a server");
        };
        // A sign-in is finished on a page of the server and nowhere else.
        let sign_in = "https://cloud.example.com/v1/auth/google/start?code=1";
        assert!(real.is_sign_in_page(sign_in) && real.may_open(sign_in));
        for refused in [
            "https://cloud.example.com.evil.example/v1/auth/google/start",
            "https://cloud.example.com@evil.example/start",
            "https://evil.example/v1/auth/google/start?code=1",
            "https://checkout.stripe.com/c/pay/x",
            "http://cloud.example.com/v1/auth/google/start",
            "https://cloud.example.com",
        ] {
            assert!(!real.is_sign_in_page(refused), "{refused}");
        }
        // Paying happens on a page of the payment service and nowhere else.
        for page in [
            "https://checkout.stripe.com/c/pay/x",
            "https://billing.stripe.com/p/session/y",
            "https://Checkout.Stripe.com/c/pay/x",
            "https://stripe.com/somewhere",
        ] {
            assert!(real.is_payment_page(page) && real.may_open(page), "{page}");
        }
        for refused in [
            "http://checkout.stripe.com/c/pay/x",
            "https://checkout.stripe.com.evil.example/c/pay/x",
            "https://evilstripe.com/c/pay/x",
            "https://checkout.stripe.com:8443/c/pay/x",
            "https://checkout.example.com/pay/yearly",
            "https://cloud.example.com/pay",
            "calc.exe",
            "file:///C:/x",
            "",
        ] {
            assert!(!real.is_payment_page(refused), "{refused}");
        }
        for refused in [
            "http://example.com/",
            "https://example.com/",
            "calc.exe",
            "",
        ] {
            assert!(!real.may_open(refused), "{refused}");
        }
        // A test server on this computer hands out its own plain addresses.
        assert!(!real.may_open("http://127.0.0.1:8787/v1/auth/google/start"));
        let Some(local) = Server::at("http://127.0.0.1:8787") else {
            unreachable!("this computer is a server");
        };
        assert!(local.may_open("http://127.0.0.1:8787/v1/auth/google/start?code=1"));
        assert!(local.is_payment_page("http://127.0.0.1:8787/pay"));
        assert!(!local.may_open("http://127.0.0.1:8788/elsewhere"));
        assert!(!local.may_open("http://127.0.0.1:87870/elsewhere"));
    }

    #[test]
    fn a_refusal_is_read_by_its_code_and_by_its_status_without_one() {
        let quota = br#"{"error":{"code":"quota_exceeded","message":"used up"}}"#;
        assert_eq!(
            refusal(429, quota),
            Failure::refused(Code::QuotaExceeded, "used up")
        );
        assert_eq!(
            refusal(402, b"<html>"),
            Failure::refused(Code::SubscriptionRequired, "HTTP 402")
        );
        let unknown = br#"{"error":{"code":"new_thing","message":"hm"}}"#;
        assert!(matches!(
            refusal(500, unknown),
            Failure::Refused {
                code: Code::Internal,
                ..
            }
        ));
        for (status, code) in [
            (400, Code::BadRequest),
            (401, Code::Unauthorized),
            (404, Code::NotFound),
            (429, Code::TooManyRequests),
            (502, Code::UpstreamFailed),
            (503, Code::Unavailable),
        ] {
            assert!(
                matches!(refusal(status, b""), Failure::Refused { code: found, .. } if found == code)
            );
        }
    }

    #[test]
    fn a_refusal_says_what_it_is_about_where_the_app_acts_on_that() {
        let about = |body: &str| match refusal(400, body.as_bytes()) {
            Failure::Refused { code, reason, .. } => (code, reason),
            other => unreachable!("a refusal was expected, not {other:?}"),
        };
        for (reason, means) in [
            ("authorization_pending", Reason::Pending),
            ("already_subscribed", Reason::AlreadySubscribed),
            ("nothing_to_manage", Reason::NothingToManage),
            ("something_newer", Reason::Other),
        ] {
            let body = format!(
                r#"{{"error":{{"code":"bad_request","reason":"{reason}","message":"words"}}}}"#
            );
            assert_eq!(about(&body), (Code::BadRequest, means), "{reason}");
        }
        // Servers from before there was a reason said it in the message.
        for (message, means) in [
            ("authorization_pending", Reason::Pending),
            ("already subscribed", Reason::AlreadySubscribed),
            ("no such plan", Reason::Other),
        ] {
            let body = format!(r#"{{"error":{{"code":"bad_request","message":"{message}"}}}}"#);
            assert_eq!(about(&body).1, means, "{message}");
        }
        assert_eq!(about("<html>").1, Reason::Other);
    }

    #[test]
    fn what_passes_is_told_from_what_does_not() {
        assert!(Failure::Unreachable("offline".into()).is_passing());
        for code in [
            Code::TooManyRequests,
            Code::Internal,
            Code::UpstreamFailed,
            Code::Unavailable,
        ] {
            assert!(Failure::refused(code, "x").is_passing(), "{code:?}");
        }
        for code in [
            Code::Unauthorized,
            Code::BadRequest,
            Code::NotFound,
            Code::SubscriptionRequired,
            Code::QuotaExceeded,
        ] {
            assert!(!Failure::refused(code, "x").is_passing(), "{code:?}");
        }
        for failure in [Failure::SignedOut, Failure::Expired, Failure::Stopped] {
            assert!(!failure.is_passing(), "{failure:?}");
        }
    }

    #[test]
    fn the_server_is_told_what_this_pc_is_called() {
        let named = |name: &str| device_name(Some(name.to_owned()));
        assert_eq!(named(" ANAPC ").as_deref(), Some("ANAPC"));
        assert_eq!(named("A\u{7}B\nC").as_deref(), Some("ABC"));
        assert_eq!(named("   "), None);
        assert_eq!(device_name(None), None);
        let long = named(&"\u{17e}".repeat(100));
        assert_eq!(long.map(|name| name.chars().count()), Some(64));
        // With a name the request says it; without one it has no body.
        let request: Value =
            serde_json::from_str(&sign_in_request(Some("ANA \"PC\""))).unwrap_or_default();
        assert_eq!(request, json!({"device_name": "ANA \"PC\""}));
        assert_eq!(sign_in_request(None), "");
    }

    #[test]
    fn the_prices_are_read_or_else_the_built_in_ones_stand() {
        let offer = |tier, monthly, yearly, cap_seconds| Offer {
            tier,
            monthly,
            first: 0,
            yearly,
            cap_seconds,
        };
        let built_in = Prices::default();
        assert_eq!(
            built_in.offers,
            [
                offer(Tier::Plus, 250, 2400, 28_800),
                offer(Tier::Pro, 500, 4900, 90_000)
            ]
        );
        assert_eq!(built_in.currency, "eur");
        // A server from before there were tiers: its two plans are those of
        // Pro, with the allowance it names for all.
        let said = parse_prices(&json!({
            "plans": [
                {"plan": "yearly", "amount": 4900, "currency": "EUR", "interval": "year"},
                {"plan": "monthly", "amount": 500, "currency": "EUR", "interval": "month"},
                {"plan": "lifetime", "amount": 1, "currency": "EUR", "interval": "ever"},
            ],
            "cap_seconds": 144_000,
        }));
        assert_eq!(
            said,
            Some(Prices {
                offers: vec![offer(Tier::Pro, 500, 4900, 144_000)],
                currency: "eur".into(),
                open: true,
            })
        );
        // Each tier with its own plans and allowance, the smaller first
        // whatever the order they come in; a plan that names no allowance
        // has the one named for all, and a tier this version does not know
        // is left out.
        let plan = |tier: &str, plan: &str, amount: u32, cap: Value| {
            json!({"tier": tier, "plan": plan, "amount": amount, "currency": "eur",
                "cap_seconds": cap})
        };
        let said = parse_prices(&json!({
            "plans": [
                plan("pro", "monthly", 600, json!(72_000)),
                plan("pro", "yearly", 5900, json!(72_000)),
                plan("plus", "monthly", 300, Value::Null),
                plan("plus", "yearly", 2900, Value::Null),
                plan("max", "monthly", 900, json!(1)),
                plan("max", "yearly", 9900, json!(1)),
            ],
            "cap_seconds": 30_000,
        }));
        assert_eq!(
            said.map(|prices| prices.offers),
            Some(vec![
                offer(Tier::Plus, 300, 2900, 30_000),
                offer(Tier::Pro, 600, 5900, 72_000)
            ])
        );
        // A tier with one of its plans missing is not on offer.
        let said = parse_prices(&json!({"plans": [
            plan("pro", "monthly", 600, json!(72_000)),
            plan("pro", "yearly", 5900, json!(72_000)),
            plan("plus", "monthly", 300, json!(28_800)),
        ]}));
        assert_eq!(
            said.map(|prices| prices.offers),
            Some(vec![offer(Tier::Pro, 600, 5900, 72_000)])
        );
        // Half an answer is none.
        for bad in [
            json!({}),
            json!({"plans": []}),
            json!({"plans": [{"plan": "monthly", "amount": 600, "currency": "eur"}]}),
            json!({"plans": [
                {"plan": "monthly", "amount": 600, "currency": "eur"},
                {"plan": "yearly", "amount": 5900, "currency": "usd"},
            ]}),
            json!({"plans": [
                {"plan": "monthly", "amount": -1, "currency": "eur"},
                {"plan": "yearly", "amount": 5900, "currency": "eur"},
            ]}),
        ] {
            assert_eq!(parse_prices(&bad), None, "{bad}");
        }
    }

    #[test]
    fn an_account_is_new_only_when_the_server_says_so() {
        // Whether the price of a first month is this account's is the
        // server's to say, and nothing else stands for a yes.
        for (said, eligible) in [
            (json!(true), true),
            (json!(false), false),
            (json!("yes"), false),
        ] {
            let account = parse_account(&json!({
                "email": "ana@example.com", "entitled": false, "intro_eligible": said
            }));
            assert_eq!(
                account.map(|account| account.intro_eligible),
                Some(eligible)
            );
        }
    }

    #[test]
    fn the_checkout_is_open_unless_the_server_says_it_is_not() {
        assert!(Prices::default().open);
        for (said, open) in [
            (Value::Null, true),
            (json!(true), true),
            (json!("no"), true),
            (json!(false), false),
        ] {
            let prices = parse_prices(&json!({"checkout_open": said, "plans": [
                {"plan": "monthly", "amount": 500, "currency": "eur"},
                {"plan": "yearly", "amount": 4900, "currency": "eur"},
            ]}));
            assert_eq!(prices.map(|prices| prices.open), Some(open));
        }
    }

    #[test]
    fn the_price_of_a_first_month_is_the_servers_or_there_is_none() {
        // Never the app's own promise.
        assert!(BUILT_IN.iter().all(|offer| offer.first == 0));
        let said = |plus: Value, pro: Value, yearly: Value| {
            let plan = |tier: &str, plan: &str, amount: u32, intro: &Value| {
                json!({"tier": tier, "plan": plan, "amount": amount, "currency": "eur",
                    "intro": intro})
            };
            let prices = parse_prices(&json!({"plans": [
                plan("plus", "monthly", 250, &plus),
                plan("plus", "yearly", 2400, &yearly),
                plan("pro", "monthly", 500, &pro),
                plan("pro", "yearly", 4900, &yearly),
            ]}));
            let offers = prices.map(|prices| prices.offers).unwrap_or_default();
            // Whatever is said about a first month, the prices stand.
            assert_eq!(offers.len(), 2);
            assert!(offers.iter().all(|offer| offer.yearly > offer.monthly));
            (offers[0].first, offers[1].first)
        };
        let intro = |amount: u32, months: u32| json!({"amount": amount, "months": months});
        assert_eq!(said(intro(149, 1), intro(349, 1), Value::Null), (149, 349));
        // Of one tier alone; and a year has none, whatever is said of it.
        assert_eq!(said(Value::Null, intro(349, 1), intro(1900, 1)), (0, 349));
        // Not for three months, not at the price of a month or above it, not
        // for nothing, and not in a shape this version does not know.
        assert_eq!(said(intro(149, 3), intro(500, 1), Value::Null), (0, 0));
        assert_eq!(said(intro(0, 1), intro(600, 1), Value::Null), (0, 0));
        assert_eq!(
            said(
                json!(149),
                json!({"amount": "349", "months": 1}),
                Value::Null
            ),
            (0, 0)
        );
        assert_eq!(said(json!({"amount": 149}), json!({}), Value::Null), (0, 0));
    }

    #[test]
    fn the_token_requests_are_what_the_server_takes() {
        let device: Value = serde_json::from_str(&device_request("dev")).unwrap_or_default();
        assert_eq!(
            device,
            json!({"grant_type": "device_code", "device_code": "dev"})
        );
        let refresh: Value = serde_json::from_str(&refresh_request("r\"1")).unwrap_or_default();
        assert_eq!(
            refresh,
            json!({"grant_type": "refresh_token", "refresh_token": "r\"1"})
        );
    }

    #[test]
    fn the_tier_of_an_account_is_read_and_pro_where_none_is_named() {
        // The tier is the one in effect, or else the one of the
        // subscription; an answer that names none, or one this version does
        // not know, is Pro.
        let tier = |top: Value, subscription: Value| {
            parse_account(&json!({
                "email": "a@example.com", "entitled": true, "tier": top,
                "subscription": {"status": "active", "tier": subscription},
            }))
            .map(|account| account.tier)
        };
        assert_eq!(tier(json!("plus"), json!("plus")), Some(Tier::Plus));
        assert_eq!(tier(Value::Null, json!("plus")), Some(Tier::Plus));
        assert_eq!(tier(json!("pro"), json!("plus")), Some(Tier::Pro));
        assert_eq!(tier(Value::Null, Value::Null), Some(Tier::Pro));
        assert_eq!(tier(json!("max"), Value::Null), Some(Tier::Pro));
    }

    #[test]
    fn a_sign_in_with_a_code_is_read_from_the_servers_answer() {
        let sign_in = parse_sign_in(&json!({
            "device_code": "dev", "verification_url": "https://x/start?code=1",
            "confirm_code": "K7M2QX", "interval": 2, "expires_in": 600
        }));
        let Some(sign_in) = sign_in else {
            unreachable!("a complete answer reads");
        };
        assert_eq!(sign_in.confirm_code, "K7M2QX");
        assert!(matches!(
            &sign_in.way,
            Way::Asking { device_code, interval }
                if device_code == "dev" && *interval == Duration::from_secs(2)
        ));
        assert_eq!(sign_in.expires_in, Duration::from_secs(600));
        assert_eq!(sign_in.minutes(), 10);
        assert!(!format!("{sign_in:?}").contains("dev"));
        assert!(parse_sign_in(&json!({"verification_url": "https://x"})).is_none());
    }

    #[test]
    fn the_answers_of_the_server_are_read() {
        let granted = parse_granted(&json!({
            "access_token": "a", "expires_in": 3600, "refresh_token": "r", "email": "ana@example.com"
        }));
        assert!(granted.is_some_and(|granted| {
            granted.expires_in == Duration::from_secs(3600) && granted.email == "ana@example.com"
        }));
        assert!(parse_granted(&json!({"access_token": "a"})).is_none());

        let account = parse_account(&json!({
            "email": "ana@example.com", "entitled": true,
            "subscription": {"status": "active", "plan": "monthly",
                "current_period_end": 1_793_837_814_u64, "cancel_at_period_end": false},
            "usage": {"month": "2026-10", "seconds": 5210, "cap_seconds": 144_000}
        }));
        let plain = AccountInfo {
            email: "ana@example.com".into(),
            entitled: true,
            subscription: Some(Subscription {
                status: "active".into(),
                plan: Some(Plan::Monthly),
                period_end: 1_793_837_814,
                ends: false,
                grace_until: 0,
                ends_at: 0,
                ended_at: 0,
            }),
            used_seconds: 5210,
            cap_seconds: 144_000,
            tier: Tier::Pro,
            resets_at: 0,
            complimentary: false,
            intro_eligible: false,
        };
        assert_eq!(account, Some(plain.clone()));
        assert!(!plain.at_no_charge());
        // The dates the server gives where they apply.
        let dated = parse_account(&json!({
            "email": "ana@example.com", "entitled": true,
            "subscription": {"status": "past_due", "plan": "monthly",
                "current_period_start": 1_791_245_814_u64,
                "current_period_end": 1_793_837_814_u64, "cancel_at_period_end": true,
                "grace_until": 1_794_097_014_u64, "ends_at": 1_793_837_814_u64,
                "ended_at": 1_793_900_000_u64},
            "usage": {"month": "2026-10", "seconds": 5210, "cap_seconds": 144_000,
                "resets_at": 1_793_491_200_u64}
        }));
        let Some(dated) = dated else {
            unreachable!("a complete answer reads");
        };
        assert_eq!(dated.resets_at, 1_793_491_200);
        assert_eq!(
            dated.subscription.map(|subscription| (
                subscription.ends,
                subscription.grace_until,
                subscription.ends_at,
                subscription.ended_at
            )),
            Some((true, 1_794_097_014, 1_793_837_814, 1_793_900_000))
        );
        let never = parse_account(&json!({
            "email": "ana@example.com", "entitled": false, "subscription": null,
            "usage": {"month": "2026-10", "seconds": 0, "cap_seconds": 144_000}
        }));
        assert!(never.is_some_and(|account| account.subscription.is_none()
            && !account.entitled
            && !account.complimentary
            && !account.intro_eligible
            && !account.over_allowance()));
        // An account the server gives Pro to: at no charge unless a
        // subscription of its own is paying, whatever became of an old one.
        let given = |subscription: Value| {
            parse_account(&json!({
                "email": "ana@example.com", "entitled": true, "complimentary": true,
                "subscription": subscription,
                "usage": {"month": "2026-10", "seconds": 0, "cap_seconds": 144_000}
            }))
        };
        for (subscription, free) in [
            (Value::Null, true),
            (json!({"status": "canceled", "plan": "monthly"}), true),
            (json!({"status": "active", "plan": "monthly"}), false),
            (json!({"status": "trialing", "plan": "yearly"}), false),
        ] {
            let account = given(subscription);
            assert!(account.is_some_and(|account| account.complimentary
                && account.entitled
                && account.at_no_charge() == free));
        }
    }

    #[test]
    fn an_access_token_is_good_until_shortly_before_it_runs_out() {
        let now = Instant::now();
        let access = Access {
            token: "a".into(),
            expires: now + Duration::from_secs(3600),
        };
        assert!(access.good_for(EXPIRY_MARGIN, now));
        assert!(access.good_for(FRESH_FOR, now));
        let later = now + Duration::from_secs(3600 - 600);
        assert!(access.good_for(EXPIRY_MARGIN, later));
        assert!(!access.good_for(FRESH_FOR, later));
        let at_the_end = now + Duration::from_secs(3600 - 30);
        assert!(!access.good_for(EXPIRY_MARGIN, at_the_end));
        assert!(!access.good_for(EXPIRY_MARGIN, now + Duration::from_secs(7200)));
    }

    #[test]
    fn what_is_kept_survives_being_written_down() {
        let kept = Kept {
            refresh_token: "r-1".into(),
            email: "ana@example.com".into(),
        };
        assert!(Kept::from_text(&kept.to_text()) == Some(kept));
        assert!(Kept::from_text("r-1").is_some_and(|kept| kept.email.is_empty()));
        assert!(Kept::from_text("").is_none());
        assert!(Kept::from_text("\nana@example.com").is_none());
    }

    #[cfg(windows)]
    #[test]
    fn signing_in_polls_until_the_browser_is_done_and_keeps_the_session() -> Outcome {
        let fake = Fake::start()?;
        fake.change(|server| server.pending = 2);
        let (account, memory) = fake.account();
        assert_eq!(account.signed_in_as(), None);
        assert_eq!(account.access_token(), Err(Failure::SignedOut));

        let sign_in = account
            .start_sign_in_with_code()
            .map_err(|err| err.to_string())?;
        assert_eq!(sign_in.confirm_code, "K7M2QX");
        assert!(sign_in.verification_url.starts_with(&fake.base));
        // The server was told what this PC is called, if it has a name.
        let name = device_name(std::env::var("COMPUTERNAME").ok());
        assert_eq!(
            fake.read(|server| server.device_name.as_str().map(str::to_owned)),
            name
        );
        let email = account.finish_sign_in(&sign_in, &AtomicBool::new(false));
        assert_eq!(email.as_deref(), Ok("ana@example.com"));
        // Asked three times: twice too early.
        assert_eq!(fake.asked("POST /v1/auth/token"), 3);
        assert_eq!(account.signed_in_as().as_deref(), Some("ana@example.com"));
        assert!(
            memory
                .refresh_token()
                .is_some_and(|token| token == fake.read(|s| s.refresh.clone()))
        );
        assert!(account.is_ready());
        // The session it came with is used as it is.
        assert!(account.info().is_ok_and(|info| info.entitled));
        assert_eq!(fake.asked("POST /v1/auth/token"), 3);
        Ok(())
    }

    /// What the page of a sign-in without a code to type was opened with:
    /// the port, the challenge and the state.
    fn opened_with(sign_in: &SignIn, fake: &Fake) -> Option<(u16, String, String)> {
        let query = sign_in
            .verification_url
            .strip_prefix(&format!("{}/v1/auth/app/start?", fake.base))?;
        let mut parts = query.split('&');
        let port = parts.next()?.strip_prefix("port=")?.parse().ok()?;
        let challenge = parts.next()?.strip_prefix("code_challenge=")?.to_owned();
        let state = parts.next()?.strip_prefix("state=")?.to_owned();
        parts.next().is_none().then_some((port, challenge, state))
    }

    /// Comes back to the app on `port` as the browser does, with `query`,
    /// and returns where the app sends the browser next.
    fn come_back(port: u16, query: &str) -> std::io::Result<String> {
        use std::io::{Read as _, Write as _};
        let mut browser = std::net::TcpStream::connect(("127.0.0.1", port))?;
        browser.set_read_timeout(Some(Duration::from_secs(20)))?;
        let request = format!("GET /callback?{query} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n");
        browser.write_all(request.as_bytes())?;
        let mut answer = String::new();
        browser.read_to_string(&mut answer)?;
        let location = answer
            .lines()
            .find_map(|line| line.strip_prefix("Location: "));
        Ok(location.unwrap_or(&answer).to_owned())
    }

    #[cfg(windows)]
    #[test]
    fn signing_in_needs_no_code_when_the_browser_can_come_back_to_this_pc() -> Outcome {
        let fake = Fake::start()?;
        let (account, memory) = fake.account();
        let sign_in = account.start_sign_in().map_err(|err| err.to_string())?;
        // Nothing to type, nothing asked of the server yet, and a page of
        // the server's to open.
        assert_eq!(sign_in.confirm_code, "");
        assert_eq!(sign_in.minutes(), 10);
        assert_eq!(fake.asked("POST /v1/auth/device"), 0);
        assert!(account.server().is_sign_in_page(&sign_in.verification_url));
        let (port, challenge, state) =
            opened_with(&sign_in, &fake).ok_or("not the address of a sign-in that listens")?;
        // What the address carries is the hash of the secret, not the
        // secret, and both are 256 bits in base64url.
        let Way::Listening { verifier, .. } = &sign_in.way else {
            return Err("the sign-in does not listen".into());
        };
        assert_eq!(challenge, challenge_of(verifier));
        assert_ne!(&challenge, verifier);
        assert_eq!((verifier.len(), challenge.len(), state.len()), (43, 43, 43));
        assert!(!format!("{sign_in:?}").contains(verifier.as_str()));

        // The browser comes back from the server with a code, and is sent
        // on to the page that says so once the app has its session.
        fake.change(|server| server.app_code = Some(("code-1".to_owned(), challenge)));
        let browser =
            std::thread::spawn(move || come_back(port, &format!("code=code-1&state={state}")));
        let email = account.finish_sign_in(&sign_in, &AtomicBool::new(false));
        assert_eq!(email.as_deref(), Ok("ana@example.com"));
        let sent_to = browser.join().map_err(|_| "the browser panicked")??;
        assert_eq!(
            sent_to,
            format!("{}/v1/auth/app/done?result=signed_in", fake.base)
        );

        // One request made the session, which is kept and used as it is.
        assert_eq!(fake.asked("POST /v1/auth/token"), 1);
        let name = device_name(std::env::var("COMPUTERNAME").ok());
        assert_eq!(
            fake.read(|server| server.device_name.as_str().map(str::to_owned)),
            name
        );
        assert_eq!(account.signed_in_as().as_deref(), Some("ana@example.com"));
        assert!(
            memory
                .refresh_token()
                .is_some_and(|token| token == fake.read(|s| s.refresh.clone()))
        );
        assert!(account.is_ready());
        assert!(account.info().is_ok_and(|info| info.entitled));
        assert_eq!(fake.asked("POST /v1/auth/token"), 1);
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn a_sign_in_that_ends_badly_in_the_browser_signs_nobody_in_and_says_why() -> Outcome {
        let fake = Fake::start()?;
        let cases = [
            ("error=cancelled", Failure::Stopped, "cancelled"),
            (
                "error=unverified",
                Failure::Browser(UNVERIFIED),
                "unverified",
            ),
            ("error=failed", Failure::Browser(NOT_THROUGH), "failed"),
            (
                "error=something_new",
                Failure::Browser(NOT_THROUGH),
                "failed",
            ),
            // A code the server does not take: spent, or too late.
            ("code=stale", Failure::Expired, "failed"),
        ];
        for (outcome, failure, result) in cases {
            let (account, _) = fake.account();
            let sign_in = account.start_sign_in().map_err(|err| err.to_string())?;
            let (port, _, state) = opened_with(&sign_in, &fake).ok_or("not that address")?;
            let query = format!("{outcome}&state={state}");
            let browser = std::thread::spawn(move || come_back(port, &query));
            assert_eq!(
                account.finish_sign_in(&sign_in, &AtomicBool::new(false)),
                Err(failure),
                "{outcome}"
            );
            let sent_to = browser.join().map_err(|_| "the browser panicked")??;
            assert_eq!(
                sent_to,
                format!("{}/v1/auth/app/done?result={result}", fake.base),
                "{outcome}"
            );
            assert_eq!(account.signed_in_as(), None, "{outcome}");
        }
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn a_code_brought_with_another_state_or_secret_signs_nobody_in() -> Outcome {
        let fake = Fake::start()?;
        let (account, _) = fake.account();
        let sign_in = account.start_sign_in().map_err(|err| err.to_string())?;
        let (port, challenge, state) = opened_with(&sign_in, &fake).ok_or("not that address")?;
        fake.change(|server| server.app_code = Some(("code-1".to_owned(), challenge)));

        // Someone else's sign-in, brought to this app: not taken, and the
        // server is asked nothing. The app goes on waiting for its own.
        let stop = Arc::new(AtomicBool::new(false));
        let waiting = {
            let (account, stop) = (Arc::clone(&account), Arc::clone(&stop));
            std::thread::spawn(move || account.finish_sign_in(&sign_in, &stop))
        };
        let other = "b".repeat(43);
        let answer = come_back(port, &format!("code=code-1&state={other}"))?;
        assert!(answer.starts_with("HTTP/1.1 404 "), "{answer}");
        assert_eq!(fake.asked("POST /v1/auth/token"), 0);
        assert!(!waiting.is_finished());

        // Its own code, traded by another app with a secret of its own, is
        // refused and spent: the challenge was this app's.
        let (stranger, _) = fake.account();
        let traded = stranger.trade_app_code("code-1", &"c".repeat(43), &AtomicBool::new(false));
        assert_eq!(traded, Err(Failure::Expired));
        assert_eq!(stranger.signed_in_as(), None);

        stop.store(true, Ordering::Relaxed);
        let ended = waiting.join().map_err(|_| "the wait panicked")?;
        assert_eq!(ended, Err(Failure::Stopped));
        assert_eq!(account.signed_in_as(), None);
        // What the browser would have brought now finds nobody listening.
        assert!(come_back(port, &format!("code=code-1&state={state}")).is_err());
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn a_sign_in_called_off_while_its_code_was_traded_signs_nobody_in() -> Outcome {
        let fake = Fake::start()?;
        let (account, _) = fake.account();
        fake.change(|server| {
            server.app_code = Some(("code-1".to_owned(), challenge_of("secret")));
        });
        // Called off already when the answer arrives: the session the
        // server made is given back, and none is kept.
        let stop = AtomicBool::new(true);
        assert_eq!(
            account.trade_app_code("code-1", "secret", &stop),
            Err(Failure::Stopped)
        );
        assert_eq!(account.signed_in_as(), None);
        assert_eq!(fake.asked("POST /v1/auth/signout"), 1);
        assert_eq!(fake.read(|server| server.refresh.clone()), "signed-out");
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn a_code_is_traded_again_when_the_server_is_busy_for_a_moment() -> Outcome {
        let fake = Fake::start()?;
        let (account, _) = fake.account();
        fake.change(|server| {
            server.app_code = Some(("code-1".to_owned(), challenge_of("secret")));
            server.faults = vec![("POST /v1/auth/token", 503, "unavailable")];
        });
        let traded = account.trade_app_code("code-1", "secret", &AtomicBool::new(false));
        assert_eq!(traded.as_deref(), Ok("ana@example.com"));
        assert_eq!(fake.asked("POST /v1/auth/token"), 2);
        Ok(())
    }

    #[test]
    fn a_challenge_is_the_hash_of_its_secret_as_pkce_makes_it() {
        // RFC 7636, appendix B.
        assert_eq!(
            challenge_of("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let (first, second) = (random_token(), random_token());
        assert!(first.as_ref().is_some_and(|token| token.len() == 43));
        assert_ne!(first, second);
        let request: Value =
            serde_json::from_str(&app_code_request("c", "v", Some("ANA \"PC\"")))
                .unwrap_or_default();
        assert_eq!(
            request,
            json!({
                "grant_type": "app_code", "code": "c", "code_verifier": "v",
                "device_name": "ANA \"PC\"",
            })
        );
        let unnamed: Value =
            serde_json::from_str(&app_code_request("c", "v", None)).unwrap_or_default();
        assert_eq!(
            unnamed,
            json!({"grant_type": "app_code", "code": "c", "code_verifier": "v"})
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_sign_in_ends_when_it_is_called_off_or_forgotten() -> Outcome {
        let fake = Fake::start()?;
        fake.change(|server| server.pending = u32::MAX);
        let (account, memory) = fake.account();
        let sign_in = account
            .start_sign_in_with_code()
            .map_err(|err| err.to_string())?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            stopping.store(true, Ordering::Relaxed);
        });
        assert_eq!(
            account.finish_sign_in(&sign_in, &stop),
            Err(Failure::Stopped)
        );
        let _ = stopper.join();
        assert!(memory.refresh_token().is_none());

        // The server no longer knows the code.
        fake.change(|server| {
            server.pending = 0;
            server.device_traded = true;
        });
        assert_eq!(
            account.finish_sign_in(&sign_in, &AtomicBool::new(false)),
            Err(Failure::Expired)
        );
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn a_refresh_keeps_the_next_token_before_the_session_is_used() -> Outcome {
        let fake = Fake::start()?;
        let (account, memory) = fake.account();
        memory.sign_in(&fake);
        let first = fake.read(|server| server.refresh.clone());

        // Nothing in memory after a start: the first request trades the token.
        assert!(!account.is_ready());
        assert!(account.info().is_ok());
        let second = fake.read(|server| server.refresh.clone());
        assert_ne!(first, second);
        assert_eq!(memory.refresh_token(), Some(second.clone()));
        assert_eq!(account.signed_in_as().as_deref(), Some("ana@example.com"));

        // Good for an hour: nothing is traded for the next request, nor
        // for keeping it fresh.
        assert!(account.info().is_ok());
        assert!(account.keep_fresh().is_ok());
        assert_eq!(fake.asked("POST /v1/auth/token"), 1);

        // An access token the server has stopped taking is traded once.
        fake.change(|server| server.access = "withdrawn".to_owned());
        assert!(account.info().is_ok());
        assert_eq!(fake.asked("POST /v1/auth/token"), 2);
        assert_ne!(memory.refresh_token(), Some(second));
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn a_token_that_runs_out_soon_is_traded_ahead_of_time() -> Outcome {
        let fake = Fake::start()?;
        // Shorter than what a dictation wants ahead, longer than the margin.
        fake.change(|server| server.expires_in = 300);
        let (account, memory) = fake.account();
        memory.sign_in(&fake);
        assert!(account.access_token().is_ok());
        assert_eq!(fake.asked("POST /v1/auth/token"), 1);
        assert!(account.access_token().is_ok());
        assert_eq!(fake.asked("POST /v1/auth/token"), 1);
        assert!(account.keep_fresh().is_ok());
        assert_eq!(fake.asked("POST /v1/auth/token"), 2);
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn a_session_the_server_refuses_is_let_go_of() -> Outcome {
        let fake = Fake::start()?;
        let (account, memory) = fake.account();
        memory.sign_in(&fake);
        fake.change(|server| server.refresh = "another".to_owned());
        assert_eq!(account.info(), Err(Failure::SignedOut));
        assert!(memory.refresh_token().is_none());
        assert_eq!(account.signed_in_as(), None);
        assert_eq!(account.access_token(), Err(Failure::SignedOut));
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn a_token_another_process_traded_is_not_the_end_of_the_session() -> Outcome {
        let fake = Fake::start()?;
        let (account, memory) = fake.account();
        memory.sign_in(&fake);
        // The other process trades it and keeps the next one, between this
        // one reading the vault and asking the server.
        let stale = Kept {
            refresh_token: "refresh-stale".into(),
            email: "ana@example.com".into(),
        };
        let current = fake.read(|server| server.refresh.clone());
        memory.swap_after_next_read(&stale, &current);
        assert!(account.access_token().is_ok());
        assert_eq!(fake.asked("POST /v1/auth/token"), 2);
        assert!(memory.refresh_token().is_some());
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn without_a_server_nothing_is_lost() -> Outcome {
        let fake = Fake::start()?;
        let (account, memory) = fake.account();
        memory.sign_in(&fake);
        let Some(nowhere) = Server::at("http://127.0.0.1:9") else {
            unreachable!("this computer is a server");
        };
        let offline = Account::new(nowhere, Box::new(memory.clone()));
        assert!(matches!(offline.info(), Err(Failure::Unreachable(_))));
        // Still signed in, with the same token, for when the server is back.
        assert!(memory.refresh_token().is_some());
        assert!(account.info().is_ok());
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn the_pages_for_paying_come_from_the_server() -> Outcome {
        let fake = Fake::start()?;
        let (account, memory) = fake.account();
        memory.sign_in(&fake);
        assert_eq!(
            account.checkout(Tier::Pro, Plan::Yearly).as_deref(),
            Ok("https://checkout.stripe.com/c/pay/pro-yearly")
        );
        assert_eq!(
            account.checkout(Tier::Plus, Plan::Monthly).as_deref(),
            Ok("https://checkout.stripe.com/c/pay/plus-monthly")
        );
        assert_eq!(
            account.portal().as_deref(),
            Ok("https://billing.stripe.com/p/session/test")
        );
        // What cannot be done is said by its reason, by servers old and new.
        for old in [false, true] {
            fake.change(|server| {
                server.subscribed = true;
                server.old_errors = old;
            });
            assert!(
                matches!(
                    account.checkout(Tier::Pro, Plan::Monthly),
                    Err(Failure::Refused {
                        reason: Reason::AlreadySubscribed,
                        ..
                    })
                ),
                "{old}"
            );
        }
        fake.change(|server| {
            server.old_errors = false;
            server.portal_url = String::new();
        });
        assert!(matches!(
            account.portal(),
            Err(Failure::Refused {
                reason: Reason::NothingToManage,
                ..
            })
        ));
        // An address that is no page of the payment service is never
        // handed on, whatever the server says.
        for address in [
            "file:///C:/Windows/calc.exe",
            "https://billing.example.com/portal",
            "http://billing.stripe.com/p/session/test",
        ] {
            fake.change(|server| server.portal_url = address.to_owned());
            assert!(
                matches!(account.portal(), Err(Failure::Unreachable(_))),
                "{address}"
            );
        }
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn the_prices_come_from_the_server_without_a_session() -> Outcome {
        let fake = Fake::start()?;
        let (account, memory) = fake.account();
        assert!(memory.refresh_token().is_none());
        assert_eq!(account.prices(), Ok(Prices::default()));
        fake.change(|server| {
            server.plans["plans"][0]["amount"] = json!(700);
            server.plans["plans"][0]["cap_seconds"] = json!(90_000);
        });
        let prices = account.prices().map_err(|err| err.to_string())?;
        let pro = prices
            .offers
            .last()
            .map(|pro| (pro.monthly, pro.cap_seconds));
        assert_eq!(pro, Some((700, 90_000)));
        // A server that does not know the route yet: the caller falls back.
        fake.change(|server| server.faults = vec![("GET /v1/plans", 404, "not_found")]);
        assert!(account.prices().is_err());
        assert_eq!(fake.asked("POST /v1/auth/token"), 0);
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn a_sign_in_survives_a_server_that_is_busy_or_at_fault_for_a_moment() -> Outcome {
        let fake = Fake::start()?;
        fake.change(|server| {
            server.pending = 1;
            server.faults = vec![
                ("POST /v1/auth/token", 429, "too_many_requests"),
                ("POST /v1/auth/token", 500, "internal"),
                ("POST /v1/auth/token", 502, "upstream_failed"),
                ("POST /v1/auth/token", 503, "unavailable"),
            ];
        });
        let (account, memory) = fake.account();
        let sign_in = account
            .start_sign_in_with_code()
            .map_err(|err| err.to_string())?;
        // The person finishes in the browser meanwhile; so does the app.
        let email = account.finish_sign_in(&sign_in, &AtomicBool::new(false));
        assert_eq!(email.as_deref(), Ok("ana@example.com"));
        assert_eq!(fake.asked("POST /v1/auth/token"), 6);
        assert!(memory.refresh_token().is_some());
        // The old wording of "not yet" is still understood.
        let fake = Fake::start()?;
        fake.change(|server| {
            server.pending = 2;
            server.old_errors = true;
        });
        let (account, _memory) = fake.account();
        let sign_in = account
            .start_sign_in_with_code()
            .map_err(|err| err.to_string())?;
        assert!(
            account
                .finish_sign_in(&sign_in, &AtomicBool::new(false))
                .is_ok()
        );
        // What does not pass ends it.
        let fake = Fake::start()?;
        fake.change(|server| server.faults = vec![("POST /v1/auth/token", 400, "bad_request")]);
        let (account, memory) = fake.account();
        let sign_in = account
            .start_sign_in_with_code()
            .map_err(|err| err.to_string())?;
        assert!(matches!(
            account.finish_sign_in(&sign_in, &AtomicBool::new(false)),
            Err(Failure::Refused {
                code: Code::BadRequest,
                ..
            })
        ));
        assert!(memory.refresh_token().is_none());
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn a_sign_in_called_off_while_its_answer_was_on_the_way_signs_nobody_in() -> Outcome {
        let fake = Fake::start()?;
        let (account, memory) = fake.account();
        let sign_in = account
            .start_sign_in_with_code()
            .map_err(|err| err.to_string())?;
        // Finished in the browser, and called off before the app has asked:
        // the wait ends before it asks, as ever.
        let stop = AtomicBool::new(true);
        assert_eq!(
            account.finish_sign_in(&sign_in, &stop),
            Err(Failure::Stopped)
        );
        assert_eq!(fake.asked("POST /v1/auth/token"), 0);
        // Called off while the answer is on its way: the session is not
        // kept, and the server is told to forget it.
        let stop = Arc::new(AtomicBool::new(false));
        let (calling_off, slow) = (Arc::clone(&stop), Duration::from_millis(300));
        fake.change(|server| server.slow_answers = slow);
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(120));
            calling_off.store(true, Ordering::Relaxed);
        });
        assert_eq!(
            account.finish_sign_in(&sign_in, &stop),
            Err(Failure::Stopped)
        );
        let _ = stopper.join();
        assert_eq!(fake.asked("POST /v1/auth/token"), 1);
        assert_eq!(fake.asked("POST /v1/auth/signout"), 1);
        assert!(memory.refresh_token().is_none());
        assert_eq!(account.signed_in_as(), None);
        assert!(!account.is_ready());
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn a_refresh_whose_answer_was_lost_is_made_again_at_once() -> Outcome {
        let fake = Fake::start()?;
        let (account, memory) = fake.account();
        memory.sign_in(&fake);
        let before = memory.refresh_token();
        // The server trades the token, and the answer never arrives.
        fake.change(|server| server.lost_answers = 1);
        assert!(account.keep_fresh().is_ok());
        // Asked twice, with the same token: the server took it again, and
        // what it answered the second time is what is kept.
        assert_eq!(fake.asked("POST /v1/auth/token"), 2);
        assert_ne!(memory.refresh_token(), before);
        assert_eq!(
            memory.refresh_token(),
            Some(fake.read(|server| server.refresh.clone()))
        );
        assert!(account.info().is_ok());
        assert_eq!(account.signed_in_as().as_deref(), Some("ana@example.com"));

        // Someone looking at a page waits for two more tries, not for all.
        let (looking, memory) = fake.account();
        memory.sign_in(&fake);
        fake.change(|server| server.lost_answers = 5);
        let asked = fake.asked("POST /v1/auth/token");
        assert!(matches!(looking.info(), Err(Failure::Unreachable(_))));
        assert_eq!(fake.asked("POST /v1/auth/token"), asked + 3);
        // Someone holding a key down does not wait at all.
        assert!(matches!(
            looking.access_token(),
            Err(Failure::Unreachable(_))
        ));
        assert_eq!(fake.asked("POST /v1/auth/token"), asked + 4);
        // Still signed in: the next quiet moment makes it good.
        assert!(memory.refresh_token().is_some());
        fake.change(|server| server.lost_answers = 1);
        assert!(looking.keep_fresh().is_ok());
        assert!(looking.info().is_ok());
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn two_processes_that_refresh_at_once_both_stay_signed_in() -> Outcome {
        for _ in 0..8 {
            let fake = Fake::start()?;
            // The resident process and a `vinput transcribe` beside it:
            // two accounts over one credential store.
            let (first, memory) = fake.account();
            let second = fake.account_keeping(&memory);
            memory.sign_in(&fake);
            let trade =
                |account: Arc<Account>| std::thread::spawn(move || account.access_token().is_ok());
            let (one, two) = (trade(Arc::clone(&first)), trade(Arc::clone(&second)));
            assert_eq!(one.join().ok(), Some(true));
            assert_eq!(two.join().ok(), Some(true));
            // They took turns: what is kept is the token the server gave
            // last, and both go on working with it.
            assert_eq!(
                memory.refresh_token(),
                Some(fake.read(|server| server.refresh.clone()))
            );
            fake.change(|server| server.access = "withdrawn".to_owned());
            assert!(first.info().is_ok());
            fake.change(|server| server.access = "withdrawn".to_owned());
            assert!(second.info().is_ok());
            assert!(memory.refresh_token().is_some());
        }
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn an_account_that_is_gone_signs_this_pc_out() -> Outcome {
        let fake = Fake::start()?;
        let (account, memory) = fake.account();
        memory.sign_in(&fake);
        // The token can be traded, and the server refuses the new one too:
        // the account was deleted, or this PC signed out elsewhere.
        fake.change(|server| {
            server.faults = vec![
                ("GET /v1/account", 401, "unauthorized"),
                ("GET /v1/account", 401, "unauthorized"),
            ];
        });
        assert_eq!(account.info(), Err(Failure::SignedOut));
        assert_eq!(fake.asked("GET /v1/account"), 2);
        assert!(memory.refresh_token().is_none());
        assert!(!account.is_ready());
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn removing_vinput_tells_the_server_and_never_waits_long_for_it() -> Outcome {
        let fake = Fake::start()?;
        let (account, memory) = fake.account();
        // Nobody signed in: nothing to tell.
        account.end_at_server(Duration::from_secs(1));
        assert_eq!(fake.asked("POST /v1/auth/signout"), 0);
        memory.sign_in(&fake);
        account.end_at_server(Duration::from_secs(1));
        assert_eq!(fake.asked("POST /v1/auth/signout"), 1);
        // What is kept is left for removal itself to delete.
        assert!(memory.refresh_token().is_some());
        // The server no longer takes the session.
        assert_eq!(account.info(), Err(Failure::SignedOut));

        // A server that does not answer holds removal up for the limit
        // and no longer.
        let started = Instant::now();
        assert!(!within(Duration::from_millis(100), || {
            std::thread::sleep(Duration::from_secs(3));
        }));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(within(Duration::from_secs(5), || {}));
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn a_token_that_was_handed_over_is_used_and_nothing_is_kept() -> Outcome {
        let fake = Fake::start()?;
        let access = fake.read(|server| server.access.clone());
        let server = Server::at(&fake.base).ok_or("the stand-in has an address")?;
        let account = Account::with_access_token(server.clone(), &format!(" {access}\n"));
        assert!(account.is_ready());
        assert_eq!(account.signed_in_as(), None);
        assert!(account.info().is_ok_and(|info| info.entitled));
        assert_eq!(fake.asked("POST /v1/auth/token"), 0);
        // One the server does not take cannot be traded for another.
        let stale = Account::with_access_token(server, "stale");
        assert_eq!(stale.info(), Err(Failure::SignedOut));
        assert_eq!(fake.asked("POST /v1/auth/token"), 0);
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn signing_out_forgets_the_session_here_and_there() -> Outcome {
        let fake = Fake::start()?;
        let (account, memory) = fake.account();
        memory.sign_in(&fake);
        assert!(account.info().is_ok());
        account.sign_out();
        assert!(memory.refresh_token().is_none());
        assert!(!account.is_ready());
        assert_eq!(fake.asked("POST /v1/auth/signout"), 1);
        assert_eq!(account.info(), Err(Failure::SignedOut));
        Ok(())
    }
}
