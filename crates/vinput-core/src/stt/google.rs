//! Streaming recognition with Google Cloud Speech-to-Text v2 over gRPC.
//!
//! [`GoogleSpeech`] is created once and kept for the life of the process: it owns
//! the async runtime, the TLS connection and the credentials, so starting a
//! dictation session only opens a new stream on a connection that is already warm.

use std::sync::Arc;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use googleapis_tonic_google_cloud_speech_v2::google::cloud::speech::v2 as pb;
use pb::explicit_decoding_config::AudioEncoding;
use pb::recognition_config::DecodingConfig;
use pb::speech_client::SpeechClient;
use pb::streaming_recognize_request::StreamingRequest;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_stream::{Stream, StreamExt};
use tonic::metadata::MetadataMap;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};

use self::token::Tokens;
use super::live::{Network, failed as stt_error};
use super::{Emit, ProviderKind, SessionOptions, SpeechProvider, Transcriber, Transcript};
use crate::audio::SAMPLE_RATE_HZ;
use crate::{Error, Result, log};

pub(super) mod token;

/// Google rejects audio messages above 15 kB; stay well under it.
const MAX_AUDIO_BYTES: usize = 12_000;
/// The same in samples, each of which is two bytes.
const MAX_AUDIO_SAMPLES: usize = MAX_AUDIO_BYTES / 2;
/// How long `finish` waits for the service to deliver the last transcript.
const FINISH_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(30);
/// How soon a failed request for an access token is tried again, at first
/// and at most.
const TOKEN_RETRY: Duration = Duration::from_secs(60);
const TOKEN_RETRY_MAX: Duration = Duration::from_secs(900);

/// Which Google Cloud project, region, model and languages to recognize with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoogleConfig {
    /// Google Cloud project that is billed for recognition.
    pub project_id: String,
    /// Speech-to-Text location, such as `eu`, `us` or `global`.
    pub location: String,
    /// Recognition model, such as `chirp_3`.
    pub model: String,
    /// BCP-47 language codes to recognize, such as `en-US`.
    pub language_codes: Vec<String>,
}

impl GoogleConfig {
    /// Resource name of the implicit default recognizer in this project and location.
    fn recognizer(&self) -> String {
        format!(
            "projects/{}/locations/{}/recognizers/_",
            self.project_id, self.location
        )
    }

    /// The service endpoint for this location.
    fn endpoint(&self) -> String {
        endpoint(&self.location)
    }

    /// The first message of every stream, describing the audio and what to return.
    fn streaming_config(&self, languages: &[String]) -> pb::StreamingRecognizeRequest {
        let sample_rate_hertz = i32::try_from(SAMPLE_RATE_HZ).unwrap_or(i32::MAX);
        pb::StreamingRecognizeRequest {
            recognizer: self.recognizer(),
            streaming_request: Some(StreamingRequest::StreamingConfig(
                pb::StreamingRecognitionConfig {
                    config: Some(pb::RecognitionConfig {
                        model: self.model.clone(),
                        language_codes: languages.to_vec(),
                        features: Some(pb::RecognitionFeatures {
                            enable_automatic_punctuation: true,
                            ..Default::default()
                        }),
                        decoding_config: Some(DecodingConfig::ExplicitDecodingConfig(
                            pb::ExplicitDecodingConfig {
                                encoding: AudioEncoding::Linear16.into(),
                                sample_rate_hertz,
                                audio_channel_count: 1,
                            },
                        )),
                        ..Default::default()
                    }),
                    streaming_features: Some(pb::StreamingRecognitionFeatures {
                        interim_results: true,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )),
        }
    }
}

/// The address of the service for a location; every location except
/// `global` has a host of its own.
pub(super) fn endpoint(location: &str) -> String {
    if location == "global" {
        "https://speech.googleapis.com".to_owned()
    } else {
        format!("https://{location}-speech.googleapis.com")
    }
}

/// A warm connection to Google Cloud Speech-to-Text, shared by all sessions.
pub struct GoogleSpeech {
    network: Network,
    channel: Channel,
    tokens: Arc<Tokens>,
    config: GoogleConfig,
}

impl std::fmt::Debug for GoogleSpeech {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoogleSpeech")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl GoogleSpeech {
    /// Reads the Google sign-in of this PC and connects to the service.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transcription`] if credentials are missing or the service
    /// cannot be reached.
    pub fn connect(config: GoogleConfig) -> Result<Self> {
        let network = Network::start()?;
        let tokens = Arc::new(Tokens::load()?);
        // Asks for the first token now and for the next before each runs out,
        // so a session never has to wait for one.
        drop(network.handle().spawn(keep_fresh(Arc::clone(&tokens))));
        let endpoint = Endpoint::from_shared(config.endpoint())
            .map_err(stt_error("building the service address"))?
            .tls_config(ClientTlsConfig::new().with_webpki_roots())
            .map_err(stt_error("configuring TLS"))?
            .connect_timeout(CONNECT_TIMEOUT)
            .http2_keep_alive_interval(KEEP_ALIVE_INTERVAL)
            .keep_alive_while_idle(true);
        let channel = network
            .wait_for(endpoint.connect())
            .map_err(stt_error("connecting to Google Speech-to-Text"))?;
        Ok(Self {
            network,
            channel,
            tokens,
            config,
        })
    }

    /// Opens a recognition stream for one dictation session.
    ///
    /// This returns immediately; a failure to open the stream is reported by the
    /// session's first `feed` or by `finish`.
    #[must_use]
    pub fn start_session(&self, languages: &[String]) -> GoogleSession {
        let (audio_tx, audio_rx) = unbounded_channel();
        let (result_tx, result_rx) = std_mpsc::channel();
        let (alive, gone) = oneshot::channel();
        // Queue the config first so it is always the first message on the stream.
        let _ = audio_tx.send(self.config.streaming_config(languages));

        let tokens = Arc::clone(&self.tokens);
        let project = self.config.project_id.clone();
        let mut client = SpeechClient::new(self.channel.clone());
        let recognizer = self.config.recognizer();
        let stream = async move {
            let outcome = async {
                let mut request = tonic::Request::new(UnboundedReceiverStream::new(audio_rx));
                *request.metadata_mut() = request_metadata(tokens, &project, &recognizer).await?;
                let responses = client
                    .streaming_recognize(request)
                    .await
                    .map_err(stt_error("opening the recognition stream"))?
                    .into_inner();
                relay(responses, &result_tx).await
            }
            .await;
            if let Err(err) = outcome {
                let _ = result_tx.send(Err(err));
            }
        };
        drop(self.network.handle().spawn(until_gone(gone, stream)));

        GoogleSession {
            recognizer: self.config.recognizer(),
            audio_tx: Some(audio_tx),
            result_rx,
            _alive: alive,
        }
    }
}

/// Hands on every transcript of a recognition stream as it arrives, until the
/// service ends the stream or nobody is listening any more.
async fn relay<S>(mut responses: S, results: &std_mpsc::Sender<Result<Transcript>>) -> Result<()>
where
    S: Stream<Item = std::result::Result<pb::StreamingRecognizeResponse, tonic::Status>> + Unpin,
{
    while let Some(response) = responses.next().await {
        let response = response.map_err(stt_error("receiving a transcript"))?;
        for result in response.results {
            let Some(best) = result.alternatives.into_iter().next() else {
                continue;
            };
            let transcript = Transcript {
                text: best.transcript,
                is_final: result.is_final,
            };
            if results.send(Ok(transcript)).is_err() {
                return Ok(());
            }
        }
    }
    Ok(())
}

/// Runs the task of a stream until it is done or its session has been
/// dropped. Dropping the task cancels the stream, so after a failure to type,
/// say, nothing more is asked of the service.
async fn until_gone(gone: oneshot::Receiver<()>, stream: impl Future<Output = ()>) {
    tokio::select! {
        () = stream => {}
        _ = gone => {}
    }
}

/// Splits a chunk of audio into messages the service accepts.
///
/// The protocol lets the messages after the first leave the recognizer out,
/// but every released version has named it on each, so that is what is sent.
fn audio_requests<'a>(
    recognizer: &'a str,
    pcm: &'a [i16],
) -> impl Iterator<Item = pb::StreamingRecognizeRequest> + 'a {
    pcm.chunks(MAX_AUDIO_SAMPLES)
        .map(move |samples| pb::StreamingRecognizeRequest {
            recognizer: recognizer.to_owned(),
            streaming_request: Some(StreamingRequest::Audio(
                samples
                    .iter()
                    .flat_map(|sample| sample.to_le_bytes())
                    .collect(),
            )),
        })
}

impl SpeechProvider for GoogleSpeech {
    fn kind(&self) -> ProviderKind {
        ProviderKind::GoogleCloud
    }

    fn start_session(&self, options: SessionOptions<'_>) -> Result<Box<dyn Transcriber + Send>> {
        Ok(Box::new(Self::start_session(self, options.languages)))
    }
}

/// Keeps a usable access token in memory for as long as the provider lives.
/// A failure is logged and tried again; a session that needs a token before
/// then asks for one itself and reports its own error.
async fn keep_fresh(tokens: Arc<Tokens>) {
    let mut retry = TOKEN_RETRY;
    loop {
        let asking = Arc::clone(&tokens);
        let fresh = tokio::task::spawn_blocking(move || asking.token().map(|_| asking.fresh_for()));
        let wait = match fresh.await {
            Ok(Ok(fresh)) => {
                retry = TOKEN_RETRY;
                fresh.max(TOKEN_RETRY)
            }
            Ok(Err(err)) => {
                log::write(&format!("no Google access token yet: {err}"));
                retry = (retry * 2).min(TOKEN_RETRY_MAX);
                retry
            }
            Err(_) => return,
        };
        tokio::time::sleep(wait).await;
    }
}

/// Builds the per-request metadata: the access token, the project that is
/// billed, and the routing header.
async fn request_metadata(
    tokens: Arc<Tokens>,
    project: &str,
    recognizer: &str,
) -> Result<MetadataMap> {
    // Asking Google for a token waits on the network, so it is kept off the
    // thread that runs the connection. Usually the token is already there.
    let token = tokio::task::spawn_blocking(move || tokens.token())
        .await
        .map_err(stt_error("getting a Google access token"))??;
    let mut metadata = MetadataMap::new();
    metadata.insert(
        "authorization",
        format!("Bearer {token}")
            .parse()
            .map_err(stt_error("building the authorization header"))?,
    );
    metadata.insert(
        "x-goog-user-project",
        project
            .parse()
            .map_err(stt_error("building the project header"))?,
    );
    metadata.insert(
        "x-goog-request-params",
        format!("recognizer={recognizer}")
            .parse()
            .map_err(stt_error("building the routing header"))?,
    );
    Ok(metadata)
}

/// One recognition stream. Audio goes in through [`Transcriber::feed`]; transcripts
/// come back as the service produces them.
pub struct GoogleSession {
    recognizer: String,
    /// `None` once `finish` has closed the sending half of the stream.
    audio_tx: Option<UnboundedSender<pb::StreamingRecognizeRequest>>,
    result_rx: std_mpsc::Receiver<Result<Transcript>>,
    /// Dropped with the session, which ends its stream at once.
    _alive: oneshot::Sender<()>,
}

impl std::fmt::Debug for GoogleSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoogleSession")
            .field("recognizer", &self.recognizer)
            .finish_non_exhaustive()
    }
}

impl GoogleSession {
    /// The error to report when the stream ended without the service saying why.
    fn closed() -> Error {
        Error::Transcription("the recognition stream closed unexpectedly".into())
    }
}

impl Transcriber for GoogleSession {
    fn feed(&mut self, pcm: &[i16]) -> Result<Option<Transcript>> {
        let Some(audio_tx) = &self.audio_tx else {
            return Err(Self::closed());
        };
        for request in audio_requests(&self.recognizer, pcm) {
            if audio_tx.send(request).is_err() {
                // The stream task has ended; it will have left its reason behind.
                return Err(self
                    .result_rx
                    .try_iter()
                    .find_map(Result::err)
                    .unwrap_or_else(Self::closed));
            }
        }

        // Hand back the first final transcript that is ready, else the newest interim.
        let mut interim = None;
        for result in self.result_rx.try_iter() {
            let transcript = result?;
            if transcript.is_final {
                return Ok(Some(transcript));
            }
            interim = Some(transcript);
        }
        Ok(interim)
    }

    fn finish(&mut self, emit: &mut Emit<'_>) -> Result<()> {
        // Dropping the sender half-closes the stream, which tells the service that
        // the audio is complete and makes it flush what it has.
        self.audio_tx = None;
        // Each final transcript is handed on when it arrives; the stream may
        // stay open a while longer before it is known to be complete.
        loop {
            match self.result_rx.recv_timeout(FINISH_TIMEOUT) {
                Ok(result) => {
                    let transcript = result?;
                    if transcript.is_final {
                        emit(transcript)?;
                    }
                }
                Err(std_mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
                Err(std_mpsc::RecvTimeoutError::Timeout) => {
                    return Err(Error::Transcription(
                        "timed out waiting for the final transcript".into(),
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use tokio::runtime::Runtime;

    use super::*;

    fn config(location: &str) -> GoogleConfig {
        GoogleConfig {
            project_id: "demo".into(),
            location: location.into(),
            model: "chirp_3".into(),
            language_codes: vec!["en-US".into()],
        }
    }

    #[test]
    fn regional_locations_use_their_own_endpoint() {
        assert_eq!(config("eu").endpoint(), "https://eu-speech.googleapis.com");
        assert_eq!(config("global").endpoint(), "https://speech.googleapis.com");
    }

    #[test]
    fn recognizer_is_the_implicit_default() {
        assert_eq!(
            config("eu").recognizer(),
            "projects/demo/locations/eu/recognizers/_"
        );
    }

    #[test]
    fn streaming_config_describes_16khz_mono_pcm_with_interim_results() {
        let request = config("eu").streaming_config(&["sl-SI".to_owned()]);
        // If the first message is not the config, the default fails the checks below.
        let streaming = match request.streaming_request {
            Some(StreamingRequest::StreamingConfig(streaming)) => streaming,
            _ => pb::StreamingRecognitionConfig::default(),
        };
        assert_eq!(
            streaming.streaming_features.map(|f| f.interim_results),
            Some(true)
        );
        let recognition = streaming.config.unwrap_or_default();
        assert_eq!(recognition.model, "chirp_3");
        assert_eq!(
            recognition.decoding_config,
            Some(DecodingConfig::ExplicitDecodingConfig(
                pb::ExplicitDecodingConfig {
                    encoding: AudioEncoding::Linear16.into(),
                    sample_rate_hertz: 16_000,
                    audio_channel_count: 1,
                }
            ))
        );
    }

    #[test]
    fn audio_goes_out_once_in_small_messages_that_name_the_recognizer() {
        let pcm: Vec<i16> = (0..13_000).collect();
        let recognizer = config("eu").recognizer();
        let requests: Vec<_> = audio_requests(&recognizer, &pcm).collect();
        assert_eq!(requests.len(), 3);
        let mut bytes = Vec::new();
        for request in requests {
            assert_eq!(request.recognizer, recognizer);
            let audio = match request.streaming_request {
                Some(StreamingRequest::Audio(audio)) => audio,
                _ => Vec::new(),
            };
            assert!(!audio.is_empty() && audio.len() <= MAX_AUDIO_BYTES);
            bytes.extend_from_slice(&audio);
        }
        let sent: Vec<i16> = bytes
            .chunks_exact(2)
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        assert_eq!(sent, pcm);
    }

    /// What the service sends for one recognized stretch of speech.
    fn response(text: &str, is_final: bool) -> pb::StreamingRecognizeResponse {
        pb::StreamingRecognizeResponse {
            results: vec![pb::StreamingRecognitionResult {
                alternatives: vec![pb::SpeechRecognitionAlternative {
                    transcript: text.to_owned(),
                    ..Default::default()
                }],
                is_final,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    type Responses =
        UnboundedSender<std::result::Result<pb::StreamingRecognizeResponse, tonic::Status>>;

    fn runtime() -> std::io::Result<Runtime> {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
    }

    /// A session whose responses come from the returned sender instead of the
    /// service: what is sent there arrives as if the service had said it, and
    /// dropping it ends the stream.
    fn session_over_a_stand_in(runtime: &Runtime) -> (GoogleSession, Responses) {
        let (responses, from_service) = unbounded_channel();
        let (audio_tx, mut to_service) = unbounded_channel();
        let (result_tx, result_rx) = std_mpsc::channel();
        let (alive, gone) = oneshot::channel();
        runtime.spawn(until_gone(gone, async move {
            let outcome = relay(UnboundedReceiverStream::new(from_service), &result_tx).await;
            if let Err(err) = outcome {
                let _ = result_tx.send(Err(err));
            }
        }));
        // The audio goes nowhere, but is taken, as the service would take it.
        runtime.spawn(async move { while to_service.recv().await.is_some() {} });
        let session = GoogleSession {
            recognizer: config("eu").recognizer(),
            audio_tx: Some(audio_tx),
            result_rx,
            _alive: alive,
        };
        (session, responses)
    }

    #[test]
    fn a_final_transcript_is_handed_on_before_the_stream_closes() -> std::io::Result<()> {
        let runtime = runtime()?;
        let (mut session, responses) = session_over_a_stand_in(&runtime);
        // The service sends the last words, then an interim result that is
        // not typed, and keeps the stream open for a while before closing it.
        let linger = Duration::from_millis(400);
        runtime.spawn(async move {
            let _ = responses.send(Ok(response("last words", true)));
            let _ = responses.send(Ok(response("and", false)));
            tokio::time::sleep(linger).await;
        });
        let mut handed_on = Vec::new();
        let finished = session.finish(&mut |transcript| {
            handed_on.push((transcript.text, Instant::now()));
            Ok(())
        });
        let returned = Instant::now();
        assert!(finished.is_ok(), "{finished:?}");
        let texts: Vec<&str> = handed_on.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(texts, ["last words"]);
        // Typing did not wait for the stream to close.
        let ahead = handed_on
            .first()
            .map(|(_, at)| returned.duration_since(*at));
        assert!(ahead.is_some_and(|ahead| ahead >= linger / 2), "{ahead:?}");
        Ok(())
    }

    #[test]
    fn a_final_transcript_while_speaking_comes_back_from_feed() -> std::io::Result<()> {
        let runtime = runtime()?;
        let (mut session, responses) = session_over_a_stand_in(&runtime);
        let _ = responses.send(Ok(response("first", false)));
        let _ = responses.send(Ok(response("first words", true)));
        let started = Instant::now();
        let mut transcript = None;
        while transcript
            .as_ref()
            .is_none_or(|heard: &Transcript| !heard.is_final)
            && started.elapsed() < Duration::from_secs(3)
        {
            transcript = session.feed(&[0i16; 160]).ok().flatten().or(transcript);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            transcript,
            Some(Transcript {
                text: "first words".into(),
                is_final: true
            })
        );
        Ok(())
    }

    #[test]
    fn a_failure_to_type_ends_the_stream_at_once() -> std::io::Result<()> {
        let runtime = runtime()?;
        let (mut session, responses) = session_over_a_stand_in(&runtime);
        let _ = responses.send(Ok(response("last words", true)));
        let result = session.finish(&mut |_| Err(Error::Injection("no focused window".into())));
        assert!(matches!(result, Err(Error::Injection(_))), "{result:?}");
        drop(session);
        // The service has not closed the stream; the session let go of it.
        let let_go = runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(3), responses.closed()).await
        });
        assert!(let_go.is_ok());
        Ok(())
    }

    #[test]
    fn a_failure_of_the_stream_is_reported() -> std::io::Result<()> {
        let runtime = runtime()?;
        let (mut session, responses) = session_over_a_stand_in(&runtime);
        let _ = responses.send(Err(tonic::Status::unavailable("gone fishing")));
        let result = session.finish(&mut |_| Ok(()));
        assert!(
            matches!(&result, Err(Error::Transcription(why)) if why.contains("gone fishing")),
            "{result:?}"
        );
        Ok(())
    }
}
