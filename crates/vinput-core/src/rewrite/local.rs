//! Rewriting with a model on this PC.
//!
//! The models are the ones the programs of the family share
//! (`putlab_core::llm::models`): one file each, downloaded and checked like
//! a speech model ([`crate::models::Installer`]), and run in the model
//! engine, the same `llama-server` the speech models of two files run in.
//! The engine and how it is made ready are shared too
//! (`putlab_core::engine`): the model is loaded, and the graphics card made
//! ready for it, before anyone waits for a word, which is when the settings
//! first ask for it and again at every key press that will need it. It runs
//! on the graphics card whenever the PC has one the user has not switched
//! off, and is let go of half an hour after it was last asked anything.
//!
//! Nothing leaves this PC on this path.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;

use putlab_core::engine::{Engine, IDLE_FOR};
pub use putlab_core::engine::{Standing, Step};
use putlab_core::hardware::{Fit, Pc};
use putlab_core::llm::models::text_model;
use putlab_core::llm::{Ask, Chat, Failed};

use super::{Job, Rewriter};
use crate::models::{self, Model};
use crate::{Error, Result, gpu};

/// The model engine of this process. There is one, with one model in it.
#[must_use]
pub fn engine() -> Arc<Engine> {
    static ENGINE: OnceLock<Arc<Engine>> = OnceLock::new();
    Arc::clone(ENGINE.get_or_init(|| Arc::new(Engine::new())))
}

/// How long `model` would take over a dictation of a minute on `pc`, in
/// seconds, with the graphics card if `card` says models are to use it:
/// some two hundred tokens to read and as many to write. `None` for a model
/// that is not one of these, and for one this PC has too little memory for.
#[must_use]
pub fn seconds_a_minute(model: &Model, pc: &Pc, card: bool) -> Option<u64> {
    text_model(model.id)?.seconds_for(200, 200, pc, card)
}

/// How `model` would do on `pc`: well if the wait is hardly one, slow if it
/// is one worth knowing of before downloading gigabytes, and heavy if it
/// is longer than anyone would wait after every dictation, or the PC has
/// too little memory. `None` for a model that is not one of these.
#[must_use]
pub fn fit(model: &Model, pc: &Pc, card: bool) -> Option<Fit> {
    text_model(model.id)?;
    Some(match seconds_a_minute(model, pc, card) {
        Some(..=3) => Fit::Well,
        Some(..=20) => Fit::Slow,
        Some(_) | None => Fit::Heavy,
    })
}

/// Where a model that is installed is run from: the engine's program, the
/// file of the model, and the name of the graphics card to run it on, if
/// it is to run on one.
type Place = (PathBuf, PathBuf, Option<String>);

/// A model on this PC.
#[derive(Debug)]
pub(super) struct Local {
    /// The model the settings name, if it is one of these.
    model: Option<&'static Model>,
    /// Whether models are to run on the graphics card.
    gpu: bool,
    engine: Arc<Engine>,
}

impl Local {
    /// The model the settings name as `id`, to run on the graphics card if
    /// `gpu` says so.
    pub(super) fn chosen(id: &str, gpu: bool) -> Self {
        Self {
            model: models::writer(id),
            gpu,
            engine: engine(),
        }
    }

    /// Where the model is run from, if all of it is on this PC.
    fn place(&self) -> Result<Place> {
        let missing = || {
            Error::Setup(
                "The text model is not on this PC. Download it under Cleanup, or choose another."
                    .to_owned(),
            )
        };
        let model = self.model.ok_or_else(missing)?;
        let (file, runner) = model.local.as_ref().ok_or_else(missing)?;
        if !models::is_installed(model, self.gpu) {
            return Err(missing());
        }
        let program = models::usable_program(self.gpu, *runner).ok_or_else(missing)?;
        let file = models::model_path(file).ok_or_else(missing)?;
        let card = gpu::card()
            .filter(|_| self.gpu)
            .map(|card| card.name.clone());
        Ok((program, file, card))
    }
}

/// Lets the engine go when it has not been asked anything for
/// [`IDLE_FOR`], to free the memory the model takes. One thread does that
/// for as long as an engine runs, asleep until the time has passed; with
/// no engine there is no thread.
fn let_go_when_idle(engine: &Arc<Engine>) {
    static WATCHING: AtomicBool = AtomicBool::new(false);
    if WATCHING.swap(true, Ordering::AcqRel) {
        return;
    }
    let engine = Arc::clone(engine);
    let spawned = thread::Builder::new()
        .name("vinput-text-idle".into())
        .spawn(move || {
            loop {
                thread::sleep(IDLE_FOR);
                if !engine.let_go_if_idle(IDLE_FOR) {
                    WATCHING.store(false, Ordering::Release);
                    return;
                }
            }
        });
    if spawned.is_err() {
        WATCHING.store(false, Ordering::Release);
    }
}

impl Rewriter for Local {
    fn rewrite(&self, job: &Job<'_>, stop: &AtomicBool, wrote: &mut dyn FnMut(&str)) -> Result<()> {
        let (program, file, card) = self.place()?;
        let (base, key) = self
            .engine
            .serve(&program, &file, card.as_deref(), stop)
            .map_err(Error::Rewrite)?;
        let_go_when_idle(&self.engine);
        let material = job.material();
        let ask = Ask {
            system: job.kind.instructions(),
            user: &material,
            temperature: job.kind.temperature(),
            max_tokens: job.max_tokens(),
        };
        let failed = |failed: Failed| {
            Error::Rewrite(
                match failed {
                    Failed::Unreachable => "The text model on this PC could not be reached.",
                    Failed::Status(_) => "The text model on this PC answered with an error.",
                    Failed::Stalled => "The text model on this PC stopped answering.",
                    Failed::Nothing => "The text model on this PC wrote nothing.",
                }
                .to_owned(),
            )
        };
        Chat::engine(&base, &key)
            .write(&ask, stop, wrote)
            .map_err(failed)
    }

    fn make_ready(&self) {
        if let Ok((program, file, card)) = self.place() {
            self.engine.prepare(&program, &file, card.as_deref());
            let_go_when_idle(&self.engine);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rewrite::Kind;

    const GB: u64 = 1_000_000_000;

    fn pc(card_gb: u64, memory_gb: u64, threads: usize) -> Pc {
        Pc {
            card_memory: card_gb * GB,
            memory: memory_gb * GB,
            threads,
        }
    }

    #[test]
    fn a_model_runs_well_on_a_card_that_holds_it_and_slowly_on_a_processor() {
        let fits = |pc: &Pc, card| -> Vec<Fit> {
            let all = models::writers().iter();
            all.filter_map(|model| fit(model, pc, card)).collect()
        };
        assert_eq!(fits(&pc(12, 16, 8), true), [Fit::Well; 4]);
        // Without the card, or with it switched off, the wait is one to
        // know of; the large models are more than anyone would wait.
        let ordinary = fits(&pc(0, 16, 8), false);
        assert_eq!(ordinary, fits(&pc(12, 16, 8), false));
        assert!(ordinary.iter().all(|fit| *fit != Fit::Well));
        assert!(ordinary.contains(&Fit::Slow));
        // Too little memory for any of them.
        assert_eq!(fits(&pc(0, 3, 8), false), [Fit::Heavy; 4]);
        // A speech model is none of these.
        let speech = models::CATALOG.iter().find(|model| model.local.is_some());
        assert!(speech.is_some_and(|model| fit(model, &pc(12, 16, 8), true).is_none()));
    }

    #[test]
    fn a_model_that_is_not_on_this_pc_says_where_to_get_it() {
        for id in ["no-such-model", "gemma-4-e4b"] {
            let local = Local {
                model: models::writer(id),
                gpu: false,
                engine: Arc::new(Engine::new()),
            };
            // Whether a model of the catalog is on the PC that runs the
            // tests is not for a test to say; one that is, is not asked.
            if local.place().is_ok() {
                continue;
            }
            let job = Job::tidy(Kind::Clean, "hello", &[]);
            let stop = AtomicBool::new(false);
            let result = local.rewrite(&job, &stop, &mut |_| {});
            assert!(
                matches!(&result, Err(Error::Setup(why)) if why.contains("Cleanup")),
                "{result:?}"
            );
            // Nothing is started for it either.
            local.make_ready();
            assert!(!local.engine.is_running());
        }
    }
}
