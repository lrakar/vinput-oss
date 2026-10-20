//! Runs sessions: each one captures the microphone and either streams it to
//! the recognizer and types the result or hands it on, or records it as a voice
//! memo, on its own thread.
//!
//! What was recognized may be tidied by a language model before it is typed
//! ([`crate::rewrite`]), in the way the settings name for the key that
//! started the session. And a session can be an instruction instead of a
//! text ([`Target::Command`]): what is selected in the focused window is
//! taken when the key goes down, what is said is carried out on it, and the
//! result is put in its place ([`command`]).

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, Scope};
use std::time::{Duration, Instant};

use crate::audio::{ArmedMicrophone, AudioSource, LevelMeter, Metered, StopHandle, UntilQuiet};
use crate::inject::live::erasable;
use crate::inject::{SendInputSink, TextSink};
use crate::rewrite::shown::{put_over_selection, type_tidied};
use crate::rewrite::{self, Job, Kind, Mode, Rewriter};
use crate::session::{Controller, Ended, Failure, Outcome, Said, Target, Told, TypingEnter};
use crate::settings::{Settings, SpeechConfig};
use crate::sound::{self, Cue};
use crate::stt::deferred::{Deferred, MAX_WAITING_SAMPLES, Session};
use crate::stt::{self, ProviderKind, SessionOptions, SpeechProvider};
use crate::words::{self, Rules, Shaped};
use crate::{Error, Result, log, memo, pipeline};

/// A running session: the switch that ends it, whether it was called off,
/// whether the user pressed Enter while it listened, and whether its thread
/// has left behind everything it is going to.
struct Active {
    stop: StopHandle,
    cancelled: Arc<AtomicBool>,
    entered: Arc<AtomicBool>,
    ended: Arc<AtomicBool>,
}

/// The last thing the thread of a session does: it puts what the session
/// leaves behind where the host looks for it, says that the session has
/// ended, and then calls whoever asked to be told. In that order, so that
/// whoever is told finds everything in place.
///
/// Being dropped is what says so and tells, so a session whose thread could
/// not be started, or did not get as far as [`Closing::with`], ends too.
struct Closing {
    outcomes: Arc<Mutex<Vec<Outcome>>>,
    failures: Arc<Mutex<Vec<Failure>>>,
    /// The id the session was started with, which its failure carries.
    id: Option<String>,
    /// The speech service the session uses, which its failure names.
    provider: ProviderKind,
    ended: Arc<AtomicBool>,
    notify: Option<Ended>,
}

impl Closing {
    /// Ends the session with what running it came to.
    fn with(self, result: Result<Option<Outcome>>) {
        match result {
            Ok(None) => {}
            Ok(Some(outcome)) => self
                .outcomes
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(outcome),
            Err(err) => {
                log::write(&format!("session stopped: {err}"));
                self.failures
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(Failure {
                        message: err.to_string(),
                        provider: self.provider,
                        setup: matches!(err, Error::Setup(_)),
                        id: self.id.clone(),
                    });
            }
        }
    }
}

impl Drop for Closing {
    fn drop(&mut self) {
        self.ended.store(true, Ordering::Release);
        if let Some(notify) = &self.notify {
            notify();
        }
    }
}

/// What a session needs: what the settings said when it started, and the
/// microphone, which every session shares.
#[derive(Clone)]
#[expect(clippy::struct_excessive_bools, reason = "independent on/off switches")]
struct Options {
    languages: Vec<String>,
    /// Whether a sound says that listening has started, and that it has
    /// stopped.
    start_sound: bool,
    stop_sound: bool,
    /// After how many minutes without a sound a dictation stops by itself.
    stop_after_minutes: u32,
    /// The configured microphone. For a while after a session its stream is
    /// kept, stopped, so that the next session only has to start it.
    microphone: Arc<ArmedMicrophone>,
    /// Whether the key that started the session is still being held down.
    /// Whoever holds a key is there, so such a dictation is not ended for
    /// being quiet; one that goes on after a tap, or was started by a
    /// program, is.
    held: Arc<AtomicBool>,
    memo_dir: String,
    memo_to_clipboard: bool,
    /// Whether words are typed while the recognizer may still change them.
    live: bool,
    /// Told around every Enter a session types.
    typing_enter: Option<TypingEnter>,
    /// Where a dictation leaves what it has typed so far, for programs
    /// connected to the local API. `None` unless the settings let them read
    /// it.
    said: Option<Arc<Transcripts>>,
    /// What the recognized text is put into the user's own words with: the
    /// dictionary, the snippets and spoken formatting. `None` if there is
    /// nothing of them.
    rules: Option<Arc<Rules>>,
    /// The same for a dictation that a model tidies, and for an instruction
    /// to the key that edits: what is put right before the model is given
    /// the text (the dictionary and spoken formatting), and what after it has
    /// written it (the snippets, which it would otherwise rewrite).
    around_a_model: (Option<Arc<Rules>>, Option<Arc<Rules>>),
    /// What is done with a dictation before it is typed: of the key that
    /// types, and of the one that types and presses Enter.
    cleanup: (Mode, Mode),
    /// What is done with the dictation of each action of the user's own
    /// before it is handed over, in the order of the settings.
    action_cleanup: Vec<Mode>,
    /// The model that tidies and edits.
    rewriter: Arc<dyn Rewriter>,
    /// How many dictations a model on this PC has typed something for since
    /// the host last asked. Kept on this PC and never sent anywhere: the
    /// host counts them towards the one time it mentions `VinPUT` Pro.
    typed_locally: Arc<AtomicU32>,
}

impl Options {
    fn of(
        settings: &Settings,
        microphone: Arc<ArmedMicrophone>,
        held: Arc<AtomicBool>,
        rewriter: Arc<dyn Rewriter>,
    ) -> Self {
        Self {
            held,
            cleanup: (settings.cleanup, settings.cleanup_enter),
            action_cleanup: settings
                .actions
                .iter()
                .map(|action| action.cleanup)
                .collect(),
            rewriter,
            languages: settings.languages.clone(),
            start_sound: settings.ready_sound,
            stop_sound: settings.stop_sound,
            stop_after_minutes: settings.stop_after_minutes,
            microphone,
            memo_dir: settings.memo_dir.clone(),
            memo_to_clipboard: settings.memo_to_clipboard,
            live: settings.live_typing,
            typing_enter: None,
            said: None,
            rules: Rules::of(&settings.words),
            around_a_model: Rules::around_a_model(&settings.words),
            typed_locally: Arc::default(),
        }
    }

    /// What a model is asked to make of a dictation to `target` before it
    /// is typed or handed to an action; `None` for one that is used as it
    /// is said, and for every other session.
    fn tidying(&self, target: Target) -> Option<Kind> {
        match target {
            Target::Focused => self.cleanup.0.kind(),
            Target::FocusedEnter => self.cleanup.1.kind(),
            Target::Action(slot) => self
                .action_cleanup
                .get(usize::from(slot))
                .and_then(|mode| mode.kind()),
            _ => None,
        }
    }

    /// Whether a session to `target` will ask the model anything.
    fn rewrites(&self, target: Target) -> bool {
        target == Target::Command || self.tidying(target).is_some()
    }
}

/// The part of the settings that decides which model rewrites and where it
/// runs. When it changes, the model is chosen again.
type RewriteConfig = (String, bool);

fn rewrite_config(settings: &Settings) -> RewriteConfig {
    (settings.text_model.clone(), settings.use_gpu())
}

/// Has the model that rewrites ready if the settings ask for anything to
/// be rewritten, and lets go of the one on this PC if they no longer ask
/// for it.
fn ready_to_rewrite(settings: &Settings, rewriter: &dyn Rewriter) {
    let in_use = rewrite::in_use(settings);
    if in_use {
        rewriter.make_ready();
    }
    if !in_use || rewrite::uses_pro(settings) {
        rewrite::local::engine().end();
    }
}

/// Starts and stops sessions on a shared speech provider.
pub struct Dictation {
    speech: Arc<dyn SpeechProvider>,
    /// What `speech` was set up from, to notice when it has to be set up again.
    speech_config: SpeechConfig,
    /// What the model that rewrites was chosen from.
    rewrite_config: RewriteConfig,
    options: Options,
    meter: LevelMeter,
    /// Where failed sessions leave their errors for the host to show, each
    /// with the id of its session: one failure never takes the place of
    /// another.
    failures: Arc<Mutex<Vec<Failure>>>,
    /// Where finished sessions leave what the host has to pass on.
    outcomes: Arc<Mutex<Vec<Outcome>>>,
    /// Where dictations leave what they have typed so far.
    said: Arc<Transcripts>,
    active: Option<Active>,
    /// Stopped sessions that may still be flushing or saving: for each, the
    /// switch that calls it off and whether it has ended.
    finishing: Vec<(Arc<AtomicBool>, Arc<AtomicBool>)>,
    /// Called on the thread of a session when that session has ended.
    notify: Option<Ended>,
}

impl std::fmt::Debug for Dictation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dictation")
            .field("active", &self.is_active())
            .finish_non_exhaustive()
    }
}

impl Dictation {
    /// Starts setting up the speech provider the settings choose, in the
    /// background. No session is running yet, and the microphone is not
    /// opened until one is.
    #[must_use]
    pub fn new(settings: &Settings) -> Self {
        let microphone = Arc::new(ArmedMicrophone::new(&settings.microphone));
        let said = Arc::<Transcripts>::default();
        words::listen_for(settings.words.terms());
        let rewriter = rewrite::connect(settings);
        ready_to_rewrite(settings, rewriter.as_ref());
        Self {
            speech: stt::connect_in_background(settings),
            speech_config: settings.speech_config(),
            rewrite_config: rewrite_config(settings),
            options: Options {
                said: said_for(settings, &said),
                ..Options::of(settings, microphone, Arc::default(), rewriter)
            },
            meter: LevelMeter::new(),
            failures: Arc::default(),
            outcomes: Arc::default(),
            said,
            active: None,
            finishing: Vec::new(),
            notify: None,
        }
    }
}

impl Controller for Dictation {
    fn start(&mut self, target: Target, id: Option<String>) {
        if self.is_active() {
            return;
        }
        // The sound answers the key press itself, like the indicator: it is
        // not left to the session's thread, which has a microphone to open
        // first. Without one, the way to the speakers is still made ready
        // for the sound of the stop.
        if self.options.start_sound {
            sound::play(Cue::Start);
        } else if self.options.stop_sound {
            sound::warm();
        }
        // The model that rewrites is made ready while the user speaks.
        if self.options.rewrites(target) {
            self.options.rewriter.make_ready();
        }
        let stop = StopHandle::new();
        let speech = Arc::clone(&self.speech);
        let options = self.options.clone();
        let meter = self.meter.clone();
        let failures = Arc::clone(&self.failures);
        let outcomes = Arc::clone(&self.outcomes);
        let worker_stop = stop.clone();
        let ended = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::new(AtomicBool::new(false));
        let called_off = Arc::clone(&cancelled);
        let entered = Arc::new(AtomicBool::new(false));
        let pressed = Arc::clone(&entered);
        let closing = Closing {
            outcomes,
            failures,
            id: id.clone(),
            provider: speech.kind(),
            ended: Arc::clone(&ended),
            notify: self.notify.clone(),
        };
        let spawned = thread::Builder::new()
            .name("vinput-dictation".into())
            .spawn(move || {
                let result = run_session(
                    speech.as_ref(),
                    &options,
                    worker_stop,
                    meter.clone(),
                    target,
                    id.clone(),
                    (&called_off, &pressed),
                );
                meter.reset();
                closing.with(if called_off.load(Ordering::Acquire) {
                    Ok(called_off_outcome(target, id))
                } else {
                    result
                });
            });
        match spawned {
            Ok(_) => {
                self.active = Some(Active {
                    stop,
                    cancelled,
                    entered,
                    ended,
                });
            }
            Err(err) => log::write(&format!("could not start a session: {err}")),
        }
    }

    fn held(&mut self, held: bool) {
        self.options.held.store(held, Ordering::Relaxed);
    }

    fn stop(&mut self) {
        // The worker is left to flush and type its last words on its own; letting
        // go of it here means a new session can start straight away.
        if let Some(active) = self.active.take() {
            // A dictation nobody spoke into has stopped by itself, and
            // makes the sound of it when its microphone is let go of,
            // though its last words may still be on their way.
            let stopped = active.stop.is_stopped();
            active.stop.stop();
            // After the microphone was told to stop, so that it hears as
            // little of the sound as can be. A session that had ended by
            // itself has nothing left to stop.
            if self.options.stop_sound && !stopped && !active.ended.load(Ordering::Acquire) {
                sound::play(Cue::Stop);
            }
            self.finishing
                .retain(|(_, ended)| !ended.load(Ordering::Acquire));
            self.finishing.push((active.cancelled, active.ended));
        }
    }

    fn enter(&mut self) {
        if let Some(active) = &self.active {
            active.entered.store(true, Ordering::Release);
        }
    }

    fn cancel(&mut self) {
        // Called off before it is stopped, so that whatever the session
        // does on its way out, it does knowing.
        if let Some(active) = &self.active {
            active.cancelled.store(true, Ordering::Release);
        }
        self.stop();
        for (cancelled, _) in &self.finishing {
            cancelled.store(true, Ordering::Release);
        }
    }

    fn can_cancel(&self) -> bool {
        self.is_active()
            || self.finishing.iter().any(|(cancelled, ended)| {
                !ended.load(Ordering::Acquire) && !cancelled.load(Ordering::Acquire)
            })
    }

    fn is_active(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| !active.ended.load(Ordering::Acquire))
    }

    fn is_finishing(&self) -> bool {
        self.finishing
            .iter()
            .any(|(_, ended)| !ended.load(Ordering::Acquire))
    }

    fn on_session_end(&mut self, notify: Ended) {
        self.notify = Some(notify);
    }

    fn on_typing_enter(&mut self, around: TypingEnter) {
        self.options.typing_enter = Some(around);
    }

    fn level(&self) -> f32 {
        self.meter.level()
    }

    fn take_failures(&mut self) -> Vec<Failure> {
        std::mem::take(&mut *self.failures.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn take_outcomes(&mut self) -> Vec<Outcome> {
        std::mem::take(&mut *self.outcomes.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn take_local_dictations(&mut self) -> u32 {
        self.options.typed_locally.swap(0, Ordering::AcqRel)
    }

    fn take_said(&mut self) -> Vec<Said> {
        // A dictation that started while programs were let read what is
        // typed goes on leaving it here after they no longer are. It is
        // let fall: nothing is passed on from the moment the user says so.
        if self.options.said.is_some() {
            self.said.take_due(Instant::now())
        } else {
            self.said.take();
            Vec::new()
        }
    }

    fn configure(&mut self, settings: &Settings, reconnect: bool) {
        let microphone = Arc::clone(&self.options.microphone);
        microphone.set_microphone(&settings.microphone);
        let held = Arc::clone(&self.options.held);
        let config = rewrite_config(settings);
        let rewriter = if config == self.rewrite_config {
            Arc::clone(&self.options.rewriter)
        } else {
            rewrite::connect(settings)
        };
        self.rewrite_config = config;
        ready_to_rewrite(settings, rewriter.as_ref());
        let typing_enter = self.options.typing_enter.take();
        // The count goes on across changes of the settings.
        let typed_locally = Arc::clone(&self.options.typed_locally);
        self.options = Options::of(settings, microphone, held, rewriter);
        self.options.typing_enter = typing_enter;
        self.options.typed_locally = typed_locally;
        self.options.said = said_for(settings, &self.said);
        // The dictation that starts next asks for the words as they are now.
        words::listen_for(settings.words.terms());
        let speech_config = settings.speech_config();
        if reconnect || speech_config != self.speech_config {
            // Sessions still running keep the provider they started with.
            self.speech = stt::connect_in_background(settings);
            self.speech_config = speech_config;
        }
    }
}

/// Counts what passes through to the real sink, to know whether anything was typed.
struct Counting<S> {
    sink: S,
    typed: usize,
}

impl<S: TextSink> TextSink for Counting<S> {
    fn type_text(&mut self, text: &str) -> Result<()> {
        self.typed += text.trim().len();
        self.sink.type_text(text)
    }

    fn rewrites(&self) -> bool {
        self.sink.rewrites()
    }

    fn rewrite(&mut self, erase: usize, text: &str) -> Result<()> {
        // Near enough: all that is asked of the count is whether anything
        // is left of what was typed.
        self.typed = self.typed.saturating_sub(erase) + text.trim().len();
        self.sink.rewrite(erase, text)
    }

    fn disturbed(&mut self) -> bool {
        self.sink.disturbed()
    }

    fn paste(&mut self, text: &str) -> Result<bool> {
        let pasted = self.sink.paste(text)?;
        if pasted {
            self.typed += text.trim().len();
        }
        Ok(pasted)
    }

    fn type_lines(&mut self, text: &str) -> Result<()> {
        self.typed += text.trim().len();
        self.sink.type_lines(text)
    }
}

/// Passes text on to the real sink until the session is called off, and
/// lets it fall from then on: the words the recognizer still comes up with
/// are the ones the user no longer wants.
struct UnlessCancelled<'a, S> {
    sink: S,
    cancelled: &'a AtomicBool,
}

impl<S: TextSink> TextSink for UnlessCancelled<'_, S> {
    fn type_text(&mut self, text: &str) -> Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        self.sink.type_text(text)
    }

    fn rewrites(&self) -> bool {
        self.sink.rewrites()
    }

    /// A guess that is on the screen when the session is called off stays
    /// there: nothing more is typed, and Backspace is typing too.
    fn rewrite(&mut self, erase: usize, text: &str) -> Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        self.sink.rewrite(erase, text)
    }

    fn disturbed(&mut self) -> bool {
        self.sink.disturbed()
    }

    /// Said to be done, so that nothing is typed in its place.
    fn paste(&mut self, text: &str) -> Result<bool> {
        if self.cancelled.load(Ordering::Acquire) {
            return Ok(true);
        }
        self.sink.paste(text)
    }

    fn type_lines(&mut self, text: &str) -> Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        self.sink.type_lines(text)
    }
}

/// Says so around every Enter that is typed, to whoever keeps that key from
/// other programs while a dictation listens: an Enter typed here is a key
/// press like the user's own, and would be kept from the program it is for.
struct Announcing<S> {
    sink: S,
    enter: Option<TypingEnter>,
}

impl<S: TextSink> TextSink for Announcing<S> {
    fn type_text(&mut self, text: &str) -> Result<()> {
        self.rewrite(0, text)
    }

    fn rewrites(&self) -> bool {
        self.sink.rewrites()
    }

    fn rewrite(&mut self, erase: usize, text: &str) -> Result<()> {
        let enter = self
            .enter
            .as_ref()
            .filter(|_| text.bytes().any(|byte| matches!(byte, b'\n' | b'\r')));
        if let Some(enter) = enter {
            enter(true);
        }
        let typed = self.sink.rewrite(erase, text);
        if let Some(enter) = enter {
            enter(false);
        }
        typed
    }

    fn disturbed(&mut self) -> bool {
        self.sink.disturbed()
    }

    /// Pasting presses no Enter.
    fn paste(&mut self, text: &str) -> Result<bool> {
        self.sink.paste(text)
    }

    /// Nor does a line break that sends nothing.
    fn type_lines(&mut self, text: &str) -> Result<()> {
        self.sink.type_lines(text)
    }
}

/// Where dictations leave what they have typed so far, until it is taken.
/// Only the latest of each dictation is kept: what it typed before is in it.
#[derive(Default)]
struct Transcripts {
    /// How many dictations have been given a number.
    sessions: AtomicU64,
    waiting: Mutex<Vec<Said>>,
    /// When what waited was last handed on.
    handed_on: Mutex<Option<Instant>>,
}

/// How long what is typed waits before it is handed on again while a
/// dictation goes on. Words that are typed as they are said change many
/// times a second; a program that shows them does not need each letter, and
/// one that is slow to read must not lose other events for them.
const SAID_EVERY: Duration = Duration::from_millis(100);

impl Transcripts {
    /// The number of the dictation that starts now.
    fn next_session(&self) -> u64 {
        self.sessions.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn leave(&self, said: Said) {
        let mut waiting = self.waiting.lock().unwrap_or_else(PoisonError::into_inner);
        match waiting.iter_mut().find(|left| left.session == said.session) {
            Some(left) => *left = said,
            None => waiting.push(said),
        }
    }

    fn take(&self) -> Vec<Said> {
        std::mem::take(&mut *self.waiting.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Takes what waits if it is time to at `now`: [`SAID_EVERY`] has passed
    /// since something was last taken, or a dictation has ended, which is
    /// said at once.
    fn take_due(&self, now: Instant) -> Vec<Said> {
        let mut handed_on = (self.handed_on.lock()).unwrap_or_else(PoisonError::into_inner);
        let mut waiting = self.waiting.lock().unwrap_or_else(PoisonError::into_inner);
        let soon = handed_on.is_some_and(|then| now.saturating_duration_since(then) < SAID_EVERY);
        if waiting.is_empty() || (soon && !waiting.iter().any(|said| said.last)) {
            return Vec::new();
        }
        *handed_on = Some(now);
        std::mem::take(&mut *waiting)
    }
}

/// Where dictations leave what they type, if the settings let programs
/// connected to the local API read it.
fn said_for(settings: &Settings, said: &Arc<Transcripts>) -> Option<Arc<Transcripts>> {
    (settings.api_enabled && settings.api_transcript).then(|| Arc::clone(said))
}

/// Keeps what passes through to the real sink, and leaves all of it where
/// the host finds it: each time it changes, and once more, marked as the
/// last, when the dictation is over. It sits right above the real sink, so
/// what it keeps is what was typed, in the user's own words, and nothing
/// that was let fall on the way. Without `said` it keeps nothing.
struct Telling<S> {
    sink: S,
    typed: String,
    /// Where it is left, and the number of this dictation there.
    said: Option<(Arc<Transcripts>, u64)>,
    target: Target,
    id: Option<String>,
}

impl<S> Telling<S> {
    fn new(sink: S, said: Option<&Arc<Transcripts>>, target: Target, id: Option<String>) -> Self {
        Self {
            sink,
            typed: String::new(),
            said: said.map(|said| (Arc::clone(said), said.next_session())),
            target,
            id,
        }
    }

    /// `text` was put at the caret, after what was there.
    fn told(&mut self, text: &str) {
        if self.said.is_some() {
            self.typed.push_str(text);
            self.leave(false);
        }
    }

    fn leave(&self, last: bool) {
        if let Some((said, session)) = &self.said {
            said.leave(Said {
                session: *session,
                target: self.target,
                id: self.id.clone(),
                text: self.typed.clone(),
                last,
            });
        }
    }
}

impl<S: TextSink> TextSink for Telling<S> {
    fn type_text(&mut self, text: &str) -> Result<()> {
        self.rewrite(0, text)
    }

    fn rewrites(&self) -> bool {
        self.sink.rewrites()
    }

    fn rewrite(&mut self, erase: usize, text: &str) -> Result<()> {
        self.sink.rewrite(erase, text)?;
        if self.said.is_some() {
            for _ in 0..erase {
                self.typed.pop();
            }
            self.told(text);
        }
        Ok(())
    }

    fn disturbed(&mut self) -> bool {
        self.sink.disturbed()
    }

    fn paste(&mut self, text: &str) -> Result<bool> {
        let pasted = self.sink.paste(text)?;
        if pasted {
            self.told(text);
        }
        Ok(pasted)
    }

    fn type_lines(&mut self, text: &str) -> Result<()> {
        self.sink.type_lines(text)?;
        self.told(text);
        Ok(())
    }
}

/// The dictation is over, however it ended. One that types where the caret
/// is says so even if it typed nothing, so that whoever shows its text
/// knows to stop; a search or a captured dictation only if it came to type.
impl<S> Drop for Telling<S> {
    fn drop(&mut self) {
        if self.target.types_at_caret() || !self.typed.is_empty() {
            self.leave(true);
        }
    }
}

/// What a session that was called off leaves behind: nothing, except for
/// the program that asked for its text and waits to hear that it is over.
fn called_off_outcome(target: Target, id: Option<String>) -> Option<Outcome> {
    (target == Target::Capture).then(|| Outcome::Captured {
        text: String::new(),
        id,
    })
}

/// Keeps the text instead of typing it, for sessions that hand it on at the end.
#[derive(Default)]
struct Collecting {
    text: String,
}

impl TextSink for Collecting {
    fn type_text(&mut self, text: &str) -> Result<()> {
        self.text.push_str(text);
        Ok(())
    }
}

/// Runs one session to its end. A typed dictation has put its text where it
/// belongs by then; the other kinds return what the host has to pass on.
fn run_session(
    speech: &dyn SpeechProvider,
    options: &Options,
    stop: StopHandle,
    meter: LevelMeter,
    target: Target,
    id: Option<String>,
    (cancelled, entered): (&AtomicBool, &AtomicBool),
) -> Result<Option<Outcome>> {
    if target == Target::Memo {
        return record_memo(options, stop, meter, id, cancelled);
    }
    let alone = AtomicBool::new(false);
    let microphone = || {
        let microphone = listen(options, stop.clone(), meter)?;
        Ok(while_spoken_to(microphone, options, stop, &alone))
    };
    let sink = UnlessCancelled {
        sink: typing_into(SendInputSink::new(), options, target, id.clone()),
        cancelled,
    };
    let tidying = options.tidying(target);
    let rules = if tidying.is_some() || target == Target::Command {
        options.around_a_model.0.clone()
    } else {
        options.rules.clone()
    };
    let speech = &InOwnWords { speech, rules };
    if target == Target::Command {
        let told = command(
            speech,
            &options.languages,
            microphone,
            (&Focused, options.rewriter.as_ref()),
            sink,
            cancelled,
        )?;
        return Ok(told.map(Outcome::Told));
    }
    let tidy = tidying.map(|kind| Tidy {
        rewriter: options.rewriter.as_ref(),
        kind,
        snippets: options.around_a_model.1.as_ref(),
        stop: cancelled,
    });
    // Only VinPUT Pro makes guesses that are meant to be typed. A dictation
    // that is tidied first shows what is said as well, as a preview the
    // tidied text takes the place of.
    let typing = Typing {
        live: options.live && speech.kind() == ProviderKind::Cloud,
        preview: options.live,
    };
    let typed = AtomicBool::new(false);
    let sink = Noticing {
        sink,
        typed: &typed,
    };
    let done = dictate(
        speech,
        (&options.languages, typing),
        microphone,
        (target, id),
        (&alone, entered),
        sink,
        tidy,
    )?;
    if typed.load(Ordering::Acquire) && speech.kind() == ProviderKind::Local {
        options.typed_locally.fetch_add(1, Ordering::AcqRel);
    }
    Ok(done)
}

/// Notes whether anything at all was typed through it: what a dictation
/// that counts is told apart by from one that heard nothing.
struct Noticing<'a, S> {
    sink: S,
    typed: &'a AtomicBool,
}

impl<S: TextSink> Noticing<'_, S> {
    fn note(&self, text: &str) {
        if !text.trim().is_empty() {
            self.typed.store(true, Ordering::Release);
        }
    }
}

impl<S: TextSink> TextSink for Noticing<'_, S> {
    fn type_text(&mut self, text: &str) -> Result<()> {
        self.note(text);
        self.sink.type_text(text)
    }

    fn rewrites(&self) -> bool {
        self.sink.rewrites()
    }

    fn rewrite(&mut self, erase: usize, text: &str) -> Result<()> {
        self.note(text);
        self.sink.rewrite(erase, text)
    }

    fn disturbed(&mut self) -> bool {
        self.sink.disturbed()
    }

    fn paste(&mut self, text: &str) -> Result<bool> {
        let pasted = self.sink.paste(text)?;
        if pasted {
            self.note(text);
        }
        Ok(pasted)
    }

    fn type_lines(&mut self, text: &str) -> Result<()> {
        self.note(text);
        self.sink.type_lines(text)
    }
}

/// The sink of a dictation over the real one: what is typed into `sink` is
/// left for the programs that may read it, and an Enter is said around.
fn typing_into<S: TextSink>(
    sink: S,
    options: &Options,
    target: Target,
    id: Option<String>,
) -> Telling<Announcing<S>> {
    let sink = Announcing {
        sink,
        enter: options.typing_enter.clone(),
    };
    Telling::new(sink, options.said.as_ref(), target, id)
}

/// A provider whose sessions say everything in the user's own words: what
/// comes back from the recognizer is shaped by `rules` (`words/shape.rs`)
/// before anything is typed, guessed or handed on.
struct InOwnWords<'a> {
    speech: &'a dyn SpeechProvider,
    rules: Option<Arc<Rules>>,
}

impl SpeechProvider for InOwnWords<'_> {
    fn kind(&self) -> ProviderKind {
        self.speech.kind()
    }

    fn start_session(&self, options: SessionOptions<'_>) -> Result<Session> {
        let session = self.speech.start_session(options)?;
        Ok(match &self.rules {
            Some(rules) => Box::new(Shaped::new(session, Some(Arc::clone(rules)))),
            None => session,
        })
    }
}

/// How a dictation is tidied before it is typed.
struct Tidy<'a> {
    /// The model that tidies.
    rewriter: &'a dyn Rewriter,
    /// What it is asked to make of the dictation.
    kind: Kind,
    /// The snippets, which are put in what the model writes.
    snippets: Option<&'a Arc<Rules>>,
    /// Set when the session is called off.
    stop: &'a AtomicBool,
}

/// The text that is selected in the program the user is in, and the window
/// that program has in front. Asked of Windows by the real one; made up in
/// tests.
trait Selection: Sync {
    /// The selection as the program says it when asked, which touches
    /// nothing.
    fn asked(&self) -> Option<String>;
    /// The selection, taken by copying it.
    fn copied(&self) -> Option<String>;
    /// What tells the window in front from every other.
    fn window(&self) -> usize;
}

/// The selection in the window that is in front on this PC.
struct Focused;

impl Selection for Focused {
    fn asked(&self) -> Option<String> {
        putlab_core::selection::asked()
    }

    fn copied(&self) -> Option<String> {
        putlab_core::selection::copied()
    }

    #[allow(unsafe_code)]
    fn window(&self) -> usize {
        // SAFETY: a plain call that reads which window is in the foreground.
        unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow() as usize }
    }
}

/// What the user is told when a selection was not edited.
fn unchanged(message: &str, setup: bool) -> Told {
    Told {
        title: "Nothing was changed",
        message: message.to_owned(),
        setup,
    }
}

/// Carries out what `listen` hears on the text that is selected, and puts
/// the result in its place. Returns what there is to tell the user when
/// the selection was left as it was.
///
/// The selection is asked for the moment the session starts, on a thread
/// of its own, in the way that touches nothing; only if the program does
/// not answer is it copied, once the key is let go and the instruction is
/// there. The result is put in place whole, and not as the model writes
/// it: a model that fails halfway must not leave half of the user's own
/// text behind. It is not put anywhere if the user has gone to another
/// window meanwhile.
fn command<A: AudioSource>(
    speech: &dyn SpeechProvider,
    languages: &[String],
    listen: impl FnOnce() -> Result<A>,
    (selection, rewriter): (&dyn Selection, &dyn Rewriter),
    mut sink: impl TextSink,
    cancelled: &AtomicBool,
) -> Result<Option<Told>> {
    let window = selection.window();
    let mut said = Collecting::default();
    let asked = thread::scope(|scope| {
        let asked = thread::Builder::new()
            .name("vinput-selection".into())
            .spawn_scoped(scope, || selection.asked());
        transcribe(speech, languages, listen, &mut said, false)?;
        // A program that takes its time to answer is waited for: without
        // its answer there is nothing to edit.
        Ok::<_, Error>(asked.ok().and_then(|asked| asked.join().ok()).flatten())
    })?;
    if cancelled.load(Ordering::Acquire) {
        return Ok(None);
    }
    let instruction = said.text.trim();
    if instruction.is_empty() {
        return Ok(Some(unchanged("No instruction was heard.", false)));
    }
    let Some(text) = asked.or_else(|| selection.copied()) else {
        let how = "Select the text first, then hold the key and say what to do with it.";
        return Ok(Some(unchanged(how, false)));
    };
    let job = Job::command(&text, instruction, languages);
    if job.too_long() {
        return Ok(Some(unchanged(
            "The selection is too long to edit in one go.",
            false,
        )));
    }
    let mut result = String::new();
    let written = rewriter.rewrite(&job, cancelled, &mut |piece| result.push_str(piece));
    if cancelled.load(Ordering::Acquire) {
        return Ok(None);
    }
    match written {
        Ok(()) => {}
        Err(Error::Setup(why)) => return Ok(Some(unchanged(&why, true))),
        Err(err) => return Ok(Some(unchanged(&err.to_string(), false))),
    }
    if result.trim().is_empty() {
        return Ok(Some(unchanged("The model wrote nothing.", false)));
    }
    if selection.window() != window {
        let moved = "You moved to another window before the text was ready.";
        return Ok(Some(unchanged(moved, false)));
    }
    put_over_selection(&mut sink, &result)?;
    Ok(None)
}

/// Recognizes what `listen` hears and puts the text where `target` says:
/// typed into `sink`, or returned for the host to pass on.
///
/// `alone` says, once the microphone has ended, whether the dictation ended
/// by itself because nobody spoke into it. Nobody is at the keyboard then,
/// and whatever has the focus by now is not what the dictation was meant
/// for. So nothing is done in the user's name: no Enter is pressed after
/// the text, and no search is opened. Nothing is lost either: the text is
/// typed, as that of a plain dictation is. A program that asked for the
/// text is handed it as always.
///
/// `entered` says whether the user pressed Enter to end the dictation. That
/// key press was kept from the program it was meant for, so that it does not
/// arrive before the last words do, and is made here in its place: after
/// the text of any dictation that types, said or not, and once, also where
/// the dictation would have ended with Enter anyway. A dictation that fails
/// presses nothing: the words the Enter was meant to follow are not there.
///
/// With `tidy`, a dictation that is typed is tidied first. With
/// `typing.preview`, what is said is typed while the user speaks, as it
/// would be without cleanup, and when the recognizer is done it is taken
/// back as the model's version arrives ([`Preview`]); without it nothing is
/// typed until then. What cannot be tidied is typed as it was said, and the
/// user is told why.
fn dictate<A: AudioSource>(
    speech: &dyn SpeechProvider,
    (languages, typing): (&[String], Typing),
    listen: impl FnOnce() -> Result<A>,
    (target, id): (Target, Option<String>),
    (alone, entered): (&AtomicBool, &AtomicBool),
    sink: impl TextSink,
    tidy: Option<Tidy<'_>>,
) -> Result<Option<Outcome>> {
    let mut sink = Counting { sink, typed: 0 };
    if !matches!(target, Target::Capture | Target::Search | Target::Action(_)) {
        let told = match tidy {
            None => {
                transcribe(speech, languages, listen, &mut sink, typing.live)?;
                None
            }
            Some(tidy) => {
                let shows = typing.preview && sink.rewrites();
                let mut preview = Preview::new(&mut sink, shows);
                transcribe(speech, languages, listen, &mut preview, typing.live)?;
                let Preview {
                    said, shown, moved, ..
                } = preview;
                tidy_over(&tidy, languages, &said, (shown, moved), &mut sink)?
            }
        };
        let ends_with_enter =
            target == Target::FocusedEnter && sink.typed > 0 && !alone.load(Ordering::Acquire);
        if ends_with_enter || entered.load(Ordering::Acquire) {
            sink.type_text("\n")?;
        }
        return Ok(told.map(Outcome::Told));
    }
    let mut kept = Collecting::default();
    transcribe(speech, languages, listen, &mut kept, false)?;
    let text = kept.text.trim().to_owned();
    if target == Target::Capture {
        // Sent even when empty, so whoever asked knows the session is over.
        return Ok(Some(Outcome::Captured { text, id }));
    }
    if !alone.load(Ordering::Acquire) {
        return Ok(Some(match target {
            Target::Action(slot) => handed_over(slot, text, languages, tidy)?,
            _ => Outcome::Search { text },
        }));
    }
    if !text.is_empty() {
        sink.type_text(&text)?;
    }
    Ok(None)
}

/// How a dictation is typed while the user speaks.
#[derive(Debug, Clone, Copy)]
struct Typing {
    /// Whether the recognizer's guesses are typed as well as what it
    /// settles.
    live: bool,
    /// Whether a dictation that is tidied first shows what is said while
    /// the user speaks.
    preview: bool,
}

/// What the user is told when the words on the screen could not be swapped
/// for the tidied ones.
const LEFT_AS_SAID: &str = "The caret moved while you spoke, so what you said was left as it is.";

/// Types what was `said` into `sink` tidied, in place of the `shown`
/// characters of it that are on the screen already ([`Preview`]). Where
/// the caret has `moved` from them, or they can no longer be taken back, they stand as they are, and the user
/// is told so: typing the tidied text after them would say it all twice.
fn tidy_over(
    tidy: &Tidy<'_>,
    languages: &[String],
    said: &str,
    (shown, moved): (usize, bool),
    sink: &mut impl TextSink,
) -> Result<Option<Told>> {
    if shown > 0 && (moved || !sink.rewrites() || sink.disturbed()) {
        return Ok(Some(Told {
            title: "Typed as you said it",
            message: LEFT_AS_SAID.to_owned(),
            setup: false,
        }));
    }
    let mut sink = Replacing { sink, shown };
    let text = said.trim();
    if text.is_empty() {
        sink.take_back()?;
        return Ok(None);
    }
    let job = Job::tidy(tidy.kind, text, languages);
    let told = type_tidied(
        tidy.rewriter,
        &job,
        said,
        tidy.snippets,
        &mut sink,
        tidy.stop,
    )?;
    Ok(told)
}

/// What is said while a dictation that is tidied first listens: kept for
/// the model, and with `shows`, typed at the caret meanwhile, so the user
/// sees their words as they speak.
///
/// What is typed is only ever text that one Backspace a character takes
/// back ([`erasable`]): a line break or a tab is typed as a space, and
/// what cannot be taken back is left out. A line break typed would be the
/// Enter key, which sends a message before it was tidied. The recognizer's
/// guesses are typed and taken back here as they would be without cleanup
/// (`inject/live.rs`), and so is what is kept for the model.
struct Preview<'a, S> {
    sink: &'a mut S,
    shows: bool,
    /// Everything the recognizer has said, guesses taken back included, as
    /// it would have been typed.
    said: String,
    /// How many characters of it are on the screen.
    shown: usize,
    /// Whether the caret was seen to move away from them.
    moved: bool,
}

impl<'a, S: TextSink> Preview<'a, S> {
    fn new(sink: &'a mut S, shows: bool) -> Self {
        Self {
            sink,
            shows,
            said: String::new(),
            shown: 0,
            moved: false,
        }
    }
}

/// What of `text` a preview puts on the screen.
fn previewed(text: &str) -> String {
    text.chars()
        .filter_map(|c| match c {
            c if c.is_whitespace() => Some(' '),
            c if erasable(c) => Some(c),
            _ => None,
        })
        .collect()
}

impl<S: TextSink> TextSink for Preview<'_, S> {
    fn type_text(&mut self, text: &str) -> Result<()> {
        self.rewrite(0, text)
    }

    fn rewrites(&self) -> bool {
        self.shows && self.sink.rewrites()
    }

    fn rewrite(&mut self, erase: usize, text: &str) -> Result<()> {
        // Taken back from what was said: the last `erase` characters, of
        // which those that were shown are taken back from the screen.
        let at = match erase {
            0 => self.said.len(),
            erase => self
                .said
                .char_indices()
                .rev()
                .nth(erase - 1)
                .map_or(0, |(at, _)| at),
        };
        let unsaid = previewed(&self.said[at..]).chars().count();
        self.said.truncate(at);
        self.said.push_str(text);
        if !self.shows {
            return Ok(());
        }
        // Once the caret has moved, what was shown stays where it is.
        self.moved |= self.sink.disturbed();
        let shown = previewed(text);
        self.shown = self.shown.saturating_sub(unsaid) + shown.chars().count();
        self.sink.rewrite(unsaid, &shown)
    }

    fn disturbed(&mut self) -> bool {
        self.shows && self.sink.disturbed()
    }
}

/// The real sink, while what a [`Preview`] showed is still on the screen:
/// the first thing that is put there takes its place.
struct Replacing<'a, S> {
    sink: &'a mut S,
    /// How many characters the preview left on the screen.
    shown: usize,
}

impl<S: TextSink> Replacing<'_, S> {
    /// Takes back what the preview showed, if that is still to be done.
    fn take_back(&mut self) -> Result<()> {
        let shown = std::mem::take(&mut self.shown);
        if shown > 0 {
            self.sink.rewrite(shown, "")?;
        }
        Ok(())
    }
}

impl<S: TextSink> TextSink for Replacing<'_, S> {
    fn type_text(&mut self, text: &str) -> Result<()> {
        match std::mem::take(&mut self.shown) {
            0 => self.sink.type_text(text),
            shown => self.sink.rewrite(shown, text),
        }
    }

    fn rewrites(&self) -> bool {
        self.sink.rewrites()
    }

    fn rewrite(&mut self, erase: usize, text: &str) -> Result<()> {
        let shown = std::mem::take(&mut self.shown);
        self.sink.rewrite(erase + shown, text)
    }

    fn paste(&mut self, text: &str) -> Result<bool> {
        self.take_back()?;
        self.sink.paste(text)
    }

    fn type_lines(&mut self, text: &str) -> Result<()> {
        self.take_back()?;
        self.sink.type_lines(text)
    }

    fn disturbed(&mut self) -> bool {
        self.sink.disturbed()
    }
}

/// What a dictation for the action at `slot` hands over: `text`, tidied
/// first when `tidy` says so. What cannot be tidied is handed over as it
/// was said, and the user is told why.
fn handed_over(
    slot: u8,
    text: String,
    languages: &[String],
    tidy: Option<Tidy<'_>>,
) -> Result<Outcome> {
    let Some(tidy) = tidy.filter(|_| !text.is_empty()) else {
        return Ok(Outcome::Action {
            slot,
            text,
            told: None,
        });
    };
    let job = Job::tidy(tidy.kind, &text, languages);
    let mut tidied = Collecting::default();
    let told = type_tidied(
        tidy.rewriter,
        &job,
        &text,
        tidy.snippets,
        &mut tidied,
        tidy.stop,
    )?;
    Ok(Outcome::Action {
        slot,
        text: tidied.text.trim().to_owned(),
        told,
    })
}

/// Starts the microphone. It is live from here on, whatever the recognizer
/// is still busy with.
fn listen(options: &Options, stop: StopHandle, meter: LevelMeter) -> Result<impl AudioSource> {
    Ok(Metered::new(options.microphone.open(stop)?, meter))
}

/// A microphone for a dictation, which is over when it is stopped or when
/// nobody has spoken into it for as long as the settings allow: one that
/// was started with a tap and forgotten does not listen for good. It is
/// then stopped as a key stops it, so the microphone is let go of at once,
/// whatever the recognizer still has to do; the sound of the stop follows
/// that, `alone` says how it ended, what was said before is typed, and the
/// key starts the next one. While the key is held down it is never ended.
fn while_spoken_to<'a>(
    microphone: impl AudioSource + 'a,
    options: &'a Options,
    stop: StopHandle,
    alone: &'a AtomicBool,
) -> impl AudioSource + 'a {
    let minutes = options.stop_after_minutes;
    let held = || options.held.load(Ordering::Relaxed);
    UntilQuiet::new(microphone, minutes, stop, held, move || {
        alone.store(true, Ordering::Release);
        log::write(&format!(
            "nothing was said for {minutes} min: the dictation has ended"
        ));
        if options.stop_sound {
            sound::play(Cue::Stop);
        }
    })
}

/// Recognizes what a microphone hears and hands the text to `sink`, without
/// ever making the microphone wait for the recognizer.
///
/// Asking the provider for a session is usually immediate, but not always:
/// right after Vinput starts, after another model was chosen and after an
/// engine update, the provider is still connecting, which for a local model
/// means loading it and takes seconds. The indicator shows "listening" from
/// the key press on, so those seconds must not be lost. The session is
/// therefore asked for on a thread of its own while this one opens the
/// microphone, and what is said meanwhile is kept and recognized as soon as
/// the session is there ([`Deferred`]).
///
/// The session is asked for first, so that an online provider opens its
/// stream while the microphone starts, as it always has. The price is that a
/// microphone that cannot be opened is reported only once the provider has
/// answered, should the two ever coincide.
///
/// A provider that says no at once (no key, no model) has usually said so
/// by the time the microphone is open; the session then ends with that error
/// without listening to anything.
///
/// With `live`, the recognizer's guesses are typed as well as what it
/// settles, where the sink can take text back.
fn transcribe<A: AudioSource>(
    speech: &dyn SpeechProvider,
    languages: &[String],
    listen: impl FnOnce() -> Result<A>,
    sink: &mut impl TextSink,
    live: bool,
) -> Result<()> {
    thread::scope(|scope| {
        let mut session = ask(scope, speech, languages);
        // Dropped on every way out of here, which lets go of the microphone.
        let mut microphone = listen()?;
        session.session(false)?;
        if live {
            pipeline::run_live_session(&mut microphone, &mut session, sink)
        } else {
            pipeline::run_session(&mut microphone, &mut session, sink)
        }
    })
}

/// Asks `speech` for a session on a thread of its own and returns at once
/// with the session that is still to come.
fn ask<'scope>(
    scope: &'scope Scope<'scope, '_>,
    speech: &'scope dyn SpeechProvider,
    languages: &'scope [String],
) -> Deferred {
    let (arrived, arriving) = mpsc::channel();
    let fallback = arrived.clone();
    let asked = thread::Builder::new()
        .name("vinput-recognizer".into())
        .spawn_scoped(scope, move || {
            let _ = arrived.send(speech.start_session(SessionOptions { languages }));
        });
    if asked.is_err() {
        // Without a thread to wait on, wait here, as sessions used to.
        let _ = fallback.send(speech.start_session(SessionOptions { languages }));
    }
    Deferred::awaiting(arriving, MAX_WAITING_SAMPLES)
}

/// Records until stopped and saves the recording as an MP3.
///
/// A memo is for someone to listen to, not for a recognizer, so the
/// microphone is opened for it at its own rate rather than the one of
/// speech ([`ArmedMicrophone::record`]), and the recording goes to disk as
/// it comes rather than into memory ([`memo::Recording`]).
fn record_memo(
    options: &Options,
    stop: StopHandle,
    meter: LevelMeter,
    id: Option<String>,
    cancelled: &AtomicBool,
) -> Result<Option<Outcome>> {
    let microphone = options.microphone.record(stop)?;
    let mut recording = memo::Recording::new(microphone.rate())?;
    let mut microphone = Metered::new(microphone, meter);
    // A tenth of a second at 48 kHz.
    let mut chunk = [0i16; 4800];
    while !recording.is_full() {
        let read = microphone.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        recording.push(&chunk[..read])?;
    }
    // The microphone is released before the encoding starts.
    drop(microphone);
    // A memo that was called off is not saved, not even for a moment.
    if recording.is_too_short() || cancelled.load(Ordering::Acquire) {
        return Ok(None);
    }
    let saved = recording.save(&options.memo_dir)?;
    let on_clipboard = options.memo_to_clipboard
        && memo::copy_to_clipboard(&saved)
            .map_err(|err| log::write(&err.to_string()))
            .is_ok();
    Ok(Some(Outcome::Memo {
        path: saved.path,
        seconds: saved.seconds,
        on_clipboard,
        id,
    }))
}

#[cfg(test)]
mod tests {
    use std::sync::Condvar;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;
    use crate::rewrite::testing::{Script, Scripted};
    use crate::stt::deferred::Session;
    use crate::stt::{Emit, ProviderKind, Transcriber, Transcript};

    /// Samples the made-up microphone hears per read: 10 ms, as a real one.
    const PACKET: usize = 160;

    /// A door that stays shut until the test opens it.
    #[derive(Default)]
    struct Gate {
        open: Mutex<bool>,
        opened: Condvar,
    }

    impl Gate {
        fn open(&self) {
            *self.open.lock().unwrap_or_else(PoisonError::into_inner) = true;
            self.opened.notify_all();
        }

        fn pass(&self) {
            let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
            while !*open {
                open = self
                    .opened
                    .wait(open)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        }
    }

    /// What the recognizer was fed and what it said, as the test sees it.
    #[derive(Default)]
    struct Heard {
        fed: Mutex<Vec<i16>>,
        said: Mutex<Vec<String>>,
    }

    impl Heard {
        fn fed(&self) -> Vec<i16> {
            self.fed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }

        fn said(&self) -> Vec<String> {
            self.said
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }
    }

    /// A provider that is still loading its model until the gate opens.
    struct Loading {
        gate: Arc<Gate>,
        heard: Arc<Heard>,
        /// Whether loading ends in an error.
        fails: bool,
        /// Whether its sessions say something for every piece of audio.
        talkative: bool,
    }

    impl Loading {
        fn new(fails: bool, talkative: bool) -> Self {
            Self {
                gate: Arc::default(),
                heard: Arc::default(),
                fails,
                talkative,
            }
        }
    }

    impl SpeechProvider for Loading {
        fn kind(&self) -> ProviderKind {
            ProviderKind::Local
        }

        fn start_session(&self, _: SessionOptions<'_>) -> Result<Session> {
            self.gate.pass();
            if self.fails {
                return Err(Error::Transcription("the model did not load".into()));
            }
            Ok(Box::new(Listener {
                heard: Arc::clone(&self.heard),
                talkative: self.talkative,
            }))
        }
    }

    struct Listener {
        heard: Arc<Heard>,
        talkative: bool,
    }

    impl Listener {
        fn say(&self, text: String) -> Transcript {
            self.heard
                .said
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(text.clone());
            Transcript {
                text,
                is_final: true,
            }
        }
    }

    impl Transcriber for Listener {
        fn feed(&mut self, pcm: &[i16]) -> Result<Option<Transcript>> {
            self.heard
                .fed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend_from_slice(pcm);
            Ok(self
                .talkative
                .then(|| self.say(format!("{} ", pcm.first().copied().unwrap_or(-1)))))
        }

        fn finish(&mut self, emit: &mut Emit<'_>) -> Result<()> {
            emit(self.say("end".into()))
        }
    }

    /// A microphone that hears the numbers from zero up, one packet a read,
    /// for a set number of reads, and then has been stopped.
    struct Counted {
        next: i16,
        read: usize,
        reads: usize,
        /// Called before each read with its number.
        before: Box<dyn FnMut(usize)>,
        released: Arc<AtomicBool>,
    }

    impl Counted {
        fn new(reads: usize, before: impl FnMut(usize) + 'static) -> (Self, Arc<AtomicBool>) {
            let released = Arc::new(AtomicBool::new(false));
            let microphone = Self {
                next: 0,
                read: 0,
                reads,
                before: Box::new(before),
                released: Arc::clone(&released),
            };
            (microphone, released)
        }
    }

    impl AudioSource for Counted {
        fn read(&mut self, buf: &mut [i16]) -> Result<usize> {
            if self.read == self.reads {
                return Ok(0);
            }
            (self.before)(self.read);
            self.read += 1;
            let count = buf.len().min(PACKET);
            for slot in &mut buf[..count] {
                *slot = self.next;
                self.next += 1;
            }
            Ok(count)
        }
    }

    impl Drop for Counted {
        fn drop(&mut self) {
            self.released.store(true, Ordering::SeqCst);
        }
    }

    #[derive(Default)]
    struct Typed(Vec<String>);

    impl TextSink for Typed {
        fn type_text(&mut self, text: &str) -> Result<()> {
            self.0.push(text.to_owned());
            Ok(())
        }
    }

    /// Every number from zero up that `reads` reads hear.
    fn everything(reads: usize) -> Vec<i16> {
        (0..).take(reads * PACKET).collect()
    }

    #[test]
    fn what_is_said_while_the_model_loads_is_recognized_in_full_and_in_order() {
        let provider = Loading::new(false, false);
        // The model has loaded once two tenths of a second have been heard.
        let fed_before_loaded = Arc::new(AtomicUsize::new(usize::MAX));
        let (microphone, released) = {
            let gate = Arc::clone(&provider.gate);
            let heard = Arc::clone(&provider.heard);
            let fed_before_loaded = Arc::clone(&fed_before_loaded);
            Counted::new(50, move |read| {
                if read == 20 {
                    fed_before_loaded.store(heard.fed().len(), Ordering::SeqCst);
                    gate.open();
                }
            })
        };
        let mut sink = Typed::default();

        let result = transcribe(&provider, &[], || Ok(microphone), &mut sink, false);

        assert!(result.is_ok(), "{result:?}");
        // The microphone was heard before there was anyone to recognize it,
        assert_eq!(fed_before_loaded.load(Ordering::SeqCst), 0);
        // and nothing of it was lost or moved.
        assert_eq!(provider.heard.fed(), everything(50));
        assert_eq!(sink.0, ["end"]);
        assert!(released.load(Ordering::SeqCst));
    }

    #[test]
    fn a_session_stopped_before_the_model_has_loaded_is_still_recognized() {
        let provider = Loading::new(false, false);
        let (microphone, _) = Counted::new(10, |_| {});
        let mut sink = Typed::default();
        let gate = Arc::clone(&provider.gate);
        let loaded = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            gate.open();
        });

        let result = transcribe(&provider, &[], || Ok(microphone), &mut sink, false);

        assert!(result.is_ok(), "{result:?}");
        assert!(loaded.join().is_ok());
        assert_eq!(provider.heard.fed(), everything(10));
        assert_eq!(sink.0, ["end"]);
    }

    #[test]
    fn a_model_that_does_not_load_ends_the_session_and_lets_go_of_the_microphone() {
        let provider = Loading::new(true, false);
        let (microphone, released) = {
            let gate = Arc::clone(&provider.gate);
            Counted::new(50, move |read| {
                if read == 5 {
                    gate.open();
                }
            })
        };
        let mut sink = Typed::default();

        let result = transcribe(&provider, &[], || Ok(microphone), &mut sink, false);

        assert!(matches!(result, Err(Error::Transcription(_))), "{result:?}");
        assert!(released.load(Ordering::SeqCst));
        assert!(sink.0.is_empty());
    }

    #[test]
    fn a_model_that_fails_after_the_session_was_stopped_is_reported_too() {
        let provider = Loading::new(true, false);
        let (microphone, released) = Counted::new(3, |_| {});
        let gate = Arc::clone(&provider.gate);
        let failed = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            gate.open();
        });

        let result = transcribe(
            &provider,
            &[],
            || Ok(microphone),
            &mut Typed::default(),
            false,
        );

        assert!(matches!(result, Err(Error::Transcription(_))), "{result:?}");
        assert!(failed.join().is_ok());
        assert!(released.load(Ordering::SeqCst));
    }

    #[test]
    fn everything_said_about_the_kept_audio_is_typed_in_order() {
        let provider = Loading::new(false, true);
        let (microphone, _) = {
            let gate = Arc::clone(&provider.gate);
            Counted::new(50, move |read| {
                if read == 30 {
                    gate.open();
                }
            })
        };
        let mut sink = Typed::default();

        let result = transcribe(&provider, &[], || Ok(microphone), &mut sink, false);

        assert!(result.is_ok(), "{result:?}");
        assert_eq!(provider.heard.fed(), everything(50));
        // Handing over what was kept made the recognizer speak several times
        // at once; none of it is dropped and the order holds.
        let said = provider.heard.said();
        assert!(said.len() > 2, "{said:?}");
        assert_eq!(sink.0, said);
    }

    #[test]
    fn a_microphone_that_cannot_be_opened_is_reported() {
        let provider = Loading::new(false, false);
        provider.gate.open();
        let result = transcribe(
            &provider,
            &[],
            || Err::<Counted, _>(Error::Audio("no microphone".into())),
            &mut Typed::default(),
            false,
        );
        assert!(matches!(result, Err(Error::Audio(_))), "{result:?}");
    }

    #[test]
    fn a_provider_that_says_no_at_once_ends_the_session_before_it_listens() {
        // No key, no model: the answer is there before the microphone is.
        let provider = Loading::new(true, false);
        provider.gate.open();
        let (microphone, released) = Counted::new(50, |_| {});

        let result = transcribe(
            &provider,
            &[],
            || {
                // Opening a microphone takes longer than saying no.
                thread::sleep(Duration::from_millis(100));
                Ok(microphone)
            },
            &mut Typed::default(),
            false,
        );

        assert!(matches!(result, Err(Error::Transcription(_))), "{result:?}");
        assert!(released.load(Ordering::SeqCst));
    }

    #[test]
    fn kept_audio_is_handed_over_before_what_follows_and_is_bounded() -> Result<()> {
        let heard = Arc::new(Heard::default());
        let (arrived, arriving) = mpsc::channel();
        let mut session = Deferred::awaiting(arriving, 10);

        assert_eq!(session.feed(&[0, 1, 2, 3])?, None);
        assert_eq!(session.feed(&[4, 5, 6, 7])?, None);
        // Room for two more; the rest of what is said is lost, and said so.
        assert_eq!(session.feed(&[8, 9, 10, 11])?, None);
        assert_eq!(session.feed(&[12, 13])?, None);
        assert!(heard.fed().is_empty());

        let listener: Session = Box::new(Listener {
            heard: Arc::clone(&heard),
            talkative: false,
        });
        assert!(arrived.send(Ok(listener)).is_ok());
        assert_eq!(session.feed(&[20, 21])?, None);
        assert_eq!(heard.fed(), [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 20, 21]);

        let mut last = Vec::new();
        session.finish(&mut |transcript| {
            last.push(transcript.text);
            Ok(())
        })?;
        assert_eq!(last, ["end"]);
        Ok(())
    }

    #[test]
    fn a_provider_whose_answer_can_no_longer_come_is_an_error() {
        let (arrived, arriving) = mpsc::channel::<Result<Session>>();
        let mut session = Deferred::awaiting(arriving, 10);
        drop(arrived);
        assert!(matches!(session.feed(&[1]), Err(Error::Transcription(_))));
        assert!(session.finish(&mut |_| Ok(())).is_err());
    }

    /// What whoever is told that a session has ended finds at that moment.
    #[derive(Debug, Clone, PartialEq)]
    struct Found {
        outcomes: Vec<Outcome>,
        failures: Vec<Failure>,
        ended: bool,
    }

    /// A session about to end, and what was found each time someone was told.
    fn closing() -> (Closing, Arc<Mutex<Vec<Found>>>, Arc<AtomicBool>) {
        let outcomes = Arc::new(Mutex::new(Vec::new()));
        let failures = Arc::new(Mutex::new(Vec::new()));
        let ended = Arc::new(AtomicBool::new(false));
        let told = Arc::new(Mutex::new(Vec::new()));
        let notify: Ended = {
            let (outcomes, failures) = (Arc::clone(&outcomes), Arc::clone(&failures));
            let (ended, told) = (Arc::clone(&ended), Arc::clone(&told));
            Arc::new(move || {
                let found = Found {
                    outcomes: outcomes
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clone(),
                    failures: failures
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clone(),
                    ended: ended.load(Ordering::Acquire),
                };
                told.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(found);
            })
        };
        let closing = Closing {
            outcomes,
            failures,
            id: Some("my-42".to_owned()),
            provider: ProviderKind::Local,
            ended: Arc::clone(&ended),
            notify: Some(notify),
        };
        (closing, told, ended)
    }

    fn told(told: &Arc<Mutex<Vec<Found>>>) -> Vec<Found> {
        told.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    #[test]
    fn whoever_is_told_of_the_end_finds_what_the_session_left_behind() {
        let (closing, heard, _) = closing();
        let outcome = Outcome::Captured {
            text: "hello".to_owned(),
            id: Some("my-42".to_owned()),
        };
        assert!(told(&heard).is_empty());
        closing.with(Ok(Some(outcome.clone())));
        // Told once, with the text in place and the session no longer counted.
        assert_eq!(
            told(&heard),
            [Found {
                outcomes: vec![outcome],
                failures: Vec::new(),
                ended: true,
            }]
        );
    }

    #[test]
    fn a_session_that_fails_tells_of_its_end_with_the_error_in_place() {
        let (closing, heard, _) = closing();
        closing.with(Err(Error::Audio("no microphone".to_owned())));
        let found = told(&heard);
        assert_eq!(found.len(), 1);
        assert!(found[0].ended && found[0].outcomes.is_empty());
        // The failure says which session it is about.
        let [failure] = found[0].failures.as_slice() else {
            return assert_eq!(found[0].failures.len(), 1);
        };
        assert!(failure.message.contains("no microphone"), "{failure:?}");
        assert_eq!(failure.id.as_deref(), Some("my-42"));
        // Another press may find the microphone back.
        assert!(!failure.setup);
    }

    #[test]
    fn a_session_that_fails_for_want_of_setting_up_says_that_pressing_again_is_no_use() {
        let (closing, heard, _) = closing();
        closing.with(Err(Error::Setup("no model is installed".to_owned())));
        let found = told(&heard);
        let [failure] = found[0].failures.as_slice() else {
            return assert_eq!(found[0].failures.len(), 1);
        };
        assert!(failure.setup, "{failure:?}");
        // In the same words as any failure to recognize.
        assert_eq!(
            failure.message,
            "transcription failed: no model is installed"
        );
    }

    #[test]
    fn two_sessions_that_fail_each_leave_their_own_error_behind() {
        // One started with an id and one without, failing in the other
        // order before anyone has looked.
        let failures = Arc::new(Mutex::new(Vec::new()));
        let session = |id: Option<&str>| Closing {
            outcomes: Arc::default(),
            failures: Arc::clone(&failures),
            id: id.map(str::to_owned),
            provider: ProviderKind::Cloud,
            ended: Arc::default(),
            notify: None,
        };
        let (first, second) = (session(Some("a")), session(None));
        second.with(Err(Error::Audio("second".to_owned())));
        first.with(Err(Error::Audio("first".to_owned())));
        let left: Vec<(bool, Option<String>)> = failures
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|failure| (failure.message.contains("second"), failure.id.clone()))
            .collect();
        assert_eq!(left, [(true, None), (false, Some("a".to_owned()))]);
    }

    #[test]
    fn a_session_that_leaves_nothing_behind_tells_of_its_end_all_the_same() {
        // A typed dictation, or a memo too short to keep.
        let (closing, heard, _) = closing();
        closing.with(Ok(None));
        assert_eq!(
            told(&heard),
            [Found {
                outcomes: Vec::new(),
                failures: Vec::new(),
                ended: true,
            }]
        );
    }

    #[test]
    fn a_session_that_is_called_off_types_nothing_more() -> Result<()> {
        let cancelled = AtomicBool::new(false);
        let mut sink = Counting {
            sink: UnlessCancelled {
                sink: Collecting::default(),
                cancelled: &cancelled,
            },
            typed: 0,
        };
        sink.type_text("One.")?;
        cancelled.store(true, Ordering::Release);
        // What the recognizer still comes up with, and the Enter after it.
        sink.type_text(" Two.")?;
        sink.type_text(
            "
",
        )?;
        assert_eq!(sink.sink.sink.text, "One.");

        // It leaves nothing behind either: no search, no memo. Only the
        // program that asked for the text hears that there is none.
        for target in [
            Target::Focused,
            Target::FocusedEnter,
            Target::Search,
            Target::Memo,
        ] {
            assert_eq!(called_off_outcome(target, None), None);
        }
        assert_eq!(
            called_off_outcome(Target::Capture, Some("my-42".to_owned())),
            Some(Outcome::Captured {
                text: String::new(),
                id: Some("my-42".to_owned()),
            })
        );
        Ok(())
    }

    #[test]
    fn a_dictation_counts_once_something_is_typed_through_it() -> Result<()> {
        let typed = AtomicBool::new(false);
        let mut sink = Noticing {
            sink: Collecting::default(),
            typed: &typed,
        };
        sink.type_text("  ")?;
        sink.type_lines("\n")?;
        assert!(!typed.load(Ordering::Acquire));
        sink.type_text("Hello")?;
        assert!(typed.load(Ordering::Acquire));
        assert_eq!(sink.sink.text, "  \nHello");
        Ok(())
    }

    #[test]
    fn the_count_of_local_dictations_is_taken_once_and_kept_across_settings() {
        let settings = Settings::default();
        let microphone = Arc::new(ArmedMicrophone::new(&settings.microphone));
        let mut dictation = Dictation {
            speech: Arc::new(Loading::new(false, false)),
            speech_config: settings.speech_config(),
            rewrite_config: rewrite_config(&settings),
            options: Options::of(&settings, microphone, Arc::default(), quiet_model()),
            meter: LevelMeter::new(),
            failures: Arc::default(),
            outcomes: Arc::default(),
            said: Arc::default(),
            active: None,
            finishing: Vec::new(),
            notify: None,
        };
        assert_eq!(dictation.take_local_dictations(), 0);
        dictation
            .options
            .typed_locally
            .fetch_add(3, Ordering::AcqRel);
        // A session still running counts into the same place after the
        // settings change.
        let running = Arc::clone(&dictation.options.typed_locally);
        dictation.configure(&settings, false);
        running.fetch_add(1, Ordering::AcqRel);
        assert_eq!(dictation.take_local_dictations(), 4);
        assert_eq!(dictation.take_local_dictations(), 0);
    }

    #[test]
    fn calling_off_reaches_the_session_that_listens_and_those_still_at_work() {
        let settings = Settings::default();
        let microphone = Arc::new(ArmedMicrophone::new(&settings.microphone));
        let mut dictation = Dictation {
            speech: Arc::new(Loading::new(false, false)),
            speech_config: settings.speech_config(),
            rewrite_config: rewrite_config(&settings),
            options: Options::of(&settings, microphone, Arc::default(), quiet_model()),
            meter: LevelMeter::new(),
            failures: Arc::default(),
            outcomes: Arc::default(),
            said: Arc::default(),
            active: None,
            finishing: Vec::new(),
            notify: None,
        };
        assert!(!dictation.can_cancel());
        // One session was stopped and is at its last words, one listens.
        let flags = || {
            (
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicBool::new(false)),
            )
        };
        let (stopped, stopped_ended) = flags();
        let (listening, listening_ended) = flags();
        dictation
            .finishing
            .push((Arc::clone(&stopped), Arc::clone(&stopped_ended)));
        assert!(dictation.can_cancel());
        dictation.active = Some(Active {
            stop: StopHandle::new(),
            cancelled: Arc::clone(&listening),
            entered: Arc::default(),
            ended: Arc::clone(&listening_ended),
        });

        dictation.cancel();
        assert!(stopped.load(Ordering::Acquire) && listening.load(Ordering::Acquire));
        // Both are still on their way out, with nothing left to call off.
        assert!(!dictation.is_active() && dictation.is_finishing());
        assert!(!dictation.can_cancel());
        stopped_ended.store(true, Ordering::Release);
        listening_ended.store(true, Ordering::Release);
        assert!(!dictation.is_finishing());

        // A session that is only stopped can still be called off.
        let (later, later_ended) = flags();
        dictation.active = Some(Active {
            stop: StopHandle::new(),
            cancelled: Arc::clone(&later),
            entered: Arc::default(),
            ended: later_ended,
        });
        dictation.stop();
        assert!(dictation.can_cancel() && !later.load(Ordering::Acquire));
        dictation.cancel();
        assert!(!dictation.can_cancel() && later.load(Ordering::Acquire));
    }

    /// A dictation that streams to a server while it listens is called off
    /// like any other: the server is told the audio is over, what it still
    /// settles is not typed, its stream is closed, and the connection that
    /// is kept for the next press is there for the next press.
    #[cfg(windows)]
    #[test]
    fn a_dictation_with_vinput_pro_that_is_called_off_types_nothing_and_ends_its_stream()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use crate::cloud::fake::Fake;
        use crate::stt::CloudSpeech;

        let fake = Fake::start()?;
        let (account, memory) = fake.account();
        memory.sign_in(&fake);
        let speech = CloudSpeech::connect(account, Arc::default())?;
        // The token is fetched when the provider starts; what is connected
        // after that is for the audio.
        assert!(fake.wait_for(|state| !state.asked.is_empty()));
        let before = fake.read(|state| state.connections);

        // Called off before the microphone ends, as `Dictation::cancel` does.
        let cancelled = Arc::new(AtomicBool::new(false));
        let called_off = Arc::clone(&cancelled);
        let (microphone, _) = Counted::new(5, move |read| {
            if read == 4 {
                called_off.store(true, Ordering::Release);
            }
        });
        let mut sink = UnlessCancelled {
            sink: Collecting::default(),
            cancelled: &cancelled,
        };
        let started = Instant::now();
        transcribe(&speech, &[], || Ok(microphone), &mut sink, false)?;
        assert!(started.elapsed() < Duration::from_secs(5));
        // The stand-in settled "Hello world." when the audio ended.
        assert_eq!(sink.sink.text, "");
        assert!(fake.wait(|seen| seen.len() == 1 && seen[0].events.contains(&"closed")));
        let events = fake.read(|state| state.streams[0].events.clone());
        assert!(events.ends_with(&["finish", "closed"]), "{events:?}");

        // Nothing of it is in the way of the next dictation, which finds
        // the connection made when this one ended: one more than the
        // stream's own, on which no second stream was asked for until now.
        assert!(fake.wait_for(|state| state.connections == before + 2));
        assert_eq!(fake.asked("GET /v1/stream"), 1);
        let (microphone, _) = Counted::new(3, |_| {});
        let mut sink = Collecting::default();
        transcribe(&speech, &[], || Ok(microphone), &mut sink, false)?;
        assert_eq!(sink.text, "Hello world.");
        assert!(fake.wait(|seen| seen.len() == 2 && seen[1].events.contains(&"closed")));
        // That one took the connection over and left another in its turn;
        // none was made in vain.
        assert!(fake.wait_for(|state| state.connections == before + 3));
        assert_eq!(fake.read(|state| state.left_unused), 0);
        Ok(())
    }

    /// A text field that can be typed into and taken back from, and every
    /// text that ever stood in it.
    #[derive(Default)]
    struct Field {
        text: String,
        stood: Vec<String>,
    }

    impl TextSink for Field {
        fn type_text(&mut self, text: &str) -> Result<()> {
            self.rewrite(0, text)
        }

        fn rewrites(&self) -> bool {
            true
        }

        fn rewrite(&mut self, erase: usize, text: &str) -> Result<()> {
            for _ in 0..erase {
                self.text.pop();
            }
            self.text.push_str(text);
            self.stood.push(self.text.clone());
            Ok(())
        }
    }

    /// A [`Field`] that is typed into by a dictation and looked at after.
    struct Lent<'a>(&'a mut Field);

    impl TextSink for Lent<'_> {
        fn type_text(&mut self, text: &str) -> Result<()> {
            self.0.type_text(text)
        }

        fn rewrites(&self) -> bool {
            self.0.rewrites()
        }

        fn rewrite(&mut self, erase: usize, text: &str) -> Result<()> {
            self.0.rewrite(erase, text)
        }
    }

    /// With `VinPUT` Pro the guess of the server is on the screen while the
    /// user still speaks, and what it settles when the key is let go takes
    /// its place; the Enter of a dictation that ends with one follows that.
    /// A program that asked for the text gets the settled text alone.
    #[cfg(windows)]
    #[test]
    fn a_dictation_with_vinput_pro_types_the_guess_and_then_what_is_settled()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use crate::cloud::fake::Fake;
        use crate::stt::CloudSpeech;

        let fake = Fake::start()?;
        let (account, memory) = fake.account();
        memory.sign_in(&fake);
        let speech = CloudSpeech::connect(account, Arc::default())?;
        // A microphone that is spoken into for a third of a second.
        let spoken = || Counted::new(30, |_| thread::sleep(Duration::from_millis(10))).0;

        let mut field = Field::default();
        transcribe(&speech, &[], || Ok(spoken()), &mut field, true)?;
        // The stand-in guesses "hello wor" and settles "Hello world.".
        assert_eq!(field.text, "Hello world.");
        assert!(
            field.stood.iter().any(|text| text == "h"),
            "{:?}",
            field.stood
        );
        assert!(field.stood.iter().any(|text| text == "hello wor"));

        // Without the setting, the same dictation types what is settled.
        let mut field = Field::default();
        transcribe(&speech, &[], || Ok(spoken()), &mut field, false)?;
        assert_eq!(field.stood, ["Hello world."]);

        let alone = AtomicBool::new(false);
        let mut field = Field::default();
        let left = dictate(
            &speech,
            (&[], LIVE),
            || Ok(spoken()),
            (Target::FocusedEnter, None),
            (&alone, &AtomicBool::new(false)),
            Lent(&mut field),
            None,
        )?;
        assert_eq!((left, field.text.as_str()), (None, "Hello world.\n"));
        let captured = dictate(
            &speech,
            (&[], LIVE),
            || Ok(spoken()),
            (Target::Capture, None),
            (&alone, &AtomicBool::new(false)),
            Field::default(),
            None,
        )?;
        let text = "Hello world.".to_owned();
        assert_eq!(captured, Some(Outcome::Captured { text, id: None }));
        Ok(())
    }

    /// A connection to `VinPUT` Pro that drops in the middle of a dictation
    /// costs no word: what the server settled stays, its guess at what
    /// followed is not typed a second time, and the model on this PC
    /// recognizes everything the settled text did not reach.
    #[cfg(windows)]
    #[test]
    fn a_dictation_whose_connection_drops_is_finished_by_the_model_on_this_pc()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use crate::cloud::fake::{Fake, Script};
        use crate::stt::CloudSpeech;
        use crate::stt::standby::tests::{out_of_reach, with_model};

        let fake = Fake::start()?;
        // The server settles "So far. ", says that this reaches five
        // milliseconds of the audio, guesses on, and is gone.
        fake.change(|state| {
            state.scripts = vec![Script::SettlesThenDrops {
                reached: Some(5),
                guessing: true,
            }];
        });
        let (account, memory) = fake.account();
        memory.sign_in(&fake);
        let (standby, heard) = with_model();
        let speech = CloudSpeech::connect(account, Arc::clone(&standby))?;
        // A microphone that is spoken into for a third of a second.
        let spoken = Counted::new(30, |_| thread::sleep(Duration::from_millis(10))).0;

        let mut field = Field::default();
        transcribe(&speech, &[], || Ok(spoken), &mut field, true)?;
        // The made-up model settles "Locally." when the key is let go.
        assert_eq!(field.text, "So far. Locally.");
        // It was handed all of it but those five milliseconds.
        assert_eq!(heard.fed().len(), 30 * PACKET - 80);
        assert_eq!(standby.take_served(), Some(out_of_reach(true)));
        Ok(())
    }

    /// The dictionary, the snippets and spoken formatting work on every
    /// dictation: on the guesses that are typed while the user speaks, on
    /// what is settled, and on the text a program asked for.
    #[cfg(windows)]
    #[test]
    fn a_dictation_comes_out_in_the_users_own_words()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use crate::cloud::fake::Fake;
        use crate::stt::CloudSpeech;
        use crate::words::Words;

        let fake = Fake::start()?;
        let (account, memory) = fake.account();
        memory.sign_in(&fake);
        let speech = CloudSpeech::connect(account, Arc::default())?;
        let mut words = Words::default();
        assert_eq!(words.add_word("WORLD", ""), Ok(true));
        assert_eq!(words.add_snippet("hello", "Good day"), Ok(true));
        let speech = InOwnWords {
            speech: &speech,
            rules: Rules::of(&words),
        };
        let spoken = || Counted::new(30, |_| thread::sleep(Duration::from_millis(10))).0;

        // The stand-in guesses "hello wor" and settles "Hello world.".
        let mut field = Field::default();
        transcribe(&speech, &[], || Ok(spoken()), &mut field, true)?;
        assert_eq!(field.text, "Good day WORLD.");
        assert!(
            field.stood.iter().any(|text| text == "Good day wor"),
            "{:?}",
            field.stood
        );

        let mut field = Field::default();
        transcribe(&speech, &[], || Ok(spoken()), &mut field, false)?;
        assert_eq!(field.text, "Good day WORLD.");

        let captured = dictate(
            &speech,
            (&[], SETTLED),
            || Ok(spoken()),
            (Target::Capture, None),
            (&AtomicBool::new(false), &AtomicBool::new(false)),
            Field::default(),
            None,
        )?;
        let text = "Good day WORLD.".to_owned();
        assert_eq!(captured, Some(Outcome::Captured { text, id: None }));

        // Without any of them a session is the provider's own.
        let plain = InOwnWords {
            speech: speech.speech,
            rules: None,
        };
        let mut field = Field::default();
        transcribe(&plain, &[], || Ok(spoken()), &mut field, false)?;
        assert_eq!(field.text, "Hello world.");
        Ok(())
    }

    /// A dictation that is called off while it is handed from `VinPUT` Pro
    /// to the model on this PC, because the hours ran out, types nothing:
    /// not what the server settled, not what the model makes of the rest.
    #[cfg(windows)]
    #[test]
    fn a_dictation_called_off_while_the_model_on_this_pc_takes_over_types_nothing()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use crate::cloud::fake::{Fake, Script};
        use crate::stt::CloudSpeech;
        use crate::stt::standby::tests::with_model;

        for script in [
            Script::Refuse(429, "quota_exceeded"),
            Script::SpentMidway(0),
        ] {
            let fake = Fake::start()?;
            fake.change(|state| state.scripts = vec![script]);
            let (account, memory) = fake.account();
            memory.sign_in(&fake);
            let (standby, heard) = with_model();
            let speech = CloudSpeech::connect(account, Arc::clone(&standby))?;
            let cancelled = Arc::new(AtomicBool::new(false));
            let called_off = Arc::clone(&cancelled);
            let (microphone, _) = Counted::new(5, move |read| {
                if read == 4 {
                    called_off.store(true, Ordering::Release);
                }
            });
            let mut sink = UnlessCancelled {
                sink: Collecting::default(),
                cancelled: &cancelled,
            };
            transcribe(&speech, &[], || Ok(microphone), &mut sink, false)?;
            assert_eq!(sink.sink.text, "", "{script:?}");
            // The model did take over, and what it said fell.
            assert_eq!(heard.sessions(), 1, "{script:?}");
            // Not called off, the same dictation is typed, by the model.
            let (microphone, _) = Counted::new(3, |_| {});
            let mut sink = Collecting::default();
            transcribe(&speech, &[], || Ok(microphone), &mut sink, false)?;
            assert_eq!(sink.text, "Locally.", "{script:?}");
        }
        Ok(())
    }

    /// A microphone in a still room, heard until it is stopped.
    struct Still(StopHandle);

    impl AudioSource for Still {
        fn read(&mut self, buf: &mut [i16]) -> Result<usize> {
            if self.0.is_stopped() {
                return Ok(0);
            }
            buf.fill(0);
            Ok(buf.len())
        }
    }

    /// Text typed on the thread of a session, as the test sees it.
    #[derive(Default, Clone)]
    struct Seen(Arc<Mutex<Vec<String>>>);

    impl TextSink for Seen {
        fn type_text(&mut self, text: &str) -> Result<()> {
            let mut seen = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            seen.push(text.to_owned());
            Ok(())
        }
    }

    /// Options that allow a minute without a sound.
    fn a_minute() -> Options {
        let settings = Settings {
            stop_after_minutes: 1,
            ..Settings::default()
        };
        let microphone = Arc::new(ArmedMicrophone::new(&settings.microphone));
        Options::of(&settings, microphone, Arc::default(), quiet_model())
    }

    /// A model that writes nothing, for sessions that ask it nothing.
    fn quiet_model() -> Arc<dyn Rewriter> {
        Arc::new(Scripted::writing(&[]))
    }

    /// What a dictation to `target`, of which the recognizer makes "end",
    /// types and leaves for the host: one that is `forgotten` until it ends
    /// by itself, or one that is stopped with its key.
    fn dictated(target: Target, forgotten: bool) -> (Vec<String>, Option<Outcome>) {
        let provider = Loading::new(false, false);
        provider.gate.open();
        let options = a_minute();
        let stop = StopHandle::new();
        if !forgotten {
            stop.stop();
        }
        let alone = AtomicBool::new(false);
        let listen = || Ok(while_spoken_to(Still(stop.clone()), &options, stop, &alone));
        let seen = Seen::default();
        let id = Some("my-42".to_owned());
        let left = dictate(
            &provider,
            (&[], SETTLED),
            listen,
            (target, id),
            (&alone, &AtomicBool::new(false)),
            seen.clone(),
            None,
        );
        assert!(left.is_ok(), "{left:?}");
        assert_eq!(alone.load(Ordering::Acquire), forgotten);
        let typed = seen
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        (typed, left.ok().flatten())
    }

    #[test]
    fn a_dictation_that_ended_by_itself_presses_no_enter_and_opens_no_search() {
        let end = || vec!["end".to_owned()];
        let captured = Some(Outcome::Captured {
            text: "end".to_owned(),
            id: Some("my-42".to_owned()),
        });
        // Stopped with its key, each does what its key is for.
        assert_eq!(dictated(Target::Focused, false), (end(), None));
        assert_eq!(
            dictated(Target::FocusedEnter, false),
            (vec!["end".to_owned(), "\n".to_owned()], None)
        );
        let search = Outcome::Search {
            text: "end".to_owned(),
        };
        assert_eq!(dictated(Target::Search, false), (vec![], Some(search)));
        assert_eq!(dictated(Target::Capture, false), (vec![], captured.clone()));

        // Forgotten, with nobody there: the text is typed and that is all.
        assert_eq!(dictated(Target::Focused, true), (end(), None));
        assert_eq!(dictated(Target::FocusedEnter, true), (end(), None));
        assert_eq!(dictated(Target::Search, true), (end(), None));
        // The program that asked for the text still gets it.
        assert_eq!(dictated(Target::Capture, true), (vec![], captured));
    }

    /// What a dictation to `target` types and leaves for the host when the
    /// user ends it with Enter; the recognizer makes "end" of it, or fails.
    fn ended_with_enter(target: Target, fails: bool) -> (Vec<String>, Result<Option<Outcome>>) {
        let provider = Loading::new(fails, false);
        provider.gate.open();
        // The host has told the session of the Enter and stopped it.
        let (alone, entered) = (AtomicBool::new(false), AtomicBool::new(true));
        let stop = StopHandle::new();
        stop.stop();
        let seen = Seen::default();
        let left = dictate(
            &provider,
            (&[], SETTLED),
            || Ok(Still(stop)),
            (target, None),
            (&alone, &entered),
            seen.clone(),
            None,
        );
        let typed = seen
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        (typed, left)
    }

    #[test]
    fn enter_that_ends_a_dictation_is_pressed_once_after_its_last_words() {
        let then_enter = vec!["end".to_owned(), "\n".to_owned()];
        // A plain dictation: the Enter the user pressed follows the text.
        let (typed, left) = ended_with_enter(Target::Focused, false);
        assert_eq!((typed, left.ok()), (then_enter.clone(), Some(None)));
        // One that ends with Enter anyway: once, not twice.
        let (typed, left) = ended_with_enter(Target::FocusedEnter, false);
        assert_eq!((typed, left.ok()), (then_enter, Some(None)));
        // The kinds that type nothing are not ended with Enter, and would
        // press none if they were.
        let (typed, left) = ended_with_enter(Target::Search, false);
        assert!(typed.is_empty());
        assert!(matches!(left, Ok(Some(Outcome::Search { .. }))), "{left:?}");
        let (typed, _) = ended_with_enter(Target::Capture, false);
        assert!(typed.is_empty());
        // A dictation that fails presses nothing: the words are not there.
        for target in [Target::Focused, Target::FocusedEnter] {
            let (typed, left) = ended_with_enter(target, true);
            assert!(typed.is_empty() && left.is_err(), "{typed:?} {left:?}");
        }
    }

    #[test]
    fn whoever_holds_enter_is_told_around_each_one_that_is_typed_and_no_other_text() -> Result<()> {
        // What happened, in order: told that one is coming, typed, told
        // that it has been.
        let told = Seen::default();
        let around: TypingEnter = {
            let told = told.clone();
            Arc::new(move |typing| {
                let _ = told.clone().type_text(if typing { "<" } else { ">" });
            })
        };
        let mut sink = Announcing {
            sink: told.clone(),
            enter: Some(around),
        };
        sink.type_text("One. ")?;
        sink.rewrite(0, "Two.")?;
        sink.type_text("\n")?;
        sink.rewrite(0, "Three\r\nfour")?;
        let happened = told
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        assert_eq!(
            happened,
            ["One. ", "Two.", "<", "\n", ">", "<", "Three\r\nfour", ">"]
        );
        // With nobody to tell, text is typed as ever.
        let mut sink = Announcing {
            sink: Seen::default(),
            enter: None,
        };
        sink.type_text("\n")?;
        assert_eq!(sink.sink.0.lock().map(|typed| typed.len()).ok(), Some(1));
        Ok(())
    }

    /// Enter ends a dictation with `VinPUT` Pro as the key that started it
    /// does: the microphone stops, the server is told that the audio is
    /// over, what it settles takes the place of its guess, one Enter follows
    /// that, and the stream is closed. So it is with either key.
    #[cfg(windows)]
    #[test]
    fn a_dictation_with_vinput_pro_ended_with_enter_types_its_last_words_and_closes_its_stream()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use crate::cloud::fake::Fake;
        use crate::stt::CloudSpeech;

        let fake = Fake::start()?;
        let (account, memory) = fake.account();
        memory.sign_in(&fake);
        let speech = CloudSpeech::connect(account, Arc::default())?;
        for (round, target) in [Target::FocusedEnter, Target::Focused]
            .into_iter()
            .enumerate()
        {
            // Spoken into until Enter is pressed, a quarter of a second in:
            // the host tells the session of it and stops the microphone.
            let stop = StopHandle::new();
            let entered = Arc::new(AtomicBool::new(false));
            let (pressed, stopped) = (Arc::clone(&entered), stop.clone());
            let (microphone, released) = Counted::new(1000, move |read| {
                thread::sleep(Duration::from_millis(10));
                if read == 25 {
                    pressed.store(true, Ordering::Release);
                    stopped.stop();
                }
            });
            let alone = AtomicBool::new(false);
            let mut field = Field::default();
            let started = Instant::now();
            let left = dictate(
                &speech,
                (&[], LIVE),
                || Ok(Stopped(microphone, stop)),
                (target, None),
                (&alone, &entered),
                Lent(&mut field),
                None,
            )?;
            // It did not listen on: nothing like the ten seconds the
            // microphone had left to hear.
            assert!(started.elapsed() < Duration::from_secs(5), "{target:?}");
            assert!(released.load(Ordering::SeqCst), "{target:?}");
            // The stand-in guesses "hello wor" and settles "Hello world.".
            assert_eq!((left, field.text.as_str()), (None, "Hello world.\n"));
            assert_eq!(
                field
                    .stood
                    .iter()
                    .filter(|text| text.ends_with('\n'))
                    .count(),
                1
            );
            // The server heard the end of the audio and the stream is gone.
            assert!(fake.wait(|seen| {
                seen.len() == round + 1 && seen[round].events.contains(&"closed")
            }));
            let events = fake.read(|state| state.streams[round].events.clone());
            assert!(events.ends_with(&["finish", "closed"]), "{events:?}");
            // Less than a third of a second of audio went out, not more.
            let sent = fake.read(|state| state.streams[round].audio.len());
            assert!(sent <= 30 * PACKET, "{sent}");
        }
        Ok(())
    }

    /// A microphone that has ended once it was stopped, as a real one has.
    struct Stopped<A>(A, StopHandle);

    impl<A: AudioSource> AudioSource for Stopped<A> {
        fn read(&mut self, buf: &mut [i16]) -> Result<usize> {
            if self.1.is_stopped() {
                return Ok(0);
            }
            self.0.read(buf)
        }
    }

    /// A dictation with `VinPUT` Pro that is forgotten ends by itself when
    /// nothing was said for as long as the settings allow, and its stream
    /// with it: nothing stays open that the account pays for.
    #[cfg(windows)]
    #[test]
    fn a_dictation_with_vinput_pro_that_ends_for_being_quiet_closes_its_stream()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use crate::cloud::fake::Fake;
        use crate::stt::CloudSpeech;

        let fake = Fake::start()?;
        let (account, memory) = fake.account();
        memory.sign_in(&fake);
        let speech = CloudSpeech::connect(account, Arc::default())?;
        let options = a_minute();
        let stop = StopHandle::new();
        let (alone, entered) = (AtomicBool::new(false), AtomicBool::new(false));
        let listen = || Ok(while_spoken_to(Still(stop.clone()), &options, stop, &alone));
        let seen = Seen::default();
        let target = (Target::FocusedEnter, None);
        let left = dictate(
            &speech,
            (&[], SETTLED),
            listen,
            target,
            (&alone, &entered),
            seen.clone(),
            None,
        )?;
        assert!(alone.load(Ordering::Acquire) && left.is_none());
        // What was settled is typed, and no Enter with nobody there.
        let typed = seen
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        assert_eq!(typed, ["Hello world."]);
        assert!(fake.wait(|seen| seen.len() == 1 && seen[0].events.contains(&"closed")));
        let events = fake.read(|state| state.streams[0].events.clone());
        assert!(events.ends_with(&["finish", "closed"]), "{events:?}");
        Ok(())
    }

    #[test]
    fn a_dictation_is_not_ended_for_being_quiet_while_its_key_is_held() {
        let mut dictation = Dictation {
            speech: Arc::new(Loading::new(false, false)),
            speech_config: Settings::default().speech_config(),
            rewrite_config: rewrite_config(&Settings::default()),
            options: a_minute(),
            meter: LevelMeter::new(),
            failures: Arc::default(),
            outcomes: Arc::default(),
            said: Arc::default(),
            active: None,
            finishing: Vec::new(),
            notify: None,
        };
        let options = dictation.options.clone();
        let stop = StopHandle::new();
        let alone = AtomicBool::new(false);
        let mut microphone = while_spoken_to(Still(stop.clone()), &options, stop.clone(), &alone);
        let mut buf = [0i16; 1600];
        let mut minutes = |minutes: usize| {
            (0..minutes * 600).all(|_| microphone.read(&mut buf).is_ok_and(|read| read > 0))
        };
        // The key is down: three minutes of nothing, and it listens on.
        dictation.held(true);
        assert!(minutes(3));
        assert!(!stop.is_stopped());
        // Let go after a tap, it goes on by itself, for a minute of nothing.
        dictation.held(false);
        assert!(!minutes(2));
        assert!(stop.is_stopped() && alone.load(Ordering::Acquire));
    }

    #[test]
    fn a_session_whose_thread_never_ran_has_ended_too() {
        // What Windows does with the work of a thread it could not start.
        let (closing, heard, ended) = closing();
        assert!(!ended.load(Ordering::Acquire));
        drop(closing);
        assert!(ended.load(Ordering::Acquire));
        assert_eq!(told(&heard).len(), 1);
    }

    #[test]
    fn a_session_ends_with_nobody_to_tell() {
        let ended = Arc::new(AtomicBool::new(false));
        let outcomes = Arc::new(Mutex::new(Vec::new()));
        Closing {
            outcomes: Arc::clone(&outcomes),
            failures: Arc::default(),
            id: None,
            provider: ProviderKind::Local,
            ended: Arc::clone(&ended),
            notify: None,
        }
        .with(Ok(Some(Outcome::Search {
            text: "weather".to_owned(),
        })));
        assert!(ended.load(Ordering::Acquire));
        assert_eq!(
            outcomes
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .len(),
            1
        );
    }

    /// A recognizer that settles on one text when the audio ends.
    struct Says(&'static str);

    impl SpeechProvider for Says {
        fn kind(&self) -> ProviderKind {
            ProviderKind::Local
        }

        fn start_session(&self, _: SessionOptions<'_>) -> Result<Session> {
            Ok(Box::new(Settles(self.0)))
        }
    }

    struct Settles(&'static str);

    impl Transcriber for Settles {
        fn feed(&mut self, _: &[i16]) -> Result<Option<Transcript>> {
            Ok(None)
        }

        fn finish(&mut self, emit: &mut Emit<'_>) -> Result<()> {
            if self.0.is_empty() {
                return Ok(());
            }
            emit(Transcript {
                text: self.0.to_owned(),
                is_final: true,
            })
        }
    }

    /// What was sent to the window, one call at a time: which, and with what.
    type Sent = Vec<(&'static str, String)>;

    /// What a made-up window was sent: `typed`, or `pasted`, and the text.
    #[derive(Default, Clone)]
    struct Board(Arc<Mutex<Sent>>);

    impl Board {
        fn sent(&self) -> Sent {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }

        fn note(&self, how: &'static str, text: &str) {
            let mut sent = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            sent.push((how, text.to_owned()));
        }
    }

    impl TextSink for Board {
        fn type_text(&mut self, text: &str) -> Result<()> {
            self.note("typed", text);
            Ok(())
        }

        fn paste(&mut self, text: &str) -> Result<bool> {
            self.note("pasted", text);
            Ok(true)
        }
    }

    /// A dictation of which the recognizer makes `said`, tidied by `model`
    /// on its way to `target`: what was sent to the window, and what is
    /// left for the host.
    fn tidied(
        said: &'static str,
        model: &Scripted,
        target: Target,
    ) -> Result<(Sent, Option<Outcome>)> {
        let board = Board::default();
        let cancelled = AtomicBool::new(false);
        let alone = AtomicBool::new(false);
        let languages = ["en-US".to_owned()];
        let left = dictate(
            &Says(said),
            (&languages, SETTLED),
            || Ok(Counted::new(3, |_| {}).0),
            (target, None),
            (&alone, &AtomicBool::new(false)),
            board.clone(),
            Some(Tidy {
                rewriter: model,
                kind: Kind::Clean,
                snippets: None,
                stop: &cancelled,
            }),
        )?;
        Ok((board.sent(), left))
    }

    fn sent(how: &'static str, text: &str) -> (&'static str, String) {
        (how, text.to_owned())
    }

    #[test]
    fn a_dictation_is_tidied_before_it_is_typed_and_enter_follows_the_tidied_text() -> Result<()> {
        let model = Scripted::writing(&["So we meet ", "at six."]);
        let (typed, left) = tidied(" um so we meet at six ", &model, Target::Focused)?;
        assert_eq!(
            typed,
            [sent("typed", "So we meet"), sent("typed", " at six.")]
        );
        assert_eq!(left, None);
        // The model was given what was said, without the space around it.
        assert_eq!(
            model.asked(),
            [(
                Kind::Clean,
                "um so we meet at six".to_owned(),
                String::new()
            )]
        );
        let (typed, _) = tidied(
            "um yes",
            &Scripted::writing(&["Yes."]),
            Target::FocusedEnter,
        )?;
        assert_eq!(typed, [sent("typed", "Yes."), sent("typed", "\n")]);
        Ok(())
    }

    /// Guesses typed as well as settled text, and a preview while tidying.
    const LIVE: Typing = Typing {
        live: true,
        preview: true,
    };
    /// Settled text alone, and nothing while tidying.
    const SETTLED: Typing = Typing {
        live: false,
        preview: false,
    };

    /// A made-up text field that takes text back: what stands in it, and
    /// each call it was sent.
    #[derive(Default, Clone)]
    struct Screen(Arc<Mutex<(String, Sent, bool)>>);

    impl Screen {
        fn text(&self) -> String {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .0
                .clone()
        }

        fn sent(&self) -> Sent {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .1
                .clone()
        }

        /// The caret is moved away from what was typed.
        fn move_caret(&self) {
            self.0.lock().unwrap_or_else(PoisonError::into_inner).2 = true;
        }
    }

    impl TextSink for Screen {
        fn type_text(&mut self, text: &str) -> Result<()> {
            let mut field = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            field.0.push_str(text);
            field.1.push(("typed", text.to_owned()));
            Ok(())
        }

        fn rewrites(&self) -> bool {
            true
        }

        fn rewrite(&mut self, erase: usize, text: &str) -> Result<()> {
            let mut field = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            for _ in 0..erase {
                field.0.pop();
            }
            field.0.push_str(text);
            field.1.push(("erased", format!("{erase}:{text}")));
            Ok(())
        }

        fn disturbed(&mut self) -> bool {
            self.0.lock().unwrap_or_else(PoisonError::into_inner).2
        }

        fn paste(&mut self, text: &str) -> Result<bool> {
            let mut field = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            field.0.push_str(text);
            field.1.push(("pasted", text.to_owned()));
            Ok(true)
        }
    }

    /// A dictation of `said` tidied by `model` into `field` with a preview,
    /// with the caret moved away while the user speaks if `moved`.
    fn previewed_into(
        field: &Screen,
        said: &'static str,
        model: &Scripted,
        target: Target,
        moved: bool,
    ) -> Result<Option<Outcome>> {
        let cancelled = AtomicBool::new(false);
        let languages = ["en-US".to_owned()];
        let mark = field.clone();
        dictate(
            &Says(said),
            (&languages, LIVE),
            move || {
                if moved {
                    mark.move_caret();
                }
                Ok(Counted::new(3, |_| {}).0)
            },
            (target, None),
            (&AtomicBool::new(false), &AtomicBool::new(false)),
            field.clone(),
            Some(Tidy {
                rewriter: model,
                kind: Kind::Clean,
                snippets: None,
                stop: &cancelled,
            }),
        )
    }

    #[test]
    fn what_is_said_shows_while_it_is_tidied_and_the_tidied_text_takes_its_place() -> Result<()> {
        let field = Screen::default();
        let model = Scripted::writing(&["So we meet ", "at six."]);
        let left = previewed_into(
            &field,
            "um so we\nmeet at six",
            &model,
            Target::FocusedEnter,
            false,
        )?;
        assert_eq!(left, None);
        // The line break was shown as a space, not pressed as Enter; the
        // model's first words took the place of the preview in one go, and
        // the Enter came after the tidied text.
        assert_eq!(
            field.sent(),
            [
                sent("erased", "0:um so we meet at six"),
                sent("erased", "20:So we meet"),
                sent("typed", " at six."),
                sent("typed", "\n"),
            ]
        );
        assert_eq!(field.text(), "So we meet at six.\n");
        // The model was given what was said, line break and all.
        assert_eq!(model.asked()[0].1, "um so we\nmeet at six");
        Ok(())
    }

    #[test]
    fn a_preview_is_left_as_it_is_once_the_caret_has_moved() -> Result<()> {
        let field = Screen::default();
        let model = Scripted::writing(&["So we meet at six."]);
        let left = previewed_into(
            &field,
            "um so we meet at six",
            &model,
            Target::Focused,
            true,
        )?;
        assert_eq!(field.text(), "um so we meet at six");
        assert!(model.asked().is_empty());
        let Some(Outcome::Told(told)) = left else {
            return Err(Error::Host(format!("the user is told: {left:?}")));
        };
        assert_eq!(told.message, LEFT_AS_SAID);
        Ok(())
    }

    #[test]
    fn a_preview_that_the_model_fails_to_tidy_ends_as_it_was_said() -> Result<()> {
        let field = Screen::default();
        let model = Scripted::new(Script::NeedsSetup("Choose a model."));
        let left = previewed_into(
            &field,
            "um so we meet at six",
            &model,
            Target::Focused,
            false,
        )?;
        assert_eq!(field.text(), "um so we meet at six");
        assert!(matches!(left, Some(Outcome::Told(_))));
        Ok(())
    }

    #[test]
    fn the_model_sees_the_users_spelling_and_the_snippets_follow_what_it_wrote() -> Result<()> {
        let words = words::Words {
            dictionary: vec![words::Word {
                word: "VinPUT".to_owned(),
                sounds_like: "vin put".to_owned(),
            }],
            snippets: vec![words::Snippet {
                trigger: "my email".to_owned(),
                text: "me@example.com".to_owned(),
            }],
            commands: true,
        };
        let (before, after) = Rules::around_a_model(&words);
        let model = Scripted::writing(&["Write to VinPUT at my", " email."]);
        let board = Board::default();
        let cancelled = AtomicBool::new(false);
        let languages = ["en-US".to_owned()];
        let speech = InOwnWords {
            speech: &Says("um write to vin put at my email"),
            rules: before,
        };
        dictate(
            &speech,
            (&languages, SETTLED),
            || Ok(Counted::new(3, |_| {}).0),
            (Target::Focused, None),
            (&AtomicBool::new(false), &AtomicBool::new(false)),
            board.clone(),
            Some(Tidy {
                rewriter: &model,
                kind: Kind::Clean,
                snippets: after.as_ref(),
                stop: &cancelled,
            }),
        )?;
        // The dictionary has spelled the name, and the snippet is still the
        // words that were said.
        assert_eq!(model.asked()[0].1, "um write to VinPUT at my email");
        let typed: String = board.sent().into_iter().map(|(_, text)| text).collect();
        assert_eq!(typed, "Write to VinPUT at me@example.com.");
        Ok(())
    }

    #[test]
    fn a_dictation_that_cannot_be_tidied_is_typed_as_said_and_the_host_is_told() -> Result<()> {
        let model = Scripted::new(Script::NeedsSetup("Choose a model."));
        let (typed, left) = tidied("um yes ", &model, Target::FocusedEnter)?;
        // What a plain dictation would have typed, and its Enter.
        assert_eq!(typed, [sent("typed", "um yes "), sent("typed", "\n")]);
        let Some(Outcome::Told(told)) = left else {
            return Err(Error::Host(format!("{left:?}")));
        };
        assert!(told.setup);
        assert_eq!(told.message, "Choose a model.");
        Ok(())
    }

    #[test]
    fn nothing_said_is_nothing_asked_of_the_model() -> Result<()> {
        let model = Scripted::writing(&["Made up."]);
        let (typed, left) = tidied("", &model, Target::FocusedEnter)?;
        assert!(typed.is_empty() && left.is_none());
        assert!(model.asked().is_empty());
        Ok(())
    }

    #[test]
    fn only_the_keys_that_type_tidy_and_each_in_its_own_way() {
        let settings = Settings {
            cleanup: Mode::Clean,
            cleanup_enter: Mode::Prompt,
            ..Settings::default()
        };
        let microphone = Arc::new(ArmedMicrophone::new(&settings.microphone));
        let rewriter = Arc::new(Scripted::writing(&[]));
        let options = Options::of(&settings, microphone, Arc::default(), rewriter);
        assert_eq!(options.tidying(Target::Focused), Some(Kind::Clean));
        assert_eq!(options.tidying(Target::FocusedEnter), Some(Kind::Prompt));
        for other in [
            Target::Capture,
            Target::Search,
            Target::Memo,
            Target::Command,
        ] {
            assert_eq!(options.tidying(other), None, "{other:?}");
        }
        assert!(options.rewrites(Target::Command) && options.rewrites(Target::Focused));
        assert!(!options.rewrites(Target::Capture));
        // As it is said, unless the settings say otherwise.
        let plain = a_minute();
        assert_eq!(plain.tidying(Target::Focused), None);
        assert!(!plain.rewrites(Target::FocusedEnter) && plain.rewrites(Target::Command));
    }

    /// What is selected in a made-up window.
    #[derive(Default)]
    struct Chosen {
        /// What the program says when asked.
        says: Option<&'static str>,
        /// What copying yields.
        copies: Option<&'static str>,
        /// Whether the selection was copied.
        copied: AtomicBool,
        /// Whether the user goes to another window once the selection was
        /// taken.
        wanders: bool,
        looked: AtomicUsize,
    }

    impl Selection for Chosen {
        fn asked(&self) -> Option<String> {
            self.says.map(str::to_owned)
        }

        fn copied(&self) -> Option<String> {
            self.copied.store(true, Ordering::Release);
            self.copies.map(str::to_owned)
        }

        fn window(&self) -> usize {
            let looks = self.looked.fetch_add(1, Ordering::AcqRel);
            if self.wanders { looks } else { 7 }
        }
    }

    /// The instruction `said` over what is `chosen`, carried out by
    /// `model`: what was sent to the window, and what the user is told.
    fn commanded(
        said: &'static str,
        chosen: &Chosen,
        model: &Scripted,
        called_off: bool,
    ) -> Result<(Sent, Option<Told>)> {
        let board = Board::default();
        let cancelled = AtomicBool::new(called_off);
        let told = command(
            &Says(said),
            &[],
            || Ok(Counted::new(3, |_| {}).0),
            (chosen, model),
            board.clone(),
            &cancelled,
        )?;
        Ok((board.sent(), told))
    }

    #[test]
    fn a_selection_is_edited_as_told_and_the_result_put_in_its_place_whole() -> Result<()> {
        let chosen = Chosen {
            says: Some("A long paragraph."),
            ..Chosen::default()
        };
        let model = Scripted::writing(&[" A short ", "one.\n"]);
        let (sent_to, told) = commanded(" make it shorter ", &chosen, &model, false)?;
        assert_eq!(sent_to, [sent("pasted", "A short one.")]);
        assert_eq!(told, None);
        assert_eq!(
            model.asked(),
            [(
                Kind::Command,
                "A long paragraph.".to_owned(),
                "make it shorter".to_owned()
            )]
        );
        // The program answered, so nothing was copied.
        assert!(!chosen.copied.load(Ordering::Acquire));
        Ok(())
    }

    #[test]
    fn a_program_that_does_not_say_what_is_selected_has_it_copied() -> Result<()> {
        let chosen = Chosen {
            copies: Some("Copied text."),
            ..Chosen::default()
        };
        let model = Scripted::writing(&["Edited."]);
        let (sent_to, told) = commanded("fix it", &chosen, &model, false)?;
        assert_eq!((sent_to, told), (vec![sent("pasted", "Edited.")], None));
        assert_eq!(model.asked()[0].1, "Copied text.");
        Ok(())
    }

    #[test]
    fn a_selection_is_left_as_it_was_whenever_it_cannot_be_edited() -> Result<()> {
        let text = Chosen {
            says: Some("Some text."),
            ..Chosen::default()
        };
        let writes = || Scripted::writing(&["Edited."]);
        // Nothing is selected.
        let (sent_to, told) = commanded("shorter", &Chosen::default(), &writes(), false)?;
        assert!(sent_to.is_empty());
        assert!(told.is_some_and(|told| told.message.starts_with("Select the text first")));
        // Nothing was said: the model is not asked, and nothing is copied.
        let model = writes();
        let silent = Chosen::default();
        let (sent_to, told) = commanded("", &silent, &model, false)?;
        assert!(sent_to.is_empty() && model.asked().is_empty());
        assert!(!silent.copied.load(Ordering::Acquire));
        assert_eq!(
            told.map(|told| (told.title, told.message)),
            Some((
                "Nothing was changed",
                "No instruction was heard.".to_owned()
            ))
        );
        // The model cannot be had, fails halfway, or writes nothing.
        for (script, setup) in [
            (Script::NeedsSetup("Choose a model."), true),
            (
                Script::Breaks(vec!["Half of "], "The server stopped."),
                false,
            ),
            (Script::Writes(vec![" \n"]), false),
        ] {
            let (sent_to, told) = commanded("shorter", &text, &Scripted::new(script), false)?;
            assert!(sent_to.is_empty(), "{sent_to:?}");
            assert!(told.is_some_and(|told| told.setup == setup && !told.message.is_empty()));
        }
        // The user went to another window while the model wrote.
        let wandered = Chosen {
            says: Some("Some text."),
            wanders: true,
            ..Chosen::default()
        };
        let (sent_to, told) = commanded("shorter", &wandered, &writes(), false)?;
        assert!(sent_to.is_empty());
        assert!(told.is_some_and(|told| told.message.contains("another window")));
        // Called off: nothing is put anywhere and nothing is said.
        let (sent_to, told) = commanded("shorter", &text, &writes(), true)?;
        assert!(sent_to.is_empty() && told.is_none());
        // More than a model is asked to take.
        let endless: &'static str = Box::leak("x".repeat(rewrite::MAX_TEXT_CHARS + 1).into());
        let long = Chosen {
            says: Some(endless),
            ..Chosen::default()
        };
        let model = writes();
        let (sent_to, told) = commanded("shorter", &long, &model, false)?;
        assert!(sent_to.is_empty() && model.asked().is_empty());
        assert!(told.is_some_and(|told| told.message.contains("too long")));
        Ok(())
    }

    /// Options of someone who lets connected programs read what is typed,
    /// and where it is left for them.
    fn read_by_programs() -> (Options, Arc<Transcripts>) {
        let said = Arc::<Transcripts>::default();
        let options = Options {
            said: Some(Arc::clone(&said)),
            ..a_minute()
        };
        (options, said)
    }

    fn said(session: u64, target: Target, text: &str, last: bool) -> Said {
        Said {
            session,
            target,
            id: None,
            text: text.to_owned(),
            last,
        }
    }

    #[test]
    fn what_is_typed_is_left_for_programs_as_it_stands_and_once_more_at_the_end() {
        let (options, left) = read_by_programs();
        let mut field = Field::default();
        let mut sink = typing_into(Lent(&mut field), &options, Target::Focused, None);
        assert!(left.take().is_empty(), "nothing was typed yet");

        assert!(sink.type_text("Hello wor").is_ok());
        assert_eq!(left.take(), [said(1, Target::Focused, "Hello wor", false)]);
        assert!(left.take().is_empty(), "it is taken once");

        // Words that change are taken back, here across a letter that is
        // more than one byte; only how it stands now is kept.
        assert!(sink.rewrite(3, "world, ž").is_ok());
        assert!(sink.rewrite(1, "š.").is_ok());
        assert_eq!(
            left.take(),
            [said(1, Target::Focused, "Hello world, š.", false)]
        );

        drop(sink);
        assert_eq!(
            left.take(),
            [said(1, Target::Focused, "Hello world, š.", true)]
        );
        assert_eq!(field.text, "Hello world, š.");

        // The next dictation has a number of its own, and the id it was
        // started with.
        let id = Some("deck-7".to_owned());
        let mut sink = typing_into(Field::default(), &options, Target::FocusedEnter, id.clone());
        assert!(sink.type_text("Next").is_ok());
        drop(sink);
        assert_eq!(
            left.take(),
            [Said {
                id,
                ..said(2, Target::FocusedEnter, "Next", true)
            }]
        );
    }

    #[test]
    fn what_is_typed_is_handed_on_a_few_times_a_second_and_its_end_at_once() {
        let (options, left) = read_by_programs();
        let start = Instant::now();
        let mut sink = typing_into(Field::default(), &options, Target::Focused, None);
        assert!(left.take_due(start).is_empty(), "nothing was typed yet");
        assert!(sink.type_text("a").is_ok());
        assert_eq!(left.take_due(start), [said(1, Target::Focused, "a", false)]);
        // Letters that follow at once wait, and are handed on together.
        assert!(sink.type_text("b").is_ok());
        assert!(left.take_due(start + SAID_EVERY / 2).is_empty());
        assert!(sink.type_text("c").is_ok());
        assert_eq!(
            left.take_due(start + SAID_EVERY),
            [said(1, Target::Focused, "abc", false)]
        );
        // The end does not wait.
        assert!(sink.type_text("d").is_ok());
        drop(sink);
        assert_eq!(
            left.take_due(start + SAID_EVERY),
            [said(1, Target::Focused, "abcd", true)]
        );
    }

    #[test]
    fn a_dictation_that_typed_nothing_still_says_that_it_is_over() {
        let (options, left) = read_by_programs();
        drop(typing_into(
            Field::default(),
            &options,
            Target::Focused,
            None,
        ));
        assert_eq!(left.take(), [said(1, Target::Focused, "", true)]);
        // What a program asked for, or a search, is not typed: there is
        // nothing to say of it, unless it came to be typed after all.
        for target in [Target::Capture, Target::Search] {
            drop(typing_into(Field::default(), &options, target, None));
        }
        assert!(left.take().is_empty());
        let mut sink = typing_into(Field::default(), &options, Target::Search, None);
        assert!(sink.type_text("weather").is_ok());
        drop(sink);
        assert_eq!(left.take(), [said(4, Target::Search, "weather", true)]);
    }

    #[test]
    fn nothing_is_left_for_programs_unless_the_settings_say_so() {
        // The settings as they come: programs may connect, and are not
        // sent what is typed.
        let settings = Settings::default();
        assert!(settings.api_enabled && !settings.api_transcript);
        let kept = Arc::<Transcripts>::default();
        assert!(said_for(&settings, &kept).is_none());
        let on = Settings {
            api_transcript: true,
            ..Settings::default()
        };
        assert!(said_for(&on, &kept).is_some());
        // Switched on, but no program may connect.
        let unconnected = Settings {
            api_enabled: false,
            ..on.clone()
        };
        assert!(said_for(&unconnected, &kept).is_none());

        // Such a dictation keeps no copy of what it types.
        let options = a_minute();
        assert!(options.said.is_none());
        let mut sink = typing_into(Field::default(), &options, Target::Focused, None);
        assert!(sink.type_text("for nobody else").is_ok());
        assert!(sink.typed.is_empty() && sink.said.is_none());
        drop(sink);
        assert!(kept.take().is_empty());
    }

    #[test]
    fn what_could_not_be_typed_is_not_said_to_have_been() {
        struct Shut;
        impl TextSink for Shut {
            fn type_text(&mut self, _: &str) -> Result<()> {
                Err(Error::Injection("the window is gone".into()))
            }
        }
        let (options, left) = read_by_programs();
        let mut sink = typing_into(Shut, &options, Target::Focused, None);
        assert!(sink.type_text("lost").is_err());
        drop(sink);
        assert_eq!(left.take(), [said(1, Target::Focused, "", true)]);
    }

    /// Programs are told what was typed and not what was recognized: the
    /// text in the user's own words, with the Enter that follows it.
    #[test]
    fn programs_read_the_text_in_the_users_own_words() {
        use crate::words::Words;

        let provider = Loading::new(false, false);
        provider.gate.open();
        let mut words = Words::default();
        assert_eq!(words.add_snippet("end", "Kind regards"), Ok(true));
        let speech = InOwnWords {
            speech: &provider,
            rules: Rules::of(&words),
        };
        let (options, left) = read_by_programs();
        let mut field = Field::default();
        let (microphone, _) = Counted::new(3, |_| {});
        let ended = dictate(
            &speech,
            (&[], SETTLED),
            || Ok(microphone),
            (Target::FocusedEnter, None),
            (&AtomicBool::new(false), &AtomicBool::new(false)),
            typing_into(Lent(&mut field), &options, Target::FocusedEnter, None),
            None,
        );
        assert!(matches!(ended, Ok(None)), "{ended:?}");
        assert_eq!(field.text, "Kind regards\n");
        assert_eq!(
            left.take(),
            [said(1, Target::FocusedEnter, "Kind regards\n", true)]
        );
    }

    /// A tidied dictation tells programs what the model's version came to
    /// with the snippets in it, and an edit of the selection what was put
    /// in its place, under a mode of its own.
    #[test]
    fn programs_read_what_cleanup_and_editing_put_on_the_screen() -> Result<()> {
        let mut words = words::Words::default();
        assert_eq!(words.add_snippet("my email", "me@example.com"), Ok(true));
        let (before, after) = Rules::around_a_model(&words);
        let (options, left) = read_by_programs();
        let model = Scripted::writing(&["Write to me at my", " email."]);
        let speech = InOwnWords {
            speech: &Says("um write to me at my email"),
            rules: before,
        };
        let board = Board::default();
        dictate(
            &speech,
            (&[], SETTLED),
            || Ok(Counted::new(3, |_| {}).0),
            (Target::Focused, None),
            (&AtomicBool::new(false), &AtomicBool::new(false)),
            typing_into(board.clone(), &options, Target::Focused, None),
            Some(Tidy {
                rewriter: &model,
                kind: Kind::Clean,
                snippets: after.as_ref(),
                stop: &AtomicBool::new(false),
            }),
        )?;
        let text = "Write to me at me@example.com.";
        assert_eq!(left.take(), [said(1, Target::Focused, text, true)]);

        let chosen = Chosen {
            says: Some("A long paragraph."),
            ..Chosen::default()
        };
        let told = command(
            &Says("make it shorter"),
            &[],
            || Ok(Counted::new(3, |_| {}).0),
            (&chosen, &Scripted::writing(&["Short."])),
            typing_into(board.clone(), &options, Target::Command, None),
            &AtomicBool::new(false),
        )?;
        assert_eq!(told, None);
        assert_eq!(board.sent().last(), Some(&sent("pasted", "Short.")));
        assert_eq!(left.take(), [said(2, Target::Command, "Short.", true)]);
        Ok(())
    }

    #[test]
    fn nothing_is_passed_on_from_the_moment_it_is_switched_off() {
        let on = Settings {
            api_transcript: true,
            ..Settings::default()
        };
        let microphone = Arc::new(ArmedMicrophone::new(&on.microphone));
        let kept = Arc::<Transcripts>::default();
        let mut dictation = Dictation {
            speech: Arc::new(Loading::new(false, false)),
            speech_config: on.speech_config(),
            rewrite_config: rewrite_config(&on),
            options: Options {
                said: said_for(&on, &kept),
                ..Options::of(&on, microphone, Arc::default(), quiet_model())
            },
            meter: LevelMeter::new(),
            failures: Arc::default(),
            outcomes: Arc::default(),
            said: kept,
            active: None,
            finishing: Vec::new(),
            notify: None,
        };
        // A dictation that started while it was on goes on typing.
        let options = dictation.options.clone();
        let mut sink = typing_into(Field::default(), &options, Target::Focused, None);
        assert!(sink.type_text("one").is_ok());
        assert_eq!(
            dictation.take_said(),
            [said(1, Target::Focused, "one", false)]
        );
        assert!(sink.type_text(" two").is_ok());
        dictation.configure(&Settings::default(), false);
        assert!(dictation.take_said().is_empty());
        drop(sink);
        assert!(dictation.take_said().is_empty());
        // And what it typed meanwhile does not come out when it is
        // switched on again.
        dictation.configure(&on, false);
        assert!(dictation.take_said().is_empty());
    }

    /// A recognizer that settles on `0` as soon as it hears anything, and
    /// on `1` when the audio ends.
    struct SaysInTwo(&'static str, &'static str);

    impl SpeechProvider for SaysInTwo {
        fn kind(&self) -> ProviderKind {
            ProviderKind::Local
        }

        fn start_session(&self, _: SessionOptions<'_>) -> Result<Session> {
            Ok(Box::new(SettlesInTwo(Some(self.0), self.1)))
        }
    }

    struct SettlesInTwo(Option<&'static str>, &'static str);

    impl Transcriber for SettlesInTwo {
        fn feed(&mut self, _: &[i16]) -> Result<Option<Transcript>> {
            Ok(self.0.take().map(|text| Transcript {
                text: text.to_owned(),
                is_final: true,
            }))
        }

        fn finish(&mut self, emit: &mut Emit<'_>) -> Result<()> {
            emit(Transcript {
                text: self.1.to_owned(),
                is_final: true,
            })
        }
    }

    /// The real model `VINPUT_TEXT_MODEL` chooses (empty: the one of
    /// `VinPUT` Pro), made ready. A local one that is not on this PC is
    /// installed first, and then `None` is returned: whatever has the focus
    /// after a long download is not what was focused for a test.
    #[cfg(windows)]
    #[allow(clippy::print_stderr, reason = "how far the download is")]
    fn real_rewriter() -> Result<Option<Arc<dyn Rewriter>>> {
        let settings = Settings {
            text_model: std::env::var("VINPUT_TEXT_MODEL").unwrap_or_default(),
            cleanup: Mode::Clean,
            ..Settings::default()
        };
        if let Some(model) = crate::models::writer(&settings.text_model) {
            let gpu = settings.use_gpu();
            if !crate::models::is_installed(model, gpu) {
                let installer = crate::models::Installer::new();
                installer.install(model, gpu);
                loop {
                    match installer.status(model, gpu) {
                        crate::models::Status::Installing { done, total } => {
                            eprintln!("installing {}: {done} of {total}", model.id);
                        }
                        crate::models::Status::Installed => break,
                        other => return Err(Error::Host(format!("{other:?}"))),
                    }
                    thread::sleep(Duration::from_secs(5));
                }
                eprintln!("{} is installed; run again", model.id);
                return Ok(None);
            }
        }
        let rewriter = rewrite::connect(&settings);
        rewriter.make_ready();
        Ok(Some(rewriter))
    }

    /// A dictation said in two pieces two seconds apart, shown into `sink`
    /// while it is said and tidied by `rewriter` after.
    #[cfg(windows)]
    #[allow(clippy::print_stderr, reason = "what it did is its output")]
    fn said_in_two_and_tidied(
        rewriter: &dyn Rewriter,
        target: Target,
        sink: impl TextSink,
    ) -> Result<Option<Outcome>> {
        let cancelled = AtomicBool::new(false);
        let languages = ["en-US".to_owned()];
        let started = Instant::now();
        let left = dictate(
            &SaysInTwo(
                "um so I think we should uh meet at six",
                " tomorrow at the office, no wait, at the cafe",
            ),
            (&languages, LIVE),
            || Ok(Counted::new(20, |_| thread::sleep(Duration::from_millis(100))).0),
            (target, None),
            (&AtomicBool::new(false), &AtomicBool::new(false)),
            sink,
            Some(Tidy {
                rewriter,
                kind: Kind::Clean,
                snippets: None,
                stop: &cancelled,
            }),
        )?;
        eprintln!("done in {:?}: {left:?}", started.elapsed());
        Ok(left)
    }

    /// What is said shows in the window in front while the user speaks, and
    /// the real model's tidied text takes its place. Focus a text field
    /// within three seconds of the start; `VINPUT_TEST_ENTER=1` ends with
    /// Enter. See [`real_rewriter`] for the model.
    #[cfg(windows)]
    #[test]
    #[ignore = "types into the window in front, and needs a text model"]
    fn a_real_preview_is_replaced_by_real_tidied_text() -> Result<()> {
        let Some(rewriter) = real_rewriter()? else {
            return Ok(());
        };
        let target = if std::env::var("VINPUT_TEST_ENTER").as_deref() == Ok("1") {
            Target::FocusedEnter
        } else {
            Target::Focused
        };
        thread::sleep(Duration::from_secs(3));
        said_in_two_and_tidied(rewriter.as_ref(), target, SendInputSink::new())?;
        Ok(())
    }

    /// The same with a made-up field: once the real model is done, the
    /// field holds what it wrote and nothing of what was said.
    #[cfg(windows)]
    #[test]
    #[ignore = "needs a text model"]
    #[allow(clippy::print_stderr, reason = "what it did is its output")]
    fn a_preview_is_replaced_by_real_tidied_text() -> Result<()> {
        let Some(rewriter) = real_rewriter()? else {
            return Ok(());
        };
        let field = Screen::default();
        let left = said_in_two_and_tidied(rewriter.as_ref(), Target::FocusedEnter, field.clone())?;
        eprintln!("sent: {:#?}", field.sent());
        let text = field.text();
        eprintln!("field: {text:?}");
        assert_eq!(left, None);
        assert!(text.ends_with('\n') && !text[..text.len() - 1].contains('\n'));
        for raw in ["um ", " uh "] {
            assert!(!text.contains(raw), "{raw:?} is left in {text:?}");
        }
        assert_eq!(text.matches("six").count(), 1, "{text:?}");
        assert!(text.contains("caf"), "{text:?}");
        // Whether the model follows "no wait" is up to the model, and not
        // checked. What was said was on the screen first, as it was said,
        // and was taken back whole in one go by the model's first words.
        let said =
            "um so I think we should uh meet at six tomorrow at the office, no wait, at the cafe";
        let sent = field.sent();
        let shown = sent.iter().take_while(|(_, text)| text.starts_with("0:"));
        let shown: String = shown.map(|(_, text)| &text[2..]).collect();
        assert_eq!(shown, said);
        let replaced = &sent[sent
            .iter()
            .filter(|(_, text)| text.starts_with("0:"))
            .count()];
        assert!(
            replaced
                .1
                .starts_with(&format!("{}:", said.chars().count())),
            "{replaced:?}"
        );
        assert!(
            sent.iter().skip(1).all(|(how, text)| how == &"typed"
                || text.starts_with("0:")
                || text == &replaced.1)
        );
        Ok(())
    }
}
