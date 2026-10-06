//! Runs sessions: each one captures the microphone and either streams it to
//! the recognizer and types the result or hands it on, or records it as a voice
//! memo, on its own thread.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, Scope};

use crate::audio::{
    ArmedMicrophone, AudioSource, LevelMeter, Metered, SAMPLE_RATE_HZ, StopHandle, UntilQuiet,
};
use crate::inject::{SendInputSink, TextSink};
use crate::session::{Controller, Ended, Failure, Outcome, Target};
use crate::settings::{Settings, SpeechConfig};
use crate::sound::{self, Cue};
use crate::stt::{
    self, Emit, ProviderKind, SessionOptions, SpeechProvider, Transcriber, Transcript,
};
use crate::{Error, Result, log, memo, pipeline};

/// The most audio that is kept for a recognizer that is not there yet: one
/// minute, which is 2 MB. The largest local model loads in a few seconds; a
/// recognizer that takes longer than this has something wrong with it, and
/// the session is not allowed to grow without end while it waits.
const MAX_WAITING_SAMPLES: usize = SAMPLE_RATE_HZ as usize * 60;
/// How much of the kept audio a recognizer is handed at a time, once it is
/// there: 100 ms, the most it is ever handed while listening.
const REPLAY_SAMPLES: usize = SAMPLE_RATE_HZ as usize / 10;

/// A running session: the switch that ends it, whether it was called off,
/// and whether its thread has left behind everything it is going to.
struct Active {
    stop: StopHandle,
    cancelled: Arc<AtomicBool>,
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
}

impl Options {
    fn of(settings: &Settings, microphone: Arc<ArmedMicrophone>, held: Arc<AtomicBool>) -> Self {
        Self {
            held,
            languages: settings.languages.clone(),
            start_sound: settings.ready_sound,
            stop_sound: settings.stop_sound,
            stop_after_minutes: settings.stop_after_minutes,
            microphone,
            memo_dir: settings.memo_dir.clone(),
            memo_to_clipboard: settings.memo_to_clipboard,
        }
    }
}

/// Starts and stops sessions on a shared speech provider.
pub struct Dictation {
    speech: Arc<dyn SpeechProvider>,
    /// What `speech` was set up from, to notice when it has to be set up again.
    speech_config: SpeechConfig,
    options: Options,
    meter: LevelMeter,
    /// Where failed sessions leave their errors for the host to show, each
    /// with the id of its session: one failure never takes the place of
    /// another.
    failures: Arc<Mutex<Vec<Failure>>>,
    /// Where finished sessions leave what the host has to pass on.
    outcomes: Arc<Mutex<Vec<Outcome>>>,
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
        Self {
            speech: stt::connect_in_background(settings),
            speech_config: settings.speech_config(),
            options: Options::of(settings, microphone, Arc::default()),
            meter: LevelMeter::new(),
            failures: Arc::default(),
            outcomes: Arc::default(),
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
                    &called_off,
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

    fn level(&self) -> f32 {
        self.meter.level()
    }

    fn take_failures(&mut self) -> Vec<Failure> {
        std::mem::take(&mut *self.failures.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn take_outcomes(&mut self) -> Vec<Outcome> {
        std::mem::take(&mut *self.outcomes.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn configure(&mut self, settings: &Settings, reconnect: bool) {
        let microphone = Arc::clone(&self.options.microphone);
        microphone.set_microphone(&settings.microphone);
        let held = Arc::clone(&self.options.held);
        self.options = Options::of(settings, microphone, held);
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
    cancelled: &AtomicBool,
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
        sink: SendInputSink::new(),
        cancelled,
    };
    dictate(
        speech,
        &options.languages,
        microphone,
        (target, id),
        &alone,
        sink,
    )
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
fn dictate<A: AudioSource>(
    speech: &dyn SpeechProvider,
    languages: &[String],
    listen: impl FnOnce() -> Result<A>,
    (target, id): (Target, Option<String>),
    alone: &AtomicBool,
    sink: impl TextSink,
) -> Result<Option<Outcome>> {
    let mut sink = Counting { sink, typed: 0 };
    if !matches!(target, Target::Capture | Target::Search) {
        transcribe(speech, languages, listen, &mut sink)?;
        if target == Target::FocusedEnter && sink.typed > 0 && !alone.load(Ordering::Acquire) {
            sink.type_text("\n")?;
        }
        return Ok(None);
    }
    let mut kept = Collecting::default();
    transcribe(speech, languages, listen, &mut kept)?;
    let text = kept.text.trim().to_owned();
    if target == Target::Capture {
        // Sent even when empty, so whoever asked knows the session is over.
        return Ok(Some(Outcome::Captured { text, id }));
    }
    if !alone.load(Ordering::Acquire) {
        return Ok(Some(Outcome::Search { text }));
    }
    if !text.is_empty() {
        sink.type_text(&text)?;
    }
    Ok(None)
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
fn transcribe<A: AudioSource>(
    speech: &dyn SpeechProvider,
    languages: &[String],
    listen: impl FnOnce() -> Result<A>,
    sink: &mut impl TextSink,
) -> Result<()> {
    thread::scope(|scope| {
        let mut session = Deferred::start(scope, speech, languages);
        // Dropped on every way out of here, which lets go of the microphone.
        let mut microphone = listen()?;
        session.session(false)?;
        pipeline::run_session(&mut microphone, &mut session, sink)
    })
}

/// A recognizer session of the provider.
type Session = Box<dyn Transcriber + Send>;

/// A session that may not be there yet. Audio fed to it before it arrives is
/// kept, and handed over first and in order when it does.
struct Deferred {
    /// Where the session, or the reason there is none, arrives.
    arriving: Receiver<Result<Session>>,
    session: Option<Session>,
    /// What was heard before the session arrived.
    waiting: Vec<i16>,
    /// The most that `waiting` may hold.
    limit: usize,
    /// Whether audio had to be left out of `waiting`.
    overflowed: bool,
    /// Transcripts the session has produced that have not been passed on:
    /// handing over what was kept can produce several at once.
    ready: VecDeque<Transcript>,
}

impl Deferred {
    /// Asks `speech` for a session on a thread of its own and returns at once.
    fn start<'scope>(
        scope: &'scope Scope<'scope, '_>,
        speech: &'scope dyn SpeechProvider,
        languages: &'scope [String],
    ) -> Self {
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
        Self::awaiting(arriving, MAX_WAITING_SAMPLES)
    }

    fn awaiting(arriving: Receiver<Result<Session>>, limit: usize) -> Self {
        Self {
            arriving,
            session: None,
            waiting: Vec::new(),
            limit,
            overflowed: false,
            ready: VecDeque::new(),
        }
    }

    /// The session, if it has arrived; with `wait`, once it has. On arrival
    /// it is handed everything that was kept for it.
    fn session(&mut self, wait: bool) -> Result<Option<&mut Session>> {
        if self.session.is_none() {
            let gone = || Error::Transcription("the speech provider gave no answer".into());
            let arrived = if wait {
                self.arriving.recv().map_err(|_| gone())?
            } else {
                match self.arriving.try_recv() {
                    Ok(arrived) => arrived,
                    Err(TryRecvError::Empty) => return Ok(None),
                    Err(TryRecvError::Disconnected) => return Err(gone()),
                }
            };
            let mut session = arrived?;
            for chunk in std::mem::take(&mut self.waiting).chunks(REPLAY_SAMPLES) {
                self.ready.extend(session.feed(chunk)?);
            }
            self.session = Some(session);
        }
        Ok(self.session.as_mut())
    }

    /// Keeps audio for the session that is still to come, up to the limit.
    fn keep(&mut self, pcm: &[i16]) {
        let room = self.limit.saturating_sub(self.waiting.len());
        self.waiting.extend_from_slice(&pcm[..pcm.len().min(room)]);
        if pcm.len() > room && !self.overflowed {
            self.overflowed = true;
            log::write(
                "the speech provider is taking too long to start; what is said from here on is lost",
            );
        }
    }
}

impl Transcriber for Deferred {
    fn feed(&mut self, pcm: &[i16]) -> Result<Option<Transcript>> {
        match self.session(false)? {
            Some(session) => {
                let transcript = session.feed(pcm)?;
                self.ready.extend(transcript);
            }
            None => self.keep(pcm),
        }
        Ok(self.ready.pop_front())
    }

    /// Waits for the session if it is still not there: the microphone has
    /// stopped by now, and what was said is recognized however late.
    ///
    /// The wait has no limit of its own. It ends when the provider answers,
    /// so it is as bounded as `start_session` of every provider is, and each
    /// of them gives up on a service or an engine that does not come up.
    fn finish(&mut self, emit: &mut Emit<'_>) -> Result<()> {
        self.session(true)?;
        for transcript in std::mem::take(&mut self.ready) {
            emit(transcript)?;
        }
        match self.session.as_mut() {
            Some(session) => session.finish(emit),
            None => Ok(()),
        }
    }
}

/// Records until stopped and saves the recording as an MP3.
fn record_memo(
    options: &Options,
    stop: StopHandle,
    meter: LevelMeter,
    id: Option<String>,
    cancelled: &AtomicBool,
) -> Result<Option<Outcome>> {
    let mut microphone = listen(options, stop, meter)?;
    let mut samples = Vec::new();
    let mut chunk = [0i16; 1600];
    while samples.len() < memo::MAX_SAMPLES {
        let read = microphone.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        samples.extend_from_slice(&chunk[..read]);
    }
    // The microphone is released before the encoding starts.
    drop(microphone);
    // A memo that was called off is not saved, not even for a moment.
    if samples.len() < memo::MIN_SAMPLES || cancelled.load(Ordering::Acquire) {
        return Ok(None);
    }
    let saved = memo::save(&samples, &options.memo_dir)?;
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
    use crate::stt::ProviderKind;

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

        let result = transcribe(&provider, &[], || Ok(microphone), &mut sink);

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

        let result = transcribe(&provider, &[], || Ok(microphone), &mut sink);

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

        let result = transcribe(&provider, &[], || Ok(microphone), &mut sink);

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

        let result = transcribe(&provider, &[], || Ok(microphone), &mut Typed::default());

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

        let result = transcribe(&provider, &[], || Ok(microphone), &mut sink);

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
    fn calling_off_reaches_the_session_that_listens_and_those_still_at_work() {
        let settings = Settings::default();
        let microphone = Arc::new(ArmedMicrophone::new(&settings.microphone));
        let mut dictation = Dictation {
            speech: Arc::new(Loading::new(false, false)),
            speech_config: settings.speech_config(),
            options: Options::of(&settings, microphone, Arc::default()),
            meter: LevelMeter::new(),
            failures: Arc::default(),
            outcomes: Arc::default(),
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
        let started = std::time::Instant::now();
        transcribe(&speech, &[], || Ok(microphone), &mut sink)?;
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
        transcribe(&speech, &[], || Ok(microphone), &mut sink)?;
        assert_eq!(sink.text, "Hello world.");
        assert!(fake.wait(|seen| seen.len() == 2 && seen[1].events.contains(&"closed")));
        // That one took the connection over and left another in its turn;
        // none was made in vain.
        assert!(fake.wait_for(|state| state.connections == before + 3));
        assert_eq!(fake.read(|state| state.left_unused), 0);
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
            transcribe(&speech, &[], || Ok(microphone), &mut sink)?;
            assert_eq!(sink.sink.text, "", "{script:?}");
            // The model did take over, and what it said fell.
            assert_eq!(heard.sessions(), 1, "{script:?}");
            // Not called off, the same dictation is typed, by the model.
            let (microphone, _) = Counted::new(3, |_| {});
            let mut sink = Collecting::default();
            transcribe(&speech, &[], || Ok(microphone), &mut sink)?;
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
        Options::of(&settings, microphone, Arc::default())
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
        let left = dictate(&provider, &[], listen, (target, id), &alone, seen.clone());
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

    #[test]
    fn a_dictation_is_not_ended_for_being_quiet_while_its_key_is_held() {
        let mut dictation = Dictation {
            speech: Arc::new(Loading::new(false, false)),
            speech_config: Settings::default().speech_config(),
            options: a_minute(),
            meter: LevelMeter::new(),
            failures: Arc::default(),
            outcomes: Arc::default(),
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
}
