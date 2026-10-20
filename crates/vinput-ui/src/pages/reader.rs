//! The pages of Voutput: the keys that read a selection aloud, and the two
//! choices behind them, the voice that speaks and the model that sums up.
//!
//! The Models page mirrors Vinput's: every voice and every model is a row
//! that carries its own actions, the one in use stands out, and one that
//! runs on this PC says in the same quiet line how it would run here. The
//! two lists are picked apart from each other, so a voice on this PC can
//! read what an online model summed up, and the other way round.

use vinput_view::{
    Busy, Command, Fit, Length, ModelState, Page, Pick, Place, Reader, ReaderAsk, ReaderChange,
    ReaderSettings, View,
};

use super::Entry;
use super::common::{DOT, fraction, identity, links, notice_bar, related, so_far};
use crate::node::{
    Action, ButtonKind, Content, Icon, Node, Severity, Tone, badge, button, card, choice, expander,
    field, header, hotkey, human_size, icon_button, info_bar, link, note, plain_setting, row,
    scroll, setting, toggle,
};
use crate::theme::metric;

/// The pages listed in Voutput's navigation pane, top to bottom: one for
/// each of the two choices behind the keys, so that what belongs to the
/// voice and what belongs to the summaries are each in one place.
pub const NAVIGATION: [Entry; 5] = [
    (Page::Reading, "General", Icon::Settings),
    (Page::Voices, "Voice", Icon::Volume),
    (Page::Summaries, "Summaries", Icon::Memo),
    (Page::ReaderAdvanced, "Advanced", Icon::Plug),
    (Page::ReaderAbout, "About", Icon::Info),
];

/// The paces a voice can be set to, in hundredths of its usual one.
pub const SPEEDS: [u32; 8] = [50, 65, 80, 100, 115, 130, 150, 200];

/// Where Voutput is described.
const SITE: &str = "https://vinput-dasvcr.web.app";

fn ask(ask: ReaderAsk) -> Action {
    Action::send(Command::Reader(ask))
}

fn set(change: ReaderChange) -> Action {
    ask(ReaderAsk::Set(change))
}

fn page(title: &str, body: Vec<Node>) -> Content {
    Content {
        title: title.to_owned(),
        pinned: Vec::new(),
        body,
        footer: Vec::new(),
        back: None,
    }
}

/// A pace as people say it: `1×`, `1.5×`.
fn pace_name(hundredths: u32) -> String {
    let (whole, rest) = (hundredths / 100, hundredths % 100);
    if rest == 0 {
        format!("{whole}\u{d7}")
    } else if rest % 10 == 0 {
        format!("{whole}.{}\u{d7}", rest / 10)
    } else {
        format!("{whole}.{rest:02}\u{d7}")
    }
}

/// The General page: the two keys, and starting with Windows.
#[must_use]
pub fn reading(view: &View) -> Content {
    let settings = &view.reader.settings;
    let mut body = Vec::new();
    body.extend(notice_bar(view, Page::Reading));
    if !view.reader.voices.iter().any(|voice| voice.in_use) {
        body.push(Node::InfoBar {
            severity: Severity::Info,
            text: "Choose a voice to start. The voice that is part of Windows needs no download."
                .to_owned(),
            action: Some(("Choose a voice".to_owned(), Action::Go(Page::Voices))),
            dismiss: None,
        });
    }
    body.push(header("Shortcuts"));
    body.push(card([
        setting(Icon::Volume, "Read aloud")
            .control(hotkey("hotkey-read", &settings.hotkey_read, false, |key| {
                set(ReaderChange::HotkeyRead(key))
            }))
            .into(),
        setting(Icon::Memo, "Summarise and read")
            .control(hotkey(
                "hotkey-summary",
                &settings.hotkey_summary,
                true,
                |key| set(ReaderChange::HotkeySummary(key)),
            ))
            .into(),
    ]));
    body.push(note(
        "Select text in any app, then press a key. With nothing selected, Summarise and read \
         takes the whole window in front. Escape stops the reading.",
    ));

    body.push(header("Startup"));
    body.push(card([setting(Icon::Power, "Start with Windows")
        .detail("Ready in the tray when you sign in")
        .control(toggle(settings.autostart, |on| {
            set(ReaderChange::Autostart(on))
        }))
        .into()]));
    page("General", body)
}

/// What the model that sums up is told: closed, whether it is what
/// Voutput comes with or the user's own; open, the words to change and the
/// way back to the ones Voutput comes with.
fn summary_instructions(settings: &ReaderSettings) -> Node {
    let reset = set(ReaderChange::SummaryInstructions(String::new()));
    let own = settings.summary_instructions_own;
    expander(
        "summary-instructions-row",
        Icon::Edit,
        "Instructions",
        [plain_setting("What the model is told")
            .indented()
            .detail(
                "Voutput adds the summary length you chose. Ask for plain sentences, since \
                 the summary is read aloud.",
            )
            .control(
                button("Reset to default", reset)
                    .kind(ButtonKind::Subtle)
                    .enabled(own)
                    .into(),
            )
            .below(
                field(
                    "summary-instructions",
                    &settings.summary_instructions,
                    |text| set(ReaderChange::SummaryInstructions(text)),
                )
                .lines(INSTRUCTION_LINES)
                .into(),
            )
            .into()],
    )
    .value(if own { "Your own" } else { "Default" })
    .into()
}

/// How many lines the field of the summary instructions shows.
const INSTRUCTION_LINES: u8 = 5;

/// How a voice or model would run on this PC, as its row says it.
fn fit_words(fit: Fit) -> &'static str {
    match fit {
        Fit::Well => "Runs well on this PC",
        Fit::Slow => "Runs slowly on this PC",
        Fit::Heavy => "Too heavy for this PC",
    }
}

/// What a voice or model is, on one line: how it would run on this PC, its
/// size, its languages or what it needs, and what it is good at.
fn facts(pick: &Pick) -> String {
    let mut parts = Vec::new();
    parts.extend(pick.fit.map(|fit| fit_words(fit).to_owned()));
    if !pick.pace.is_empty() {
        parts.push(pick.pace.clone());
    }
    if pick.bytes > 0 {
        parts.push(human_size(pick.bytes));
    }
    if !pick.detail.is_empty() {
        parts.push(pick.detail.clone());
    }
    let about = pick.summary.trim().trim_end_matches('.');
    if !about.is_empty() {
        parts.push(about.to_owned());
    }
    parts.join(DOT)
}

/// The line under a name, and its color: where it stands if that needs
/// saying, otherwise what it is.
fn detail_line(pick: &Pick) -> (String, Tone) {
    let line = match &pick.state {
        ModelState::Failed { reason } => return (reason.clone(), Tone::Critical),
        ModelState::Installing { done, total } => {
            let line = format!("Downloading{DOT}{}", so_far(*done, *total));
            return (line, Tone::Secondary);
        }
        // What the one in use is doing right now comes before what it is.
        _ if !pick.status.is_empty() => format!("{}{DOT}{}", pick.status, facts(pick)),
        ModelState::Installed if pick.place == Place::ThisPc => {
            format!("Downloaded{DOT}{}", facts(pick))
        }
        _ => facts(pick),
    };
    // Only what reading depends on right now is said in a color.
    let tone = if pick.in_use && pick.fit == Some(Fit::Heavy) {
        Tone::Caution
    } else {
        Tone::Secondary
    };
    (line, tone)
}

/// The command that puts a voice or a model to use.
type Use = fn(String) -> ReaderAsk;

/// What is at the right edge of a row: the badge of the one in use, then
/// what can be done with it as it stands. `ready` says whether what it
/// needs besides a download is there: a key, an address.
fn controls(pick: &Pick, choose: Use, ready: bool) -> Vec<Node> {
    let id = || pick.id.clone();
    let mut controls = Vec::new();
    if pick.soon {
        controls.push(badge("Coming soon", Tone::Secondary));
        return controls;
    }
    if pick.in_use {
        controls.push(badge("In use", Tone::Accent));
    }
    let press = match pick.state {
        ModelState::Online | ModelState::Installed if pick.in_use => None,
        // The key is kept on the Advanced page, with what else is shared.
        ModelState::Online | ModelState::Installed if !ready && pick.place == Place::OwnKey => {
            Some(button("Add key", Action::Go(Page::ReaderAdvanced)))
        }
        ModelState::Online | ModelState::Installed => {
            Some(button("Use", ask(choose(id()))).enabled(ready))
        }
        ModelState::Missing => {
            Some(button("Download", ask(ReaderAsk::Install(id()))).icon(Icon::Download))
        }
        ModelState::Installing { .. } => {
            Some(button("Cancel", ask(ReaderAsk::CancelInstall(id()))))
        }
        ModelState::Failed { .. } => Some(button("Retry", ask(ReaderAsk::Install(id())))),
    };
    controls.extend(press.map(Node::from));
    let removable =
        pick.place == Place::ThisPc && !pick.in_use && pick.state == ModelState::Installed;
    if removable {
        controls.push(icon_button(Icon::Delete, "Delete", ask(ReaderAsk::Remove(id()))).into());
    }
    controls
}

/// One voice or model: what it is, where it stands and what can be done
/// with it.
fn pick_row(pick: &Pick, choose: Use, reader: &Reader) -> Node {
    let icon = match pick.place {
        Place::ThisPc | Place::Windows => Icon::Model,
        Place::OwnKey => Icon::Key,
        Place::Endpoint => Icon::Plug,
        Place::Cloud => Icon::Cloud,
    };
    let endpoint = &reader.settings;
    let ready = match pick.place {
        Place::OwnKey => reader.google_key_stored,
        Place::Endpoint => !endpoint.endpoint_url.is_empty() && !endpoint.endpoint_model.is_empty(),
        Place::ThisPc | Place::Windows | Place::Cloud => true,
    };
    let (mut detail, tone) = detail_line(pick);
    if pick.place == Place::OwnKey && !ready {
        detail = format!("Needs your Google API key{DOT}{detail}");
    }
    let mut line = setting(icon, &pick.name)
        .detail(detail)
        .detail_tone(tone)
        .selected(pick.in_use)
        .control(row(controls(pick, choose, ready)));
    if let ModelState::Installing { done, total } = pick.state {
        line = line.below(Node::Progress(fraction(done, total)));
    }
    match pick.busy {
        Busy::No => {}
        Busy::Moving => line = line.below(Node::Progress(None)),
        Busy::Part(done) => line = line.below(Node::Progress(Some(f32::from(done) / 100.0))),
    }
    if pick.place == Place::Endpoint {
        line = line.below(row([
            field("endpoint-url", &endpoint.endpoint_url, |text| {
                set(ReaderChange::EndpointUrl(text.trim().to_owned()))
            })
            .placeholder("http://127.0.0.1:1234/v1")
            .wide()
            .into(),
            field("endpoint-model", &endpoint.endpoint_model, |text| {
                set(ReaderChange::EndpointModel(text.trim().to_owned()))
            })
            .placeholder("Model name")
            .into(),
        ]));
    }
    line.into()
}

/// What to do on a PC that runs none of the models on it well: said once,
/// quietly, and only while one of those is what sums up.
fn none_runs_well(summaries: &[Pick]) -> Option<Node> {
    let local = || summaries.iter().filter(|pick| pick.place == Place::ThisPc);
    let on_this_pc = local().any(|pick| pick.in_use);
    let mut fits = local().filter_map(|pick| pick.fit).peekable();
    if !on_this_pc || fits.peek().is_none() || fits.any(|fit| fit == Fit::Well) {
        return None;
    }
    Some(note(
        "None of these is quick on this PC. An online model answers in about a second, on any PC.",
    ))
}

/// The Google API key that the voices and models at Google run with.
fn google_key(reader: &Reader) -> Node {
    let title = "Your Google API key";
    if reader.google_key_stored {
        return setting(Icon::Key, title)
            .detail("Kept in the Windows credential store, on this PC only")
            .control(
                button("Forget", ask(ReaderAsk::ForgetGoogleKey))
                    .kind(ButtonKind::Subtle)
                    .into(),
            )
            .into();
    }
    let save = Action::Submit {
        field: "google-key",
        make: |key| Command::Reader(ReaderAsk::SaveGoogleKey(key)),
    };
    setting(Icon::Key, title)
        .detail("For the voice and the model at Google. A key from Google AI Studio is enough.")
        .below(row([
            field("google-key", "", |_| Action::None)
                .placeholder("Paste your key")
                .secret()
                .wide()
                .on_enter(save.clone())
                .into(),
            button("Save", save).into(),
            link(
                "Get a key",
                Action::send(Command::OpenLink(
                    "https://aistudio.google.com/apikey".to_owned(),
                )),
            ),
        ]))
        .into()
}

/// What a closed row says when nothing of its list is in use.
const NOT_CHOSEN: &str = "Not chosen";

/// The row that names what is in use of `picks` and opens onto all of
/// them, in a list that scrolls by itself. It is open by itself while there
/// is something to do in it: nothing is chosen yet, or a download is under
/// way or has failed.
fn picker(key: &'static str, icon: Icon, title: &str, picks: &[Pick], rows: Vec<Node>) -> Node {
    let in_use = picks.iter().find(|pick| pick.in_use);
    let busy = |pick: &Pick| {
        matches!(
            pick.state,
            ModelState::Installing { .. } | ModelState::Failed { .. }
        )
    };
    // The two lists are told apart by the key of the row they are under.
    let list = if key == "voice" {
        "voices"
    } else {
        "summaries"
    };
    expander(key, icon, title, [scroll(list, metric::LIST_MAX, rows)])
        .value(in_use.map_or(NOT_CHOSEN, |pick| pick.name.as_str()))
        .value_tone(if in_use.is_some() {
            Tone::Secondary
        } else {
            Tone::Caution
        })
        .open(in_use.is_none() || picks.iter().any(busy))
        .into()
}

/// The rows under the voice in use: how fast it speaks, the speaker of its
/// several it speaks as, and a sentence to hear the two in.
fn how_it_speaks(reader: &Reader) -> Vec<Node> {
    let settings = &reader.settings;
    let speeds = SPEEDS.iter().map(|speed| pace_name(*speed));
    let speed = SPEEDS
        .iter()
        .position(|speed| *speed == settings.speed)
        .unwrap_or(3);
    let mut rows = vec![
        setting(Icon::Timer, "Speed")
            .control(choice("speed", speeds, speed, |index| {
                set(ReaderChange::Speed(SPEEDS[index.min(SPEEDS.len() - 1)]))
            }))
            .into(),
    ];
    if reader.speakers.len() > 1 {
        let speakers = reader.speakers.clone();
        let speaker = speakers
            .iter()
            .position(|speaker| *speaker == settings.speaker)
            .unwrap_or(0);
        let names = speakers.clone();
        rows.push(
            setting(Icon::Person, "Speaker")
                .control(choice("speaker", names, speaker, move |index| {
                    let chosen = speakers.get(index).cloned().unwrap_or_default();
                    set(ReaderChange::Speaker(chosen))
                }))
                .into(),
        );
    }
    let (label, press) = if reader.speaking {
        ("Stop", ReaderAsk::Stop)
    } else {
        ("Play", ReaderAsk::Sample)
    };
    rows.push(
        setting(Icon::Volume, "Try it")
            .detail("Reads a sentence with this voice, speaker and speed")
            .control(button(label, ask(press)).into())
            .into(),
    );
    rows
}

/// The Voice page: the voice in use, and under it how fast it speaks, as
/// which of its speakers, and a sentence to hear the two in.
#[must_use]
pub fn voices(view: &View) -> Content {
    let reader = &view.reader;
    let mut body = Vec::new();
    body.extend(notice_bar(view, Page::Voices));
    let chosen = reader.voices.iter().any(|voice| voice.in_use);
    if !chosen {
        body.push(info_bar(
            Severity::Info,
            "Choose a voice to start reading aloud. The Windows voice needs no download.",
        ));
    }
    let voices = reader
        .voices
        .iter()
        .map(|voice| pick_row(voice, ReaderAsk::UseVoice, reader));
    body.push(card([picker(
        "voice",
        Icon::Volume,
        "Voice",
        &reader.voices,
        voices.collect(),
    )]));
    if chosen {
        body.push(header("How it sounds"));
        body.push(card(how_it_speaks(reader)));
    }
    page("Voice", body)
}

/// The Summaries page: the model that sums up, how long a summary is, and
/// what the model is told.
#[must_use]
pub fn summaries(view: &View) -> Content {
    let reader = &view.reader;
    let settings = &reader.settings;
    let mut body = Vec::new();
    body.extend(notice_bar(view, Page::Summaries));
    let summaries = reader
        .summaries
        .iter()
        .map(|model| pick_row(model, ReaderAsk::UseSummary, reader));
    body.push(card([picker(
        "summary",
        Icon::Model,
        "Model",
        &reader.summaries,
        summaries.collect(),
    )]));
    body.extend(none_runs_well(&reader.summaries));

    let lengths = ["Two or three sentences", "About half a minute"];
    let length = match settings.summary_length {
        Length::Short => 0,
        Length::Medium => 1,
    };
    body.push(header("What it writes"));
    body.push(card([
        setting(Icon::Timer, "Length")
            .control(choice(
                "summary-length",
                lengths.map(str::to_owned),
                length,
                |index| {
                    set(ReaderChange::SummaryLength(if index == 0 {
                        Length::Short
                    } else {
                        Length::Medium
                    }))
                },
            ))
            .into(),
        summary_instructions(settings),
    ]));
    page("Summaries", body)
}

/// The Advanced page: the Google API key both choices can use, the
/// graphics card, and the disk.
#[must_use]
pub fn advanced(view: &View) -> Content {
    let reader = &view.reader;
    let mut body = Vec::new();
    body.extend(notice_bar(view, Page::ReaderAdvanced));
    body.push(header("Google"));
    body.push(card([google_key(reader)]));
    if let Some(name) = &reader.graphics_card {
        body.push(header("Graphics card"));
        body.push(card([
            setting(Icon::Chip, "Run models on the graphics card")
                .detail(name)
                .control(toggle(reader.settings.graphics_card, |on| {
                    set(ReaderChange::GraphicsCard(on))
                }))
                .into(),
            setting(Icon::Chip, "Free graphics memory when idle")
                .detail(
                    "Unloads the model after 30 minutes without a summary. The next summary \
                     then waits a few seconds.",
                )
                .control(toggle(reader.settings.free_memory_when_idle, |on| {
                    set(ReaderChange::FreeMemoryWhenIdle(on))
                }))
                .into(),
        ]));
    }
    body.push(header("Storage"));
    body.push(card([setting(Icon::Storage, "Disk space")
        .detail(format!(
            "{} used by voices and models",
            human_size(reader.disk_bytes)
        ))
        .control(button("Open folder", ask(ReaderAsk::OpenFolder)).into())
        .into()]));
    page("Advanced", body)
}

/// The About page of Voutput, laid out as Vinput's is.
#[must_use]
pub fn about(view: &View) -> Content {
    let mut body = vec![identity(
        Icon::Volume,
        "Voutput",
        "Select text anywhere, press a key, and hear it. By PUTLAB",
        &view.version,
    )];
    body.extend(links(&[(
        Icon::Globe,
        "Website",
        "vinput-dasvcr.web.app",
        SITE,
    )]));
    body.extend(related(view));
    page("About", body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock;
    use crate::pages::common::probe::{Seen, all, command};
    use vinput_view::App;

    fn scene(name: &str) -> View {
        mock::scene(name).unwrap_or_default()
    }

    fn reader(ask: ReaderAsk) -> Command {
        Command::Reader(ask)
    }

    /// What the Voice and the Summaries pages show together.
    fn both(view: &View) -> Vec<Node> {
        let mut body = voices(view).body;
        body.extend(summaries(view).body);
        body
    }

    #[test]
    fn every_page_in_the_pane_is_one_of_voutputs() {
        for &(page, name, _) in &NAVIGATION {
            assert_eq!(page.app(), App::Voutput, "{name}");
        }
    }

    #[test]
    fn a_pace_is_named_the_way_people_say_it() {
        let names: Vec<String> = SPEEDS.iter().map(|speed| pace_name(*speed)).collect();
        assert_eq!(
            names,
            ["0.5×", "0.65×", "0.8×", "1×", "1.15×", "1.3×", "1.5×", "2×"]
        );
    }

    #[test]
    fn the_two_choices_are_on_pages_of_their_own_and_each_has_one_in_use() {
        let view = scene("voutput");
        for page in [voices(&view), summaries(&view)] {
            let seen = Seen::of(&page.body);
            let in_use = seen.badges.iter().filter(|badge| *badge == "In use");
            assert_eq!(in_use.count(), 1, "{}", page.title);
        }
        let body = both(&view);
        let seen = Seen::of(&body);
        assert_eq!(
            seen.badges
                .iter()
                .filter(|badge| *badge == "In use")
                .count(),
            2
        );
        // The cloud that comes with a subscription is announced, not offered.
        assert_eq!(
            seen.badges
                .iter()
                .filter(|badge| *badge == "Coming soon")
                .count(),
            2
        );
        let cloud = view.reader.voices.iter().find(|voice| voice.soon);
        let cloud = cloud.map(|voice| reader(ReaderAsk::UseVoice(voice.id.clone())));
        assert!(cloud.is_some_and(|use_it| !seen.commands().contains(&use_it)));
    }

    #[test]
    fn the_voice_in_use_has_its_speed_speaker_and_a_sentence_to_try_it() {
        let view = scene("voutput");
        let page = voices(&view);
        let seen = Seen::of(&page.body);
        assert!(seen.says("Speed") && seen.says("Speaker") && seen.says("Try it"));
        assert!(seen.commands().contains(&reader(ReaderAsk::Sample)));
        let nodes = all(&page.body);
        let speakers = nodes.iter().find_map(|node| match node {
            Node::Choice {
                key: "speaker",
                items,
                action,
                ..
            } => Some((items.len(), command(&action(6)))),
            _ => None,
        });
        assert_eq!(
            speakers,
            Some((
                10,
                Some(reader(ReaderAsk::Set(ReaderChange::Speaker(
                    "Male 2".to_owned()
                ))))
            ))
        );
        // A voice of one speaker has nothing to choose; a reading can be
        // stopped from where it was started.
        let mut one = scene("voutput-reading");
        one.reader.speakers.clear();
        let seen = Seen::of(&voices(&one).body);
        assert!(!seen.says("Speaker"));
        assert!(seen.commands().contains(&reader(ReaderAsk::Stop)));
        // Nothing to set before a voice is chosen.
        assert!(!Seen::of(&voices(&scene("voutput-first-run")).body).says("Speed"));
        assert!(!Seen::of(&reading(&view).body).says("Speed"));
    }

    #[test]
    fn a_model_on_this_pc_says_how_it_would_run_here() {
        let seen = Seen::of(&both(&scene("voutput")));
        assert!(seen.says("Runs well on this PC"));
        let weak = Seen::of(&both(&scene("voutput-weak-pc")));
        assert!(weak.says("Runs slowly on this PC"));
        assert!(weak.says("Too heavy for this PC"));
        assert!(weak.says("None of these is quick on this PC"));
        // Nothing is said to someone whose PC runs them well.
        assert!(!seen.says("None of these is quick"));
    }

    #[test]
    fn what_the_model_in_use_is_doing_comes_before_what_it_is() {
        let mut view = scene("voutput");
        let local = view
            .reader
            .summaries
            .iter_mut()
            .find(|pick| pick.place == Place::ThisPc);
        let Some(local) = local else {
            unreachable!("the scene has a model on this PC");
        };
        local.state = ModelState::Installed;
        "Ready on the graphics card".clone_into(&mut local.status);
        let (line, _) = detail_line(local);
        assert!(line.starts_with("Ready on the graphics card"), "{line}");
        assert!(line.contains("Runs well on this PC"), "{line}");
        let seen = Seen::of(&summaries(&view).body);
        assert!(seen.says("Ready on the graphics card"));
        assert!(seen.progress.is_empty());
        assert!(Seen::of(&advanced(&view).body).says("Free graphics memory when idle"));
    }

    #[test]
    fn a_model_that_is_being_made_ready_shows_how_far_it_is() {
        let bars = |busy: Busy| {
            let mut view = scene("voutput");
            let local = view
                .reader
                .summaries
                .iter_mut()
                .find(|pick| pick.place == Place::ThisPc);
            if let Some(local) = local {
                local.state = ModelState::Installed;
                local.busy = busy;
            }
            Seen::of(&summaries(&view).body).progress
        };
        assert_eq!(bars(Busy::Part(40)), [Some(0.4)]);
        // No saying how far: a bar that moves.
        assert_eq!(bars(Busy::Moving), [None]);
        assert!(bars(Busy::No).is_empty());
    }

    #[test]
    fn a_download_shows_how_far_it_is_and_can_be_stopped() {
        let seen = Seen::of(&both(&scene("voutput-downloading")));
        assert_eq!(seen.progress.len(), 1);
        assert!(seen.says("Downloading"));
        assert!(seen.labels().contains(&"Cancel"));
    }

    #[test]
    fn what_runs_at_google_waits_for_the_key() {
        let without = scene("voutput-first-run");
        let body = both(&without);
        let seen = Seen::of(&body);
        assert!(seen.says("Needs your Google API key"));
        // The key is kept on the Advanced page, where the row leads.
        assert!(seen.labels().contains(&"Add key"));
        let own = without
            .reader
            .summaries
            .iter()
            .find(|pick| pick.place == Place::OwnKey);
        let use_it = own.map(|pick| reader(ReaderAsk::UseSummary(pick.id.clone())));
        assert!(use_it.is_some_and(|use_it| !seen.commands().contains(&use_it)));
        let advanced_page = Seen::of(&advanced(&without).body);
        assert!(
            advanced_page
                .fields
                .iter()
                .any(|(key, _)| *key == "google-key")
        );
        let with = Seen::of(&advanced(&scene("voutput")).body);
        assert!(
            with.commands()
                .contains(&reader(ReaderAsk::ForgetGoogleKey))
        );
    }

    #[test]
    fn a_server_of_ones_own_asks_for_its_address_and_model() {
        let seen = Seen::of(&summaries(&scene("voutput")).body);
        let keys: Vec<_> = seen.fields.iter().map(|(key, _)| *key).collect();
        assert!(keys.contains(&"endpoint-url") && keys.contains(&"endpoint-model"));
    }

    #[test]
    fn the_summary_instructions_can_be_changed_and_put_back() {
        let view = scene("voutput");
        let page = summaries(&view);
        let seen = Seen::of(&page.body);
        let nodes = all(&page.body);
        let field = nodes.iter().find_map(|node| match node {
            Node::Field(field) if field.key == "summary-instructions" => Some(field),
            _ => None,
        });
        let Some(field) = field else {
            unreachable!("the Summaries page has the summary instructions");
        };
        // Closed, the row says whose words they are.
        let row = nodes.iter().find_map(|node| match node {
            Node::Expander(row) if row.title == "Instructions" => Some(row),
            _ => None,
        });
        assert!(row.is_some_and(|row| !row.open && row.value == "Default"));
        assert_eq!(field.text, view.reader.settings.summary_instructions);
        assert!(field.lines > 1 && field.wide);
        // What is typed is handed in as it is.
        let typed = (field.action)("Use easy words.".to_owned());
        assert_eq!(
            command(&typed),
            Some(reader(ReaderAsk::Set(ReaderChange::SummaryInstructions(
                "Use easy words.".to_owned()
            ))))
        );
        assert!(seen.says("What the model is told"));
        assert!(seen.says("Voutput adds the summary length"));
        let reset = reader(ReaderAsk::Set(ReaderChange::SummaryInstructions(
            String::new(),
        )));
        // Nothing to put back while the words are Voutput's own.
        assert!(!seen.commands().contains(&reset));
        let own = Seen::of(&summaries(&scene("voutput-own-instructions")).body);
        assert!(own.commands().contains(&reset));
        assert!(!Seen::of(&reading(&view).body).says("What the model is told"));
    }

    #[test]
    fn someone_new_is_pointed_at_a_voice() {
        let seen = Seen::of(&reading(&scene("voutput-first-run")).body);
        assert_eq!(seen.bars.len(), 1);
        assert!(Seen::of(&reading(&scene("voutput")).body).bars.is_empty());
    }
}
