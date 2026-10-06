//! The line to the background process: events come in on standard input,
//! commands go out on standard output, one line of JSON each.
//!
//! Because standard output is the line, nothing else in a window process may
//! print to it.

use std::io::{BufRead, Write};
use std::sync::Mutex;
use std::thread;

use vinput_view::{Command, Event};

use crate::{Backend, Listener};

/// The background process, as a window process sees it.
#[derive(Debug)]
pub struct Host<W: Write + Send> {
    out: Mutex<W>,
}

impl<W: Write + Send> Backend for Host<W> {
    fn send(&self, command: Command) {
        let mut line = command.to_line();
        line.push('\n');
        // If the background process is gone there is nobody to tell, and the
        // reading side is about to close the window anyway.
        if let Ok(mut out) = self.out.lock() {
            let _ = out.write_all(line.as_bytes()).and_then(|()| out.flush());
        }
    }
}

/// Connects `input` and `output` as the line to the background process.
///
/// `listener` is called for every event, on a thread started here. When the
/// input ends, because the background process closed it or went away, the
/// listener gets a last [`Event::Close`]: a window with nobody behind it has
/// nothing true to show.
pub fn connect<R, W>(input: R, output: W, listener: Listener) -> Host<W>
where
    R: BufRead + Send + 'static,
    W: Write + Send,
{
    thread::spawn(move || {
        for line in input.lines() {
            let Ok(line) = line else { break };
            if let Some(event) = Event::from_line(&line) {
                listener(event);
            }
        }
        listener(Event::Close);
    });
    Host {
        out: Mutex::new(output),
    }
}

/// Connects this process's standard input and output; see [`connect`].
#[must_use]
pub fn connect_stdio(listener: Listener) -> Host<std::io::Stdout> {
    connect(
        std::io::BufReader::new(std::io::stdin()),
        std::io::stdout(),
        listener,
    )
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::mpsc;
    use std::time::Duration;

    use vinput_view::Page;

    use super::*;

    #[test]
    fn events_arrive_in_order_and_the_end_of_input_closes_the_window() {
        let input = format!(
            "{}\nthis is not an event\n{}\n",
            Event::Show(Page::Models).to_line(),
            Event::Show(Page::Advanced).to_line()
        );
        let (seen, events) = mpsc::channel();
        let host = connect(
            Cursor::new(input),
            Vec::new(),
            Box::new(move |event| {
                let _ = seen.send(event);
            }),
        );
        let wait = Duration::from_secs(5);
        assert_eq!(events.recv_timeout(wait), Ok(Event::Show(Page::Models)));
        assert_eq!(events.recv_timeout(wait), Ok(Event::Show(Page::Advanced)));
        assert_eq!(events.recv_timeout(wait), Ok(Event::Close));

        host.send(Command::Ready);
        host.send(Command::Closed);
        let written = host.out.lock().map(|out| out.clone()).unwrap_or_default();
        let written = String::from_utf8(written).unwrap_or_default();
        let commands: Vec<_> = written.lines().filter_map(Command::from_line).collect();
        assert_eq!(commands, [Command::Ready, Command::Closed]);
    }
}
