//! The line to a window process: events go out to its standard input,
//! commands come back from its standard output, one line of JSON each.
//!
//! A window process exists only while its window is open, and so does
//! everything here: a [`Link`] starts the process and two threads, one that
//! reads what the window says and one that writes what it is told. Nothing
//! the window process does, or fails to do, can hold up the thread that owns
//! the link: sending never waits for the other side to read.

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, ErrorKind, Write};
use std::path::Path;
use std::process::{Child, Command as Process, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use vinput_view::{Command, Event, Page, Surface};

/// How long a window process gets to leave after it was told to close,
/// before it is made to.
const GRACE: Duration = Duration::from_millis(1500);
/// How often a window process that was told to close is looked at.
const REAP_STEP: Duration = Duration::from_millis(20);
/// The longest line a window may send. A picture is the longest thing on the
/// line and goes the other way; a longer line than this is not a command and
/// is passed over without being kept.
const MAX_LINE: usize = 1 << 20;

/// What follows `ui` on the command line of the window process that shows
/// `surface`.
#[must_use]
pub fn window_name(surface: Surface) -> &'static str {
    match surface {
        Surface::Settings(Page::General) => "settings",
        Surface::Settings(Page::Shortcuts) => "shortcuts",
        Surface::Settings(Page::Models) => "models",
        Surface::Settings(Page::AddModel) => "add-model",
        Surface::Settings(Page::Google) => "google",
        Surface::Settings(Page::Pro) => "pro",
        Surface::Settings(Page::Words) => "words",
        Surface::Settings(Page::Cleanup) => "cleanup",
        Surface::Settings(Page::Advanced) => "advanced",
        Surface::Settings(Page::About) => "about",
        Surface::Settings(Page::Welcome) => "welcome",
        Surface::Settings(Page::WelcomePro) => "welcome-pro",
        Surface::Settings(Page::WelcomeLocal) => "welcome-local",
        Surface::Update => "update",
        // A page of another program of the family, when one is built
        // beside this one: Vinput has no window for it.
        #[allow(unreachable_patterns, clippy::match_same_arms)]
        Surface::Settings(_) => "settings",
    }
}

/// What the window has said and not been heard on yet.
#[derive(Default)]
struct Inbox {
    commands: Mutex<VecDeque<Command>>,
    /// Set when the window's side of the line has ended: it is gone.
    ended: AtomicBool,
}

/// What the window is still to be told.
#[derive(Default)]
struct Outbox {
    queue: Mutex<Queue>,
    changed: Condvar,
}

/// The events waiting to be written, oldest first.
#[derive(Default)]
struct Queue {
    events: VecDeque<Event>,
    /// Nothing more is coming: the writer leaves once the queue is empty.
    done: bool,
}

impl Queue {
    /// Adds an event. A picture that was not written yet gives way to a
    /// newer one, so a window that reads slowly never falls behind.
    fn push(&mut self, event: Event) {
        if self.done {
            return;
        }
        if matches!(event, Event::State(_)) {
            self.events
                .retain(|waiting| !matches!(waiting, Event::State(_)));
        }
        self.events.push_back(event);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A window process that was started, and what ties it to this one.
struct Started {
    child: Child,
    /// Ends the window process if this process goes without seeing it off,
    /// because it crashed or was killed. A window with nobody behind it
    /// closes by itself when its input ends; this is for one that cannot.
    #[cfg(windows)]
    _guard: Option<vinput_core::job::KillOnDrop>,
}

impl Started {
    fn new(child: Child) -> Self {
        Self {
            #[cfg(windows)]
            _guard: vinput_core::job::KillOnDrop::attach(&child),
            child,
        }
    }
}

/// A window process, as the background process sees it.
///
/// Dropping the link tells the window to close and sees the process off in
/// the background; [`Link::finish`] does the same and waits for it.
pub struct Link {
    inbox: Arc<Inbox>,
    outbox: Arc<Outbox>,
    /// The process, when the link is to a real one.
    child: Option<Started>,
}

impl std::fmt::Debug for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Link")
            .field("process", &self.process_id())
            .field("ended", &self.has_ended())
            .finish_non_exhaustive()
    }
}

impl Link {
    /// Starts this program again as the window process for `surface`.
    /// `wake` is called, on another thread, each time the window has said
    /// something and once more when it is gone.
    ///
    /// # Errors
    ///
    /// Returns why the process or its threads could not be started.
    pub fn open(surface: Surface, wake: impl Fn() + Send + 'static) -> io::Result<Self> {
        let program = std::env::current_exe()?;
        Self::start(&program, &["ui", window_name(surface)], wake)
    }

    /// Starts `program` with `arguments` as a window process; see
    /// [`Link::open`].
    ///
    /// # Errors
    ///
    /// Returns why the process or its threads could not be started.
    pub fn start(
        program: &Path,
        arguments: &[&str],
        wake: impl Fn() + Send + 'static,
    ) -> io::Result<Self> {
        let mut child = Process::new(program)
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // What a window process has to say for itself is of use only to
            // someone working on it, who has a console.
            .stderr(if cfg!(debug_assertions) {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .spawn()?;
        let pipes = child.stdout.take().zip(child.stdin.take());
        let connected = pipes
            .ok_or_else(|| io::Error::other("the window process has no pipes"))
            .and_then(|(output, input)| Self::connect(BufReader::new(output), input, wake));
        let started = Started::new(child);
        match connected {
            Ok(mut link) => {
                link.child = Some(started);
                Ok(link)
            }
            Err(err) => {
                reap(started, Duration::ZERO);
                Err(err)
            }
        }
    }

    /// Makes a link out of the two halves of a line: `input` is what the
    /// window says, `output` what it is told. See [`Link::open`] for `wake`.
    ///
    /// # Errors
    ///
    /// Returns why the threads of the link could not be started.
    pub fn connect<R, W>(input: R, output: W, wake: impl Fn() + Send + 'static) -> io::Result<Self>
    where
        R: BufRead + Send + 'static,
        W: Write + Send + 'static,
    {
        let inbox = Arc::new(Inbox::default());
        let outbox = Arc::new(Outbox::default());
        let (heard, told) = (Arc::clone(&inbox), Arc::clone(&outbox));
        thread::Builder::new()
            .name("vinput-window-out".into())
            .spawn(move || write_events(output, &told))?;
        let reading = thread::Builder::new()
            .name("vinput-window-in".into())
            .spawn(move || read_commands(input, &heard, &wake));
        if let Err(err) = reading {
            // The writer that was just started has nobody to write for.
            lock(&outbox.queue).done = true;
            outbox.changed.notify_all();
            return Err(err);
        }
        Ok(Self {
            inbox,
            outbox,
            child: None,
        })
    }

    /// Tells the window something. Returns at once; the event is written by
    /// a thread of the link, and dropped if the window is gone.
    pub fn send(&self, event: Event) {
        lock(&self.outbox.queue).push(event);
        self.outbox.changed.notify_all();
    }

    /// What the window has said since the last call, oldest first.
    #[must_use]
    pub fn commands(&self) -> Vec<Command> {
        lock(&self.inbox.commands).drain(..).collect()
    }

    /// Whether the window's side of the line has ended: its process has
    /// exited, or is no longer one that can be talked to.
    #[must_use]
    pub fn has_ended(&self) -> bool {
        self.inbox.ended.load(Ordering::Acquire)
    }

    /// What Windows knows the window process by, when there is one.
    #[must_use]
    pub fn process_id(&self) -> Option<u32> {
        self.child.as_ref().map(|started| started.child.id())
    }

    /// Tells the window to close and waits until its process is gone, making
    /// it go if it takes too long. For when the background process is about
    /// to exit and must leave nothing of itself running.
    pub fn finish(mut self) {
        self.close();
        if let Some(child) = self.child.take() {
            reap(child, GRACE);
        }
    }

    /// Tells the window to close, and the writer that this was the last
    /// word: nothing sent afterwards goes out. The process is seen off when
    /// the link is dropped or finished. Closing twice is the same as once.
    pub fn close(&self) {
        {
            let mut queue = lock(&self.outbox.queue);
            queue.push(Event::Close);
            queue.done = true;
        }
        self.outbox.changed.notify_all();
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.close();
        let Some(child) = self.child.take() else {
            return;
        };
        // Seeing the process off takes a moment that the caller, which has a
        // message loop to get back to, does not have. Both halves of the
        // exchange are needed because a closure gives up what it holds when
        // the thread could not be started.
        let seen_off = Arc::new(Mutex::new(Some(child)));
        let theirs = Arc::clone(&seen_off);
        let spawned = thread::Builder::new()
            .name("vinput-window-end".into())
            .spawn(move || {
                if let Some(child) = lock(&theirs).take() {
                    reap(child, GRACE);
                }
            });
        if spawned.is_err()
            && let Some(child) = lock(&seen_off).take()
        {
            reap(child, Duration::ZERO);
        }
    }
}

/// Waits for a window process to exit, for as long as `grace`, then ends it.
/// Either way the process is collected, so nothing of it stays behind.
fn reap(mut started: Started, grace: Duration) {
    let child = &mut started.child;
    let asked = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if asked.elapsed() < grace => thread::sleep(REAP_STEP),
            _ => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// What reading the next line of the input came to.
#[derive(Debug, PartialEq, Eq)]
enum Line {
    /// A line, without its line break, was read.
    Read,
    /// A line longer than [`MAX_LINE`] was passed over.
    TooLong,
    /// The input has ended.
    End,
}

/// Reads the next line of `input` into `line`, without its line break. A
/// line that is too long is read to its end and thrown away, so that the
/// lines after it are still heard and a window can never make this process
/// keep more than [`MAX_LINE`] bytes of what it says.
fn next_line(input: &mut impl BufRead, line: &mut Vec<u8>) -> io::Result<Line> {
    line.clear();
    let (mut read_any, mut too_long) = (false, false);
    loop {
        let buffer = match input.fill_buf() {
            Ok(buffer) => buffer,
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        if buffer.is_empty() {
            // A last line without a line break still counts.
            return Ok(match (read_any, too_long) {
                (false, _) => Line::End,
                (true, false) => Line::Read,
                (true, true) => Line::TooLong,
            });
        }
        read_any = true;
        let end = buffer.iter().position(|byte| *byte == b'\n');
        let text = &buffer[..end.unwrap_or(buffer.len())];
        if !too_long {
            if line.len() + text.len() > MAX_LINE {
                too_long = true;
                line.clear();
            } else {
                line.extend_from_slice(text);
            }
        }
        let taken = end.map_or(buffer.len(), |end| end + 1);
        input.consume(taken);
        if end.is_some() {
            return Ok(if too_long { Line::TooLong } else { Line::Read });
        }
    }
}

/// The reading half: every line that is a command joins the inbox. A line
/// that is not one, whatever is wrong with it, is passed over: only the end
/// of the input is the end of the window.
fn read_commands(mut input: impl BufRead, inbox: &Inbox, wake: &dyn Fn()) {
    let mut line = Vec::new();
    loop {
        match next_line(&mut input, &mut line) {
            Ok(Line::Read) => {}
            Ok(Line::TooLong) => continue,
            Ok(Line::End) | Err(_) => break,
        }
        if let Some(command) = Command::from_line(&String::from_utf8_lossy(&line)) {
            lock(&inbox.commands).push_back(command);
            wake();
        }
    }
    inbox.ended.store(true, Ordering::Release);
    wake();
}

/// The writing half: writes events as they are queued, until the last one
/// has gone out or the window stops taking them. Dropping `output` at the
/// end closes the window's input, which tells it that nobody is behind it.
fn write_events(mut output: impl Write, outbox: &Outbox) {
    loop {
        let next = {
            let mut queue = lock(&outbox.queue);
            loop {
                if let Some(event) = queue.events.pop_front() {
                    break Some(event);
                }
                if queue.done {
                    break None;
                }
                queue = outbox
                    .changed
                    .wait(queue)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        let Some(event) = next else { return };
        let mut line = event.to_line();
        line.push('\n');
        if output
            .write_all(line.as_bytes())
            .and_then(|()| output.flush())
            .is_err()
        {
            // Nobody is reading: what is still queued, and whatever is sent
            // from now on, has nowhere to go.
            let mut queue = lock(&outbox.queue);
            queue.done = true;
            queue.events.clear();
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use vinput_view::{Change, View};

    use super::*;

    const WAIT: Duration = Duration::from_secs(10);

    fn state(version: &str) -> Event {
        Event::State(Box::new(View {
            version: version.to_owned(),
            ..View::default()
        }))
    }

    /// A link over two pipes, with the far ends of both and the wake calls.
    fn linked() -> io::Result<(
        Link,
        io::PipeWriter,
        BufReader<io::PipeReader>,
        mpsc::Receiver<()>,
    )> {
        let (from_window, window_out) = io::pipe()?;
        let (window_in, to_window) = io::pipe()?;
        let (woken, wakes) = mpsc::channel();
        let link = Link::connect(BufReader::new(from_window), to_window, move || {
            let _ = woken.send(());
        })?;
        Ok((link, window_out, BufReader::new(window_in), wakes))
    }

    fn next_event(window_in: &mut impl BufRead) -> Option<Event> {
        let mut line = String::new();
        match window_in.read_line(&mut line) {
            Ok(read) if read > 0 => Event::from_line(&line),
            _ => None,
        }
    }

    #[test]
    fn commands_arrive_in_order_and_each_one_wakes_the_host() -> io::Result<()> {
        let (link, mut window_out, _window_in, wakes) = linked()?;
        assert!(link.commands().is_empty());
        assert!(!link.has_ended());

        let ready = Command::Ready.to_line();
        let set = Command::Set(Change::Autostart(false)).to_line();
        write!(window_out, "{ready}\nnot a command\n{set}\n")?;
        window_out.flush()?;
        // One wake for each command; the line that is not one is passed over.
        assert_eq!(wakes.recv_timeout(WAIT), Ok(()));
        assert_eq!(wakes.recv_timeout(WAIT), Ok(()));
        assert_eq!(
            link.commands(),
            [Command::Ready, Command::Set(Change::Autostart(false))]
        );
        assert!(link.commands().is_empty());
        assert!(!link.has_ended());
        Ok(())
    }

    #[test]
    fn the_end_of_the_input_says_the_window_is_gone() -> io::Result<()> {
        let (link, mut window_out, _window_in, wakes) = linked()?;
        writeln!(window_out, "{}", Command::Closed.to_line())?;
        drop(window_out);
        assert_eq!(wakes.recv_timeout(WAIT), Ok(()));
        assert_eq!(wakes.recv_timeout(WAIT), Ok(()));
        assert!(link.has_ended());
        // What it said before it went is still there to be heard.
        assert_eq!(link.commands(), [Command::Closed]);
        Ok(())
    }

    #[test]
    fn a_line_that_is_not_text_or_far_too_long_is_passed_over() -> io::Result<()> {
        let (link, mut window_out, _window_in, wakes) = linked()?;
        let ready = Command::Ready.to_line();
        // Bytes that are not UTF-8, then a line longer than any command.
        window_out.write_all(b"\xff\xfe\x00 not text\n")?;
        let feeding = thread::spawn(move || -> io::Result<io::PipeWriter> {
            let chunk = vec![b'x'; 64 * 1024];
            for _ in 0..=(MAX_LINE / chunk.len()) {
                window_out.write_all(&chunk)?;
            }
            write!(window_out, "\n{ready}\n")?;
            window_out.flush()?;
            Ok(window_out)
        });
        assert_eq!(wakes.recv_timeout(WAIT), Ok(()));
        assert_eq!(link.commands(), [Command::Ready]);
        // The window is still there: the line goes on.
        assert!(!link.has_ended());
        let window_out = feeding
            .join()
            .map_err(|_| io::Error::other("the writer panicked"))??;
        drop(window_out);
        assert_eq!(wakes.recv_timeout(WAIT), Ok(()));
        assert!(link.has_ended());
        Ok(())
    }

    #[test]
    fn lines_are_read_whole_whatever_their_ending() -> io::Result<()> {
        let mut input = io::Cursor::new(b"one\r\ntwo\n\nlast".to_vec());
        let mut line = Vec::new();
        let mut read = Vec::new();
        while next_line(&mut input, &mut line)? == Line::Read {
            read.push(String::from_utf8_lossy(&line).into_owned());
        }
        // The carriage return is trimmed with the rest of the white space
        // when the line is read as a command.
        assert_eq!(read, ["one\r", "two", "", "last"]);
        assert_eq!(next_line(&mut input, &mut line)?, Line::End);

        let long = vec![b'x'; MAX_LINE + 1];
        let mut input = io::Cursor::new([long.as_slice(), b"\nafter\n"].concat());
        assert_eq!(next_line(&mut input, &mut line)?, Line::TooLong);
        assert!(line.is_empty());
        assert_eq!(next_line(&mut input, &mut line)?, Line::Read);
        assert_eq!(line, b"after");
        Ok(())
    }

    #[test]
    fn closing_twice_says_so_once() -> io::Result<()> {
        let (link, _window_out, mut window_in, _wakes) = linked()?;
        link.close();
        link.close();
        link.send(Event::Show(Page::About));
        drop(link);
        assert_eq!(next_event(&mut window_in), Some(Event::Close));
        assert_eq!(next_event(&mut window_in), None);
        Ok(())
    }

    #[test]
    fn events_are_written_a_line_each_in_the_order_sent() -> io::Result<()> {
        let (link, _window_out, mut window_in, _wakes) = linked()?;
        link.send(Event::Show(Page::Models));
        assert_eq!(next_event(&mut window_in), Some(Event::Show(Page::Models)));
        link.send(state("1"));
        assert_eq!(next_event(&mut window_in), Some(state("1")));
        link.send(Event::Show(Page::About));
        assert_eq!(next_event(&mut window_in), Some(Event::Show(Page::About)));
        Ok(())
    }

    #[test]
    fn dropping_the_link_closes_the_window_and_ends_its_input() -> io::Result<()> {
        let (link, _window_out, mut window_in, _wakes) = linked()?;
        link.send(Event::Show(Page::Advanced));
        drop(link);
        assert_eq!(
            next_event(&mut window_in),
            Some(Event::Show(Page::Advanced))
        );
        assert_eq!(next_event(&mut window_in), Some(Event::Close));
        // After the last word the line itself ends.
        assert_eq!(next_event(&mut window_in), None);
        Ok(())
    }

    #[test]
    fn a_window_that_stopped_reading_does_not_hold_up_the_sender() -> io::Result<()> {
        let (link, _window_out, window_in, _wakes) = linked()?;
        // Far more than a pipe holds, to a window that reads none of it.
        let large = "x".repeat(64 * 1024);
        for _ in 0..64 {
            link.send(state(&large));
        }
        // The window goes away: the writer finds out and lets go of the rest.
        drop(window_in);
        link.send(state("after"));
        drop(link);
        Ok(())
    }

    #[test]
    fn a_newer_picture_replaces_one_that_was_not_written_yet() {
        let mut queue = Queue::default();
        queue.push(state("1"));
        queue.push(Event::Show(Page::Models));
        queue.push(state("2"));
        assert_eq!(queue.events, [Event::Show(Page::Models), state("2")]);
        // After the last word nothing more is taken.
        queue.push(Event::Close);
        queue.done = true;
        queue.push(state("3"));
        assert_eq!(
            queue.events,
            [Event::Show(Page::Models), state("2"), Event::Close]
        );
    }

    #[test]
    fn every_window_has_a_name_on_the_command_line() {
        let surfaces = [
            Surface::Settings(Page::General),
            Surface::Settings(Page::Shortcuts),
            Surface::Settings(Page::Models),
            Surface::Settings(Page::AddModel),
            Surface::Settings(Page::Google),
            Surface::Settings(Page::Pro),
            Surface::Settings(Page::Words),
            Surface::Settings(Page::Cleanup),
            Surface::Settings(Page::Advanced),
            Surface::Settings(Page::About),
            Surface::Update,
        ];
        let mut names: Vec<_> = surfaces.into_iter().map(window_name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), surfaces.len());
    }
}
