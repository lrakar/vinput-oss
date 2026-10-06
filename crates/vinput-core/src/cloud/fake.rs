//! A stand-in for the server, for tests: the routes of the account over
//! plain HTTP and the audio stream over a WebSocket, on a port of this
//! computer, with a session kept in memory instead of the credential store.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime::Runtime;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;

use super::{Account, Kept, Server, Vault};

/// How the stand-in treats one audio stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Script {
    /// Says it is ready, answers each piece of audio with a guess and the
    /// end of it with the settled text, then `done`.
    Usual,
    /// Refuses the upgrade with this status and code.
    Refuse(u16, &'static str),
    /// Refuses the upgrade with this status and says nothing more.
    RefuseBare(u16),
    /// Like `Usual`, but takes this long to answer the upgrade.
    SlowToOpen(Duration),
    /// Settles some text at the first audio, then ends with this error.
    FailMidway(&'static str),
    /// Settles some text at the first audio, then says that the month's
    /// allowance is used up, having counted this many milliseconds.
    SpentMidway(u32),
    /// Drops the connection at the first audio, without a word.
    DropMidway,
    /// Settles some text at the first audio and says `done`, as a server
    /// that is being restarted does, having counted this many milliseconds
    /// of the audio it was sent.
    DoneEarly(u32),
    /// Says `done` at the first audio without having settled or counted
    /// anything.
    DoneAtOnce,
    /// Lets the stream in and then reads nothing more, like a connection
    /// that has stalled without closing.
    Deaf,
}

/// What the stand-in saw of one audio stream.
#[derive(Debug, Clone, Default)]
pub(crate) struct Seen {
    pub(crate) events: Vec<&'static str>,
    pub(crate) languages: Value,
    pub(crate) sample_rate: Value,
    pub(crate) audio: Vec<i16>,
    /// The size of the largest binary message, in bytes.
    pub(crate) largest: usize,
    /// How many binary messages the audio came in.
    pub(crate) messages: usize,
}

/// What the stand-in knows and has seen. Tests change it as they go.
pub(crate) struct State {
    /// How many more times a device code is told to wait.
    pub(crate) pending: u32,
    /// Whether the device code has been traded, or forgotten.
    pub(crate) device_traded: bool,
    /// The refresh token that is good now.
    pub(crate) refresh: String,
    /// The one before it, which the server takes once more for a while.
    pub(crate) previous: Option<String>,
    /// How many answers to a trade of the refresh token are lost on their
    /// way: the trade happens, and the connection is dropped without a word.
    pub(crate) lost_answers: u32,
    /// Requests that are refused once each, in this order: the request, the
    /// status and the code.
    pub(crate) faults: Vec<(&'static str, u16, &'static str)>,
    /// Whether refusals are worded as servers did before there was a
    /// `reason`.
    pub(crate) old_errors: bool,
    /// The name the last sign-in was started with.
    pub(crate) device_name: Value,
    /// How long every answer takes.
    pub(crate) slow_answers: Duration,
    /// What `/v1/plans` answers.
    pub(crate) plans: Value,
    /// The access token that is good now.
    pub(crate) access: String,
    /// How long an access token is said to be good for, in seconds.
    pub(crate) expires_in: u64,
    granted: u32,
    /// Whether a checkout is refused because there is a subscription.
    pub(crate) subscribed: bool,
    /// What `/v1/account` answers.
    pub(crate) account: Value,
    /// What `/v1/billing/portal` answers with.
    pub(crate) portal_url: String,
    /// Every request, as `POST /v1/auth/token`.
    pub(crate) asked: Vec<String>,
    /// How each audio stream is treated, in the order they arrive; the ones
    /// after the last are treated as `Usual`.
    pub(crate) scripts: Vec<Script>,
    /// What was seen of each audio stream.
    pub(crate) streams: Vec<Seen>,
    /// How many connections were made to the stand-in.
    pub(crate) connections: usize,
    /// How many of them ended without anything having been asked on them.
    pub(crate) left_unused: usize,
    /// How long a connection may stay without anything being asked on it
    /// before the stand-in hangs up; `None` waits for as long as it takes.
    pub(crate) unused_for: Option<Duration>,
}

impl State {
    fn grant(&mut self) -> Value {
        self.granted += 1;
        self.previous = Some(std::mem::take(&mut self.refresh));
        self.refresh = format!("refresh-{}", self.granted);
        self.access = format!("access-{}", self.granted);
        json!({
            "access_token": self.access, "expires_in": self.expires_in,
            "refresh_token": self.refresh, "email": "ana@example.com",
        })
    }
}

type Shared = Arc<Mutex<State>>;

/// The stand-in, running until it is dropped.
pub(crate) struct Fake {
    // Kept for the tasks that run on it.
    _runtime: Runtime,
    /// Its address, such as `http://127.0.0.1:50123`.
    pub(crate) base: String,
    state: Shared,
}

impl Fake {
    pub(crate) fn start() -> std::io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()?;
        let listener = runtime.block_on(TcpListener::bind("127.0.0.1:0"))?;
        let base = format!("http://{}", listener.local_addr()?);
        let state = Arc::new(Mutex::new(State {
            pending: 0,
            device_traded: false,
            refresh: "refresh-0".to_owned(),
            previous: None,
            lost_answers: 0,
            faults: Vec::new(),
            old_errors: false,
            device_name: Value::Null,
            slow_answers: Duration::ZERO,
            plans: json!({
                "plans": [
                    {"tier": "pro", "plan": "monthly", "amount": 500, "currency": "eur",
                        "interval": "month", "cap_seconds": 90_000},
                    {"tier": "pro", "plan": "yearly", "amount": 4900, "currency": "eur",
                        "interval": "year", "cap_seconds": 90_000},
                    {"tier": "plus", "plan": "monthly", "amount": 250, "currency": "eur",
                        "interval": "month", "cap_seconds": 28_800},
                    {"tier": "plus", "plan": "yearly", "amount": 2400, "currency": "eur",
                        "interval": "year", "cap_seconds": 28_800},
                ],
                "cap_seconds": 90_000,
            }),
            access: "access-0".to_owned(),
            expires_in: 3600,
            granted: 0,
            subscribed: false,
            account: json!({
                "email": "ana@example.com", "entitled": true, "tier": "pro",
                "subscription": {"status": "active", "plan": "monthly", "tier": "pro",
                    "current_period_start": 1_791_245_814_u64,
                    "current_period_end": 1_793_837_814_u64, "cancel_at_period_end": false},
                "usage": {"month": "2026-10", "seconds": 5210, "cap_seconds": 90_000,
                    "resets_at": 1_793_491_200_u64},
            }),
            portal_url: "https://billing.stripe.com/p/session/test".to_owned(),
            asked: Vec::new(),
            scripts: Vec::new(),
            streams: Vec::new(),
            connections: 0,
            left_unused: 0,
            unused_for: None,
        }));
        let shared = Arc::clone(&state);
        let address = base.clone();
        runtime.spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(connection(stream, Arc::clone(&shared), address.clone()));
            }
        });
        Ok(Self {
            _runtime: runtime,
            base,
            state,
        })
    }

    pub(crate) fn change(&self, change: impl FnOnce(&mut State)) {
        change(&mut self.state.lock().unwrap_or_else(PoisonError::into_inner));
    }

    pub(crate) fn read<T>(&self, read: impl FnOnce(&State) -> T) -> T {
        read(&self.state.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// How often `request`, such as `POST /v1/auth/token`, was made.
    pub(crate) fn asked(&self, request: &str) -> usize {
        self.read(|state| state.asked.iter().filter(|asked| *asked == request).count())
    }

    /// Waits until what was seen of the audio streams passes `check`. Says
    /// whether it did.
    pub(crate) fn wait(&self, check: impl Fn(&[Seen]) -> bool) -> bool {
        self.wait_for(|state| check(&state.streams))
    }

    /// Waits until what the stand-in knows passes `check`. Says whether it
    /// did.
    pub(crate) fn wait_for(&self, check: impl Fn(&State) -> bool) -> bool {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(5) {
            if self.read(&check) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    /// An account at the stand-in with nobody signed in, and where its
    /// session is kept.
    pub(crate) fn account(&self) -> (Arc<Account>, Memory) {
        let memory = Memory::default();
        (self.account_keeping(&memory), memory)
    }

    /// Another account at the stand-in that keeps its session in `memory`:
    /// what a second Vinput process on the same PC is.
    pub(crate) fn account_keeping(&self, memory: &Memory) -> Arc<Account> {
        /// Tests do not wait half a minute for a retry.
        const QUICK: [Duration; 3] = [Duration::from_millis(10); 3];
        let server = Server {
            base: self.base.clone(),
        };
        let mut account = Account::new(server, Box::new(memory.clone()));
        account.poll_floor = Duration::from_millis(10);
        account.retry_waits = &QUICK;
        Arc::new(account)
    }
}

struct Held {
    kept: Option<Kept>,
    /// Takes the place of `kept` once that has been read.
    then: Option<Kept>,
    /// The name its users take turns under, as processes that share the
    /// credential store do.
    turn: String,
}

/// A session kept in memory.
#[derive(Clone)]
pub(crate) struct Memory(Arc<Mutex<Held>>);

impl Default for Memory {
    fn default() -> Self {
        static COUNT: AtomicU32 = AtomicU32::new(0);
        let turn = format!(
            "Local\\VinputTurnTest-memory-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        );
        Self(Arc::new(Mutex::new(Held {
            kept: None,
            then: None,
            turn,
        })))
    }
}

impl Memory {
    fn held(&self) -> std::sync::MutexGuard<'_, Held> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// As if someone had signed in earlier: the refresh token the stand-in
    /// takes is kept, and nothing is in memory.
    pub(crate) fn sign_in(&self, fake: &Fake) {
        self.held().kept = Some(Kept {
            refresh_token: fake.read(|state| state.refresh.clone()),
            email: "ana@example.com".to_owned(),
        });
    }

    pub(crate) fn refresh_token(&self) -> Option<String> {
        self.held()
            .kept
            .as_ref()
            .map(|kept| kept.refresh_token.clone())
    }

    /// Keeps `first` until it has been read once, and `refresh_token` from
    /// then on: what another process trading the token looks like.
    pub(crate) fn swap_after_next_read(&self, first: &Kept, refresh_token: &str) {
        let mut held = self.held();
        held.kept = Some(first.clone());
        held.then = Some(Kept {
            refresh_token: refresh_token.to_owned(),
            email: first.email.clone(),
        });
    }
}

impl Vault for Memory {
    fn get(&self) -> Option<Kept> {
        let mut held = self.held();
        let kept = held.kept.clone();
        if let Some(then) = held.then.take() {
            held.kept = Some(then);
        }
        kept
    }

    fn set(&self, kept: &Kept) -> bool {
        self.held().kept = Some(kept.clone());
        true
    }

    fn clear(&self) {
        self.held().kept = None;
    }

    fn turn(&self) -> Option<String> {
        Some(self.held().turn.clone())
    }
}

fn error(code: &str, message: &str) -> String {
    json!({"error": {"code": code, "message": message}}).to_string()
}

/// A refusal the app acts on by its `reason`, or as servers worded it
/// before there was one.
fn reasoned(old: bool, reason: &str, old_message: &str) -> String {
    if old {
        return error("bad_request", old_message);
    }
    json!({"error": {"code": "bad_request", "reason": reason, "message": "for a person"}})
        .to_string()
}

/// Where the head of a request ends in `bytes`, if it is all there.
fn head_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

async fn connection(mut stream: TcpStream, state: Shared, base: String) {
    let change = |change: &dyn Fn(&mut State)| {
        change(&mut state.lock().unwrap_or_else(PoisonError::into_inner));
    };
    change(&|state| state.connections += 1);
    let unused_for = state
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .unused_for;
    // Looked at without being read, so that the WebSocket handshake finds
    // the request where it expects it.
    let mut peeked = vec![0u8; 8192];
    let started = Instant::now();
    let head = loop {
        let peeking = stream.peek(&mut peeked);
        let seen = match unused_for {
            Some(limit) => tokio::time::timeout(limit, peeking).await.unwrap_or(Ok(0)),
            None => peeking.await,
        };
        let seen = seen.unwrap_or(0);
        if head_end(&peeked[..seen]).is_some() {
            break String::from_utf8_lossy(&peeked[..seen]).into_owned();
        }
        if seen == 0 {
            // Closed, or hung up on, before anything was asked.
            change(&|state| state.left_unused += 1);
            return;
        }
        if started.elapsed() > Duration::from_secs(5) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    };
    if head.starts_with("GET /v1/stream ") {
        return audio_stream(stream, state).await;
    }

    let mut asked = Vec::new();
    let mut buffer = [0u8; 4096];
    let (head, body) = loop {
        let Ok(read) = stream.read(&mut buffer).await else {
            return;
        };
        asked.extend_from_slice(&buffer[..read]);
        if let Some(end) = head_end(&asked) {
            let head = String::from_utf8_lossy(&asked[..end]).into_owned();
            let announced = head
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")?
                        .trim()
                        .parse()
                        .ok()
                })
                .unwrap_or(0usize);
            if asked.len() >= end + 4 + announced {
                break (head, asked[end + 4..].to_vec());
            }
        }
        if read == 0 {
            return;
        }
    };
    let Some((status, answer)) = route(&head, &body, &state, &base) else {
        // The answer is lost on its way.
        return;
    };
    let slow = state
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .slow_answers;
    tokio::time::sleep(slow).await;
    let reply = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{answer}",
        answer.len()
    );
    let _ = stream.write_all(reply.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// The bearer token a request came with.
fn bearer(head: &str) -> Option<&str> {
    head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("authorization")
            .then(|| value.trim().strip_prefix("Bearer "))
            .flatten()
    })
}

/// Answers one request of the account: a status and a body, or nothing
/// when the answer is lost.
fn route(head: &str, body: &[u8], state: &Shared, base: &str) -> Option<(u16, String)> {
    let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
    let request: String = head.split(' ').take(2).collect::<Vec<_>>().join(" ");
    state.asked.push(request.clone());
    if let Some(index) = state.faults.iter().position(|fault| fault.0 == request) {
        let (_, status, code) = state.faults.remove(index);
        return Some((status, error(code, "not now")));
    }
    let body: Value = serde_json::from_slice(body).unwrap_or_default();
    let authorized = bearer(head) == Some(state.access.as_str());
    let unauthorized = || (401, error("unauthorized", "no valid session"));
    let old = state.old_errors;
    Some(match request.as_str() {
        "POST /v1/auth/device" => {
            state.device_name = body["device_name"].clone();
            (
                200,
                json!({
                    "device_code": "device-1",
                    "verification_url": format!("{base}/v1/auth/google/start?code=u1"),
                    "confirm_code": "K7M2QX", "interval": 0, "expires_in": 600,
                })
                .to_string(),
            )
        }
        "GET /v1/plans" => (200, state.plans.to_string()),
        "POST /v1/auth/token" => match body["grant_type"].as_str() {
            Some("device_code") if state.device_traded || body["device_code"] != "device-1" => {
                unauthorized()
            }
            Some("device_code") if state.pending > 0 => {
                state.pending -= 1;
                let pending = "authorization_pending";
                (400, reasoned(old, pending, pending))
            }
            Some("device_code") => {
                state.device_traded = true;
                (200, state.grant().to_string())
            }
            // The token that is good, or the one before it, which is taken
            // again from an app that never got the answer.
            Some("refresh_token")
                if body["refresh_token"] == state.refresh.as_str()
                    || body["refresh_token"].as_str() == state.previous.as_deref() =>
            {
                let before = state.previous.clone();
                let again = body["refresh_token"].as_str() == before.as_deref();
                let granted = state.grant().to_string();
                if again {
                    state.previous = before;
                }
                if state.lost_answers > 0 {
                    state.lost_answers -= 1;
                    return None;
                }
                (200, granted)
            }
            _ => unauthorized(),
        },
        "POST /v1/auth/signout" => {
            if body["refresh_token"] == state.refresh.as_str() {
                state.refresh = "signed-out".to_owned();
                state.access = "signed-out".to_owned();
            }
            (204, String::new())
        }
        _ if !authorized => unauthorized(),
        "GET /v1/account" => (200, state.account.to_string()),
        "POST /v1/billing/checkout" if state.subscribed => (
            400,
            reasoned(old, "already_subscribed", "already subscribed"),
        ),
        "POST /v1/billing/checkout" => match (body["tier"].as_str(), body["plan"].as_str()) {
            (Some(tier @ ("pro" | "plus")), Some(plan @ ("monthly" | "yearly"))) => (
                200,
                json!({"url": format!("https://checkout.stripe.com/c/pay/{tier}-{plan}")})
                    .to_string(),
            ),
            _ => (400, error("bad_request", "no such plan")),
        },
        "POST /v1/billing/portal" if state.portal_url.is_empty() => {
            (400, reasoned(old, "nothing_to_manage", "no customer yet"))
        }
        "POST /v1/billing/portal" => (200, json!({"url": state.portal_url}).to_string()),
        _ => (404, error("not_found", "no such thing")),
    })
}

/// Lets a stream in, or refuses it the way its script says.
#[expect(
    clippy::result_large_err,
    reason = "the type the WebSocket handshake takes a refusal in"
)]
fn admit(
    script: Script,
    access: &str,
    request: &Request,
    response: Response,
) -> Result<Response, ErrorResponse> {
    let refuse = |status: u16, body: Option<String>| {
        let mut refusal = ErrorResponse::new(body);
        *refusal.status_mut() =
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        refusal
    };
    let token = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    match script {
        _ if token != Some(access) => {
            Err(refuse(401, Some(error("unauthorized", "no valid session"))))
        }
        Script::Refuse(status, code) => Err(refuse(status, Some(error(code, "refused")))),
        Script::RefuseBare(status) => Err(refuse(status, None)),
        _ => Ok(response),
    }
}

/// What a piece of audio is answered with; more than one message ends the
/// stream, and `None` drops it.
fn to_audio(script: Script) -> Option<Vec<Value>> {
    let so_far = json!({"type": "final", "text": "So far. "});
    Some(match script {
        Script::DropMidway => return None,
        Script::FailMidway(code) => vec![
            so_far,
            json!({"type": "error", "error": {"code": code, "message": "refused"}}),
        ],
        Script::SpentMidway(counted) => vec![
            so_far,
            json!({"type": "error", "seconds": f64::from(counted) / 1000.0,
                "error": {"code": "quota_exceeded", "message": "used up"}}),
        ],
        Script::DoneEarly(counted) => vec![
            so_far,
            json!({"type": "done", "seconds": f64::from(counted) / 1000.0}),
        ],
        Script::DoneAtOnce => vec![json!({"type": "done", "seconds": 0.0}), Value::Null],
        _ => vec![json!({"type": "partial", "text": "hello wor"})],
    })
}

#[expect(
    clippy::result_large_err,
    reason = "the type the WebSocket handshake takes a refusal in"
)]
async fn audio_stream(stream: TcpStream, state: Shared) {
    let (index, script) = {
        let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
        state.asked.push("GET /v1/stream".to_owned());
        state.streams.push(Seen::default());
        let index = state.streams.len() - 1;
        (
            index,
            state.scripts.get(index).copied().unwrap_or(Script::Usual),
        )
    };
    let note = |change: &dyn Fn(&mut Seen)| {
        let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(seen) = state.streams.get_mut(index) {
            change(seen);
        }
    };
    if let Script::SlowToOpen(wait) = script {
        tokio::time::sleep(wait).await;
    }
    let access = state
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .access
        .clone();
    let admit = |request: &Request, response: Response| admit(script, &access, request, response);
    let Ok(mut socket) = tokio_tungstenite::accept_hdr_async(stream, admit).await else {
        note(&|seen| seen.events.push("refused"));
        return;
    };
    let text = |value: Value| Message::text(value.to_string());
    if script == Script::Deaf {
        // Holds the connection and never reads from it again.
        note(&|seen| seen.events.push("deaf"));
        tokio::time::sleep(Duration::from_secs(30)).await;
        return;
    }
    while let Some(Ok(message)) = socket.next().await {
        let mut replies = Vec::new();
        let mut close = false;
        match message {
            Message::Text(said) => {
                let said: Value = serde_json::from_str(said.as_str()).unwrap_or_default();
                match said["type"].as_str() {
                    Some("start") => {
                        note(&|seen| {
                            seen.events.push("start");
                            seen.languages = said["languages"].clone();
                            seen.sample_rate = said["sample_rate"].clone();
                        });
                        replies.push(text(json!({"type": "ready"})));
                    }
                    Some("finish") => {
                        note(&|seen| seen.events.push("finish"));
                        replies.push(text(json!({"type": "final", "text": "Hello world."})));
                        replies.push(text(json!({"type": "done", "seconds": 1.5})));
                        close = true;
                    }
                    _ => {}
                }
            }
            Message::Binary(bytes) => {
                let samples: Vec<i16> = bytes
                    .chunks_exact(2)
                    .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
                    .collect();
                note(&|seen| {
                    seen.events.push("audio");
                    seen.audio.extend_from_slice(&samples);
                    seen.largest = seen.largest.max(bytes.len());
                    seen.messages += 1;
                });
                let Some(answers) = to_audio(script) else {
                    return;
                };
                close = answers.len() > 1;
                let said = answers.into_iter().filter(|answer| !answer.is_null());
                replies.extend(said.map(text));
            }
            _ => {}
        }
        for reply in replies {
            if socket.send(reply).await.is_err() {
                return;
            }
        }
        if close {
            // Closes the way a server does: what the client had on its way
            // is still read, so that it is not answered with a reset.
            let _ = socket.close(None).await;
            let rest = async { while socket.next().await.is_some() {} };
            let _ = tokio::time::timeout(Duration::from_secs(1), rest).await;
            break;
        }
    }
    note(&|seen| seen.events.push("closed"));
}
