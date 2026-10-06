//! A stand-in for a window process, for testing the line between the
//! background process and its windows from end to end without a window.
//!
//! It speaks the window's side of the line with the code a real window
//! uses, says what a window says, and answers what it is told in a way a
//! test can recognize. It is never part of Vinput itself: only the tests of
//! this crate start it.
//!
//! ```text
//! vinput-stand-in            play a window until told to close
//! vinput-stand-in --leave    leave at once without a word, like a process that shows no window
//! ```

use std::process::ExitCode;
use std::sync::mpsc;

use vinput_ui::{Backend, link};
use vinput_view::{Command, Event};

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        None => {
            play();
            ExitCode::SUCCESS
        }
        Some(_) => ExitCode::FAILURE,
    }
}

/// Plays the window until it is told to close.
fn play() {
    let (heard, events) = mpsc::channel();
    let host = link::connect_stdio(Box::new(move |event| {
        let _ = heard.send(event);
    }));
    host.send(Command::Ready);
    for event in events {
        match event {
            // A picture is answered with what it said the version is.
            Event::State(view) => host.send(Command::FindModel(view.version)),
            Event::Show(page) => host.send(Command::OpenLink(format!("{page:?}"))),
            Event::Close => {
                host.send(Command::Closed);
                break;
            }
        }
    }
}
