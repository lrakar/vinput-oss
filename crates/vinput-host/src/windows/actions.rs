//! The actions of the user's own: a dictation handed to a program they
//! named, and the test of one from Settings.
//!
//! The program runs on a thread of its own, which also types or copies
//! what it wrote, and leaves how it went in a queue and posts
//! `ACTION_DONE`. Only a failure is said, in a notification; a test says
//! how it went on the page.
#![allow(unsafe_code)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

use super::tray::About;
use super::{ACTION_DONE, Host};
use vinput_core::actions::{self, Action, Failure};
use vinput_core::log;
use vinput_view::{Notice, NoticeKind, Page};

/// What a test of an action hands its program.
pub(super) const TEST_TEXT: &str = "This is a test from VinPUT.";
/// The most of what a tested program wrote that the page shows.
const SHOWN: usize = 160;

/// How one run of an action went.
#[derive(Debug)]
struct Done {
    /// The name of the action.
    name: String,
    /// Whether it was a test from Settings.
    test: bool,
    /// What the program wrote, or why it did not work.
    result: Result<String, Failure>,
}

/// The runs of actions that have finished and were not heard of yet.
#[derive(Debug, Default)]
pub(super) struct Actions {
    done: Arc<Mutex<VecDeque<Done>>>,
}

impl Host {
    /// Hands `text` to the action at `slot`. Nothing said starts nothing.
    pub(super) fn run_action(&mut self, slot: u8, text: String) {
        let Some(action) = self.settings.actions.get(usize::from(slot)).cloned() else {
            return;
        };
        if !text.trim().is_empty() {
            self.start_action(action, text, false);
        }
    }

    /// Runs the action at `index` with [`TEST_TEXT`]; the page says how it
    /// went. What the program writes is shown and not typed or copied.
    pub(super) fn test_action(&mut self, index: usize) {
        if let Some(action) = self.settings.actions.get(index).cloned() {
            self.start_action(action, TEST_TEXT.to_owned(), true);
        }
    }

    fn start_action(&mut self, action: Action, text: String, tried: bool) {
        let done = Arc::clone(&self.actions.done);
        // A window handle is an address that any thread may post to.
        let window = self.hwnd as usize;
        let name = action.name.clone();
        let started = thread::Builder::new()
            .name("vinput-action".into())
            .spawn(move || {
                let result = if tried {
                    actions::run(&action, &text)
                } else {
                    actions::carry_out(&action, &text).map(|()| String::new())
                };
                done.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push_back(Done {
                        name: action.name,
                        test: tried,
                        result,
                    });
                // SAFETY: posting to a window that has gone away fails harmlessly.
                unsafe { PostMessageW(window as HWND, ACTION_DONE, 0, 0) };
            });
        if let Err(err) = started {
            log::write(&format!("starting a thread for an action failed: {err}"));
            self.tray.warn(
                About::Settings(Page::Shortcuts),
                &format!("{name} did not run"),
                "Windows did not let VinPUT start it.",
            );
        }
    }

    /// Says how the actions that have finished went.
    pub(super) fn on_action_done(&mut self) {
        loop {
            let next = self
                .actions
                .done
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop_front();
            let Some(done) = next else { break };
            if let Some(notice) = tested(&done) {
                self.notice = Some(notice);
            } else if let Err(failure) = &done.result {
                log::write(&format!("the action {} failed: {failure}", done.name));
                self.tray.warn(
                    About::Settings(Page::Shortcuts),
                    &format!("{} did not work", done.name),
                    &failure.to_string(),
                );
            }
        }
        self.show_state();
    }
}

/// What the page says after a test; `None` for a run that was no test.
fn tested(done: &Done) -> Option<Notice> {
    if !done.test {
        return None;
    }
    let (kind, text) = match &done.result {
        Ok(output) if output.trim().is_empty() => {
            (NoticeKind::Success, format!("{} worked.", done.name))
        }
        Ok(output) => {
            let line = output.split_whitespace().collect::<Vec<_>>().join(" ");
            let mut shown: String = line.chars().take(SHOWN).collect();
            if shown.len() < line.len() {
                shown.push_str("...");
            }
            (
                NoticeKind::Success,
                format!("{} worked. It wrote: {shown}", done.name),
            )
        }
        Err(failure) => (
            NoticeKind::Error,
            format!("{} did not work. {failure}", done.name),
        ),
    };
    Some(Notice {
        kind,
        text,
        page: Page::Shortcuts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn done(test: bool, result: Result<String, Failure>) -> Done {
        Done {
            name: "Notes".to_owned(),
            test,
            result,
        }
    }

    #[test]
    fn a_test_says_how_it_went_on_the_page() {
        let said = |result| tested(&done(true, result)).map(|notice| (notice.kind, notice.text));
        assert_eq!(
            said(Ok(String::new())),
            Some((NoticeKind::Success, "Notes worked.".to_owned()))
        );
        assert_eq!(
            said(Ok("THIS IS\r\nA TEST".to_owned())),
            Some((
                NoticeKind::Success,
                "Notes worked. It wrote: THIS IS A TEST".to_owned()
            ))
        );
        let long = said(Ok("x".repeat(500))).map(|(_, text)| text);
        assert!(long.is_some_and(|text| text.ends_with("...") && text.len() < 200));
        assert_eq!(
            said(Err(Failure::TimedOut(30))),
            Some((
                NoticeKind::Error,
                "Notes did not work. It took longer than 30 seconds and was stopped.".to_owned()
            ))
        );
        // A run that was no test says nothing on the page.
        assert_eq!(tested(&done(false, Err(Failure::TimedOut(30)))), None);
    }
}
