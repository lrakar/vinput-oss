//! The host's side of the local API: carrying out what connected programs
//! ask for, and telling them what happens.
//!
//! The pipe and its protocol are in `vinput_core::api`. Its thread wakes the
//! message loop for each command; the loop answers here, on the thread that
//! owns the sessions, so a command and a hotkey can never run into each
//! other. What a command comes to is decided by [`decide`], which looks at
//! nothing but the command and how things stand, so every rule of
//! `docs/api.md` about commands has a test.
#![allow(unsafe_code)]

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

use super::{API_COMMAND, Host};
use vinput_core::api::{self, ClientId, Command, ErrorCode, Event, Request};
use vinput_core::log;
use vinput_core::session::{Action, Controller, Target};

/// What a command of an API client comes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Start a session in this mode.
    Start(Target),
    /// Stop the running session.
    Stop,
    /// Nothing to do, and nothing wrong.
    Nothing,
    /// Tell the client why not, with this code and in these words.
    Refuse(ErrorCode, &'static str),
}

/// Decides what `command` comes to. `running` says whether a session is
/// running, `capturing` whether a hotkey field in Settings has the keys.
///
/// While a hotkey is being set, a dictation would be typed into the field
/// that is waiting for a key, so nothing starts until that is done. Stopping
/// and saying hello are never refused: neither can do any harm.
fn decide(command: &Command, running: bool, capturing: bool) -> Step {
    match command {
        Command::Stop { .. } | Command::Toggle { .. } if running => Step::Stop,
        Command::Start { .. } if running => {
            Step::Refuse(ErrorCode::Busy, "Vinput is listening already")
        }
        Command::Start { .. } | Command::Toggle { .. } if capturing => Step::Refuse(
            ErrorCode::SettingsOpen,
            "a hotkey is being set in Vinput's settings window",
        ),
        Command::Start { target, .. } | Command::Toggle { target, .. } => Step::Start(*target),
        // Stopping when nothing runs, and names, which the API keeps to
        // itself, need nothing done.
        Command::Stop { .. } | Command::Hello { .. } => Step::Nothing,
    }
}

impl Host {
    /// Carries out what clients of the local API have asked for.
    pub(super) fn on_api_command(&mut self, controller: &mut dyn Controller) {
        let requests: Vec<Request> = self.api.iter().flat_map(api::Server::requests).collect();
        for Request { from, command } in requests {
            let action = match decide(&command, self.trigger.is_running(), self.capturing) {
                Step::Refuse(code, message) => {
                    self.answer(from, code, message, command.id());
                    continue;
                }
                Step::Stop => self.trigger.stop(),
                Step::Start(target) => self.trigger.start(target),
                Step::Nothing => Action::None,
            };
            let requester = matches!(action, Action::Start { .. })
                .then(|| (from, command.id().map(str::to_owned)));
            self.act(action, None, requester, controller);
        }
    }

    /// Answers one client of the local API with an error about its command.
    pub(super) fn answer(&self, to: ClientId, code: ErrorCode, message: &str, id: Option<&str>) {
        if let Some(api) = &self.api {
            api.reply(to, Event::Error { code, message, id });
        }
    }

    /// Tells every client of the local API what has happened.
    pub(super) fn send_event(&self, event: Event<'_>) {
        if let Some(api) = &self.api {
            api.send(event);
        }
    }
}

/// Starts the local API, whose thread wakes the message loop for each command.
pub(super) fn start_api(hwnd: HWND) -> Option<api::Server> {
    // A window handle is an address that any thread may post to.
    let window = hwnd as usize;
    api::Server::start(move || {
        // SAFETY: posting to a window that has gone away fails harmlessly.
        unsafe { PostMessageW(window as HWND, API_COMMAND, 0, 0) };
    })
    .map_err(|err| log::write(&format!("the local API did not start: {err}")))
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(target: Target) -> Command {
        Command::Start { target, id: None }
    }

    fn toggle(target: Target) -> Command {
        Command::Toggle { target, id: None }
    }

    const STOP: Command = Command::Stop { id: None };

    #[test]
    fn start_starts_once_and_toggle_goes_either_way() {
        assert_eq!(
            decide(&start(Target::Capture), false, false),
            Step::Start(Target::Capture)
        );
        assert_eq!(
            decide(&start(Target::Capture), true, false),
            Step::Refuse(ErrorCode::Busy, "Vinput is listening already")
        );
        assert_eq!(
            decide(&toggle(Target::Memo), false, false),
            Step::Start(Target::Memo)
        );
        assert_eq!(decide(&toggle(Target::Memo), true, false), Step::Stop);
    }

    #[test]
    fn stop_stops_what_runs_and_is_never_an_error() {
        assert_eq!(decide(&STOP, true, false), Step::Stop);
        assert_eq!(decide(&STOP, false, false), Step::Nothing);
        assert_eq!(decide(&STOP, false, true), Step::Nothing);
        assert_eq!(decide(&STOP, true, true), Step::Stop);
    }

    #[test]
    fn nothing_starts_while_a_hotkey_is_being_set() {
        for command in [start(Target::Focused), toggle(Target::Search)] {
            assert!(
                matches!(
                    decide(&command, false, true),
                    Step::Refuse(ErrorCode::SettingsOpen, _)
                ),
                "{command:?}"
            );
        }
        // With Settings open and no hotkey being set, everything works: the
        // settings take effect as they are made and the hotkeys stay on.
        assert_eq!(
            decide(&start(Target::Focused), false, false),
            Step::Start(Target::Focused)
        );
    }

    #[test]
    fn saying_hello_is_never_refused() {
        let hello = Command::Hello {
            name: "My deck".to_owned(),
        };
        for (running, capturing) in [(false, false), (true, false), (false, true), (true, true)] {
            assert_eq!(decide(&hello, running, capturing), Step::Nothing);
        }
    }
}
