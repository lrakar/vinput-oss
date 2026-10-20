//! Recognition on this computer, with a model installed from Settings.
//!
//! The model runs in a separate engine process, never inside Vinput, so the
//! background app stays as small as it is and a model can be unloaded by
//! ending its process. Models with a server keep it loaded between
//! dictations and let it go after a while without use; the others start their
//! program for each dictation.
//!
//! A model with a server recognizes while the user is still speaking. The
//! dictation is cut wherever the speaker pauses ([`pauses`]), each stretch is
//! recognized on a thread of its own while the microphone goes on
//! ([`stretches`]), and its text is typed as soon as it is there, behind the
//! text of the stretches before it. The wait at the end is then for the last
//! stretch alone, however long the dictation was. A dictation without a pause
//! is one stretch and one request, and so is every dictation with an engine
//! too slow to be done with a stretch before the next one is spoken. How a
//! stretch is recognized, and in which of the configured languages, is in
//! [`dictation`]. A model whose program is started per dictation is
//! recognized whole when the dictation ends.
//!
//! There are two engines with a server. The second one runs the models that
//! come as two files, and is asked and answers in another form
//! ([`converse`]). Its models write every stretch as a sentence of its own,
//! so a dictation is not cut at its pauses for them: it is one stretch,
//! recognized when it ends, unless it goes on for minutes, when it is cut
//! at a pause every two or so ([`WHOLE_SAMPLES`]) so that no request grows
//! without end. Everything else here is the same for both. That engine
//! answers anyone who can reach its port, so it is started with a key made
//! up for that start, which every request to it carries ([`listener`]).
//!
//! The engine makes words up for audio without speech, so it is handed as
//! little of that as can be: the quiet at either end of a stretch is cut
//! away and a stretch with nothing in it is not sent ([`sound`]), what may
//! not be speech is recognized with care ([`dictation`]), and what the
//! engine is known to say when nobody spoke is not typed ([`phantoms`]).
//!
//! A loaded server is not left running while nobody dictates: on its own it
//! wakes the processor sixty to several hundred times a second to look for
//! requests, and on a graphics card it has been seen to keep a whole
//! processor core busy doing nothing. Half a minute after its last use its
//! process is suspended ([`crate::suspend`]), model and all, and costs
//! nothing but its memory; the key press that starts the next dictation
//! resumes it, in a fraction of a millisecond, and a request to an engine
//! just resumed takes as long as any other. The life of a server is
//! therefore: started (the model loads), running while in use, suspended
//! while idle ([`QUIET`]), ended after ten minutes without a dictation
//! ([`IDLE_UNLOAD`]), started again by the next one. [`Server::address`] is
//! the one door to it: whoever goes through gets a running engine, and
//! suspending and ending are decided on one thread per engine
//! ([`Server::unload_when_idle`]).

mod cards;
mod dictation;
pub(crate) mod languages;
mod pauses;
mod phantoms;
mod sound;
mod stretches;

use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::Value;

pub use self::cards::fell_back;
use self::dictation::{Care, Dictation, Inference};
use self::pauses::Pauses;
use self::phantoms::clean;
use self::stretches::{Experience, Stretches};
use super::{Emit, ProviderKind, SessionOptions, SpeechProvider, Transcriber, Transcript};
use crate::audio::{SAMPLE_RATE_HZ, wav_bytes};
use crate::models::{self, Model, Runner, hide_console};
use crate::{Error, Result, log, suspend};

/// How long a model may take to load before the engine is given up on.
const LOAD_TIMEOUT: Duration = Duration::from_secs(120);
/// How long recognizing a stretch may take, however short it is.
const RECOGNIZE_TIMEOUT: Duration = Duration::from_secs(180);
/// How long recognizing a stretch may take, however long it is: a minute
/// less than [`IDLE_UNLOAD`], after which an engine that has recorded no
/// answer is ended whatever it is doing.
const RECOGNIZE_LIMIT: Duration = Duration::from_secs(IDLE_UNLOAD.as_secs() - 60);
/// An engine that is handed its dictations whole is not handed more than
/// about this at once: two minutes, after which the dictation is cut at the
/// next pause. Held back until the key is let go, half an hour of speech
/// would be one request that no engine answers in time, and all of it would
/// be lost with that one.
const WHOLE_SAMPLES: usize = 120 * SAMPLE_RATE_HZ as usize;
/// The server is ended after this long without a dictation, to free the
/// memory the model takes.
const IDLE_UNLOAD: Duration = Duration::from_secs(10 * 60);
/// A loaded server is suspended after this long without use. Whisper takes
/// speech thirty seconds at a time, so the requests of one dictation are not
/// further apart than that and the engine stays awake between them; and a
/// dictation that follows another within half a minute finds it awake too.
/// Nothing depends on that: an engine suspended in the middle of a dictation
/// is resumed by the next request like any other, in a fraction of a
/// millisecond. Against the ten minutes it used to idle for, half a minute is
/// a twentieth.
const QUIET: Duration = Duration::from_secs(30);
/// An engine handed to a caller counts as in use for this long if no answer
/// is recorded: the five seconds a request may take to connect and the time
/// most requests may take to be answered. A long recording may take longer
/// ([`patience`]); its request is then seen as a connection of the engine,
/// which is not suspended while it has one.
const LENT: Duration = Duration::from_secs(5 + RECOGNIZE_TIMEOUT.as_secs());
/// How long the silent first request of a freshly loaded engine may take. It
/// is made while everyone else waits for the engine, so not the three minutes
/// a dictation gets.
const WARM_UP_TIMEOUT: Duration = Duration::from_secs(30);
/// Less audio than this cannot hold a word and is not sent to the engine.
const MIN_SAMPLES: usize = SAMPLE_RATE_HZ as usize / 4;
const POLL: Duration = Duration::from_millis(100);

/// How long the engine may take over a recording of this many bytes: three
/// minutes and twice as long as the recording lasts, since a large model on
/// a processor can be slower than speech, but never past
/// [`RECOGNIZE_LIMIT`].
fn patience(wav: usize) -> Duration {
    let lasts = Duration::from_secs((wav / (2 * SAMPLE_RATE_HZ as usize)) as u64);
    (RECOGNIZE_TIMEOUT + 2 * lasts).min(RECOGNIZE_LIMIT)
}

/// A local model, ready to recognize.
#[derive(Debug)]
pub struct LocalSpeech {
    engine: Engine,
    /// What the dictations so far have shown of the engine and the speaker.
    experience: Arc<Experience>,
}

#[derive(Debug, Clone)]
enum Engine {
    Server(Arc<Server>),
    Command(Arc<Program>),
}

impl LocalSpeech {
    /// Checks that the model is installed and, for a model with a server,
    /// starts loading it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transcription`] if the model is unknown or not
    /// installed, or if its engine does not start.
    pub fn connect(model_id: &str, gpu: bool) -> Result<Self> {
        let model = models::find(ProviderKind::Local, model_id).ok_or_else(|| {
            Error::Setup(format!("{model_id:?} is not a local model Vinput knows"))
        })?;
        let not_installed = || {
            Error::Setup(format!(
                "{} is not installed; open Settings, select it and press Install",
                model.name
            ))
        };
        let Some((file, runner)) = &model.local else {
            return Err(not_installed());
        };
        if !models::is_installed(model, gpu) {
            return Err(not_installed());
        }
        let program = models::usable_program(gpu, *runner).ok_or_else(not_installed)?;
        let model_file = models::model_path(file).ok_or_else(not_installed)?;
        let engine = match runner {
            // It writes, and hears nothing.
            Runner::Writer => return Err(not_installed()),
            Runner::Command => Engine::Command(Arc::new(Program {
                executable: program,
                model_file,
                gpu,
            })),
            Runner::Server | Runner::Listener(_) => {
                let server = Arc::new(Server {
                    program,
                    model_file,
                    ears: runner.second().and_then(models::model_path),
                    name: model.name,
                    gpu,
                    running: Mutex::default(),
                });
                // Loading takes seconds; doing it now keeps it off the first dictation.
                server.wake()?;
                Engine::Server(server)
            }
        };
        Ok(Self {
            engine,
            experience: Arc::default(),
        })
    }

    /// The catalog entry of a model id, for callers that only need to know
    /// whether it is one of the local ones.
    #[must_use]
    pub fn model(model_id: &str) -> Option<&'static Model> {
        models::find(ProviderKind::Local, model_id)
    }
}

impl SpeechProvider for LocalSpeech {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Local
    }

    fn start_session(&self, options: SessionOptions<'_>) -> Result<Box<dyn Transcriber + Send>> {
        if let Engine::Server(server) = &self.engine {
            // If the server was let go while idle, it reloads while the user speaks.
            let warming = Arc::clone(server);
            let _ = thread::Builder::new()
                .name("vinput-local-load".into())
                .spawn(move || {
                    if let Err(err) = warming.wake() {
                        log::write(&format!("local model not ready: {err}"));
                    }
                });
        }
        let languages = codes_of(options.languages);
        Ok(Box::new(match &self.engine {
            Engine::Server(server) => LocalSession::at_pauses(
                Arc::new(Arc::clone(server)),
                languages,
                Arc::clone(&self.experience),
            )?,
            Engine::Command(program) => LocalSession::AtEnd {
                program: Arc::clone(program),
                samples: Vec::new(),
            },
        }))
    }
}

/// The configured languages as the two-letter codes the engine knows.
fn codes_of(languages: &[String]) -> Vec<String> {
    languages
        .iter()
        .filter_map(|language| language.split(['-', '_']).next())
        .filter(|code| !code.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

/// One dictation on its way to a local model.
enum LocalSession {
    /// With an engine server: recognized a stretch at a time, at the pauses.
    AtPauses {
        pauses: Pauses,
        stretches: Stretches,
        experience: Arc<Experience>,
        /// Whether the dictation is one stretch whatever its pauses, as for
        /// an engine that cannot carry a sentence across a cut, for as long
        /// as that stretch is not too long for one request.
        whole: bool,
    },
    /// With a program that is run once: recognized when the dictation ends.
    AtEnd {
        program: Arc<Program>,
        samples: Vec<i16>,
    },
}

impl LocalSession {
    /// A session that recognizes with `engine` while the user speaks.
    fn at_pauses(
        engine: Arc<dyn Inference>,
        languages: Vec<String>,
        experience: Arc<Experience>,
    ) -> Result<Self> {
        let whole = !engine.led();
        let dictation = Dictation::new(engine, languages, experience.language())
            .listening_for(&crate::words::listened_for());
        Ok(Self::AtPauses {
            pauses: Pauses::new(),
            stretches: Stretches::start(dictation, Arc::clone(&experience))?,
            experience,
            whole,
        })
    }
}

/// A piece of text that is ready to be typed, if there is anything in it.
fn typed(text: String) -> Option<Transcript> {
    (!text.is_empty()).then_some(Transcript {
        text,
        is_final: true,
    })
}

impl Transcriber for LocalSession {
    fn feed(&mut self, pcm: &[i16]) -> Result<Option<Transcript>> {
        match self {
            Self::AtPauses {
                pauses,
                stretches,
                experience,
                whole,
            } => {
                let ready = stretches.ready()?;
                // A stretch is cut off only when the engine is free to take
                // it and quick enough to be done with it soon, so that
                // nothing is ever waiting behind it when the dictation ends.
                // An engine that gets its dictations whole gets a stretch
                // only once it is as long as one request may be.
                let may_cut = if *whole {
                    pauses.held() >= WHOLE_SAMPLES
                } else {
                    stretches.idle() && experience.keeps_up()
                };
                if let Some(stretch) = pauses.feed(pcm, may_cut) {
                    send(stretches, &stretch, Care::default(), experience.voice());
                }
                Ok(ready.and_then(typed))
            }
            Self::AtEnd { samples, .. } => {
                samples.extend_from_slice(pcm);
                Ok(None)
            }
        }
    }

    fn finish(&mut self, emit: &mut Emit<'_>) -> Result<()> {
        match self {
            Self::AtPauses {
                pauses,
                stretches,
                experience,
                ..
            } => {
                if let Some(rest) = pauses.rest()
                    && rest.samples.len() >= MIN_SAMPLES
                {
                    let care = Care {
                        doubtful: rest.faint,
                        quiet: rest.quiet,
                    };
                    send(stretches, &rest.samples, care, experience.voice());
                }
                while let Some(text) = stretches.wait()? {
                    if let Some(transcript) = typed(text) {
                        emit(transcript)?;
                    }
                }
                Ok(())
            }
            Self::AtEnd { program, samples } => {
                let samples = std::mem::take(samples);
                if samples.len() < MIN_SAMPLES {
                    return Ok(());
                }
                // Such a program cannot be asked whether it heard speech,
                // so it is only spared what has no sound in it.
                let Some(sound) = sound::listen(&samples) else {
                    return Ok(());
                };
                let heard = samples.get(sound.heard).unwrap_or_default();
                match typed(clean(&program.recognize(heard)?)) {
                    Some(transcript) => emit(transcript),
                    None => Ok(()),
                }
            }
        }
    }
}

/// Queues what there is to hear in `stretch` for recognition: nothing if
/// it is silent, and otherwise the part with sound in it, with the care
/// that `care` and the sound itself call for. `voice` is the level the user
/// was heard to speak at in the dictations before, if there were any.
fn send(stretches: &mut Stretches, stretch: &[i16], care: Care, voice: Option<f32>) {
    let Some(sound) = sound::listen(stretch) else {
        return;
    };
    let far_under = voice.is_some_and(|voice| sound.far_under(voice));
    let care = Care {
        doubtful: care.doubtful || sound.doubtful || far_under,
        quiet: care.quiet || sound.hushed || far_under,
    };
    let heard = stretch.get(sound.heard.clone()).unwrap_or_default();
    stretches.send(heard.to_vec(), care, sound.loudest);
}

/// A model run by a program that is started for each dictation.
#[derive(Debug)]
struct Program {
    executable: PathBuf,
    model_file: PathBuf,
    gpu: bool,
}

impl Program {
    fn recognize(&self, samples: &[i16]) -> Result<String> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let failed =
            |what: String| Error::Transcription(format!("the local engine failed: {what}"));
        let recording = std::env::temp_dir().join(format!(
            "vinput-{}-{}.wav",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&recording, wav_bytes(samples)).map_err(|err| failed(err.to_string()))?;
        let mut command = Command::new(&self.executable);
        command
            .arg("-m")
            .arg(&self.model_file)
            .arg("-f")
            .arg(&recording)
            .arg("-t")
            .arg(threads().to_string())
            .stdin(Stdio::null());
        if self.gpu {
            command
                .arg("-dev")
                .arg(cards::number(&self.executable, false).to_string());
        } else {
            command.arg("-ng");
        }
        engine_folder(&mut command, &self.executable);
        hide_console(&mut command);
        let output = command.output();
        // The recording is the user's voice; it does not stay on disk.
        let _ = std::fs::remove_file(&recording);
        let output = output.map_err(|err| failed(err.to_string()))?;
        if !output.status.success() {
            let reason = String::from_utf8_lossy(&output.stderr);
            let last = reason.lines().rev().find(|line| !line.trim().is_empty());
            return Err(failed(
                last.unwrap_or("it stopped without saying why")
                    .trim()
                    .to_owned(),
            ));
        }
        Ok(spoken_lines(&String::from_utf8_lossy(&output.stdout)))
    }
}

/// The transcript in a program's output, without its progress lines.
fn spoken_lines(output: &str) -> String {
    output
        .lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty()
                && !line.starts_with("Processing audio")
                && !line.starts_with("parakeet_")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A model kept loaded by a server on a loopback port.
#[derive(Debug)]
struct Server {
    program: PathBuf,
    model_file: PathBuf,
    /// The file that hears, of a model that comes as two; with it, the
    /// program is the second engine.
    ears: Option<PathBuf>,
    name: &'static str,
    gpu: bool,
    running: Mutex<Option<Running>>,
}

struct Running {
    child: Child,
    address: SocketAddr,
    /// What the second engine asks of whoever makes a request of it; empty
    /// for the first, which asks for nothing.
    key: String,
    /// When the engine was last handed out or last answered.
    last_used: Instant,
    /// When [`Server::address`] last handed the engine to a caller. It is in
    /// use from then until an answer moves `last_used` on, or [`LENT`] passes.
    lent: Instant,
    /// The engine's threads, while it is suspended.
    asleep: Option<suspend::Suspended>,
    /// Whether the engine may be suspended at all: not once that has failed,
    /// and not unless Windows ends it together with Vinput, because a
    /// suspended engine left behind by a crash would hold its memory with
    /// nothing to show for it.
    may_sleep: bool,
    /// Wakes the thread that suspends and ends this engine, and tells it
    /// when the engine is gone.
    watcher: Arc<Bell>,
    #[cfg(windows)]
    _guard: Option<crate::job::KillOnDrop>,
}

/// What an engine and the thread looking after it share: a way to wake the
/// thread before its time, and to tell it that its engine is gone.
#[derive(Debug, Default)]
struct Bell {
    state: Mutex<(Rung, Gone)>,
    changed: Condvar,
}

type Rung = bool;
type Gone = bool;

impl Bell {
    fn set(&self, rung: Rung, gone: Gone) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.0 |= rung;
        state.1 |= gone;
        self.changed.notify_all();
    }

    fn gone(&self) -> bool {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).1
    }

    /// Waits until rung, but no longer than `wait`. `false` if the engine is
    /// gone.
    fn wait(&self, wait: Duration) -> bool {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let (mut state, _) = self
            .changed
            .wait_timeout_while(state, wait, |(rung, gone)| !*rung && !*gone)
            .unwrap_or_else(PoisonError::into_inner);
        state.0 = false;
        !state.1
    }
}

/// The waits in an engine's life; shorter in tests.
#[derive(Debug, Clone, Copy)]
struct Pace {
    quiet: Duration,
    lent: Duration,
    unload: Duration,
}

const PACE: Pace = Pace {
    quiet: QUIET,
    lent: LENT,
    unload: IDLE_UNLOAD,
};

/// What the thread watching an engine does next.
#[derive(Debug, PartialEq)]
enum Next {
    /// Look again after this long, or when woken.
    Wait(Duration),
    /// End the engine.
    Unload,
}

impl Running {
    /// Makes a suspended engine run again; nothing to do if it is running.
    /// `false` if it could not be resumed, after which it is of no use.
    fn wake(&mut self) -> bool {
        let Some(asleep) = self.asleep.take() else {
            return true;
        };
        if let Err(err) = asleep.resume() {
            log::write(&format!("the local engine could not be resumed: {err}"));
            return false;
        }
        // Whoever watches it is waiting for the day it is ended.
        self.watcher.set(true, false);
        true
    }

    /// Suspends the engine. If that fails it stays running, as it always did
    /// before engines were suspended, and is not tried again. Finding the
    /// threads takes Windows a tenth of a second or two on a busy PC, which
    /// is how long a key press at that very moment waits for its engine.
    fn sleep(&mut self) {
        match suspend::suspend(&self.child) {
            Ok(asleep) => self.asleep = Some(asleep),
            Err(err) => {
                log::write(&format!(
                    "the local engine could not be suspended and stays running: {err}"
                ));
                self.may_sleep = false;
            }
        }
    }

    /// Records a use of the engine now: handing it to a caller who will
    /// make a request, or only having it ready. Having it ready takes nothing
    /// away from a caller it is still handed out to.
    fn touch(&mut self, lend: bool) {
        let now = Instant::now();
        if lend {
            self.lent = now;
            self.last_used = now;
        } else if self.last_used > self.lent {
            self.last_used = now;
        }
    }

    /// From when the engine may be suspended, as things stand.
    fn sleep_due(&self, pace: Pace) -> Instant {
        if self.last_used > self.lent {
            self.last_used + pace.quiet
        } else {
            // Handed out and not heard of since: a request may be under way.
            self.lent + pace.lent + pace.quiet
        }
    }

    /// One look at the engine by the thread that suspends and ends it.
    /// `answering` tells whether the process of that number has a connection
    /// open, if that can be told.
    fn look(
        &mut self,
        now: Instant,
        pace: Pace,
        answering: impl FnOnce(u32) -> Option<bool>,
    ) -> Next {
        let idle = now.saturating_duration_since(self.last_used);
        if idle >= pace.unload {
            return Next::Unload;
        }
        let until_unload = pace.unload.saturating_sub(idle);
        if self.asleep.is_some() || !self.may_sleep {
            return Next::Wait(until_unload);
        }
        let due = self.sleep_due(pace);
        if now < due {
            // An answer recorded meanwhile brings the moment forward and
            // wakes nobody, so the wait is never longer than `quiet`.
            let wait = due.saturating_duration_since(now);
            return Next::Wait(wait.min(pace.quiet).min(until_unload));
        }
        // The record of use says the engine is free. The engine itself has
        // the last word, so that no request is ever cut off by a suspension:
        // one that outlasts the record, or one of two that overlap, shows as
        // a connection, since each request has its own for as long as it
        // lasts. No new one can begin meanwhile: that takes the lock this
        // look is made under. If Windows cannot say, the engine is left
        // running: that costs a little work, the other mistake a dictation.
        if answering(self.child.id()) != Some(false) {
            return Next::Wait(pace.quiet.min(until_unload));
        }
        self.sleep();
        Next::Wait(until_unload)
    }
}

impl std::fmt::Debug for Running {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Running")
            .field("address", &self.address)
            .finish_non_exhaustive()
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        // A suspended process is ended like a running one. One that could
        // not be ended is not waited for, which would never end either: it
        // is left running, for the job to end with Vinput.
        if self.child.kill().is_ok() {
            let _ = self.child.wait();
        } else {
            log::write("the local engine could not be ended");
            self.asleep = None;
        }
        self.watcher.set(false, true);
    }
}

impl Running {
    /// A process just started as the engine at `address`, with the bell
    /// for the thread that is to look after it.
    fn of(child: Child, address: SocketAddr, key: String) -> (Self, Arc<Bell>) {
        let watcher = Arc::new(Bell::default());
        let watching = Arc::clone(&watcher);
        #[cfg(windows)]
        let guard = crate::job::KillOnDrop::attach(&child);
        #[cfg(windows)]
        let tied = guard.is_some();
        #[cfg(not(windows))]
        let tied = false;
        let now = Instant::now();
        let engine = Self {
            child,
            address,
            key,
            last_used: now,
            lent: now,
            asleep: None,
            may_sleep: tied,
            watcher,
            #[cfg(windows)]
            _guard: guard,
        };
        (engine, watching)
    }
}

impl Server {
    /// The address of the running server and the key it asks for, starting
    /// it first if it is not loaded and resuming it first if it is
    /// suspended. Blocks while the model loads.
    ///
    /// The engine is running when this returns, and stays so for as long as
    /// the caller may take over a request ([`LENT`]) or until the answer is
    /// recorded in `last_used`, whichever comes first. A suspended process
    /// still has its listening socket: connecting to it succeeds and the
    /// answer then never comes. So nobody may connect to an address obtained
    /// in any other way, and both resuming here and suspending in
    /// [`Server::unload_when_idle`] happen under the lock on `running`.
    fn address(self: &Arc<Self>) -> Result<(SocketAddr, String)> {
        self.reach(true)
    }

    /// Has the engine loaded and running ahead of a request that may or may
    /// not follow, as at a key press: started or resumed like
    /// [`Server::address`], but not handed out, so that a press nothing
    /// comes of lets it be suspended [`QUIET`] later and not [`LENT`] later.
    /// A request that does follow goes through `address` like any other.
    fn wake(self: &Arc<Self>) -> Result<()> {
        self.reach(false).map(drop)
    }

    /// The engine, running; handed out to the caller if `lend`.
    fn reach(self: &Arc<Self>, lend: bool) -> Result<(SocketAddr, String)> {
        let mut running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(server) = running.as_mut() {
            if !matches!(server.child.try_wait(), Ok(None)) {
                log::write("the local engine had stopped; starting it again");
            } else if server.wake() {
                server.touch(lend);
                return Ok((server.address, server.key.clone()));
            } else {
                log::write("starting the local engine again instead");
            }
        }
        // Ends the engine there was, suspended or not.
        *running = None;
        let (mut server, watcher) = self.start()?;
        server.touch(lend);
        let door = (server.address, server.key.clone());
        *running = Some(server);
        drop(running);
        self.unload_when_idle(watcher, PACE);
        Ok(door)
    }

    /// Starts the engine and waits for the model to load. Returns it with
    /// the bell for the thread that is to look after it.
    fn start(&self) -> Result<(Running, Arc<Bell>)> {
        let failed = |what: String| {
            Error::Transcription(format!(
                "starting the engine for {} failed: {what}",
                self.name
            ))
        };
        // Ask the system for a free port, then hand it to the server.
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .and_then(|listener| listener.local_addr())
            .map_err(|err| failed(err.to_string()))?
            .port();
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let mut command = Command::new(&self.program);
        command
            .arg("-m")
            .arg(&self.model_file)
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg(port.to_string())
            .arg("-t")
            .arg(threads().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut key = String::new();
        if let Some(ears) = &self.ears {
            key = secret().map_err(|err| failed(err.to_string()))?;
            let set = std::env::vars_os().map(|(name, _)| name);
            let card = self.gpu.then(|| cards::number(&self.program, true));
            listener(&mut command, ears, card, &key, set);
        } else if self.gpu {
            // Which card, not whichever comes first: see `cards`.
            command
                .arg("-dev")
                .arg(cards::number(&self.program, false).to_string());
        } else {
            // The build at hand may be one for graphics cards while the user
            // wants the processor.
            command.arg("-ng");
        }
        // The first engine says, as it loads the model, whether it uses the
        // card it was told; that is listened for, and nothing else is.
        let heeded = self.gpu && self.ears.is_none();
        if heeded {
            command.stderr(Stdio::piped());
        }
        engine_folder(&mut command, &self.program);
        hide_console(&mut command);
        let mut child = command.spawn().map_err(|err| failed(err.to_string()))?;
        if let Some(printed) = child.stderr.take().filter(|_| heeded) {
            cards::heed(printed, &self.program);
        }
        let (mut server, watcher) = Running::of(child, address, key);
        // The server listens once the model is loaded.
        let started = Instant::now();
        loop {
            if let Ok(Some(status)) = server.child.try_wait() {
                return Err(failed(format!(
                    "it stopped while loading the model ({status})"
                )));
            }
            if self.loaded(address, &server.key) {
                if self.gpu {
                    let left = LOAD_TIMEOUT.saturating_sub(started.elapsed());
                    self.warm_up(address, &server.key, left.clamp(POLL, WARM_UP_TIMEOUT));
                }
                // Used as of now, not as of before the model loaded, and
                // not handed out to anyone yet.
                server.last_used = Instant::now();
                return Ok((server, watcher));
            }
            if started.elapsed() > LOAD_TIMEOUT {
                return Err(failed("loading the model took too long".into()));
            }
            thread::sleep(POLL);
        }
    }

    /// Looks after the engine just started while it is not in use: suspends
    /// it once it has gone unused for [`QUIET`] and ends it once it has gone
    /// unused for [`IDLE_UNLOAD`].
    ///
    /// The thread sleeps until the next of those is due, so it costs nothing
    /// in between: once the engine is suspended, that is the day it is ended.
    /// `watcher` is the engine's bell. It is rung when the engine was resumed
    /// and is to be looked at again, and it says when the engine is gone, by
    /// being replaced or with the whole server, which ends the thread.
    fn unload_when_idle(self: &Arc<Self>, watcher: Arc<Bell>, pace: Pace) {
        let server = Arc::downgrade(self);
        let spawned = thread::Builder::new()
            .name("vinput-local-idle".into())
            .spawn(move || {
                loop {
                    let wait = {
                        let Some(server) = server.upgrade() else {
                            return;
                        };
                        let mut running = server
                            .running
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner);
                        // Engines are dropped under this lock, so unless
                        // the bell says gone the engine here is this thread's.
                        if watcher.gone() {
                            return;
                        }
                        let Some(engine) = running.as_mut() else {
                            return;
                        };
                        match engine.look(Instant::now(), pace, suspend::has_connection) {
                            Next::Wait(wait) => wait,
                            Next::Unload => {
                                *running = None;
                                return;
                            }
                        }
                    };
                    if !watcher.wait(wait) {
                        return;
                    }
                }
            });
        if let Err(err) = spawned {
            log::write(&format!(
                "the local engine stays running and loaded while idle: {err}"
            ));
        }
    }

    /// Whether the engine listening at `address` has its model loaded. The
    /// first engine listens once it has; the second one from the start, and
    /// says how it is when asked. Neither is waited for longer than [`POLL`]
    /// while nothing listens there yet: Windows takes a second or two to say
    /// that nothing does.
    fn loaded(&self, address: SocketAddr, key: &str) -> bool {
        let health = || exchange(address, "GET /health", &bearer(key), &[], WARM_UP_TIMEOUT);
        TcpStream::connect_timeout(&address, POLL).is_ok()
            && (self.ears.is_none() || matches!(health(), Ok((200, _))))
    }

    /// Has a freshly loaded engine recognize half a second of silence, so
    /// that what it only sets up for its first request is set up before a
    /// dictation asks. On a graphics card that is the programs the card
    /// runs, which the driver builds the first time they are used: seconds,
    /// the first time ever. On the processor a first request was measured to
    /// take as long as any other, so there this would only make loading
    /// longer and is not done.
    ///
    /// Part of loading: whoever waits for the engine waits for this too, and
    /// the engine is not yet in use while it lasts, for `patience` at the
    /// most. A failure costs nothing but the head start.
    fn warm_up(&self, address: SocketAddr, key: &str, patience: Duration) {
        let silence = wav_bytes(&[0; SAMPLE_RATE_HZ as usize / 2]);
        let fields = [
            ("response_format", "json"),
            ("temperature", "0.0"),
            ("language", "en"),
        ];
        if let Err(err) = self.ask((address, key), &fields, &silence, patience) {
            log::write(&format!("warming the local engine up failed: {err}"));
        }
    }

    /// One request to the engine at `address`, in the form it understands
    /// and with the `key` it asks for.
    fn ask(
        &self,
        (address, key): (SocketAddr, &str),
        fields: &[(&str, &str)],
        wav: &[u8],
        patience: Duration,
    ) -> Result<Value> {
        if self.ears.is_some() {
            converse((address, key), fields, wav, patience)
        } else {
            ask_within(address, fields, wav, patience)
        }
    }

    /// One request to the engine; its answer, which has the `text`.
    fn infer(self: &Arc<Self>, fields: &[(&str, &str)], wav: &[u8]) -> Result<Value> {
        let (address, key) = self.address()?;
        let reply = self.ask((address, &key), fields, wav, patience(wav.len()))?;
        if let Some(server) = self
            .running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
        {
            server.last_used = Instant::now();
        }
        Ok(reply)
    }
}

impl Inference for Arc<Server> {
    fn infer(&self, fields: &[(&str, &str)], wav: &[u8]) -> Result<Value> {
        Server::infer(self, fields, wav)
    }

    fn led(&self) -> bool {
        self.ears.is_none()
    }
}

/// Posts one recognition request to the engine at `address` and returns its
/// answer, which has the `text`.
#[cfg(test)]
fn ask(address: SocketAddr, fields: &[(&str, &str)], wav: &[u8]) -> Result<Value> {
    ask_within(address, fields, wav, RECOGNIZE_TIMEOUT)
}

fn failed(what: &dyn std::fmt::Display) -> Error {
    Error::Transcription(format!("the local engine failed: {what}"))
}

fn unexpected(status: u16) -> Error {
    failed(&format_args!("unexpected answer (HTTP {status})"))
}

/// [`ask`], giving up when the engine sends nothing for `patience`.
fn ask_within(
    address: SocketAddr,
    fields: &[(&str, &str)],
    wav: &[u8],
    patience: Duration,
) -> Result<Value> {
    let kind = format!("Content-Type: multipart/form-data; boundary={BOUNDARY}\r\n");
    let form = multipart(fields, wav);
    let (status, reply) =
        exchange(address, "POST /inference", &kind, &form, patience).map_err(|err| failed(&err))?;
    let reply: Value = serde_json::from_slice(&reply).unwrap_or_default();
    let error = reply.get("error").and_then(Value::as_str);
    match (status, reply.get("text").is_some(), error) {
        (200, true, _) => Ok(reply),
        (_, _, Some(error)) => Err(failed(&error)),
        _ => Err(unexpected(status)),
    }
}

/// What a model of the second engine writes between the language it heard
/// and what was said: `language English<asr_text>Hello.`
const SAID: &str = "<asr_text>";

/// One recognition request to the second engine at `address`, which asks
/// for `key`, made of the same `fields` as one to the first, and its answer
/// in the form of the first engine's: the `text`, and the `language` by its
/// name.
///
/// This engine is a language model that is shown the recording and writes
/// down what it hears. Its answer starts with the language. When the
/// language is known the answer is started for it, and it goes on in that
/// language. The text so far is not handed to it: it is taught to take such
/// text for words to listen for, not for the sentence to carry on. Words to
/// listen for are what the field `context` holds, the user's dictionary,
/// and they go where the model was taught to find them: in a message of
/// the system, before the recording.
fn converse(
    (address, key): (SocketAddr, &str),
    fields: &[(&str, &str)],
    wav: &[u8],
    patience: Duration,
) -> Result<Value> {
    let language = fields.iter().find(|(name, _)| *name == "language");
    let begun = language
        .and_then(|(_, code)| languages::taught(code))
        .map(|name| format!(r#",{{"role":"assistant","content":"language {name}{SAID}"}}"#))
        .unwrap_or_default();
    let listened_for = fields
        .iter()
        .find(|(name, _)| *name == "context")
        .map(|(_, words)| format!(r#"{{"role":"system","content":{}}},"#, Value::from(*words)))
        .unwrap_or_default();
    // A model that loses its way repeats itself for as long as it may, so
    // it may write no more than fits in the recording at any pace of speech.
    let most = 64 + 16 * wav.len() / (2 * SAMPLE_RATE_HZ as usize);
    let request = format!(
        r#"{{"messages":[{listened_for}{{"role":"user","content":[{{"type":"input_audio","input_audio":{{"data":"{}","format":"wav"}}}}]}}{begun}],"temperature":0,"max_tokens":{most}}}"#,
        BASE64.encode(wav)
    );
    let (status, reply) = exchange(
        address,
        "POST /v1/chat/completions",
        &format!("Content-Type: application/json\r\n{}", bearer(key)),
        request.as_bytes(),
        patience,
    )
    .map_err(|err| failed(&err))?;
    let mut reply: Value = serde_json::from_slice(&reply).unwrap_or_default();
    let written = reply["choices"][0]["message"]["content"].as_str();
    let Some(written) = written.filter(|_| status == 200) else {
        return Err(match reply["error"]["message"].as_str() {
            Some(error) => failed(&error),
            None => unexpected(status),
        });
    };
    let (language, text) = written.split_once(SAID).unwrap_or(("", written));
    let language = language
        .trim_start_matches("language ")
        .to_ascii_lowercase();
    let text = text.to_owned();
    reply["text"] = text.into();
    reply["language"] = language.into();
    Ok(reply)
}

/// The line of a request that shows the second engine its `key`.
fn bearer(key: &str) -> String {
    format!("Authorization: Bearer {key}\r\n")
}

/// A key for one start of the second engine: 128 bits nobody can guess.
fn secret() -> std::io::Result<String> {
    let mut bytes = [0u8; 16];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| std::io::Error::other("Windows gave no random numbers"))?;
    Ok(BASE64.encode(bytes))
}

/// What the second engine is started with besides its model and its port.
///
/// One dictation at a time, no page for a browser, and one graphics card,
/// the one with the number `card`, or none: left to itself this engine
/// spreads a model over every card it finds.
///
/// It takes its settings from the environment as well as from its command
/// line, under names that start with `LLAMA_`. Of the variables `set` for
/// Vinput it is handed none of those: they are the user's, for a llama.cpp
/// of their own, and one of them, a key, would have this engine refuse
/// every dictation. The one it gets is the `key` of this start. It listens
/// on this PC only, but there any program can reach its port, and so can a
/// page in a browser: without the key none of them is answered.
fn listener(
    command: &mut Command,
    ears: &Path,
    card: Option<u32>,
    key: &str,
    set: impl Iterator<Item = OsString>,
) {
    let device = card.map_or_else(|| "none".to_owned(), |number| format!("Vulkan{number}"));
    command
        .arg("--mmproj")
        .arg(ears)
        .args(["--no-webui", "-np", "1", "--device", &device]);
    // Windows does not tell the names of variables apart by their case.
    let theirs = |name: &OsString| {
        name.to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("LLAMA_")
    };
    for name in set.filter(theirs) {
        command.env_remove(name);
    }
    command.env("LLAMA_API_KEY", key);
}

/// Runs an engine program from its own folder, where its libraries are.
fn engine_folder(command: &mut Command, program: &Path) {
    if let Some(folder) = program.parent() {
        command.current_dir(folder);
    }
}

fn threads() -> usize {
    thread::available_parallelism().map_or(4, |count| count.get().clamp(1, 8))
}

const BOUNDARY: &str = "vinput-audio-boundary-7f3a9c";

/// A form with text fields and the recording as its `file`.
fn multipart(fields: &[(&str, &str)], wav: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(wav.len() + 512);
    for (name, value) in fields {
        let _ = write!(
            body,
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
        );
    }
    let _ = write!(
        body,
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\n\
         Content-Type: audio/wav\r\n\r\n"
    );
    body.extend_from_slice(wav);
    let _ = write!(body, "\r\n--{BOUNDARY}--\r\n");
    body
}

/// Makes `request` (`POST /inference`) of the engine with the lines `head`,
/// which say what kind of body it has and show a key, and returns the
/// status code and body of the answer, giving up when the engine sends
/// nothing for `patience`. The request is local and one-shot, so it is
/// written by hand on a plain socket.
fn exchange(
    address: SocketAddr,
    request: &str,
    head: &str,
    body: &[u8],
    patience: Duration,
) -> std::io::Result<(u16, Vec<u8>)> {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(patience))?;
    write!(
        stream,
        "{request} HTTP/1.1\r\nHost: {address}\r\n{head}\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply)?;
    let invalid = || std::io::Error::new(std::io::ErrorKind::InvalidData, "not an HTTP reply");
    let split = reply
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(invalid)?;
    let head = String::from_utf8_lossy(&reply[..split]).to_ascii_lowercase();
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or_else(invalid)?;
    let content = &reply[split + 4..];
    let body = if head.contains("transfer-encoding: chunked") {
        unchunk(content)
    } else {
        content.to_vec()
    };
    Ok((status, body))
}

/// Joins the pieces of a chunked HTTP body.
fn unchunk(mut content: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(content.len());
    while let Some(line_end) = content.windows(2).position(|window| window == b"\r\n") {
        let size = std::str::from_utf8(&content[..line_end])
            .ok()
            .and_then(|line| usize::from_str_radix(line.split(';').next()?.trim(), 16).ok());
        let Some(size) = size.filter(|size| *size > 0) else {
            break;
        };
        let start = line_end + 2;
        let Some(chunk) = content.get(start..start + size) else {
            break;
        };
        body.extend_from_slice(chunk);
        content = content.get(start + size + 2..).unwrap_or_default();
    }
    body
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::pauses::testing::{PACKET, room, voice};
    use super::*;

    /// What a test that talks to a stand-in engine returns.
    type Outcome = std::result::Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn progress_lines_are_not_part_of_the_transcript() {
        let output = "Processing audio (176000 samples, 11.00 seconds)\n\
                      parakeet_decode: starting decode with n_frames=138\n\
                      And so, my fellow Americans.\n";
        assert_eq!(spoken_lines(output), "And so, my fellow Americans.");
    }

    #[test]
    fn chunked_bodies_are_joined() {
        assert_eq!(
            unchunk(b"4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n"),
            b"Wikipedia"
        );
        assert_eq!(unchunk(b""), b"");
    }

    #[test]
    fn an_unknown_or_missing_model_says_what_to_do() {
        let unknown = LocalSpeech::connect("banana", false);
        assert!(matches!(&unknown, Err(Error::Setup(why)) if why.contains("banana")));
        assert!(LocalSpeech::model("whisper-base").is_some());
    }

    /// A stand-in for the engine's server: gives one of `answers` to each
    /// request, after `delay`, and returns the requests it was sent. It is
    /// gone after the last answer, so a request too many fails.
    fn fake_engine(
        answers: Vec<String>,
        delay: Duration,
    ) -> std::io::Result<(SocketAddr, thread::JoinHandle<Vec<Vec<u8>>>)> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let address = listener.local_addr()?;
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            for answer in answers {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut request = Vec::new();
                let mut chunk = [0u8; 4096];
                while !whole(&request) {
                    match stream.read(&mut chunk) {
                        Ok(read) if read > 0 => request.extend_from_slice(&chunk[..read]),
                        _ => break,
                    }
                }
                thread::sleep(delay);
                let _ = stream.write_all(answer.as_bytes());
                requests.push(request);
            }
            requests
        });
        Ok((address, server))
    }

    /// Whether all of a request has arrived: its head, and a body as long
    /// as the head says.
    fn whole(request: &[u8]) -> bool {
        let Some(head) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
            return false;
        };
        let length = String::from_utf8_lossy(&request[..head])
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length: ")?.parse().ok());
        request.len() - (head + 4) >= length.unwrap_or(0)
    }

    /// What the engine sends back for a recording it heard `text` in.
    fn heard(text: &str) -> String {
        answer("200 OK", &serde_json::json!({ "text": text }).to_string())
    }

    fn answer(status: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    /// The engine at an address, without a process behind it.
    struct At(SocketAddr);

    impl Inference for At {
        fn infer(&self, fields: &[(&str, &str)], wav: &[u8]) -> Result<Value> {
            ask(self.0, fields, wav)
        }
    }

    /// A session in English with the engine at `address`.
    fn session(address: SocketAddr, experience: Arc<Experience>) -> Result<LocalSession> {
        LocalSession::at_pauses(Arc::new(At(address)), vec!["en".into()], experience)
    }

    /// Feeds `audio` the way the microphone does, all at once, and returns
    /// what was ready to type meanwhile.
    fn speak(session: &mut LocalSession, audio: &[i16]) -> Result<Vec<String>> {
        let mut typed = Vec::new();
        for packet in audio.chunks(PACKET) {
            typed.extend(session.feed(packet)?.map(|transcript| transcript.text));
        }
        Ok(typed)
    }

    /// Lets the room be heard until the engine has answered, as long as it
    /// takes within reason, and returns the answer.
    fn wait_for_text(session: &mut LocalSession) -> Result<Option<String>> {
        for _ in 0..2000 {
            if let Some(transcript) = session.feed(&room(10))? {
                return Ok(Some(transcript.text));
            }
            thread::sleep(Duration::from_millis(2));
        }
        Ok(None)
    }

    /// Ends the dictation and returns what was typed from then on.
    fn finish(session: &mut LocalSession) -> Result<Vec<String>> {
        let mut typed = Vec::new();
        session.finish(&mut |transcript| {
            typed.push(transcript.text);
            Ok(())
        })?;
        Ok(typed)
    }

    /// The value of the form field `name` in a request.
    fn field(request: &[u8], name: &str) -> Option<String> {
        let text = String::from_utf8_lossy(request);
        let start = format!("name=\"{name}\"\r\n\r\n");
        let value = &text[text.find(&start)? + start.len()..];
        Some(value[..value.find("\r\n")?].to_owned())
    }

    #[test]
    fn a_recording_is_posted_as_a_form_and_the_text_read_back() -> Outcome {
        let (address, engine) = fake_engine(vec![heard(" Hello there.\n")], Duration::ZERO)?;
        let wav = wav_bytes(&[1, 2, 3]);
        let reply = ask(address, &[("language", "sl")], &wav)?;
        assert_eq!(reply["text"], " Hello there.\n");

        let requests = engine.join().unwrap_or_default();
        let request = requests.first().map_or(&[][..], Vec::as_slice);
        assert!(request.starts_with(b"POST /inference HTTP/1.1\r\n"));
        assert_eq!(field(request, "language").as_deref(), Some("sl"));
        assert!(String::from_utf8_lossy(request).contains("name=\"file\"; filename=\"audio.wav\""));
        assert!(request.windows(wav.len()).any(|window| window == wav));
        Ok(())
    }

    #[test]
    fn the_second_engine_is_asked_in_its_form_and_answers_in_the_first_ones() -> Outcome {
        let written = |content: &str| {
            let body = json!({"choices": [{"message": {"content": content}}]});
            answer("200 OK", &body.to_string())
        };
        let answers = vec![
            written("language English<asr_text>Hello there."),
            written("language None<asr_text>"),
            answer(
                "500 Internal Server Error",
                r#"{"error":{"message":"no room"}}"#,
            ),
            written("language English<asr_text>VinPUT."),
        ];
        let (address, engine) = fake_engine(answers, Duration::ZERO)?;
        let wav = wav_bytes(&[1, 2, 3]);
        let ask = |language| {
            let door = (address, "Zm9v");
            converse(door, &[("language", language)], &wav, RECOGNIZE_TIMEOUT)
        };

        // A language the model was taught is the one it answers in.
        let reply = ask("en")?;
        assert_eq!(reply["text"], "Hello there.");
        assert_eq!(reply["language"], "english");
        // One it was not taught is left to it, and silence is no text.
        let reply = ask("sl")?;
        assert_eq!(reply["text"], "");
        assert!(languages::code_of(reply["language"].as_str().unwrap_or_default()).is_none());
        let failed = ask("auto");
        assert!(matches!(failed, Err(Error::Transcription(why)) if why.contains("no room")));
        // The words of the user's dictionary go in a message of the system.
        let fields = [("language", "en"), ("context", "VinPUT, \"Lovro\".")];
        converse((address, "Zm9v"), &fields, &wav, RECOGNIZE_TIMEOUT)?;

        let requests = engine.join().unwrap_or_default();
        let request = |index: usize| String::from_utf8_lossy(&requests[index]).into_owned();
        assert!(request(0).starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
        // Every request shows the key the engine was started with.
        for index in 0..3 {
            assert!(request(index).contains("\r\nAuthorization: Bearer Zm9v\r\n"));
        }
        assert!(request(0).contains(&BASE64.encode(&wav)));
        assert!(request(0).contains("language English<asr_text>"));
        assert!(!request(1).contains("<asr_text>"));
        assert!(!request(2).contains("<asr_text>"));
        assert!(!request(0).contains(r#""role":"system""#));
        assert!(request(3).contains(
            r#"{"messages":[{"role":"system","content":"VinPUT, \"Lovro\"."},{"role":"user","#
        ));
        Ok(())
    }

    #[test]
    fn what_the_engine_says_went_wrong_is_the_error() -> Outcome {
        let refusal = answer("400 Bad Request", r#"{"error":"no audio in the file"}"#);
        let (address, _engine) = fake_engine(vec![refusal], Duration::ZERO)?;
        let failed = ask(address, &[], &wav_bytes(&[1]));
        assert!(matches!(failed, Err(Error::Transcription(why)) if why.contains("no audio")));
        Ok(())
    }

    #[test]
    fn a_dictation_without_a_pause_is_one_request_when_it_ends() -> Outcome {
        let (address, engine) = fake_engine(vec![heard(" Just this.\n")], Duration::ZERO)?;
        let experience = Arc::<Experience>::default();
        let mut session = session(address, Arc::clone(&experience))?;
        let audio = [room(200), voice(2500), room(300)].concat();
        assert!(speak(&mut session, &audio)?.is_empty());
        assert_eq!(finish(&mut session)?, ["Just this."]);
        // The next dictation knows which language this one was in.
        assert_eq!(experience.language().as_deref(), Some("en"));

        let requests = engine.join().unwrap_or_default();
        assert_eq!(requests.len(), 1);
        let whole = wav_bytes(&audio);
        assert!(
            requests[0]
                .windows(whole.len())
                .any(|window| window == whole)
        );
        Ok(())
    }

    #[test]
    fn a_stretch_is_typed_while_the_dictation_goes_on() -> Outcome {
        let answers = vec![
            heard(" We meet on Thursday,\n"),
            heard(" at ten.\n"),
            heard(" Bring the budget.\n"),
        ];
        let (address, engine) = fake_engine(answers, Duration::from_millis(50))?;
        let mut session = session(address, Arc::default())?;

        // The first stretch is on its way as soon as the speaker pauses, and
        // its text is there while the key is still held.
        let first = [voice(3000), room(700)].concat();
        assert!(speak(&mut session, &first)?.is_empty());
        assert_eq!(
            wait_for_text(&mut session)?.as_deref(),
            Some("We meet on Thursday,")
        );
        // The second is cut off too, and the dictation ends about when the
        // engine answers: the texts that are still to come are typed in the
        // order they were spoken, each set apart from the one before.
        let rest = [voice(2500), room(700), voice(2500)].concat();
        let mut typed = speak(&mut session, &rest)?;
        typed.extend(finish(&mut session)?);
        assert_eq!(typed, [" at ten.", " Bring the budget."]);

        let requests = engine.join().unwrap_or_default();
        assert_eq!(requests.len(), 3);
        assert_eq!(field(&requests[0], "prompt"), None);
        assert_eq!(
            field(&requests[1], "prompt").as_deref(),
            Some("We meet on Thursday,")
        );
        assert_eq!(
            field(&requests[2], "prompt").as_deref(),
            Some("We meet on Thursday, at ten.")
        );
        // Each request carries its own stretch, not the dictation so far.
        assert!(requests.iter().all(|request| request.len() < 4 * 32_000));
        Ok(())
    }

    #[test]
    fn an_engine_that_cannot_be_led_is_handed_the_dictation_whole() -> Outcome {
        struct Unled(SocketAddr);

        impl Inference for Unled {
            fn infer(&self, fields: &[(&str, &str)], wav: &[u8]) -> Result<Value> {
                ask(self.0, fields, wav)
            }

            fn led(&self) -> bool {
                false
            }
        }

        // Quick and free, and there is a pause to cut at: it is not cut,
        // because the two halves would come back as two sentences.
        let answers = vec![heard(" We meet on Thursday at ten.")];
        let (address, engine) = fake_engine(answers, Duration::ZERO)?;
        let engine_of_session: Arc<dyn Inference> = Arc::new(Unled(address));
        let mut session =
            LocalSession::at_pauses(engine_of_session, vec!["en".into()], Arc::default())?;
        let audio = [voice(3000), room(700), voice(2500)].concat();
        assert!(speak(&mut session, &audio)?.is_empty());
        assert_eq!(finish(&mut session)?, ["We meet on Thursday at ten."]);
        assert_eq!(engine.join().unwrap_or_default().len(), 1);

        // Someone who speaks for minutes on end is not held back whole: at
        // the first pause after two minutes what was said so far is sent,
        // so that no request is longer than the engine can answer in time.
        let answers = vec![heard("The first two minutes."), heard("And the rest.")];
        let (address, engine) = fake_engine(answers, Duration::ZERO)?;
        let engine_of_session: Arc<dyn Inference> = Arc::new(Unled(address));
        let mut session =
            LocalSession::at_pauses(engine_of_session, vec!["en".into()], Arc::default())?;
        // Sentences of three seconds with half a second between them.
        let talk = |sentences| [voice(3000), room(500)].concat().repeat(sentences);
        let long = [talk(17), room(700), talk(19), room(700)].concat();
        let mut typed = speak(&mut session, &long)?;
        typed.extend(speak(&mut session, &[voice(2500), room(400)].concat())?);
        typed.extend(finish(&mut session)?);
        assert_eq!(typed.concat(), "The first two minutes. And the rest.");
        let requests = engine.join().unwrap_or_default();
        assert_eq!(requests.len(), 2);
        // The pause after one minute was not one to cut at.
        let seconds = |request: &Vec<u8>| request.len() / (2 * SAMPLE_RATE_HZ as usize);
        assert!((125..=128).contains(&seconds(&requests[0])));
        assert!(seconds(&requests[1]) < 4);
        Ok(())
    }

    #[test]
    fn a_long_recording_is_given_longer_but_not_for_ever() {
        let recording = |seconds: usize| patience(seconds * 2 * SAMPLE_RATE_HZ as usize);
        assert_eq!(recording(0), RECOGNIZE_TIMEOUT);
        assert_eq!(recording(60), Duration::from_secs(300));
        // The longest stretch an engine that takes them whole is handed.
        assert_eq!(recording(125), Duration::from_secs(430));
        // An engine that has not answered after ten minutes is ended.
        assert_eq!(recording(30 * 60), RECOGNIZE_LIMIT);
        assert!(RECOGNIZE_LIMIT < IDLE_UNLOAD);
    }

    /// A server of the second engine, or of the first, that is not started.
    fn unstarted(second: bool) -> Server {
        Server {
            program: PathBuf::from("no-such-engine"),
            model_file: PathBuf::from("no-such-model"),
            ears: second.then(|| PathBuf::from("no-such-ears")),
            name: "a test",
            gpu: false,
            running: Mutex::default(),
        }
    }

    #[test]
    fn an_engine_that_does_not_listen_yet_is_asked_again_soon() -> Outcome {
        // A port nothing listens at: Windows takes two seconds to say so.
        let nowhere = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?.local_addr()?;
        for second in [false, true] {
            let asked = Instant::now();
            assert!(!unstarted(second).loaded(nowhere, "a2V5"));
            let took = asked.elapsed();
            assert!(took < Duration::from_secs(1), "{second}: {took:?}");
        }

        // The second engine listens before its model is loaded, and says
        // when it is, to whoever shows its key. Each look is a connection
        // to see that it listens, and one to ask.
        let nothing = || answer("200 OK", "");
        let loading = answer(
            "503 Service Unavailable",
            r#"{"error":{"message":"Loading"}}"#,
        );
        let ready = answer("200 OK", r#"{"status":"ok"}"#);
        let (address, engine) =
            fake_engine(vec![nothing(), loading, nothing(), ready], Duration::ZERO)?;
        let server = unstarted(true);
        assert!(!server.loaded(address, "a2V5"));
        assert!(server.loaded(address, "a2V5"));
        let requests = engine.join().unwrap_or_default();
        let asked = String::from_utf8_lossy(&requests[3]).into_owned();
        assert!(asked.starts_with("GET /health HTTP/1.1\r\n"), "{asked}");
        assert!(asked.contains("\r\nAuthorization: Bearer a2V5\r\n"));
        Ok(())
    }

    #[test]
    fn the_second_engine_gets_a_key_of_its_own_and_none_of_the_users_settings() -> Outcome {
        let mut command = Command::new("no-such-engine");
        // What someone with a llama.cpp of their own may have set.
        let set = [
            "Path",
            "LLAMA_API_KEY",
            "llama_arg_ctx_size",
            "LLAMA_ARG_N_GPU_LAYERS",
            "LLAMAFILE",
            "HF_TOKEN",
        ];
        let key = secret()?;
        listener(
            &mut command,
            Path::new("ears.gguf"),
            None,
            &key,
            set.into_iter().map(OsString::from),
        );
        let text = |value: &std::ffi::OsStr| value.to_string_lossy().to_ascii_uppercase();
        let mut changed: Vec<(String, Option<String>)> = command
            .get_envs()
            .map(|(name, value)| {
                (
                    text(name),
                    value.map(|value| value.to_string_lossy().into()),
                )
            })
            .collect();
        changed.sort();
        assert_eq!(
            changed,
            [
                ("LLAMA_API_KEY".to_owned(), Some(key.clone())),
                ("LLAMA_ARG_CTX_SIZE".to_owned(), None),
                ("LLAMA_ARG_N_GPU_LAYERS".to_owned(), None),
            ]
        );
        let args: Vec<String> = command.get_args().map(text).collect();
        assert_eq!(
            args,
            [
                "--MMPROJ",
                "EARS.GGUF",
                "--NO-WEBUI",
                "-NP",
                "1",
                "--DEVICE",
                "NONE"
            ]
        );
        // On a graphics card it is told which one, by the engine's number.
        let mut on_a_card = Command::new("llama-server");
        listener(
            &mut on_a_card,
            Path::new("ears.gguf"),
            Some(2),
            &key,
            std::iter::empty(),
        );
        assert_eq!(
            on_a_card.get_args().last().map(text),
            Some("VULKAN2".into())
        );
        // The key is on no command line, where any program could read it,
        assert!(!args.contains(&key.to_ascii_uppercase()));
        // and every start has another.
        assert_eq!(key.len(), 24);
        assert_ne!(secret()?, key);
        Ok(())
    }

    #[test]
    fn a_stretch_that_fails_ends_the_dictation_with_its_error() -> Outcome {
        let broken = answer("500 Internal Server Error", r#"{"error":"out of memory"}"#);
        // While the user speaks: the error comes out of the audio's way in.
        let (address, _engine) = fake_engine(vec![broken.clone()], Duration::ZERO)?;
        let mut session = session(address, Arc::default())?;
        speak(&mut session, &[voice(3000), room(700)].concat())?;
        let failed = wait_for_text(&mut session);
        assert!(matches!(failed, Err(Error::Transcription(why)) if why.contains("out of memory")));

        // Later on: what was recognized before has been typed, the failure
        // is how the dictation ends, and the engine is not asked for what
        // comes after the hole in the text.
        let answers = vec![heard(" One."), broken];
        let (address, engine) = fake_engine(answers, Duration::from_millis(50))?;
        let mut session = self::session(address, Arc::default())?;
        speak(&mut session, &[voice(3000), room(700)].concat())?;
        assert_eq!(wait_for_text(&mut session)?.as_deref(), Some("One."));
        let rest = [voice(2500), room(700), voice(2500)].concat();
        let failed = speak(&mut session, &rest).and_then(|_| finish(&mut session));
        assert!(matches!(failed, Err(Error::Transcription(why)) if why.contains("out of memory")));
        assert_eq!(engine.join().unwrap_or_default().len(), 2);
        Ok(())
    }

    #[test]
    fn an_engine_that_is_gone_is_an_error_too() -> Outcome {
        let (address, engine) = fake_engine(Vec::new(), Duration::ZERO)?;
        let _ = engine.join();
        let mut session = session(address, Arc::default())?;
        speak(&mut session, &voice(2000))?;
        assert!(matches!(finish(&mut session), Err(Error::Transcription(_))));
        Ok(())
    }

    #[test]
    fn a_slow_engine_is_handed_the_dictation_whole() -> Outcome {
        let experience = Arc::<Experience>::default();
        experience.took(Duration::from_secs(4));
        let (address, engine) = fake_engine(vec![heard(" All of it.")], Duration::ZERO)?;
        let mut session = session(address, Arc::clone(&experience))?;
        let audio = [voice(3000), room(1500), voice(3000)].concat();
        assert!(speak(&mut session, &audio)?.is_empty());
        assert_eq!(finish(&mut session)?, ["All of it."]);
        assert_eq!(engine.join().unwrap_or_default().len(), 1);
        // It was quick this time, so the next dictation is cut again.
        assert!(experience.keeps_up());
        Ok(())
    }

    /// The recordings in `clips/`: a sentence, then the room and the click
    /// of the key. `clips/README.md` says where they are from.
    const RECORDED: [(&str, &[u8]); 2] = [
        ("en", include_bytes!("local/clips/en.wav")),
        ("sl", include_bytes!("local/clips/sl.wav")),
    ];
    /// How much of each recording follows its last word.
    const RECORDED_END: usize = 185 * SAMPLE_RATE_HZ as usize / 100;

    /// The samples of a recording, which is a plain 16-bit file whose
    /// header is the usual 44 bytes.
    fn samples_of(wav: &[u8]) -> Vec<i16> {
        wav.get(44..)
            .unwrap_or_default()
            .chunks_exact(2)
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
            .collect()
    }

    #[test]
    fn the_room_after_a_recorded_sentence_is_not_sent_to_the_engine() {
        for (language, wav) in RECORDED {
            let audio = samples_of(wav);
            assert!(audio.len() > 5 * SAMPLE_RATE_HZ as usize, "{language}");
            // As the session does it, with an engine that is always free.
            let mut pauses = Pauses::new();
            let mut sent = Vec::new();
            for packet in audio.chunks(PACKET) {
                sent.extend(pauses.feed(packet, true));
            }
            // The sentence was cut off at the pause after it, and what
            // followed, the room and the key, is no stretch at all, or in
            // a room loud enough to hold a whisper one the engine has to
            // hear speech in with the language left open. 0.3.2 sent it
            // like any other and typed what the engine made up for it.
            assert!(!sent.is_empty(), "{language}");
            let rest = pauses.rest();
            assert!(
                rest.is_none_or(|rest| rest.faint && rest.quiet),
                "{language}"
            );
            // A dictation of nothing but that end, as when the key is
            // pressed by mistake, is doubtful: by itself, or next to the
            // voice the sentence was spoken in.
            let (said, end) = audio.split_at(audio.len() - RECORDED_END);
            let voice = sound::listen(said).map_or(0.0, |sound| sound.loudest);
            let doubtful =
                sound::listen(end).is_none_or(|sound| sound.doubtful || sound.far_under(voice));
            assert!(doubtful, "{language}");
        }
    }

    /// The engine itself, loaded once for [`nothing_is_made_up`].
    fn real_engine() -> Result<Arc<Server>> {
        let setting = |name: &str| {
            std::env::var(name).map_err(|_| Error::Transcription(format!("{name} is not set")))
        };
        let server = Arc::new(Server {
            program: PathBuf::from(setting("VINPUT_TEST_ENGINE")?),
            model_file: PathBuf::from(setting("VINPUT_TEST_MODEL")?),
            ears: std::env::var_os("VINPUT_TEST_EARS").map(PathBuf::from),
            name: "a test",
            gpu: std::env::var_os("VINPUT_TEST_GPU").is_some(),
            running: Mutex::default(),
        });
        server.wake()?;
        Ok(server)
    }

    /// Dictates `audio` to the engine in `languages` and returns what is
    /// typed. Speaks as fast as the engine answers, so that the dictation
    /// is cut at its pauses as one spoken in its own time is.
    fn dictate(
        server: &Arc<Server>,
        languages: &[&str],
        experience: &Arc<Experience>,
        audio: &[i16],
    ) -> Result<String> {
        let languages = languages.iter().map(|code| (*code).to_owned()).collect();
        let engine: Arc<dyn Inference> = Arc::new(Arc::clone(server));
        let mut session = LocalSession::at_pauses(engine, languages, Arc::clone(experience))?;
        let mut typed = String::new();
        for packet in audio.chunks(PACKET) {
            typed.extend(session.feed(packet)?.map(|transcript| transcript.text));
            while matches!(&session, LocalSession::AtPauses { stretches, .. } if !stretches.idle())
            {
                thread::sleep(Duration::from_millis(5));
                typed.extend(session.feed(&[])?.map(|transcript| transcript.text));
            }
        }
        typed.extend(finish(&mut session)?);
        Ok(typed)
    }

    /// Run by hand against the engine itself, never by `cargo test` alone:
    /// `VINPUT_TEST_ENGINE` is the path of `whisper-server.exe` and
    /// `VINPUT_TEST_MODEL` that of a model; `VINPUT_TEST_GPU` set means the
    /// graphics card. Dictates the recordings in `clips/` with one language
    /// configured and with two, and then only what follows their last word,
    /// and holds that nothing is typed that was not said.
    #[test]
    #[ignore = "needs the engine and a model"]
    fn nothing_is_made_up_for_the_room_after_the_last_sentence() -> Result<()> {
        let server = real_engine()?;
        for (language, wav) in RECORDED {
            let audio = samples_of(wav);
            let end = &audio[audio.len() - RECORDED_END..];
            let said = &audio[..audio.len() - RECORDED_END];
            for languages in [&[language][..], &["en", "sl"]] {
                // As from one start of Vinput to the next: what a dictation
                // shows of the speaker, the next one knows.
                let experience = Arc::<Experience>::default();
                let dictate = |audio: &[i16]| dictate(&server, languages, &experience, audio);
                let sentence = dictate(said)?;
                let whole = dictate(&audio)?;
                eprintln!("{language} in {languages:?}: {whole}");
                assert!(whole.split_whitespace().count() >= 5, "{whole}");
                // What follows the sentence adds nothing to it.
                assert_eq!(whole, sentence);
                assert!(!whole.contains(['[', '(', '*', '♪']), "{whole}");
                assert!(!whole.ends_with("..."), "{whole}");
                let last = whole
                    .rsplit(['.', '!', '?'])
                    .find(|part| !part.trim().is_empty());
                assert!(!phantoms::made_up(last.unwrap_or_default()), "{whole}");
                // And a dictation of nothing else comes to nothing.
                assert_eq!(dictate(end)?, "");
            }
        }
        Ok(())
    }

    /// The life of an engine between dictations, with harmless processes
    /// standing in for the engine.
    #[cfg(windows)]
    mod idle {
        use super::*;

        const SECOND: Duration = Duration::from_secs(1);

        /// A process to stand in for the engine: `ping`, which waits between
        /// its echoes to this computer and has no connection.
        fn stand_in() -> std::io::Result<Child> {
            let mut command = Command::new("ping");
            command
                .args(["-n", "600", "127.0.0.1"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            hide_console(&mut command);
            command.spawn()
        }

        /// A stand-in as a loaded engine, handed out just now.
        fn engine() -> std::io::Result<(Running, Arc<Bell>)> {
            let address = SocketAddr::from((Ipv4Addr::LOCALHOST, 9));
            let (engine, watcher) = Running::of(stand_in()?, address, String::new());
            assert!(engine.may_sleep, "the stand-in is in no job");
            Ok((engine, watcher))
        }

        /// A server whose engine cannot be started, so that a test notices
        /// if one is started instead of the stand-in being used.
        fn server(engine: Running) -> Arc<Server> {
            Arc::new(Server {
                program: PathBuf::from("no-such-engine"),
                model_file: PathBuf::from("no-such-model"),
                ears: None,
                name: "a test",
                gpu: false,
                running: Mutex::new(Some(engine)),
            })
        }

        /// Looks at the engine of `server` under its lock.
        fn with<T>(server: &Server, look: impl FnOnce(Option<&mut Running>) -> T) -> T {
            look(
                server
                    .running
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .as_mut(),
            )
        }

        fn asleep(server: &Server) -> bool {
            with(server, |engine| {
                engine.is_some_and(|engine| engine.asleep.is_some())
            })
        }

        /// Records an answer from the engine, as a request does.
        fn answer(server: &Server) {
            with(server, |engine| {
                if let Some(engine) = engine {
                    engine.last_used = Instant::now();
                }
            });
        }

        /// Whether `what` comes true within a generous while.
        fn soon(mut what: impl FnMut() -> bool) -> bool {
            let started = Instant::now();
            while started.elapsed() < 20 * SECOND {
                if what() {
                    return true;
                }
                thread::sleep(Duration::from_millis(5));
            }
            false
        }

        #[test]
        fn an_engine_is_suspended_when_it_is_free_and_has_been_quiet() -> std::io::Result<()> {
            let (mut engine, _watcher) = engine()?;
            let handed_out = engine.lent;
            let free = |_| Some(false);

            // Handed out and no answer recorded: a request may be under way,
            // for as long as one can take.
            assert_eq!(
                engine.look(handed_out + 60 * SECOND, PACE, free),
                Next::Wait(QUIET)
            );
            assert_eq!(
                engine.look(handed_out + LENT + QUIET.saturating_sub(SECOND), PACE, free),
                Next::Wait(SECOND)
            );
            assert!(engine.asleep.is_none());

            // An answer frees it, and the quiet time counts from the answer.
            let answered = handed_out + 2 * SECOND;
            engine.last_used = answered;
            assert_eq!(
                engine.look(answered + 10 * SECOND, PACE, free),
                Next::Wait(QUIET.saturating_sub(10 * SECOND))
            );
            assert!(engine.asleep.is_none());

            // Then it is suspended, until it is ended.
            assert_eq!(
                engine.look(answered + QUIET, PACE, free),
                Next::Wait(IDLE_UNLOAD.saturating_sub(QUIET))
            );
            assert!(engine.asleep.is_some());
            assert!(matches!(engine.child.try_wait(), Ok(None)));
            assert_eq!(
                engine.look(answered + IDLE_UNLOAD.saturating_sub(SECOND), PACE, free),
                Next::Wait(SECOND)
            );
            assert_eq!(
                engine.look(answered + IDLE_UNLOAD, PACE, free),
                Next::Unload
            );
            Ok(())
        }

        #[test]
        fn an_engine_never_answered_is_suspended_once_no_request_can_be_left() -> std::io::Result<()>
        {
            let (mut engine, _watcher) = engine()?;
            let due = engine.lent + LENT + QUIET;
            assert_eq!(
                engine.look(due, PACE, |_| Some(false)),
                Next::Wait(IDLE_UNLOAD.saturating_sub(LENT + QUIET))
            );
            assert!(engine.asleep.is_some());
            Ok(())
        }

        #[test]
        fn an_engine_answering_a_request_is_not_suspended() -> std::io::Result<()> {
            let (mut engine, _watcher) = engine()?;
            // The record says free and quiet, as when a second request is
            // still being answered after the first was.
            let answered = engine.lent + SECOND;
            engine.last_used = answered;
            let process = engine.child.id();
            let answering = |asked| Some(asked == process);
            assert_eq!(
                engine.look(answered + QUIET, PACE, answering),
                Next::Wait(QUIET)
            );
            assert!(engine.asleep.is_none());
            // It is still ended on time.
            assert_eq!(
                engine.look(answered + IDLE_UNLOAD, PACE, answering),
                Next::Unload
            );
            Ok(())
        }

        #[test]
        fn an_engine_is_not_suspended_when_windows_cannot_say_if_it_is_answering()
        -> std::io::Result<()> {
            let (mut engine, _watcher) = engine()?;
            let answered = engine.lent + SECOND;
            engine.last_used = answered;
            assert_eq!(
                engine.look(answered + QUIET, PACE, |_| None),
                Next::Wait(QUIET)
            );
            assert!(engine.asleep.is_none());
            assert_eq!(
                engine.look(answered + IDLE_UNLOAD, PACE, |_| None),
                Next::Unload
            );
            Ok(())
        }

        #[test]
        fn an_engine_that_would_outlive_vinput_is_not_suspended() -> std::io::Result<()> {
            let (mut engine, _watcher) = engine()?;
            engine.may_sleep = false;
            let answered = engine.lent + SECOND;
            engine.last_used = answered;
            assert_eq!(
                engine.look(answered + QUIET, PACE, |_| Some(false)),
                Next::Wait(IDLE_UNLOAD.saturating_sub(QUIET))
            );
            assert!(engine.asleep.is_none());
            Ok(())
        }

        #[test]
        fn a_suspended_engine_runs_again_for_whoever_asks_for_its_address() -> std::io::Result<()> {
            let pace = Pace {
                quiet: Duration::from_millis(40),
                lent: 600 * SECOND,
                unload: 600 * SECOND,
            };
            let (engine, watcher) = engine()?;
            let process = engine.child.id();
            let address = engine.address;
            let server = server(engine);
            server.unload_when_idle(watcher, pace);

            // Handed out and never answered, it stays awake.
            thread::sleep(10 * pace.quiet);
            assert!(!asleep(&server));

            for _ in 0..3 {
                answer(&server);
                assert!(soon(|| asleep(&server)));
                // The same engine, running, and not a new one: starting one
                // fails here.
                assert_eq!(server.address().ok().map(|door| door.0), Some(address));
                assert!(with(&server, |engine| engine.is_some_and(|engine| {
                    engine.asleep.is_none() && engine.child.id() == process
                })));
                // In use again until the next answer.
                thread::sleep(10 * pace.quiet);
                assert!(!asleep(&server));
            }
            Ok(())
        }

        #[test]
        fn a_suspended_engine_is_still_ended_after_its_time() -> std::io::Result<()> {
            let pace = Pace {
                quiet: Duration::from_millis(40),
                lent: 600 * SECOND,
                unload: 3 * SECOND,
            };
            let (engine, watcher) = engine()?;
            let server = server(engine);
            answer(&server);
            server.unload_when_idle(watcher, pace);
            assert!(soon(|| asleep(&server)));
            assert!(soon(|| with(&server, |engine| engine.is_none())));
            Ok(())
        }

        #[test]
        fn an_engine_that_cannot_be_resumed_is_replaced() -> std::io::Result<()> {
            let (mut engine, _watcher) = engine()?;
            engine.sleep();
            assert!(engine.asleep.is_some());
            // Suspended by another as well, it would not answer after ours
            // is undone.
            let other = suspend::suspend(&engine.child)?;
            let server = server(engine);
            // Starting the replacement is what fails here, with a reason.
            let replaced = server.address();
            assert!(
                matches!(&replaced, Err(Error::Transcription(why)) if why.contains("starting"))
            );
            assert!(with(&server, |engine| engine.is_none()));
            drop(other);
            Ok(())
        }

        #[test]
        fn an_engine_that_ended_while_suspended_is_replaced() -> std::io::Result<()> {
            let (mut engine, watcher) = engine()?;
            engine.sleep();
            engine.child.kill()?;
            assert!(soon(|| matches!(engine.child.try_wait(), Ok(Some(_)))));
            let server = server(engine);
            server.unload_when_idle(Arc::clone(&watcher), PACE);
            assert!(server.address().is_err());
            assert!(with(&server, |engine| engine.is_none()));
            // The thread that looked after it goes with it.
            assert!(soon(|| Arc::strong_count(&watcher) == 1));
            Ok(())
        }

        #[test]
        fn a_press_nothing_comes_of_does_not_keep_the_engine_awake() -> std::io::Result<()> {
            let pace = Pace {
                quiet: Duration::from_millis(40),
                lent: 600 * SECOND,
                unload: 600 * SECOND,
            };
            let (engine, watcher) = engine()?;
            let process = engine.child.id();
            let server = server(engine);
            answer(&server);
            server.unload_when_idle(watcher, pace);
            for _ in 0..3 {
                assert!(soon(|| asleep(&server)));
                // What a key press does: the engine runs, nobody has it.
                assert!(server.wake().is_ok());
                assert!(with(&server, |engine| engine.is_some_and(|engine| {
                    engine.asleep.is_none()
                        && engine.child.id() == process
                        && engine.last_used > engine.lent
                })));
            }
            assert!(soon(|| asleep(&server)));

            // A request under way is not forgotten for a press.
            let handed_out = with(&server, |engine| {
                engine.map(|engine| (engine.lent, engine.last_used))
            });
            assert!(server.address().is_ok());
            assert!(server.wake().is_ok());
            let lent = with(&server, |engine| {
                engine.is_some_and(|engine| engine.last_used == engine.lent)
            });
            assert!(lent && handed_out.is_some());
            thread::sleep(10 * pace.quiet);
            assert!(!asleep(&server));
            Ok(())
        }

        /// Whether a process with this identifier exists.
        fn exists(process: u32) -> std::io::Result<bool> {
            let mut list = Command::new("tasklist");
            list.args(["/fi", &format!("PID eq {process}"), "/fo", "csv", "/nh"])
                .stdin(Stdio::null());
            hide_console(&mut list);
            let listed = String::from_utf8_lossy(&list.output()?.stdout).into_owned();
            Ok(listed.contains(&format!("\"{process}\"")))
        }

        #[test]
        fn a_suspended_engine_goes_with_its_server() -> std::io::Result<()> {
            let pace = Pace {
                quiet: Duration::from_millis(40),
                lent: 600 * SECOND,
                unload: 600 * SECOND,
            };
            let (engine, watcher) = engine()?;
            let process = engine.child.id();
            let server = server(engine);
            answer(&server);
            server.unload_when_idle(Arc::clone(&watcher), pace);
            assert!(soon(|| asleep(&server)));
            assert!(exists(process)?);

            // As when another model is chosen. The thread looking after the
            // engine is waiting for the day it would be ended.
            drop(server);
            assert!(soon(|| Arc::strong_count(&watcher) == 1));
            assert!(!exists(process)?);
            Ok(())
        }

        /// The engine itself, for the test that is run by hand.
        struct Real {
            server: Arc<Server>,
            wav: Vec<u8>,
        }

        impl Real {
            fn process(&self) -> u32 {
                with(&self.server, |engine| {
                    engine.map(|engine| engine.child.id())
                })
                .unwrap_or_default()
            }

            /// The processor time the engine has used.
            fn work(&self) -> Duration {
                with(&self.server, |engine| {
                    engine.and_then(|engine| suspend::cpu_time(&engine.child))
                })
                .unwrap_or_default()
            }

            /// Lets the engine be suspended when idle, or not.
            fn allow(&self, sleep: bool) {
                with(&self.server, |engine| {
                    if let Some(engine) = engine {
                        engine.may_sleep = sleep;
                        engine.watcher.set(true, false);
                    }
                });
            }

            /// One request as `infer` makes it: the time to get the address,
            /// which resumes, the time to the answer after that, and the text.
            fn request(&self) -> Result<(Duration, Duration, String)> {
                let fields = [("response_format", "json"), ("language", "en")];
                let started = Instant::now();
                let (address, key) = self.server.address()?;
                let resumed = started.elapsed();
                let reply =
                    self.server
                        .ask((address, &key), &fields, &self.wav, RECOGNIZE_TIMEOUT)?;
                let answered = started.elapsed().saturating_sub(resumed);
                answer(&self.server);
                let text = clean(reply["text"].as_str().unwrap_or_default());
                assert!(!text.is_empty());
                Ok((resumed, answered, text))
            }

            /// Requests to the engine left awake and to the engine suspended,
            /// turn about, so that whatever else the PC is doing weighs on
            /// both alike.
            fn compare(&self, runs: usize) -> Result<()> {
                let mut times = [(Vec::new(), Vec::new()), (Vec::new(), Vec::new())];
                for run in 0..2 * runs {
                    let sleeping = run % 2 == 1;
                    self.allow(sleeping);
                    thread::sleep(3 * SECOND);
                    assert_eq!(asleep(&self.server), sleeping);
                    if sleeping {
                        // Truly idle: not a tick of processor time.
                        let before = self.work();
                        thread::sleep(SECOND);
                        assert_eq!(self.work(), before);
                    }
                    let (resumed, answered, _) = self.request()?;
                    assert!(!asleep(&self.server));
                    let (to_address, to_answer) = &mut times[usize::from(sleeping)];
                    to_address.push(resumed);
                    to_answer.push(answered);
                }
                let median = |times: &mut Vec<Duration>| {
                    times.sort();
                    times
                        .get(times.len() / 2)
                        .map_or(0.0, Duration::as_secs_f64)
                        * 1000.0
                };
                for (state, (to_address, to_answer)) in
                    ["awake    ", "suspended"].iter().zip(&mut times)
                {
                    eprintln!(
                        "{state}: address {:.3} ms, answer {:.1} ms (medians of {runs})",
                        median(to_address),
                        median(to_answer),
                    );
                }
                Ok(())
            }

            /// Keeps the engine awake and then suspended, for it to be
            /// measured from outside.
            fn hold(&self, hold: Duration) -> Result<()> {
                for sleeping in [false, true] {
                    self.allow(sleeping);
                    self.request()?;
                    thread::sleep(3 * SECOND);
                    assert_eq!(asleep(&self.server), sleeping);
                    let before = self.work();
                    eprintln!("{} from now", if sleeping { "SUSPENDED" } else { "AWAKE" });
                    thread::sleep(hold);
                    eprintln!(
                        "used {:.1} ms of processor time in {} s",
                        self.work().saturating_sub(before).as_secs_f64() * 1000.0,
                        hold.as_secs()
                    );
                }
                Ok(())
            }
        }

        /// Run by hand against the engine itself, never by `cargo test`
        /// alone: `VINPUT_TEST_ENGINE` is the path of `whisper-server.exe`,
        /// `VINPUT_TEST_MODEL` that of a model and `VINPUT_TEST_WAV` that of
        /// a recording of English speech; `VINPUT_TEST_GPU` set means the
        /// graphics card. Prints how long loading and the first request
        /// take, then what a request costs an engine that was left awake and
        /// one that was suspended (`VINPUT_TEST_RUNS` of each, nine if not
        /// set), and how long resuming and suspending take. With
        /// `VINPUT_TEST_HOLD` set to a number of seconds it then stays awake
        /// for that long and suspended for that long.
        #[test]
        #[ignore = "needs the engine, a model and a recording"]
        fn the_real_engine_sleeps_and_wakes() -> Result<()> {
            let setting = |name: &str| {
                std::env::var(name).map_err(|_| Error::Transcription(format!("{name} is not set")))
            };
            let number = |name: &str| setting(name).ok().and_then(|text| text.parse::<u64>().ok());
            let real = Real {
                server: Arc::new(Server {
                    program: PathBuf::from(setting("VINPUT_TEST_ENGINE")?),
                    model_file: PathBuf::from(setting("VINPUT_TEST_MODEL")?),
                    ears: std::env::var_os("VINPUT_TEST_EARS").map(PathBuf::from),
                    name: "a test",
                    gpu: std::env::var_os("VINPUT_TEST_GPU").is_some(),
                    running: Mutex::default(),
                }),
                wav: std::fs::read(setting("VINPUT_TEST_WAV")?)
                    .map_err(|err| Error::Transcription(err.to_string()))?,
            };
            let server = &real.server;
            let pace = Pace {
                quiet: SECOND,
                ..PACE
            };
            let started = Instant::now();
            let (engine, watcher) = server.start()?;
            let loaded = started.elapsed();
            eprintln!("engine process {}", engine.child.id());
            *server
                .running
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(engine);
            server.unload_when_idle(watcher, pace);

            let (_, first, text) = real.request()?;
            eprintln!(
                "loaded in {:.0} ms; first answer {:.1} ms: {text}",
                loaded.as_secs_f64() * 1000.0,
                first.as_secs_f64() * 1000.0
            );

            // A request under way shows as a connection of the engine, and
            // only then.
            let seen = thread::scope(|scope| {
                let answer = scope.spawn(|| real.request());
                thread::sleep(Duration::from_millis(40));
                let seen = suspend::has_connection(real.process());
                answer.join().map(|_| seen)
            });
            assert!(matches!(seen, Ok(Some(true))));
            thread::sleep(SECOND / 2);
            assert_eq!(suspend::has_connection(real.process()), Some(false));

            real.compare(usize::try_from(number("VINPUT_TEST_RUNS").unwrap_or(9)).unwrap_or(9))?;

            let suspending = with(server, |engine| {
                engine.map(|engine| {
                    let started = Instant::now();
                    engine.sleep();
                    let took = started.elapsed();
                    assert!(engine.wake());
                    took
                })
            });
            eprintln!(
                "suspending takes {:.2} ms",
                suspending.unwrap_or_default().as_secs_f64() * 1000.0
            );

            if let Some(hold) = number("VINPUT_TEST_HOLD") {
                real.hold(Duration::from_secs(hold))?;
            }
            let (resumed, answered, _) = real.request()?;
            eprintln!(
                "after the hold: address {:.3} ms, answer {:.1} ms",
                resumed.as_secs_f64() * 1000.0,
                answered.as_secs_f64() * 1000.0
            );
            Ok(())
        }
    }
}
