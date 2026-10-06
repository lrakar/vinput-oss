//! The Models page: the speech model in use and the downloads.
//!
//! Every model is a row that carries its own actions, so nothing has to be
//! selected first: `Use` on a model that is ready, `Download` on one that is
//! not on this PC, `Cancel` while it downloads. The model in use is the row
//! that stands out and wears the `In use` badge.
//!
//! A model that was not taught a language dictation is set to says so on
//! its row, before it is downloaded or put to use; if it is the one in use
//! and knows none of them, a bar says what to do about it.
//!
//! `VinPUT` Pro is not a row among them. It is the app's own service, so it
//! has a card of its own under the title, which stays there while the lists
//! scroll under it.

use vinput_view::{
    Change, Command, Engine, GoogleService, Model, ModelState, Models, PRO_MODEL, Page, ProDoing,
    ProWho, Setup, View,
};

use super::common::{
    DOT, ProTrouble, fraction, hours_low, least_a_month, notice_bar, owing, pro_trouble, so_far,
};
use super::pro::{usage, works_again};
use crate::node::{
    Action, ButtonKind, Content, Icon, Node, Severity, Tone, accent_button, badge, button, card,
    header, human_size, icon_button, info_bar, row, setting, toggle,
};

/// What the page shows for `view`.
#[must_use]
pub fn content(view: &View) -> Content {
    let models = &view.models;
    let mut body = Vec::new();
    body.extend(notice_bar(view, Page::Models));
    body.extend(prompt(&models.list));
    body.extend(wrong_language(&models.list));
    body.extend(engine_failure(&models.engine));
    body.extend(online(view));
    body.extend(on_this_pc(models, view.settings.graphics_card));
    Content {
        title: "Models".to_owned(),
        pinned: pro_card(view).into_iter().collect(),
        body,
        footer: Vec::new(),
        back: None,
    }
}

/// What to do first, for someone who has not chosen a model yet.
fn prompt(list: &[Model]) -> Option<Node> {
    if list.iter().any(|model| model.in_use) {
        return None;
    }
    Some(info_bar(
        Severity::Info,
        "Choose a speech model to start dictating. Use an online model, or download one that \
         runs on this PC.",
    ))
}

/// What to do when the model in use knows none of the languages dictation
/// is set to: it types, but not what was said.
fn wrong_language(list: &[Model]) -> Option<Node> {
    let model = list.iter().find(|model| model.in_use && model.knows_none)?;
    Some(info_bar(
        Severity::Caution,
        format!(
            "This model does not know {}. Choose another model, or change the languages under \
             General.",
            listed(&model.lacks)
        ),
    ))
}

/// Names as a sentence lists them: `Slovenian`, `Slovenian or Croatian`,
/// `Slovenian, Croatian or Serbian`.
fn listed(names: &[String]) -> String {
    match names {
        [first @ .., last] if !first.is_empty() => format!("{} or {last}", first.join(", ")),
        _ => names.concat(),
    }
}

/// Why the engine that local models need is not here, if its download failed.
pub(super) fn engine_failure(engine: &Engine) -> Option<Node> {
    let Engine::Failed { reason } = engine else {
        return None;
    };
    Some(info_bar(
        Severity::Critical,
        format!("The engine that runs models on this PC could not be downloaded. {reason}"),
    ))
}

/// The models that run at an online service.
fn online(view: &View) -> Vec<Node> {
    let rows: Vec<Node> = view
        .models
        .list
        .iter()
        .filter(|model| !model.local && model.id != PRO_MODEL)
        .map(|model| match model.google {
            Some(GoogleService::Gemini) => {
                google_row(model, GoogleService::Gemini, view.gemini_key_stored)
            }
            Some(GoogleService::Speech) => {
                google_row(model, GoogleService::Speech, view.google.speech_ready)
            }
            None => model_row(model),
        })
        .collect();
    if rows.is_empty() {
        Vec::new()
    } else {
        vec![header("Online"), card(rows)]
    }
}

/// The models that run on this PC, the way to add one, and what they take.
fn on_this_pc(models: &Models, graphics_card_on: bool) -> Vec<Node> {
    let mut rows: Vec<Node> = engine_download(&models.engine).into_iter().collect();
    rows.extend(
        models
            .list
            .iter()
            .filter(|model| model.local)
            .map(model_row),
    );
    rows.push(add_row());
    let mut facts = Vec::new();
    facts.extend(
        models
            .graphics_card
            .as_deref()
            .map(|name| graphics_card_row(name, graphics_card_on)),
    );
    facts.push(disk_row(models.disk_bytes));
    vec![header("On this PC"), card(rows), card(facts)]
}

/// The download of the engine that local models need, while it is under way.
pub(super) fn engine_download(engine: &Engine) -> Option<Node> {
    let Engine::Fetching { done, total } = *engine else {
        return None;
    };
    Some(
        setting(
            Icon::Download,
            "Getting the engine that runs models on this PC",
        )
        .detail(so_far(done, total))
        .below(Node::Progress(fraction(done, total)))
        .into(),
    )
}

/// One model: what it is, where it stands and what can be done with it.
pub(super) fn model_row(model: &Model) -> Node {
    let icon = if model.local {
        Icon::Model
    } else {
        Icon::Cloud
    };
    let (detail, tone) = detail_line(model);
    let mut line = setting(icon, &model.name)
        .detail(detail)
        .detail_tone(tone)
        .selected(model.in_use)
        .control(row(controls(model)));
    if let ModelState::Installing { done, total } = model.state {
        line = line.below(Node::Progress(fraction(done, total)));
    }
    line.into()
}

/// `VinPUT` Pro, in a card of its own: what it is and what it costs for
/// someone who has no subscription, how much of the month is used for
/// someone who has one, and what is wrong if something is. Never more than
/// one line and one button; everything else about it is on its page.
fn pro_card(view: &View) -> Option<Node> {
    let mut models = view.models.list.iter();
    let model = models.find(|model| model.id == PRO_MODEL)?;
    let pro = &view.pro;
    let page = || Action::Go(Page::Pro);
    let open = || button("Open VinPUT Pro", page());
    let unknown = pro.who == ProWho::SignedIn && !pro.known;
    // A renewal failed and Pro works for now; nothing is owed for an
    // account that is given it.
    let behind = owing(&pro.status) && !pro.complimentary;
    let trouble = pro_trouble(pro);
    let manage = || button("Manage", page()).kind(ButtonKind::Subtle);
    let (said, alarm, press) = match trouble {
        // The server has not said yet, or could not be asked.
        None if unknown && pro.doing == ProDoing::Asking && pro.problem.is_none() => {
            ("Checking your subscription".to_owned(), None, open())
        }
        None if unknown => ("Could not check your subscription".to_owned(), None, open()),
        // The benefit first, then what it costs.
        // The one button of the page in the accent color: it is what makes
        // the card easy to find.
        Some(ProTrouble::SignedOut | ProTrouble::NoSubscription) => (
            format!("{}{DOT}from {}", about(&model.summary), least_a_month(pro)),
            None,
            accent_button("See plans", page()),
        ),
        // Typing goes on with a model on this PC: said, not warned of.
        Some(ProTrouble::UsedUp) if pro.spare.is_some() => (
            format!(
                "Hours used up{DOT}using {} {}",
                pro.spare.as_deref().unwrap_or_default(),
                works_again(pro, "until", "until next month")
            ),
            None,
            manage(),
        ),
        Some(ProTrouble::UsedUp) => (
            // The models to download are right below.
            format!(
                "Paused {}{DOT}download a model below to carry on",
                works_again(pro, "until", "until next month")
            ),
            Some(Tone::Caution),
            open(),
        ),
        Some(ProTrouble::Unpaid) => ("Needs attention".to_owned(), Some(Tone::Critical), open()),
        Some(ProTrouble::Ended) => (
            "Subscription ended".to_owned(),
            Some(Tone::Critical),
            open(),
        ),
        None if behind => ("Needs attention".to_owned(), Some(Tone::Caution), open()),
        // Nearly used up: what will happen then, for someone it will stop.
        None if model.in_use && pro.spare.is_none() && hours_low(pro) => (
            format!("{}{DOT}no model on this PC to carry on with", usage(pro)),
            None,
            manage(),
        ),
        None if model.in_use => (usage(pro), None, manage()),
        None => {
            let choose = Action::send(Command::UseModel(model.id.clone()));
            (usage(pro), None, button("Use", choose))
        }
    };
    // Only what dictation depends on right now is said in a color.
    let tone = alarm.filter(|_| model.in_use).unwrap_or(Tone::Secondary);
    // The badge of the model in use, as the rows under it wear it. Not
    // while nothing is typed with it.
    let mut controls = Vec::new();
    if model.in_use && trouble.is_none() {
        controls.push(badge("In use", Tone::Accent));
    }
    controls.push(press.into());
    let line = setting(Icon::Star, &model.name)
        .icon_tone(Tone::Accent)
        .detail(said)
        .detail_tone(tone)
        .control(row(controls));
    Some(card([line.into()]))
}

/// A model that runs at a Google service of the user's own. Until that is
/// set up the row leads to its setup; after that it is used like any other
/// model, and a quiet button leads back into the setup with what is set.
fn google_row(model: &Model, service: GoogleService, ready: bool) -> Node {
    let setup = Action::send(Command::Google(Setup::Open(service))).then(Action::Go(Page::Google));
    let mut controls = Vec::new();
    if model.in_use {
        controls.push(badge("In use", Tone::Accent));
    }
    let detail = if ready {
        if !model.in_use {
            let choose = Action::send(Command::UseModel(model.id.clone()));
            controls.push(button("Use", choose).into());
        }
        controls.push(button("Change", setup).kind(ButtonKind::Subtle).into());
        let yours = match service {
            GoogleService::Gemini => "With your own API key",
            GoogleService::Speech => "With your own Google Cloud project",
        };
        format!("{}{DOT}{yours}", about(&model.summary))
    } else {
        controls.push(button("Set up", setup).into());
        facts(model)
    };
    setting(Icon::Cloud, &model.name)
        .detail(detail)
        .selected(model.in_use)
        .control(row(controls))
        .into()
}

/// The line under a model's name, and its color: where it stands if that
/// needs saying, otherwise what it is.
///
/// A model that lacks a language dictation is set to says that first:
/// quietly if it knows another of them, as a caution if it knows none.
fn detail_line(model: &Model) -> (String, Tone) {
    let line = match &model.state {
        ModelState::Failed { reason } => return (reason.clone(), Tone::Critical),
        ModelState::Installing { done, total } => {
            let line = format!("Downloading{DOT}{}", so_far(*done, *total));
            return (line, Tone::Secondary);
        }
        ModelState::Installed => format!("Downloaded{DOT}{}", facts(model)),
        ModelState::Online | ModelState::Missing => facts(model),
    };
    if model.lacks.is_empty() {
        return (line, Tone::Secondary);
    }
    let tone = if model.knows_none {
        Tone::Caution
    } else {
        Tone::Secondary
    };
    let lacks = listed(&model.lacks);
    (format!("Does not know {lacks}{DOT}{line}"), tone)
}

/// What a model is, on one line: its size, its languages or what it needs,
/// and what it is good at.
fn facts(model: &Model) -> String {
    let about = about(&model.summary);
    let mut parts = Vec::new();
    if model.bytes > 0 {
        parts.push(human_size(model.bytes));
    }
    // The summary often says the detail again; once is enough.
    let repeated = about.to_lowercase().contains(&model.detail.to_lowercase());
    if !model.detail.is_empty() && !repeated {
        parts.push(model.detail.clone());
    }
    if !about.is_empty() {
        parts.push(about.to_owned());
    }
    parts.join(DOT)
}

/// A model's summary without what the page says already: where it runs,
/// which is the heading it is listed under.
fn about(summary: &str) -> &str {
    let summary = summary.trim();
    let summary = ["On this PC.", "Online."]
        .iter()
        .find_map(|place| summary.strip_prefix(place))
        .unwrap_or(summary);
    summary.trim().trim_end_matches('.')
}

/// What is at the right edge of a model's row: the badge of the one in use,
/// then what can be done with it as it stands.
fn controls(model: &Model) -> Vec<Node> {
    let mut controls = Vec::new();
    if model.in_use {
        controls.push(badge("In use", Tone::Accent));
    }
    controls.extend(state_button(model));
    controls.extend(remove_button(model));
    controls
}

/// The button a model's state calls for, if any.
fn state_button(model: &Model) -> Option<Node> {
    let id = model.id.clone();
    let button = match model.state {
        ModelState::Online | ModelState::Installed if model.in_use => return None,
        ModelState::Online | ModelState::Installed => {
            button("Use", Action::send(Command::UseModel(id)))
        }
        ModelState::Missing => {
            button("Download", Action::send(Command::InstallModel(id))).icon(Icon::Download)
        }
        ModelState::Installing { .. } => button("Cancel", Action::send(Command::CancelInstall(id))),
        ModelState::Failed { .. } => button("Retry", Action::send(Command::InstallModel(id))),
    };
    Some(button.into())
}

/// The quiet button that deletes a model, where that is possible: never the
/// one in use or one that is downloading, a built-in one only while it is on
/// this PC, and one the user added in any other state.
fn remove_button(model: &Model) -> Option<Node> {
    let removable = !model.in_use
        && match model.state {
            ModelState::Installing { .. } => false,
            ModelState::Installed => true,
            ModelState::Online | ModelState::Missing | ModelState::Failed { .. } => model.custom,
        };
    if !removable {
        return None;
    }
    let hint = if model.custom {
        format!("Remove {}", model.name)
    } else {
        format!("Remove {} from this PC", model.name)
    };
    let remove = Action::send(Command::RemoveModel(model.id.clone()));
    Some(icon_button(Icon::Delete, hint, remove).into())
}

/// The row that leads to the page for adding a model of one's own.
fn add_row() -> Node {
    setting(Icon::Add, "Add a model")
        .detail("From Hugging Face or a file on this PC")
        .action(Action::Go(Page::AddModel))
        .into()
}

/// The switch that runs local models on the graphics card called `name`.
fn graphics_card_row(name: &str, on: bool) -> Node {
    setting(Icon::Chip, format!("Run on the graphics card ({name})"))
        .detail("Models on this PC run faster on a graphics card than on the processor")
        .control(toggle(on, |on| {
            Action::send(Command::Set(Change::GraphicsCard(on)))
        }))
        .into()
}

/// What the downloads take on disk, and the way to their folder.
fn disk_row(bytes: u64) -> Node {
    let detail = if bytes == 0 {
        "Nothing is downloaded yet".to_owned()
    } else {
        format!("Models and their engine take {}", human_size(bytes))
    };
    setting(Icon::Storage, "Disk space")
        .detail(detail)
        .control(button("Open folder", Action::send(Command::OpenModelsDir)).into())
        .into()
}

#[cfg(test)]
mod tests {
    use vinput_view::{Notice, NoticeKind};

    use super::super::common::probe::{Seen, command, scene, settings};
    use super::*;
    use crate::node::Setting;

    const PROMPT: &str = "Choose a speech model";

    /// The labels and commands of the row titled `name`.
    fn offers(content: &Content, name: &str) -> (Vec<String>, Vec<Command>) {
        let rows = settings(&content.body);
        let Some(found) = rows.iter().find(|line| line.title == name) else {
            unreachable!("no row called {name}");
        };
        let seen = Seen::of_setting(found);
        let labels = seen.labels().into_iter().map(str::to_owned).collect();
        (labels, seen.commands())
    }

    fn row_of<'a>(content: &'a Content, name: &str) -> &'a Setting {
        let rows = settings(&content.body);
        let Some(found) = rows.into_iter().find(|line| line.title == name) else {
            unreachable!("no row called {name}");
        };
        found
    }

    fn id(text: &str) -> String {
        text.to_owned()
    }

    #[test]
    fn a_settled_user_sees_one_model_in_use_and_the_right_action_on_each() {
        let content = content(&scene("settings"));
        let rows = settings(&content.body);
        let chosen: Vec<_> = rows.iter().filter(|line| line.selected).collect();
        assert_eq!(chosen.len(), 1);
        assert_eq!(chosen[0].title, "Whisper base");
        assert_eq!(Seen::of_setting(chosen[0]).badges, ["In use"]);
        assert_eq!(Seen::of(&content.body).badges, ["In use"]);
        assert!(!Seen::of(&content.body).says(PROMPT));

        assert_eq!(offers(&content, "Whisper base"), (vec![], vec![]));
        for (name, model, service) in [
            (
                "Google Cloud Speech (Chirp 3)",
                "chirp_3",
                GoogleService::Speech,
            ),
            (
                "Google Gemini 3.5 Transcribe",
                "gemini-3.5-transcribe",
                GoogleService::Gemini,
            ),
        ] {
            assert_eq!(
                offers(&content, name),
                (
                    vec![id("Use"), id("Change")],
                    vec![
                        Command::UseModel(id(model)),
                        Command::Google(Setup::Open(service))
                    ]
                )
            );
            assert!(row_of(&content, name).detail.contains("With your own"));
        }
        for (name, model) in [
            ("NVIDIA Parakeet TDT v3", "parakeet-tdt-v3"),
            (
                "Whisper large-v3 turbo (compact)",
                "ggml-large-v3-turbo-q5_0",
            ),
        ] {
            assert_eq!(
                offers(&content, name),
                (vec![id("Download")], vec![Command::InstallModel(id(model))])
            );
        }
        assert_eq!(
            offers(&content, "Whisper large-v3 turbo"),
            (
                vec![id("Use"), id("Remove Whisper large-v3 turbo from this PC")],
                vec![
                    Command::UseModel(id("ggml-large-v3-turbo")),
                    Command::RemoveModel(id("ggml-large-v3-turbo")),
                ]
            )
        );
    }

    #[test]
    fn a_new_user_is_told_what_to_do_and_nothing_is_in_use() {
        let content = content(&scene("first-run"));
        let seen = Seen::of(&content.body);
        assert!(seen.says(PROMPT));
        assert_eq!(seen.bars.len(), 1);
        assert!(seen.badges.is_empty());
        assert!(settings(&content.body).iter().all(|line| !line.selected));
        // The engine is missing but nothing is fetching it: not worth a word.
        assert!(seen.progress.is_empty());
        assert!(seen.says("Nothing is downloaded yet"));
        let labels = seen.labels();
        let count = |caption: &str| labels.iter().filter(|label| **label == caption).count();
        // Nothing online can be used before it is set up, or subscribed to.
        assert_eq!(
            (count("Use"), count("Set up"), count("Download")),
            (0, 2, 6)
        );
        assert_eq!(pro(&scene("first-run")).labels, ["See plans"]);
    }

    #[test]
    fn a_google_model_leads_to_its_setup_and_back_into_it() {
        let name = "Google Gemini 3.5 Transcribe";
        let open = Command::Google(Setup::Open(GoogleService::Gemini));
        // No key yet: the one thing to do is to set it up.
        let page = content(&scene("first-run"));
        assert_eq!(
            offers(&page, name),
            (vec![id("Set up")], vec![open.clone()])
        );
        assert!(
            row_of(&page, name)
                .detail
                .contains("Needs a Gemini API key")
        );
        let press = &Seen::of_setting(row_of(&page, name)).presses[0];
        assert!(matches!(
            &press.action,
            Action::Both(_, turn) if matches!(**turn, Action::Go(Page::Google))
        ));

        // In use: no Use, and one quiet way to change the key.
        let mut view = scene("settings");
        for model in &mut view.models.list {
            model.in_use = model.google == Some(GoogleService::Gemini);
        }
        let page = content(&view);
        assert_eq!(offers(&page, name), (vec![id("Change")], vec![open]));
        let seen = Seen::of_setting(row_of(&page, name));
        assert_eq!(seen.badges, ["In use"]);
        assert_eq!(seen.presses[0].kind, ButtonKind::Subtle);

        // Google Cloud without a project is not ready, whatever is signed in.
        view.google.speech_ready = false;
        assert_eq!(
            offers(&content(&view), "Google Cloud Speech (Chirp 3)").0,
            ["Set up"]
        );
    }

    #[test]
    fn models_are_listed_under_where_they_run() {
        let content = content(&scene("settings"));
        let texts = Seen::of(&content.body).texts;
        let at = |text: &str| texts.iter().position(|seen| seen == text);
        let (online, here) = (at("Online"), at("On this PC"));
        assert!(online.is_some() && online < here);
        assert!(at("Google Gemini 3.5 Transcribe") < here);
        assert!(at("Whisper base") > here);
        assert!(at("Add a model") > at("Whisper base"));
    }

    #[test]
    fn a_download_shows_how_far_it_is_and_can_be_cancelled() {
        let content = content(&scene("downloading"));
        let name = "NVIDIA Parakeet TDT v3";
        assert_eq!(
            offers(&content, name),
            (
                vec![id("Cancel")],
                vec![Command::CancelInstall(id("parakeet-tdt-v3"))]
            )
        );
        let line = row_of(&content, name);
        assert!(line.detail.contains("412 MB of 669 MB"));
        let progress = Seen::of_setting(line).progress;
        assert!(matches!(progress.as_slice(), [Some(part)] if (0.61..0.62).contains(part)));
    }

    #[test]
    fn a_failed_download_gives_its_reason_and_can_be_retried() {
        let content = content(&scene("downloading"));
        let name = "Whisper large-v3 turbo (compact)";
        assert_eq!(
            offers(&content, name),
            (
                vec![id("Retry")],
                vec![Command::InstallModel(id("ggml-large-v3-turbo-q5_0"))]
            )
        );
        let line = row_of(&content, name);
        assert_eq!(line.detail_tone, Tone::Critical);
        assert!(line.detail.contains("no connection to huggingface.co"));
    }

    #[test]
    fn the_engine_is_mentioned_only_while_it_matters() {
        let fetching = content(&scene("downloading"));
        let line = row_of(&fetching, "Getting the engine that runs models on this PC");
        assert_eq!(line.detail, "9 MB of 31 MB");
        assert_eq!(Seen::of_setting(line).progress.len(), 1);

        let mut view = scene("settings");
        assert!(!Seen::of(&content(&view).body).says("engine that runs"));
        view.models.engine = Engine::Failed {
            reason: "The disk is full.".to_owned(),
        };
        let bars = Seen::of(&content(&view).body).bars;
        assert!(
            matches!(bars.as_slice(), [(Severity::Critical, text)] if text.ends_with("The disk is full."))
        );
    }

    #[test]
    fn an_added_model_can_be_removed_unless_it_is_busy_or_in_use() {
        let mut view = scene("settings");
        let custom = |state: ModelState, in_use: bool| Model {
            id: id("custom-mine"),
            name: id("Mine"),
            local: true,
            bytes: 1 << 20,
            custom: true,
            in_use,
            state,
            ..Model::default()
        };
        let remove = Command::RemoveModel(id("custom-mine"));
        let failed = ModelState::Failed { reason: id("No") };
        for (state, first) in [
            (ModelState::Missing, "Download"),
            (failed, "Retry"),
            (ModelState::Installed, "Use"),
        ] {
            view.models.list.push(custom(state, false));
            let (labels, commands) = offers(&content(&view), "Mine");
            assert_eq!(labels, [first, "Remove Mine"]);
            assert_eq!(commands.last(), Some(&remove));
            view.models.list.pop();
        }
        let installing = ModelState::Installing { done: 0, total: 0 };
        view.models.list.push(custom(installing, false));
        let page = content(&view);
        assert_eq!(offers(&page, "Mine").0, ["Cancel"]);
        assert_eq!(Seen::of_setting(row_of(&page, "Mine")).progress, [None]);
        view.models.list.pop();

        view.models.list[8].in_use = false;
        view.models.list.push(custom(ModelState::Installed, true));
        let page = content(&view);
        assert_eq!(offers(&page, "Mine"), (vec![], vec![]));
        assert!(row_of(&page, "Mine").selected);
        assert_eq!(Seen::of(&page.body).badges, ["In use"]);
    }

    #[test]
    fn a_notice_shows_on_its_own_page_only_and_can_be_dismissed() {
        let mut view = scene("notice");
        assert!(Seen::of(&content(&view).body).bars.is_empty());
        view.notice = Some(Notice {
            kind: NoticeKind::Success,
            text: id("Whisper base was removed."),
            page: Page::Models,
        });
        let seen = Seen::of(&content(&view).body);
        assert_eq!(
            seen.bars,
            [(Severity::Success, id("Whisper base was removed."))]
        );
        assert_eq!(seen.texts.first(), Some(&id("Whisper base was removed.")));
        assert!(seen.commands().contains(&Command::DismissNotice));
    }

    #[test]
    fn the_facts_about_this_pc_follow_the_models() {
        let mut view = scene("settings");
        let page = content(&view);
        let card = row_of(&page, "Run on the graphics card (NVIDIA GeForce RTX 3090)");
        let toggles = Seen::of_setting(card).toggles;
        let [(true, off)] = toggles.as_slice() else {
            unreachable!("one switch that is on was expected");
        };
        assert_eq!(
            command(off),
            Some(Command::Set(Change::GraphicsCard(false)))
        );
        let disk = row_of(&page, "Disk space");
        assert_eq!(disk.detail, "Models and their engine take 1.9 GB");
        assert_eq!(Seen::of_setting(disk).commands(), [Command::OpenModelsDir]);
        let add = row_of(&page, "Add a model");
        assert!(matches!(add.action, Action::Go(Page::AddModel)));

        view.models.graphics_card = None;
        assert!(Seen::of(&content(&view).body).toggles.is_empty());
    }

    /// The card of `VinPUT` Pro as a test reads it.
    struct ProCard {
        detail: String,
        tone: Tone,
        labels: Vec<String>,
        kinds: Vec<ButtonKind>,
        commands: Vec<Command>,
        to_its_page: bool,
        /// Whether it wears the badge of the model in use.
        in_use: bool,
    }

    /// The card pinned to the top of the page for `view`: one row in a card
    /// of its own, with the star in the accent color and one button, and
    /// the badge of the model in use or none.
    fn pro(view: &View) -> ProCard {
        let page = content(view);
        let [Node::Card(rows)] = page.pinned.as_slice() else {
            unreachable!("one card was expected at the top");
        };
        let [Node::Setting(line)] = rows.as_slice() else {
            unreachable!("one row was expected in it");
        };
        assert_eq!(line.title, "VinPUT Pro");
        assert_eq!(
            (line.icon, line.icon_tone),
            (Some(Icon::Star), Tone::Accent)
        );
        assert!(!line.selected && line.action.is_none() && line.below.is_empty());
        let seen = Seen::of_setting(line);
        assert!(seen.badges.iter().all(|badge| badge == "In use"));
        assert!(seen.badges.len() <= 1 && seen.presses.len() == 1);
        // It is listed once: not among the online models as well.
        let listed = settings(&page.body);
        assert!(listed.iter().all(|row| row.title != line.title));
        ProCard {
            detail: line.detail.clone(),
            tone: line.detail_tone,
            labels: seen.labels().into_iter().map(str::to_owned).collect(),
            kinds: seen.presses.iter().map(|press| press.kind).collect(),
            commands: seen.commands(),
            to_its_page: matches!(seen.presses[0].action, Action::Go(Page::Pro)),
            in_use: !seen.badges.is_empty(),
        }
    }

    /// The scene `name` with dictation switched to a model on this PC.
    fn elsewhere(name: &str) -> View {
        let mut view = scene(name);
        for model in &mut view.models.list {
            model.in_use = model.id == "ggml-base";
        }
        view
    }

    #[test]
    fn pro_has_a_card_of_its_own_at_the_top_in_every_scene() {
        for name in crate::mock::SCENES {
            let card = pro(&scene(name));
            assert!(!card.detail.is_empty(), "{name}");
            assert!(!card.detail.contains(". "), "{name}: {}", card.detail);
            // One line beside its button, as the pictures show.
            assert!(card.detail.chars().count() <= 60, "{name}: {}", card.detail);
        }
        // A list without it has nothing pinned over it.
        let mut view = scene("settings");
        view.models.list.retain(|model| model.id != PRO_MODEL);
        assert!(content(&view).pinned.is_empty());
    }

    #[test]
    fn pro_says_what_it_costs_to_someone_who_has_no_subscription() {
        // What it is and what it costs, and the way to the plans. Nothing
        // about it is to be "set up".
        for name in [
            "settings",
            "pro-signing-in",
            "pro-not-subscribed",
            "pro-awaiting-payment",
        ] {
            let card = pro(&scene(name));
            assert_eq!(
                card.detail,
                "Types as you speak, in 60+ languages \u{b7} from \u{20ac}2.50 a month"
            );
            assert_eq!(card.tone, Tone::Secondary);
            assert_eq!(card.labels, ["See plans"], "{name}");
            // The button is what makes the card easy to find: the one of
            // the page in the accent color, with no tint or border beside.
            assert_eq!(card.kinds, [ButtonKind::Accent]);
            assert!(card.to_its_page && card.commands.is_empty() && !card.in_use);
            let page = Seen::of(&content(&scene(name)).body);
            let accent = page.presses.iter();
            let accent = accent.filter(|press| press.kind == ButtonKind::Accent);
            assert_eq!(accent.count(), 0, "{name}");
        }
        // The price is the least a month costs, as the server gave it.
        let mut view = scene("pro-not-subscribed");
        view.pro.offers[0].monthly = 275;
        view.pro.currency = "usd".to_owned();
        assert!(pro(&view).detail.ends_with("\u{b7} from $2.75 a month"));
        view.pro.offers.remove(0);
        assert!(pro(&view).detail.ends_with("\u{b7} from $5 a month"));
        // A price for the first month is the plans' to say: the line here
        // has no room for both, and stays what a month costs.
        for name in ["pro-intro", "pro-intro-signed-out"] {
            let detail = pro(&scene(name)).detail;
            assert!(
                detail.ends_with("\u{b7} from \u{20ac}2.50 a month"),
                "{name}"
            );
        }
    }

    #[test]
    fn pro_shows_a_subscriber_the_use_of_the_month_and_whether_it_is_in_use() {
        // In use: the badge the rows of the models wear, the use of the
        // month against what the tier comes with, and a quiet way to its
        // page.
        for (name, used) in [
            ("pro-subscribed", "12 h 30 min of 25 hours"),
            ("pro-complimentary", "12 h 30 min of 25 hours"),
            ("pro-offline-known", "12 h 30 min of 25 hours"),
            ("pro-plus", "3 h 10 min of 8 hours"),
        ] {
            let card = pro(&scene(name));
            assert_eq!(card.detail, used, "{name}");
            assert!(card.in_use, "{name}");
            assert_eq!(card.tone, Tone::Secondary);
            assert_eq!(card.labels, ["Manage"]);
            assert_eq!(card.kinds, [ButtonKind::Subtle]);
            assert!(card.to_its_page);
        }
        // Another model in use: one press switches to it.
        for name in ["pro-subscribed-elsewhere", "pro-complimentary"] {
            let card = pro(&elsewhere(name));
            assert_eq!(card.detail, "12 h 30 min of 25 hours", "{name}");
            assert!(!card.in_use);
            assert_eq!(card.labels, ["Use"]);
            assert_eq!(card.kinds, [ButtonKind::Standard]);
            assert_eq!(card.commands, [Command::UseModel(id(PRO_MODEL))]);
        }
    }

    #[test]
    fn pro_says_what_types_while_the_hours_of_the_month_are_used_up() {
        // A model on this PC carries on: said quietly, since nothing
        // stops, and without the badge, since Pro types nothing.
        for name in ["pro-over-allowance", "pro-plus-used-up"] {
            let card = pro(&scene(name));
            assert_eq!(
                card.detail,
                "Hours used up \u{b7} using Whisper base until 1 November"
            );
            assert_eq!(card.tone, Tone::Secondary, "{name}");
            assert!(!card.in_use && card.to_its_page);
            assert_eq!(card.labels, ["Manage"]);
        }
        let mut view = scene("pro-over-allowance");
        view.pro.resets_at = 0;
        assert!(pro(&view).detail.ends_with("until next month"));
        // No model on this PC: dictation has stopped, and what to do about
        // it is in the list right under the card.
        let card = pro(&scene("pro-used-up-no-model"));
        assert_eq!(
            card.detail,
            "Paused until 1 November \u{b7} download a model below to carry on"
        );
        assert_eq!(card.tone, Tone::Caution);
        assert!(!card.in_use);
        assert_eq!(card.labels, ["Open VinPUT Pro"]);
        // Nearly used up, it is said before it happens; with plenty left
        // or a model on this PC, never.
        let card = pro(&scene("pro-low-no-model"));
        assert_eq!(
            card.detail,
            "7 h 35 min of 8 hours \u{b7} no model on this PC to carry on with"
        );
        assert!(card.in_use && card.tone == Tone::Secondary);
        let mut view = scene("pro-low-no-model");
        view.pro.used_seconds = 7 * 3600;
        assert_eq!(pro(&view).detail, "7 h of 8 hours");
        view.pro.used_seconds = 8 * 3600 - 1500;
        view.pro.spare = Some(id("Whisper base"));
        assert_eq!(pro(&view).detail, "7 h 35 min of 8 hours");
    }

    #[test]
    fn pro_says_briefly_what_is_wrong_and_leads_to_its_page() {
        // What dictation depends on is said in a color; what it does not
        // depend on right now is said quietly.
        for (name, words, alarm) in [
            ("pro-payment-failed", "Needs attention", Tone::Critical),
            ("pro-expired", "Subscription ended", Tone::Critical),
        ] {
            let mut view = scene(name);
            for model in &mut view.models.list {
                model.in_use = model.id == PRO_MODEL;
            }
            for (view, tone) in [(view, alarm), (elsewhere(name), Tone::Secondary)] {
                let card = pro(&view);
                assert_eq!((card.detail.as_str(), card.tone), (words, tone), "{name}");
                assert_eq!(card.labels, ["Open VinPUT Pro"]);
                assert_eq!(card.kinds, [ButtonKind::Standard]);
                assert!(card.to_its_page && card.commands.is_empty());
            }
        }
        // A server that does not say when the month is over: no date is
        // made up.
        let mut view = scene("pro-used-up-no-model");
        view.pro.resets_at = 0;
        assert!(
            pro(&view)
                .detail
                .starts_with("Paused until next month \u{b7}")
        );

        // A renewal that failed while Pro still works.
        let card = pro(&scene("pro-payment-problem"));
        assert_eq!(card.detail, "Needs attention");
        assert!(card.in_use);
        assert_eq!(card.tone, Tone::Caution);
        assert_eq!(card.labels, ["Open VinPUT Pro"]);
        let card = pro(&elsewhere("pro-payment-problem"));
        assert_eq!(card.detail, "Needs attention");
        assert_eq!(card.tone, Tone::Secondary);
        // Nothing is owed for an account that is given Pro.
        let mut view = scene("pro-complimentary");
        view.pro.status = id("past_due");
        assert_eq!(pro(&view).labels, ["Manage"]);
    }

    #[test]
    fn pro_says_so_when_the_account_could_not_be_checked() {
        let mut view = scene("pro-offline");
        let card = pro(&view);
        assert_eq!(card.detail, "Could not check your subscription");
        assert!(card.in_use);
        assert_eq!(card.tone, Tone::Secondary);
        assert_eq!(card.labels, ["Open VinPUT Pro"]);
        assert!(card.to_its_page);
        assert_eq!(
            pro(&elsewhere("pro-offline")).detail,
            "Could not check your subscription"
        );
        // While the server is being asked for the first time, nothing has
        // failed yet.
        view.pro.problem = None;
        view.pro.doing = ProDoing::Asking;
        assert_eq!(pro(&view).detail, "Checking your subscription");
    }

    #[test]
    fn every_line_of_detail_is_set_with_dots_and_nothing_usable_says_it_needs_something() {
        for name in crate::mock::SCENES {
            let page = content(&scene(name));
            for row in settings(&page.body) {
                let offered = Seen::of_setting(row);
                let needs = row.detail.contains("Needs ");
                assert!(
                    !(needs && offered.labels().contains(&"Use")),
                    "{name}: {} offers Use and says {}",
                    row.title,
                    row.detail
                );
                // Facts are set apart by dots, not strung into sentences.
                if needs || row.detail.contains("Types as you speak") {
                    assert!(!row.detail.contains(". "), "{name}: {}", row.detail);
                }
            }
        }
    }

    #[test]
    fn a_model_says_which_of_the_languages_it_does_not_know() {
        const CAUTION: &str = "This model does not know Slovenian. Choose another model, or \
                               change the languages under General.";
        let qwen = "Qwen3-ASR 0.6B";
        // English and Slovenian, and a model for both in use: the ones
        // that lack Slovenian say so quietly, and nothing is warned of.
        let page = content(&scene("settings"));
        let line = row_of(&page, qwen);
        assert!(line.detail.starts_with("Does not know Slovenian \u{b7} "));
        assert_eq!(line.detail_tone, Tone::Secondary);
        assert!(!row_of(&page, "Whisper base").detail.contains("not know"));
        assert!(Seen::of(&page.body).bars.is_empty());
        // English alone: nothing to say.
        let page = content(&scene("first-run"));
        assert!(!Seen::of(&page.body).says("not know"));

        // Slovenian alone, and one of them in use: its row is a caution,
        // and a bar says what to do. Dictation is not held back for it.
        let page = content(&scene("wrong-language"));
        let line = row_of(&page, qwen);
        assert!(
            line.selected
                && line
                    .detail
                    .starts_with("Does not know Slovenian \u{b7} Downloaded")
        );
        assert_eq!(line.detail_tone, Tone::Caution);
        assert_eq!(
            Seen::of(&page.body).bars,
            [(Severity::Caution, id(CAUTION))]
        );
        // The other one says it before it is downloaded or put to use.
        let other = row_of(&page, "Qwen3-ASR 1.7B");
        assert_eq!(other.detail_tone, Tone::Caution);
        assert_eq!(offers(&page, "Qwen3-ASR 1.7B").0, ["Download"]);
        assert_eq!(row_of(&page, "Whisper base").detail_tone, Tone::Secondary);
        // A download under way and one that failed keep their own line.
        let mut view = scene("wrong-language");
        view.models.list[3].state = ModelState::Installing { done: 1, total: 2 };
        let page = content(&view);
        assert!(
            row_of(&page, "Qwen3-ASR 1.7B")
                .detail
                .starts_with("Downloading")
        );

        let names = |names: &[&str]| -> Vec<String> { names.iter().map(|name| id(name)).collect() };
        assert_eq!(listed(&names(&["Slovenian"])), "Slovenian");
        assert_eq!(
            listed(&names(&["Slovenian", "Croatian"])),
            "Slovenian or Croatian"
        );
        assert_eq!(
            listed(&names(&["Slovenian", "Croatian", "Serbian"])),
            "Slovenian, Croatian or Serbian"
        );
    }

    #[test]
    fn a_model_is_described_once_and_briefly() {
        let model = |summary: &str, detail: &str, bytes: u64| Model {
            summary: id(summary),
            detail: id(detail),
            bytes,
            ..Model::default()
        };
        assert_eq!(
            facts(&model(
                "On this PC. Small and fast on any processor, lower accuracy.",
                "99 languages",
                148 << 20
            )),
            "148 MB \u{b7} 99 languages \u{b7} Small and fast on any processor, lower accuracy"
        );
        assert_eq!(
            facts(&model(
                "Online. Types as you speak. Needs a Gemini API key.",
                "Needs a Gemini API key",
                0
            )),
            "Types as you speak. Needs a Gemini API key"
        );
        assert_eq!(facts(&model("", "", 0)), "");
    }
}
