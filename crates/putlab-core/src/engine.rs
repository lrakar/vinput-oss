//! The model engine: `llama-server` of llama.cpp, a child process that
//! keeps a language model loaded between the texts it is asked about.
//! Voutput sums texts up with it; it is here, and not in Voutput, so that
//! the other programs can ask the same engine.
//!
//! Its life: made ready as soon as a model on this PC is what sums up, when
//! Voutput starts and when the user picks one, so that a summary never
//! waits for it; kept for as long as Voutput runs, and started again at
//! once if it ends by itself, as when a driver is replaced under it; ended
//! with Voutput, by the job it is in (`job.rs`). Someone who needs the
//! memory can have it ended when nothing was asked of it for a while; the
//! next summary then starts it again.
//!
//! Making it ready is done on a thread of its own and takes two steps:
//! loading the model, and, on a graphics card, one summary of a text made
//! up here. The second matters. The driver of a card builds the programs
//! the card runs when they are first used, and keeps them on disk: the
//! first summary after an install, or after a new driver, takes half a
//! minute or more of that, and every one after it starts in a fraction of
//! a second. That wait belongs to the moment a model is picked, where the
//! page says what is going on, and not to the first press of the key.
//!
//! It runs on the graphics card whenever the PC has one the user has not
//! turned off, and on the processor when the card cannot run it.
//!
//! It is started without the `LLAMA_` variables of the user's environment,
//! which are for a llama.cpp of their own, and with a key made up for that
//! start, which every request of Voutput carries: it listens on this PC
//! only, but there any program can reach its port, and so can a page in a
//! browser.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::net::Request;
use crate::process::{hide_console, secret};

/// How long a model may take to load. A large one read from a slow disk
/// takes a while.
const LOAD_LIMIT: Duration = Duration::from_secs(180);
/// How long the summary that makes a graphics card ready may take. The
/// first one on a PC that is busy with something else took two minutes
/// when it was measured.
const WARM_LIMIT: Duration = Duration::from_secs(360);
/// How often a loading engine is asked whether it is ready.
const POLL: Duration = Duration::from_millis(100);
/// How often someone who waits for the engine looks whether it is ready.
const LOOK: Duration = Duration::from_millis(25);
/// How long an engine nobody asked anything is kept, by someone who wants
/// its memory back when it is idle.
pub const IDLE_FOR: Duration = Duration::from_secs(30 * 60);
/// How long the summary that makes a graphics card ready may take before
/// it is said to be the first on this PC: later ones take a fraction of a
/// second.
const FIRST_TIME_AFTER: Duration = Duration::from_secs(2);
/// An engine that ends by itself this often, each time within
/// [`SOON`] of being ready, is not started again.
const ENDINGS: u32 = 3;
const SOON: Duration = Duration::from_secs(60);
/// The most text the engine is started to hold at once, in tokens: a long
/// selection and its summary.
const CONTEXT: &str = "16384";
/// The file in the folder of the settings that the engine writes what it
/// has to say to, from its last start on.
const LOG: &str = "engine.log";

/// Where the engine stands.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Standing {
    /// Not running, and nobody waits for it.
    #[default]
    Idle,
    /// Being made ready, on the graphics card if `card` says so.
    Loading {
        /// Whether it is to run on the graphics card.
        card: bool,
        /// How far that is.
        step: Step,
    },
    /// Loaded, and answers at once.
    Ready {
        /// Whether it runs on the graphics card.
        card: bool,
        /// Whether it was to run on the graphics card and the card could
        /// not run it, so the processor does.
        fell_back: bool,
    },
    /// Could not be started: why, in words for the user.
    Failed(String),
}

/// How far an engine is on the way to being ready.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The model is being read in: so many hundredths of it are, as far as
    /// the memory the engine holds says.
    Model(u8),
    /// The graphics card is being made ready, which is slow only the first
    /// time on a PC: for so many seconds now.
    Card(u64),
}

/// What an engine is started for.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Wanted {
    program: PathBuf,
    model: PathBuf,
    /// The name of the graphics card to run on; `None` for the processor.
    card: Option<String>,
}

/// An engine that is running with a model loaded.
struct Running {
    child: Child,
    #[cfg(windows)]
    _job: Option<crate::job::KillOnDrop>,
    base: String,
    key: String,
    on_card: bool,
    last_used: Instant,
    /// Its end, for the thread that starts it again if it ends by itself.
    #[cfg(windows)]
    exit: Option<Arc<crate::job::Exit>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Default)]
struct Inner {
    wanted: Option<Wanted>,
    standing: Standing,
    running: Option<Running>,
    /// Counts what was wanted: a start that finds another number here than
    /// the one it was begun with has been overtaken, and gives up.
    turn: u64,
}

/// The model engine of this process: at most one, with one model.
#[derive(Default)]
pub struct Engine {
    inner: Mutex<Inner>,
    /// Called when where the engine stands has changed.
    changed: Option<Box<dyn Fn() + Send + Sync>>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("standing", &self.standing())
            .finish_non_exhaustive()
    }
}

/// What the engine is started with for `model`, to listen on `port`, on
/// the graphics card it knows as `device` or on the processor.
fn arguments(model: &Path, port: u16, device: Option<&str>) -> Vec<String> {
    let threads = thread::available_parallelism().map_or(4, |count| count.get().clamp(1, 8));
    let mut arguments = vec![
        "--model".to_owned(),
        model.display().to_string(),
        "--host".to_owned(),
        "127.0.0.1".to_owned(),
        "--port".to_owned(),
        port.to_string(),
        "--threads".to_owned(),
        threads.to_string(),
        "--ctx-size".to_owned(),
        CONTEXT.to_owned(),
        // One summary at a time, no page for a browser, and the template
        // the model came with.
        "--no-webui".to_owned(),
        "--parallel".to_owned(),
        "1".to_owned(),
        "--jinja".to_owned(),
        // One graphics card or none: left to itself the engine spreads a
        // model over every card it finds.
        "--device".to_owned(),
        device.unwrap_or("none").to_owned(),
    ];
    if device.is_some() {
        // The whole model on the card: a part of it left to the processor
        // is what the whole summary then waits for.
        arguments.extend(["--gpu-layers".to_owned(), "999".to_owned()]);
    }
    arguments
}

/// The name the engine knows the graphics card `card` by, given what it
/// lists as its devices, a line each: `Vulkan1: AMD Radeon RX 7800 XT (16368
/// MiB, 15000 MiB free)`. The engine numbers the cards in an order of its
/// own, and its first is not always the one Windows calls the best. A card
/// it does not list by that name is taken to be its first.
fn device_for(listing: &str, card: &str) -> String {
    let devices = listing.lines().filter_map(|line| {
        let (id, rest) = line.trim().split_once(": ")?;
        let named = id.starts_with("Vulkan") && !id.contains(' ');
        let name = rest.rfind(" (").map_or(rest, |end| &rest[..end]);
        named.then_some((id, name.trim()))
    });
    let devices: Vec<(&str, &str)> = devices.collect();
    let same = devices
        .iter()
        .find(|(_, name)| name.eq_ignore_ascii_case(card.trim()));
    same.or(devices.first())
        .map_or("Vulkan0", |(id, _)| id)
        .to_owned()
}

/// Whether the variable `name` is one the engine would take a setting from.
/// Windows does not tell the names of variables apart by their case.
fn is_engine_setting(name: &std::ffi::OsStr) -> bool {
    name.to_string_lossy()
        .to_ascii_uppercase()
        .starts_with("LLAMA_")
}

/// The engine `program`, to be started with nothing of the user's own
/// llama.cpp, from its own folder, where its libraries are, and unseen.
fn command(program: &Path) -> Command {
    let mut command = Command::new(program);
    for (name, _) in std::env::vars_os() {
        if is_engine_setting(&name) {
            command.env_remove(name);
        }
    }
    if let Some(folder) = program.parent() {
        command.current_dir(folder);
    }
    command.stdin(Stdio::null());
    hide_console(&mut command);
    command
}

/// What the engine lists as the graphics cards it can run on; nothing if
/// it could not be asked.
fn listing(program: &Path) -> String {
    let asked = command(program).arg("--list-devices").output();
    asked.map_or_else(
        |_| String::new(),
        |output| String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

/// Where a starting engine writes what it has to say: a file beside the
/// settings, begun anew at each start, or nowhere.
fn log_of_the_engine() -> (Stdio, Stdio) {
    let nowhere = || (Stdio::null(), Stdio::null());
    // Tests start engines that are none; that is nothing to keep.
    if cfg!(any(test, feature = "test-instance")) {
        return nowhere();
    }
    let Some(folder) = crate::folders::data_dir() else {
        return nowhere();
    };
    let _ = std::fs::create_dir_all(&folder);
    let Ok(file) = std::fs::File::create(folder.join(LOG)) else {
        return nowhere();
    };
    match file.try_clone() {
        Ok(same) => (Stdio::from(file), Stdio::from(same)),
        Err(_) => nowhere(),
    }
}

/// A port of this computer that is free right now.
fn free_port() -> Option<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").ok()?;
    listener.local_addr().ok().map(|address| address.port())
}

/// Whether the engine at `base` has its model loaded and answers.
fn is_ready(base: &str, key: &str) -> bool {
    let bearer = format!("Bearer {key}");
    let asked = Request {
        headers: &[("Authorization", &bearer)],
        ..Request::get(&format!("{base}/health"), Duration::from_secs(2))
    }
    .send();
    asked.is_ok_and(|response| response.status() == 200)
}

/// The text a graphics card is made ready with: as long as a few
/// paragraphs, because the programs a card runs for a long text are not
/// the ones it runs for a short one.
fn warming_text() -> String {
    "The river runs past the old mill and under the stone bridge on its way to the sea. ".repeat(24)
}

/// How far the load of `model` is, in hundredths, going by the memory the
/// engine `child` holds: a loaded model takes up a little more than its
/// file. Never whole; the engine says when it is.
fn loaded(child: &Child, model_bytes: u64) -> u8 {
    #[cfg(windows)]
    let held = crate::job::memory_of(child).unwrap_or(0);
    #[cfg(not(windows))]
    let held = {
        let _ = child;
        0u64
    };
    let whole = (model_bytes / 4 * 5).max(1);
    u8::try_from((held.saturating_mul(100) / whole).min(99)).unwrap_or(99)
}

/// Does `work`, and while it lasts tells `report` every second for how long
/// it has, from [`FIRST_TIME_AFTER`] on.
fn timed<T>(report: &(dyn Fn(Step) + Sync), work: impl FnOnce() -> T) -> T {
    let done = AtomicBool::new(false);
    thread::scope(|scope| {
        scope.spawn(|| {
            let began = Instant::now();
            let mut told = 0;
            while !done.load(Ordering::Acquire) {
                thread::sleep(POLL);
                let lasted = began.elapsed();
                if lasted >= FIRST_TIME_AFTER && lasted.as_secs() != told {
                    told = lasted.as_secs();
                    report(Step::Card(told));
                }
            }
        });
        let made = work();
        done.store(true, Ordering::Release);
        made
    })
}

/// Has the engine sum a text up once, so that everything the graphics card
/// needs for it is built before anyone waits for a summary.
fn warm(engine: &Running) -> Result<(), String> {
    let body = json!({
        "model": "local",
        "stream": false,
        "max_tokens": 4,
        "cache_prompt": false,
        "chat_template_kwargs": { "enable_thinking": false },
        "messages": [
            { "role": "system", "content": "Sum the text up in one sentence." },
            { "role": "user", "content": warming_text() },
        ],
    })
    .to_string();
    let bearer = format!("Bearer {}", engine.key);
    let address = format!("{}/v1/chat/completions", engine.base);
    let mut response = Request {
        json: Some(&body),
        headers: &[("Authorization", &bearer)],
        connect_timeout: Duration::from_secs(10),
        stall_timeout: WARM_LIMIT,
        ..Request::get(&address, WARM_LIMIT)
    }
    .send()?;
    let status = response.status();
    response.body(1 << 20, WARM_LIMIT)?;
    if status == 200 {
        Ok(())
    } else {
        Err(format!("the engine answered with an error ({status})"))
    }
}

impl Engine {
    /// An engine that is not running.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The same, which calls `changed` whenever where it stands has changed.
    #[must_use]
    pub fn telling(changed: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            inner: Mutex::default(),
            changed: Some(Box::new(changed)),
        }
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn tell(&self) {
        if let Some(changed) = &self.changed {
            changed();
        }
    }

    /// Where the engine stands.
    #[must_use]
    pub fn standing(&self) -> Standing {
        self.inner().standing.clone()
    }

    /// Whether an engine is running, or being made ready.
    #[must_use]
    pub fn is_running(&self) -> bool {
        let inner = self.inner();
        inner.running.is_some() || matches!(inner.standing, Standing::Loading { .. })
    }

    /// Makes an engine ready that serves `model` with `program`, on the
    /// graphics card named `card` or, without one, on the processor: on a
    /// thread of its own, and only if the one that is running or being
    /// made ready is not that already. Returns at once.
    pub fn prepare(self: &Arc<Self>, program: &Path, model: &Path, card: Option<&str>) {
        let wanted = Wanted {
            program: program.to_owned(),
            model: model.to_owned(),
            card: card.map(str::to_owned),
        };
        let turn = {
            let mut inner = self.inner();
            let same = inner.wanted.as_ref() == Some(&wanted);
            let alive = inner
                .running
                .as_mut()
                .is_some_and(|engine| matches!(engine.child.try_wait(), Ok(None)));
            match inner.standing {
                Standing::Loading { .. } if same => return,
                Standing::Ready { .. } if same && alive => return,
                _ => {}
            }
            // Another model, another place to run it, or an engine that
            // died or never started.
            inner.running = None;
            inner.turn += 1;
            inner.standing = Standing::Loading {
                card: wanted.card.is_some(),
                step: Step::Model(0),
            };
            inner.wanted = Some(wanted.clone());
            inner.turn
        };
        self.tell();
        let engine = Arc::clone(self);
        let started = thread::Builder::new()
            .name("voutput-engine".into())
            .spawn(move || engine.keep(&wanted, turn));
        if started.is_err() {
            let why = "The model on this PC could not be started: Windows gave it no thread.";
            self.inner().standing = Standing::Failed(why.to_owned());
            self.tell();
        }
    }

    /// The address and the key of an engine that serves `model`, on the
    /// graphics card named `card` or on the processor: the one that is
    /// running if it does, else one started with `program`, which is waited
    /// for until it is ready or `stop` is set.
    ///
    /// # Errors
    ///
    /// Returns why no engine serves the model, in words for the user.
    pub fn serve(
        self: &Arc<Self>,
        program: &Path,
        model: &Path,
        card: Option<&str>,
        stop: &AtomicBool,
    ) -> Result<(String, String), String> {
        self.prepare(program, model, card);
        loop {
            {
                let mut inner = self.inner();
                match &inner.standing {
                    Standing::Ready { .. } => {
                        if let Some(engine) = inner.running.as_mut() {
                            engine.last_used = Instant::now();
                            return Ok((engine.base.clone(), engine.key.clone()));
                        }
                    }
                    Standing::Failed(why) => return Err(why.clone()),
                    Standing::Idle => {
                        return Err("The model on this PC was stopped.".to_owned());
                    }
                    Standing::Loading { .. } => {}
                }
            }
            // The engine goes on being made ready: the next summary finds
            // it so.
            if stop.load(Ordering::Acquire) {
                return Err("The model on this PC was not ready yet.".to_owned());
            }
            thread::sleep(LOOK);
        }
    }

    /// The thread of an engine: makes one ready for `wanted`, and another
    /// whenever that one ends by itself, until something else is wanted,
    /// which is when `turn` is no longer the number of what is.
    fn keep(&self, wanted: &Wanted, turn: u64) {
        let overtaken = || self.inner().turn != turn;
        let card = wanted.card.is_some();
        let report = |step: Step| {
            {
                let mut inner = self.inner();
                let loading = Standing::Loading { card, step };
                if inner.turn != turn || inner.standing == loading {
                    return;
                }
                inner.standing = loading;
            }
            self.tell();
        };
        let mut endings = 0;
        loop {
            let started = Self::start(wanted, &overtaken, &report);
            let (ended, since) = {
                let mut inner = self.inner();
                if inner.turn != turn {
                    return;
                }
                let ended = match started {
                    Ok(running) => {
                        inner.standing = Standing::Ready {
                            card: running.on_card,
                            fell_back: card && !running.on_card,
                        };
                        #[cfg(windows)]
                        let ended = running.exit.clone();
                        #[cfg(not(windows))]
                        let ended = None::<()>;
                        inner.running = Some(running);
                        ended
                    }
                    Err(why) => {
                        inner.standing = Standing::Failed(why);
                        None
                    }
                };
                (ended, Instant::now())
            };
            self.tell();
            // Until it ends: because something else is wanted, or by itself.
            #[cfg(windows)]
            let Some(ended) = ended else { return };
            #[cfg(not(windows))]
            let Some(()) = ended else { return };
            #[cfg(windows)]
            ended.wait();
            endings = if since.elapsed() < SOON {
                endings + 1
            } else {
                1
            };
            {
                let mut inner = self.inner();
                if inner.turn != turn {
                    return;
                }
                inner.running = None;
                if endings >= ENDINGS {
                    let why = "The model on this PC keeps stopping. See engine.log in the \
                               settings folder.";
                    inner.standing = Standing::Failed(why.to_owned());
                } else {
                    inner.standing = Standing::Loading {
                        card,
                        step: Step::Model(0),
                    };
                }
            }
            crate::log::write("the model engine ended by itself");
            self.tell();
            if endings >= ENDINGS {
                return;
            }
        }
    }

    /// Starts an engine for `wanted` and waits until it answers at once:
    /// on its graphics card, or, if the card cannot run it, on the
    /// processor. How far it is goes to `report`.
    fn start(
        wanted: &Wanted,
        overtaken: &dyn Fn() -> bool,
        report: &(dyn Fn(Step) + Sync),
    ) -> Result<Running, String> {
        if let Some(card) = &wanted.card {
            let device = device_for(&listing(&wanted.program), card);
            let on_card = Self::launch(wanted, Some(&device), overtaken, report)
                .and_then(|engine| timed(report, || warm(&engine)).map(|()| engine));
            match on_card {
                Ok(engine) => return Ok(engine),
                Err(why) if overtaken() => return Err(why),
                Err(why) => crate::log::write(&format!(
                    "the graphics card could not run the model, so the processor does: {why}"
                )),
            }
        }
        report(Step::Model(0));
        Self::launch(wanted, None, overtaken, report)
    }

    fn launch(
        wanted: &Wanted,
        device: Option<&str>,
        overtaken: &dyn Fn() -> bool,
        report: &(dyn Fn(Step) + Sync),
    ) -> Result<Running, String> {
        let failed = |what: &str| format!("The model on this PC could not be started: {what}.");
        let port = free_port().ok_or_else(|| failed("no port is free"))?;
        let key = secret().ok_or_else(|| failed("Windows gave no random numbers"))?;
        let (output, errors) = log_of_the_engine();
        let mut command = command(&wanted.program);
        command
            .args(arguments(&wanted.model, port, device))
            .stdout(output)
            .stderr(errors)
            .env("LLAMA_API_KEY", &key);
        let child = command.spawn().map_err(|err| failed(&err.to_string()))?;
        let mut engine = Running {
            #[cfg(windows)]
            _job: crate::job::KillOnDrop::attach(&child),
            #[cfg(windows)]
            exit: crate::job::Exit::of(&child).map(Arc::new),
            child,
            base: format!("http://127.0.0.1:{port}"),
            key,
            on_card: device.is_some(),
            last_used: Instant::now(),
        };
        // It listens once the model is loaded.
        let began = Instant::now();
        let model_bytes = std::fs::metadata(&wanted.model).map_or(0, |file| file.len());
        loop {
            report(Step::Model(loaded(&engine.child, model_bytes)));
            if let Ok(Some(status)) = engine.child.try_wait() {
                return Err(failed(&format!("it stopped while loading ({status})")));
            }
            if is_ready(&engine.base, &engine.key) {
                engine.last_used = Instant::now();
                return Ok(engine);
            }
            if overtaken() {
                return Err(failed("stopped"));
            }
            if began.elapsed() > LOAD_LIMIT {
                return Err(failed("the model took too long to load"));
            }
            thread::sleep(POLL);
        }
    }

    /// Ends the engine if nothing was asked of it for `idle`. Returns
    /// whether an engine is still running, or being made ready.
    pub fn let_go_if_idle(&self, idle: Duration) -> bool {
        let unused = self
            .inner()
            .running
            .as_ref()
            .is_some_and(|engine| engine.last_used.elapsed() >= idle);
        if unused {
            self.end();
        }
        self.is_running()
    }

    /// Ends the engine, if one is running or being made ready.
    pub fn end(&self) {
        {
            let mut inner = self.inner();
            if inner.wanted.is_none() && inner.standing == Standing::Idle {
                return;
            }
            inner.turn += 1;
            inner.wanted = None;
            inner.running = None;
            inner.standing = Standing::Idle;
        }
        self.tell();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    #[test]
    fn the_engine_is_told_one_summary_at_a_time_on_this_pc_only() {
        let model = Path::new(r"C:\models\gemma.gguf");
        let on_processor = arguments(model, 4321, None);
        let has = |arguments: &[String], pair: [&str; 2]| {
            arguments
                .windows(2)
                .any(|two| two[0] == pair[0] && two[1] == pair[1])
        };
        assert!(has(&on_processor, ["--host", "127.0.0.1"]));
        assert!(has(&on_processor, ["--port", "4321"]));
        assert!(has(&on_processor, ["--parallel", "1"]));
        assert!(has(&on_processor, ["--device", "none"]));
        assert!(has(&on_processor, ["--model", r"C:\models\gemma.gguf"]));
        assert!(on_processor.contains(&"--no-webui".to_owned()));
        assert!(!on_processor.contains(&"--gpu-layers".to_owned()));
        let on_card = arguments(model, 4321, Some("Vulkan1"));
        assert!(has(&on_card, ["--device", "Vulkan1"]));
        assert!(has(&on_card, ["--gpu-layers", "999"]));
    }

    #[test]
    fn the_card_windows_names_is_found_among_those_the_engine_lists() {
        let listing = "Available devices:\n  \
                       Vulkan0: Intel(R) UHD Graphics 770 (32000 MiB, 31000 MiB free)\n  \
                       Vulkan1: AMD Radeon RX 7800 XT (16368 MiB, 15000 MiB free)\n";
        assert_eq!(device_for(listing, "AMD Radeon RX 7800 XT"), "Vulkan1");
        assert_eq!(device_for(listing, " amd radeon rx 7800 xt "), "Vulkan1");
        assert_eq!(device_for(listing, "Intel(R) UHD Graphics 770"), "Vulkan0");
        // A card it lists under another name, or an engine that lists
        // nothing: its first.
        assert_eq!(device_for(listing, "Some Other Card"), "Vulkan0");
        assert_eq!(device_for("", "AMD Radeon RX 7800 XT"), "Vulkan0");
        assert_eq!(
            device_for("  Vulkan3: A Card (1 MiB, 1 MiB free)", "x"),
            "Vulkan3"
        );
    }

    #[test]
    fn a_card_is_made_ready_with_a_text_as_long_as_a_few_paragraphs() {
        let words = warming_text().split_whitespace().count();
        assert!((300..=600).contains(&words), "{words}");
    }

    #[test]
    fn settings_of_a_llama_of_the_users_own_are_not_handed_on() {
        for theirs in [
            "LLAMA_API_KEY",
            "llama_arg_ctx_size",
            "Llama_Arg_N_Gpu_Layers",
        ] {
            assert!(is_engine_setting(std::ffi::OsStr::new(theirs)), "{theirs}");
        }
        for other in ["PATH", "ALLAMA_X", "APPDATA"] {
            assert!(!is_engine_setting(std::ffi::OsStr::new(other)), "{other}");
        }
    }

    #[test]
    fn an_engine_that_cannot_be_started_is_an_error_and_none_is_left_running() {
        let told = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&told);
        let engine = Arc::new(Engine::telling(move || {
            counted.fetch_add(1, Ordering::AcqRel);
        }));
        assert_eq!(engine.standing(), Standing::Idle);
        let stop = AtomicBool::new(false);
        // With a card and without: neither can start a program that is not
        // there, and the card is not blamed for it twice.
        for card in [Some("A Card"), None] {
            let served = engine.serve(
                Path::new(r"C:\no\such\llama-server.exe"),
                Path::new(r"C:\no\such\model.gguf"),
                card,
                &stop,
            );
            assert!(
                served
                    .is_err_and(|why| why.starts_with("The model on this PC could not be started"))
            );
            assert!(matches!(engine.standing(), Standing::Failed(_)));
            assert!(!engine.is_running());
        }
        // Each start was told of twice: when it began, and when it failed.
        assert_eq!(told.load(Ordering::Acquire), 4);
        assert!(!engine.let_go_if_idle(Duration::ZERO));
        engine.end();
        assert_eq!(engine.standing(), Standing::Idle);
        assert_eq!(told.load(Ordering::Acquire), 5);
        // Ending what is not running is nothing to tell of.
        engine.end();
        assert_eq!(told.load(Ordering::Acquire), 5);
    }

    #[test]
    fn how_far_a_load_is_goes_by_the_memory_the_engine_holds() {
        // This test is a process too, and holds some memory.
        let Ok(mut child) = Command::new(std::env::current_exe().unwrap_or_default())
            .arg("--list")
            .stdout(Stdio::null())
            .spawn()
        else {
            unreachable!("the tests can start themselves");
        };
        // Against a file of a byte it is as good as loaded, and never whole.
        let small = loaded(&child, 1);
        let large = loaded(&child, u64::MAX / 200);
        let _ = child.wait();
        assert!(small <= 99 && large == 0, "{small} {large}");
    }

    #[test]
    fn what_takes_long_says_for_how_long() {
        let seen = Mutex::new(Vec::new());
        let report = |step: Step| {
            if let Ok(mut seen) = seen.lock() {
                seen.push(step);
            }
        };
        // Nothing is said of what is over at once.
        assert_eq!(timed(&report, || 7), 7);
        assert!(seen.lock().is_ok_and(|seen| seen.is_empty()));
        timed(&report, || {
            thread::sleep(FIRST_TIME_AFTER + Duration::from_millis(1300));
        });
        let seen = seen.lock().map(|seen| seen.clone()).unwrap_or_default();
        assert_eq!(seen, [Step::Card(2), Step::Card(3)]);
    }

    /// With `VOUTPUT_TEST_ENGINE` and `VOUTPUT_TEST_MODEL` naming a
    /// `llama-server.exe` and a model file, and `VOUTPUT_TEST_CARD` a
    /// graphics card if it is to run on one: starts the engine for real,
    /// prints where it stood on the way, ends it from outside and waits
    /// for it to be ready again by itself.
    #[test]
    #[ignore = "needs an engine and a model on this PC"]
    #[allow(clippy::print_stderr, reason = "what it saw is its output")]
    fn a_real_engine_says_how_far_it_is_and_comes_back_when_it_ends() {
        let named = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
        let (Some(program), Some(model)) =
            (named("VOUTPUT_TEST_ENGINE"), named("VOUTPUT_TEST_MODEL"))
        else {
            unreachable!("set VOUTPUT_TEST_ENGINE and VOUTPUT_TEST_MODEL");
        };
        let card = named("VOUTPUT_TEST_CARD");
        let began = Instant::now();
        let engine = Arc::new(Engine::new());
        let watch = |until: &dyn Fn(&Standing) -> bool| {
            let mut last = None;
            loop {
                let standing = engine.standing();
                if last.as_ref() != Some(&standing) {
                    eprintln!("{:6.2} s  {standing:?}", began.elapsed().as_secs_f64());
                    last = Some(standing.clone());
                }
                if until(&standing) {
                    return standing;
                }
                thread::sleep(Duration::from_millis(20));
            }
        };
        let settled = |standing: &Standing| !matches!(standing, Standing::Loading { .. });
        engine.prepare(Path::new(&program), Path::new(&model), card.as_deref());
        let ready = watch(&settled);
        assert!(matches!(ready, Standing::Ready { .. }), "{ready:?}");
        // Ended from outside, as a driver that is replaced would end it.
        if let Some(running) = engine.inner().running.as_mut() {
            let _ = running.child.kill();
        }
        eprintln!("ended from outside");
        watch(&|standing| matches!(standing, Standing::Loading { .. }));
        let again = watch(&settled);
        assert!(matches!(again, Standing::Ready { .. }), "{again:?}");
        engine.end();
        assert_eq!(engine.standing(), Standing::Idle);
    }

    #[test]
    fn a_port_and_a_key_are_made_up_for_each_start() {
        assert!(free_port().is_some_and(|port| port > 0));
        let (first, second) = (secret(), secret());
        assert!(first.as_ref().is_some_and(|key| key.len() == 32));
        assert_ne!(first, second);
        // Nothing answers on a port nobody listens on.
        assert!(!is_ready("http://127.0.0.1:9", "key"));
    }
}
