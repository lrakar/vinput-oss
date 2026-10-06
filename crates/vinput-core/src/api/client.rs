//! A client for the local API, for programs written in Rust.
//!
//! It keeps one connection to Vinput open on its own thread, reconnects when
//! Vinput restarts, and hands every event to a callback. Reading and writing
//! happen on the same thread, which matters on Windows: a pipe opened the
//! plain way cannot be written to while another thread is blocked reading it.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::ClientOptions;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use super::{Command, ErrorCode, mode_from_name};
use crate::session::Target;

/// How long to wait before looking for Vinput again when it is not running.
const RETRY: Duration = Duration::from_secs(1);

/// What a [`Client`] tells its owner.
#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    /// Connected to Vinput, which is listening or not.
    Connected {
        /// Whether a session is running.
        listening: bool,
    },
    /// Vinput went away. The client keeps trying to reconnect.
    Disconnected,
    /// Vinput started or stopped listening.
    State {
        /// Whether a session is running.
        listening: bool,
        /// The mode of the session that started; `None` when it stopped.
        mode: Option<Target>,
        /// The id the session was started with over the API.
        id: Option<String>,
    },
    /// A dictation in `capture` mode ended.
    Dictation {
        /// What was said.
        text: String,
        /// The id it was started with.
        id: Option<String>,
    },
    /// A voice memo was saved.
    Memo {
        /// Full path of the MP3 file.
        path: String,
        /// Length of the recording.
        seconds: f64,
        /// The id it was started with.
        id: Option<String>,
    },
    /// A session failed or Vinput refused a command. Refusals of a command
    /// reach only the client that sent it.
    Error {
        /// What went wrong, for programs; `None` for a code this client does
        /// not know, or from a Vinput older than 0.2.5.
        code: Option<ErrorCode>,
        /// What went wrong, for people.
        message: String,
        /// The id of the command or session it is about.
        id: Option<String>,
    },
}

impl Notice {
    /// Reads one event line sent by Vinput. `None` for events this client does
    /// not know, which newer versions of Vinput may send.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let event: Value = serde_json::from_str(line).ok()?;
        let text = |name: &str| event.get(name).and_then(Value::as_str).map(str::to_owned);
        let listening = || event.get("state").and_then(Value::as_str) == Some("listening");
        match event.get("event")?.as_str()? {
            "hello" => Some(Self::Connected {
                listening: listening(),
            }),
            "state" => Some(Self::State {
                listening: listening(),
                mode: event
                    .get("mode")
                    .and_then(Value::as_str)
                    .and_then(mode_from_name),
                id: text("id"),
            }),
            "dictation" => Some(Self::Dictation {
                text: text("text")?,
                id: text("id"),
            }),
            "memo" => Some(Self::Memo {
                path: text("path")?,
                seconds: event.get("seconds").and_then(Value::as_f64).unwrap_or(0.0),
                id: text("id"),
            }),
            "error" => Some(Self::Error {
                code: event
                    .get("code")
                    .and_then(Value::as_str)
                    .and_then(ErrorCode::from_name),
                message: text("message")?,
                id: text("id"),
            }),
            _ => None,
        }
    }
}

/// A connection to Vinput that looks after itself. Dropping it disconnects.
#[derive(Debug)]
pub struct Client {
    commands: UnboundedSender<String>,
    connected: Arc<AtomicBool>,
}

impl Client {
    /// Starts connecting to Vinput under `name`, which Vinput shows in its
    /// Settings. `on_notice` is called on the client thread for everything
    /// that happens.
    ///
    /// # Errors
    ///
    /// Returns an error if the client thread cannot be started.
    pub fn start(name: &str, on_notice: impl Fn(Notice) + Send + 'static) -> io::Result<Self> {
        Self::start_on(&crate::instance::pipe_name(), name, on_notice)
    }

    /// Like [`start`](Self::start), on a pipe of another name.
    ///
    /// # Errors
    ///
    /// Returns an error if the client thread cannot be started.
    pub fn start_on(
        pipe: &str,
        name: &str,
        on_notice: impl Fn(Notice) + Send + 'static,
    ) -> io::Result<Self> {
        let (commands, queued) = unbounded_channel();
        let connected = Arc::new(AtomicBool::new(false));
        let (pipe, flag) = (pipe.to_owned(), Arc::clone(&connected));
        let hello = Command::Hello {
            name: name.to_owned(),
        }
        .to_line();
        thread::Builder::new()
            .name("vinput-api-client".into())
            .spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                runtime.block_on(run(&pipe, &hello, queued, &flag, &on_notice));
            })?;
        Ok(Self {
            commands,
            connected,
        })
    }

    /// Whether Vinput is connected right now.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// Sends a command. Returns `false`, sending nothing, if Vinput is not
    /// connected: a command is never kept for later.
    #[must_use]
    pub fn send(&self, command: &Command) -> bool {
        self.is_connected() && self.commands.send(command.to_line()).is_ok()
    }
}

/// Connects, serves the connection until it ends, and starts over, until the
/// [`Client`] is dropped.
async fn run(
    pipe: &str,
    hello: &str,
    mut queued: UnboundedReceiver<String>,
    connected: &AtomicBool,
    on_notice: &impl Fn(Notice),
) {
    loop {
        if let Ok(connection) = ClientOptions::new().open(pipe) {
            let dropped = serve(connection, hello, &mut queued, connected, on_notice).await;
            if connected.swap(false, Ordering::Relaxed) {
                on_notice(Notice::Disconnected);
            }
            if dropped {
                return;
            }
        }
        // Wait a moment, leaving at once if the client is dropped meanwhile.
        // A command that arrives now has nowhere to go.
        let wait = tokio::time::sleep(RETRY);
        tokio::pin!(wait);
        loop {
            tokio::select! {
                () = &mut wait => break,
                command = queued.recv() => if command.is_none() {
                    return;
                },
            }
        }
    }
}

/// Serves one connection. Returns whether the [`Client`] was dropped, as
/// opposed to the connection ending.
async fn serve(
    connection: tokio::net::windows::named_pipe::NamedPipeClient,
    hello: &str,
    queued: &mut UnboundedReceiver<String>,
    connected: &AtomicBool,
    on_notice: &impl Fn(Notice),
) -> bool {
    let (reader, mut writer) = tokio::io::split(connection);
    let mut lines = BufReader::new(reader).lines();
    let mut write = async |line: &str| {
        writer.write_all(line.as_bytes()).await?;
        writer.write_all(b"\n").await
    };
    if write(hello).await.is_err() {
        return false;
    }
    loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(line)) => {
                    let Some(notice) = Notice::parse(&line) else { continue };
                    if matches!(notice, Notice::Connected { .. }) {
                        connected.store(true, Ordering::Relaxed);
                    }
                    on_notice(notice);
                }
                Ok(None) | Err(_) => return false,
            },
            command = queued.recv() => match command {
                Some(command) => {
                    if write(&command).await.is_err() {
                        return false;
                    }
                }
                None => return true,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::super::{Event, Server};
    use super::*;

    #[test]
    fn events_are_read_into_notices() {
        let parse = Notice::parse;
        assert_eq!(
            parse(
                &Event::Hello {
                    session: Some(Target::Focused)
                }
                .to_line()
            ),
            Some(Notice::Connected { listening: true })
        );
        assert_eq!(
            parse(
                &Event::State {
                    session: None,
                    id: None
                }
                .to_line()
            ),
            Some(Notice::State {
                listening: false,
                mode: None,
                id: None
            })
        );
        assert_eq!(
            parse(
                &Event::State {
                    session: Some(Target::Capture),
                    id: Some("c")
                }
                .to_line()
            ),
            Some(Notice::State {
                listening: true,
                mode: Some(Target::Capture),
                id: Some("c".into())
            })
        );
        assert_eq!(
            parse(
                &Event::Error {
                    code: ErrorCode::NoModel,
                    message: "no model",
                    id: Some("n")
                }
                .to_line()
            ),
            Some(Notice::Error {
                code: Some(ErrorCode::NoModel),
                message: "no model".into(),
                id: Some("n".into())
            })
        );
        // What Vinput 0.2.4 and older send.
        assert_eq!(
            parse(r#"{"event":"error","message":"old"}"#),
            Some(Notice::Error {
                code: None,
                message: "old".into(),
                id: None
            })
        );
        assert_eq!(
            parse(
                &Event::Dictation {
                    text: "hi",
                    id: Some("1")
                }
                .to_line()
            ),
            Some(Notice::Dictation {
                text: "hi".into(),
                id: Some("1".into())
            })
        );
        assert_eq!(
            parse(
                &Event::Memo {
                    path: r"C:\a.mp3",
                    seconds: 2.5,
                    id: None
                }
                .to_line()
            ),
            Some(Notice::Memo {
                path: r"C:\a.mp3".into(),
                seconds: 2.5,
                id: None
            })
        );
        assert_eq!(parse(r#"{"event":"from-the-future"}"#), None);
        assert_eq!(parse("nonsense"), None);
    }

    #[test]
    fn a_client_connects_names_itself_sends_and_receives() -> io::Result<()> {
        let pipe = format!(r"\\.\pipe\vinput-client-test-{}", std::process::id());
        let (woken_tx, woken) = mpsc::channel();
        let server = Server::start_on(&pipe, move || {
            let _ = woken_tx.send(());
        })?;
        let (notices_tx, notices) = mpsc::channel();
        let client = Client::start_on(&pipe, "Test client", move |notice| {
            let _ = notices_tx.send(notice);
        })?;
        let next = || notices.recv_timeout(Duration::from_secs(10)).ok();

        assert_eq!(next(), Some(Notice::Connected { listening: false }));
        assert!(client.is_connected());
        let start = Command::Start {
            target: Target::Capture,
            id: Some("c-1".into()),
        };
        assert!(client.send(&start));
        assert!(woken.recv_timeout(Duration::from_secs(5)).is_ok());
        assert_eq!(
            server
                .requests()
                .map(|request| request.command)
                .collect::<Vec<_>>(),
            [start]
        );
        assert_eq!(server.clients(), ["Test client"]);

        server.send(Event::State {
            session: Some(Target::Capture),
            id: Some("c-1"),
        });
        assert_eq!(
            next(),
            Some(Notice::State {
                listening: true,
                mode: Some(Target::Capture),
                id: Some("c-1".into())
            })
        );

        // When Vinput goes away the client says so and refuses commands.
        drop(server);
        assert_eq!(next(), Some(Notice::Disconnected));
        assert!(!client.send(&Command::Stop { id: None }));
        Ok(())
    }
}
