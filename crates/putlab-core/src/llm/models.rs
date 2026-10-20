//! The models that write and run on this PC, for the programs that offer
//! them: which they are, where their files come from, and how each would
//! do on a PC.
//!
//! A model is one file, pinned by address, size and SHA-256 like every
//! other download, and runs in the model engine ([`crate::engine`]). Every
//! file is pinned to one revision of its repository, so the bytes behind an
//! address cannot change under the checksum.

use crate::download::Source;
use crate::hardware::Pc;

/// A model that writes and runs on this PC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextModel {
    /// What a settings file names it by.
    pub id: &'static str,
    /// Name shown in a list.
    pub name: &'static str,
    /// What a list says beside its size: its languages.
    pub detail: &'static str,
    /// File name on disk.
    pub file: &'static str,
    /// Address, size and checksum of the file.
    pub source: Source<'static>,
    /// How many tokens of a text one thread of an ordinary processor reads
    /// into it in a second.
    pub reads: u64,
    /// How many tokens an ordinary processor has it write in a second.
    pub writes: u64,
}

macro_rules! text_model {
    ($id:literal, $name:literal, $detail:literal, $base:literal, $file:literal, $bytes:literal, $sha256:literal, $reads:literal, $writes:literal) => {
        TextModel {
            id: $id,
            name: $name,
            detail: $detail,
            file: $file,
            source: Source {
                url: concat!($base, $file),
                sha256: $sha256,
                bytes: $bytes,
            },
            reads: $reads,
            writes: $writes,
        }
    };
}

/// Every model, the one to start with first.
pub static TEXT_MODELS: [TextModel; 4] = ALL;

/// [`TEXT_MODELS`], for whoever builds a list of its own from them where
/// only a constant will do.
pub const ALL: [TextModel; 4] = [
    text_model!(
        "gemma-4-e2b",
        "Gemma 4 E2B",
        "140+ languages",
        "https://huggingface.co/unsloth/gemma-4-E2B-it-GGUF/resolve/0314792d7f1f7e229411f620751375812bb9faf2/",
        "gemma-4-E2B-it-Q4_K_M.gguf",
        3_106_738_272,
        "740185b21d22ceb83a11c3aa62ad5842ef32c70f6096d756bbee85a1e4ec34b8",
        19,
        12
    ),
    text_model!(
        "gemma-4-e4b",
        "Gemma 4 E4B",
        "140+ languages",
        "https://huggingface.co/unsloth/gemma-4-E4B-it-GGUF/resolve/bfc15c382204943c3a8fff0c750b94ae2364d7a3/",
        "gemma-4-E4B-it-Q4_K_M.gguf",
        4_977_171_584,
        "85a896a047553e842f25297ee5b031d64ff30147d9c4af17b1e4b394cd1fab87",
        12,
        7
    ),
    text_model!(
        "qwen3.5-2b",
        "Qwen3.5 2B",
        "201 languages",
        "https://huggingface.co/unsloth/Qwen3.5-2B-GGUF/resolve/f6d5376be1edb4d416d56da11e5397a961aca8ae/",
        "Qwen3.5-2B-Q4_K_M.gguf",
        1_280_835_840,
        "aaf42c8b7c3cab2bf3d69c355048d4a0ee9973d48f16c731c0520ee914699223",
        22,
        14
    ),
    text_model!(
        "qwen3.5-4b",
        "Qwen3.5 4B",
        "201 languages",
        "https://huggingface.co/unsloth/Qwen3.5-4B-GGUF/resolve/e87f176479d0855a907a41277aca2f8ee7a09523/",
        "Qwen3.5-4B-Q4_K_M.gguf",
        2_740_937_888,
        "00fe7986ff5f6b463e62455821146049db6f9313603938a70800d1fb69ef11a4",
        13,
        8
    ),
];

/// The model with this id.
#[must_use]
pub fn text_model(id: &str) -> Option<&'static TextModel> {
    TEXT_MODELS
        .iter()
        .find(|model| model.id.eq_ignore_ascii_case(id))
}

const MB: u64 = 1_000_000;

impl TextModel {
    /// The memory it takes while it runs: the model itself, and room for
    /// the text it is given.
    #[must_use]
    pub fn needs(&self) -> u64 {
        self.source.bytes / 10 * 12 + 1000 * MB
    }

    /// Whether the graphics card of `pc` holds all of it. A card that does
    /// is quick with any of these models; one that holds only a part leaves
    /// the rest to the processor, which is then what everything waits for.
    #[must_use]
    pub fn fits_the_card(&self, pc: &Pc) -> bool {
        pc.card_memory >= self.needs()
    }

    /// Whether the memory of `pc` is enough to run it on the processor,
    /// where it shares the memory with everything else.
    #[must_use]
    pub fn fits_the_memory(&self, pc: &Pc) -> bool {
        self.needs() <= pc.memory / 10 * 6
    }

    /// How long it would take to read `read` tokens and write `written`
    /// tokens on `pc`, in seconds, with the graphics card if `card` says
    /// models are to use it; `None` if this PC has too little memory for
    /// it. A rule of thumb, there so that nobody downloads gigabytes to
    /// find out about a wait.
    #[must_use]
    pub fn seconds_for(&self, read: u64, written: u64, pc: &Pc, card: bool) -> Option<u64> {
        if card && self.fits_the_card(pc) {
            // A card reads a page in a second or two and writes a hundred
            // tokens a second or more, whichever model it is.
            let pace: u64 = if self.source.bytes < 3500 * MB { 2 } else { 1 };
            return Some((read / (1000 * pace) + written / (60 * pace)).max(1));
        }
        if !self.fits_the_memory(pc) {
            return None;
        }
        // On a processor every thread that helps reads so many tokens a
        // second; writing is one token after another, and hardly quicker
        // with more than a few threads.
        let threads = u64::try_from(pc.threads.clamp(1, 16)).unwrap_or(1);
        let reading = read / (self.reads.max(1) * threads);
        let writing = written / (self.writes.max(1) * threads.min(4) / 4).max(1);
        Some((reading + writing).max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1000 * MB;

    fn pc(card_gb: u64, memory_gb: u64, threads: usize) -> Pc {
        Pc {
            card_memory: card_gb * GB,
            memory: memory_gb * GB,
            threads,
        }
    }

    #[test]
    fn every_model_is_pinned_and_every_id_is_its_own() {
        for model in &TEXT_MODELS {
            let source = model.source;
            assert!(source.url.starts_with("https://"), "{}", model.id);
            assert!(source.url.ends_with(model.file), "{}", model.id);
            assert!(!source.url.contains("/resolve/main/"), "{}", model.id);
            assert_eq!(source.sha256.len(), 64, "{}", model.id);
            assert!(source.bytes > GB, "{}", model.id);
            assert!(
                std::path::Path::new(model.file)
                    .extension()
                    .is_some_and(|ext| ext == "gguf"),
                "{}",
                model.id
            );
        }
        let mut ids: Vec<&str> = TEXT_MODELS.iter().map(|model| model.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), TEXT_MODELS.len());
        assert_eq!(
            text_model("GEMMA-4-E2B").map(|model| model.name),
            Some("Gemma 4 E2B")
        );
        assert!(text_model("no-such").is_none());
    }

    #[test]
    fn a_card_that_holds_a_model_is_quick_and_a_processor_is_told_the_wait() {
        let small = text_model("qwen3.5-2b").unwrap_or(&TEXT_MODELS[0]);
        let large = text_model("gemma-4-e4b").unwrap_or(&TEXT_MODELS[0]);
        // A dictation of a minute, tidied: some 200 tokens in and out.
        let wait = |model: &TextModel, pc: &Pc, card| model.seconds_for(200, 200, pc, card);
        let with_card = pc(12, 16, 8);
        assert!(wait(small, &with_card, true).is_some_and(|seconds| seconds <= 2));
        assert!(wait(large, &with_card, true).is_some_and(|seconds| seconds <= 4));
        // The card counts only while it is to be used, and only if it
        // holds the whole model.
        let on_processor = wait(small, &with_card, false);
        assert!(on_processor > wait(small, &with_card, true));
        assert_eq!(
            wait(large, &pc(4, 16, 8), true),
            wait(large, &pc(0, 16, 8), false)
        );
        assert!(large.fits_the_card(&with_card) && !large.fits_the_card(&pc(4, 16, 8)));
        // On a processor the wait is a good many seconds, more for the
        // larger model.
        let ordinary = pc(0, 16, 8);
        let (quick, slow) = (wait(small, &ordinary, false), wait(large, &ordinary, false));
        assert!(
            quick.is_some_and(|seconds| (8..=30).contains(&seconds)),
            "{quick:?}"
        );
        assert!(slow > quick, "{slow:?}");
        // Too little memory, however many threads.
        assert_eq!(wait(large, &pc(0, 8, 16), false), None);
        assert!(wait(small, &pc(0, 8, 4), false).is_some());
    }
}
