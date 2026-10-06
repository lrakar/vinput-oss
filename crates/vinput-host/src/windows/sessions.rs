//! Dictation sessions as the host runs them: a key or an API client starts
//! one, the tick timer watches the key and moves the indicator while it
//! runs, and what it leaves behind is passed on when it has ended.
//!
//! The session itself, listening and typing, is the controller's. This is
//! the part around it: the tray icon, the indicator, the state file other
//! tools read, and the events of the local API. Releases of the key are
//! found by polling it while a session runs; when idle there is no fast
//! timer and no CPU use.
//!
//! Escape calls a dictation off ([`Host::cancel`]): the session that listens
//! and every stopped one that is still at work on its last words. Nothing
//! more is typed, searched for or saved. The key is ours only while there is
//! such a session ([`Host::hold_cancel_key`]).
//!
//! That a session has ended is not polled for. Its thread posts
//! `SESSION_ENDED` as the last thing it does, whether the session was
//! stopped or ended by itself, and [`Ending`] says what that comes to. A
//! stopped session is therefore waited for with no timer at all.
#![allow(unsafe_code)]

use std::sync::Arc;
use std::time::Instant;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{KillTimer, PostMessageW, SetTimer};

use super::tray::{self, About};
use super::{Host, SESSION_ENDED, TICK_MS, TICK_TIMER_ID, hotkeys};
use crate::indicator::Indicator;
use crate::win::{is_down, open};
use vinput_core::api::{ClientId, ErrorCode, Event};
use vinput_core::session::{Action, Controller, Failure, Outcome, Target};
use vinput_core::settings::data_dir;
use vinput_core::stt::ProviderKind;
use vinput_core::{log, search};
use vinput_view::Page;

/// What it comes to for the host that a session has ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    /// The session the host shows as listening ended by itself, most
    /// likely with an error: listening is over.
    Broke,
    /// Nothing listens and every stopped session has done its work: the
    /// indicator can go.
    Finished,
    /// A stopped session has done its work while a newer one listens,
    /// which keeps the indicator it shows.
    Behind,
    /// Another stopped session is still at work.
    Waiting,
}

impl Ending {
    /// Looks at how things stand now rather than at which session ended, so
    /// hearing of an end twice, or of two ends at once, does no harm.
    /// `listening` is whether the host shows a session as listening.
    fn of(listening: bool, controller: &dyn Controller) -> Self {
        let active = controller.is_active();
        if listening && !active {
            Self::Broke
        } else if controller.is_finishing() {
            Self::Waiting
        } else if active {
            Self::Behind
        } else {
            Self::Finished
        }
    }
}

/// What the host finds when it hears that a session has ended: what that
/// comes to, and everything sessions have left behind since it last looked.
/// Each failure carries the id of its own session, so it does not matter
/// which session ended first or how many ended before the host looked.
#[derive(Debug, PartialEq)]
struct Left {
    ending: Ending,
    outcomes: Vec<Outcome>,
    failures: Vec<Failure>,
}

impl Left {
    fn of(listening: bool, controller: &mut dyn Controller) -> Self {
        Self {
            ending: Ending::of(listening, controller),
            outcomes: controller.take_outcomes(),
            failures: controller.take_failures(),
        }
    }
}

/// What programs connected to the local API are told of a failed session:
/// `session_failed`, with the id that session was started with.
fn failure_event(failure: &Failure) -> Event<'_> {
    Event::Error {
        code: ErrorCode::SessionFailed,
        message: &failure.message,
        id: failure.id.as_deref(),
    }
}

impl Host {
    /// Has the thread of each session wake the loop when the session has
    /// ended, so that nothing has to ask whether it has.
    pub(super) fn hear_sessions_end(&self, controller: &mut dyn Controller) {
        // A window handle is an address that any thread may post to.
        let window = self.hwnd as usize;
        controller.on_session_end(Arc::new(move || {
            // SAFETY: posting to a window that has gone away fails harmlessly.
            unsafe { PostMessageW(window as HWND, SESSION_ENDED, 0, 0) };
        }));
    }

    pub(super) fn on_hotkey(&mut self, id: usize, controller: &mut dyn Controller) {
        if hotkeys::is_cancel(id) {
            return self.cancel(controller);
        }
        let Some((target, hotkey)) = hotkeys::pressed(&self.settings, id) else {
            return;
        };
        let action = self.trigger.key_down(Instant::now(), target);
        self.act(action, hotkey.map(|hotkey| hotkey.vk), None, controller);
    }

    /// Starts or stops a session. `held_vk` is the key to watch for a release,
    /// when a key started it, and `requester` the API client that asked for it
    /// with the id it wants the session's events marked with.
    pub(super) fn act(
        &mut self,
        action: Action,
        held_vk: Option<u16>,
        requester: Option<(ClientId, Option<String>)>,
        controller: &mut dyn Controller,
    ) {
        match action {
            // Only a voice memo works without a speech model.
            Action::Start { target } if target != Target::Memo && !self.settings.has_model() => {
                self.trigger.session_ended();
                let message = "no speech model is chosen";
                match &requester {
                    Some((from, id)) => {
                        self.answer(*from, ErrorCode::NoModel, message, id.as_deref());
                    }
                    None => self.send_event(Event::Error {
                        code: ErrorCode::NoModel,
                        message,
                        id: None,
                    }),
                }
                self.tray.warn(
                    About::Settings(Page::Models),
                    "Choose a speech model first",
                    "VinPUT has no speech model yet. Click here to choose one.",
                );
            }
            Action::Start { target } => {
                let id = requester.and_then(|(_, id)| id);
                self.held_vk = held_vk;
                self.on_screen = !target.types_at_caret();
                self.session = Some((target, id.clone()));
                controller.start(target, id);
                // The indicator answers the key press itself: its first
                // frame is not left to the first tick. The caret is looked
                // for while the tray, the state file and the API are told,
                // none of which waits for it; then the frame is drawn.
                if let Some(indicator) = self.indicator_of(self.on_screen) {
                    indicator.begin();
                }
                self.set_listening(true);
                self.show_level(controller);
                self.hold_cancel_key(controller);
            }
            Action::Stop => self.stop(controller),
            Action::None => {}
        }
    }

    /// Passes on what finished sessions left behind: captured text and saved
    /// memos to the API, a search to the browser, a memo and an error to the
    /// user as well, under `title`.
    fn report(&mut self, title: &str, outcomes: Vec<Outcome>, failures: &[Failure]) {
        for outcome in outcomes {
            match outcome {
                Outcome::Captured { text, id } => self.send_event(Event::Dictation {
                    text: &text,
                    id: id.as_deref(),
                }),
                Outcome::Search { text } => self.search_for(&text),
                Outcome::Memo {
                    path,
                    seconds,
                    on_clipboard,
                    id,
                } => {
                    self.send_event(Event::Memo {
                        path: &path.to_string_lossy(),
                        seconds,
                        id: id.as_deref(),
                    });
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    let hint = if on_clipboard {
                        "Paste it anywhere with Ctrl+V."
                    } else {
                        "Find it under Open voice memos in the tray menu."
                    };
                    self.tray.inform(
                        About::Memo,
                        "Voice memo saved",
                        &format!("{name} ({}). {hint}", length(seconds)),
                    );
                }
            }
        }
        for failure in failures {
            self.send_event(failure_event(failure));
            // Nearly always it is the model or the access to its service.
            let page = if failure.provider == ProviderKind::Cloud {
                // The account may stand differently than when it was last
                // asked about; the pages are to show why this failed.
                self.pro_after_failure();
                Page::Pro
            } else if failure.message.contains("Advanced") {
                Page::Advanced
            } else {
                Page::Models
            };
            // A failure that comes back with every press is not said over
            // and over.
            self.tray
                .warn_of_failure(About::Settings(page), title, &failure.message);
        }
    }

    pub(super) fn on_tick(&mut self, controller: &mut dyn Controller) {
        // A tick that was on its way when listening ended.
        if !self.ticking {
            return;
        }
        if self.trigger.is_held()
            && self.held_vk.is_none_or(|vk| !is_down(vk))
            && self.trigger.key_up(Instant::now()) == Action::Stop
        {
            self.stop(controller);
            return;
        }
        if controller.is_active() {
            self.show_level(controller);
        } else {
            // The session ended by itself and said so with a message. Should
            // that message ever fail to arrive, this tick, which runs anyway
            // while the host shows a session as listening, makes up for it.
            self.on_session_ended(controller);
        }
    }

    /// Redraws the indicator of the running session with how loud it is now.
    fn show_level(&mut self, controller: &dyn Controller) {
        if !controller.is_active() {
            return;
        }
        let level = controller.level();
        if let Some(indicator) = self.indicator_of(self.on_screen) {
            indicator.update(level);
        }
    }

    pub(super) fn stop(&mut self, controller: &mut dyn Controller) {
        controller.stop();
        self.set_listening(false);
        // Listening is over, but the last words are still on their way: the
        // indicator says so until they have arrived.
        if let Some(indicator) = self.indicator_of(self.on_screen) {
            indicator.finish();
        }
        // The session still has its last words to type or its memo to save,
        // and says so with a message when it has. One that had ended before
        // it was stopped has said so already, and is seen off here.
        if !controller.is_finishing() {
            self.on_session_ended(controller);
        }
    }

    /// Calls off the session that is listening and every stopped one whose
    /// last words are still on their way: nothing more comes of them. What
    /// the user sees is over at once, the indicator included; the sessions
    /// themselves end when their recognizer lets go of them, and are seen
    /// off like any other, with nothing to pass on.
    pub(super) fn cancel(&mut self, controller: &mut dyn Controller) {
        controller.cancel();
        self.trigger.session_ended();
        self.set_listening(false);
        self.hide_indicators();
        if !controller.is_finishing() {
            self.on_session_ended(controller);
        }
        // Nothing is left to call off, so Escape goes back now and not
        // when the recognizer has answered.
        self.hold_cancel_key(controller);
    }

    /// Takes Escape while there is a session to call off and gives it back
    /// to the other programs the moment there is none.
    fn hold_cancel_key(&mut self, controller: &dyn Controller) {
        self.hotkeys.hold_cancel(controller.can_cancel());
    }

    /// A session has ended: what it leaves behind is passed on at once, and
    /// the indicator, the tray icon and what other programs are told follow.
    pub(super) fn on_session_ended(&mut self, controller: &mut dyn Controller) {
        let left = Left::of(self.ticking, controller);
        let title = match left.ending {
            Ending::Broke => {
                self.trigger.session_ended();
                self.set_listening(false);
                self.hide_indicators();
                "Vinput stopped listening"
            }
            Ending::Finished => {
                self.hide_indicators();
                "Vinput could not finish"
            }
            Ending::Behind => {
                if let Some(indicator) = self.indicator_of(!self.on_screen) {
                    indicator.hide();
                }
                "Vinput could not finish"
            }
            Ending::Waiting => "Vinput could not finish",
        };
        self.report(title, left.outcomes, &left.failures);
        self.pro_after_session();
        self.hold_cancel_key(controller);
    }

    /// The indicator at the bottom of the screen, or the one at the caret.
    fn indicator_of(&mut self, on_screen: bool) -> Option<&mut Indicator> {
        if on_screen {
            self.screen_indicator.as_mut()
        } else {
            self.indicator.as_mut()
        }
    }

    fn hide_indicators(&mut self) {
        for indicator in [&mut self.indicator, &mut self.screen_indicator]
            .into_iter()
            .flatten()
        {
            indicator.hide();
        }
    }

    /// Switches the tray icon, the tick timer and what other programs are
    /// told together. The indicator is left to whoever knows how the
    /// session ended.
    fn set_listening(&mut self, listening: bool) {
        if listening != self.ticking {
            // SAFETY: `hwnd` is our live window; the timer id is ours.
            unsafe {
                if listening {
                    SetTimer(self.hwnd, TICK_TIMER_ID, TICK_MS, None);
                } else {
                    KillTimer(self.hwnd, TICK_TIMER_ID);
                }
            }
            self.ticking = listening;
            publish_state(listening);
            let session = self.session.as_ref().filter(|_| listening);
            self.send_event(Event::State {
                session: session.map(|(target, _)| *target),
                id: session.and_then(|(_, id)| id.as_deref()),
            });
            self.show_state();
        }
        if !listening {
            self.held_vk = None;
        }
        let tip = tray::tooltip(&self.settings, self.pro.carrying_with(&self.settings));
        self.tray.show(listening, &tip);
    }

    /// Opens a web search for what was said in the default browser. Silence
    /// searches for nothing.
    fn search_for(&mut self, said: &str) {
        let Some(address) = search::url(&self.settings.search_url, said) else {
            return;
        };
        if !open(&address) {
            log::write("could not open the browser for a search");
            self.tray.warn(
                About::Settings(Page::Advanced),
                "Search not opened",
                "Windows could not open the search in a browser.",
            );
        }
    }
}

/// A length for people: `8 s`, `2 min 05 s`.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a recording length, rounded to whole seconds"
)]
fn length(seconds: f32) -> String {
    let total = seconds.round().max(0.0) as u32;
    if total < 60 {
        format!("{total} s")
    } else {
        format!("{} min {:02} s", total / 60, total % 60)
    }
}

/// Publishes whether Vinput is listening to `%APPDATA%\Vinput\state`, as the word
/// `listening` or `idle`, for other tools (such as a control deck) to show.
///
/// It is written only when the state changes, and through a temporary file that
/// is renamed into place, so a reader never sees a half-written value. Failures
/// are ignored: the state file is a courtesy and must never affect dictation.
pub(super) fn publish_state(listening: bool) {
    let Some(dir) = data_dir() else { return };
    let temporary = dir.join("state.tmp");
    let written = std::fs::create_dir_all(&dir)
        .and_then(|()| std::fs::write(&temporary, if listening { "listening" } else { "idle" }));
    if written.is_ok() {
        let _ = std::fs::rename(&temporary, dir.join("state"));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use vinput_core::session::Ended;
    use vinput_core::settings::Settings;

    /// A controller whose sessions end when the test says so. Like the real
    /// one it tells of each end after what the session left is in place.
    #[derive(Default)]
    struct Fake {
        active: bool,
        /// Stopped sessions that have not ended.
        finishing: usize,
        /// Whether what is running was called off.
        cancelled: bool,
        outcomes: Vec<Outcome>,
        failures: Vec<Failure>,
        notify: Option<Ended>,
    }

    impl Fake {
        /// A controller, and how often it has told of a session's end.
        fn heard() -> (Self, Arc<AtomicUsize>) {
            let told = Arc::new(AtomicUsize::new(0));
            let mut fake = Self::default();
            let counted = Arc::clone(&told);
            fake.on_session_end(Arc::new(move || {
                counted.fetch_add(1, Ordering::Relaxed);
            }));
            (fake, told)
        }

        fn tell(&self) {
            if let Some(notify) = &self.notify {
                notify();
            }
        }

        /// The session that is listening ends by itself.
        fn breaks(&mut self, error: &str) {
            self.active = false;
            self.failures.push(failed(error, None));
            self.tell();
        }

        /// A stopped session, started with `id`, fails at its last words.
        fn fails(&mut self, id: &str, error: &str) {
            self.finishing -= 1;
            self.failures.push(failed(error, Some(id)));
            self.tell();
        }

        /// A stopped session has done its work.
        fn delivers(&mut self, outcome: Option<Outcome>) {
            self.finishing -= 1;
            self.outcomes.extend(outcome);
            self.tell();
        }
    }

    impl Controller for Fake {
        fn start(&mut self, _target: Target, _id: Option<String>) {
            self.active = true;
            self.cancelled = false;
        }

        fn stop(&mut self) {
            if std::mem::take(&mut self.active) {
                self.finishing += 1;
            }
        }

        fn cancel(&mut self) {
            self.stop();
            self.cancelled = true;
        }

        fn can_cancel(&self) -> bool {
            self.active || (self.finishing > 0 && !self.cancelled)
        }

        fn is_active(&self) -> bool {
            self.active
        }

        fn is_finishing(&self) -> bool {
            self.finishing > 0
        }

        fn on_session_end(&mut self, notify: Ended) {
            self.notify = Some(notify);
        }

        fn level(&self) -> f32 {
            0.0
        }

        fn take_failures(&mut self) -> Vec<Failure> {
            std::mem::take(&mut self.failures)
        }

        fn take_outcomes(&mut self) -> Vec<Outcome> {
            std::mem::take(&mut self.outcomes)
        }

        fn configure(&mut self, _settings: &Settings, _reconnect: bool) {}
    }

    fn failed(message: &str, id: Option<&str>) -> Failure {
        Failure {
            message: message.to_owned(),
            provider: ProviderKind::Local,
            id: id.map(str::to_owned),
        }
    }

    /// Two capture sessions of an API client, "a" and then "b", both
    /// stopped and both with their last words still on their way.
    fn two_stopped() -> Fake {
        let (mut fake, _) = Fake::heard();
        for id in ["a", "b"] {
            fake.start(Target::Capture, Some(id.to_owned()));
            fake.stop();
        }
        fake
    }

    #[test]
    fn each_of_two_failing_sessions_is_told_of_with_its_own_id() {
        // The service is down. "a" fails first: the host hears of it while
        // "b" is still at work, and says so with the id of "a".
        let mut fake = two_stopped();
        fake.fails("a", "no connection");
        let left = Left::of(false, &mut fake);
        assert_eq!(left.ending, Ending::Waiting);
        assert_eq!(left.failures, [failed("no connection", Some("a"))]);
        assert_eq!(
            failure_event(&left.failures[0]),
            Event::Error {
                code: ErrorCode::SessionFailed,
                message: "no connection",
                id: Some("a"),
            }
        );
        fake.fails("b", "no connection");
        let left = Left::of(false, &mut fake);
        assert_eq!(left.ending, Ending::Finished);
        assert_eq!(left.failures, [failed("no connection", Some("b"))]);
    }

    #[test]
    fn the_later_session_failing_first_changes_nothing_about_the_ids() {
        let mut fake = two_stopped();
        fake.fails("b", "timed out");
        let left = Left::of(false, &mut fake);
        assert_eq!(left.ending, Ending::Waiting);
        assert_eq!(left.failures, [failed("timed out", Some("b"))]);
        fake.fails("a", "no connection");
        let left = Left::of(false, &mut fake);
        assert_eq!(left.ending, Ending::Finished);
        assert_eq!(left.failures, [failed("no connection", Some("a"))]);
        // Hearing of an end once more finds nothing to say again.
        let again = Left::of(false, &mut fake);
        assert!(again.failures.is_empty() && again.outcomes.is_empty());
    }

    #[test]
    fn two_failures_before_the_host_looks_are_both_passed_on() {
        let mut fake = two_stopped();
        fake.fails("a", "no connection");
        fake.fails("b", "timed out");
        let left = Left::of(false, &mut fake);
        assert_eq!(left.ending, Ending::Finished);
        assert_eq!(
            left.failures,
            [
                failed("no connection", Some("a")),
                failed("timed out", Some("b"))
            ]
        );
        let ids: Vec<_> = left
            .failures
            .iter()
            .map(|failure| match failure_event(failure) {
                Event::Error { id, .. } => id,
                _ => None,
            })
            .collect();
        assert_eq!(ids, [Some("a"), Some("b")]);
    }

    #[test]
    fn a_failure_behind_a_listening_session_carries_its_own_id_not_that_one() {
        let (mut fake, _) = Fake::heard();
        fake.start(Target::Capture, Some("a".to_owned()));
        fake.stop();
        fake.start(Target::Capture, Some("b".to_owned()));
        fake.fails("a", "no connection");
        let left = Left::of(true, &mut fake);
        assert_eq!(left.ending, Ending::Behind);
        assert_eq!(left.failures, [failed("no connection", Some("a"))]);
        // A session started with no id fails with none.
        fake.breaks("the microphone is gone");
        let left = Left::of(true, &mut fake);
        assert_eq!(left.ending, Ending::Broke);
        assert_eq!(left.failures, [failed("the microphone is gone", None)]);
    }

    fn captured(text: &str) -> Outcome {
        Outcome::Captured {
            text: text.to_owned(),
            id: None,
        }
    }

    #[test]
    fn a_stopped_session_is_seen_off_when_it_says_it_has_ended() {
        let (mut fake, told) = Fake::heard();
        fake.start(Target::Capture, None);
        // The host stops it and no longer shows it as listening. Nothing is
        // heard, and nothing asked, while the last words are on their way.
        fake.stop();
        assert_eq!(told.load(Ordering::Relaxed), 0);
        assert_eq!(Ending::of(false, &fake), Ending::Waiting);
        fake.delivers(Some(captured("hello")));
        assert_eq!(told.load(Ordering::Relaxed), 1);
        // What it left is there to pass on by the time the host hears.
        assert_eq!(Ending::of(false, &fake), Ending::Finished);
        assert_eq!(fake.take_outcomes(), [captured("hello")]);
    }

    #[test]
    fn a_session_that_ends_by_itself_ends_the_listening() {
        let (mut fake, told) = Fake::heard();
        fake.start(Target::Focused, None);
        fake.breaks("the microphone is gone");
        assert_eq!(told.load(Ordering::Relaxed), 1);
        assert_eq!(Ending::of(true, &fake), Ending::Broke);
        assert_eq!(
            fake.take_failures(),
            [failed("the microphone is gone", None)]
        );
        // The host has taken it for over. Hearing of the same end again,
        // as a wake that was replayed after the tray menu is, changes
        // nothing: there is nothing left to hide or to pass on.
        assert_eq!(Ending::of(false, &fake), Ending::Finished);
        assert!(fake.take_outcomes().is_empty() && fake.take_failures().is_empty());
    }

    #[test]
    fn a_session_that_broke_with_another_still_at_work_is_over_all_the_same() {
        let (mut fake, _) = Fake::heard();
        fake.start(Target::Capture, None);
        fake.stop();
        fake.start(Target::Focused, None);
        fake.breaks("no connection");
        assert_eq!(Ending::of(true, &fake), Ending::Broke);
        // The earlier one is waited for as any stopped session is.
        assert_eq!(Ending::of(false, &fake), Ending::Waiting);
        fake.delivers(Some(captured("earlier")));
        assert_eq!(Ending::of(false, &fake), Ending::Finished);
    }

    #[test]
    fn a_session_that_ends_behind_a_newer_one_leaves_that_one_listening() {
        let (mut fake, told) = Fake::heard();
        fake.start(Target::Capture, None);
        fake.stop();
        // The next session starts while the last words of the first are on
        // their way, and is stopped too.
        fake.start(Target::Focused, None);
        fake.delivers(Some(captured("first")));
        assert_eq!(Ending::of(true, &fake), Ending::Behind);
        // Its text does not wait for the newer session to end.
        assert_eq!(fake.take_outcomes(), [captured("first")]);

        fake.stop();
        fake.start(Target::Capture, None);
        assert_eq!(Ending::of(true, &fake), Ending::Waiting);
        fake.delivers(None);
        assert_eq!(Ending::of(true, &fake), Ending::Behind);
        fake.stop();
        assert_eq!(Ending::of(false, &fake), Ending::Waiting);
        fake.delivers(Some(captured("third")));
        assert_eq!(Ending::of(false, &fake), Ending::Finished);
        assert_eq!(told.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn a_session_that_had_ended_before_it_was_stopped_is_not_waited_for() {
        let (mut fake, _) = Fake::heard();
        fake.start(Target::Memo, None);
        // It ends by itself, and the key that stops it is heard before the
        // message that says so.
        fake.active = false;
        fake.stop();
        assert!(!fake.is_finishing());
        assert_eq!(Ending::of(false, &fake), Ending::Finished);
    }

    #[test]
    fn recording_lengths_read_naturally() {
        assert_eq!(length(7.6), "8 s");
        assert_eq!(length(125.2), "2 min 05 s");
    }
}
