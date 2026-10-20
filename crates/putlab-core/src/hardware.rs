//! What this PC has to run a local model with, and the three verdicts a
//! program can give a model for it.
//!
//! A verdict is a rule of thumb, drawn from what a model needs and three
//! facts about the PC: the memory of its graphics card, its own memory and
//! how many threads its processor runs. It is there so that nobody downloads
//! gigabytes to find out that the wait is half a minute, not to rank models
//! to the second. The rule itself belongs to the program whose models they
//! are: dictation and reading aloud ask different work of a PC.

use std::sync::OnceLock;

use crate::gpu;

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
    /// As quick as it can be.
    Well,
    /// It works, with a wait.
    Slow,
    /// Too little memory for it, or a wait nobody would put up with.
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
    #[cfg(windows)]
    #[test]
    fn this_pc_can_be_looked_over() {
        let found = super::pc().copied();
        assert!(found.is_some_and(|pc| pc.memory > 1_000_000_000 && pc.threads > 0));
    }
}
