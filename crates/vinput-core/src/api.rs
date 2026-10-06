//! The local API: how other programs follow and drive Vinput.
//!
//! Vinput itself only types into the focused window, searches the web for what
//! was said and saves voice memos.
//! Anything else, such as sending a dictation to a chat app, mailing a memo or
//! lighting a key on a control deck, is the job of a separate program that
//! connects here. The API is a named pipe that carries one JSON object per
//! line in each direction: Vinput sends [`Event`]s and accepts [`Command`]s.
//! See `docs/api.md` for the protocol.

#[cfg(windows)]
mod client;
#[cfg(windows)]
mod pipe;

use std::fmt;

use serde_json::{Map, Value, json};

#[cfg(windows)]
pub use client::{Client, Notice};
#[cfg(windows)]
pub use pipe::{ClientId, Request, Server};

use crate::session::Target;

/// The pipe other programs connect to.
pub const PIPE_NAME: &str = r"\\.\pipe\vinput";

/// Version of the protocol, sent in the first event. It grows only when a
/// change would break an existing client; additions keep it.
pub const PROTOCOL_VERSION: u32 = 1;

/// The longest line, in bytes without its line break, that either side sends.
/// A client's longer line is answered with [`ErrorCode::LineTooLong`] and
/// skipped.
pub const MAX_LINE: usize = 64 * 1024;

/// Programs served at once. Any more are told so with
/// [`ErrorCode::TooManyClients`] and disconnected.
pub const MAX_CLIENTS: usize = 16;

/// The longest `id`, in characters. Longer ones are refused.
pub const MAX_ID: usize = 128;

/// The longest program name, in characters. Longer ones are shortened.
const MAX_NAME: usize = 48;

/// The modes a session can be started in, as the API names them.
pub const MODES: [&str; 5] = ["type", "type-enter", "capture", "search", "memo"];

/// The commands Vinput understands, as the API names them.
pub const COMMANDS: [&str; 4] = ["hello", "start", "stop", "toggle"];

/// The name the API gives a mode.
#[must_use]
pub fn mode_name(target: Target) -> &'static str {
    match target {
        Target::Focused => "type",
        Target::FocusedEnter => "type-enter",
        Target::Capture => "capture",
        Target::Search => "search",
        Target::Memo => "memo",
    }
}

/// The mode an API name stands for.
#[must_use]
pub fn mode_from_name(name: &str) -> Option<Target> {
    match name {
        "type" => Some(Target::Focused),
        "type-enter" => Some(Target::FocusedEnter),
        "capture" => Some(Target::Capture),
        "search" => Some(Target::Search),
        "memo" => Some(Target::Memo),
        _ => None,
    }
}

/// Why something failed, for programs to act on. Each is sent as a fixed
/// `snake_case` word in the `code` of an `error` event; the words never change
/// meaning, and new ones may be added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    /// The line is not a JSON object.
    InvalidJson,
    /// A JSON object, but not a valid command: `command` is missing or a field
    /// has the wrong type or size.
    InvalidRequest,
    /// `command` names nothing Vinput knows.
    UnknownCommand,
    /// `mode` names nothing Vinput knows.
    UnknownMode,
    /// The line is longer than [`MAX_LINE`] bytes.
    LineTooLong,
    /// The client speaks a newer protocol than this Vinput.
    UnsupportedProtocol,
    /// As many programs as Vinput serves are connected already.
    TooManyClients,
    /// A hotkey is being set in Vinput's settings window, and sessions wait
    /// until that is done.
    SettingsOpen,
    /// A session is running already, so `start` did nothing.
    Busy,
    /// No speech model is chosen, so only voice memos can be recorded.
    NoModel,
    /// The session started but failed.
    SessionFailed,
}

impl ErrorCode {
    /// Every code, for tests and documentation.
    pub const ALL: [Self; 11] = [
        Self::InvalidJson,
        Self::InvalidRequest,
        Self::UnknownCommand,
        Self::UnknownMode,
        Self::LineTooLong,
        Self::UnsupportedProtocol,
        Self::TooManyClients,
        Self::SettingsOpen,
        Self::Busy,
        Self::NoModel,
        Self::SessionFailed,
    ];

    /// The word sent for this code.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidJson => "invalid_json",
            Self::InvalidRequest => "invalid_request",
            Self::UnknownCommand => "unknown_command",
            Self::UnknownMode => "unknown_mode",
            Self::LineTooLong => "line_too_long",
            Self::UnsupportedProtocol => "unsupported_protocol",
            Self::TooManyClients => "too_many_clients",
            Self::SettingsOpen => "settings_open",
            Self::Busy => "busy",
            Self::NoModel => "no_model",
            Self::SessionFailed => "session_failed",
        }
    }

    /// The code a word stands for; `None` for words from a newer Vinput.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|code| code.as_str() == name)
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A line Vinput could not take as a command, with what to tell the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// Why, for programs.
    pub code: ErrorCode,
    /// Why, for people.
    pub message: String,
    /// The `id` the line carried, if it had a usable one.
    pub id: Option<String>,
}

impl Refusal {
    fn new(code: ErrorCode, message: impl Into<String>, id: Option<String>) -> Self {
        Self {
            code,
            message: message.into(),
            id,
        }
    }

    /// The `error` event that answers the line.
    #[must_use]
    pub fn event(&self) -> Event<'_> {
        Event::Error {
            code: self.code,
            message: &self.message,
            id: self.id.as_deref(),
        }
    }
}

/// Something a connected program asks Vinput to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Start a session in the given mode, unless one is running.
    Start {
        /// Where the result goes.
        target: Target,
        /// Echoed in the events of the session, so the program recognizes its
        /// own.
        id: Option<String>,
    },
    /// Stop the running session, if there is one.
    Stop {
        /// Echoed in an error about this command.
        id: Option<String>,
    },
    /// Stop the running session, or start one if none is running.
    Toggle {
        /// Where the result goes, if this starts a session.
        target: Target,
        /// Echoed in the events of the session it starts.
        id: Option<String>,
    },
    /// The program says what it is called, for the list in Settings.
    Hello {
        /// A short name for people.
        name: String,
    },
}

impl Command {
    /// Reads one line sent by a client.
    ///
    /// # Errors
    ///
    /// Returns what to tell the client if the line is not a valid command.
    pub fn parse(line: &str) -> Result<Self, Refusal> {
        let message: Map<String, Value> = serde_json::from_str(line).map_err(|err| {
            Refusal::new(
                ErrorCode::InvalidJson,
                format!("not a JSON object: {err}"),
                None,
            )
        })?;
        let id = match message.get("id") {
            None | Some(Value::Null) => None,
            Some(Value::String(id)) if id.chars().count() <= MAX_ID => Some(id.clone()),
            Some(Value::String(_)) => {
                return Err(Refusal::new(
                    ErrorCode::InvalidRequest,
                    format!("\"id\" is longer than {MAX_ID} characters"),
                    None,
                ));
            }
            Some(_) => {
                return Err(Refusal::new(
                    ErrorCode::InvalidRequest,
                    "\"id\" must be a string",
                    None,
                ));
            }
        };
        let refuse = |code, message: String| Err(Refusal::new(code, message, id.clone()));
        let text = |name: &str| match message.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => Ok(Some(text.as_str())),
            Some(_) => Err(Refusal::new(
                ErrorCode::InvalidRequest,
                format!("{name:?} must be a string"),
                id.clone(),
            )),
        };
        let target = || match text("mode")? {
            None => Ok(Target::Focused),
            Some(mode) => mode_from_name(mode).ok_or_else(|| {
                Refusal::new(
                    ErrorCode::UnknownMode,
                    format!("unknown mode {mode:?}; use {}", MODES.join(", ")),
                    id.clone(),
                )
            }),
        };
        match text("command")? {
            Some("start") => Ok(Self::Start {
                target: target()?,
                id: id.clone(),
            }),
            Some("stop") => Ok(Self::Stop { id: id.clone() }),
            Some("toggle") => Ok(Self::Toggle {
                target: target()?,
                id: id.clone(),
            }),
            Some("hello") => {
                if let Some(protocol) = message.get("protocol") {
                    let supported = 1..=u64::from(PROTOCOL_VERSION);
                    match protocol.as_u64() {
                        Some(asked) if supported.contains(&asked) => {}
                        Some(asked) if asked > *supported.end() => {
                            return refuse(
                                ErrorCode::UnsupportedProtocol,
                                format!(
                                    "this Vinput speaks protocol {PROTOCOL_VERSION}, not {asked}; update Vinput"
                                ),
                            );
                        }
                        _ => {
                            return refuse(
                                ErrorCode::InvalidRequest,
                                format!(
                                    "\"protocol\" must be a whole number from 1 to {PROTOCOL_VERSION}"
                                ),
                            );
                        }
                    }
                }
                Ok(Self::Hello {
                    name: printable(text("name")?.unwrap_or_default(), MAX_NAME),
                })
            }
            Some(other) => refuse(
                ErrorCode::UnknownCommand,
                format!("unknown command {other:?}; use {}", COMMANDS.join(", ")),
            ),
            None => refuse(ErrorCode::InvalidRequest, "missing \"command\"".to_owned()),
        }
    }

    /// The `id` the command carries.
    #[must_use]
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Start { id, .. } | Self::Stop { id } | Self::Toggle { id, .. } => id.as_deref(),
            Self::Hello { .. } => None,
        }
    }

    /// Renders the command as one line of JSON, without the line break. This
    /// is what a client sends.
    #[must_use]
    pub fn to_line(&self) -> String {
        let mut line = match self {
            Self::Start { target, .. } => {
                json!({ "command": "start", "mode": mode_name(*target) })
            }
            Self::Toggle { target, .. } => {
                json!({ "command": "toggle", "mode": mode_name(*target) })
            }
            Self::Stop { .. } => json!({ "command": "stop" }),
            Self::Hello { name } => {
                json!({ "command": "hello", "name": name, "protocol": PROTOCOL_VERSION })
            }
        };
        if let Some(id) = self.id() {
            line["id"] = Value::from(id);
        }
        line.to_string()
    }
}

/// The first `max` characters of `text`, trimmed, with control characters left
/// out: it is shown to people as one line.
fn printable(text: &str, max: usize) -> String {
    text.trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim_end()
        .to_owned()
}

/// Something Vinput tells connected programs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event<'a> {
    /// The first line a client receives: who it is talking to and the state now.
    Hello {
        /// The mode of the running session, or `None` when idle.
        session: Option<Target>,
    },
    /// Vinput started or stopped listening.
    State {
        /// The mode of the session that started, or `None` when listening
        /// stopped.
        session: Option<Target>,
        /// The id given when the session was started over the API.
        id: Option<&'a str>,
    },
    /// A dictation started in `capture` mode ended; this is what was said. The
    /// text of dictations that are typed is never sent.
    Dictation {
        /// Everything recognized, or an empty string if nothing was.
        text: &'a str,
        /// The id given when the dictation was started.
        id: Option<&'a str>,
    },
    /// A voice memo was saved, whoever started it.
    Memo {
        /// Full path of the MP3 file.
        path: &'a str,
        /// Length of the recording.
        seconds: f32,
        /// The id given when the memo was started over the API.
        id: Option<&'a str>,
    },
    /// A session failed, or a command could not be carried out. Errors about a
    /// command go only to the client that sent it.
    Error {
        /// What went wrong, for programs.
        code: ErrorCode,
        /// What went wrong, in words meant for a person.
        message: &'a str,
        /// The id of the command or session it is about.
        id: Option<&'a str>,
    },
}

impl Event<'_> {
    /// Renders the event as one line of JSON, without the line break.
    #[must_use]
    pub fn to_line(&self) -> String {
        let state = |session: Option<Target>| {
            if session.is_some() {
                "listening"
            } else {
                "idle"
            }
        };
        let (mut line, id) = match *self {
            Self::Hello { session } => (
                json!({
                    "event": "hello",
                    "app": "vinput",
                    "version": env!("CARGO_PKG_VERSION"),
                    "protocol": PROTOCOL_VERSION,
                    "modes": MODES,
                    "commands": COMMANDS,
                    "max_line": MAX_LINE,
                    "state": state(session),
                }),
                None,
            ),
            Self::State { session, id } => {
                (json!({ "event": "state", "state": state(session) }), id)
            }
            Self::Dictation { text, id } => (json!({ "event": "dictation", "text": text }), id),
            Self::Memo { path, seconds, id } => (
                json!({
                    "event": "memo",
                    "path": path,
                    // One decimal place is all a length in seconds needs.
                    "seconds": (f64::from(seconds) * 10.0).round() / 10.0,
                }),
                id,
            ),
            Self::Error { code, message, id } => (
                json!({ "event": "error", "code": code.as_str(), "message": message }),
                id,
            ),
        };
        if let Self::Hello {
            session: Some(target),
        }
        | Self::State {
            session: Some(target),
            ..
        } = *self
        {
            line["mode"] = Value::from(mode_name(target));
        }
        if let Some(id) = id {
            line["id"] = Value::from(id);
        }
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal(line: &str) -> Refusal {
        Command::parse(line).err().unwrap_or_else(|| Refusal {
            code: ErrorCode::InvalidJson,
            message: format!("{line:?} was taken"),
            id: Some("taken".into()),
        })
    }

    #[test]
    fn commands_are_read_with_their_mode_and_id() {
        let parse = Command::parse;
        assert_eq!(
            parse(r#"{"command":"start"}"#),
            Ok(Command::Start {
                target: Target::Focused,
                id: None
            })
        );
        assert_eq!(
            parse(r#"{"command":"start","mode":"memo","id":"m-1"}"#),
            Ok(Command::Start {
                target: Target::Memo,
                id: Some("m-1".into())
            })
        );
        assert_eq!(
            parse(r#" {"command": "toggle", "mode": "capture"} "#),
            Ok(Command::Toggle {
                target: Target::Capture,
                id: None
            })
        );
        assert_eq!(
            parse(r#"{"command":"start","mode":"search","id":null}"#),
            Ok(Command::Start {
                target: Target::Search,
                id: None
            })
        );
        assert_eq!(
            parse(r#"{"command":"stop","extra":1}"#),
            Ok(Command::Stop { id: None })
        );
        assert_eq!(
            parse(r#"{"command":"stop","id":"s"}"#),
            Ok(Command::Stop {
                id: Some("s".into())
            })
        );
        assert_eq!(
            parse(r#"{"command":"hello","name":" Deck ","protocol":1}"#),
            Ok(Command::Hello {
                name: "Deck".into()
            })
        );
        assert_eq!(
            parse(r#"{"command":"hello"}"#),
            Ok(Command::Hello {
                name: String::new()
            })
        );
    }

    #[test]
    fn names_are_cut_to_one_short_printable_line() {
        let long = "x".repeat(100);
        let parsed = Command::parse(&json!({ "command": "hello", "name": long }).to_string());
        assert!(
            matches!(&parsed, Ok(Command::Hello { name }) if name.chars().count() == MAX_NAME),
            "{parsed:?}"
        );
        assert_eq!(
            printable("  Stream\u{7}\r\nDeck\u{200b}  ", 48),
            "ControlDeck\u{200b}"
        );
        assert_eq!(printable("ab   cd", 3), "ab");
    }

    #[test]
    fn commands_survive_a_round_trip() {
        for command in [
            Command::Start {
                target: Target::FocusedEnter,
                id: None,
            },
            Command::Toggle {
                target: Target::Capture,
                id: Some("x \"1\"".into()),
            },
            Command::Toggle {
                target: Target::Search,
                id: None,
            },
            Command::Stop { id: None },
            Command::Stop {
                id: Some("é".repeat(MAX_ID)),
            },
            Command::Hello {
                name: "Deck".into(),
            },
        ] {
            assert_eq!(Command::parse(&command.to_line()), Ok(command));
        }
    }

    #[test]
    fn bad_commands_say_what_is_wrong_with_a_code_and_the_id() {
        for (line, code, id) in [
            ("start", ErrorCode::InvalidJson, None),
            ("[1,2]", ErrorCode::InvalidJson, None),
            ("{}", ErrorCode::InvalidRequest, None),
            (r#"{"command":7}"#, ErrorCode::InvalidRequest, None),
            (
                r#"{"command":"start","id":7}"#,
                ErrorCode::InvalidRequest,
                None,
            ),
            (
                r#"{"command":"dance","id":"d"}"#,
                ErrorCode::UnknownCommand,
                Some("d"),
            ),
            (
                r#"{"command":"start","mode":"shout","id":"s"}"#,
                ErrorCode::UnknownMode,
                Some("s"),
            ),
            (
                r#"{"command":"start","mode":3}"#,
                ErrorCode::InvalidRequest,
                None,
            ),
            (
                r#"{"command":"hello","name":["a"]}"#,
                ErrorCode::InvalidRequest,
                None,
            ),
            (
                r#"{"command":"hello","protocol":2}"#,
                ErrorCode::UnsupportedProtocol,
                None,
            ),
            (
                r#"{"command":"hello","protocol":0}"#,
                ErrorCode::InvalidRequest,
                None,
            ),
            (
                r#"{"command":"hello","protocol":"1"}"#,
                ErrorCode::InvalidRequest,
                None,
            ),
        ] {
            let refused = refusal(line);
            assert_eq!(refused.code, code, "{line:?} gave {refused:?}");
            assert_eq!(refused.id.as_deref(), id, "{line:?} gave {refused:?}");
            assert!(!refused.message.is_empty());
        }
        let long_id = json!({ "command": "stop", "id": "x".repeat(MAX_ID + 1) }).to_string();
        assert_eq!(refusal(&long_id).code, ErrorCode::InvalidRequest);
    }

    #[test]
    fn modes_and_codes_have_one_name_each() {
        for name in MODES {
            assert_eq!(mode_from_name(name).map(mode_name), Some(name));
        }
        assert_eq!(mode_from_name("Type"), None);
        let mut names: Vec<_> = ErrorCode::ALL.iter().map(|code| code.as_str()).collect();
        for code in ErrorCode::ALL {
            assert_eq!(ErrorCode::from_name(code.as_str()), Some(code));
        }
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), ErrorCode::ALL.len());
        assert_eq!(ErrorCode::from_name("from_the_future"), None);
    }

    #[test]
    fn events_are_single_lines_of_json() {
        let hello = Event::Hello { session: None }.to_line();
        assert!(hello.contains(r#""event":"hello""#) && hello.contains(r#""state":"idle""#));
        assert!(hello.contains(r#""modes":["type","type-enter","capture","search","memo"]"#));
        assert!(hello.contains(r#""max_line":65536"#) && !hello.contains("mode\""));
        let busy = Event::Hello {
            session: Some(Target::Memo),
        }
        .to_line();
        assert!(busy.contains(r#""state":"listening""#) && busy.contains(r#""mode":"memo""#));
        assert_eq!(
            Event::State {
                session: Some(Target::Capture),
                id: Some("c-1")
            }
            .to_line(),
            r#"{"event":"state","id":"c-1","mode":"capture","state":"listening"}"#
        );
        assert_eq!(
            Event::State {
                session: None,
                id: None
            }
            .to_line(),
            r#"{"event":"state","state":"idle"}"#
        );
        let dictation = Event::Dictation {
            text: "two\nlines \"quoted\" čšž",
            id: Some("a-7"),
        }
        .to_line();
        assert!(!dictation.contains('\n') && dictation.contains(r#""id":"a-7""#));
        assert_eq!(
            serde_json::from_str::<Value>(&dictation)
                .ok()
                .and_then(|value| value.get("text").and_then(Value::as_str).map(str::to_owned)),
            Some("two\nlines \"quoted\" čšž".to_owned())
        );
        assert_eq!(
            Event::Memo {
                path: r"C:\memos\a.mp3",
                seconds: 12.34,
                id: None,
            }
            .to_line(),
            r#"{"event":"memo","path":"C:\\memos\\a.mp3","seconds":12.3}"#
        );
        assert_eq!(
            Event::Error {
                code: ErrorCode::Busy,
                message: "a session is running",
                id: Some("b"),
            }
            .to_line(),
            r#"{"code":"busy","event":"error","id":"b","message":"a session is running"}"#
        );
    }
}
