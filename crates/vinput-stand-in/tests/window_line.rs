//! The line between the background process and a window process, from end
//! to end: a real process, real pipes, and both halves of the line as they
//! are used, the host's from `vinput-host` and the window's from `vinput-ui`.
//!
//! The windows themselves are left out. In their place runs the stand-in of
//! this crate, which speaks the window's side of the line and shows nothing.
#![cfg(windows)]

use std::collections::VecDeque;
use std::io;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use vinput_host::link::{Link, window_name};
use vinput_ui::{Options, Source};
use vinput_view::{Command, Event, Page, Surface, View};

const WAIT: Duration = Duration::from_secs(30);

/// A window process and what it has said so far.
struct Window {
    link: Link,
    wakes: mpsc::Receiver<()>,
    said: VecDeque<Command>,
}

impl Window {
    fn start(arguments: &[&str]) -> io::Result<Self> {
        let (woken, wakes) = mpsc::channel();
        let program = Path::new(env!("CARGO_BIN_EXE_vinput-stand-in"));
        let link = Link::start(program, arguments, move || {
            let _ = woken.send(());
        })?;
        Ok(Self {
            link,
            wakes,
            said: VecDeque::new(),
        })
    }

    /// The next thing the window says, or `None` if it says nothing more.
    fn next(&mut self) -> Option<Command> {
        let started = Instant::now();
        loop {
            self.said.extend(self.link.commands());
            if let Some(command) = self.said.pop_front() {
                return Some(command);
            }
            if self.link.has_ended() || started.elapsed() > WAIT {
                // Its last words may have come in just before it went.
                self.said.extend(self.link.commands());
                return self.said.pop_front();
            }
            let _ = self.wakes.recv_timeout(Duration::from_millis(100));
        }
    }

    /// Whether the window's process goes away in good time.
    fn leaves(&self) -> bool {
        let started = Instant::now();
        while !self.link.has_ended() && started.elapsed() < WAIT {
            let _ = self.wakes.recv_timeout(Duration::from_millis(100));
        }
        self.link.has_ended()
    }
}

#[test]
fn a_window_process_is_told_and_heard_over_real_pipes() -> io::Result<()> {
    let mut window = Window::start(&[])?;
    assert!(window.link.process_id().is_some());
    assert_eq!(window.next(), Some(Command::Ready));

    let picture = View {
        version: "a picture".to_owned(),
        ..View::default()
    };
    window.link.send(Event::State(Box::new(picture)));
    assert_eq!(
        window.next(),
        Some(Command::FindModel("a picture".to_owned()))
    );
    window.link.send(Event::Show(Page::Advanced));
    assert_eq!(
        window.next(),
        Some(Command::OpenLink("Advanced".to_owned()))
    );

    // Told to close, it says that it has, and its process ends.
    window.link.send(Event::Close);
    assert_eq!(window.next(), Some(Command::Closed));
    assert!(window.leaves());
    assert_eq!(window.next(), None);
    Ok(())
}

#[test]
fn letting_go_of_the_line_closes_the_window() -> io::Result<()> {
    let mut window = Window::start(&[])?;
    assert_eq!(window.next(), Some(Command::Ready));
    let asked = Instant::now();
    // Returns once the process is gone, which a window that is told to
    // close sees to by itself, long before anyone would make it.
    window.link.finish();
    assert!(asked.elapsed() < Duration::from_secs(1));
    Ok(())
}

#[test]
fn a_process_that_shows_no_window_ends_the_line_without_a_word() -> io::Result<()> {
    let mut window = Window::start(&["--leave"])?;
    assert!(window.leaves());
    assert_eq!(window.next(), None);
    Ok(())
}

#[test]
fn the_host_starts_each_window_by_a_name_the_window_process_knows() {
    for surface in [
        Surface::Settings(Page::General),
        Surface::Settings(Page::Models),
        Surface::Settings(Page::AddModel),
        Surface::Settings(Page::Pro),
        Surface::Settings(Page::Advanced),
        Surface::Settings(Page::About),
        Surface::Update,
    ] {
        assert_eq!(
            Options::parse(&[window_name(surface)]).ok(),
            Some(Options::new(surface, Source::Host))
        );
    }
}
