//! What the providers that stream over a WebSocket share: the network
//! thread, the TLS settings, connecting in two steps, and the dictation's
//! end of a stream.
//!
//! Connecting is two steps on purpose. [`dial`] reaches the server and sets
//! up TLS, which needs nothing but its name; [`upgrade`] turns that
//! connection into a WebSocket, which is where a key or a token is shown.
//! So a connection can be made before the token for it is at hand, and one
//! can be kept ready that has not asked the server for anything yet.

use std::sync::Arc;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tokio::runtime::Handle;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::tungstenite::error::UrlError;
use tokio_tungstenite::tungstenite::handshake::client::Request;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, client_async_with_config};

use super::{Emit, Transcriber, Transcript};
use crate::{Error, Result};

/// How long closing a connection politely may take before it is just dropped.
const CLOSE_WAIT: Duration = Duration::from_secs(1);
/// How long `finish` waits for the next thing the service says before it
/// gives the stream up.
const FINISH_TIMEOUT: Duration = Duration::from_secs(12);

/// A connection to a server, encrypted unless it is to a stand-in on this
/// computer, that has not asked for anything yet.
pub(super) type Stream = MaybeTlsStream<TcpStream>;
/// A WebSocket over a [`Stream`].
pub(super) type Socket = WebSocketStream<Stream>;

/// The thread the streams of one provider run on.
///
/// It is one thread, so the runtime on it is the single-threaded one: the
/// thread is started here, runs the runtime until this is dropped, and is
/// the only one that polls what is spawned on it. (The scheduler for several
/// workers, asked for one worker, did the same and cost the executable
/// 48 kB.)
///
/// A provider is let go of on the thread of the message loop, when another
/// model is chosen or someone signs in. Dropping a runtime waits for what
/// runs on its blocking threads, which here are requests that may wait ten
/// seconds for an answer; so this one is shut down without waiting, and
/// whatever was under way ends by itself.
pub(super) struct Network {
    /// Tells the thread to shut the runtime down. `None` only while this is
    /// being dropped.
    stop: Option<oneshot::Sender<()>>,
    handle: Handle,
}

impl Network {
    /// Starts the thread.
    pub(super) fn start() -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(failed("starting the network runtime"))?;
        let handle = runtime.handle().clone();
        let (stop, stopped) = oneshot::channel::<()>();
        std::thread::Builder::new()
            .name("vinput-stt".to_owned())
            .spawn(move || {
                // Until `stop` is used or dropped; either way it is over.
                runtime.block_on(async {
                    let _ = stopped.await;
                });
                runtime.shutdown_background();
            })
            .map_err(failed("starting the network runtime"))?;
        Ok(Self {
            handle,
            stop: Some(stop),
        })
    }

    /// The way onto the thread, for whoever starts a task there.
    pub(super) fn handle(&self) -> &Handle {
        &self.handle
    }

    /// Runs `work` to its end and waits for what it comes to: for connecting
    /// once, when the provider starts. The caller's thread polls it; the
    /// network thread wakes it.
    pub(super) fn wait_for<F: Future>(&self, work: F) -> F::Output {
        self.handle.block_on(work)
    }
}

impl Drop for Network {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

/// The TLS settings every stream of a provider shares, so that each one
/// after the first resumes the session of the one before.
///
/// A server is trusted if Windows trusts it: the roots are read from the
/// certificate store once, here, which is when a provider starts and not
/// when a key is pressed. That is the store every other request of Vinput
/// is checked against (`net.rs` asks Windows itself), so a PC whose
/// employer inspects its traffic can dictate as well as sign in, and a root
/// Windows has dropped is dropped here too. A list of roots built into the
/// executable was 54 kB of it and as old as the build. A certificate the
/// store holds and that cannot be read is left out; with none at all every
/// connection fails as one to an unknown server.
pub(super) fn tls() -> Result<Arc<rustls::ClientConfig>> {
    let mut roots = rustls::RootCertStore::empty();
    let _ = roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(failed("configuring TLS"))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Ok(Arc::new(config))
}

/// Reaches the server `request` is for and sets up TLS with it, without
/// asking it for anything. Audio leaves a connection as it is written:
/// small writes are not gathered (`TCP_NODELAY`).
pub(super) async fn dial(
    tls: &Arc<rustls::ClientConfig>,
    request: &Request,
) -> std::result::Result<Stream, WsError> {
    let uri = request.uri();
    let no_host = || WsError::Url(UrlError::NoHostName);
    let host = uri.host().ok_or_else(no_host)?;
    let secure = uri.scheme_str() == Some("wss");
    let port = uri.port_u16().unwrap_or(if secure { 443 } else { 80 });
    let tcp = TcpStream::connect((host, port)).await?;
    tcp.set_nodelay(true)?;
    if !secure {
        return Ok(MaybeTlsStream::Plain(tcp));
    }
    let name = ServerName::try_from(host.to_owned()).map_err(|_| no_host())?;
    let connector = tokio_rustls::TlsConnector::from(Arc::clone(tls));
    Ok(MaybeTlsStream::Rustls(connector.connect(name, tcp).await?))
}

/// Turns a connection into the WebSocket `request` asks for.
pub(super) async fn upgrade(
    request: Request,
    stream: Stream,
) -> std::result::Result<Socket, WsError> {
    let (socket, _) = client_async_with_config(request, stream, None).await?;
    Ok(socket)
}

/// Closes a connection politely, without letting a dead one hold anything up.
pub(super) async fn close(mut socket: Socket) {
    let _ = timeout(CLOSE_WAIT, socket.close(None)).await;
}

/// The error for a step that failed, in the words of what failed it.
pub(super) fn failed<E: std::fmt::Display>(action: &'static str) -> impl Fn(E) -> Error {
    move |err| Error::Transcription(format!("{action} failed: {err}"))
}

/// The task's end of a dictation: where its audio comes from and where its
/// transcripts go.
pub(super) struct Client {
    /// The audio, as the bytes of 16-bit little-endian samples.
    pub(super) audio: UnboundedReceiver<Vec<u8>>,
    /// Where transcripts, and the failure that ends them, go.
    pub(super) results: std_mpsc::Sender<Result<Transcript>>,
    /// Ends when the dictation has let go of the session, whether it
    /// finished or gave up; nothing is owed to it after that.
    pub(super) gone: oneshot::Receiver<()>,
}

/// The dictation's end of a stream. Audio goes in through
/// [`Transcriber::feed`]; transcripts come back as the service produces
/// them.
pub(super) struct Session {
    /// What the stream is called when it ends without a word.
    what: &'static str,
    /// The most bytes of audio handed on in one piece.
    piece: usize,
    /// `None` once `finish` has ended the audio.
    audio: Option<UnboundedSender<Vec<u8>>>,
    results: std_mpsc::Receiver<Result<Transcript>>,
    /// The audio that was fed and not taken, because the task had let go
    /// of the stream by then: whoever carries the dictation on takes it.
    unsent: Vec<i16>,
    /// The newest guess at what follows the settled text, if one has come
    /// since the last settled text and nobody has taken it yet.
    guess: Option<String>,
    /// The failure that came after settled text, which was handed back
    /// first.
    failed: Option<Error>,
    /// Dropped with the session, which tells its task to stop at once: after
    /// a failure to type, say, nothing more is wanted from the service.
    _alive: oneshot::Sender<()>,
}

/// The two ends of a new stream called `what`, whose audio is handed on in
/// pieces of at most `piece` bytes.
pub(super) fn session(what: &'static str, piece: usize) -> (Session, Client) {
    let (audio_tx, audio) = unbounded_channel();
    let (results, result_rx) = std_mpsc::channel();
    let (alive, gone) = oneshot::channel();
    let session = Session {
        what,
        piece: piece.max(2),
        audio: Some(audio_tx),
        results: result_rx,
        unsent: Vec::new(),
        guess: None,
        failed: None,
        _alive: alive,
    };
    let client = Client {
        audio,
        results,
        gone,
    };
    (session, client)
}

impl Session {
    fn closed(&self) -> Error {
        Error::Transcription(format!("the {} closed unexpectedly", self.what))
    }

    /// What the task left behind that was not handed on yet: transcripts,
    /// and the failure it ended with, if it ended with one.
    pub(super) fn rest(&mut self) -> (Vec<Transcript>, Option<Error>) {
        let mut transcripts = Vec::new();
        let mut failed = self.failed.take();
        for result in self.results.try_iter() {
            match result {
                Ok(transcript) => transcripts.push(transcript),
                Err(why) => failed = Some(why),
            }
        }
        (transcripts, failed)
    }

    /// The audio that was fed after the task had let go of the stream,
    /// in the order it was fed. It is not kept here after this.
    pub(super) fn take_unsent(&mut self) -> Vec<i16> {
        std::mem::take(&mut self.unsent)
    }

    /// The service's guess at what follows the text handed back so far, if
    /// it has made a new one since this was last asked.
    pub(super) fn take_guess(&mut self) -> Option<String> {
        self.guess.take()
    }
}

/// Samples as the bytes they are sent as.
pub(super) fn bytes_of(pcm: &[i16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(pcm.len() * 2);
    for sample in pcm {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

impl Transcriber for Session {
    fn feed(&mut self, pcm: &[i16]) -> Result<Option<Transcript>> {
        if let Some(failed) = self.failed.take() {
            // The task had ended before this audio could be handed to it.
            self.unsent.extend_from_slice(pcm);
            return Err(failed);
        }
        let Some(audio) = &self.audio else {
            return Err(self.closed());
        };
        // The samples become bytes here, on the thread that has them, and
        // the network thread only passes them on.
        // Once the task has let go, nothing more is taken: what is fed
        // from then on is kept, also by a call that still has settled text
        // to hand back and so does not fail.
        let mut sent = true;
        for piece in pcm.chunks(self.piece / 2) {
            sent = sent && audio.send(bytes_of(piece)).is_ok();
            if !sent {
                self.unsent.extend_from_slice(piece);
            }
        }
        // Hand back all the settled text that is ready, as one piece, else
        // the newest interim, and keep the guess that came after the
        // settled text for whoever asks: the two belong together, and a
        // guess read without the text settled before it would be a guess at
        // something else. What was settled before a failure is typed before
        // the failure is reported.
        let mut settled = String::new();
        let mut interim = None;
        for result in self.results.try_iter() {
            match result {
                Ok(transcript) if transcript.is_final => {
                    settled.push_str(&transcript.text);
                    // It took the place of the guess that was showing.
                    self.guess = None;
                    interim = None;
                }
                Ok(transcript) => {
                    self.guess = Some(transcript.text.clone());
                    interim = Some(transcript);
                }
                Err(failed) if settled.is_empty() => return Err(failed),
                Err(failed) => {
                    self.failed = Some(failed);
                    break;
                }
            }
        }
        if !settled.is_empty() {
            return Ok(Some(Transcript {
                text: settled,
                is_final: true,
            }));
        }
        if !sent {
            // The task has ended and has left nothing more behind.
            return Err(self.closed());
        }
        Ok(interim)
    }

    fn finish(&mut self, emit: &mut Emit<'_>) -> Result<()> {
        // Dropping the sender tells the task that the audio is complete.
        self.audio = None;
        // Each final transcript is handed on when it arrives; the stream may
        // stay open a while longer before it is known to be complete.
        loop {
            match self.results.recv_timeout(FINISH_TIMEOUT) {
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
    use super::*;

    #[test]
    fn samples_are_sent_as_little_endian_bytes() {
        assert_eq!(bytes_of(&[1, -2]), [1, 0, 0xFE, 0xFF]);
        assert!(bytes_of(&[]).is_empty());
    }

    #[test]
    fn audio_is_handed_on_in_pieces_no_larger_than_asked_for() {
        let (mut session, mut client) = session("test stream", 1000);
        assert!(session.feed(&[7i16; 1200]).is_ok());
        let mut pieces = Vec::new();
        while let Ok(piece) = client.audio.try_recv() {
            pieces.push(piece.len());
        }
        assert_eq!(pieces, [1000, 1000, 400]);
    }

    #[test]
    fn what_was_settled_is_handed_back_before_the_failure_that_followed() {
        let (mut session, client) = session("test stream", 1000);
        let settled = Transcript {
            text: "So far. ".into(),
            is_final: true,
        };
        assert!(client.results.send(Ok(settled.clone())).is_ok());
        let failure = Error::Transcription("it broke".into());
        assert!(client.results.send(Err(failure)).is_ok());
        drop(client);
        assert_eq!(session.feed(&[0; 160]).ok(), Some(Some(settled)));
        let next = session.feed(&[0; 160]);
        assert!(
            matches!(&next, Err(Error::Transcription(why)) if why == "it broke"),
            "{next:?}"
        );
        // And after that, only that it is over.
        let after = session.feed(&[0; 160]);
        assert!(
            matches!(&after, Err(Error::Transcription(why)) if why.contains("test stream closed")),
            "{after:?}"
        );
    }

    /// Reaches a real server over TLS without asking it for anything, and
    /// holds the connection for as long as one is kept ready after a
    /// dictation, to see whether the server leaves it open that long. Run
    /// by hand, with the server's name in `VINPUT_TLS_HOST`:
    /// `cargo test -p vinput-core a_real_server -- --ignored --nocapture`.
    #[test]
    #[ignore = "reaches a real server; needs VINPUT_TLS_HOST"]
    fn a_real_server_is_reached_over_tls_and_keeps_an_idle_connection() -> Result<()> {
        use tokio::io::AsyncReadExt;
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        let Ok(host) = std::env::var("VINPUT_TLS_HOST") else {
            return Ok(());
        };
        let request = format!("wss://{host}/")
            .into_client_request()
            .map_err(failed("naming the server"))?;
        let network = Network::start()?;
        let tls = tls()?;
        let limit = Duration::from_secs(95);
        let (reached, first, second, closed_after) = network.wait_for(async {
            let started = std::time::Instant::now();
            let reached = dial(&tls, &request).await.map(drop);
            let first = started.elapsed();
            // A second one resumes the TLS session of the first.
            let started = std::time::Instant::now();
            let again = dial(&tls, &request).await;
            let second = started.elapsed();
            let mut closed_after = None;
            if let Ok(mut stream) = again {
                let since = std::time::Instant::now();
                let mut byte = [0u8; 1];
                if timeout(limit, stream.read(&mut byte)).await.is_ok() {
                    closed_after = Some(since.elapsed());
                }
            }
            (reached, first, second, closed_after)
        });
        eprintln!(
            "first connection {first:?}, second {second:?}; idle one closed by the server after \
             {closed_after:?} (None: still open after {limit:?})"
        );
        assert!(reached.is_ok(), "{reached:?}");
        Ok(())
    }

    #[test]
    fn a_network_thread_is_let_go_of_without_waiting_for_what_runs_on_it() -> Result<()> {
        let network = Network::start()?;
        let (begun, has_begun) = std_mpsc::channel();
        let (release, held) = std_mpsc::channel::<()>();
        // A request that waits for an answer, as a token refresh does.
        drop(network.handle().spawn_blocking(move || {
            let _ = begun.send(());
            let _ = held.recv_timeout(Duration::from_secs(20));
        }));
        assert!(has_begun.recv_timeout(Duration::from_secs(5)).is_ok());
        let started = std::time::Instant::now();
        drop(network);
        let took = started.elapsed();
        drop(release);
        assert!(took < Duration::from_secs(2), "{took:?}");
        Ok(())
    }
}
