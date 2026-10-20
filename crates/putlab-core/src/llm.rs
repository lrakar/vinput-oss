//! How a model that writes is asked: over HTTP, answering as it writes.
//!
//! Whatever is done with what a model writes (it is spoken, or typed) is
//! done while the model is still writing, so every model is asked to
//! stream: its words arrive a few at a time, as server-sent events, and are
//! handed on at once ([`events`]). One form of request covers the model
//! engine on this PC and a server of the user's own: the chat form most
//! servers speak ([`Chat`]). Some models write what they think to
//! themselves first, however they are asked; that is taken out
//! ([`Aloud`]).

pub mod models;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Value, json};

use crate::net::{Request, Response};

/// How long connecting may take.
const CONNECT: Duration = Duration::from_secs(10);
/// How long a model may write nothing: one on this PC reads the whole text
/// before its first word.
const SILENCE: Duration = Duration::from_secs(180);
/// The longest line of an answer that is read; a line is a few words.
const MAX_LINE: usize = 256 * 1024;

/// Reads an answer that arrives as server-sent events and hands the data of
/// each event to `event`, until the answer ends, `event` says it is over,
/// or `stop` is set.
///
/// # Errors
///
/// Returns why the answer stopped arriving, in words for the log.
pub fn events(
    response: &mut Response,
    stop: &AtomicBool,
    event: &mut dyn FnMut(&str) -> bool,
) -> Result<(), String> {
    let mut lines = Lines::default();
    let mut buffer = [0u8; 4096];
    loop {
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        let read = response.read(&mut buffer)?;
        if read == 0 {
            return Ok(());
        }
        if !lines.push(&buffer[..read], event)? {
            return Ok(());
        }
    }
}

/// The bytes of an answer that arrives as server-sent events, cut into its
/// lines as they are complete.
#[derive(Debug, Default)]
struct Lines {
    held: Vec<u8>,
}

impl Lines {
    /// Takes `more` of the answer and hands the data of every event that is
    /// complete with it to `event`. Says whether `event` wants more.
    fn push(&mut self, more: &[u8], event: &mut dyn FnMut(&str) -> bool) -> Result<bool, String> {
        self.held.extend_from_slice(more);
        while let Some(end) = self.held.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.held.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line);
            if let Some(data) = line.trim_end().strip_prefix("data:")
                && !event(data.trim_start())
            {
                return Ok(false);
            }
        }
        if self.held.len() > MAX_LINE {
            return Err("the answer could not be read".to_owned());
        }
        Ok(true)
    }
}

/// Takes what a model thinks to itself out of what it writes: everything
/// between `<think>` and `</think>`, which some models write first however
/// they are asked. Fed a few words at a time, like the answer itself.
#[derive(Debug, Default)]
pub struct Aloud {
    thinking: bool,
    held: String,
}

impl Aloud {
    const OPEN: &'static str = "<think>";
    const CLOSE: &'static str = "</think>";

    /// What of `more`, with what was held back, is said aloud.
    pub fn push(&mut self, more: &str) -> String {
        self.held.push_str(more);
        let mut aloud = String::new();
        loop {
            let mark = if self.thinking {
                Self::CLOSE
            } else {
                Self::OPEN
            };
            if let Some(at) = self.held.find(mark) {
                if !self.thinking {
                    aloud.push_str(&self.held[..at]);
                }
                self.held.drain(..at + mark.len());
                self.thinking = !self.thinking;
                continue;
            }
            // The end of what is held may be the start of a mark.
            let keep = (1..mark.len())
                .rev()
                .find(|length| self.held.ends_with(&mark[..*length]))
                .unwrap_or(0);
            let ready = self.held.len() - keep;
            if !self.thinking {
                aloud.push_str(&self.held[..ready]);
            }
            self.held.drain(..ready);
            return aloud;
        }
    }
}

/// The address chat requests go to at a server whose address the user gave
/// as `base`: with or without `/v1`, and with or without the rest.
#[must_use]
pub fn chat_address(base: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    if base.ends_with("/chat/completions") {
        base.to_owned()
    } else if base.ends_with("/v1") {
        format!("{base}/chat/completions")
    } else {
        format!("{base}/v1/chat/completions")
    }
}

/// What a model is asked to write.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ask<'a> {
    /// What it is told to do.
    pub system: &'a str,
    /// What it is given to do it with.
    pub user: &'a str,
    /// How freely it writes; zero is the same answer every time.
    pub temperature: f64,
    /// The most it may write, in tokens.
    pub max_tokens: u32,
}

/// Why a model wrote nothing, or stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failed {
    /// No answer: nothing listens there, or it cannot be reached.
    Unreachable,
    /// The server answered with this status instead of writing.
    Status(u16),
    /// The answer stopped arriving in the middle.
    Stalled,
    /// The answer ended without a word in it.
    Nothing,
}

/// A server that speaks the chat form most servers speak: the model engine
/// on this PC, or a server of the user's own.
pub struct Chat {
    /// The address requests go to, ending in `/chat/completions`.
    address: String,
    /// The name of the model to ask for.
    model: String,
    /// The key the server asks for, if it asks for one.
    key: Option<String>,
    /// Whether the server is the model engine of this PC, which is told
    /// more than a server of someone's own would understand.
    own_engine: bool,
}

impl std::fmt::Debug for Chat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The key stays out of logs.
        f.debug_struct("Chat")
            .field("address", &self.address)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl Chat {
    /// A server of the user's own at `base`, asked for `model`.
    #[must_use]
    pub fn of_the_user(base: &str, model: &str) -> Self {
        Self {
            address: chat_address(base),
            model: model.trim().to_owned(),
            key: None,
            own_engine: false,
        }
    }

    /// The model engine of this PC, listening at `base` for requests that
    /// carry `key`.
    #[must_use]
    pub fn engine(base: &str, key: &str) -> Self {
        Self {
            address: chat_address(base),
            model: "local".to_owned(),
            key: Some(key.to_owned()),
            own_engine: true,
        }
    }

    /// The name of the model that is asked for.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Whether this is the model engine of this PC.
    #[must_use]
    pub fn is_own_engine(&self) -> bool {
        self.own_engine
    }

    /// The request that asks for `ask`.
    #[must_use]
    pub fn request(&self, ask: &Ask<'_>) -> Value {
        let mut request = json!({
            "model": self.model,
            "stream": true,
            "temperature": ask.temperature,
            "max_tokens": ask.max_tokens,
            "messages": [
                { "role": "system", "content": ask.system },
                { "role": "user", "content": ask.user },
            ],
        });
        if self.own_engine {
            // A model that can think to itself is asked not to: the wait
            // for its first word is the whole wait.
            request["chat_template_kwargs"] = json!({ "enable_thinking": false });
            request["cache_prompt"] = json!(false);
        }
        request
    }

    /// Asks for `ask`. What the model writes is handed to `wrote` a few
    /// words at a time, in order. Ends early, without an error, once `stop`
    /// is set.
    ///
    /// # Errors
    ///
    /// Returns why nothing was written, or why it stopped.
    pub fn write(
        &self,
        ask: &Ask<'_>,
        stop: &AtomicBool,
        wrote: &mut dyn FnMut(&str),
    ) -> Result<(), Failed> {
        let body = self.request(ask).to_string();
        let bearer = self.key.as_ref().map(|key| format!("Bearer {key}"));
        let headers: Vec<(&str, &str)> = bearer
            .iter()
            .map(|bearer| ("Authorization", bearer.as_str()))
            .collect();
        let mut response = Request {
            json: Some(&body),
            headers: &headers,
            connect_timeout: CONNECT,
            stall_timeout: SILENCE,
            ..Request::get(&self.address, CONNECT)
        }
        .send()
        .map_err(|_| Failed::Unreachable)?;
        let status = response.status();
        if !(200..300).contains(&status) {
            return Err(Failed::Status(status));
        }
        let mut aloud = Aloud::default();
        let mut said = false;
        let read = events(&mut response, stop, &mut |data| {
            if data == "[DONE]" {
                return false;
            }
            let Ok(event) = serde_json::from_str::<Value>(data) else {
                return true;
            };
            if let Some(more) = event["choices"][0]["delta"]["content"].as_str() {
                let more = aloud.push(more);
                if !more.is_empty() {
                    said = true;
                    wrote(&more);
                }
            }
            true
        });
        read.map_err(|_| Failed::Stalled)?;
        if said || stop.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(Failed::Nothing)
        }
    }
}

/// A server on this computer that answers one request, for the tests of
/// whoever asks a model.
#[cfg(any(test, feature = "fake", feature = "test-instance"))]
#[allow(missing_docs, clippy::pedantic)]
pub mod testing {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    /// A server on this computer that answers one request with `answer` and
    /// hands back what it was asked.
    pub fn serve_once(answer: String) -> (String, mpsc::Receiver<String>) {
        let (asked, requests) = mpsc::channel();
        let Ok(listener) = TcpListener::bind("127.0.0.1:0") else {
            return (String::new(), requests);
        };
        let port = listener.local_addr().map_or(0, |address| address.port());
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            // The head, then as much body as it says it has.
            loop {
                let Ok(read) = stream.read(&mut buffer) else {
                    return;
                };
                request.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&request).into_owned();
                let Some(head) = text.find("\r\n\r\n") else {
                    if read == 0 {
                        break;
                    }
                    continue;
                };
                let length = text[..head]
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())?
                    })
                    .unwrap_or(0);
                if request.len() >= head + 4 + length || read == 0 {
                    break;
                }
            }
            let _ = asked.send(String::from_utf8_lossy(&request).into_owned());
            let _ = stream.write_all(answer.as_bytes());
        });
        (format!("http://127.0.0.1:{port}"), requests)
    }

    /// An answer that streams `events`, each as the data of one event.
    pub fn stream_of(events: &[&str]) -> String {
        let mut body = String::new();
        for event in events {
            body.push_str("data: ");
            body.push_str(event);
            body.push_str("\n\n");
        }
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{serve_once, stream_of};
    use super::*;

    const ASK: Ask<'static> = Ask {
        system: "Do it.",
        user: "A long text.",
        temperature: 0.3,
        max_tokens: 220,
    };

    fn written(chat: &Chat) -> Result<String, Failed> {
        let mut whole = String::new();
        let stop = AtomicBool::new(false);
        chat.write(&ASK, &stop, &mut |more| whole.push_str(more))?;
        Ok(whole)
    }

    #[test]
    fn the_address_of_a_server_is_completed_whatever_part_of_it_was_given() {
        for given in [
            "http://127.0.0.1:1234",
            "http://127.0.0.1:1234/",
            "http://127.0.0.1:1234/v1",
            " http://127.0.0.1:1234/v1/ ",
            "http://127.0.0.1:1234/v1/chat/completions",
        ] {
            assert_eq!(
                chat_address(given),
                "http://127.0.0.1:1234/v1/chat/completions",
                "{given}"
            );
        }
    }

    #[test]
    fn events_are_cut_at_their_lines_however_the_bytes_arrive() {
        let mut lines = Lines::default();
        let mut seen = Vec::new();
        let mut note = |data: &str| {
            seen.push(data.to_owned());
            data != "stop"
        };
        // An event in two pieces, a line that is not data, two in one.
        assert_eq!(lines.push(b"data: one", &mut note), Ok(true));
        assert_eq!(lines.push(b" two\n\n: a comment\n", &mut note), Ok(true));
        assert_eq!(
            lines.push(b"data:three\r\ndata: four\n", &mut note),
            Ok(true)
        );
        // Whoever reads says when it is over; the rest is not handed on.
        assert_eq!(
            lines.push(b"data: stop\ndata: five\n", &mut note),
            Ok(false)
        );
        assert_eq!(seen, ["one two", "three", "four", "stop"]);
        // A line without an end is not held for ever.
        let mut endless = Lines::default();
        let long = vec![b'x'; MAX_LINE + 1];
        assert!(endless.push(&long, &mut |_| true).is_err());
    }

    #[test]
    fn a_chat_server_is_asked_to_stream_and_its_words_are_handed_on_in_order() {
        let delta =
            |text: &str| json!({ "choices": [{ "delta": { "content": text } }] }).to_string();
        let events = [
            delta("The report "),
            delta("is short."),
            "[DONE]".to_owned(),
            delta(" Not this."),
        ];
        let events: Vec<&str> = events.iter().map(String::as_str).collect();
        let (base, requests) = serve_once(stream_of(&events));
        let server = Chat::of_the_user(&base, " my-model ");
        assert_eq!(written(&server).as_deref(), Ok("The report is short."));
        let asked = requests
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_default();
        assert!(
            asked.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"),
            "{asked}"
        );
        assert!(!asked.to_ascii_lowercase().contains("authorization:"));
        let body = asked.split("\r\n\r\n").nth(1).unwrap_or_default();
        let body: Value = serde_json::from_str(body).unwrap_or_default();
        assert_eq!(body["model"], "my-model");
        assert_eq!(body["stream"], true);
        assert_eq!(body["max_tokens"], 220);
        assert_eq!(body["messages"][0]["content"], "Do it.");
        assert_eq!(body["messages"][1]["content"], "A long text.");
        // A server of someone's own is told nothing it may not understand.
        assert!(body.get("chat_template_kwargs").is_none());
    }

    #[test]
    fn the_engine_of_this_pc_is_asked_with_its_key_and_not_to_think() {
        let delta = json!({ "choices": [{ "delta": { "content": "Kratko." } }] }).to_string();
        let (base, requests) = serve_once(stream_of(&[&delta]));
        let engine = Chat::engine(&base, "k3y");
        assert_eq!(written(&engine).as_deref(), Ok("Kratko."));
        let asked = requests
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_default();
        assert!(asked.contains("Authorization: Bearer k3y\r\n"), "{asked}");
        let body = asked.split("\r\n\r\n").nth(1).unwrap_or_default();
        let body: Value = serde_json::from_str(body).unwrap_or_default();
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false);
        assert!(!format!("{engine:?}").contains("k3y"));
    }

    #[test]
    fn what_a_model_thinks_to_itself_is_not_said_aloud() {
        let mut aloud = Aloud::default();
        let parts = [
            "<thi",
            "nk>Let me see",
            ".</th",
            "ink>The ",
            "report.",
            " <",
            "b>",
        ];
        let said: String = parts.iter().map(|part| aloud.push(part)).collect();
        assert_eq!(said, "The report. <b>");
        let mut plain = Aloud::default();
        assert_eq!(plain.push("No marks here."), "No marks here.");
    }

    #[test]
    fn a_server_that_refuses_or_writes_nothing_says_which() {
        let refused = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let (base, _requests) = serve_once(refused.to_owned());
        let server = Chat::of_the_user(&base, "gone");
        assert_eq!(written(&server), Err(Failed::Status(404)));
        let (base, _requests) = serve_once(stream_of(&["[DONE]"]));
        let server = Chat::of_the_user(&base, "quiet");
        assert_eq!(written(&server), Err(Failed::Nothing));
        // Nothing listens here.
        let nobody = Chat::of_the_user("http://127.0.0.1:9", "x");
        assert_eq!(written(&nobody), Err(Failed::Unreachable));
    }

    #[test]
    fn an_answer_that_is_stopped_ends_without_an_error() {
        let delta = json!({ "choices": [{ "delta": { "content": "One." } }] }).to_string();
        let (base, _requests) = serve_once(stream_of(&[&delta, &delta, &delta]));
        let server = Chat::of_the_user(&base, "m");
        let stop = AtomicBool::new(true);
        let mut count = 0;
        let done = server.write(&ASK, &stop, &mut |_| count += 1);
        assert_eq!((done, count), (Ok(()), 0));
    }
}
