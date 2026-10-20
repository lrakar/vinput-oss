//! The canned scenes of Voutput's windows.

use vinput_view::{
    Busy, Fit, Length, ModelState, Pick, Place, Reader, ReaderAsk, ReaderChange, ReaderSettings,
    RunsOn, Status, View,
};

/// The names [`scene`] knows.
pub const SCENES: [&str; 6] = [
    "voutput",
    "voutput-first-run",
    "voutput-weak-pc",
    "voutput-downloading",
    "voutput-reading",
    "voutput-own-instructions",
];

/// The words Voutput tells a model that sums up, until the user writes
/// their own; as `STYLE` in apps/voutput/src/summary.rs.
const INSTRUCTIONS: &str = "You sum up a text for someone who will listen to the summary, not \
                            read it. Write in the language the text is written in. Say what the \
                            text says, not that it is a text. Plain, short sentences only: no \
                            lists, no headings, no formatting, no introduction.";

const MEGABYTE: u64 = 1 << 20;

fn pick(id: &str, name: &str, place: Place, megabytes: u64, detail: &str, summary: &str) -> Pick {
    let local = place == Place::ThisPc;
    Pick {
        id: id.to_owned(),
        name: name.to_owned(),
        summary: summary.to_owned(),
        detail: detail.to_owned(),
        place,
        bytes: megabytes * MEGABYTE,
        in_use: false,
        soon: place == Place::Cloud,
        // The PC of the scenes has a large graphics card.
        fit: local.then_some(Fit::Well),
        pace: String::new(),
        status: String::new(),
        busy: Busy::No,
        state: if local {
            ModelState::Missing
        } else {
            ModelState::Online
        },
    }
}

fn voices() -> Vec<Pick> {
    vec![
        pick(
            "putlab-voice",
            "PUTLAB cloud voice",
            Place::Cloud,
            0,
            "Comes with a subscription",
            "The most natural voice, quick on any PC.",
        ),
        pick(
            "gemini-tts",
            "Google Gemini Flash TTS",
            Place::OwnKey,
            0,
            "",
            "Natural, in many languages, Slovenian among them.",
        ),
        pick(
            "supertonic-3",
            "Supertonic 3",
            Place::ThisPc,
            123,
            "31 languages, Slovenian among them",
            "Ten voices. Quick on any processor.",
        ),
        pick(
            "piper-sl-artur",
            "Piper Artur",
            Place::ThisPc,
            64,
            "Slovenian",
            "Small. Sounds plainer.",
        ),
        pick(
            "windows",
            "Windows voice",
            Place::Windows,
            0,
            "The voices installed in Windows",
            "Nothing to download.",
        ),
    ]
}

fn summaries() -> Vec<Pick> {
    vec![
        pick(
            "putlab-summary",
            "PUTLAB cloud",
            Place::Cloud,
            0,
            "Comes with a subscription",
            "Answers in under a second, on any PC.",
        ),
        pick(
            "gemini-flash-lite",
            "Google Gemini 3.5 Flash-Lite",
            Place::OwnKey,
            0,
            "",
            "Answers in about a second.",
        ),
        pick(
            "gemma-4-e2b",
            "Gemma 4 E2B",
            Place::ThisPc,
            2963,
            "140+ languages",
            "The one to start with.",
        ),
        pick(
            "gemma-4-e4b",
            "Gemma 4 E4B",
            Place::ThisPc,
            4747,
            "140+ languages",
            "Better summaries, slower.",
        ),
        pick(
            "qwen3.5-2b",
            "Qwen3.5 2B",
            Place::ThisPc,
            1222,
            "201 languages",
            "The smallest.",
        ),
        pick(
            "qwen3.5-4b",
            "Qwen3.5 4B",
            Place::ThisPc,
            2614,
            "201 languages",
            "Between the two Gemma models.",
        ),
        pick(
            "endpoint",
            "Your own server",
            Place::Endpoint,
            0,
            "",
            "Any server that speaks the OpenAI chat API, such as LM Studio or Ollama.",
        ),
    ]
}

fn find<'a>(list: &'a mut [Pick], id: &str) -> Option<&'a mut Pick> {
    list.iter_mut().find(|pick| pick.id == id)
}

fn choose(list: &mut [Pick], id: &str) -> Option<String> {
    for pick in list.iter_mut() {
        pick.in_use = pick.id == id;
        if pick.in_use && pick.place == Place::ThisPc {
            pick.state = ModelState::Installed;
        }
    }
    list.iter()
        .find(|pick| pick.in_use)
        .map(|pick| pick.name.clone())
}

/// The speakers of Supertonic 3, as Voutput lists them.
fn supertonic_speakers() -> Vec<String> {
    let female = (1..=5).map(|n| format!("Female {n}"));
    female.chain((1..=5).map(|n| format!("Male {n}"))).collect()
}

/// Someone who has used Voutput for a while: a voice and a model on this
/// PC, and a Google key stored.
fn settled() -> View {
    let mut reader = Reader {
        settings: ReaderSettings {
            hotkey_read: "F6".to_owned(),
            hotkey_summary: "F7".to_owned(),
            speed: 100,
            speaker: "Female 1".to_owned(),
            summary_length: Length::Short,
            summary_instructions: INSTRUCTIONS.to_owned(),
            summary_instructions_own: false,
            autostart: true,
            check_updates: true,
            graphics_card: true,
            free_memory_when_idle: false,
            endpoint_url: String::new(),
            endpoint_model: String::new(),
        },
        voices: voices(),
        summaries: summaries(),
        speakers: supertonic_speakers(),
        speaking: false,
        google_key_stored: true,
        graphics_card: Some("NVIDIA GeForce RTX 4070".to_owned()),
        disk_bytes: (123 + 2963 + 32) * MEGABYTE,
    };
    let voice = choose(&mut reader.voices, "supertonic-3");
    choose(&mut reader.summaries, "gemma-4-e2b");
    View {
        version: "0.1.0".to_owned(),
        status: Status {
            listening: false,
            model: voice,
            runs_on: RunsOn::Processor,
        },
        reader,
        ..View::default()
    }
}

/// How the models of the catalog stand on a small laptop without a
/// graphics card.
fn on_a_weak_pc(reader: &mut Reader) {
    reader.graphics_card = None;
    reader.settings.graphics_card = false;
    let verdicts = [
        ("gemma-4-e2b", Fit::Slow, "About 15 s for a page"),
        ("gemma-4-e4b", Fit::Heavy, ""),
        ("qwen3.5-2b", Fit::Slow, "About 10 s for a page"),
        ("qwen3.5-4b", Fit::Heavy, ""),
    ];
    for (id, fit, pace) in verdicts {
        if let Some(pick) = find(&mut reader.summaries, id) {
            pick.fit = Some(fit);
            pace.clone_into(&mut pick.pace);
        }
    }
}

/// The view that puts a window of Voutput in the state `name` names.
#[must_use]
pub fn scene(name: &str) -> Option<View> {
    let mut view = settled();
    match name {
        "voutput" => {}
        "voutput-first-run" => {
            view.reader.voices = voices();
            view.reader.summaries = summaries();
            view.reader.speakers = Vec::new();
            view.reader.settings.speaker = String::new();
            view.reader.google_key_stored = false;
            view.reader.disk_bytes = 0;
            view.status.model = None;
            view.status.runs_on = RunsOn::Nowhere;
        }
        "voutput-weak-pc" => on_a_weak_pc(&mut view.reader),
        "voutput-downloading" => {
            if let Some(pick) = find(&mut view.reader.summaries, "gemma-4-e4b") {
                pick.state = ModelState::Installing {
                    done: 1730 * MEGABYTE,
                    total: 4747 * MEGABYTE,
                };
            }
        }
        "voutput-reading" => {
            view.reader.speaking = true;
            view.status.listening = true;
        }
        "voutput-own-instructions" => {
            let settings = &mut view.reader.settings;
            "Use easy words, as for a child of twelve.\n\
             Start with the main point. Never start with \"This text\" or \"The author\".\n\
             Leave out numbers unless they matter."
                .clone_into(&mut settings.summary_instructions);
            settings.summary_instructions_own = true;
        }
        _ => return None,
    }
    Some(view)
}

fn set(settings: &mut ReaderSettings, change: ReaderChange) {
    match change {
        ReaderChange::HotkeyRead(key) => settings.hotkey_read = key,
        ReaderChange::HotkeySummary(key) => settings.hotkey_summary = key,
        ReaderChange::Speed(speed) => settings.speed = speed,
        ReaderChange::Speaker(speaker) => settings.speaker = speaker,
        ReaderChange::SummaryLength(length) => settings.summary_length = length,
        ReaderChange::SummaryInstructions(text) => {
            settings.summary_instructions_own = !text.is_empty() && text != INSTRUCTIONS;
            settings.summary_instructions = if text.is_empty() {
                INSTRUCTIONS.to_owned()
            } else {
                text
            };
        }
        ReaderChange::Autostart(on) => settings.autostart = on,
        ReaderChange::CheckUpdates(on) => settings.check_updates = on,
        ReaderChange::GraphicsCard(on) => settings.graphics_card = on,
        ReaderChange::FreeMemoryWhenIdle(on) => settings.free_memory_when_idle = on,
        ReaderChange::EndpointUrl(url) => settings.endpoint_url = url,
        ReaderChange::EndpointModel(model) => settings.endpoint_model = model,
    }
}

/// Changes `view` the way Voutput would after `ask`, as far as the picture
/// goes.
pub(super) fn apply(view: &mut View, ask: ReaderAsk) {
    let reader = &mut view.reader;
    let both = |reader: &mut Reader, id: &str, change: &dyn Fn(&mut Pick)| {
        let lists = [&mut reader.voices, &mut reader.summaries];
        for list in lists {
            if let Some(pick) = find(list, id) {
                change(pick);
            }
        }
    };
    match ask {
        ReaderAsk::Set(change) => set(&mut reader.settings, change),
        ReaderAsk::UseVoice(id) => {
            view.status.model = choose(&mut reader.voices, &id);
            let several = id == "supertonic-3";
            reader.speakers = if several {
                supertonic_speakers()
            } else {
                Vec::new()
            };
            reader.settings.speaker = reader.speakers.first().cloned().unwrap_or_default();
        }
        ReaderAsk::UseSummary(id) => {
            choose(&mut reader.summaries, &id);
        }
        ReaderAsk::Install(id) => both(reader, &id, &|pick| {
            pick.state = ModelState::Installing {
                done: 0,
                total: pick.bytes,
            };
        }),
        ReaderAsk::CancelInstall(id) | ReaderAsk::Remove(id) => {
            both(reader, &id, &|pick| pick.state = ModelState::Missing);
        }
        ReaderAsk::SaveGoogleKey(key) => reader.google_key_stored = !key.trim().is_empty(),
        ReaderAsk::ForgetGoogleKey => reader.google_key_stored = false,
        ReaderAsk::Sample => {
            reader.speaking = true;
            view.status.listening = true;
        }
        ReaderAsk::Stop => {
            reader.speaking = false;
            view.status.listening = false;
        }
        ReaderAsk::OpenFolder => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_named_scene_exists() {
        for name in SCENES {
            assert!(scene(name).is_some(), "no scene called {name}");
        }
        assert!(scene("voutput-no-such").is_none());
    }

    #[test]
    fn a_voice_and_a_model_are_chosen_apart_from_each_other() {
        let mut view = settled();
        apply(&mut view, ReaderAsk::UseVoice("windows".to_owned()));
        apply(&mut view, ReaderAsk::UseSummary("endpoint".to_owned()));
        let in_use = |list: &[Pick]| -> Vec<String> {
            let used = list.iter().filter(|pick| pick.in_use);
            used.map(|pick| pick.id.clone()).collect()
        };
        assert_eq!(in_use(&view.reader.voices), ["windows"]);
        assert_eq!(in_use(&view.reader.summaries), ["endpoint"]);
        assert_eq!(view.status.model.as_deref(), Some("Windows voice"));
    }
}
