//! What this PC has to run a local model with, and how well a model of the
//! catalog would run on it.
//!
//! The verdict is a rule of thumb, drawn from the size of a model, the kind
//! of work it is and three facts about the PC: the memory of its graphics
//! card, its own memory and how many threads its processor runs. It is there
//! so that nobody downloads gigabytes to find out that dictation takes half
//! a minute, not to rank models to the second.

use std::sync::OnceLock;

use crate::gpu;
use crate::models::{Model, Runner};

/// The parts of a PC that decide how a local model runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pc {
    /// Memory of the graphics card local models can run on, in bytes; zero
    /// without one.
    pub card_memory: u64,
    /// Memory of the PC, in bytes.
    pub memory: u64,
    /// Threads the processor runs at once.
    pub threads: usize,
}

/// How a local model would run on a PC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// Text arrives about as soon as it can.
    Well,
    /// It works, with a wait after each dictation.
    Slow,
    /// Too little memory for it, or a wait nobody would dictate with.
    Heavy,
}

/// How much work a model is for a processor, whatever its file size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Effort {
    Light,
    Medium,
    Heavy,
}

/// This PC. `None` if Windows cannot say how much memory it has. Looked up
/// once.
#[must_use]
pub fn pc() -> Option<&'static Pc> {
    static PC: OnceLock<Option<Pc>> = OnceLock::new();
    PC.get_or_init(|| {
        let memory = platform::memory()?;
        Some(Pc {
            card_memory: gpu::card().map_or(0, |card| card.memory),
            memory,
            threads: std::thread::available_parallelism().map_or(1, usize::from),
        })
    })
    .as_ref()
}

const MB: u64 = 1_000_000;

/// The memory a model takes while it runs: its files, and room to work in.
fn needs(model: &Model) -> u64 {
    model.bytes() / 10 * 13 + 400 * MB
}

/// The work of a model. Whisper reads half a minute of sound whatever was
/// said, so its larger models are the heaviest here, compact or not; the
/// others read only what was said.
fn effort(model: &Model, runner: Runner) -> Effort {
    let bytes = model.bytes();
    match runner {
        Runner::Server if bytes < 200 * MB => Effort::Light,
        Runner::Server if bytes < 520 * MB => Effort::Medium,
        Runner::Server => Effort::Heavy,
        Runner::Listener(_) if bytes < 1_500 * MB => Effort::Light,
        Runner::Command | Runner::Listener(_) => Effort::Medium,
    }
}

/// How `model` would run on `pc`, with the graphics card if `card` says
/// local models are to use it. `None` for an online model.
#[must_use]
pub fn fit(model: &Model, pc: &Pc, card: bool) -> Option<Fit> {
    let (_, runner) = model.local.as_ref()?;
    let needs = needs(model);
    // A graphics card that holds the whole model is quick with any of them.
    if card && pc.card_memory >= needs {
        return Some(Fit::Well);
    }
    // On the processor the model shares the memory with everything else.
    if needs > pc.memory / 2 {
        return Some(Fit::Heavy);
    }
    Some(match (effort(model, *runner), pc.threads) {
        (Effort::Light, 4..) | (Effort::Medium, 8..) => Fit::Well,
        (Effort::Light, _) | (Effort::Medium, 4..) | (Effort::Heavy, 12..) => Fit::Slow,
        (Effort::Medium | Effort::Heavy, _) => Fit::Heavy,
    })
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod platform {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    /// The memory of the PC, in bytes.
    pub(super) fn memory() -> Option<u64> {
        // SAFETY: the structure is plain numbers and says how large it is,
        // as the call asks.
        unsafe {
            let mut status: MEMORYSTATUSEX = std::mem::zeroed();
            status.dwLength = u32::try_from(size_of::<MEMORYSTATUSEX>()).unwrap_or(0);
            (GlobalMemoryStatusEx(&raw mut status) != 0).then_some(status.ullTotalPhys)
        }
    }
}

#[cfg(not(windows))]
mod platform {
    pub(super) fn memory() -> Option<u64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{CATALOG, by_id};

    const GB: u64 = 1000 * MB;

    fn pc(card_gb: u64, memory_gb: u64, threads: usize) -> Pc {
        Pc {
            card_memory: card_gb * GB,
            memory: memory_gb * GB,
            threads,
        }
    }

    fn fits(pc: &Pc, card: bool) -> Vec<(&'static str, Fit)> {
        let local = CATALOG.iter().filter_map(|model| {
            let fit = fit(model, pc, card)?;
            Some((model.id, fit))
        });
        local.collect()
    }

    fn of(id: &str, pc: &Pc, card: bool) -> Option<Fit> {
        fit(by_id(id)?, pc, card)
    }

    #[cfg(windows)]
    #[test]
    fn this_pc_can_be_looked_over() {
        let found = super::pc().copied().unwrap_or(pc(0, 0, 0));
        assert!(found.memory > GB && found.threads > 0);
    }

    #[test]
    fn an_online_model_has_no_verdict() {
        let pc = pc(24, 64, 32);
        let online = CATALOG.iter().filter(|model| model.local.is_none());
        assert!(online.count() > 0);
        for model in CATALOG.iter().filter(|model| model.local.is_none()) {
            assert_eq!(fit(model, &pc, true), None);
        }
    }

    #[test]
    fn a_large_graphics_card_runs_every_model_well() {
        let all = fits(&pc(24, 64, 32), true);
        assert_eq!(all.len(), 6);
        assert!(all.iter().all(|(_, fit)| *fit == Fit::Well));
        // Four gigabytes hold the largest of the catalog too.
        assert!(
            fits(&pc(4, 8, 4), true)
                .iter()
                .all(|(_, fit)| *fit == Fit::Well)
        );
    }

    #[test]
    fn the_graphics_card_counts_only_while_it_is_to_be_used() {
        let pc = pc(24, 16, 8);
        assert_eq!(of("whisper-large-v3-turbo", &pc, true), Some(Fit::Well));
        assert_eq!(of("whisper-large-v3-turbo", &pc, false), Some(Fit::Heavy));
    }

    #[test]
    fn a_card_too_small_for_a_model_leaves_it_to_the_processor() {
        // Two gigabytes hold the small models, not the large ones.
        let pc = pc(2, 16, 16);
        assert_eq!(of("whisper-base", &pc, true), Some(Fit::Well));
        assert_eq!(of("whisper-large-v3-turbo-q5", &pc, true), Some(Fit::Well));
        assert_eq!(of("whisper-large-v3-turbo", &pc, true), Some(Fit::Slow));
        assert_eq!(of("qwen3-asr-1.7b", &pc, true), Some(Fit::Well));
    }

    #[test]
    fn a_desktop_without_a_card_runs_the_light_models_well() {
        let pc = pc(0, 16, 12);
        let expected = [
            ("qwen3-asr-1.7b", Fit::Well),
            ("qwen3-asr-0.6b", Fit::Well),
            ("parakeet-tdt-0.6b-v3", Fit::Well),
            ("whisper-large-v3-turbo", Fit::Slow),
            ("whisper-large-v3-turbo-q5", Fit::Slow),
            ("whisper-base", Fit::Well),
        ];
        assert_eq!(fits(&pc, true), expected);
    }

    #[test]
    fn a_small_laptop_is_told_what_is_too_much_for_it() {
        let pc = pc(0, 8, 4);
        let expected = [
            ("qwen3-asr-1.7b", Fit::Slow),
            ("qwen3-asr-0.6b", Fit::Well),
            ("parakeet-tdt-0.6b-v3", Fit::Slow),
            ("whisper-large-v3-turbo", Fit::Heavy),
            ("whisper-large-v3-turbo-q5", Fit::Heavy),
            ("whisper-base", Fit::Well),
        ];
        assert_eq!(fits(&pc, true), expected);
    }

    #[test]
    fn nothing_runs_well_on_a_pc_with_two_threads() {
        let all = fits(&pc(0, 4, 2), true);
        assert!(all.iter().all(|(_, fit)| *fit != Fit::Well));
        assert_eq!(of("whisper-base", &pc(0, 4, 2), true), Some(Fit::Slow));
        // Too little memory, however many threads.
        assert_eq!(of("qwen3-asr-1.7b", &pc(0, 4, 16), true), Some(Fit::Heavy));
    }
}
