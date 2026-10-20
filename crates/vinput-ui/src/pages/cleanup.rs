//! The Cleanup page: what is done with a dictation before it is typed, the
//! key that edits the selected text, and the model that does both.
//!
//! Each of the two keys that type has its own way to tidy, picked from a
//! list on its row. The key that edits is set with the other keys, on the
//! Shortcuts page; a row here names it and goes there.
//! The model is named on one row, which opens onto the one that comes with
//! the subscription and the ones that run on this PC, each a row with its
//! own actions as on the Models page. The model on this PC that is in use
//! says where its engine stands: getting ready, ready on the graphics card
//! or on the processor.

use vinput_view::{
    Change, Command, Model, ModelState, Page, Rewrite, Settings, Standing, Tidy, View,
};

use super::common::{DOT, ProTrouble, notice_bar, pro_trouble};
use super::models::{detail_line, remove_button};
use crate::node::{
    Action, ButtonKind, Content, Icon, Node, Severity, Tone, badge, button, card, choice, expander,
    header, note, row, scroll, setting,
};
use crate::theme::metric;

/// The ways to tidy a dictation, in the order they are offered, with what
/// each is called.
const WAYS: [(Tidy, &str); 4] = [
    (Tidy::Plain, "As said"),
    (Tidy::Clean, "Clean"),
    (Tidy::Prompt, "Coding prompt"),
    (Tidy::Email, "Email"),
];

/// What the model that comes with the subscription is called.
const PRO_NAME: &str = "VinPUT Plus and Pro";

/// What the page shows for `view`.
#[must_use]
pub fn content(view: &View) -> Content {
    let mut body = Vec::new();
    body.extend(notice_bar(view, Page::Cleanup));
    body.extend(no_model(view));
    body.extend(after_dictation(&view.settings));
    body.extend(model(view));
    body.extend(edit_by_voice(&view.settings));
    Content {
        title: "Cleanup".to_owned(),
        pinned: Vec::new(),
        body,
        footer: Vec::new(),
        back: None,
    }
}

/// Whether the settings ask for anything to be tidied or edited.
fn in_use(settings: &Settings) -> bool {
    settings.cleanup != Tidy::Plain
        || (settings.cleanup_enter != Tidy::Plain && !settings.hotkey_enter.is_empty())
        || !settings.hotkey_command.is_empty()
}

/// Why the model of the subscription cannot be had, if it cannot: the
/// hours of the month have no say in it.
fn no_plan(view: &View) -> Option<ProTrouble> {
    pro_trouble(&view.pro).filter(|trouble| *trouble != ProTrouble::UsedUp)
}

/// What to do when something is to be tidied or edited and the model that
/// is chosen cannot do it.
fn no_model(view: &View) -> Option<Node> {
    if !in_use(&view.settings) {
        return None;
    }
    let rewrite = &view.rewrite;
    if rewrite.with_pro {
        no_plan(view)?;
        return Some(Node::InfoBar {
            severity: Severity::Caution,
            text: "This needs VinPUT Plus or Pro, or a text model on this PC. Until then, \
                   dictation is typed as you say it."
                .to_owned(),
            action: Some(("See plans".to_owned(), Action::Go(Page::Pro))),
            dismiss: None,
        });
    }
    let chosen = rewrite.models.iter().find(|model| model.in_use);
    let here = chosen.is_some_and(|model| model.state == ModelState::Installed);
    (!here).then(|| Node::InfoBar {
        severity: Severity::Caution,
        text: "The text model is not on this PC. Download it below, or choose another.".to_owned(),
        action: None,
        dismiss: None,
    })
}

/// The row that picks how the dictations of one key are tidied. `key` is
/// the key as its field shows it; a key that is not set has nothing to
/// pick for.
fn way_row(
    name: &'static str,
    title: &str,
    key: &str,
    chosen: Tidy,
    change: fn(Tidy) -> Change,
) -> Node {
    let names: Vec<String> = WAYS.iter().map(|(_, name)| (*name).to_owned()).collect();
    let selected = WAYS.iter().position(|(way, _)| *way == chosen).unwrap_or(0);
    let detail = if key.is_empty() { "No key set" } else { key };
    setting(Icon::Keyboard, title)
        .detail(detail)
        .enabled(!key.is_empty())
        .control(choice(name, names, selected, move |index| {
            match WAYS.get(index) {
                Some((way, _)) if index != selected => Action::send(Command::Set(change(*way))),
                _ => Action::None,
            }
        }))
        .into()
}

/// How the dictations of each key that types are tidied.
fn after_dictation(settings: &Settings) -> [Node; 3] {
    [
        header("After dictation"),
        card([
            way_row(
                "cleanup",
                "Dictate",
                &settings.hotkey,
                settings.cleanup,
                Change::Cleanup,
            ),
            way_row(
                "cleanup-enter",
                "Dictate, then press Enter",
                &settings.hotkey_enter,
                settings.cleanup_enter,
                Change::CleanupEnter,
            ),
        ]),
        note(
            "Clean takes out filler words, false starts and repeats, and fixes punctuation. \
             Coding prompt and Email rewrite what you said for that use.",
        ),
    ]
}

/// The key that edits the selected text, which the same model serves. The
/// key is set with the others, on the Shortcuts page; this row says which
/// it is and goes there.
fn edit_by_voice(settings: &Settings) -> [Node; 2] {
    let key = if settings.hotkey_command.is_empty() {
        "No key set. Choose one on the Shortcuts page".to_owned()
    } else {
        format!(
            "Select text, hold {} and say what to change",
            settings.hotkey_command
        )
    };
    [
        header("Edit by voice"),
        card([setting(Icon::Keyboard, "Edit the selected text")
            .detail(key)
            .action(Action::Go(Page::Shortcuts))
            .into()]),
    ]
}

/// Where the engine of the model in use stands, in words, and whether that
/// is something gone wrong.
fn standing_words(standing: &Standing) -> (String, Tone) {
    let words = match standing {
        Standing::Idle => "",
        Standing::Loading {
            read: Some(hundredths),
            ..
        } => return (format!("Loading, {hundredths}%"), Tone::Secondary),
        Standing::Loading { card: true, .. } => {
            "Getting the graphics card ready. The first time takes a minute"
        }
        Standing::Loading { card: false, .. } => "Getting ready",
        Standing::Ready { card: true, .. } => "Ready on the graphics card",
        Standing::Ready {
            fell_back: true, ..
        } => "Ready on the processor. The graphics card could not run it",
        Standing::Ready { .. } => "Ready on the processor",
        Standing::Failed { reason } => return (reason.clone(), Tone::Critical),
    };
    (words.to_owned(), Tone::Secondary)
}

/// The model that comes with the subscription, as a row of the list.
fn pro_row(view: &View) -> Node {
    let in_use = view.rewrite.with_pro;
    let mut controls = Vec::new();
    let (detail, tone) = if no_plan(view).is_some() {
        controls.push(
            button("See plans", Action::Go(Page::Pro))
                .kind(ButtonKind::Subtle)
                .into(),
        );
        let tone = if in_use {
            Tone::Caution
        } else {
            Tone::Secondary
        };
        ("Needs a subscription".to_owned(), tone)
    } else {
        let detail = format!("Online{DOT}Answers in under a second on any PC");
        (detail, Tone::Secondary)
    };
    if in_use {
        controls.insert(0, badge("In use", Tone::Accent));
    } else {
        let choose = Action::send(Command::UseTextModel(String::new()));
        controls.insert(0, button("Use", choose).into());
    }
    setting(Icon::Cloud, PRO_NAME)
        .detail(detail)
        .detail_tone(tone)
        .selected(in_use)
        .control(row(controls))
        .into()
}

/// The button the state of a model on this PC calls for, if any.
fn state_button(model: &Model) -> Option<Node> {
    let id = model.id.clone();
    let button = match model.state {
        ModelState::Online | ModelState::Installed if model.in_use => return None,
        ModelState::Online | ModelState::Installed => {
            button("Use", Action::send(Command::UseTextModel(id)))
        }
        ModelState::Missing => {
            button("Download", Action::send(Command::InstallModel(id))).icon(Icon::Download)
        }
        ModelState::Installing { .. } => button("Cancel", Action::send(Command::CancelInstall(id))),
        ModelState::Failed { .. } => button("Retry", Action::send(Command::InstallModel(id))),
    };
    Some(button.into())
}

/// One model on this PC: what it is, where it stands and what can be done
/// with it. The one in use says where its engine stands first.
fn local_row(model: &Model, standing: &Standing) -> Node {
    let (mut detail, mut tone) = detail_line(model);
    if model.in_use && model.state == ModelState::Installed {
        let (stands, alarm) = standing_words(standing);
        if !stands.is_empty() {
            detail = format!("{stands}{DOT}{detail}");
            tone = alarm;
        }
    }
    let mut controls = Vec::new();
    if model.in_use {
        controls.push(badge("In use", Tone::Accent));
    }
    controls.extend(state_button(model));
    controls.extend(remove_button(model));
    let mut line = setting(Icon::Model, &model.name)
        .detail(detail)
        .detail_tone(tone)
        .selected(model.in_use)
        .control(row(controls));
    if let ModelState::Installing { done, total } = model.state {
        line = line.below(Node::Progress(super::common::fraction(done, total)));
    }
    line.into()
}

/// What the closed row calls the model in use.
fn chosen_name(rewrite: &Rewrite) -> &str {
    if rewrite.with_pro {
        return PRO_NAME;
    }
    let chosen = rewrite.models.iter().find(|model| model.in_use);
    chosen.map_or("Not chosen", |model| model.name.as_str())
}

/// The row that names the model in use and opens onto all of them.
fn model(view: &View) -> [Node; 3] {
    let rewrite = &view.rewrite;
    let busy = |model: &Model| {
        matches!(
            model.state,
            ModelState::Installing { .. } | ModelState::Failed { .. }
        )
    };
    let mut list = vec![pro_row(view), header("On this PC")];
    list.extend(
        rewrite
            .models
            .iter()
            .map(|model| local_row(model, &rewrite.standing)),
    );
    let settling = matches!(
        rewrite.standing,
        Standing::Loading { .. } | Standing::Failed { .. }
    );
    let picker = expander(
        "text-model",
        Icon::Model,
        "Text model",
        [scroll("text-models", metric::LIST_MAX, list)],
    )
    .value(chosen_name(rewrite))
    .open(rewrite.models.iter().any(busy) || (!rewrite.with_pro && settling));
    [
        header("Model"),
        card([picker.into()]),
        note(
            "A model on this PC keeps what you say on this PC. Cleanup and editing are not \
             counted against your monthly hours.",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::super::common::probe::{Seen, all, scene, set, setting_row, settings};
    use super::*;

    /// The badges and labels, and the commands, of the row titled `name`.
    fn offers(content: &Content, name: &str) -> (Vec<String>, Vec<Command>) {
        let rows = settings(&content.body);
        let Some(found) = rows.iter().find(|line| line.title == name) else {
            unreachable!("no row called {name}");
        };
        let seen = Seen::of_setting(found);
        let pressed = seen.labels().into_iter().map(str::to_owned);
        let labels = seen.badges.iter().cloned().chain(pressed).collect();
        (labels, seen.commands())
    }

    fn bars(content: &Content) -> Vec<String> {
        let texts = all(&content.body)
            .into_iter()
            .filter_map(|node| match node {
                Node::InfoBar { text, .. } => Some(text.clone()),
                _ => None,
            });
        texts.collect()
    }

    #[test]
    fn the_page_has_the_two_keys_that_type_the_key_that_edits_and_the_model() {
        let page = content(&scene("cleanup"));
        assert_eq!(page.title, "Cleanup");
        let headers: Vec<_> = page
            .body
            .iter()
            .filter_map(|node| match node {
                Node::Header(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(headers, ["After dictation", "Model", "Edit by voice"]);
        let nodes = all(&page.body);
        for title in ["Dictate", "Dictate, then press Enter"] {
            assert!(setting_row(&nodes, title).control.is_some(), "{title}");
        }
        // Each key says which key it is.
        assert_eq!(setting_row(&nodes, "Dictate").detail, "F1");
        assert_eq!(
            setting_row(&nodes, "Dictate, then press Enter").detail,
            "F2"
        );
        assert!(bars(&page).is_empty(), "{:?}", bars(&page));
    }

    #[test]
    fn each_key_is_given_its_own_way_and_only_a_new_one_is_sent() {
        let view = scene("cleanup");
        let page = content(&view);
        let nodes = all(&page.body);
        let pick = |title: &str, index: usize| {
            let row = setting_row(&nodes, title);
            match row.control.as_deref() {
                Some(Node::Choice { action, .. }) => set(&action(index)),
                _ => unreachable!("{title} has no list"),
            }
        };
        // The scene tidies the first key and makes prompts of the second.
        assert_eq!(pick("Dictate", 3), Some(Change::Cleanup(Tidy::Email)));
        assert_eq!(pick("Dictate", 1), None);
        assert_eq!(
            pick("Dictate, then press Enter", 0),
            Some(Change::CleanupEnter(Tidy::Plain))
        );
        assert_eq!(pick("Dictate, then press Enter", 2), None);
        assert_eq!(pick("Dictate", 9), None);
    }

    #[test]
    fn a_key_that_is_not_set_has_nothing_to_pick_for() {
        let mut view = scene("cleanup");
        view.settings.hotkey_enter = String::new();
        let page = content(&view);
        let nodes = all(&page.body);
        let row = setting_row(&nodes, "Dictate, then press Enter");
        assert_eq!(row.detail, "No key set");
        assert!(!row.enabled);
    }

    #[test]
    fn the_key_that_edits_is_named_here_and_set_with_the_other_keys() {
        let mut view = scene("cleanup");
        let page = content(&view);
        let nodes = all(&page.body);
        let row = setting_row(&nodes, "Edit the selected text");
        assert!(row.detail.contains("F8"), "{}", row.detail);
        assert!(matches!(row.action, Action::Go(Page::Shortcuts)));
        assert!(row.control.is_none());
        view.settings.hotkey_command.clear();
        let page = content(&view);
        let nodes = all(&page.body);
        let row = setting_row(&nodes, "Edit the selected text");
        assert!(row.detail.starts_with("No key set"), "{}", row.detail);
    }

    #[test]
    fn the_model_of_the_subscription_is_in_use_until_one_on_this_pc_is_chosen() {
        let page = content(&scene("cleanup"));
        let (labels, commands) = offers(&page, PRO_NAME);
        assert!(labels.contains(&"In use".to_owned()), "{labels:?}");
        assert!(commands.is_empty());
        // A model that is not here is downloaded; one that is, is used.
        let (labels, commands) = offers(&page, "Gemma 4 E4B");
        assert_eq!(labels, ["Download"]);
        assert_eq!(commands, [Command::InstallModel("gemma-4-e4b".to_owned())]);
        let (labels, commands) = offers(&page, "Qwen3.5 2B");
        assert!(labels.contains(&"Use".to_owned()), "{labels:?}");
        assert!(commands.contains(&Command::UseTextModel("qwen3.5-2b".to_owned())));
        assert!(commands.contains(&Command::RemoveModel("qwen3.5-2b".to_owned())));
    }

    #[test]
    fn a_model_on_this_pc_says_where_its_engine_stands() {
        let view = scene("cleanup-local");
        let page = content(&view);
        let nodes = all(&page.body);
        let row = setting_row(&nodes, "Gemma 4 E2B");
        assert!(row.selected);
        assert!(
            row.detail.starts_with("Ready on the graphics card"),
            "{}",
            row.detail
        );
        // The subscription's model can be gone back to.
        let (labels, commands) = offers(&page, PRO_NAME);
        assert!(labels.contains(&"Use".to_owned()));
        assert!(commands.contains(&Command::UseTextModel(String::new())));
        // One that is on its way shows how far it is and can be stopped.
        let (labels, _) = offers(&page, "Qwen3.5 4B");
        assert_eq!(labels, ["Cancel"]);
        for (standing, words, tone) in [
            (Standing::Idle, "Downloaded", Tone::Secondary),
            (
                Standing::Loading {
                    card: true,
                    read: Some(40),
                },
                "Loading, 40%",
                Tone::Secondary,
            ),
            (
                Standing::Loading {
                    card: true,
                    read: None,
                },
                "Getting the graphics card ready",
                Tone::Secondary,
            ),
            (
                Standing::Loading {
                    card: false,
                    read: None,
                },
                "Getting ready",
                Tone::Secondary,
            ),
            (
                Standing::Ready {
                    card: false,
                    fell_back: true,
                },
                "Ready on the processor. The graphics card could not run it",
                Tone::Secondary,
            ),
            (
                Standing::Failed {
                    reason: "The model on this PC could not be started.".to_owned(),
                },
                "The model on this PC could not be started.",
                Tone::Critical,
            ),
        ] {
            let mut view = view.clone();
            view.rewrite.standing = standing;
            let page = content(&view);
            let nodes = all(&page.body);
            let row = setting_row(&nodes, "Gemma 4 E2B");
            assert!(row.detail.starts_with(words), "{}", row.detail);
            assert_eq!(row.detail_tone, tone, "{words}");
        }
    }

    #[test]
    fn without_a_plan_or_a_model_the_page_says_what_is_missing() {
        // Signed out, with a key that tidies: said once, with the way out.
        let view = scene("cleanup-no-plan");
        let page = content(&view);
        let said = bars(&page);
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].contains("VinPUT Plus or Pro"));
        let (labels, _) = offers(&page, PRO_NAME);
        assert!(labels.contains(&"See plans".to_owned()), "{labels:?}");
        // With nothing to tidy and no key that edits, nothing is missing.
        let mut idle = view.clone();
        idle.settings.cleanup = Tidy::Plain;
        idle.settings.cleanup_enter = Tidy::Plain;
        idle.settings.hotkey_command = String::new();
        assert!(bars(&content(&idle)).is_empty());
        // A model on this PC that is chosen and gone.
        let mut gone = scene("cleanup-local");
        for model in &mut gone.rewrite.models {
            model.state = ModelState::Missing;
        }
        let said = bars(&content(&gone));
        assert!(
            said.iter().any(|text| text.contains("not on this PC")),
            "{said:?}"
        );
        // The month's hours have no say in it.
        let mut used_up = scene("pro-plus-used-up");
        used_up.settings.cleanup = Tidy::Clean;
        assert!(bars(&content(&used_up)).is_empty());
    }
}
