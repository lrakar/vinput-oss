//! The named pipe that carries the local API.
//!
//! One thread runs every connection, asleep unless a client is talking, so the
//! API costs nothing while idle. The pipe is created for the signed-in user
//! only: other accounts on the machine and other machines cannot open it.
#![allow(unsafe_code)]

use std::io;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::{Notify, broadcast, mpsc as channel};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use super::{Command, ErrorCode, Event, MAX_CLIENTS, MAX_LINE};
use crate::log;
use crate::session::Target;

/// Events a slow client may fall behind by before it starts missing them.
const EVENT_BACKLOG: usize = 64;
/// Replies meant for one client that may wait for it to read them.
const REPLY_BACKLOG: usize = 16;
/// How long a client may leave a line unread before it is disconnected, so
/// one that stopped reading does not hold its place forever.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// What a client is listed as until it says its name.
const UNNAMED: &str = "Unnamed program";
/// How often the name of the pipe is asked for before giving up, when
/// somebody else has it. A copy of Vinput that is on its way out, or an API
/// that was switched off a moment ago, lets go of it within moments.
const CLAIM_TRIES: u32 = 30;
/// How long to wait between two of those tries.
const CLAIM_WAIT: Duration = Duration::from_millis(100);

/// One connection to the API, for as long as it lasts. Numbers are not reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClientId(u64);

/// A command and the client that sent it, so an answer can go back to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// Who sent it.
    pub from: ClientId,
    /// What it asks for.
    pub command: Command,
}

/// The running API: sends events to every client and collects their commands.
/// Dropping it disconnects every client and closes the pipe.
pub struct Server {
    shared: Arc<Shared>,
    requests: mpsc::Receiver<Request>,
}

/// A connected client as the server keeps it.
struct Entry {
    id: ClientId,
    /// The name it gave, for Settings.
    name: String,
    /// Lines for this client only.
    replies: channel::Sender<Arc<str>>,
}

struct Shared {
    events: broadcast::Sender<Arc<str>>,
    requests: mpsc::Sender<Request>,
    /// Called after a command is queued, to wake whoever reads them.
    wake: Box<dyn Fn() + Send + Sync>,
    /// The mode of the running session, for the greeting of new clients.
    session: Mutex<Option<Target>>,
    /// The connected clients, in the order they connected.
    clients: Mutex<Vec<Entry>>,
    next_client: AtomicU64,
    /// Signalled when the server is dropped.
    shutdown: Notify,
}

/// Locks a mutex whose data stays usable even if a holder panicked.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server").finish_non_exhaustive()
    }
}

impl Server {
    /// Starts serving the API on [`super::PIPE_NAME`], or on the pipe of this copy if
    /// it is a separate one (see [`crate::instance`]). `wake` is called, on another
    /// thread, each time a command is ready to be read with
    /// [`requests`](Self::requests).
    ///
    /// # Errors
    ///
    /// Returns an error if the serving thread cannot be started. A pipe that
    /// cannot be created is logged from that thread instead: Vinput works
    /// without its API.
    pub fn start(wake: impl Fn() + Send + Sync + 'static) -> io::Result<Self> {
        Self::start_on(&crate::instance::pipe_name(), wake)
    }

    /// Starts serving on a pipe of another name, for tests and tools.
    ///
    /// # Errors
    ///
    /// Returns an error if the serving thread cannot be started.
    pub fn start_on(name: &str, wake: impl Fn() + Send + Sync + 'static) -> io::Result<Self> {
        let (events, _) = broadcast::channel(EVENT_BACKLOG);
        let (requests_tx, requests) = mpsc::channel();
        let shared = Arc::new(Shared {
            events,
            requests: requests_tx,
            wake: Box::new(wake),
            session: Mutex::default(),
            clients: Mutex::default(),
            next_client: AtomicU64::new(1),
            shutdown: Notify::new(),
        });
        let serving = Arc::clone(&shared);
        let name = name.to_owned();
        thread::Builder::new()
            .name("vinput-api".into())
            .spawn(move || {
                let served = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .and_then(|runtime| runtime.block_on(serve(&name, serving)));
                if let Err(err) = served {
                    log::write(&format!("the local API stopped: {err}"));
                }
            })?;
        Ok(Self { shared, requests })
    }

    /// Sends an event to every connected client.
    pub fn send(&self, event: Event<'_>) {
        if let Event::State { session, .. } = event {
            *lock(&self.shared.session) = session;
        }
        // Fails only when nobody is connected, which is the usual case.
        let _ = self.shared.events.send(event.to_line().into());
    }

    /// Sends an event to one client, such as the answer to its command. It is
    /// dropped if that client has gone, or has stopped reading.
    pub fn reply(&self, to: ClientId, event: Event<'_>) {
        if let Some(client) = lock(&self.shared.clients)
            .iter()
            .find(|client| client.id == to)
        {
            let _ = client.replies.try_send(event.to_line().into());
        }
    }

    /// The commands clients have sent since the last call, oldest first.
    pub fn requests(&self) -> impl Iterator<Item = Request> + '_ {
        self.requests.try_iter()
    }

    /// The names of the programs connected right now, in the order they
    /// connected.
    #[must_use]
    pub fn clients(&self) -> Vec<String> {
        lock(&self.shared.clients)
            .iter()
            .map(|client| client.name.clone())
            .collect()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // The serving thread returns, and its runtime takes every connection
        // down with it.
        self.shared.shutdown.notify_one();
    }
}

async fn serve(name: &str, shared: Arc<Shared>) -> io::Result<()> {
    let access = UserOnly::new()?;
    let Some(mut pipe) = claim(&access, name, &shared).await? else {
        return Ok(());
    };
    loop {
        tokio::select! {
            connected = pipe.connect() => connected?,
            () = shared.shutdown.notified() => return Ok(()),
        }
        // The next instance exists before this client is served, so another
        // client never finds the pipe missing.
        let connected = std::mem::replace(&mut pipe, access.create_pipe(name, false)?);
        tokio::spawn(talk(connected, Arc::clone(&shared)));
    }
}

/// Creates the first instance of the pipe, which makes its name ours. While
/// somebody else has the name it is asked for again for a few seconds.
/// `None` if the server was dropped while waiting.
async fn claim(
    access: &UserOnly,
    name: &str,
    shared: &Shared,
) -> io::Result<Option<NamedPipeServer>> {
    let mut tries = 1;
    loop {
        match access.create_pipe(name, true) {
            Ok(pipe) => return Ok(Some(pipe)),
            Err(err) if tries >= CLAIM_TRIES => return Err(err),
            Err(_) => tries += 1,
        }
        tokio::select! {
            () = tokio::time::sleep(CLAIM_WAIT) => {}
            () = shared.shutdown.notified() => return Ok(None),
        }
    }
}

/// Lists a client for as long as it is connected.
struct Listed<'a> {
    shared: &'a Shared,
    id: ClientId,
}

impl<'a> Listed<'a> {
    /// Lists a new client and returns where its replies arrive, or `None` if
    /// there is no room for it.
    fn new(shared: &'a Shared) -> Option<(Self, channel::Receiver<Arc<str>>)> {
        let mut clients = lock(&shared.clients);
        if clients.len() >= MAX_CLIENTS {
            return None;
        }
        let id = ClientId(shared.next_client.fetch_add(1, Ordering::Relaxed));
        let (replies, received) = channel::channel(REPLY_BACKLOG);
        clients.push(Entry {
            id,
            name: UNNAMED.to_owned(),
            replies,
        });
        Some((Self { shared, id }, received))
    }

    fn rename(&self, name: String) {
        if name.is_empty() {
            return;
        }
        if let Some(client) = lock(&self.shared.clients)
            .iter_mut()
            .find(|client| client.id == self.id)
        {
            client.name = name;
        }
    }
}

impl Drop for Listed<'_> {
    fn drop(&mut self) {
        lock(&self.shared.clients).retain(|client| client.id != self.id);
    }
}

/// Serves one client until it disconnects.
async fn talk(pipe: NamedPipeServer, shared: Arc<Shared>) {
    let (reader, mut writer) = tokio::io::split(pipe);
    let mut write = async |line: &str| {
        let mut bytes = Vec::with_capacity(line.len() + 1);
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
        match tokio::time::timeout(WRITE_TIMEOUT, writer.write_all(&bytes)).await {
            Ok(written) => written,
            Err(_) => Err(io::ErrorKind::TimedOut.into()),
        }
    };
    let Some((listed, mut replies)) = Listed::new(&shared) else {
        let full = Event::Error {
            code: ErrorCode::TooManyClients,
            message: &format!("Vinput serves at most {MAX_CLIENTS} programs at once"),
            id: None,
        };
        let _ = write(&full.to_line()).await;
        return;
    };
    let mut lines = Lines::new(reader);
    // Subscribed before the greeting, so no state change falls between the two.
    let mut events = shared.events.subscribe();
    let hello = Event::Hello {
        session: *lock(&shared.session),
    };
    if write(&hello.to_line()).await.is_err() {
        return;
    }
    loop {
        let sent = tokio::select! {
            line = lines.next() => match line {
                Ok(Some(Line::Text(line))) if line.trim().is_empty() => Ok(()),
                Ok(Some(Line::Text(line))) => match Command::parse(&line) {
                    Ok(Command::Hello { name }) => {
                        listed.rename(name);
                        Ok(())
                    }
                    Ok(command) => {
                        let request = Request { from: listed.id, command };
                        if shared.requests.send(request).is_ok() {
                            (shared.wake)();
                        }
                        Ok(())
                    }
                    Err(refusal) => write(&refusal.event().to_line()).await,
                },
                Ok(Some(Line::TooLong)) => {
                    let refusal = Event::Error {
                        code: ErrorCode::LineTooLong,
                        message: &format!("a line may be at most {MAX_LINE} bytes; skipped to the next line"),
                        id: None,
                    };
                    write(&refusal.to_line()).await
                }
                Ok(Some(Line::NotUtf8)) => {
                    let refusal = Event::Error {
                        code: ErrorCode::InvalidJson,
                        message: "not UTF-8 text",
                        id: None,
                    };
                    write(&refusal.to_line()).await
                }
                // The client closed its end or the pipe broke.
                Ok(None) | Err(_) => return,
            },
            event = events.recv() => match event {
                Ok(line) => write(&line).await,
                // A client too slow to keep up misses events but stays connected.
                Err(broadcast::error::RecvError::Lagged(_)) => Ok(()),
                Err(broadcast::error::RecvError::Closed) => return,
            },
            reply = replies.recv() => match reply {
                Some(line) => write(&line).await,
                None => return,
            },
        };
        if sent.is_err() {
            return;
        }
    }
}

/// One line read from a client.
#[derive(Debug, PartialEq, Eq)]
enum Line {
    /// A line of text, without its line break.
    Text(String),
    /// A line longer than [`MAX_LINE`]. The rest of it is skipped.
    TooLong,
    /// A line that is not UTF-8.
    NotUtf8,
}

/// Splits what a client sends into lines, holding at most [`MAX_LINE`] bytes
/// of a line at a time. Lines end with `\n` or `\r\n`.
struct Lines<R> {
    reader: R,
    /// Bytes read but not yet returned as a line.
    pending: Vec<u8>,
    /// Bytes at the start of `pending` known to hold no line break.
    searched: usize,
    /// Whether the line being read was too long and is being skipped.
    skipping: bool,
}

impl<R: AsyncRead + Unpin> Lines<R> {
    fn new(reader: R) -> Self {
        Self {
            reader,
            pending: Vec::new(),
            searched: 0,
            skipping: false,
        }
    }

    /// The next line, or `None` once the client has closed its end.
    ///
    /// Safe to cancel, as in `select!`: nothing read is lost, since `pending`
    /// changes only after a read has finished.
    async fn next(&mut self) -> io::Result<Option<Line>> {
        let mut chunk = [0; 4096];
        loop {
            if let Some(at) = self.pending[self.searched..]
                .iter()
                .position(|&byte| byte == b'\n')
            {
                let end = self.searched + at;
                let mut line: Vec<u8> = self.pending.drain(..=end).collect();
                self.searched = 0;
                if std::mem::take(&mut self.skipping) {
                    // Its refusal was sent when it grew too long.
                    continue;
                }
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                // The line break can come in the read that takes it past the
                // limit.
                if line.len() > MAX_LINE {
                    return Ok(Some(Line::TooLong));
                }
                return Ok(Some(text(line)));
            }
            self.searched = self.pending.len();
            if self.pending.len() > MAX_LINE {
                self.pending.clear();
                self.searched = 0;
                if !std::mem::replace(&mut self.skipping, true) {
                    return Ok(Some(Line::TooLong));
                }
            }
            let read = self.reader.read(&mut chunk).await?;
            if read == 0 {
                // A last line without a line break still counts.
                if self.pending.is_empty() || std::mem::take(&mut self.skipping) {
                    self.pending.clear();
                    return Ok(None);
                }
                self.searched = 0;
                return Ok(Some(text(std::mem::take(&mut self.pending))));
            }
            self.pending.extend_from_slice(&chunk[..read]);
        }
    }
}

fn text(line: Vec<u8>) -> Line {
    String::from_utf8(line).map_or(Line::NotUtf8, Line::Text)
}

/// Security settings that let only the signed-in user (and the system) open a pipe.
struct UserOnly {
    descriptor: PSECURITY_DESCRIPTOR,
}

impl UserOnly {
    fn new() -> io::Result<Self> {
        let sddl: Vec<u16> = format!("D:P(A;;GA;;;{})(A;;GA;;;SY)", current_user_sid()?)
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut descriptor = null_mut();
        // SAFETY: `sddl` is NUL-terminated and `descriptor` is writable. On
        // success the descriptor is owned by `Self` and freed when it drops.
        let converted = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &raw mut descriptor,
                null_mut(),
            )
        };
        if converted == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { descriptor })
    }

    fn create_pipe(&self, name: &str, first: bool) -> io::Result<NamedPipeServer> {
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(0),
            lpSecurityDescriptor: self.descriptor,
            bInheritHandle: 0,
        };
        let mut options = ServerOptions::new();
        // Claiming the first instance fails if another program already owns the
        // name, instead of quietly sharing it.
        options
            .first_pipe_instance(first)
            .reject_remote_clients(true);
        // SAFETY: `attributes` is a valid `SECURITY_ATTRIBUTES` whose descriptor
        // lives as long as `self`; the system copies both during the call.
        unsafe { options.create_with_security_attributes_raw(name, (&raw mut attributes).cast()) }
    }
}

impl Drop for UserOnly {
    fn drop(&mut self) {
        // SAFETY: the descriptor was allocated by the system for `new` and is
        // freed exactly once.
        unsafe { LocalFree(self.descriptor) };
    }
}

/// The signed-in user's security identifier in its text form, `S-1-5-21-…`.
fn current_user_sid() -> io::Result<String> {
    /// Closes the token on every way out.
    struct Token(HANDLE);

    impl Drop for Token {
        fn drop(&mut self) {
            // SAFETY: the handle came from `OpenProcessToken` and is closed once.
            unsafe { CloseHandle(self.0) };
        }
    }

    let mut token = Token(null_mut());
    // SAFETY: the pseudo-handle of the current process is always valid and
    // `token.0` is writable.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token.0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // A `TOKEN_USER` is followed by the identifier it points to; this is room
    // for the largest one, aligned for the structure.
    let mut user = [0u64; 16];
    let mut written = 0;
    // SAFETY: the buffer is writable for the size passed and aligned for
    // `TOKEN_USER`.
    let read = unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            user.as_mut_ptr().cast(),
            u32::try_from(size_of_val(&user)).unwrap_or(0),
            &raw mut written,
        )
    };
    if read == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut text = null_mut();
    // SAFETY: the call above filled the buffer with a `TOKEN_USER` whose
    // identifier points into the same buffer, which is still alive.
    let converted = unsafe {
        let user = &*user.as_ptr().cast::<TOKEN_USER>();
        ConvertSidToStringSidW(user.User.Sid, &raw mut text)
    };
    if converted == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: on success `text` is a NUL-terminated string allocated by the
    // system, read before it is freed exactly once.
    let sid = unsafe {
        let mut length = 0;
        while *text.add(length) != 0 {
            length += 1;
        }
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
        LocalFree(text.cast());
        sid
    };
    Ok(sid)
}

#[cfg(test)]
mod tests {
    use std::fs::{File, OpenOptions};
    use std::io::{BufRead, BufReader, Write};

    use serde_json::Value;

    use super::*;

    /// Opens the pipe as a client would, waiting for the server to create it.
    fn connect(name: &str) -> io::Result<File> {
        let mut tries = 0;
        loop {
            match OpenOptions::new().read(true).write(true).open(name) {
                Ok(pipe) => return Ok(pipe),
                Err(_) if tries < 100 => {
                    tries += 1;
                    thread::sleep(Duration::from_millis(20));
                }
                Err(err) => return Err(err),
            }
        }
    }

    /// A blocking test client that takes turns writing and reading.
    struct TestClient {
        pipe: File,
        replies: BufReader<File>,
    }

    impl TestClient {
        fn connect(name: &str) -> io::Result<Self> {
            let pipe = connect(name)?;
            let replies = BufReader::new(pipe.try_clone()?);
            Ok(Self { pipe, replies })
        }

        fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
            self.pipe.write_all(bytes)
        }

        /// The next line, without its line break; empty once the pipe closed.
        fn line(&mut self) -> io::Result<String> {
            let mut line = String::new();
            self.replies.read_line(&mut line)?;
            Ok(line.trim_end().to_owned())
        }

        /// The next line as JSON.
        fn event(&mut self) -> io::Result<Value> {
            let line = self.line()?;
            serde_json::from_str(&line).map_err(|err| io::Error::other(format!("{line:?}: {err}")))
        }
    }

    /// Connects until a server greets, as a client does that finds the pipe
    /// missing or closing.
    fn greeted(name: &str) -> io::Result<TestClient> {
        let mut tries = 0;
        loop {
            let hello = TestClient::connect(name).and_then(|mut client| {
                let hello = client.event()?;
                Ok((client, hello))
            });
            match hello {
                Ok((client, hello)) if field(&hello, "event") == Some("hello") => {
                    return Ok(client);
                }
                Ok(_) | Err(_) if tries < 100 => {
                    tries += 1;
                    thread::sleep(Duration::from_millis(20));
                }
                Ok(_) => return Err(io::Error::other("no greeting")),
                Err(err) => return Err(err),
            }
        }
    }

    fn test_pipe(test: &str) -> String {
        format!(r"\\.\pipe\vinput-test-{test}-{}", std::process::id())
    }

    fn field<'a>(event: &'a Value, name: &str) -> Option<&'a str> {
        event.get(name).and_then(Value::as_str)
    }

    #[test]
    fn a_server_waits_for_the_name_while_another_is_letting_go_of_it() -> io::Result<()> {
        let name = test_pipe("claim");
        let first = Server::start_on(&name, || {})?;
        drop(greeted(&name)?);

        // The name is taken: the second server keeps asking for it.
        let (woken_tx, woken) = mpsc::channel();
        let second = Server::start_on(&name, move || {
            let _ = woken_tx.send(());
        })?;
        thread::sleep(CLAIM_WAIT * 2);
        drop(first);
        // Long enough for the second to have asked again, so that the client
        // does not catch the first on its way out.
        thread::sleep(CLAIM_WAIT * 3);

        let mut client = greeted(&name)?;
        client.send(
            b"{\"command\":\"stop\"}
",
        )?;
        assert!(woken.recv_timeout(Duration::from_secs(5)).is_ok());
        assert_eq!(second.requests().count(), 1);
        Ok(())
    }

    #[test]
    fn a_client_is_greeted_sends_commands_and_receives_events() -> io::Result<()> {
        let name = test_pipe("greet");
        let (woken_tx, woken) = mpsc::channel();
        let server = Server::start_on(&name, move || {
            let _ = woken_tx.send(());
        })?;
        server.send(Event::State {
            session: Some(Target::Memo),
            id: None,
        });

        let mut client = TestClient::connect(&name)?;
        let hello = client.event()?;
        assert_eq!(field(&hello, "event"), Some("hello"), "{hello}");
        assert_eq!(field(&hello, "state"), Some("listening"), "{hello}");
        assert_eq!(field(&hello, "mode"), Some("memo"), "{hello}");
        assert_eq!(hello.get("protocol").and_then(Value::as_u64), Some(1));

        assert_eq!(server.clients(), [UNNAMED]);
        client.send(
            b"{\"command\":\"hello\",\"name\":\"Deck\",\"protocol\":1}\r\n\
              {\"command\":\"toggle\",\"mode\":\"capture\",\"id\":\"7\"}\n\
              nonsense\n",
        )?;
        assert!(woken.recv_timeout(Duration::from_secs(5)).is_ok());
        let requests: Vec<_> = server.requests().collect();
        assert_eq!(
            requests
                .iter()
                .map(|request| &request.command)
                .collect::<Vec<_>>(),
            [&Command::Toggle {
                target: Target::Capture,
                id: Some("7".into())
            }]
        );
        // The name arrived before the command that woke us.
        assert_eq!(server.clients(), ["Deck"]);
        let refusal = client.event()?;
        assert_eq!(field(&refusal, "event"), Some("error"), "{refusal}");
        assert_eq!(field(&refusal, "code"), Some("invalid_json"), "{refusal}");

        server.reply(
            requests[0].from,
            Event::Error {
                code: ErrorCode::Busy,
                message: "a session is running",
                id: Some("7"),
            },
        );
        let answer = client.event()?;
        assert_eq!(field(&answer, "code"), Some("busy"), "{answer}");
        assert_eq!(field(&answer, "id"), Some("7"), "{answer}");

        server.send(Event::Dictation {
            text: "hello",
            id: Some("7"),
        });
        assert_eq!(
            client.line()?,
            "{\"event\":\"dictation\",\"id\":\"7\",\"text\":\"hello\"}"
        );

        // A second client can connect while the first is still there.
        let mut second = TestClient::connect(&name)?;
        assert_eq!(field(&second.event()?, "event"), Some("hello"));
        assert_eq!(server.clients().len(), 2);

        // Dropping the server closes the pipe: the client reads the end.
        drop(server);
        assert!(matches!(client.line().as_deref(), Ok("") | Err(_)));
        Ok(())
    }

    #[test]
    fn answers_go_only_to_the_client_that_asked() -> io::Result<()> {
        let name = test_pipe("answers");
        let (woken_tx, woken) = mpsc::channel();
        let server = Server::start_on(&name, move || {
            let _ = woken_tx.send(());
        })?;
        let mut asking = TestClient::connect(&name)?;
        let mut other = TestClient::connect(&name)?;
        asking.event()?;
        other.event()?;

        asking.send(b"{\"command\":\"start\",\"mode\":\"shout\",\"id\":\"q\"}\n")?;
        let refusal = asking.event()?;
        assert_eq!(field(&refusal, "code"), Some("unknown_mode"), "{refusal}");
        assert_eq!(field(&refusal, "id"), Some("q"), "{refusal}");

        asking.send(b"{\"command\":\"stop\",\"id\":\"s\"}\n")?;
        assert!(woken.recv_timeout(Duration::from_secs(5)).is_ok());
        let Some(request) = server.requests().next() else {
            return Err(io::Error::other("the stop did not arrive"));
        };
        server.reply(
            request.from,
            Event::Error {
                code: ErrorCode::SettingsOpen,
                message: "settings are open",
                id: request.command.id(),
            },
        );
        assert_eq!(field(&asking.event()?, "code"), Some("settings_open"));

        // The other client saw neither answer: its next line is the broadcast.
        server.send(Event::State {
            session: None,
            id: None,
        });
        assert_eq!(other.line()?, r#"{"event":"state","state":"idle"}"#);
        assert_eq!(asking.line()?, r#"{"event":"state","state":"idle"}"#);
        Ok(())
    }

    #[test]
    fn an_overlong_line_is_refused_and_skipped() -> io::Result<()> {
        let name = test_pipe("long");
        let (woken_tx, woken) = mpsc::channel();
        let server = Server::start_on(&name, move || {
            let _ = woken_tx.send(());
        })?;
        let mut client = TestClient::connect(&name)?;
        client.event()?;

        let mut long = b"{\"command\":\"stop\",\"id\":\"".to_vec();
        long.resize(MAX_LINE * 3, b'x');
        client.send(&long)?;
        let refusal = client.event()?;
        assert_eq!(field(&refusal, "code"), Some("line_too_long"), "{refusal}");
        // The rest of the long line is skipped; the next line is read as usual.
        client.send(b"\"}\n{\"command\":\"stop\"}\n")?;
        assert!(woken.recv_timeout(Duration::from_secs(5)).is_ok());
        assert_eq!(
            server
                .requests()
                .map(|request| request.command)
                .collect::<Vec<_>>(),
            [Command::Stop { id: None }]
        );

        client.send(b"\xff\xfe\n")?;
        assert_eq!(field(&client.event()?, "code"), Some("invalid_json"));
        Ok(())
    }

    #[test]
    fn clients_beyond_the_limit_are_told_and_disconnected() -> io::Result<()> {
        let name = test_pipe("full");
        let server = Server::start_on(&name, || {})?;
        let mut connected = Vec::new();
        for _ in 0..MAX_CLIENTS {
            let mut client = TestClient::connect(&name)?;
            assert_eq!(field(&client.event()?, "event"), Some("hello"));
            connected.push(client);
        }
        let mut turned_away = TestClient::connect(&name)?;
        let refusal = turned_away.event()?;
        assert_eq!(
            field(&refusal, "code"),
            Some("too_many_clients"),
            "{refusal}"
        );
        assert!(matches!(turned_away.line().as_deref(), Ok("") | Err(_)));
        assert_eq!(server.clients().len(), MAX_CLIENTS);

        // A place that frees up can be taken again.
        drop(connected.pop());
        let mut tries = 0;
        while server.clients().len() == MAX_CLIENTS && tries < 100 {
            tries += 1;
            thread::sleep(Duration::from_millis(20));
        }
        let mut late = TestClient::connect(&name)?;
        assert_eq!(field(&late.event()?, "event"), Some("hello"));
        Ok(())
    }

    /// Reads every line `input` splits into, fed in pieces of `piece` bytes.
    fn split(input: &[u8], piece: usize) -> Vec<Line> {
        let runtime = tokio::runtime::Builder::new_current_thread().build();
        let Ok(runtime) = runtime else {
            return Vec::new();
        };
        let mut reader = pieces(input, piece);
        runtime.block_on(async {
            let mut lines = Lines::new(&mut reader);
            let mut all = Vec::new();
            while let Ok(Some(line)) = lines.next().await {
                all.push(line);
            }
            all
        })
    }

    /// A reader that hands out `input` at most `piece` bytes at a time.
    fn pieces(input: &[u8], piece: usize) -> impl AsyncRead + Unpin {
        struct Pieces(Vec<u8>, usize);
        impl AsyncRead for Pieces {
            fn poll_read(
                mut self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
                buf: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<io::Result<()>> {
                let take = self.1.min(buf.remaining()).min(self.0.len());
                let rest = self.0.split_off(take);
                buf.put_slice(&std::mem::replace(&mut self.0, rest));
                std::task::Poll::Ready(Ok(()))
            }
        }
        Pieces(input.to_vec(), piece)
    }

    #[test]
    fn lines_are_split_however_they_arrive() {
        let text = |line: &str| Line::Text(line.to_owned());
        for piece in [1, 3, 4096] {
            assert_eq!(
                split(b"a\nb\r\n\nlast", piece),
                [text("a"), text("b"), text(""), text("last")]
            );
        }
        let mut long = vec![b'x'; MAX_LINE + 10];
        long.extend_from_slice(b"\nok\n");
        long.extend_from_slice(&vec![b'y'; MAX_LINE * 2]);
        assert_eq!(
            split(&long, 4096),
            [Line::TooLong, text("ok"), Line::TooLong]
        );
        let exact = [vec![b'z'; MAX_LINE], b"\n".to_vec()].concat();
        assert_eq!(split(&exact, 1000).len(), 1);
        assert_eq!(split(b"\xc3\n", 10), [Line::NotUtf8]);
    }
}
