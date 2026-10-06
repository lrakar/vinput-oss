//! Live transcription with `VinPUT` Pro, over a WebSocket to Vinput's own
//! server.
//!
//! Each dictation is one stream: a `start` message with the languages, then
//! the audio as it is captured, as raw 16-bit samples, and `finish` when the
//! key is let go. Guesses and settled text come back while the user speaks.
//! Audio queues while the socket is still opening, so nothing said right
//! after the key press is lost; what has queued by then goes out gathered
//! into a few large messages, and what follows is sent the moment it
//! arrives: small writes are not held back (`TCP_NODELAY`).
//!
//! # What a key press costs
//!
//! The stream is opened at the key press, on the network thread, while the
//! microphone starts on its own: nothing waits for it. Opening is three
//! round trips, and two things are kept ready to spare most of them:
//!
//! - An access token ([`crate::cloud::Account`]). It is fetched when the
//!   provider starts and traded for a new one after a dictation whenever it
//!   would run out within twenty minutes, so a press finds one in memory.
//!   Only a press after an hour without any dictation fetches one first,
//!   and the connection is made while it is fetched, not after.
//! - A connection. When a dictation ends, one connection to the server is
//!   made and kept for [`WARM_FOR`]: TCP and TLS are done, and nothing has
//!   been asked of the server yet, so the rule that a stream must start
//!   within ten seconds does not apply to it and it counts as no stream. The
//!   next press only has to ask for the WebSocket on it, one round trip.
//!   After [`WARM_FOR`] it is closed, and from then on nothing is connected
//!   and no timer runs: the next press connects for itself, resuming the
//!   TLS session of the one before. While it is kept the process is not
//!   woken: the task that holds it sleeps until it is taken, its time is up
//!   or the server closes it. One that died without notice is found out by
//!   the press that takes it, within [`WARM_ANSWER`], and replaced.
//!
//! No stream is opened ahead of a press: the server counts open streams per
//! account, closes a socket that has not sent `start` within ten seconds,
//! and ends a stream that hears nothing for half a minute.
//!
//! # When the server ends a stream
//!
//! The server says `done` when it has nothing more to send. It may say so
//! before `finish`, when it is restarted or the stream has lasted as long as
//! one may; that is an ordinary end, and the dictation carries on in a new
//! stream. `done` says how much audio was counted, and what was sent after
//! that is sent again on the new stream, so no word falls between the two. A
//! refusal ([`crate::cloud::Code`]) ends the dictation with words that say
//! what to do about it; text settled before it has been typed by then.
//!
//! # When the network stalls
//!
//! A write that the connection does not take within [`SEND_TIMEOUT`] ends
//! the dictation with a failure instead of letting the user speak into
//! nothing, and a dictation that is let go of is let go of here at once,
//! whatever was being waited for.

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::runtime::Handle;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::oneshot;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Request;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::{Bytes, Error as WsError, Message};

use super::live::{self, Client, Network, Socket, Stream};
use super::{Emit, ProviderKind, SessionOptions, SpeechProvider, Transcriber, Transcript};
use crate::audio::SAMPLE_RATE_HZ;
use crate::cloud::{self, Account, Code, Failure};
use crate::{Error, Result, log};

/// How long opening a stream may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the connection may take to accept one message before the
/// network is taken to have stalled.
const SEND_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a connection is kept ready after a dictation. Well under the two
/// minutes at which idle connections behind a home router start to be
/// dropped, so no ping is ever needed to keep it.
const WARM_FOR: Duration = Duration::from_secs(90);
/// How long a connection that was kept ready may take to answer the request
/// for a WebSocket before it is given up for dead and a new one is made. An
/// answer is one round trip away.
const WARM_ANSWER: Duration = Duration::from_secs(2);
/// How long a stream that can no longer be written to is read for what the
/// server said last.
const LAST_WORDS_WAIT: Duration = Duration::from_millis(500);
/// How long the account may be asked why a stream was refused.
const REFUSAL_WAIT: Duration = Duration::from_secs(5);
/// The most bytes of audio in one message; the server takes 64 KiB.
const MAX_MESSAGE: usize = 32 * 1024;
/// How many streams in a row the server may end without having settled or
/// counted anything before the dictation is given up. A stream that got
/// somewhere does not count: a dictation may last as long as it likes, in as
/// many streams of ten minutes as that takes.
const IDLE_STREAMS: u32 = 4;
/// Bytes of audio in a millisecond.
const BYTES_PER_MS: u64 = SAMPLE_RATE_HZ as u64 * 2 / 1000;
/// How much of the audio sent on a stream is kept in case the server ends
/// the stream without having counted it: half a minute, far more than is
/// ever on its way.
const KEEP_BYTES: usize = 30 * SAMPLE_RATE_HZ as usize * 2;

/// Where in Settings the account is, for the end of an error message.
const WHERE: &str = "Settings, VinPUT Pro";

const FINISH_MESSAGE: &str = r#"{"type":"finish"}"#;

/// The environment variable that has every dictation write its timings to
/// the log in a build that ships; a build for development always does.
const ENV_TIMING: &str = "VINPUT_TIMING";

/// The first message of a stream: what to listen for.
fn start_message(languages: &[String]) -> String {
    json!({
        "type": "start",
        "languages": hints(languages),
        "sample_rate": SAMPLE_RATE_HZ,
    })
    .to_string()
}

/// The languages as the server takes them: `sl` for `sl-SI`, each once, in
/// the order they were given.
fn hints(languages: &[String]) -> Vec<String> {
    let mut hints: Vec<String> = Vec::new();
    for language in languages {
        let primary = language
            .split(['-', '_'])
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if !primary.is_empty() && !hints.contains(&primary) {
            hints.push(primary);
        }
    }
    hints
}

/// How many bytes of audio `seconds` are, as the server counts them.
fn counted_bytes(seconds: f64) -> u64 {
    let millis = (seconds.clamp(0.0, 86_400.0) * 1000.0).round();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "between nothing and a day, as whole milliseconds"
    )]
    let millis = millis as u64;
    millis * BYTES_PER_MS
}

/// What one message from the server means for the dictation.
#[derive(Debug, PartialEq, Eq)]
enum Update {
    /// Words that may still change.
    Partial(String),
    /// Words that are settled.
    Final(String),
    /// The stream is complete, with this many bytes of its audio counted,
    /// if the server said.
    Done(Option<u64>),
    /// The server refused to go on.
    Failed(Failure),
    /// Nothing the dictation acts on: `ready`, or something newer.
    Other,
}

fn read_message(text: &str) -> Update {
    let Ok(message) = serde_json::from_str::<Value>(text) else {
        return Update::Other;
    };
    let said = || message["text"].as_str().unwrap_or_default().to_owned();
    match message["type"].as_str() {
        Some("partial") => Update::Partial(said()),
        Some("final") => Update::Final(said()),
        Some("done") => Update::Done(message["seconds"].as_f64().map(counted_bytes)),
        Some("error") => {
            let error = &message["error"];
            Update::Failed(Failure::refused(
                error["code"]
                    .as_str()
                    .and_then(Code::named)
                    .unwrap_or(Code::Internal),
                error["message"]
                    .as_str()
                    .unwrap_or("the server reported an error"),
            ))
        }
        _ => Update::Other,
    }
}

/// A failure in words for the person who dictated: what happened and, where
/// there is something to do about it, where.
fn explain(failure: &Failure) -> Error {
    let why = match failure {
        Failure::SignedOut => {
            format!("this PC is not signed in to VinPUT Pro; sign in under {WHERE}")
        }
        Failure::Refused {
            code: Code::Unauthorized,
            ..
        } => format!("this PC was signed out of VinPUT Pro; sign in again under {WHERE}"),
        Failure::Refused {
            code: Code::SubscriptionRequired,
            ..
        } => format!("VinPUT Pro needs a subscription that is paid for; see {WHERE}"),
        Failure::Refused {
            code: Code::QuotaExceeded,
            ..
        } => format!("this month's VinPUT Pro allowance is used up; see {WHERE}"),
        Failure::Refused {
            code: Code::TooManyRequests,
            ..
        } => "VinPUT Pro is busy with other dictations of this account; try again in a moment"
            .to_owned(),
        Failure::Refused { message, .. } => format!("VinPUT Pro could not transcribe: {message}"),
        Failure::Unreachable(why) => format!("VinPUT Pro could not be reached: {why}"),
        Failure::Expired | Failure::Stopped => "VinPUT Pro is not signed in".to_owned(),
    };
    Error::Transcription(why)
}

fn lost(doing: &str, err: &WsError) -> Error {
    Error::Transcription(format!("{doing} VinPUT Pro failed: {err}"))
}

fn ended_early() -> Error {
    Error::Transcription("the connection to VinPUT Pro was lost".into())
}

fn stalled() -> Error {
    Error::Transcription(
        "VinPUT Pro stopped taking the audio; check your internet connection".into(),
    )
}

/// The moments of a dictation that are measured, each from the key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    /// The WebSocket is open.
    Open,
    /// The first audio has been written to it.
    FirstAudio,
    /// The first guess has come back.
    FirstPartial,
    /// The key was let go: the audio has ended.
    Released,
    /// The last settled text has come back.
    Final,
    /// The last settled text that came after the key was let go has been
    /// typed.
    Typed,
}

const MARKS: usize = 6;

/// When what happened in one dictation, for finding out where its time
/// goes. Costs a clock reading at each mark and nothing else.
struct Timeline {
    pressed: Instant,
    /// Milliseconds from the press to each [`Mark`]; zero while it has not
    /// happened.
    at: [AtomicU32; MARKS],
    /// Whether the stream was opened on a connection that was kept ready.
    kept: AtomicBool,
}

impl Timeline {
    fn begin() -> Self {
        Self {
            pressed: Instant::now(),
            at: Default::default(),
            kept: AtomicBool::new(false),
        }
    }

    fn mark(&self, mark: Mark) {
        let now = u32::try_from(self.pressed.elapsed().as_millis())
            .unwrap_or(u32::MAX)
            .max(1);
        let at = &self.at[mark as usize];
        match mark {
            // The last of these counts, of the others the first.
            Mark::Final | Mark::Typed => at.store(now, Ordering::Relaxed),
            _ => {
                let _ = at.compare_exchange(0, now, Ordering::Relaxed, Ordering::Relaxed);
            }
        }
    }

    fn timings(&self) -> Timings {
        let at = |mark: Mark| {
            let millis = self.at[mark as usize].load(Ordering::Relaxed);
            (millis > 0).then(|| Duration::from_millis(u64::from(millis)))
        };
        Timings {
            kept_ready: self.kept.load(Ordering::Relaxed),
            socket_open: at(Mark::Open),
            first_audio: at(Mark::FirstAudio),
            first_partial: at(Mark::FirstPartial),
            released: at(Mark::Released),
            last_final: at(Mark::Final),
            typed: at(Mark::Typed),
        }
    }
}

/// When what happened in one dictation with `VinPUT` Pro, each counted from
/// the key press; `None` for what did not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Timings {
    /// Whether the stream was opened on a connection kept ready from the
    /// dictation before.
    pub kept_ready: bool,
    /// The WebSocket was open.
    pub socket_open: Option<Duration>,
    /// The first audio had been written to it.
    pub first_audio: Option<Duration>,
    /// The first guess had come back.
    pub first_partial: Option<Duration>,
    /// The key was let go.
    pub released: Option<Duration>,
    /// The last settled text had come back.
    pub last_final: Option<Duration>,
    /// The last settled text that came after the key was let go had been
    /// handed on to be typed, and typed.
    pub typed: Option<Duration>,
}

impl fmt::Display for Timings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let how = if self.kept_ready {
            "on a connection kept ready"
        } else {
            "on a new connection"
        };
        write!(f, "socket open {how}")?;
        let parts = [
            ("", self.socket_open),
            (", first audio sent", self.first_audio),
            (", first partial", self.first_partial),
            (", key released", self.released),
            (", final received", self.last_final),
            (", text typed", self.typed),
        ];
        for (what, at) in parts {
            match at {
                Some(at) => write!(f, "{what} {} ms", at.as_millis())?,
                None => write!(f, "{what} never")?,
            }
        }
        if let (Some(released), Some(typed)) = (self.released, self.typed) {
            let after = typed.saturating_sub(released).as_millis();
            write!(f, " ({after} ms after the key)")?;
        }
        Ok(())
    }
}

/// Whether every dictation writes its timings to the log.
fn timings_are_logged() -> bool {
    static LOGGED: OnceLock<bool> = OnceLock::new();
    *LOGGED.get_or_init(|| cfg!(debug_assertions) || std::env::var_os(ENV_TIMING).is_some())
}

/// The waits that decide when a stream is given up and how long a
/// connection is kept ready. Tests shorten them.
#[derive(Debug, Clone, Copy)]
struct Timing {
    connect: Duration,
    send: Duration,
    warm_for: Duration,
    warm_answer: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            connect: CONNECT_TIMEOUT,
            send: SEND_TIMEOUT,
            warm_for: WARM_FOR,
            warm_answer: WARM_ANSWER,
        }
    }
}

/// A connection that is being made or waits ready, for the next dictation.
struct Kept {
    id: u64,
    /// Asks the task that holds the connection to hand it over.
    take: oneshot::Sender<oneshot::Sender<Stream>>,
}

#[derive(Default)]
struct Ready {
    kept: Option<Kept>,
    last_id: u64,
}

/// What every stream of one provider shares.
struct Shared {
    account: Arc<Account>,
    tls: Arc<rustls::ClientConfig>,
    network: Handle,
    /// What every stream asks the server for, before the token is added.
    request: Request,
    timing: Timing,
    ready: Mutex<Ready>,
}

impl Shared {
    fn ready(&self) -> MutexGuard<'_, Ready> {
        self.ready.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Takes the connection `id` out of the offer, if it is still in it.
    fn let_go(&self, id: u64) {
        let mut ready = self.ready();
        if ready.kept.as_ref().is_some_and(|kept| kept.id == id) {
            ready.kept = None;
        }
    }

    /// The connection kept ready, on its way from the task that holds it,
    /// if there is one.
    fn take_ready(&self) -> Option<oneshot::Receiver<Stream>> {
        let kept = self.ready().kept.take()?;
        let (give, given) = oneshot::channel();
        kept.take.send(give).ok()?;
        Some(given)
    }
}

/// Has a connection made and kept for the next dictation, unless one is
/// there already. It is only ever called when a dictation ends, never from
/// a timer, so nothing here can turn into a loop.
fn keep_ready(shared: &Arc<Shared>) {
    let mut ready = shared.ready();
    if ready
        .kept
        .as_ref()
        .is_some_and(|kept| !kept.take.is_closed())
    {
        return;
    }
    let (take, taken) = oneshot::channel();
    ready.last_id += 1;
    let id = ready.last_id;
    ready.kept = Some(Kept { id, take });
    drop(ready);
    drop(shared.network.spawn(keep(Arc::clone(shared), id, taken)));
}

/// Makes a connection and holds it until a dictation takes it, its time is
/// up, or the server has closed it.
async fn keep(shared: Arc<Shared>, id: u64, mut taken: oneshot::Receiver<oneshot::Sender<Stream>>) {
    let dialing = live::dial(&shared.tls, &shared.request);
    let Ok(Ok(mut stream)) = timeout(shared.timing.connect, dialing).await else {
        // A dictation that was already waiting for it connects for itself.
        shared.let_go(id);
        return;
    };
    let mut byte = [0u8; 1];
    // Waiting costs nothing: the task sleeps until one of these happens.
    tokio::select! {
        wanted = &mut taken => {
            if let Ok(wanted_by) = wanted {
                let _ = wanted_by.send(stream);
            }
        }
        // Nothing was asked, so nothing is expected: whatever arrives, the
        // end of the connection as a rule, means it is of no more use.
        _ = stream.read(&mut byte) => shared.let_go(id),
        () = sleep(shared.timing.warm_for) => shared.let_go(id),
    }
}

/// `VinPUT` Pro, ready to open streams.
pub struct CloudSpeech {
    // Dropped first, which ends every task that holds `shared`.
    _network: Network,
    shared: Arc<Shared>,
}

impl fmt::Debug for CloudSpeech {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CloudSpeech").finish_non_exhaustive()
    }
}

impl CloudSpeech {
    /// Prepares the network runtime and TLS settings, and has an access
    /// token fetched in the background, for the first dictation to find.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transcription`] if nobody is signed in, or the
    /// runtime cannot be started.
    pub fn connect(account: Arc<Account>) -> Result<Self> {
        Self::with(account, Timing::default())
    }

    fn with(account: Arc<Account>, timing: Timing) -> Result<Self> {
        if account.signed_in_as().is_none() && !account.is_ready() {
            return Err(explain(&Failure::SignedOut));
        }
        let request = account
            .server()
            .stream_url()
            .into_client_request()
            .map_err(|err| lost("connecting to", &err))?;
        let network = Network::start()?;
        let shared = Arc::new(Shared {
            account,
            tls: live::tls()?,
            network: network.handle().clone(),
            request,
            timing,
            ready: Mutex::default(),
        });
        keep_fresh(&shared);
        Ok(Self {
            _network: network,
            shared,
        })
    }

    /// Starts a dictation and returns its end of the stream as what it is.
    fn session(&self, options: SessionOptions<'_>) -> CloudSession {
        let (stream, client) = live::session("VinPUT Pro stream", MAX_MESSAGE);
        let marks = Arc::new(Timeline::begin());
        let start = start_message(options.languages);
        let serving = serve(Arc::clone(&self.shared), start, client, Arc::clone(&marks));
        drop(self.shared.network.spawn(serving));
        CloudSession {
            stream,
            marks,
            logged: timings_are_logged(),
        }
    }
}

/// Has the access token traded for a new one if it runs out soon, off the
/// network thread, which must not wait for it.
fn keep_fresh(shared: &Arc<Shared>) {
    let account = Arc::clone(&shared.account);
    // A failure is reported by the dictation that needs the token.
    drop(shared.network.spawn_blocking(move || account.keep_fresh()));
}

impl SpeechProvider for CloudSpeech {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Cloud
    }

    fn start_session(&self, options: SessionOptions<'_>) -> Result<Box<dyn Transcriber + Send>> {
        Ok(Box::new(self.session(options)))
    }
}

/// An access token for a stream opened now. One in memory is taken without
/// leaving the network thread; fetching one is a request that waits.
async fn token(shared: &Arc<Shared>) -> Result<String> {
    let account = Arc::clone(&shared.account);
    let fetched = if account.is_ready() {
        account.access_token()
    } else {
        tokio::task::spawn_blocking(move || account.access_token())
            .await
            .map_err(live::failed("signing in to VinPUT Pro"))?
    };
    fetched.map_err(|failure| explain(&failure))
}

/// Why the server refused to open a stream. `429` stands for two things,
/// and a server in front of ours may answer without saying which; then the
/// account is asked.
async fn why_refused(shared: &Arc<Shared>, status: u16, body: Option<&[u8]>) -> Failure {
    let failure = cloud::refusal(status, body.unwrap_or_default());
    let said_which = body.is_some_and(|body| body.windows(4).any(|part| part == b"code"));
    if status != 429 || said_which {
        return failure;
    }
    let account = Arc::clone(&shared.account);
    let asking = tokio::task::spawn_blocking(move || account.info());
    let used_up = matches!(
        timeout(REFUSAL_WAIT, asking).await,
        Ok(Ok(Ok(info))) if info.over_allowance()
    );
    if used_up {
        Failure::refused(Code::QuotaExceeded, "the month's allowance is used up")
    } else {
        failure
    }
}

/// Why a stream was not opened.
enum Unopened {
    /// The server answered the request for it with this status and body.
    Refused(u16, Option<Vec<u8>>),
    /// It never came to that.
    Failed(Error),
}

/// A connection for a stream: the one kept ready, if there is one and it is
/// handed over, or a new one. Says which.
async fn connection(
    shared: &Arc<Shared>,
    given: Option<oneshot::Receiver<Stream>>,
) -> std::result::Result<(Stream, bool), Unopened> {
    if let Some(given) = given
        && let Ok(stream) = given.await
    {
        return Ok((stream, true));
    }
    match live::dial(&shared.tls, &shared.request).await {
        Ok(stream) => Ok((stream, false)),
        Err(err) => Err(Unopened::Failed(lost("connecting to", &err))),
    }
}

/// Opens a stream, within the time connecting may take. The connection is
/// made while the token is fetched, if one has to be. An access token the
/// server no longer takes is traded for a new one, once; a connection that
/// was kept ready and has died is replaced by a new one.
async fn open(shared: &Arc<Shared>, marks: &Timeline) -> std::result::Result<Socket, Unopened> {
    let opening = async {
        let mut given = shared.take_ready();
        let mut retried = false;
        loop {
            let token = async { token(shared).await.map_err(Unopened::Failed) };
            let (token, (stream, kept)) =
                tokio::try_join!(token, connection(shared, given.take()))?;
            let mut request = shared.request.clone();
            let bearer = HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(live::failed("signing in to VinPUT Pro"))
                .map_err(Unopened::Failed)?;
            request.headers_mut().insert(AUTHORIZATION, bearer);
            let upgrading = live::upgrade(request, stream);
            let upgraded = if kept {
                timeout(shared.timing.warm_answer, upgrading)
                    .await
                    .unwrap_or(Err(WsError::ConnectionClosed))
            } else {
                upgrading.await
            };
            match upgraded {
                Ok(socket) => {
                    marks.kept.store(kept, Ordering::Relaxed);
                    return Ok(socket);
                }
                Err(WsError::Http(response)) => {
                    let status = response.status().as_u16();
                    if status == 401 && !retried {
                        shared.account.refused(&token);
                        retried = true;
                        continue;
                    }
                    return Err(Unopened::Refused(status, response.body().clone()));
                }
                // It died while it waited: this press connects for itself.
                Err(_) if kept => {}
                Err(other) => return Err(Unopened::Failed(lost("connecting to", &other))),
            }
        }
    };
    let timed_out = || Error::Transcription("connecting to VinPUT Pro timed out".into());
    timeout(shared.timing.connect, opening)
        .await
        .unwrap_or_else(|_| Err(Unopened::Failed(timed_out())))
}

/// Opens a stream, or says in words why there is none. A refusal is looked
/// into only after the time for connecting, which it must not use up.
async fn opened(shared: &Arc<Shared>, marks: &Timeline) -> Result<Socket> {
    match open(shared, marks).await {
        Ok(socket) => Ok(socket),
        Err(Unopened::Failed(err)) => Err(err),
        Err(Unopened::Refused(status, body)) => {
            if status == 401 {
                // A new token was tried and refused as well: the account is
                // gone, or this PC was signed out of it elsewhere.
                shared.account.signed_out_elsewhere();
            }
            Err(explain(&why_refused(shared, status, body.as_deref()).await))
        }
    }
}

/// Writes one message, within the time the connection may take to accept it.
async fn write(shared: &Shared, socket: &mut Socket, message: Message, doing: &str) -> Result<()> {
    match timeout(shared.timing.send, socket.send(message)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => Err(lost(doing, &err)),
        Err(_) => Err(stalled()),
    }
}

/// The audio sent on one stream: how much of it, and the end of it, which
/// the server may not have counted when it ends the stream.
#[derive(Default)]
struct Sent {
    bytes: u64,
    tail: VecDeque<Bytes>,
    kept: usize,
}

impl Sent {
    fn note(&mut self, piece: &Bytes) {
        self.bytes += piece.len() as u64;
        self.kept += piece.len();
        self.tail.push_back(piece.clone());
        while self.kept > KEEP_BYTES && self.tail.len() > 1 {
            if let Some(old) = self.tail.pop_front() {
                self.kept -= old.len();
            }
        }
    }

    /// What of it lies past the first `counted` bytes, as far as it was
    /// kept: what the next stream is sent first.
    fn past(mut self, counted: u64) -> Vec<Bytes> {
        let uncounted = self.bytes.saturating_sub(counted);
        let mut left = usize::try_from(uncounted)
            .unwrap_or(usize::MAX)
            .min(self.kept);
        let mut pieces = Vec::new();
        while left > 0 {
            let Some(piece) = self.tail.pop_back() else {
                break;
            };
            let from = piece.len().saturating_sub(left);
            left -= piece.len() - from;
            pieces.push(piece.slice(from..));
        }
        pieces.reverse();
        pieces
    }
}

/// The dictation's end of things, as the task sees it.
struct Io {
    audio: UnboundedReceiver<Vec<u8>>,
    results: std::sync::mpsc::Sender<Result<Transcript>>,
    /// How many settled pieces of text were handed on.
    finals: u32,
}

/// How far a dictation has come, kept from one stream to the next.
struct Talk {
    /// Whether more audio may still come.
    audio_open: bool,
    /// Audio that was taken off the queue and did not fit the message
    /// being gathered.
    pending: Option<Vec<u8>>,
    /// Audio the stream before was sent and did not count.
    carry: Vec<Bytes>,
}

/// How a stream ended.
enum Ended {
    /// The server has sent everything there was, having counted this many
    /// bytes of the audio, if it said.
    Done(Option<u64>),
    /// The dictation has let go; nobody is left to hand anything to.
    Gone,
}

/// Why a stream cannot go on.
enum Trouble {
    /// The connection is gone, and nothing says why.
    Lost(Error),
    /// The server said why it will not go on.
    Refused(Error),
}

impl From<Trouble> for Error {
    fn from(trouble: Trouble) -> Self {
        match trouble {
            Trouble::Lost(err) | Trouble::Refused(err) => err,
        }
    }
}

/// Serves one dictation: its audio out, its transcripts back, in as many
/// streams as it takes.
async fn serve(shared: Arc<Shared>, start: String, client: Client, marks: Arc<Timeline>) {
    let Client {
        audio,
        results,
        mut gone,
    } = client;
    let mut io = Io {
        audio,
        results,
        finals: 0,
    };
    let mut socket = None;
    // Whatever is being waited for, a dictation that was let go of is not
    // served a moment longer.
    let outcome = tokio::select! {
        biased;
        _ = &mut gone => None,
        outcome = dictate(&shared, &start, &mut io, &mut socket, &marks) => Some(outcome),
    };
    if let Some(Err(err)) = outcome {
        let _ = io.results.send(Err(err));
    }
    // The dictation is told that it is over before the connection is closed.
    drop(io);
    let reached = socket.is_some();
    if let Some(socket) = socket {
        live::close(socket).await;
    }
    // A dictation has just ended: another is likely to follow, and this is
    // the moment at which nobody waits for a connection or a token.
    if reached {
        keep_ready(&shared);
    }
    keep_fresh(&shared);
}

async fn dictate(
    shared: &Arc<Shared>,
    start: &str,
    io: &mut Io,
    last: &mut Option<Socket>,
    marks: &Timeline,
) -> Result<()> {
    let mut talk = Talk {
        audio_open: true,
        pending: None,
        carry: Vec::new(),
    };
    let mut idle = 0;
    loop {
        let socket = last.insert(opened(shared, marks).await?);
        marks.mark(Mark::Open);
        let doing = "starting the stream to";
        write(shared, socket, Message::text(start), doing).await?;
        let (finals, mut sent) = (io.finals, Sent::default());
        let ended = converse(shared, socket, io, &mut talk, &mut sent, marks).await?;
        let counted = match ended {
            Ended::Gone => return Ok(()),
            Ended::Done(_) if !talk.audio_open => return Ok(()),
            Ended::Done(counted) => counted,
        };
        // The server ended the stream while audio was still coming: the
        // dictation carries on in a new one, which is first sent what this
        // one did not count.
        let progressed = io.finals > finals || counted.is_some_and(|counted| counted > 0);
        idle = if progressed { 0 } else { idle + 1 };
        if idle >= IDLE_STREAMS {
            return Err(Error::Transcription(
                "VinPUT Pro kept ending the dictation early".into(),
            ));
        }
        talk.carry = counted.map_or_else(Vec::new, |counted| sent.past(counted));
    }
}

/// What the server sent, or that the connection ended.
type Heard = Option<std::result::Result<Message, WsError>>;

/// Acts on what the server sent: hands a transcript on, or says how the
/// stream ended.
fn hear(
    heard: Heard,
    io: &mut Io,
    shared: &Shared,
    marks: &Timeline,
) -> std::result::Result<Option<Ended>, Trouble> {
    let update = match heard {
        Some(Ok(Message::Text(text))) => read_message(text.as_str()),
        Some(Ok(Message::Close(_))) | None => return Err(Trouble::Lost(ended_early())),
        Some(Ok(_)) => Update::Other,
        Some(Err(err)) => return Err(Trouble::Lost(lost("receiving from", &err))),
    };
    let (text, is_final) = match update {
        Update::Partial(text) => (text, false),
        Update::Final(text) => (text, true),
        Update::Done(counted) => return Ok(Some(Ended::Done(counted))),
        Update::Failed(failure) => {
            if matches!(
                failure,
                Failure::Refused {
                    code: Code::Unauthorized,
                    ..
                }
            ) {
                // The account was deleted, or this PC signed out of it
                // elsewhere, while the stream was open. The token is not
                // used again; the next request, which the host makes to
                // show why this failed, tries a new one and lets go of
                // the session if that is refused too.
                shared.account.distrust();
            }
            return Err(Trouble::Refused(explain(&failure)));
        }
        Update::Other => return Ok(None),
    };
    if is_final {
        marks.mark(Mark::Final);
        io.finals += 1;
    } else {
        marks.mark(Mark::FirstPartial);
    }
    let gone = io.results.send(Ok(Transcript { text, is_final })).is_err();
    Ok(gone.then_some(Ended::Gone))
}

/// A stream that could not be written to has usually been ended by the
/// server, which said why, and settled some text, just before. That is read
/// first; only if it said nothing is `failed` what happened.
async fn last_words(
    shared: &Shared,
    socket: &mut Socket,
    io: &mut Io,
    marks: &Timeline,
    failed: Error,
) -> Result<Ended> {
    while let Ok(heard) = timeout(LAST_WORDS_WAIT, socket.next()).await {
        match hear(heard, io, shared, marks) {
            Ok(None) => {}
            Ok(Some(ended)) => return Ok(ended),
            // The server's own refusal says more than the failed write.
            Err(Trouble::Refused(refused)) => return Err(refused),
            Err(Trouble::Lost(_)) => break,
        }
    }
    Err(failed)
}

/// The next audio to send: what is waiting, gathered into one message of
/// at most [`MAX_MESSAGE`] bytes, or `None` when the audio has ended. What
/// queued while the stream was opening goes out in a few messages this
/// way, and a piece that arrives by itself goes out by itself, at once.
async fn next_audio(talk: &mut Talk, audio: &mut UnboundedReceiver<Vec<u8>>) -> Option<Vec<u8>> {
    let mut bytes = match talk.pending.take() {
        Some(pending) => pending,
        None => audio.recv().await?,
    };
    while bytes.len() < MAX_MESSAGE {
        match audio.try_recv() {
            Ok(more) if bytes.len() + more.len() <= MAX_MESSAGE => bytes.extend_from_slice(&more),
            Ok(more) => {
                talk.pending = Some(more);
                break;
            }
            Err(_) => break,
        }
    }
    Some(bytes)
}

/// Carries a dictation on an open stream until the stream ends.
async fn converse(
    shared: &Shared,
    socket: &mut Socket,
    io: &mut Io,
    talk: &mut Talk,
    sent: &mut Sent,
    marks: &Timeline,
) -> Result<Ended> {
    for piece in std::mem::take(&mut talk.carry) {
        sent.note(&piece);
        if let Err(failed) = write(shared, socket, Message::Binary(piece), "sending audio to").await
        {
            return last_words(shared, socket, io, marks, failed).await;
        }
    }
    if !talk.audio_open {
        // The audio ended while this stream was being opened.
        write(
            shared,
            socket,
            Message::text(FINISH_MESSAGE),
            "ending the audio to",
        )
        .await?;
    }
    loop {
        // In this order: what the server has said is read before more is
        // written, so that its last words are not lost to a write that it
        // no longer takes.
        tokio::select! {
            biased;
            heard = socket.next() => {
                if let Some(ended) = hear(heard, io, shared, marks)? {
                    return Ok(ended);
                }
            }
            chunk = next_audio(talk, &mut io.audio), if talk.audio_open => {
                talk.audio_open = chunk.is_some();
                let (doing, message) = if let Some(bytes) = chunk {
                    let piece = Bytes::from(bytes);
                    sent.note(&piece);
                    ("sending audio to", Message::Binary(piece))
                } else {
                    marks.mark(Mark::Released);
                    ("ending the audio to", Message::text(FINISH_MESSAGE))
                };
                if let Err(failed) = write(shared, socket, message, doing).await {
                    return last_words(shared, socket, io, marks, failed).await;
                }
                marks.mark(Mark::FirstAudio);
            }
        }
    }
}

/// One dictation, and when what happened in it.
struct CloudSession {
    stream: live::Session,
    marks: Arc<Timeline>,
    /// Whether the timings go to the log when the dictation is over.
    logged: bool,
}

impl Transcriber for CloudSession {
    fn feed(&mut self, pcm: &[i16]) -> Result<Option<Transcript>> {
        self.stream.feed(pcm)
    }

    fn finish(&mut self, emit: &mut Emit<'_>) -> Result<()> {
        let marks = Arc::clone(&self.marks);
        marks.mark(Mark::Released);
        self.stream.finish(&mut |transcript| {
            emit(transcript)?;
            marks.mark(Mark::Typed);
            Ok(())
        })
    }
}

impl Drop for CloudSession {
    fn drop(&mut self) {
        if self.logged {
            log::write(&format!("VinPUT Pro timing: {}", self.marks.timings()));
        }
    }
}

/// Dictates `samples` to `VinPUT` Pro as a microphone would, ten
/// milliseconds at a time at the pace of speech, `rounds` times one after
/// the other, and says when what happened in each. `typed` is told every
/// settled piece of text as it comes back: in which round, how long after
/// that round's key press, and what it says.
///
/// This is for measuring where the time of a dictation goes without a
/// microphone, a key or a window: nothing is captured, shown, played or
/// typed anywhere. The second round and those after it find the connection
/// the round before left ready.
///
/// # Errors
///
/// Returns [`Error::Transcription`] if a dictation fails.
pub fn measure(
    account: Arc<Account>,
    samples: &[i16],
    languages: &[String],
    rounds: usize,
    typed: &mut dyn FnMut(usize, Duration, &str),
) -> Result<Vec<Timings>> {
    /// What a microphone hands on at a time: ten milliseconds.
    const PACKET: usize = SAMPLE_RATE_HZ as usize / 100;
    const PACKET_TIME: Duration = Duration::from_millis(10);
    /// The time between two dictations, in which the connection for the
    /// next one is made.
    const BETWEEN: Duration = Duration::from_millis(500);
    let speech = CloudSpeech::connect(account)?;
    let mut measured = Vec::new();
    for round in 0..rounds {
        if round > 0 {
            std::thread::sleep(BETWEEN);
        }
        let mut session = speech.session(SessionOptions { languages });
        let marks = Arc::clone(&session.marks);
        let mut due = marks.pressed;
        for packet in samples.chunks(PACKET) {
            if let Some(transcript) = session.feed(packet)?
                && transcript.is_final
            {
                typed(round, marks.pressed.elapsed(), &transcript.text);
            }
            due += PACKET_TIME;
            std::thread::sleep(due.saturating_duration_since(Instant::now()));
        }
        session.finish(&mut |transcript| {
            typed(round, marks.pressed.elapsed(), &transcript.text);
            Ok(())
        })?;
        measured.push(marks.timings());
    }
    Ok(measured)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::fake::{Fake, Memory, Script};

    type Outcome = std::result::Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn the_start_names_the_languages_and_the_rate() {
        let languages = ["sl-SI".to_owned(), "en-US".to_owned(), "en-GB".to_owned()];
        let start: Value = serde_json::from_str(&start_message(&languages)).unwrap_or_default();
        assert_eq!(
            start,
            json!({"type": "start", "languages": ["sl", "en"], "sample_rate": 16000})
        );
        let none: Value = serde_json::from_str(&start_message(&[])).unwrap_or_default();
        assert_eq!(none["languages"], json!([]));
        assert_eq!(hints(&[" EN_us ".to_owned(), String::new()]), ["en"]);
    }

    #[test]
    fn server_messages_are_understood() {
        assert_eq!(read_message(r#"{"type":"ready"}"#), Update::Other);
        assert_eq!(
            read_message(r#"{"type":"partial","text":"hel"}"#),
            Update::Partial("hel".into())
        );
        assert_eq!(
            read_message(r#"{"type":"final","text":"Hello. "}"#),
            Update::Final("Hello. ".into())
        );
        // What was counted is said in seconds and kept in bytes of audio.
        assert_eq!(
            read_message(r#"{"type":"done","seconds":12.4}"#),
            Update::Done(Some(12_400 * 32))
        );
        assert_eq!(read_message(r#"{"type":"done"}"#), Update::Done(None));
        assert_eq!(
            read_message(
                r#"{"type":"error","error":{"code":"quota_exceeded","message":"used up"}}"#
            ),
            Update::Failed(Failure::refused(Code::QuotaExceeded, "used up"))
        );
        assert!(matches!(
            read_message(r#"{"type":"error","error":{"code":"new"}}"#),
            Update::Failed(Failure::Refused {
                code: Code::Internal,
                ..
            })
        ));
        assert_eq!(read_message(r#"{"type":"later"}"#), Update::Other);
        assert_eq!(read_message("not json"), Update::Other);
    }

    #[test]
    fn what_the_server_counted_is_whole_samples_and_never_negative() {
        assert_eq!(counted_bytes(0.0), 0);
        assert_eq!(counted_bytes(0.005), 160);
        assert_eq!(counted_bytes(1.0), 32_000);
        assert_eq!(counted_bytes(-3.0), 0);
        assert_eq!(counted_bytes(f64::NAN), 0);
        assert_eq!(counted_bytes(1e12), 86_400_000 * 32);
    }

    #[test]
    fn a_refusal_says_what_to_do_about_it() {
        let refused = |code| explain(&Failure::refused(code, "x")).to_string();
        assert!(refused(Code::SubscriptionRequired).contains("needs a subscription"));
        assert!(refused(Code::QuotaExceeded).contains("allowance is used up"));
        assert!(refused(Code::Unauthorized).contains("was signed out of VinPUT Pro"));
        assert!(
            explain(&Failure::SignedOut)
                .to_string()
                .contains("not signed in")
        );
        // The host turns to the page of the account for these.
        for code in [
            Code::SubscriptionRequired,
            Code::QuotaExceeded,
            Code::Unauthorized,
        ] {
            assert!(refused(code).ends_with(WHERE));
        }
        assert!(refused(Code::TooManyRequests).contains("try again"));
        assert!(refused(Code::UpstreamFailed).ends_with(": x"));
    }

    fn pieces(sizes: &[usize]) -> Sent {
        let mut sent = Sent::default();
        let mut next = 0u8;
        for size in sizes {
            let piece: Vec<u8> = (0..*size)
                .map(|_| {
                    next = next.wrapping_add(1);
                    next
                })
                .collect();
            sent.note(&Bytes::from(piece));
        }
        sent
    }

    #[test]
    fn what_a_stream_did_not_count_is_what_the_next_one_is_sent_first() {
        let flat = |pieces: Vec<Bytes>| pieces.concat();
        // Everything counted: nothing to send again.
        assert!(pieces(&[4, 4, 4]).past(12).is_empty());
        assert!(pieces(&[4, 4, 4]).past(400).is_empty());
        // Nothing counted: all of it, in order.
        assert_eq!(
            flat(pieces(&[4, 4, 4]).past(0)),
            (1..=12).collect::<Vec<u8>>()
        );
        // Counted up to the middle of a piece.
        assert_eq!(flat(pieces(&[4, 4, 4]).past(6)), [7, 8, 9, 10, 11, 12]);
        assert_eq!(flat(pieces(&[4, 4, 4]).past(8)), [9, 10, 11, 12]);
        // Only the end of a long stream is kept; that much is sent again.
        let long = pieces(&[KEEP_BYTES, 4, 4]);
        assert_eq!(long.kept, 8);
        assert_eq!(long.past(0).concat().len(), 8);
    }

    #[test]
    fn the_timings_of_a_dictation_read_as_one_line() {
        let marks = Timeline::begin();
        assert_eq!(marks.timings(), Timings::default());
        marks.mark(Mark::Open);
        let opened = marks.timings().socket_open;
        assert!(opened.is_some());
        // The first time counts for what happens once.
        std::thread::sleep(Duration::from_millis(3));
        marks.mark(Mark::Open);
        assert_eq!(marks.timings().socket_open, opened);
        // And the last for what is typed.
        marks.mark(Mark::Final);
        let first = marks.timings().last_final;
        std::thread::sleep(Duration::from_millis(3));
        marks.mark(Mark::Final);
        assert!(marks.timings().last_final > first);

        let line = Timings {
            kept_ready: true,
            socket_open: Some(Duration::from_millis(40)),
            first_audio: Some(Duration::from_millis(41)),
            first_partial: None,
            released: Some(Duration::from_millis(2000)),
            last_final: Some(Duration::from_millis(2200)),
            typed: Some(Duration::from_millis(2201)),
        }
        .to_string();
        assert_eq!(
            line,
            "socket open on a connection kept ready 40 ms, first audio sent 41 ms, first \
             partial never, key released 2000 ms, final received 2200 ms, text typed 2201 ms \
             (201 ms after the key)"
        );
    }

    /// Waits short enough for a test.
    fn quick() -> Timing {
        Timing {
            warm_answer: Duration::from_millis(300),
            ..Timing::default()
        }
    }

    /// A provider at the stand-in, signed in.
    fn speech_with(fake: &Fake, timing: Timing) -> Result<(CloudSpeech, Memory)> {
        let (account, memory) = fake.account();
        memory.sign_in(fake);
        Ok((CloudSpeech::with(account, timing)?, memory))
    }

    fn speech(fake: &Fake) -> Result<(CloudSpeech, Memory)> {
        speech_with(fake, quick())
    }

    fn slovenian_and_english() -> Vec<String> {
        vec!["sl-SI".to_owned(), "en-US".to_owned()]
    }

    /// Feeds `chunks` chunks of audio, each sample holding the number of
    /// its chunk, and finishes. Returns what was typed and how it ended.
    fn dictate(speech: &CloudSpeech, chunks: i16) -> (String, Result<()>) {
        let languages = slovenian_and_english();
        let mut typed = String::new();
        let mut session = speech.session(SessionOptions {
            languages: &languages,
        });
        for chunk in 0..chunks {
            match session.feed(&[chunk; 160]) {
                Ok(Some(transcript)) if transcript.is_final => typed.push_str(&transcript.text),
                Ok(_) => {}
                Err(err) => return (typed, Err(err)),
            }
        }
        let finished = session.finish(&mut |transcript| {
            typed.push_str(&transcript.text);
            Ok(())
        });
        (typed, finished)
    }

    fn audio_of(chunks: std::ops::Range<i16>) -> Vec<i16> {
        chunks.flat_map(|chunk| [chunk; 160]).collect()
    }

    #[cfg(windows)]
    #[test]
    fn a_dictation_streams_its_audio_and_types_the_settled_text() -> Outcome {
        let fake = Fake::start()?;
        let (speech, _memory) = speech(&fake)?;
        let (typed, finished) = dictate(&speech, 3);
        assert!(finished.is_ok(), "{finished:?}");
        assert_eq!(typed, "Hello world.");
        assert!(fake.wait(|seen| seen[0].events.contains(&"closed")));
        let seen = fake.read(|state| state.streams.clone());
        assert_eq!(seen.len(), 1);
        let events = &seen[0].events;
        assert_eq!(events.first(), Some(&"start"));
        assert!(
            events.ends_with(&["audio", "finish", "closed"]),
            "{events:?}"
        );
        assert_eq!(seen[0].languages, json!(["sl", "en"]));
        assert_eq!(seen[0].sample_rate, json!(16000));
        assert_eq!(seen[0].audio, audio_of(0..3));
        // One token was fetched for it, when the provider started or at
        // the press, and none after: it is good for an hour.
        assert_eq!(fake.asked("POST /v1/auth/token"), 1);
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn guesses_come_back_while_speaking() -> Outcome {
        let fake = Fake::start()?;
        let (speech, _memory) = speech(&fake)?;
        let mut session = speech.start_session(SessionOptions { languages: &[] })?;
        let started = Instant::now();
        let mut guess = None;
        while guess.is_none() && started.elapsed() < Duration::from_secs(5) {
            guess = session.feed(&[0i16; 160])?;
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            guess,
            Some(Transcript {
                text: "hello wor".into(),
                is_final: false
            })
        );
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn audio_said_while_the_stream_opens_is_sent_once_it_has_in_a_few_messages() -> Outcome {
        let fake = Fake::start()?;
        fake.change(|state| state.scripts = vec![Script::SlowToOpen(Duration::from_millis(400))]);
        let (speech, _memory) = speech(&fake)?;
        let mut session = speech.start_session(SessionOptions { languages: &[] })?;
        // Starting a session and feeding it never wait for the stream: all
        // of this is queued long before the stand-in lets the stream in.
        for chunk in 0..50 {
            session.feed(&[chunk; 160])?;
        }
        // One piece of audio is larger than a message may be.
        session.feed(&vec![9i16; 40_000])?;
        assert!(fake.read(|state| state.streams.iter().all(|seen| seen.audio.is_empty())));
        let mut typed = String::new();
        session.finish(&mut |transcript| {
            typed.push_str(&transcript.text);
            Ok(())
        })?;
        assert_eq!(typed, "Hello world.");
        let seen = fake.read(|state| state.streams[0].clone());
        let mut expected = audio_of(0..50);
        expected.extend(vec![9i16; 40_000]);
        assert_eq!(seen.audio, expected);
        assert!(seen.largest <= MAX_MESSAGE, "{}", seen.largest);
        // Fifty packets and three pieces went out as four messages, not
        // fifty-three.
        assert_eq!(seen.messages, 4);
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn an_allowance_used_up_midway_types_what_was_settled_and_says_so() -> Outcome {
        let fake = Fake::start()?;
        fake.change(|state| state.scripts = vec![Script::FailMidway("quota_exceeded")]);
        let (speech, _memory) = speech(&fake)?;
        let (typed, finished) = dictate(&speech, 2);
        assert_eq!(typed, "So far. ");
        assert!(
            matches!(&finished, Err(Error::Transcription(why)) if why.contains("allowance is used up")),
            "{finished:?}"
        );
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn an_account_deleted_while_a_stream_is_open_ends_it_and_the_token_is_tried_anew() -> Outcome {
        let fake = Fake::start()?;
        fake.change(|state| state.scripts = vec![Script::FailMidway("unauthorized")]);
        let (speech, memory) = speech(&fake)?;
        let (typed, finished) = dictate(&speech, 2);
        assert_eq!(typed, "So far. ");
        assert!(
            matches!(&finished, Err(Error::Transcription(why)) if why.contains("was signed out of VinPUT Pro")),
            "{finished:?}"
        );
        // The token in memory is not trusted any more: the next request
        // trades for a new one. Here the session still stands, so it goes on.
        assert!(!speech.shared.account.is_ready() || fake.asked("POST /v1/auth/token") > 1);
        assert!(speech.shared.account.info().is_ok());
        assert!(memory.refresh_token().is_some());
        // Had the account been gone, that request would have signed this PC out.
        fake.change(|state| {
            state.refresh = "another".to_owned();
            state.previous = None;
        });
        speech.shared.account.distrust();
        assert_eq!(speech.shared.account.info(), Err(Failure::SignedOut));
        assert!(memory.refresh_token().is_none());
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn a_stream_refused_for_want_of_a_subscription_says_so() -> Outcome {
        let fake = Fake::start()?;
        fake.change(|state| {
            state.scripts = vec![
                Script::Refuse(402, "subscription_required"),
                Script::Refuse(429, "quota_exceeded"),
                Script::Refuse(429, "too_many_requests"),
            ];
        });
        let (speech, _memory) = speech(&fake)?;
        for expected in ["needs a subscription", "allowance is used up", "try again"] {
            let (typed, finished) = dictate(&speech, 2);
            assert_eq!(typed, "");
            assert!(
                matches!(&finished, Err(Error::Transcription(why)) if why.contains(expected)),
                "{expected}: {finished:?}"
            );
        }
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn a_refusal_that_does_not_say_why_is_looked_up_in_the_account() -> Outcome {
        let fake = Fake::start()?;
        fake.change(|state| {
            state.scripts = vec![Script::RefuseBare(429), Script::RefuseBare(429)];
        });
        let (speech, _memory) = speech(&fake)?;
        let (_, finished) = dictate(&speech, 1);
        assert!(
            matches!(&finished, Err(Error::Transcription(why)) if why.contains("try again")),
            "{finished:?}"
        );
        fake.change(|state| state.account["usage"]["seconds"] = json!(144_000));
        let (_, finished) = dictate(&speech, 1);
        assert!(
            matches!(&finished, Err(Error::Transcription(why)) if why.contains("allowance is used up")),
            "{finished:?}"
        );
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn a_dropped_connection_is_reported() -> Outcome {
        let fake = Fake::start()?;
        fake.change(|state| state.scripts = vec![Script::DropMidway]);
        let (speech, _memory) = speech(&fake)?;
        let (typed, finished) = dictate(&speech, 2);
        assert_eq!(typed, "");
        assert!(
            matches!(&finished, Err(Error::Transcription(why)) if why.contains("VinPUT Pro")),
            "{finished:?}"
        );
        // The next dictation is served as if nothing had happened.
        let (typed, finished) = dictate(&speech, 2);
        assert!(finished.is_ok(), "{finished:?}");
        assert_eq!(typed, "Hello world.");
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn a_stream_the_server_ends_early_is_followed_by_another() -> Outcome {
        let fake = Fake::start()?;
        fake.change(|state| state.scripts = vec![Script::DoneEarly(500)]);
        let (speech, _memory) = speech(&fake)?;
        let mut session = speech.start_session(SessionOptions { languages: &[] })?;
        let mut typed = String::new();
        session.feed(&[1i16; 160])?;
        // The first stream ends; the dictation goes on speaking.
        assert!(fake.wait(|seen| seen.len() == 2 && seen[1].events == ["start"]));
        for _ in 0..2 {
            if let Some(transcript) = session.feed(&[2i16; 160])?
                && transcript.is_final
            {
                typed.push_str(&transcript.text);
            }
        }
        session.finish(&mut |transcript| {
            typed.push_str(&transcript.text);
            Ok(())
        })?;
        assert_eq!(typed, "So far. Hello world.");
        // The first stream counted all it was sent: nothing is said twice.
        let seen = fake.read(|state| state.streams.clone());
        assert_eq!(seen[1].audio, [2i16; 320]);
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn what_a_stream_that_ended_early_did_not_count_is_sent_again() -> Outcome {
        let fake = Fake::start()?;
        // Of the ten milliseconds it is sent, the first stream counts four.
        fake.change(|state| state.scripts = vec![Script::DoneEarly(4)]);
        let (speech, _memory) = speech(&fake)?;
        let mut session = speech.start_session(SessionOptions { languages: &[] })?;
        let first: Vec<i16> = (0..160).collect();
        session.feed(&first)?;
        assert!(fake.wait(|seen| seen.len() == 2 && seen[1].events.contains(&"audio")));
        let mut typed = String::new();
        if let Some(transcript) = session.feed(&[7i16; 160])?
            && transcript.is_final
        {
            typed.push_str(&transcript.text);
        }
        session.finish(&mut |transcript| {
            typed.push_str(&transcript.text);
            Ok(())
        })?;
        assert_eq!(typed, "So far. Hello world.");
        let seen = fake.read(|state| state.streams.clone());
        assert_eq!(seen[0].audio, first);
        // Sixty-four samples were counted; the rest comes first, then what
        // was said after: every sample reaches a stream that counts it.
        let mut expected = first[64..].to_vec();
        expected.extend([7i16; 160]);
        assert_eq!(seen[1].audio, expected);
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn a_server_that_keeps_ending_streams_without_getting_anywhere_is_given_up() -> Outcome {
        let fake = Fake::start()?;
        fake.change(|state| state.scripts = vec![Script::DoneAtOnce; 8]);
        let (speech, _memory) = speech(&fake)?;
        let mut session = speech.start_session(SessionOptions { languages: &[] })?;
        session.feed(&[1i16; 160])?;
        // The key is still down while the server ends stream after stream.
        assert!(fake.wait(|seen| seen.len() == 4 && seen[3].events.contains(&"audio")));
        let finished = session.finish(&mut |_| Ok(()));
        assert!(
            matches!(&finished, Err(Error::Transcription(why)) if why.contains("kept ending")),
            "{finished:?}"
        );
        // Each of them was sent the audio the one before did not count.
        let seen = fake.read(|state| state.streams.clone());
        assert_eq!(seen.len(), 4);
        assert!(seen.iter().all(|stream| stream.audio == [1i16; 160]));
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn streams_that_get_somewhere_may_follow_one_another_without_end() -> Outcome {
        let fake = Fake::start()?;
        // Twice as many as a dictation may have of the other kind.
        fake.change(|state| state.scripts = vec![Script::DoneEarly(10); 8]);
        let (speech, _memory) = speech(&fake)?;
        let mut session = speech.start_session(SessionOptions { languages: &[] })?;
        let mut typed = String::new();
        for stream in 1..=8 {
            session.feed(&[1i16; 160])?;
            assert!(fake.wait(|seen| seen.len() == stream + 1));
        }
        session.finish(&mut |transcript| {
            typed.push_str(&transcript.text);
            Ok(())
        })?;
        assert!(typed.ends_with("Hello world."), "{typed}");
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn a_connection_that_stops_taking_audio_ends_the_dictation_soon() -> Outcome {
        let fake = Fake::start()?;
        fake.change(|state| state.scripts = vec![Script::Deaf]);
        let timing = Timing {
            send: Duration::from_millis(300),
            ..quick()
        };
        let (speech, _memory) = speech_with(&fake, timing)?;
        let mut session = speech.start_session(SessionOptions { languages: &[] })?;
        let started = Instant::now();
        // Far more than the connection holds without being read from.
        let much = vec![0i16; 1 << 20];
        let failed = loop {
            match session.feed(&much) {
                Err(err) => break Some(err),
                Ok(_) if started.elapsed() > Duration::from_secs(20) => break None,
                Ok(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        };
        assert!(
            matches!(&failed, Some(Error::Transcription(why)) if why.contains("stopped taking the audio")),
            "{failed:?}"
        );
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn a_dictation_that_is_let_go_of_is_not_waited_for() -> Outcome {
        let fake = Fake::start()?;
        fake.change(|state| state.scripts = vec![Script::SlowToOpen(Duration::from_secs(5))]);
        let (speech, _memory) = speech(&fake)?;
        let session = speech.start_session(SessionOptions { languages: &[] })?;
        assert!(fake.wait(|seen| seen.len() == 1));
        // Let go of while the stream is still being opened.
        drop(session);
        // The task ends at once: the next dictation is served, on a
        // connection of its own.
        let (typed, finished) = dictate(&speech, 1);
        assert!(finished.is_ok(), "{finished:?}");
        assert_eq!(typed, "Hello world.");
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn a_token_the_server_stopped_taking_is_traded_at_the_press() -> Outcome {
        let fake = Fake::start()?;
        let (speech, _memory) = speech(&fake)?;
        let (_, finished) = dictate(&speech, 1);
        assert!(finished.is_ok(), "{finished:?}");
        fake.change(|state| state.access = "withdrawn".to_owned());
        let (typed, finished) = dictate(&speech, 1);
        assert!(finished.is_ok(), "{finished:?}");
        assert_eq!(typed, "Hello world.");
        assert_eq!(fake.asked("POST /v1/auth/token"), 2);
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn nobody_signed_in_is_said_before_anything_is_connected() -> Outcome {
        let fake = Fake::start()?;
        let (account, memory) = fake.account();
        let refused = CloudSpeech::connect(Arc::clone(&account));
        assert!(
            matches!(&refused, Err(Error::Transcription(why)) if why.contains("sign in under Settings, VinPUT Pro")),
            "{refused:?}"
        );
        // Signed out while the provider runs: the dictation says so.
        memory.sign_in(&fake);
        let speech = CloudSpeech::connect(account)?;
        fake.change(|state| {
            state.refresh = "another".to_owned();
            state.previous = None;
            state.access = "another".to_owned();
        });
        let (_, finished) = dictate(&speech, 1);
        // The token fetched at the start may still be in memory; the
        // server refuses it, and the refresh token after it.
        assert!(
            matches!(&finished, Err(Error::Transcription(why)) if why.contains("not signed in")),
            "{finished:?}"
        );
        assert!(memory.refresh_token().is_none());
        assert!(fake.read(|state| state.streams.iter().all(|seen| seen.audio.is_empty())));
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn an_account_that_is_gone_signs_this_pc_out_and_says_so() -> Outcome {
        let fake = Fake::start()?;
        // The stream is refused whatever token it is asked with, though
        // the token itself can still be traded.
        fake.change(|state| {
            state.scripts = vec![
                Script::Refuse(401, "unauthorized"),
                Script::Refuse(401, "unauthorized"),
            ];
        });
        let (speech, memory) = speech(&fake)?;
        let (typed, finished) = dictate(&speech, 1);
        assert_eq!(typed, "");
        assert!(
            matches!(&finished, Err(Error::Transcription(why)) if why.contains("was signed out of VinPUT Pro")),
            "{finished:?}"
        );
        // One new token was tried, and then the session was let go of.
        assert_eq!(fake.asked("GET /v1/stream"), 2);
        assert!(memory.refresh_token().is_none());
        Ok(())
    }

    /// One dictation, and whether its stream was opened on a connection
    /// that was kept ready.
    fn dictate_noting(speech: &CloudSpeech) -> (String, Result<()>, bool) {
        let mut typed = String::new();
        let mut session = speech.session(SessionOptions { languages: &[] });
        let marks = Arc::clone(&session.marks);
        if let Err(err) = session.feed(&[3i16; 160]) {
            return (typed, Err(err), false);
        }
        let finished = session.finish(&mut |transcript| {
            typed.push_str(&transcript.text);
            Ok(())
        });
        (typed, finished, marks.timings().kept_ready)
    }

    #[cfg(windows)]
    #[test]
    fn the_next_dictation_takes_over_the_connection_the_one_before_left_ready() -> Outcome {
        let fake = Fake::start()?;
        let (speech, _memory) = speech(&fake)?;
        // Nothing is connected before the first press.
        assert_eq!(fake.read(|state| state.connections), 0);
        let (_, finished, kept) = dictate_noting(&speech);
        assert!(finished.is_ok() && !kept, "{finished:?}");
        // When it is over, one connection is made and nothing is asked on
        // it: the stand-in has seen it and no second stream.
        assert!(fake.wait_for(|state| state.connections == 2));
        assert_eq!(fake.asked("GET /v1/stream"), 1);
        let (typed, finished, kept) = dictate_noting(&speech);
        assert!(finished.is_ok(), "{finished:?}");
        assert_eq!(typed, "Hello world.");
        assert!(kept, "the stream was opened on a connection of its own");
        assert_eq!(fake.asked("GET /v1/stream"), 2);
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn a_connection_kept_ready_is_let_go_of_when_its_time_is_up() -> Outcome {
        let fake = Fake::start()?;
        let timing = Timing {
            warm_for: Duration::from_millis(150),
            ..quick()
        };
        let (speech, _memory) = speech_with(&fake, timing)?;
        let (_, finished, _) = dictate_noting(&speech);
        assert!(finished.is_ok(), "{finished:?}");
        assert!(fake.wait_for(|state| state.connections == 2));
        // It is closed without anything having been asked on it, and
        // nothing is kept or waited for after that.
        assert!(fake.wait_for(|state| state.left_unused == 1));
        assert!(speech.shared.ready().kept.is_none());
        // The next press connects for itself.
        let (typed, finished, kept) = dictate_noting(&speech);
        assert!(finished.is_ok() && !kept, "{finished:?}");
        assert_eq!(typed, "Hello world.");
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn a_connection_kept_ready_that_has_died_is_replaced_at_the_press() -> Outcome {
        let fake = Fake::start()?;
        // The second request for a stream, made on the connection that was
        // kept, is never answered.
        fake.change(|state| {
            state.scripts = vec![Script::Usual, Script::SlowToOpen(Duration::from_secs(8))];
        });
        let (speech, _memory) = speech(&fake)?;
        let (_, finished, _) = dictate_noting(&speech);
        assert!(finished.is_ok(), "{finished:?}");
        assert!(fake.wait_for(|state| state.connections == 2));
        let started = Instant::now();
        let (typed, finished, kept) = dictate_noting(&speech);
        assert!(finished.is_ok(), "{finished:?}");
        assert_eq!(typed, "Hello world.");
        assert!(!kept);
        // Found out within the wait for an answer, not after the eight
        // seconds the dead one would have taken.
        assert!(started.elapsed() < Duration::from_secs(4));
        // Nothing of the dictation was lost on the way.
        let seen = fake.read(|state| state.streams.clone());
        assert_eq!(seen[2].audio, [3i16; 160]);
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn a_connection_the_server_closes_while_it_is_kept_is_not_offered() -> Outcome {
        let fake = Fake::start()?;
        fake.change(|state| state.unused_for = Some(Duration::from_millis(100)));
        let (speech, _memory) = speech(&fake)?;
        let (_, finished, _) = dictate_noting(&speech);
        assert!(finished.is_ok(), "{finished:?}");
        // The stand-in hangs up on the connection nothing was asked on.
        assert!(fake.wait_for(|state| state.left_unused == 1));
        let gone = || speech.shared.ready().kept.is_none();
        let started = Instant::now();
        while !gone() && started.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(gone());
        let (typed, finished, kept) = dictate_noting(&speech);
        assert!(finished.is_ok() && !kept, "{finished:?}");
        assert_eq!(typed, "Hello world.");
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn a_file_is_dictated_at_the_pace_of_speech_and_timed_without_a_microphone() -> Outcome {
        let fake = Fake::start()?;
        let access = fake.read(|state| state.access.clone());
        // A token handed over, as for a measurement: nothing is kept and
        // nothing is traded.
        let server = cloud::Server::at(&fake.base).ok_or("the stand-in has an address")?;
        let account = Arc::new(Account::with_access_token(server, &access));
        // Three tenths of a second of audio.
        let samples = vec![5i16; 4800];
        let mut typed = Vec::new();
        let started = Instant::now();
        let measured = measure(account, &samples, &[], 2, &mut |round, at, text| {
            typed.push((round, at, text.to_owned()));
        })?;
        // Twice three tenths of a second, at the pace of speech, and the
        // pause between.
        assert!(started.elapsed() >= Duration::from_millis(1000));
        assert_eq!(fake.asked("POST /v1/auth/token"), 0);
        let [first, second] = measured.as_slice() else {
            unreachable!("two rounds were measured");
        };
        assert!(!first.kept_ready && second.kept_ready);
        for timings in [first, second] {
            let Timings {
                socket_open: Some(open),
                first_audio: Some(audio),
                first_partial: Some(partial),
                released: Some(released),
                last_final: Some(settled),
                typed: Some(typed),
                ..
            } = *timings
            else {
                unreachable!("every moment was measured: {timings}");
            };
            assert!(open <= audio && audio <= partial, "{timings}");
            assert!(released >= Duration::from_millis(290), "{timings}");
            assert!(released <= settled && settled <= typed, "{timings}");
        }
        assert_eq!(typed.len(), 2);
        assert!(typed.iter().all(|(_, _, text)| text == "Hello world."));
        assert_eq!((typed[0].0, typed[1].0), (0, 1));
        // All of the file reached the server, both times.
        let seen = fake.read(|state| state.streams.clone());
        assert!(seen.iter().all(|stream| stream.audio == samples));
        Ok(())
    }
}
