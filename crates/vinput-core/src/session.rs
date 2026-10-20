//! Dictation sessions as the rest of Vinput talks about them: where their text
//! goes, what they leave behind, and how key presses start and stop them.
//!
//! This is the whole of what the background process knows about the engine:
//! it drives a [`Controller`], asks a [`Trigger`] what a key press means, and
//! passes on the [`Outcome`]s. Nothing here touches Windows, so the rules for
//! tapping and holding a key are tested with made-up times.

use crate::stt::ProviderKind;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Releasing the key sooner than this after pressing it counts as a tap, which
/// leaves dictation running until the next press. Holding longer is push-to-talk.
pub const HOLD_THRESHOLD: Duration = Duration::from_millis(350);

/// Where the text of a dictation session goes. Each target has its own hotkey.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Typed into the focused window as it is recognized.
    Focused,
    /// Typed into the focused window, followed by Enter.
    FocusedEnter,
    /// Not typed anywhere: handed to the programs connected to the local API once
    /// the session ends.
    Capture,
    /// Not typed anywhere: searched for on the web, in the default browser, once
    /// the session ends.
    Search,
    /// Not recognized at all: recorded and saved as a voice memo.
    Memo,
    /// Not typed as it is: taken for an instruction, and carried out on the
    /// text that is selected in the focused window, which the result takes
    /// the place of ([`crate::rewrite`]).
    Command,
    /// Not typed anywhere: handed to the program of the action of the
    /// user's own at this place in the settings ([`crate::actions`]) once
    /// the session ends.
    Action(u8),
}

impl Target {
    /// Whether the text is typed where the caret is, which is also where the
    /// indicator belongs. Other sessions show it at the bottom of the screen.
    #[must_use]
    pub fn types_at_caret(self) -> bool {
        matches!(self, Self::Focused | Self::FocusedEnter | Self::Command)
    }
}

/// Something to tell the user about a session that did what it could: a
/// dictation that was typed as it was said because it could not be tidied,
/// a selection that was left as it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Told {
    /// What it is about, in a few words.
    pub title: &'static str,
    /// What happened and what to do about it, in words for the user.
    pub message: String,
    /// Whether it comes back with every press until something is done in
    /// Settings: no model to rewrite with is chosen, downloaded or paid
    /// for.
    pub setup: bool,
}

/// What a finished session leaves behind for the host to pass on.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// A [`Target::Capture`] session ended; this is what was said.
    Captured {
        /// Everything recognized, or empty if nothing was.
        text: String,
        /// The id the session was started with, if any.
        id: Option<String>,
    },
    /// A [`Target::Search`] session ended; this is what to search for.
    Search {
        /// Everything recognized, or empty if nothing was.
        text: String,
    },
    /// A [`Target::Memo`] session saved its recording.
    Memo {
        /// The MP3 file.
        path: PathBuf,
        /// Length of the recording.
        seconds: f32,
        /// Whether the file was put on the clipboard.
        on_clipboard: bool,
        /// The id the session was started with, if any.
        id: Option<String>,
    },
    /// A [`Target::Action`] session ended; this is what to hand over.
    Action {
        /// Where the action is in the settings.
        slot: u8,
        /// Everything recognized, tidied if the action asks for it, or
        /// empty if nothing was said.
        text: String,
        /// What there is to tell the user about tidying it, if it could not
        /// be tidied and is handed over as it was said.
        told: Option<Told>,
    },
    /// A session did what it could and has something to say about it.
    Told(Told),
}

/// What a dictation has typed so far, for the programs connected to the
/// local API that the user lets read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Said {
    /// A number that is the same for everything one dictation types and
    /// different for the next.
    pub session: u64,
    /// Where the text of the dictation goes.
    pub target: Target,
    /// The id the dictation was started with, if any.
    pub id: Option<String>,
    /// Everything typed since the dictation started.
    pub text: String,
    /// Whether the dictation has ended, so that this is all it typed.
    pub last: bool,
}

/// A session that ended badly, for the host to tell the user and the
/// programs that follow along.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// What went wrong, in words for the user.
    pub message: String,
    /// The speech service the session used, which is where to look for what
    /// to do about it.
    pub provider: ProviderKind,
    /// Whether it failed for want of something only the user can see to in
    /// Settings ([`Error::Setup`](crate::Error::Setup)), so that the next
    /// session would fail the same way.
    pub setup: bool,
    /// The id the session was started with, if any.
    pub id: Option<String>,
}

/// What a [`Controller`] calls when a session has ended. It is called on
/// the thread of that session, so all it may do is wake whoever asked.
pub type Ended = Arc<dyn Fn() + Send + Sync>;

/// What a [`Controller`] calls around every Enter a session types: with
/// `true` just before, and with `false` when it has been typed. It is called
/// on the thread of that session, which waits for it to return.
pub type TypingEnter = Arc<dyn Fn(bool) + Send + Sync>;

/// What the host drives: something that can start and stop dictation sessions.
pub trait Controller {
    /// Starts a session whose text goes to `target`. `id` is whatever the
    /// program that asked for it wants to recognize its result by.
    fn start(&mut self, target: Target, id: Option<String>);
    /// Says whether the key that starts a session, or started the running
    /// one, is being held down: before every start, and again when a key
    /// that was only tapped is let go. A dictation is never ended for being
    /// quiet while its key is held; one that goes on after a tap, or that a
    /// program started, is ended after the time the settings allow.
    fn held(&mut self, _held: bool) {}
    /// Asks the running session to finish; it flushes its last words by itself.
    fn stop(&mut self);
    /// The user pressed Enter while the running session listens, and the key
    /// was kept from the program it was meant for. The session presses it
    /// in their place once its last words are typed, whichever key started
    /// it; one that ends with Enter anyway presses it once, not twice.
    /// [`stop`](Self::stop) follows.
    fn enter(&mut self) {}
    /// Calls off the running session and every stopped one that is still at
    /// work: from here on none of them types anything, saves a memo or opens
    /// a search, and none reports a failure. Each still ends, and says so,
    /// when its recognizer has let go of it. A session whose text was asked
    /// for by a program ([`Target::Capture`]) ends with an empty text, so
    /// that the program knows it is over.
    fn cancel(&mut self);
    /// Whether [`cancel`](Self::cancel) would call anything off: a session
    /// is capturing, or a stopped one that was not called off is still at
    /// work.
    fn can_cancel(&self) -> bool;
    /// Whether a session is still capturing audio.
    fn is_active(&self) -> bool;
    /// Whether a stopped session is still flushing or delivering its text.
    fn is_finishing(&self) -> bool;
    /// Sets what is called each time a session has ended, by itself or
    /// after it was stopped, instead of the host having to ask. By then
    /// everything the session leaves behind can be taken, and
    /// [`is_active`](Self::is_active) and
    /// [`is_finishing`](Self::is_finishing) no longer count it.
    fn on_session_end(&mut self, notify: Ended);
    /// Sets what is called around every Enter a session types, for whoever
    /// keeps that key from other programs while a dictation listens and has
    /// to let this one through.
    fn on_typing_enter(&mut self, _around: TypingEnter) {}
    /// Loudness of the latest audio, from 0.0 to 1.0.
    fn level(&self) -> f32;
    /// Takes the sessions that ended badly since the last call, oldest
    /// first, each with the id it was started with.
    fn take_failures(&mut self) -> Vec<Failure>;
    /// Takes what dictations have typed so far, where that has changed
    /// since the last call: the latest of each, oldest dictation first.
    /// Nothing unless the settings let programs connected to the local API
    /// read it, as they stand now.
    fn take_said(&mut self) -> Vec<Said> {
        Vec::new()
    }
    /// Takes what sessions have left behind since the last call.
    fn take_outcomes(&mut self) -> Vec<Outcome>;
    /// Applies changed settings to future sessions. With `reconnect`, the
    /// speech provider is set up again even if its settings look unchanged,
    /// because something outside them, such as an API key, has changed.
    fn configure(&mut self, settings: &crate::settings::Settings, reconnect: bool);
}

/// What the host should do in response to a key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Start a dictation session.
    Start {
        /// Where the text goes.
        target: Target,
    },
    /// Stop the running session.
    Stop,
    /// Nothing to do.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    /// The key that started the session is still down.
    Held {
        since: Instant,
    },
    /// The key was tapped; the session runs until the next press.
    Latched,
}

/// Turns key presses and releases into start and stop actions.
///
/// A tap starts dictation and a second press stops it. Holding the key dictates
/// for as long as it is held and stops on release.
#[derive(Debug)]
pub struct Trigger {
    phase: Phase,
}

impl Default for Trigger {
    fn default() -> Self {
        Self { phase: Phase::Idle }
    }
}

impl Trigger {
    /// A hotkey went down.
    pub fn key_down(&mut self, now: Instant, target: Target) -> Action {
        match self.phase {
            Phase::Idle => {
                self.phase = Phase::Held { since: now };
                Action::Start { target }
            }
            Phase::Latched => {
                self.phase = Phase::Idle;
                Action::Stop
            }
            Phase::Held { .. } => Action::None,
        }
    }

    /// The key that started the session came back up.
    pub fn key_up(&mut self, now: Instant) -> Action {
        match self.phase {
            Phase::Held { since } if now.duration_since(since) >= HOLD_THRESHOLD => {
                self.phase = Phase::Idle;
                Action::Stop
            }
            Phase::Held { .. } => {
                self.phase = Phase::Latched;
                Action::None
            }
            Phase::Idle | Phase::Latched => Action::None,
        }
    }

    /// Something other than a key asked for a session, which then runs until it
    /// is stopped.
    pub fn start(&mut self, target: Target) -> Action {
        if self.phase == Phase::Idle {
            self.phase = Phase::Latched;
            Action::Start { target }
        } else {
            Action::None
        }
    }

    /// Something other than a key asked for the running session to stop.
    pub fn stop(&mut self) -> Action {
        if self.phase == Phase::Idle {
            Action::None
        } else {
            self.phase = Phase::Idle;
            Action::Stop
        }
    }

    /// Whether a session is running as far as the trigger knows.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.phase != Phase::Idle
    }

    /// The session ended by itself, for example after an error.
    pub fn session_ended(&mut self) {
        self.phase = Phase::Idle;
    }

    /// Whether the host should be watching for the key to come up.
    #[must_use]
    pub fn is_held(&self) -> bool {
        matches!(self.phase, Phase::Held { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tap_latches_until_the_next_press() {
        let mut trigger = Trigger::default();
        let t0 = Instant::now();
        assert_eq!(
            trigger.key_down(t0, Target::Focused),
            Action::Start {
                target: Target::Focused
            }
        );
        assert!(trigger.is_held());
        assert_eq!(
            trigger.key_up(t0 + Duration::from_millis(120)),
            Action::None
        );
        assert!(!trigger.is_held());
        assert_eq!(
            trigger.key_down(t0 + Duration::from_secs(5), Target::Focused),
            Action::Stop
        );
        assert_eq!(trigger.key_up(t0 + Duration::from_secs(6)), Action::None);
    }

    #[test]
    fn hold_stops_on_release() {
        let mut trigger = Trigger::default();
        let t0 = Instant::now();
        assert_eq!(
            trigger.key_down(t0, Target::Capture),
            Action::Start {
                target: Target::Capture
            }
        );
        assert_eq!(
            trigger.key_down(t0 + Duration::from_millis(100), Target::Capture),
            Action::None
        );
        assert_eq!(trigger.key_up(t0 + HOLD_THRESHOLD), Action::Stop);
        assert_eq!(
            trigger.key_down(t0 + Duration::from_secs(2), Target::Focused),
            Action::Start {
                target: Target::Focused
            }
        );
    }

    #[test]
    fn a_session_started_without_a_key_runs_until_stopped() {
        let mut trigger = Trigger::default();
        let t0 = Instant::now();
        assert_eq!(trigger.stop(), Action::None);
        assert_eq!(
            trigger.start(Target::Capture),
            Action::Start {
                target: Target::Capture
            }
        );
        assert!(trigger.is_running() && !trigger.is_held());
        assert_eq!(trigger.start(Target::Focused), Action::None);
        // A hotkey press stops it like any latched session.
        assert_eq!(trigger.key_down(t0, Target::Focused), Action::Stop);
        assert_eq!(
            trigger.start(Target::Focused),
            Action::Start {
                target: Target::Focused
            }
        );
        assert_eq!(trigger.stop(), Action::Stop);
        assert!(!trigger.is_running());
    }

    #[test]
    fn a_session_that_ends_by_itself_resets_the_trigger() {
        let mut trigger = Trigger::default();
        let t0 = Instant::now();
        trigger.key_down(t0, Target::Focused);
        trigger.key_up(t0 + Duration::from_millis(50));
        trigger.session_ended();
        assert_eq!(
            trigger.key_down(t0 + Duration::from_secs(1), Target::Focused),
            Action::Start {
                target: Target::Focused
            }
        );
    }
}
