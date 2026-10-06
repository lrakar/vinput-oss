//! Live transcription with Google's Gemini API over a WebSocket.
//!
//! Each dictation is one WebSocket session: a setup message, then the audio as
//! it is captured, with interim and final transcripts coming back while the
//! user speaks. Audio queues while the socket is still opening, so nothing
//! said right after the key press is lost. The API key comes from the Windows
//! credential store and is only ever sent to Google.
//!
//! # A session prepared ahead
//!
//! Connecting (TCP, TLS, the WebSocket handshake, the setup message and its
//! answer) takes a tenth of a second on a fast line and several times that on
//! a slow one, so one session is connected and set up before it is needed
//! and handed to the next dictation. It is made
//! when the provider starts and when the audio of a dictation ends, which are
//! the moments a dictation is likely to follow, and it is let go after
//! [`WARM_FOR`]. After that nothing is connected and no timer runs: the next
//! press connects as it always did. A session is never used for two
//! dictations, because the service bills every turn for all the audio the
//! session has heard so far.
//!
//! What the service documents, and what the numbers here rest on:
//!
//! - A connection lives for about 10 minutes and a live transcription session
//!   streams for up to 10 minutes; before closing a connection the service
//!   sends `goAway` with the time that is left. No idle timeout and no
//!   keep-alive is documented, but the developer forum has reports of idle
//!   sessions dropped after two to three minutes without notice.
//!   <https://ai.google.dev/gemini-api/docs/live-session>,
//!   <https://ai.google.dev/gemini-api/docs/live-api/live-transcribe>,
//!   <https://ai.google.dev/api/live>
//! - Billing is by token (25 for a second of audio, plus the text that comes
//!   back), not by the time a session is open, so a session that is set up
//!   and hears nothing costs nothing.
//!   <https://ai.google.dev/gemini-api/docs/pricing>
//!
//! [`WARM_FOR`] is therefore well under the two minutes at which idle
//! sessions and idle connections behind a home router start to be dropped,
//! which also means no ping is ever needed to keep it: while it waits, the
//! process is not woken at all. A prepared session may still have died
//! unnoticed, so a dictation that takes one over asks it for a sign of life
//! at once and keeps its audio until the first transcript has come back on
//! that connection. If the connection turns out to be dead before that, the
//! dictation moves to a fresh one and sends the audio again: nothing is lost,
//! and nothing is typed twice, because nothing had been typed yet.

use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::runtime::Handle;
use tokio::sync::oneshot;
use tokio::time::{Instant, sleep_until, timeout};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;

use super::live::{self, Client, Network, Socket, close};
use super::{ProviderKind, SessionOptions, SpeechProvider, Transcriber, Transcript};
use crate::audio::SAMPLE_RATE_HZ;
use crate::settings::Settings;
use crate::{Error, Result, log, secrets};

/// Environment variable that supplies the API key when none is stored.
const ENV_API_KEY: &str = "GEMINI_API_KEY";

const ENDPOINT: &str = "wss://generativelanguage.googleapis.com/ws/\
     google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent";
/// How long connecting and setting up a session may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// After the audio ends: how long to wait for a transcript that has started.
const FINAL_WAIT: Duration = Duration::from_secs(6);
/// After the audio ends: how long the service may stay quiet before the
/// session is taken to be complete.
const QUIET_WAIT: Duration = Duration::from_millis(1200);
/// The same, right after a final transcript, when little more can follow.
const SETTLE_WAIT: Duration = Duration::from_millis(350);
// `finish` waits twelve seconds for the session to wind down
// (`stt/live.rs`). That is longer than [`ANSWER_WAIT`] and
// [`CONNECT_TIMEOUT`] together, so a session that has to connect afresh at
// the very end can still report what happened. No session spends longer than
// that getting connected: one that took over a prepared connection which
// then failed to open gets only what is left of one [`CONNECT_TIMEOUT`] for
// its own attempt.
/// How long a session prepared ahead is kept before it is let go. See the
/// module documentation for why it is this short.
const WARM_FOR: Duration = Duration::from_secs(90);
/// How long a prepared session that has just been taken over may take to
/// answer before it is given up for dead and a fresh one is connected. An
/// answer is one round trip away, a few tens of milliseconds.
const ANSWER_WAIT: Duration = Duration::from_secs(1);
/// After a session could not be prepared, or had died by the time it was
/// needed: how long before another is prepared, at first and at most. Each
/// failure in a row doubles the wait.
const RETRY_FIRST: Duration = Duration::from_secs(30);
const RETRY_MAX: Duration = Duration::from_secs(30 * 60);

/// Which Gemini model to use and the key to use it with.
#[derive(Clone, PartialEq, Eq)]
pub struct GeminiConfig {
    /// The API key.
    pub api_key: String,
    /// Model name, such as `gemini-3.5-transcribe-live`.
    pub model: String,
    /// WebSocket address of the service, without the key.
    pub endpoint: String,
    /// The languages the first dictation is expected to ask for, so a session
    /// can be prepared for it. Each dictation still names its own.
    pub languages: Vec<String>,
}

impl std::fmt::Debug for GeminiConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The key stays out of logs and error messages.
        f.debug_struct("GeminiConfig")
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl GeminiConfig {
    /// The configuration the settings and the stored key describe.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transcription`] if no API key is stored.
    pub fn from_settings(settings: &Settings) -> Result<Self> {
        let from_env = || {
            std::env::var(ENV_API_KEY)
                .ok()
                .filter(|key| !key.trim().is_empty())
        };
        let api_key = secrets::get(secrets::GEMINI_API_KEY)
            .or_else(from_env)
            .ok_or_else(|| {
                Error::Transcription(
                    "no Gemini API key is stored; set Google Gemini up under Settings, Models"
                        .into(),
                )
            })?;
        Ok(Self {
            api_key: api_key.trim().to_owned(),
            model: settings.model.clone(),
            endpoint: ENDPOINT.to_owned(),
            languages: settings.languages.clone(),
        })
    }

    fn url(&self) -> String {
        format!("{}?key={}", self.endpoint, self.api_key)
    }

    /// The first message of every session: the model and what to recognize.
    fn setup_message(&self, languages: &[String]) -> String {
        json!({
            "setup": {
                "model": format!("models/{}", self.model),
                "generationConfig": { "responseModalities": ["TEXT"] },
                "inputAudioTranscription": { "languageCodes": languages },
            }
        })
        .to_string()
    }

    /// Takes the key out of an error message, should a library have put the
    /// address in it.
    fn redact(&self, message: &str) -> String {
        message.replace(&self.api_key, "(key)")
    }

    /// The error for a step of the conversation that the connection failed.
    fn failed(&self, action: &str, err: &tokio_tungstenite::tungstenite::Error) -> Error {
        Error::Transcription(format!(
            "{action} failed: {}",
            self.redact(&err.to_string())
        ))
    }
}

/// One message of audio; `bytes` are 16-bit little-endian samples.
fn audio_message(bytes: &[u8]) -> String {
    json!({
        "realtimeInput": {
            "audio": {
                "data": BASE64.encode(bytes),
                "mimeType": format!("audio/pcm;rate={SAMPLE_RATE_HZ}"),
            }
        }
    })
    .to_string()
}

const AUDIO_END_MESSAGE: &str = r#"{"realtimeInput":{"audioStreamEnd":true}}"#;

/// What one message from the service means for the session.
#[derive(Debug, PartialEq, Eq)]
enum Update {
    /// The service accepted the setup.
    Ready,
    /// Words that may still change.
    Interim(String),
    /// Words that are settled.
    Final(String),
    /// The service considers the current stretch of speech complete.
    TurnComplete,
    /// The service is about to close the connection.
    GoingAway,
    /// The service reported a problem.
    Failed(String),
    /// Nothing the session acts on.
    Other,
}

fn read_message(bytes: &[u8]) -> Update {
    let Ok(message) = serde_json::from_slice::<Value>(bytes) else {
        return Update::Other;
    };
    if message.get("setupComplete").is_some() {
        return Update::Ready;
    }
    if let Some(error) = message.get("error") {
        let text = error.get("message").and_then(Value::as_str);
        return Update::Failed(text.unwrap_or("the service reported an error").to_owned());
    }
    if message.get("goAway").is_some() {
        return Update::GoingAway;
    }
    let Some(content) = message.get("serverContent") else {
        return Update::Other;
    };
    let text_of = |name: &str| {
        content
            .get(name)
            .and_then(|part| part.get("text"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    if let Some(text) = text_of("inputTranscription") {
        return Update::Final(text);
    }
    if let Some(text) = text_of("interimInputTranscription") {
        return Update::Interim(text);
    }
    if content.get("turnComplete").and_then(Value::as_bool) == Some(true) {
        return Update::TurnComplete;
    }
    Update::Other
}

/// The waits that decide when a session is prepared ahead and when it is
/// given up. Tests shorten them.
#[derive(Debug, Clone, Copy)]
struct Timing {
    connect: Duration,
    warm_for: Duration,
    answer_wait: Duration,
    retry_first: Duration,
    retry_max: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            connect: CONNECT_TIMEOUT,
            warm_for: WARM_FOR,
            answer_wait: ANSWER_WAIT,
            retry_first: RETRY_FIRST,
            retry_max: RETRY_MAX,
        }
    }
}

/// A session that is being connected or waits connected, for the next
/// dictation that asks for the same setup.
struct Prepared {
    id: u64,
    /// The setup message it was opened with: the model and the languages.
    setup: String,
    /// Hands the dictation to the task that holds the connection.
    adopt: oneshot::Sender<Client>,
}

/// When preparing a session may be tried again after it went wrong.
#[derive(Clone, Copy)]
struct Retry {
    not_before: Instant,
    wait: Duration,
}

#[derive(Default)]
struct Ahead {
    prepared: Option<Prepared>,
    last_id: u64,
    /// `Some` from a failure until a prepared session has served a dictation.
    retry: Option<Retry>,
}

/// What every session of one provider shares.
struct Shared {
    config: GeminiConfig,
    tls: Arc<rustls::ClientConfig>,
    network: Handle,
    timing: Timing,
    /// Where a failure that no dictation reports is written down.
    note: Box<dyn Fn(&str) + Send + Sync>,
    ahead: Mutex<Ahead>,
}

impl Shared {
    fn ahead(&self) -> MutexGuard<'_, Ahead> {
        self.ahead.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Starts connecting a session for the next dictation, unless one for the
    /// same setup is there already or preparing has just failed. It is only
    /// ever called when the provider starts and when the audio of a dictation
    /// ends, never from a timer, so nothing here can turn into a loop.
    fn prepare(self: &Arc<Self>, setup: &str) {
        let mut ahead = self.ahead();
        let have_it = |prepared: &Prepared| prepared.setup == setup && !prepared.adopt.is_closed();
        if ahead.prepared.as_ref().is_some_and(have_it)
            || ahead
                .retry
                .is_some_and(|retry| Instant::now() < retry.not_before)
        {
            return;
        }
        let (adopt, adopted) = oneshot::channel();
        ahead.last_id += 1;
        let id = ahead.last_id;
        // Replacing one for another setup drops its sender, which closes it.
        ahead.prepared = Some(Prepared {
            id,
            setup: setup.to_owned(),
            adopt,
        });
        drop(ahead);
        self.network.spawn(serve_prepared(
            Arc::clone(self),
            setup.to_owned(),
            id,
            adopted,
        ));
    }

    /// Takes the prepared session `id` out of the offer. A dictation may have
    /// taken it over at that very moment; it is returned, and is then owed a
    /// session.
    fn withdraw(&self, id: u64, adopted: &mut oneshot::Receiver<Client>) -> Option<Client> {
        adopted.close();
        let client = adopted.try_recv().ok();
        let mut ahead = self.ahead();
        if ahead
            .prepared
            .as_ref()
            .is_some_and(|prepared| prepared.id == id)
        {
            ahead.prepared = None;
        }
        client
    }

    /// Remembers that preparing went wrong, so the next one waits, and writes
    /// the reason down for the first failure in a row only.
    fn preparing_failed(&self, why: &str) {
        let mut ahead = self.ahead();
        let first = ahead.retry.is_none();
        let wait = ahead.retry.map_or(self.timing.retry_first, |retry| {
            (retry.wait * 2).min(self.timing.retry_max)
        });
        ahead.retry = Some(Retry {
            not_before: Instant::now() + wait,
            wait,
        });
        drop(ahead);
        if first {
            (self.note)(why);
        }
    }

    /// A prepared session served a dictation: preparing works.
    fn preparing_works(&self) {
        self.ahead().retry = None;
    }
}

/// The Gemini live transcription service, ready to open sessions.
pub struct GeminiSpeech {
    // Dropped first, which ends every task that holds `shared`.
    _network: Network,
    shared: Arc<Shared>,
}

impl std::fmt::Debug for GeminiSpeech {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeminiSpeech")
            .field("config", &self.shared.config)
            .finish_non_exhaustive()
    }
}

impl GeminiSpeech {
    /// Prepares the network runtime and TLS settings, and starts connecting a
    /// first session in the background for the languages in `config`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transcription`] if the runtime cannot be started.
    pub fn connect(config: GeminiConfig) -> Result<Self> {
        Self::with(config, Timing::default(), Box::new(log::write))
    }

    fn with(
        config: GeminiConfig,
        timing: Timing,
        note: Box<dyn Fn(&str) + Send + Sync>,
    ) -> Result<Self> {
        let network = Network::start()?;
        let setup = config.setup_message(&config.languages);
        let shared = Arc::new(Shared {
            config,
            tls: live::tls()?,
            network: network.handle().clone(),
            timing,
            note,
            ahead: Mutex::default(),
        });
        shared.prepare(&setup);
        Ok(Self {
            _network: network,
            shared,
        })
    }
}

impl SpeechProvider for GeminiSpeech {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gemini
    }

    fn start_session(&self, options: SessionOptions<'_>) -> Result<Box<dyn Transcriber + Send>> {
        // A session takes a chunk of audio as it is handed over, whole.
        let (session, client) = live::session("Gemini session", usize::MAX);
        let setup = self.shared.config.setup_message(options.languages);
        // A prepared session serves this dictation if it was set up for the
        // same model and languages. One for another setup is of no use to
        // anyone and is closed by dropping it here.
        let prepared = self.shared.ahead().prepared.take();
        let unserved = match prepared {
            Some(prepared) if prepared.setup == setup => prepared.adopt.send(client).err(),
            _ => Some(client),
        };
        if let Some(client) = unserved {
            let shared = Arc::clone(&self.shared);
            drop(self.shared.network.spawn(async move {
                let within = shared.timing.connect;
                serve(&shared, &setup, client, Progress::default(), within).await;
            }));
        }
        Ok(Box::new(session))
    }
}

/// Connects, sends the setup and waits until the service has accepted it,
/// all `within` the given time.
async fn open(shared: &Shared, setup: &str, within: Duration) -> Result<Socket> {
    let config = &shared.config;
    let opening = async {
        let unreached = |err| config.failed("connecting to Gemini", &err);
        let request = config.url().into_client_request().map_err(unreached)?;
        let stream = live::dial(&shared.tls, &request).await.map_err(unreached)?;
        let connecting = live::upgrade(request, stream);
        let mut socket = connecting.await.map_err(|err| match err {
            // The usual reason the handshake is refused.
            tokio_tungstenite::tungstenite::Error::Http(response) => Error::Transcription(format!(
                "Gemini refused the connection (HTTP {}); check the API key under Settings, Models, Change",
                response.status().as_u16()
            )),
            other => config.failed("connecting to Gemini", &other),
        })?;
        socket
            .send(Message::text(setup))
            .await
            .map_err(|err| config.failed("starting the Gemini session", &err))?;
        loop {
            let update = match socket.next().await {
                Some(Ok(Message::Text(text))) => read_message(text.as_bytes()),
                Some(Ok(Message::Binary(bytes))) => read_message(&bytes),
                Some(Ok(Message::Close(frame))) => return Err(closed_early(frame)),
                Some(Ok(_)) => Update::Other,
                Some(Err(err)) => return Err(config.failed("receiving from Gemini", &err)),
                None => return Err(closed_early(None)),
            };
            match update {
                Update::Ready => return Ok(socket),
                Update::Failed(reason) => {
                    return Err(Error::Transcription(format!("Gemini: {reason}")));
                }
                _ => {}
            }
        }
    };
    timeout(within, opening)
        .await
        .map_err(|_| Error::Transcription("connecting to Gemini timed out".into()))?
}

fn closed_early(frame: Option<CloseFrame>) -> Error {
    let reason = frame.map(|frame| frame.reason.to_string());
    Error::Transcription(format!(
        "Gemini closed the session early: {}",
        match reason.as_deref() {
            None | Some("") => "no reason given",
            Some(reason) => reason,
        }
    ))
}

/// How far a dictation has come, kept across a change of connection.
struct Progress {
    /// The audio sent on a prepared connection before the first transcript
    /// came back on it, to be sent again on a fresh one if none ever does.
    kept: Vec<Vec<u8>>,
    /// Whether more audio may still come.
    audio_open: bool,
    /// Whether the session for the next dictation has been asked for.
    next_prepared: bool,
}

impl Default for Progress {
    fn default() -> Self {
        Self {
            kept: Vec::new(),
            audio_open: true,
            next_prepared: false,
        }
    }
}

/// How a conversation on one connection ended.
enum Ended {
    /// Every transcript there was to get has been handed on.
    Complete,
    /// A prepared connection turned out to be dead. Nothing was handed on
    /// from it, and the audio sent on it is still in [`Progress::kept`].
    Stale,
}

/// Serves one dictation on a connection of its own, made now, which may take
/// `within` to open.
async fn serve(
    shared: &Arc<Shared>,
    setup: &str,
    mut client: Client,
    mut progress: Progress,
    within: Duration,
) {
    let socket = match open(shared, setup, within).await {
        Ok(mut socket) => {
            let ended = converse(
                shared,
                setup,
                &mut socket,
                &mut client,
                &mut progress,
                false,
            );
            if let Err(err) = ended.await {
                let _ = client.results.send(Err(err));
            }
            socket
        }
        Err(err) => {
            let _ = client.results.send(Err(err));
            return;
        }
    };
    // The dictation is told that it is over before the connection is closed.
    drop(client);
    close(socket).await;
}

/// Connects a session ahead of time, keeps it until a dictation takes it over
/// or its time is up, and serves that dictation.
async fn serve_prepared(
    shared: Arc<Shared>,
    setup: String,
    id: u64,
    mut adopted: oneshot::Receiver<Client>,
) {
    let started = Instant::now();
    let let_go = started + shared.timing.warm_for;
    let mut socket = match open(&shared, &setup, shared.timing.connect).await {
        Ok(socket) => socket,
        Err(err) => {
            shared.preparing_failed(&format!(
                "no Gemini session could be prepared ahead of the next dictation: {err}"
            ));
            // A dictation that took this session over while it was still
            // connecting gets an attempt of its own, in what is left of the
            // time one connection may take: after a refusal that is nearly
            // all of it, after a timeout nothing, and the dictation is told
            // why at once instead of waiting through a second timeout.
            if let Some(client) = shared.withdraw(id, &mut adopted) {
                let left = shared.timing.connect.saturating_sub(started.elapsed());
                if left.is_zero() {
                    let _ = client.results.send(Err(err));
                } else {
                    serve(&shared, &setup, client, Progress::default(), left).await;
                }
            }
            return;
        }
    };
    // Waiting costs nothing: the task sleeps until the service says
    // something, a dictation arrives or the time is up.
    let mut alive = true;
    let client = loop {
        tokio::select! {
            client = &mut adopted => match client {
                Ok(client) => break Some(client),
                // Dropped for a dictation in other languages, or with the provider.
                Err(_) => break None,
            },
            message = socket.next() => {
                let update = match message {
                    Some(Ok(Message::Text(text))) => read_message(text.as_bytes()),
                    Some(Ok(Message::Binary(bytes))) => read_message(&bytes),
                    Some(Ok(Message::Close(_)) | Err(_)) | None => Update::GoingAway,
                    Some(Ok(_)) => Update::Other,
                };
                if matches!(update, Update::GoingAway | Update::Failed(_)) {
                    alive = false;
                    break shared.withdraw(id, &mut adopted);
                }
            }
            () = sleep_until(let_go) => break shared.withdraw(id, &mut adopted),
        }
    };
    let Some(mut client) = client else {
        close(socket).await;
        return;
    };
    let mut progress = Progress::default();
    if alive {
        let ended = converse(
            &shared,
            &setup,
            &mut socket,
            &mut client,
            &mut progress,
            true,
        );
        match ended.await {
            Ok(Ended::Complete) => {
                drop(client);
                close(socket).await;
                return;
            }
            Err(err) => {
                let _ = client.results.send(Err(err));
                return;
            }
            Ok(Ended::Stale) => {}
        }
    }
    // The connection died while it waited. Nothing of this dictation has been
    // handed on yet, so it starts over on a fresh one with all of its audio.
    drop(socket);
    shared.preparing_failed(
        "the Gemini session prepared ahead had died; the dictation connected afresh",
    );
    serve(&shared, &setup, client, progress, shared.timing.connect).await;
}

/// Carries a dictation on an open session: audio out, transcripts back, until
/// the service has nothing more to say.
///
/// A connection that was `prepared` ahead may have died while it waited. It
/// is asked for a sign of life at once, and the audio sent on it is kept
/// until the first transcript has come back, so that [`Ended::Stale`] loses
/// nothing and repeats nothing.
async fn converse(
    shared: &Arc<Shared>,
    setup: &str,
    socket: &mut Socket,
    client: &mut Client,
    progress: &mut Progress,
    prepared: bool,
) -> Result<Ended> {
    let mut talk = Conversation {
        shared,
        setup,
        progress,
        answered: !prepared,
        proven: !prepared,
        answer_by: Instant::now() + shared.timing.answer_wait,
        deadline: None,
        interim_pending: false,
    };
    if let Some(ended) = talk.begin(socket, prepared).await? {
        return Ok(ended);
    }
    loop {
        talk.prepare_the_next();
        tokio::select! {
            // The dictation has let go; nobody is left to hand anything to.
            _ = &mut client.gone => return Ok(Ended::Complete),
            chunk = client.audio.recv(), if talk.progress.audio_open => {
                if let Some(ended) = talk.send_audio(socket, chunk).await? {
                    return Ok(ended);
                }
            }
            message = socket.next() => {
                if let Some(ended) = talk.hear(message, &client.results)? {
                    return Ok(ended);
                }
            }
            () = wait_until(talk.wake()) => {
                return Ok(if talk.answered { Ended::Complete } else { Ended::Stale });
            }
        }
    }
}

/// The state of one dictation on one connection.
struct Conversation<'a> {
    shared: &'a Arc<Shared>,
    setup: &'a str,
    progress: &'a mut Progress,
    /// Whether the connection has said anything since the dictation began on
    /// it. A prepared one that stays silent past `answer_by` is dead.
    answered: bool,
    /// Whether a transcript has come back on it. Until then a prepared
    /// connection that fails has only shown that it was dead, and the
    /// dictation moves to a fresh one.
    proven: bool,
    answer_by: Instant,
    /// After the audio has ended, the session is over at this moment unless
    /// the service says more first.
    deadline: Option<Instant>,
    interim_pending: bool,
}

impl Conversation<'_> {
    /// What a failure of the connection means.
    fn broken(
        &self,
        action: &str,
        err: &tokio_tungstenite::tungstenite::Error,
    ) -> Result<Option<Ended>> {
        if self.proven {
            Err(self.shared.config.failed(action, err))
        } else {
            Ok(Some(Ended::Stale))
        }
    }

    /// Asks a prepared connection for a sign of life; gives a fresh one the
    /// audio that a dead one was sent.
    async fn begin(&mut self, socket: &mut Socket, prepared: bool) -> Result<Option<Ended>> {
        if prepared {
            let ping = Message::Ping(Vec::<u8>::new().into());
            return match socket.send(ping).await {
                Ok(()) => Ok(None),
                Err(err) => self.broken("reaching Gemini", &err),
            };
        }
        for pcm in std::mem::take(&mut self.progress.kept) {
            if let Err(err) = socket.send(Message::text(audio_message(&pcm))).await {
                return self.broken("sending audio to Gemini", &err);
            }
        }
        if self.progress.audio_open {
            Ok(None)
        } else {
            self.end_audio(socket).await
        }
    }

    /// The next dictation is likely to follow this one, so a session for it
    /// is prepared as soon as this one has all its audio and the service is
    /// known to be reachable.
    fn prepare_the_next(&mut self) {
        if !self.progress.audio_open && self.answered && !self.progress.next_prepared {
            self.progress.next_prepared = true;
            self.shared.prepare(self.setup);
        }
    }

    /// Sends a chunk of audio, or the end of it when there is no more.
    async fn send_audio(
        &mut self,
        socket: &mut Socket,
        chunk: Option<Vec<u8>>,
    ) -> Result<Option<Ended>> {
        let Some(pcm) = chunk else {
            self.progress.audio_open = false;
            return self.end_audio(socket).await;
        };
        let sent = socket.send(Message::text(audio_message(&pcm))).await;
        if !self.proven {
            self.progress.kept.push(pcm);
        }
        match sent {
            Ok(()) => Ok(None),
            Err(err) => self.broken("sending audio to Gemini", &err),
        }
    }

    async fn end_audio(&mut self, socket: &mut Socket) -> Result<Option<Ended>> {
        if let Err(err) = socket.send(Message::text(AUDIO_END_MESSAGE)).await {
            return self.broken("ending the audio", &err);
        }
        let wait = if self.interim_pending {
            FINAL_WAIT
        } else {
            QUIET_WAIT
        };
        self.deadline = Some(Instant::now() + wait);
        Ok(None)
    }

    /// When to stop waiting for the service.
    fn wake(&self) -> Option<Instant> {
        if self.answered {
            self.deadline
        } else {
            Some(
                self.deadline
                    .map_or(self.answer_by, |deadline| deadline.min(self.answer_by)),
            )
        }
    }

    /// Acts on what the service sent and hands a transcript on.
    fn hear(
        &mut self,
        message: Option<std::result::Result<Message, tokio_tungstenite::tungstenite::Error>>,
        results: &std_mpsc::Sender<Result<Transcript>>,
    ) -> Result<Option<Ended>> {
        let update = match message {
            Some(Ok(Message::Text(text))) => read_message(text.as_bytes()),
            Some(Ok(Message::Binary(bytes))) => read_message(&bytes),
            Some(Ok(Message::Close(frame))) => {
                return if !self.proven {
                    Ok(Some(Ended::Stale))
                } else if self.progress.audio_open || self.interim_pending {
                    Err(closed_early(frame))
                } else {
                    Ok(Some(Ended::Complete))
                };
            }
            // The answer to the ping, as a rule.
            Some(Ok(_)) => Update::Other,
            Some(Err(err)) => return self.broken("receiving from Gemini", &err),
            None if self.proven => return Ok(Some(Ended::Complete)),
            None => return Ok(Some(Ended::Stale)),
        };
        self.answered = true;
        let transcript = match update {
            Update::Interim(text) => {
                self.interim_pending = true;
                if !self.progress.audio_open {
                    self.deadline = Some(Instant::now() + FINAL_WAIT);
                }
                Transcript {
                    text,
                    is_final: false,
                }
            }
            Update::Final(text) => {
                self.interim_pending = false;
                if !self.progress.audio_open {
                    self.deadline = Some(Instant::now() + SETTLE_WAIT);
                }
                Transcript {
                    text: spaced(text),
                    is_final: true,
                }
            }
            Update::TurnComplete if !self.progress.audio_open && !self.interim_pending => {
                return Ok(Some(Ended::Complete));
            }
            Update::GoingAway | Update::Failed(_) if !self.proven => {
                return Ok(Some(Ended::Stale));
            }
            Update::Failed(reason) => {
                return Err(Error::Transcription(format!("Gemini: {reason}")));
            }
            Update::Ready | Update::TurnComplete | Update::GoingAway | Update::Other => {
                return Ok(None);
            }
        };
        if !self.proven {
            // The service has heard this dictation on this connection, so
            // from here on it stays here, and preparing is known to work.
            self.proven = true;
            self.progress.kept = Vec::new();
            self.shared.preparing_works();
        }
        Ok(results
            .send(Ok(transcript))
            .is_err()
            .then_some(Ended::Complete))
    }
}

/// Sleeps until `deadline`, or forever if there is none.
async fn wait_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// Ends a stretch of speech with a space, so the next one does not run into it.
fn spaced(mut text: String) -> String {
    if !text.is_empty() && !text.ends_with(char::is_whitespace) {
        text.push(' ');
    }
    text
}

#[cfg(test)]
mod tests {
    use std::time::Instant as Clock;

    use tokio::net::{TcpListener, TcpStream};
    use tokio::runtime::Runtime;

    use super::*;

    type Outcome = std::result::Result<(), Box<dyn std::error::Error>>;

    fn config(endpoint: &str) -> GeminiConfig {
        GeminiConfig {
            api_key: "secret-key".into(),
            model: "gemini-3.5-transcribe-live".into(),
            endpoint: endpoint.into(),
            languages: vec!["en-US".into()],
        }
    }

    #[test]
    fn the_setup_names_the_model_and_languages() {
        let setup: Value =
            serde_json::from_str(&config("wss://x").setup_message(&["sl-SI".to_owned()]))
                .unwrap_or_default();
        assert_eq!(setup["setup"]["model"], "models/gemini-3.5-transcribe-live");
        assert_eq!(
            setup["setup"]["inputAudioTranscription"]["languageCodes"],
            json!(["sl-SI"])
        );
        assert_eq!(
            setup["setup"]["generationConfig"]["responseModalities"],
            json!(["TEXT"])
        );
    }

    #[test]
    fn audio_is_sent_as_base64_pcm_at_the_pipeline_rate() {
        let bytes = live::bytes_of(&[1, -2]);
        let message: Value = serde_json::from_str(&audio_message(&bytes)).unwrap_or_default();
        let audio = &message["realtimeInput"]["audio"];
        assert_eq!(audio["mimeType"], "audio/pcm;rate=16000");
        assert_eq!(audio["data"], "AQD+/w==");
    }

    #[test]
    fn server_messages_are_understood() {
        let read = |text: &str| read_message(text.as_bytes());
        assert_eq!(read(r#"{"setupComplete":{}}"#), Update::Ready);
        assert_eq!(
            read(r#"{"serverContent":{"interimInputTranscription":{"text":"hel"}}}"#),
            Update::Interim("hel".into())
        );
        assert_eq!(
            read(r#"{"serverContent":{"inputTranscription":{"text":"hello"}}}"#),
            Update::Final("hello".into())
        );
        assert_eq!(
            read(r#"{"serverContent":{"turnComplete":true}}"#),
            Update::TurnComplete
        );
        assert_eq!(
            read(r#"{"error":{"message":"quota"}}"#),
            Update::Failed("quota".into())
        );
        assert_eq!(read(r#"{"goAway":{"timeLeft":"5s"}}"#), Update::GoingAway);
        assert_eq!(read("not json"), Update::Other);
    }

    #[test]
    fn the_key_never_shows_in_debug_output_or_errors() {
        let config = config("wss://x");
        assert!(!format!("{config:?}").contains("secret-key"));
        assert_eq!(
            config.redact("bad url wss://x?key=secret-key"),
            "bad url wss://x?key=(key)"
        );
    }

    /// How the stand-in for the service treats one connection.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Script {
        /// Accepts the setup, answers each audio message with an interim
        /// transcript and the end of the audio with the final one, and then
        /// keeps the connection open until the client closes it.
        Usual,
        /// Drops the connection before the WebSocket handshake.
        Refuse,
        /// Accepts the setup and then reads nothing more, like a connection
        /// that died without a word.
        Deaf,
        /// Accepts the setup and closes when the first audio arrives.
        CloseOnAudio,
        /// Accepts the setup and closes straight after.
        HangUp,
        /// Like `Usual`, but takes `SLOW_SETUP` to accept the setup, as a
        /// service a slow line away does.
        SlowSetup,
        /// Takes the setup and never answers it, like a network that
        /// swallows everything.
        Mute,
        /// Like `Usual`, but the second audio message is answered with a
        /// final transcript.
        FinalWhileSpeaking,
        /// Like `Usual`, but the final transcript at the end is followed by
        /// an interim one, as if more were coming.
        MoreAfterFinal,
    }

    const SLOW_SETUP: Duration = Duration::from_millis(800);

    /// What the stand-in saw on one connection.
    #[derive(Debug, Clone, Default)]
    struct Seen {
        events: Vec<&'static str>,
        languages: Value,
        audio: Vec<i16>,
        closed: bool,
    }

    /// A stand-in for the service that accepts any number of sessions and
    /// remembers what each was sent.
    struct Fake {
        // Kept for the tasks that run on it.
        _runtime: Runtime,
        endpoint: String,
        seen: Arc<Mutex<Vec<Seen>>>,
    }

    impl Fake {
        /// Starts it; `script` says how the connection with a given number,
        /// counted from zero in the order they arrive, is treated.
        fn start(script: impl Fn(usize) -> Script + Send + 'static) -> std::io::Result<Self> {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()?;
            let listener = runtime.block_on(TcpListener::bind("127.0.0.1:0"))?;
            let endpoint = format!("ws://{}/live", listener.local_addr()?);
            let seen = Arc::new(Mutex::new(Vec::new()));
            let all = Arc::clone(&seen);
            runtime.spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    let index = {
                        let mut all = all.lock().unwrap_or_else(PoisonError::into_inner);
                        all.push(Seen::default());
                        all.len() - 1
                    };
                    tokio::spawn(connection(stream, script(index), index, Arc::clone(&all)));
                }
            });
            Ok(Self {
                _runtime: runtime,
                endpoint,
                seen,
            })
        }

        fn seen(&self) -> Vec<Seen> {
            self.seen
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }

        /// Waits until what the stand-in has seen passes `check`. Says whether it did.
        fn wait(&self, check: impl Fn(&[Seen]) -> bool) -> bool {
            let started = Clock::now();
            while started.elapsed() < Duration::from_secs(5) {
                if check(&self.seen()) {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            false
        }

        /// A provider that talks to the stand-in, and the notes it writes
        /// where the real one writes to the log.
        fn speech(&self, timing: Timing) -> Result<(GeminiSpeech, Arc<Mutex<Vec<String>>>)> {
            let notes = Arc::new(Mutex::new(Vec::new()));
            let written = Arc::clone(&notes);
            let note = move |text: &str| {
                written
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(text.to_owned());
            };
            let speech = GeminiSpeech::with(config(&self.endpoint), timing, Box::new(note))?;
            Ok((speech, notes))
        }
    }

    async fn connection(
        stream: TcpStream,
        script: Script,
        index: usize,
        all: Arc<Mutex<Vec<Seen>>>,
    ) {
        let note = |change: &dyn Fn(&mut Seen)| {
            if let Some(seen) = all
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get_mut(index)
            {
                change(seen);
            }
        };
        if script == Script::Refuse {
            drop(stream);
            note(&|seen| seen.closed = true);
            return;
        }
        let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else {
            return;
        };
        let mut audio_messages = 0;
        while let Some(Ok(message)) = socket.next().await {
            let Message::Text(text) = message else {
                continue;
            };
            let value: Value = serde_json::from_str(text.as_str()).unwrap_or_default();
            let interim = r#"{"serverContent":{"interimInputTranscription":{"text":"hello wor"}}}"#;
            let last = r#"{"serverContent":{"inputTranscription":{"text":"Hello world."}}}"#;
            let mut replies = Vec::new();
            if let Some(setup) = value.get("setup") {
                let languages = setup["inputAudioTranscription"]["languageCodes"].clone();
                note(&|seen| {
                    seen.events.push("setup");
                    seen.languages = languages.clone();
                });
                if script == Script::Mute {
                    std::future::pending::<()>().await;
                }
                if script == Script::SlowSetup {
                    tokio::time::sleep(SLOW_SETUP).await;
                    let accepted = Message::binary(br#"{"setupComplete":{}}"#.to_vec());
                    if socket.send(accepted).await.is_err() {
                        break;
                    }
                    note(&|seen| seen.events.push("ready"));
                    continue;
                }
                replies.push(r#"{"setupComplete":{}}"#);
            } else if let Some(data) = value["realtimeInput"]["audio"]["data"].as_str() {
                if script == Script::CloseOnAudio {
                    let _ = socket.close(None).await;
                    break;
                }
                let bytes = BASE64.decode(data).unwrap_or_default();
                let samples: Vec<i16> = bytes
                    .chunks_exact(2)
                    .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
                    .collect();
                note(&|seen| {
                    seen.events.push("audio");
                    seen.audio.extend_from_slice(&samples);
                });
                audio_messages += 1;
                if script == Script::FinalWhileSpeaking && audio_messages == 2 {
                    replies.push(r#"{"serverContent":{"inputTranscription":{"text":"First."}}}"#);
                } else {
                    replies.push(interim);
                }
            } else if value["realtimeInput"]["audioStreamEnd"] == true {
                note(&|seen| seen.events.push("end"));
                replies.push(last);
                if script == Script::MoreAfterFinal {
                    replies.push(interim);
                }
            }
            for reply in replies {
                // The real service sends its JSON in binary frames.
                if socket
                    .send(Message::binary(reply.as_bytes().to_vec()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            if script == Script::Deaf {
                std::future::pending::<()>().await;
            }
            if script == Script::HangUp {
                let _ = socket.close(None).await;
                break;
            }
        }
        note(&|seen| seen.closed = true);
    }

    /// Waits that never run out within a test.
    fn patient() -> Timing {
        Timing::default()
    }

    fn english() -> Vec<String> {
        vec!["en-US".to_owned()]
    }

    /// Runs one dictation of `chunks` chunks of audio, each sample of a chunk
    /// holding the number of that chunk counted from `first`, and returns
    /// what was typed.
    fn dictate(
        speech: &GeminiSpeech,
        languages: &[String],
        first: i16,
        chunks: i16,
    ) -> Result<String> {
        let mut session = speech.start_session(SessionOptions { languages })?;
        for chunk in first..first + chunks {
            session.feed(&[chunk; 160])?;
        }
        let mut typed = String::new();
        session.finish(&mut |transcript| {
            typed.push_str(&transcript.text);
            Ok(())
        })?;
        Ok(typed)
    }

    /// The audio `dictate` sends for these chunk numbers.
    fn audio_of(chunks: std::ops::Range<i16>) -> Vec<i16> {
        chunks.flat_map(|chunk| [chunk; 160]).collect()
    }

    fn served(seen: &Seen) -> bool {
        seen.events.contains(&"end")
    }

    #[test]
    fn a_session_streams_audio_and_returns_the_final_transcript() -> Outcome {
        let fake = Fake::start(|_| Script::Usual)?;
        let (speech, _) = fake.speech(patient())?;
        let mut session = speech.start_session(SessionOptions {
            languages: &english(),
        })?;
        for _ in 0..3 {
            assert!(session.feed(&[0i16; 1600]).is_ok());
        }
        let mut finals = Vec::new();
        let finished = session.finish(&mut |transcript| {
            finals.push(transcript.text);
            Ok(())
        });
        assert!(finished.is_ok(), "{finished:?}");
        assert_eq!(finals.concat(), "Hello world. ");
        let seen = fake.seen();
        assert_eq!(seen[0].events, ["setup", "audio", "audio", "audio", "end"]);
        Ok(())
    }

    #[test]
    fn a_refused_connection_is_reported() -> Outcome {
        let fake = Fake::start(|_| Script::Refuse)?;
        let (speech, _) = fake.speech(patient())?;
        let mut session = speech.start_session(SessionOptions { languages: &[] })?;
        let result = session.finish(&mut |_| Ok(()));
        assert!(
            matches!(&result, Err(Error::Transcription(why)) if !why.contains("secret-key")),
            "{result:?}"
        );
        Ok(())
    }

    #[test]
    fn a_final_transcript_is_handed_on_before_the_session_winds_down() -> Outcome {
        let fake = Fake::start(|_| Script::Usual)?;
        let (speech, _) = fake.speech(patient())?;
        let mut session = speech.start_session(SessionOptions {
            languages: &english(),
        })?;
        session.feed(&[0i16; 160])?;
        let mut handed_on = None;
        session.finish(&mut |transcript| {
            assert_eq!(transcript.text, "Hello world. ");
            handed_on = Some(Clock::now());
            Ok(())
        })?;
        let returned = Clock::now();
        // The stand-in never closes, so the session stays open for
        // `SETTLE_WAIT` after the final transcript. Typing did not wait for that.
        let ahead = handed_on.map(|at| returned.duration_since(at));
        assert!(
            ahead.is_some_and(|ahead| ahead >= SETTLE_WAIT * 2 / 3),
            "{ahead:?}"
        );
        Ok(())
    }

    #[test]
    fn a_final_transcript_while_speaking_comes_back_from_feed_at_once() -> Outcome {
        let fake = Fake::start(|_| Script::FinalWhileSpeaking)?;
        let (speech, _) = fake.speech(patient())?;
        let mut session = speech.start_session(SessionOptions {
            languages: &english(),
        })?;
        let mut finals = Vec::new();
        let started = Clock::now();
        // As a microphone would: a chunk every 10 ms, for at most 3 seconds.
        while finals.is_empty() && started.elapsed() < Duration::from_secs(3) {
            if let Some(transcript) = session.feed(&[0i16; 160])?
                && transcript.is_final
            {
                finals.push(transcript.text);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // It came while the audio was still going, long before `finish`.
        assert_eq!(finals, ["First. "]);
        assert!(!fake.seen()[0].events.contains(&"end"));
        Ok(())
    }

    #[test]
    fn a_failure_to_type_ends_the_session_at_once() -> Outcome {
        let fake = Fake::start(|_| Script::MoreAfterFinal)?;
        let (speech, _) = fake.speech(patient())?;
        let mut session = speech.start_session(SessionOptions {
            languages: &english(),
        })?;
        session.feed(&[0i16; 160])?;
        let stopped = Clock::now();
        let result = session.finish(&mut |_| Err(Error::Injection("no focused window".into())));
        assert!(matches!(result, Err(Error::Injection(_))), "{result:?}");
        drop(session);
        // The service has more to say, which the session would wait
        // `FINAL_WAIT` for; with nobody to type it, it closes instead.
        assert!(fake.wait(|seen| seen[0].closed));
        assert!(stopped.elapsed() < FINAL_WAIT / 2);
        Ok(())
    }

    #[test]
    fn a_prepared_session_serves_the_next_dictation_and_is_replaced() -> Outcome {
        let fake = Fake::start(|_| Script::Usual)?;
        let (speech, notes) = fake.speech(patient())?;
        // One session is set up before anything is dictated.
        assert!(fake.wait(|seen| seen.len() == 1 && seen[0].events == ["setup"]));

        assert_eq!(dictate(&speech, &english(), 0, 3)?, "Hello world. ");
        // The dictation ran on it, and another was prepared in its place.
        assert!(fake.wait(|seen| seen.len() == 2 && seen[1].events == ["setup"]));
        assert_eq!(fake.seen()[0].audio, audio_of(0..3));

        assert_eq!(dictate(&speech, &english(), 3, 2)?, "Hello world. ");
        assert!(fake.wait(|seen| seen.len() == 3 && seen[2].events == ["setup"]));
        let seen = fake.seen();
        assert_eq!(seen[1].audio, audio_of(3..5));
        // Nothing went wrong, so nothing was written down.
        assert!(
            notes
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty()
        );
        Ok(())
    }

    /// Presses the key, speaks, and says how long the first words took to
    /// come back.
    fn time_to_first_words(speech: &GeminiSpeech, languages: &[String]) -> Result<Duration> {
        let pressed = Clock::now();
        let mut session = speech.start_session(SessionOptions { languages })?;
        while pressed.elapsed() < Duration::from_secs(3) {
            if session.feed(&[0i16; 160])?.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let took = pressed.elapsed();
        session.finish(&mut |_| Ok(()))?;
        Ok(took)
    }

    #[test]
    fn a_prepared_session_spares_the_dictation_the_wait_for_the_setup() -> Outcome {
        let fake = Fake::start(|_| Script::SlowSetup)?;
        let (speech, _) = fake.speech(patient())?;
        // The prepared session has been through the slow setup already.
        assert!(fake.wait(|seen| seen.len() == 1 && seen[0].events == ["setup", "ready"]));

        // So the first words on it do not wait for the setup.
        let prepared = time_to_first_words(&speech, &english())?;
        assert!(prepared < SLOW_SETUP, "{prepared:?}");
        // A dictation that has to connect for itself, here because it is in
        // another language, waits as every dictation did before.
        let alone = time_to_first_words(&speech, &["sl-SI".to_owned()])?;
        assert!(alone >= SLOW_SETUP, "{alone:?}");
        Ok(())
    }

    #[test]
    fn a_prepared_session_for_other_languages_is_discarded() -> Outcome {
        let fake = Fake::start(|_| Script::Usual)?;
        let (speech, _) = fake.speech(patient())?;
        assert!(fake.wait(|seen| seen.len() == 1 && seen[0].events == ["setup"]));

        let slovenian = ["sl-SI".to_owned()];
        assert_eq!(dictate(&speech, &slovenian, 0, 3)?, "Hello world. ");
        assert!(fake.wait(|seen| seen.len() >= 2 && seen[0].closed));
        let seen = fake.seen();
        // The one prepared for English heard nothing and was closed.
        assert_eq!(seen[0].languages, json!(["en-US"]));
        assert_eq!(seen[0].events, ["setup"]);
        // The dictation got a session of its own, in its own language.
        assert_eq!(seen[1].languages, json!(["sl-SI"]));
        assert_eq!(seen[1].audio, audio_of(0..3));
        // And the next one prepared is for the language just used.
        assert!(fake.wait(|seen| seen.len() == 3 && seen[2].events == ["setup"]));
        assert_eq!(fake.seen()[2].languages, json!(["sl-SI"]));
        Ok(())
    }

    /// Dictates on a prepared session that has died in the way `script`
    /// says, and checks that the dictation is served in full all the same.
    fn a_dead_prepared_session_costs_nothing(script: Script) -> Outcome {
        let fake = Fake::start(move |index| if index == 0 { script } else { Script::Usual })?;
        let timing = Timing {
            answer_wait: Duration::from_millis(200),
            ..patient()
        };
        let (speech, notes) = fake.speech(timing)?;
        assert!(fake.wait(|seen| seen.len() == 1 && seen[0].events == ["setup"]));

        assert_eq!(dictate(&speech, &english(), 0, 5)?, "Hello world. ");
        let seen = fake.seen();
        // Every chunk arrived on the fresh connection, once and in order.
        assert_eq!(seen[1].audio, audio_of(0..5));
        assert!(served(&seen[1]));
        // No third connection was made for it, and it is written down once.
        assert_eq!(seen.len(), 2);
        let notes = notes.lock().unwrap_or_else(PoisonError::into_inner).clone();
        assert_eq!(notes.len(), 1, "{notes:?}");
        Ok(())
    }

    #[test]
    fn a_prepared_session_that_died_silently_falls_back_with_all_the_audio() -> Outcome {
        a_dead_prepared_session_costs_nothing(Script::Deaf)
    }

    #[test]
    fn a_prepared_session_that_closes_on_use_falls_back_with_all_the_audio() -> Outcome {
        a_dead_prepared_session_costs_nothing(Script::CloseOnAudio)
    }

    #[test]
    fn a_prepared_session_that_died_is_noticed_while_the_user_still_speaks() -> Outcome {
        let fake = Fake::start(|index| {
            if index == 0 {
                Script::Deaf
            } else {
                Script::Usual
            }
        })?;
        let timing = Timing {
            answer_wait: Duration::from_millis(100),
            ..patient()
        };
        let (speech, _) = fake.speech(timing)?;
        assert!(fake.wait(|seen| seen.len() == 1 && seen[0].events == ["setup"]));

        let mut session = speech.start_session(SessionOptions {
            languages: &english(),
        })?;
        // Half a second of speech, a chunk every 10 ms: the change of
        // connection happens in the middle of it.
        for chunk in 0..50 {
            session.feed(&[chunk; 160])?;
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut typed = String::new();
        session.finish(&mut |transcript| {
            typed.push_str(&transcript.text);
            Ok(())
        })?;
        assert_eq!(typed, "Hello world. ");
        assert_eq!(fake.seen()[1].audio, audio_of(0..50));
        Ok(())
    }

    #[test]
    fn a_prepared_session_the_service_closes_is_simply_not_there() -> Outcome {
        let fake = Fake::start(|index| {
            if index == 0 {
                Script::HangUp
            } else {
                Script::Usual
            }
        })?;
        let (speech, notes) = fake.speech(patient())?;
        assert!(fake.wait(|seen| seen.len() == 1 && seen[0].closed));
        // Time for the provider to hear of it; nothing takes its place.
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(fake.seen().len(), 1);

        // The next dictation connects for itself and loses no time over it.
        assert_eq!(dictate(&speech, &english(), 0, 3)?, "Hello world. ");
        assert_eq!(fake.seen()[1].audio, audio_of(0..3));
        assert!(
            notes
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn a_prepared_session_is_let_go_when_its_time_is_up() -> Outcome {
        let fake = Fake::start(|_| Script::Usual)?;
        let timing = Timing {
            warm_for: Duration::from_millis(150),
            ..patient()
        };
        let (speech, notes) = fake.speech(timing)?;
        // It is closed unused, and nothing takes its place.
        assert!(fake.wait(|seen| seen.len() == 1 && seen[0].closed));
        std::thread::sleep(Duration::from_millis(300));
        let seen = fake.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].events, ["setup"]);

        // The next dictation connects for itself, as it did before sessions
        // were prepared.
        assert_eq!(dictate(&speech, &english(), 0, 3)?, "Hello world. ");
        assert_eq!(fake.seen()[1].audio, audio_of(0..3));
        assert!(
            notes
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn nothing_keeps_trying_when_the_service_cannot_be_reached() -> Outcome {
        let fake = Fake::start(|_| Script::Refuse)?;
        let (speech, notes) = fake.speech(patient())?;
        let count =
            |notes: &Mutex<Vec<String>>| notes.lock().unwrap_or_else(PoisonError::into_inner).len();
        // Preparing fails once and is written down once.
        assert!(fake.wait(|seen| seen.len() == 1));
        let started = Clock::now();
        while count(&notes) == 0 && started.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(fake.seen().len(), 1);
        assert_eq!(count(&notes), 1);

        // Each dictation makes its own attempt and reports its own error;
        // none of them sets off another attempt or another line in the log.
        for _ in 0..3 {
            assert!(dictate(&speech, &english(), 0, 1).is_err());
        }
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(fake.seen().len(), 4);
        assert_eq!(count(&notes), 1);
        Ok(())
    }

    #[test]
    fn a_press_during_a_connection_that_times_out_is_told_so_without_a_second_wait() -> Outcome {
        let fake = Fake::start(|_| Script::Mute)?;
        let connect = Duration::from_millis(1500);
        let timing = Timing {
            connect,
            ..patient()
        };
        let (speech, _) = fake.speech(timing)?;
        // The press comes while the session prepared at the start is still
        // waiting for the service, and takes it over.
        let pressed = Clock::now();
        let result = dictate(&speech, &english(), 0, 2);
        let took = pressed.elapsed();
        assert!(
            matches!(&result, Err(Error::Transcription(why)) if why.contains("timed out")),
            "{result:?}"
        );
        // It is told when that one attempt has run out of time: no second
        // connection is tried, and the time of a second one is not spent.
        assert!(took < connect * 2, "{took:?}");
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(fake.seen().len(), 1);
        Ok(())
    }

    #[test]
    fn a_press_during_a_connection_that_is_refused_connects_for_itself() -> Outcome {
        let fake = Fake::start(|index| {
            if index == 0 {
                Script::Refuse
            } else {
                Script::Usual
            }
        })?;
        let (speech, _) = fake.speech(patient())?;
        // Whether the press comes before the refusal or after it, the
        // dictation is served on a connection of its own.
        assert_eq!(dictate(&speech, &english(), 0, 3)?, "Hello world. ");
        assert_eq!(fake.seen()[1].audio, audio_of(0..3));
        Ok(())
    }

    #[test]
    fn preparing_waits_after_a_failure_and_then_resumes() -> Outcome {
        let fake = Fake::start(|index| {
            if index == 0 {
                Script::Refuse
            } else {
                Script::Usual
            }
        })?;
        // Long enough that a busy machine still gets the first dictation
        // done within it.
        let retry_first = Duration::from_secs(3);
        let timing = Timing {
            retry_first,
            ..patient()
        };
        let (speech, notes) = fake.speech(timing)?;
        // The failure is written down the moment the wait begins.
        let written =
            |notes: &Mutex<Vec<String>>| notes.lock().unwrap_or_else(PoisonError::into_inner).len();
        let started = Clock::now();
        while written(&notes) == 0 && started.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let failed = Clock::now();
        assert_eq!(written(&notes), 1);

        // Within the wait a dictation is served, but no session is prepared
        // after it.
        assert_eq!(dictate(&speech, &english(), 0, 2)?, "Hello world. ");
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(fake.seen().len(), 2);

        // After the wait the next dictation is followed by a prepared session
        // again, and the one after that runs on it.
        std::thread::sleep(
            (retry_first + Duration::from_millis(500)).saturating_sub(failed.elapsed()),
        );
        assert_eq!(dictate(&speech, &english(), 2, 2)?, "Hello world. ");
        assert!(fake.wait(|seen| seen.len() == 4 && seen[3].events == ["setup"]));
        assert_eq!(dictate(&speech, &english(), 4, 2)?, "Hello world. ");
        let seen = fake.seen();
        assert_eq!(seen[2].audio, audio_of(2..4));
        assert_eq!(seen[3].audio, audio_of(4..6));
        let notes = notes.lock().unwrap_or_else(PoisonError::into_inner).clone();
        assert_eq!(notes.len(), 1, "{notes:?}");
        Ok(())
    }
}
