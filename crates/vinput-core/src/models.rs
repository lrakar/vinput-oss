//! The speech models Vinput offers, and the installer for the ones that run on
//! this computer.
//!
//! A local model is two downloads: the model file itself, and the engine that
//! runs it, which every local model shares. Both come from the address pinned
//! here and are checked against the SHA-256 pinned here before they are used.
//! The engine is a separate program that Vinput starts only while a local
//! model is selected, so none of it is part of Vinput's own executable.
//!
//! Besides the catalog pinned here, people can add Whisper models of their
//! own, from Hugging Face or from a file; see [`custom`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use crate::download::{self, Source};
use crate::settings::local_dir;
use crate::stt::ProviderKind;
use crate::{Error, Result, log};

pub mod custom;
pub mod ggml;
pub mod hub;

/// A pinned download: the file name it is stored under and where it comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Download {
    /// File name on disk.
    pub file: &'static str,
    /// Address, size and checksum.
    pub source: Source<'static>,
}

/// How the engine runs a local model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runner {
    /// A server that keeps the model loaded between dictations.
    Server,
    /// A program started for each dictation.
    Command,
}

impl Runner {
    /// The engine program that runs models of this kind.
    #[must_use]
    pub fn program(self) -> &'static str {
        match self {
            Self::Server => "whisper-server.exe",
            Self::Command => "parakeet-cli.exe",
        }
    }
}

/// A model that can be picked in Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Model {
    /// The value of `model` in the settings file.
    pub id: &'static str,
    /// The provider that runs it.
    pub provider: ProviderKind,
    /// Name shown in the list.
    pub name: &'static str,
    /// One line about what it needs or is good at.
    pub summary: &'static str,
    /// What the list says beside its place and size: the languages of a local
    /// model, what an online one needs.
    pub detail: &'static str,
    /// The file to install and how it is run, for a local model.
    pub local: Option<(Download, Runner)>,
}

macro_rules! pinned {
    ($base:expr, $file:literal, $bytes:literal, $sha256:literal) => {
        Download {
            file: $file,
            source: Source {
                url: concat!($base, $file),
                sha256: $sha256,
                bytes: $bytes,
            },
        }
    };
}

// Every download is pinned to one revision of its repository, so the bytes
// behind an address cannot change under the checksum.
macro_rules! whisper_model {
    ($file:literal, $bytes:literal, $sha256:literal) => {
        pinned!(
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/",
            $file,
            $bytes,
            $sha256
        )
    };
}

/// Every model, in the order Settings lists them.
pub static CATALOG: [Model; 7] = [
    Model {
        id: crate::cloud::MODEL_ID,
        provider: ProviderKind::Cloud,
        name: "VinPUT Pro",
        summary: "Online. Types as you speak, in 60+ languages.",
        detail: "Needs a subscription",
        local: None,
    },
    Model {
        id: "chirp_3",
        provider: ProviderKind::GoogleCloud,
        name: "Google Cloud Speech (Chirp 3)",
        summary: "Online. Types as you speak.",
        detail: "Needs a Google Cloud project",
        local: None,
    },
    Model {
        id: "gemini-3.5-transcribe-live",
        provider: ProviderKind::Gemini,
        name: "Google Gemini 3.5 Transcribe",
        summary: "Online. Types as you speak.",
        detail: "Needs a Gemini API key",
        local: None,
    },
    Model {
        id: "parakeet-tdt-0.6b-v3",
        provider: ProviderKind::Local,
        name: "NVIDIA Parakeet TDT v3",
        summary: "On this PC. 25 European languages. Text arrives when you stop.",
        detail: "25 European languages",
        local: Some((
            pinned!(
                "https://huggingface.co/ggml-org/parakeet-GGUF/resolve/35156454d1a39de06863303dd209fd2bed6ee079/",
                "ggml-parakeet-tdt-0.6b-v3-q8_0.bin",
                668_757_119,
                "4d64e9e96c2792186d072fde0034df0ad670cf680a2f53069052ead827fd600e"
            ),
            Runner::Command,
        )),
    },
    Model {
        id: "whisper-large-v3-turbo",
        provider: ProviderKind::Local,
        name: "Whisper large-v3 turbo",
        summary: "On this PC. 99 languages, best accuracy. Text arrives when you stop.",
        detail: "99 languages",
        local: Some((
            whisper_model!(
                "ggml-large-v3-turbo.bin",
                1_624_555_275,
                "1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69"
            ),
            Runner::Server,
        )),
    },
    Model {
        id: "whisper-large-v3-turbo-q5",
        provider: ProviderKind::Local,
        name: "Whisper large-v3 turbo (compact)",
        summary: "On this PC. A third of the size, close to full accuracy.",
        detail: "99 languages",
        local: Some((
            whisper_model!(
                "ggml-large-v3-turbo-q5_0.bin",
                574_041_195,
                "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2"
            ),
            Runner::Server,
        )),
    },
    Model {
        id: "whisper-base",
        provider: ProviderKind::Local,
        name: "Whisper base",
        summary: "On this PC. Small and fast on any processor, lower accuracy.",
        detail: "99 languages",
        local: Some((
            whisper_model!(
                "ggml-base.bin",
                147_951_465,
                "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe"
            ),
            Runner::Server,
        )),
    },
];

/// The engine build for processors, the one for graphics cards of any make,
/// and the one for NVIDIA cards that earlier versions installed.
static ENGINE_CPU: Download = pinned!(
    "https://github.com/ggml-org/whisper.cpp/releases/download/v1.9.2/",
    "whisper-bin-x64.zip",
    8_194_445,
    "49dcc16de826f20bd53d44f947a1ae49dfa81f86cad67a64d80820cb192d674a"
);
// The engine's own releases have no build for graphics cards other than
// NVIDIA's, so this one is built from the same release with Vulkan, by the
// workflow in .github/workflows/engine-vulkan.yml, and kept on Vinput's site.
static ENGINE_VULKAN: Download = pinned!(
    "https://vinput-dasvcr.web.app/engines/",
    "whisper-vulkan-1.9.2-bin-x64.zip",
    18_208_039,
    "1f2a7800c80e0be4d2267b91ddff0b377977718f5cf0405417d31e58164d89ce"
);
static ENGINE_CUDA: Download = pinned!(
    "https://github.com/ggml-org/whisper.cpp/releases/download/v1.9.2/",
    "whisper-cublas-12.4.0-bin-x64.zip",
    670_611_449,
    "443110ddaad70d4290ab2e77179e31cf712035bbc4fad56bb4519a90c917b39c"
);

impl Model {
    /// Whether someone added it, rather than it being part of the catalog.
    #[must_use]
    pub fn is_custom(&self) -> bool {
        self.id.starts_with(custom::PREFIX)
    }
}

/// Every model that can be picked: the catalog, then the ones people added.
#[must_use]
pub fn all() -> Vec<&'static Model> {
    CATALOG.iter().chain(custom::models()).collect()
}

/// The model for a provider and model name, if it is one Vinput knows.
#[must_use]
pub fn find(provider: ProviderKind, model: &str) -> Option<&'static Model> {
    all()
        .into_iter()
        .find(|entry| entry.provider == provider && entry.id.eq_ignore_ascii_case(model))
}

/// What to call the model the settings choose: its name in the catalog, the
/// name as written for one set by hand, or `None` while none is chosen.
#[must_use]
pub fn name_in_use(provider: ProviderKind, model: &str) -> Option<&str> {
    match find(provider, model) {
        Some(entry) => Some(entry.name),
        None => Some(model).filter(|name| !name.is_empty()),
    }
}

/// The model with this id.
#[must_use]
pub fn by_id(id: &str) -> Option<&'static Model> {
    all()
        .into_iter()
        .find(|entry| entry.id.eq_ignore_ascii_case(id))
}

/// The engine build to use. On a graphics card that is the Vulkan build,
/// unless the larger NVIDIA build of an earlier version is already here.
#[must_use]
pub fn engine(gpu: bool) -> &'static Download {
    if !gpu {
        &ENGINE_CPU
    } else if program_in(&ENGINE_CUDA, Runner::Server).is_some() {
        &ENGINE_CUDA
    } else {
        &ENGINE_VULKAN
    }
}

/// Where a model file is kept: `%LOCALAPPDATA%\Vinput\models`.
#[must_use]
pub fn model_path(file: &Download) -> Option<PathBuf> {
    Some(local_dir()?.join("models").join(file.file))
}

/// The folder an engine build is unpacked into, named after its archive.
fn engine_dir(engine: &Download) -> Option<PathBuf> {
    let name = engine.file.trim_end_matches(".zip");
    Some(local_dir()?.join("engines").join(name))
}

/// `path` with `suffix` added to its last component.
fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// The path of an engine program, if that engine build is installed.
#[must_use]
pub fn engine_program(gpu: bool, runner: Runner) -> Option<PathBuf> {
    program_in(engine(gpu), runner)
}

/// The engine program to run a model with right now: the build asked for
/// or, until that one is here, whichever build is. A model is the same file
/// for every build, so it never has to wait for an engine it does not need.
#[must_use]
pub fn usable_program(gpu: bool, runner: Runner) -> Option<PathBuf> {
    engine_program(gpu, runner).or_else(|| {
        [&ENGINE_CPU, &ENGINE_VULKAN, &ENGINE_CUDA]
            .into_iter()
            .find_map(|build| program_in(build, runner))
    })
}

/// Whether the file of any local model is on this PC.
#[must_use]
pub fn any_local_model() -> bool {
    all()
        .into_iter()
        .filter_map(|model| model.local.as_ref())
        .any(|(file, _)| model_path(file).is_some_and(|path| has_size(&path, file.source.bytes)))
}

/// The model to carry on with when an online one cannot serve: `preferred`
/// if it is among `installed`, otherwise the first of them, which in the
/// order of the catalog is the best.
pub fn pick<'a>(
    preferred: &str,
    installed: impl IntoIterator<Item = &'a Model>,
) -> Option<&'a Model> {
    let mut first = None;
    for model in installed {
        if model.id.eq_ignore_ascii_case(preferred) {
            return Some(model);
        }
        first = first.or(Some(model));
    }
    first
}

/// The model on this PC to carry on with, [`pick`]ed from the local models
/// that are installed; `None` if none is. Nothing is downloaded for it.
#[must_use]
pub fn spare(preferred: &str, gpu: bool) -> Option<&'static Model> {
    let local = all().into_iter().filter(|model| model.local.is_some());
    pick(preferred, local.filter(|model| is_installed(model, gpu)))
}

/// The path of a program of an engine build, if that build is installed.
fn program_in(build: &Download, runner: Runner) -> Option<PathBuf> {
    let dir = engine_dir(build)?;
    // The archive holds everything in one folder; accept it unpacked flat too.
    [dir.join("Release"), dir]
        .into_iter()
        .map(|folder| folder.join(runner.program()))
        .find(|program| program.is_file())
}

fn has_size(path: &Path, bytes: u64) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.len() == bytes)
}

/// Whether everything a model needs is on disk. Always true for online models.
#[must_use]
pub fn is_installed(model: &Model, gpu: bool) -> bool {
    let Some((file, runner)) = &model.local else {
        return true;
    };
    model_path(file).is_some_and(|path| has_size(&path, file.source.bytes))
        && usable_program(gpu, *runner).is_some()
}

/// How many bytes installing a model would still download.
#[must_use]
pub fn download_size(model: &Model, gpu: bool) -> u64 {
    let Some((file, runner)) = &model.local else {
        return 0;
    };
    let model_bytes = if model_path(file).is_some_and(|path| has_size(&path, file.source.bytes)) {
        0
    } else {
        file.source.bytes
    };
    let engine_bytes = if usable_program(gpu, *runner).is_some() {
        0
    } else {
        engine(gpu).source.bytes
    };
    model_bytes + engine_bytes
}

/// How many bytes the downloaded models and engines take up on this PC.
#[must_use]
pub fn disk_usage() -> u64 {
    fn size_of(path: &Path) -> u64 {
        let Ok(entries) = std::fs::read_dir(path) else {
            return 0;
        };
        entries
            .flatten()
            .map(|entry| match entry.metadata() {
                Ok(meta) if meta.is_dir() => size_of(&entry.path()),
                Ok(meta) => meta.len(),
                Err(_) => 0,
            })
            .sum()
    }
    local_dir().map_or(0, |dir| {
        size_of(&dir.join("models")) + size_of(&dir.join("engines"))
    })
}

/// Where a model stands, for the list in Settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// An online model: nothing to install.
    Online,
    /// A local model that is not installed.
    Missing {
        /// Bytes an install would download.
        bytes: u64,
    },
    /// Being downloaded and installed.
    Installing {
        /// Bytes on disk so far.
        done: u64,
        /// Bytes in total.
        total: u64,
    },
    /// Installed and ready.
    Installed,
    /// The last install failed.
    Failed(String),
}

#[derive(Debug, Default, Clone)]
struct Job {
    done: u64,
    total: u64,
    running: bool,
    stop: bool,
    error: Option<String>,
}

type Jobs = Arc<Mutex<HashMap<&'static str, Job>>>;

/// The key under which the download of an engine on its own is kept among
/// the jobs. No model has this id.
const ENGINE_JOB: &str = "engine";

/// Where the engine build that the settings ask for stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineState {
    /// It is installed.
    Ready,
    /// It is not installed, and nothing is fetching it.
    Missing,
    /// It is being downloaded.
    Fetching {
        /// Bytes on disk so far.
        done: u64,
        /// Bytes in total.
        total: u64,
    },
    /// The last download failed.
    Failed(String),
}

/// Installs local models in the background and reports how far along they are.
#[derive(Default, Clone)]
pub struct Installer {
    jobs: Jobs,
    /// Held while the shared engine is installed, so two models installed at
    /// once do not both unpack it.
    engine: Arc<Mutex<()>>,
    /// Called when an engine fetched on its own has been installed.
    engine_ready: Arc<Mutex<Option<Notify>>>,
}

type Notify = Box<dyn Fn() + Send>;

impl std::fmt::Debug for Installer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Installer")
            .field("busy", &self.is_busy())
            .finish_non_exhaustive()
    }
}

fn locked(jobs: &Jobs) -> std::sync::MutexGuard<'_, HashMap<&'static str, Job>> {
    jobs.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Installer {
    /// Creates an installer with nothing in progress.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Where `model` stands right now.
    #[must_use]
    pub fn status(&self, model: &'static Model, gpu: bool) -> Status {
        if model.local.is_none() {
            return Status::Online;
        }
        if let Some(job) = locked(&self.jobs).get(model.id) {
            if job.running {
                return Status::Installing {
                    done: job.done,
                    total: job.total,
                };
            }
            if let Some(error) = &job.error {
                return Status::Failed(error.clone());
            }
        }
        if is_installed(model, gpu) {
            Status::Installed
        } else {
            Status::Missing {
                bytes: download_size(model, gpu),
            }
        }
    }

    /// Where the engine build for `gpu` stands.
    #[must_use]
    pub fn engine_state(&self, gpu: bool) -> EngineState {
        if engine_program(gpu, Runner::Server).is_some() {
            return EngineState::Ready;
        }
        match locked(&self.jobs).get(ENGINE_JOB) {
            Some(job) if job.running => EngineState::Fetching {
                done: job.done,
                total: job.total,
            },
            Some(Job {
                error: Some(error), ..
            }) => EngineState::Failed(error.clone()),
            _ => EngineState::Missing,
        }
    }

    /// Sets what is called, on another thread, when an engine build fetched
    /// by [`ensure_engine`](Self::ensure_engine) has been installed.
    pub fn on_engine_ready(&self, notify: impl Fn() + Send + 'static) {
        *self
            .engine_ready
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Box::new(notify));
    }

    /// Downloads the engine build for `gpu` in the background if it is not
    /// installed. Models keep running on the build that is here in the
    /// meantime.
    pub fn ensure_engine(&self, gpu: bool) {
        if engine_program(gpu, Runner::Server).is_some() {
            return;
        }
        {
            let mut jobs = locked(&self.jobs);
            if jobs.get(ENGINE_JOB).is_some_and(|job| job.running) {
                return;
            }
            jobs.insert(
                ENGINE_JOB,
                Job {
                    total: engine(gpu).source.bytes,
                    running: true,
                    ..Job::default()
                },
            );
        }
        let (jobs, lock) = (Arc::clone(&self.jobs), Arc::clone(&self.engine));
        let ready = Arc::clone(&self.engine_ready);
        let worker = Arc::clone(&jobs);
        let spawned = thread::Builder::new()
            .name("vinput-engine".into())
            .spawn(move || {
                let result = fetch_engine(gpu, Runner::Server, &lock, &mut |bytes| {
                    locked(&worker).get_mut(ENGINE_JOB).is_some_and(|job| {
                        job.done = bytes;
                        !job.stop
                    })
                });
                let fetched = result.is_ok();
                if let Some(job) = locked(&worker).get_mut(ENGINE_JOB) {
                    job.running = false;
                    job.error = result.err().map(|err| {
                        log::write(&format!("installing the engine: {err}"));
                        err.to_string()
                    });
                }
                if fetched
                    && let Some(notify) = &*ready.lock().unwrap_or_else(PoisonError::into_inner)
                {
                    notify();
                }
            });
        if spawned.is_err() {
            locked(&jobs).remove(ENGINE_JOB);
        }
    }

    /// Whether any install is running.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        locked(&self.jobs).values().any(|job| job.running)
    }

    /// Starts installing `model` unless that is already happening. Returns at
    /// once; follow it with [`status`](Self::status).
    pub fn install(&self, model: &'static Model, gpu: bool) {
        {
            let mut jobs = locked(&self.jobs);
            if jobs.get(model.id).is_some_and(|job| job.running) {
                return;
            }
            jobs.insert(
                model.id,
                Job {
                    total: download_size(model, gpu),
                    running: true,
                    ..Job::default()
                },
            );
        }
        let (jobs, engine) = (Arc::clone(&self.jobs), Arc::clone(&self.engine));
        let finish = move |jobs: &Jobs, result: Result<()>| {
            if let Some(job) = locked(jobs).get_mut(model.id) {
                job.running = false;
                // Stopping is what the user asked for, not a failure to show.
                job.error = result.err().filter(|_| !job.stop).map(|err| {
                    log::write(&format!("installing {}: {err}", model.id));
                    err.to_string()
                });
            }
        };
        let worker = Arc::clone(&jobs);
        let spawned = thread::Builder::new()
            .name("vinput-install".into())
            .spawn(move || {
                let result = install(model, gpu, &worker, &engine);
                finish(&worker, result);
            });
        if let Err(err) = spawned
            && let Some(job) = locked(&jobs).get_mut(model.id)
        {
            job.running = false;
            job.error = Some(format!("could not start the install: {err}"));
        }
    }

    /// Stops a running install of `model`. What was downloaded is kept, and a
    /// later install continues from it.
    pub fn stop(&self, model: &Model) {
        if let Some(job) = locked(&self.jobs).get_mut(model.id) {
            job.stop = true;
        }
    }

    /// Deletes an installed model's file, and takes a model someone added
    /// off the list. The engine stays for the others.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Install`] if the file exists and cannot be deleted,
    /// which is the case while the model is in use.
    pub fn remove(&self, model: &Model) -> Result<()> {
        locked(&self.jobs).remove(model.id);
        let Some(path) = model.local.as_ref().and_then(|(file, _)| model_path(file)) else {
            return Ok(());
        };
        let _ = std::fs::remove_file(download::partial(&path));
        match std::fs::remove_file(&path) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                return Err(Error::Install(format!(
                    "could not delete {}: {err}. Pick another model, save, and try again.",
                    path.display()
                )));
            }
            _ => {}
        }
        // A model someone added goes from the list with its file.
        if model.is_custom() {
            custom::forget(model.id)?;
        }
        Ok(())
    }
}

/// Downloads and unpacks whatever `model` is missing.
fn install(model: &Model, gpu: bool, jobs: &Jobs, engine_lock: &Mutex<()>) -> Result<()> {
    let Some((file, runner)) = &model.local else {
        return Ok(());
    };
    let no_folder = || Error::Install("LOCALAPPDATA is not set".into());
    // Bytes finished by earlier steps, added to the progress of the current one.
    let mut before = 0u64;
    let report = |before: u64, bytes: u64| {
        let mut jobs = locked(jobs);
        let Some(job) = jobs.get_mut(model.id) else {
            return false;
        };
        job.done = before + bytes;
        !job.stop
    };

    // With another build to run on, the model does not wait for the build
    // the settings ask for; that one is fetched on its own.
    if usable_program(gpu, *runner).is_none() {
        fetch_engine(gpu, *runner, engine_lock, &mut |bytes| {
            report(before, bytes)
        })?;
        before += engine(gpu).source.bytes;
    }

    let path = model_path(file).ok_or_else(no_folder)?;
    if !has_size(&path, file.source.bytes) {
        download::fetch(file.source, &path, &mut |bytes| report(before, bytes))?;
        // An added model may have no published checksum; its first bytes
        // must still be those of a Whisper model.
        if model.is_custom()
            && let Err(why) = ggml::inspect_file(&path)
        {
            let _ = std::fs::remove_file(&path);
            return Err(Error::Install(why.to_string()));
        }
    }
    Ok(())
}

/// Downloads and unpacks the engine build for `gpu`, unless it is here.
/// `progress` is told the bytes downloaded so far and returns whether to go on.
fn fetch_engine(
    gpu: bool,
    runner: Runner,
    engine_lock: &Mutex<()>,
    progress: &mut dyn FnMut(u64) -> bool,
) -> Result<()> {
    let _one_at_a_time = engine_lock.lock().unwrap_or_else(PoisonError::into_inner);
    if engine_program(gpu, runner).is_some() {
        return Ok(());
    }
    let build = engine(gpu);
    let dir = engine_dir(build).ok_or_else(|| Error::Install("LOCALAPPDATA is not set".into()))?;
    let archive = suffixed(&dir, ".zip");
    download::fetch(build.source, &archive, progress)?;
    unpack(&archive, &dir)?;
    let _ = std::fs::remove_file(&archive);
    if engine_program(gpu, runner).is_none() {
        return Err(Error::Install(format!(
            "the engine archive does not contain {}",
            runner.program()
        )));
    }
    Ok(())
}

/// Unpacks a zip archive into `dir` with the `tar` that ships with Windows.
/// The files appear under `dir` only once all of them are unpacked.
fn unpack(archive: &Path, dir: &Path) -> Result<()> {
    let failed = |what: &str| Error::Install(format!("unpacking the engine failed: {what}"));
    let staging = suffixed(dir, ".unpacking");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|err| failed(&err.to_string()))?;
    let system = std::env::var_os("SystemRoot").map_or_else(
        || PathBuf::from("tar.exe"),
        |root| PathBuf::from(root).join("System32").join("tar.exe"),
    );
    let mut command = Command::new(system);
    command.arg("-xf").arg(archive).arg("-C").arg(&staging);
    hide_console(&mut command);
    let output = command.output().map_err(|err| failed(&err.to_string()))?;
    if !output.status.success() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(failed(String::from_utf8_lossy(&output.stderr).trim()));
    }
    let _ = std::fs::remove_dir_all(dir);
    std::fs::rename(&staging, dir).map_err(|err| failed(&err.to_string()))
}

/// Keeps a console program started from the background app from flashing a
/// console window on screen.
pub fn hide_console(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        /// `CREATE_NO_WINDOW`.
        const NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

/// A size for people: `148 MB`, `1.6 GB`.
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    reason = "a size shown to one decimal place"
)]
pub fn human_size(bytes: u64) -> String {
    const MB: f64 = 1_000_000.0;
    let megabytes = bytes as f64 / MB;
    if megabytes >= 1000.0 {
        format!("{:.1} GB", megabytes / 1000.0)
    } else {
        format!("{megabytes:.0} MB")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_is_consistent() {
        for (index, model) in CATALOG.iter().enumerate() {
            assert!(
                CATALOG[..index].iter().all(|other| other.id != model.id),
                "{} is listed twice",
                model.id
            );
            assert_eq!(find(model.provider, model.id), Some(model));
            assert_eq!(model.local.is_some(), model.provider == ProviderKind::Local);
            assert!(!model.is_custom(), "{} looks like an added model", model.id);
            if let Some((file, _)) = &model.local {
                assert!(
                    file.source.url.starts_with("https://") && file.source.url.ends_with(file.file)
                );
                assert_eq!(file.source.sha256.len(), 64);
                assert!(file.source.bytes > 1_000_000);
            }
        }
        for build in [&ENGINE_CPU, &ENGINE_VULKAN, &ENGINE_CUDA] {
            assert!(build.source.url.starts_with("https://"));
            assert!(build.source.url.ends_with(build.file));
            assert_eq!(build.source.sha256.len(), 64);
        }
    }

    #[test]
    fn online_models_need_no_install() {
        let installer = Installer::new();
        let online = &CATALOG[0];
        assert!(is_installed(online, false));
        assert_eq!(download_size(online, true), 0);
        assert_eq!(installer.status(online, false), Status::Online);
        assert!(!installer.is_busy());
    }

    #[test]
    fn the_model_in_use_has_a_name_unless_none_is_chosen() {
        assert_eq!(
            name_in_use(ProviderKind::Local, "whisper-base"),
            Some("Whisper base")
        );
        assert_eq!(
            name_in_use(ProviderKind::GoogleCloud, "chirp_2"),
            Some("chirp_2")
        );
        assert_eq!(name_in_use(ProviderKind::Local, ""), None);
    }

    #[test]
    fn sizes_read_naturally() {
        assert_eq!(human_size(147_951_465), "148 MB");
        assert_eq!(human_size(1_624_555_275), "1.6 GB");
    }

    #[cfg(windows)]
    #[test]
    fn an_archive_is_unpacked_into_its_folder() -> std::io::Result<()> {
        let root = std::env::temp_dir().join(format!("vinput-unpack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("in/Release"))?;
        std::fs::write(root.join("in/Release/tool.exe"), b"program")?;
        let archive = root.join("engine.zip");
        let packed = Command::new("tar")
            .arg("-a")
            .arg("-cf")
            .arg(&archive)
            .arg("-C")
            .arg(root.join("in"))
            .arg("Release")
            .status()?;
        assert!(packed.success());

        let dir = root.join("engine");
        let result = unpack(&archive, &dir);
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(std::fs::read(dir.join("Release/tool.exe"))?, b"program");
        assert!(!suffixed(&dir, ".unpacking").exists());
        std::fs::remove_dir_all(root)
    }
}
