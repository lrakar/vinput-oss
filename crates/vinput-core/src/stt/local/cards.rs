//! Which graphics card an engine is told to run on.
//!
//! An engine left to itself takes the first card Windows hands it. On a PC
//! with one card that is the card. On one with two, a laptop with graphics
//! built into its processor and a card beside them, the first is whichever
//! drives the screen, which is often the weaker of the two, and Settings
//! would name the stronger one while the model ran on the other.
//!
//! So the engine is asked which cards it sees, each with the number it
//! knows it by, and told the number of the one Vinput means
//! ([`crate::gpu::card`], the card with the most memory of its own). Asking
//! is one run of the engine program that loads no model and answers in well
//! under a second; it is done when an engine is first started, never at a
//! key press, and once for each engine program.
//!
//! The two engines list their cards differently. The first prints, before
//! anything else it says:
//!
//! ```text
//! ggml_vulkan: 0 = Some Card 9000 (Maker) | uma: 0 | fp16: 1 | ...
//! ggml_vulkan: 1 = Graphics In The Processor | uma: 1 | fp16: 1 | ...
//! ```
//!
//! where `uma: 1` marks graphics that share the PC's memory. The second
//! answers `--list-devices` with:
//!
//! ```text
//!   Vulkan0: Some Card 9000 (24000 MiB, 23000 MiB free)
//! ```

//!
//! # Whether it did
//!
//! An engine that is told a card and cannot use it does not fail: it runs
//! the model on the processor and says so only in what it prints, which
//! nobody reads. Settings would go on naming the card. So two things are
//! noted, and [`fell_back`] says whether either was last seen: an engine
//! that lists no card at all, and the first engine saying, as it loads a
//! model, that it found no card to use ([`heed`]). The second engine and
//! the programs that run once a dictation do not say it in a form that can
//! be relied on; of them only the first of the two is known.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread;

use crate::log;
use crate::models::hide_console;

/// Whether the engine last started for a graphics card runs its model on
/// the processor instead.
static ON_THE_PROCESSOR: AtomicBool = AtomicBool::new(false);

/// Whether a model that was to run on the graphics card runs on the
/// processor instead, as far as its engine has let on. What Settings show
/// follows this; nothing else does.
#[must_use]
pub fn fell_back() -> bool {
    ON_THE_PROCESSOR.load(Ordering::Relaxed)
}

/// Notes where the engine that was started for a card runs its model, and
/// says so in the log the first time it is the processor.
fn note(on_the_processor: bool, program: &Path) {
    if on_the_processor && !ON_THE_PROCESSOR.swap(true, Ordering::Relaxed) {
        log::write(&format!(
            "{} cannot use the graphics card; the model runs on the processor",
            program.display()
        ));
    } else if !on_the_processor {
        ON_THE_PROCESSOR.store(false, Ordering::Relaxed);
    }
}

/// What the first engine says of the card as it loads a model, if `line` is
/// where it says it: `Some(true)` for a card it uses, `Some(false)` for
/// none found.
///
/// ```text
/// whisper_backend_init_gpu: using Vulkan0 backend
/// whisper_backend_init_gpu: no GPU found
/// ```
pub(super) fn uses_a_card(line: &str) -> Option<bool> {
    let said = line
        .trim()
        .strip_prefix("whisper_backend_init_gpu:")?
        .trim();
    if said.starts_with("using ") {
        Some(true)
    } else if said.starts_with("no GPU found") {
        Some(false)
    } else {
        None
    }
}

/// Reads what the first engine, started for a card, prints for as long as
/// it runs, on a thread of its own: notes what it says of the card, and
/// lets the rest go. Nothing of it is kept, in memory or anywhere else; it
/// is read to its end because an engine that nobody listens to stops when
/// what it has to say is full.
pub(super) fn heed(printed: impl Read + Send + 'static, program: &Path) {
    let program = program.to_owned();
    let listen = move || {
        let mut printed = BufReader::new(printed);
        let mut line = Vec::new();
        loop {
            line.clear();
            match printed.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            if let Some(card) = uses_a_card(&String::from_utf8_lossy(&line)) {
                note(!card, &program);
            }
        }
    };
    // Without the thread nothing is noted, and nothing is lost but that.
    let _ = thread::Builder::new()
        .name("vinput-engine-says".to_owned())
        .spawn(listen);
}

/// A card as an engine lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Listed {
    /// The number the engine knows it by.
    pub(super) number: u32,
    /// Its name, and whatever the engine writes behind it.
    pub(super) name: String,
    /// Whether it shares the PC's memory, where the engine says.
    pub(super) built_in: bool,
}

/// The cards in what an engine printed, in either form.
pub(super) fn listed(printed: &str) -> Vec<Listed> {
    printed.lines().filter_map(line).collect()
}

/// One line of a list, if it is one.
fn line(line: &str) -> Option<Listed> {
    let line = line.trim();
    if let Some(rest) = line.strip_prefix("ggml_vulkan:") {
        let (number, rest) = rest.split_once('=')?;
        let mut parts = rest.split('|').map(str::trim);
        let name = parts.next()?;
        return Some(Listed {
            number: number.trim().parse().ok()?,
            name: name.to_owned(),
            built_in: parts.any(|part| part == "uma: 1"),
        });
    }
    let (number, name) = line.strip_prefix("Vulkan")?.split_once(':')?;
    Some(Listed {
        number: number.parse().ok()?,
        name: name.trim().to_owned(),
        built_in: false,
    })
}

/// The number of the card to use, of those an engine listed: the one
/// Windows knows as `wanted` if the engine lists it, else the first that is
/// not built into the processor, else the first. `None` if it lists none.
pub(super) fn choose(listed: &[Listed], wanted: Option<&str>) -> Option<u32> {
    let wanted = wanted.map(str::to_lowercase);
    let named = |card: &&Listed| {
        wanted
            .as_deref()
            .is_some_and(|wanted| !wanted.is_empty() && card.name.to_lowercase().contains(wanted))
    };
    listed
        .iter()
        .find(named)
        .or_else(|| listed.iter().find(|card| !card.built_in))
        .or(listed.first())
        .map(|card| card.number)
}

/// What each engine program asked so far came to: the number of its card,
/// or that it lists none.
static ASKED: Mutex<Vec<(PathBuf, Option<u32>)>> = Mutex::new(Vec::new());

/// The number of the card the engine `program` is to run on. `second` says
/// that it is the second engine, which is asked in another way.
///
/// An engine that cannot be asked, or lists no card, is told the first one,
/// as it would have taken by itself, and is noted as one that runs its
/// model on the processor ([`fell_back`]): with no card to use, that is
/// what it does whatever it is told.
pub(super) fn number(program: &Path, second: bool) -> u32 {
    let found = {
        let mut asked = ASKED.lock().unwrap_or_else(PoisonError::into_inner);
        let known = asked.iter().find(|(known, _)| known == program);
        if let Some((_, found)) = known {
            *found
        } else {
            let wanted = crate::gpu::card().map(|card| card.name.as_str());
            let found = choose(&listed(&ask(program, second)), wanted);
            asked.push((program.to_owned(), found));
            found
        }
    };
    note(found.is_none(), program);
    found.unwrap_or(0)
}

/// Everything the engine `program` prints when asked for its cards.
fn ask(program: &Path, second: bool) -> String {
    let mut command = Command::new(program);
    command
        .arg(if second { "--list-devices" } else { "--help" })
        .stdin(Stdio::null());
    if let Some(folder) = program.parent() {
        command.current_dir(folder);
    }
    hide_console(&mut command);
    command.output().map_or_else(
        |_| String::new(),
        |output| {
            let mut printed = String::from_utf8_lossy(&output.stdout).into_owned();
            printed.push_str(&String::from_utf8_lossy(&output.stderr));
            printed
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIRST: &str = "\
ggml_vulkan: Found 3 Vulkan devices:
ggml_vulkan: 0 = Graphics In The Processor (Maker) | uma: 1 | fp16: 1 | bf16: 0 | warp size: 32
ggml_vulkan: 1 = Some Card 9000 (Other Maker) | uma: 0 | fp16: 1 | bf16: 1 | warp size: 32
ggml_vulkan: 2 = Another Card 500 | uma: 0 | fp16: 1 | bf16: 0 | warp size: 64

usage: whisper-server.exe [options]
  -dev N,    --device N                  [0      ] GPU device ID (default: 0)
";

    const SECOND: &str = "\
Available devices:
  Vulkan0: Graphics In The Processor (65338 MiB, 62071 MiB free)
  Vulkan1: Some Card 9000 (24540 MiB, 23448 MiB free)
";

    #[test]
    fn both_engines_lists_are_read() {
        let first = listed(FIRST);
        assert_eq!(first.len(), 3);
        assert_eq!(
            first[0],
            Listed {
                number: 0,
                name: "Graphics In The Processor (Maker)".into(),
                built_in: true,
            }
        );
        assert_eq!((first[2].number, first[2].built_in), (2, false));

        let second = listed(SECOND);
        assert_eq!(second.len(), 2);
        assert_eq!(second[1].number, 1);
        assert!(second[1].name.starts_with("Some Card 9000"));
        assert!(listed("usage: whisper-server.exe [options]\n").is_empty());
    }

    #[test]
    fn what_the_first_engine_says_of_the_card_is_heard_and_nothing_else() {
        assert_eq!(
            uses_a_card("whisper_backend_init_gpu: using Vulkan0 backend\n"),
            Some(true)
        );
        assert_eq!(
            uses_a_card("whisper_backend_init_gpu: no GPU found\r\n"),
            Some(false)
        );
        // What it says on the way to that, and everything else it prints.
        for other in [
            "whisper_backend_init_gpu: device 0: Vulkan0 (type: 1)",
            "whisper_backend_init_gpu: found GPU device 0: Vulkan0 (type: 1, cnt: 0)",
            "whisper_init_with_params_no_state: use gpu    = 1",
            "ggml_vulkan: 0 = Some Card 9000 | uma: 0 | fp16: 1",
            "",
        ] {
            assert_eq!(uses_a_card(other), None, "{other}");
        }
    }

    /// Waits a moment for the thread that listens to an engine to have
    /// noted what `fell_back` is to say.
    fn noted(fell_back_is: bool) -> bool {
        for _ in 0..200 {
            if fell_back() == fell_back_is {
                return true;
            }
            thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }

    // The one test that changes what `fell_back` says, so that no other can
    // be in its way.
    #[test]
    fn an_engine_that_says_it_found_no_card_is_known_to_run_on_the_processor() {
        let program = Path::new("whisper-server.exe");
        let without = "whisper_init_with_params_no_state: use gpu    = 1\n\
                       whisper_backend_init_gpu: no GPU found\n\
                       whisper_model_load: loading model\n";
        heed(std::io::Cursor::new(without.as_bytes().to_vec()), program);
        assert!(noted(true));
        // The next one that is started finds the card, and that is that.
        let with = "whisper_backend_init_gpu: using Vulkan0 backend\nnot text \u{fffd}\n";
        heed(std::io::Cursor::new(with.as_bytes().to_vec()), program);
        assert!(noted(false));
    }

    #[test]
    fn the_card_windows_means_is_the_one_chosen_wherever_it_is_listed() {
        // Not the first, which drives the screen.
        assert_eq!(choose(&listed(FIRST), Some("Some Card 9000")), Some(1));
        assert_eq!(choose(&listed(FIRST), Some("ANOTHER CARD 500")), Some(2));
        assert_eq!(choose(&listed(SECOND), Some("Some Card 9000")), Some(1));
    }

    #[test]
    fn a_card_the_engine_does_not_list_by_that_name_gives_way_to_a_real_one() {
        // Windows and the engine may name a card differently: then the
        // first that is not built into the processor,
        assert_eq!(choose(&listed(FIRST), Some("A Name Of Windows")), Some(1));
        assert_eq!(choose(&listed(FIRST), None), Some(1));
        // or, where the engine does not say which are, the first.
        assert_eq!(choose(&listed(SECOND), Some("A Name Of Windows")), Some(0));
        // Built-in graphics are still a card when there is no other.
        let only = &listed(FIRST)[..1];
        assert_eq!(choose(only, None), Some(0));
        assert_eq!(choose(&[], Some("Some Card 9000")), None);
    }

    /// Against engine programs on this PC, named in `VINPUT_TEST_ENGINES`
    /// with a `;` between them (a name with `llama` in it is the second
    /// engine): each lists the card Windows means by the name Windows gives
    /// it, and is told that one. Run with `--ignored` on a PC with a card.
    #[test]
    #[ignore = "needs engine programs and a graphics card"]
    fn the_engines_of_this_pc_list_the_card_windows_means() {
        let programs = std::env::var("VINPUT_TEST_ENGINES").unwrap_or_default();
        let wanted = crate::gpu::card().map(|card| card.name.to_lowercase());
        assert!(wanted.is_some(), "Windows lists no graphics card");
        let wanted = wanted.unwrap_or_default();
        let mut seen = 0;
        for program in programs.split(';').filter(|program| !program.is_empty()) {
            let second = program.to_lowercase().contains("llama");
            let cards = listed(&ask(Path::new(program), second));
            assert!(!cards.is_empty(), "{program} lists no card");
            let chosen = number(Path::new(program), second);
            let named = cards.iter().find(|card| card.number == chosen);
            assert!(
                named.is_some_and(|card| card.name.to_lowercase().contains(&wanted)),
                "{program} was told card {chosen} of {}, which is not the one Windows means",
                cards.len()
            );
            seen += 1;
        }
        assert!(seen > 0, "VINPUT_TEST_ENGINES names no program");
    }
}
