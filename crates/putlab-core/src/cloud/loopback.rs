//! Where the browser comes back to at the end of a sign-in: a port of this
//! computer that only this process listens on.
//!
//! The server sends the browser on to `http://127.0.0.1:<port>/callback`
//! with a one-time code, or with a word for what went wrong, and with the
//! `state` this process made up for the sign-in. That address reaches this
//! computer and no other, which is what makes a code to type unnecessary:
//! whoever signs in with Google in a browser signs in the copy of Vinput
//! on the computer that browser runs on, and nobody else's.
//!
//! The listener is bound to `127.0.0.1` alone, so nothing on the network
//! reaches it and Windows asks no firewall question. It speaks just enough
//! HTTP to read one request line and to answer with a redirect, and takes
//! one request for the sign-in it was made for; everything else that
//! knocks is told there is nothing here.

use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::{Failure, STOP_STEP};

/// The path the browser is sent to.
const PATH: &str = "/callback";
/// How long whoever connected may take to say what it wants. A browser
/// says it at once; a connection it opened ahead of need says nothing.
const HEAD_WAIT: Duration = Duration::from_secs(2);
/// The longest request that is read. The one that is waited for is a few
/// hundred bytes with everything a browser adds.
const HEAD_LIMIT: usize = 8 * 1024;
/// The longest code or word that is taken out of the address.
const VALUE_LIMIT: usize = 128;

/// The port this process listens on for the end of one sign-in.
#[derive(Debug)]
pub(super) struct Listener {
    socket: TcpListener,
    port: u16,
}

/// What the browser came back with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Arrived {
    /// The one-time code to trade for a session.
    Code(String),
    /// The server's word for why nobody is signed in.
    Error(String),
}

/// The browser, waiting to be told where to go next.
#[derive(Debug)]
pub(super) struct Caller(TcpStream);

impl Listener {
    /// Listens on a free port of this computer.
    ///
    /// # Errors
    ///
    /// Returns why no port could be listened on.
    pub(super) fn bind() -> std::io::Result<Self> {
        let socket = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        // So that waiting can be called off.
        socket.set_nonblocking(true)?;
        let port = socket.local_addr()?.port();
        Ok(Self { socket, port })
    }

    /// The port that is listened on.
    pub(super) fn port(&self) -> u16 {
        self.port
    }

    /// Waits for the browser to arrive with the outcome of the sign-in that
    /// `state` belongs to.
    ///
    /// # Errors
    ///
    /// Returns [`Failure::Stopped`] once `stop` is set and
    /// [`Failure::Expired`] when `limit` has passed.
    pub(super) fn wait(
        &self,
        state: &str,
        limit: Duration,
        stop: &AtomicBool,
    ) -> Result<(Arrived, Caller), Failure> {
        let started = Instant::now();
        loop {
            if stop.load(Ordering::Relaxed) {
                return Err(Failure::Stopped);
            }
            if started.elapsed() > limit {
                return Err(Failure::Expired);
            }
            match self.socket.accept() {
                Ok((stream, _)) => {
                    if let Some(arrived) = receive(stream, state) {
                        return Ok(arrived);
                    }
                }
                // Nobody yet, or a connection that failed while it was
                // made: either way the wait goes on.
                Err(_) => std::thread::sleep(STOP_STEP),
            }
        }
    }
}

impl Caller {
    /// Sends the browser on to `address`, and lets go of it.
    pub(super) fn send_to(mut self, address: &str) {
        let answer = format!(
            "HTTP/1.1 302 Found\r\nLocation: {address}\r\nCache-Control: no-store\r\n\
             Referrer-Policy: no-referrer\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        // A browser that has gone away is not told anything, and need not be.
        let _ = self.0.write_all(answer.as_bytes());
        let _ = self.0.shutdown(Shutdown::Both);
    }
}

/// Reads what `stream` asks for. If it is the outcome of the sign-in that
/// `state` belongs to, returns it with the browser still waiting; anything
/// else is answered here and let go of.
fn receive(mut stream: TcpStream, state: &str) -> Option<(Arrived, Caller)> {
    // A connection taken from a listener that does not wait does not wait
    // either, on Windows.
    stream.set_nonblocking(false).ok()?;
    stream.set_read_timeout(Some(HEAD_WAIT)).ok()?;
    stream.set_write_timeout(Some(HEAD_WAIT)).ok()?;
    let head = read_head(&mut stream)?;
    if let Some(arrived) = outcome(&head, state) {
        return Some((arrived, Caller(stream)));
    }
    let _ = stream.write_all(
        b"HTTP/1.1 404 Not Found\r\nCache-Control: no-store\r\nContent-Length: 0\r\n\
          Connection: close\r\n\r\n",
    );
    let _ = stream.shutdown(Shutdown::Both);
    None
}

/// The head of the request on `stream`: everything up to the empty line.
fn read_head(stream: &mut TcpStream) -> Option<String> {
    let mut head = Vec::new();
    let mut buffer = [0_u8; 1024];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        if head.len() > HEAD_LIMIT {
            return None;
        }
        match stream.read(&mut buffer) {
            Ok(0) => return None,
            Ok(read) => head.extend_from_slice(buffer.get(..read)?),
            Err(err) if err.kind() == ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    String::from_utf8(head).ok()
}

/// What a request with this `head` says came of the sign-in that `state`
/// belongs to; `None` if it is not about that sign-in at all.
fn outcome(head: &str, state: &str) -> Option<Arrived> {
    let mut line = head.lines().next()?.split(' ');
    let (method, target) = (line.next()?, line.next()?);
    if method != "GET" {
        return None;
    }
    let query = target.strip_prefix(PATH)?.strip_prefix('?')?;
    let (mut code, mut error, mut theirs) = (None, None, None);
    for pair in query.split('&') {
        let (name, value) = pair.split_once('=')?;
        let slot = match name {
            "code" => &mut code,
            "error" => &mut error,
            "state" => &mut theirs,
            _ => return None,
        };
        // Named twice, or holding anything the server never writes: not
        // the address the server sent the browser to.
        if slot.is_some() || !is_plain(value) {
            return None;
        }
        *slot = Some(value);
    }
    if theirs != Some(state) {
        return None;
    }
    match (code, error) {
        (Some(code), None) => Some(Arrived::Code(code.to_owned())),
        (None, Some(error)) => Some(Arrived::Error(error.to_owned())),
        _ => None,
    }
}

/// Whether `value` is made of what the server puts in the address:
/// base64url, or a word.
fn is_plain(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= VALUE_LIMIT
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

#[cfg(test)]
mod tests {
    use super::*;

    type Outcome = Result<(), String>;

    /// Asks the listener on `port` for `target` as a browser would, and
    /// returns its whole answer.
    fn ask(port: u16, target: &str) -> std::io::Result<String> {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        let request = format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n");
        stream.write_all(request.as_bytes())?;
        let mut answer = String::new();
        stream.read_to_string(&mut answer)?;
        Ok(answer)
    }

    fn head(target: &str) -> String {
        format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1:50123\r\nAccept: */*\r\n\r\n")
    }

    #[test]
    fn the_outcome_is_read_from_the_address_the_server_sends_the_browser_to() {
        assert_eq!(
            outcome(&head("/callback?code=abc-DEF_123&state=mine"), "mine"),
            Some(Arrived::Code("abc-DEF_123".to_owned()))
        );
        assert_eq!(
            outcome(&head("/callback?error=cancelled&state=mine"), "mine"),
            Some(Arrived::Error("cancelled".to_owned()))
        );
        // The order does not matter.
        assert_eq!(
            outcome(&head("/callback?state=mine&code=abc"), "mine"),
            Some(Arrived::Code("abc".to_owned()))
        );
    }

    #[test]
    fn nothing_else_is_taken_for_the_outcome() {
        let long = format!("/callback?code={}&state=mine", "a".repeat(129));
        for target in [
            "/",
            "/favicon.ico",
            "/callback",
            "/callback?",
            "/callback?code=abc",
            "/callback?state=mine",
            // Another sign-in's, or nobody's.
            "/callback?code=abc&state=theirs",
            "/callback?code=abc&state=",
            "/callback?code=abc&state=mine&state=theirs",
            "/callback?code=abc&code=def&state=mine",
            // Both a code and an error, or neither of them filled in.
            "/callback?code=abc&error=cancelled&state=mine",
            "/callback?code=&state=mine",
            // Anything the server does not write.
            "/callback?code=abc&state=mine&extra=1",
            "/callback?code=a%62c&state=mine",
            "/callback?code=a+b&state=mine",
            "/callback?code=abc&state=mine#x",
            "/callback/?code=abc&state=mine",
            "/callbacks?code=abc&state=mine",
            "/other?code=abc&state=mine",
            "http://127.0.0.1/callback?code=abc&state=mine",
            long.as_str(),
        ] {
            assert_eq!(outcome(&head(target), "mine"), None, "{target}");
        }
        let posted = "POST /callback?code=abc&state=mine HTTP/1.1\r\n\r\n";
        assert_eq!(outcome(posted, "mine"), None);
        assert_eq!(outcome("", "mine"), None);
    }

    #[test]
    fn the_browser_is_waited_for_and_sent_on() -> Outcome {
        let listener = Listener::bind().map_err(|err| err.to_string())?;
        let port = listener.port();
        let browser = std::thread::spawn(move || {
            // What knocks first is not the browser with the outcome.
            let stray = ask(port, "/favicon.ico");
            let wrong = ask(port, "/callback?code=abc&state=theirs");
            let right = ask(port, "/callback?code=abc&state=mine");
            (stray, wrong, right)
        });
        let stop = AtomicBool::new(false);
        let (arrived, caller) = listener
            .wait("mine", Duration::from_secs(20), &stop)
            .map_err(|err| err.to_string())?;
        assert_eq!(arrived, Arrived::Code("abc".to_owned()));
        caller.send_to("http://127.0.0.1:1/v1/auth/app/done?result=signed_in");

        let (stray, wrong, right) = browser.join().map_err(|_| "the browser panicked")?;
        for answer in [stray, wrong] {
            let answer = answer.map_err(|err| err.to_string())?;
            assert!(answer.starts_with("HTTP/1.1 404 "), "{answer}");
        }
        let right = right.map_err(|err| err.to_string())?;
        assert!(right.starts_with("HTTP/1.1 302 "), "{right}");
        assert!(
            right
                .contains("\r\nLocation: http://127.0.0.1:1/v1/auth/app/done?result=signed_in\r\n")
        );
        assert!(right.contains("\r\nCache-Control: no-store\r\n"));
        Ok(())
    }

    #[test]
    fn a_wait_ends_when_it_is_called_off_or_its_time_is_up() -> Outcome {
        let listener = Listener::bind().map_err(|err| err.to_string())?;
        assert!(listener.port() >= 1024);
        let stopped = AtomicBool::new(true);
        assert_eq!(
            listener
                .wait("mine", Duration::from_secs(20), &stopped)
                .map(drop),
            Err(Failure::Stopped)
        );
        let going = AtomicBool::new(false);
        assert_eq!(
            listener.wait("mine", Duration::ZERO, &going).map(drop),
            Err(Failure::Expired)
        );
        // A connection that says nothing does not hold the wait up for
        // longer than it is given to speak.
        let silent = TcpStream::connect((Ipv4Addr::LOCALHOST, listener.port()));
        let silent = silent.map_err(|err| err.to_string())?;
        let began = Instant::now();
        assert_eq!(
            listener
                .wait("mine", Duration::from_millis(200), &going)
                .map(drop),
            Err(Failure::Expired)
        );
        assert!(began.elapsed() < HEAD_WAIT + Duration::from_secs(5));
        drop(silent);
        Ok(())
    }
}
