//! The General page: what `VinPUT` listens with and for, and the everyday
//! switches. The keys are on the Shortcuts page.

use vinput_view::{Change, Command, Microphone, Page, Settings, View};

use super::common::{notice_bar, pro_in_use, pro_trouble};
use super::languages;
use crate::node::{
    Action, Content, Icon, Node, Severity, Tone, card, choice, expander, header, plain_setting,
    setting, toggle,
};

/// What the list of audio inputs calls the one Windows uses by default.
const DEFAULT_MICROPHONE: &str = "Default (system)";
/// What the list calls a chosen audio input that Windows no longer has.
const MISSING_MICROPHONE: &str = "Not connected";

/// What the page shows for `view`.
#[must_use]
pub fn content(view: &View) -> Content {
    let mut body = Vec::new();
    body.extend(notice_bar(view, Page::General));
    body.extend(model_prompt(view));
    body.extend(pro_prompt(view));
    body.extend(input(view));
    body.extend(dictation(&view.settings));
    body.extend(startup(&view.settings));
    Content {
        pinned: Vec::new(),
        title: "General".to_owned(),
        body,
        footer: Vec::new(),
        back: None,
    }
}

/// The way to the Models page for someone who has no speech model yet.
fn model_prompt(view: &View) -> Option<Node> {
    if view.status.model.is_some() {
        return None;
    }
    Some(Node::InfoBar {
        severity: Severity::Caution,
        text: "Choose a speech model to start dictating".to_owned(),
        action: Some(("Choose a model".to_owned(), Action::Go(Page::Models))),
        dismiss: None,
    })
}

/// The way to the account for someone who dictates with `VinPUT` Pro while
/// it cannot be used, with the reason.
fn pro_prompt(view: &View) -> Option<Node> {
    let why = pro_trouble(&view.pro).filter(|_| pro_in_use(view))?;
    Some(Node::InfoBar {
        severity: Severity::Caution,
        text: format!(
            "Dictation with VinPUT Pro does not work right now. {}.",
            why.words()
        ),
        action: Some(("Open VinPUT Pro".to_owned(), Action::Go(Page::Pro))),
        dismiss: None,
    })
}

/// The row that picks the audio input. A chosen input that Windows no longer
/// has stays in the list, so the choice is kept until it is plugged in again.
fn microphone_row(chosen: &str, microphones: &[Microphone]) -> Node {
    let mut names = vec![DEFAULT_MICROPHONE.to_owned()];
    let mut ids = vec![String::new()];
    for microphone in microphones {
        names.push(microphone.name.clone());
        ids.push(microphone.id.clone());
    }
    let found = ids.iter().position(|id| id == chosen);
    let selected = found.unwrap_or(ids.len());
    let (detail, tone) = if found.is_some() {
        ("", Tone::Secondary)
    } else {
        names.push(MISSING_MICROPHONE.to_owned());
        ids.push(chosen.to_owned());
        (
            "The microphone you chose is not connected. VinPUT uses the Windows default until \
             it is back.",
            Tone::Caution,
        )
    };
    setting(Icon::Microphone, "Microphone")
        .detail(detail)
        .detail_tone(tone)
        .control(choice(
            "microphone",
            names,
            selected,
            move |index| match ids.get(index) {
                Some(id) if index != selected => {
                    Action::send(Command::Set(Change::Microphone(id.clone())))
                }
                _ => Action::None,
            },
        ))
        .into()
}

/// What the app listens with and what it listens for.
fn input(view: &View) -> [Node; 2] {
    let settings = &view.settings;
    // VinPUT Pro leans towards the languages it is told of, and can take
    // speech in another one for a third: worth one quiet line, here, to
    // whoever has named a single language.
    let more = pro_in_use(view) && languages::count(&settings.languages) == 1;
    [
        header("Speech"),
        card([
            microphone_row(&settings.microphone, &view.microphones),
            languages::picker(&settings.languages, &view.system_languages, more).into(),
        ]),
    ]
}

/// The row of the two sounds of a dictation. Closed, it says which of them
/// are played.
fn sounds_row(settings: &Settings) -> Node {
    let played = match (settings.ready_sound, settings.stop_sound) {
        (true, true) => "Start and stop",
        (true, false) => "Start",
        (false, true) => "Stop",
        (false, false) => "Off",
    };
    expander(
        "sounds",
        Icon::Volume,
        "Sounds",
        [
            plain_setting("When listening starts")
                .indented()
                .compact()
                .control(toggle(settings.ready_sound, |on| {
                    Action::send(Command::Set(Change::ReadySound(on)))
                }))
                .into(),
            plain_setting("When listening stops")
                .indented()
                .compact()
                .control(toggle(settings.stop_sound, |on| {
                    Action::send(Command::Set(Change::StopSound(on)))
                }))
                .into(),
        ],
    )
    .value(played)
    .into()
}

/// How a dictation goes: when its words appear, what it sounds like and
/// when it gives up.
fn dictation(settings: &Settings) -> [Node; 2] {
    [
        header("Dictation"),
        card([
            setting(Icon::Keyboard, "Type words as you say them")
                .detail("With VinPUT Pro. Otherwise the text comes when you stop")
                .control(toggle(settings.live_typing, |on| {
                    Action::send(Command::Set(Change::LiveTyping(on)))
                }))
                .into(),
            sounds_row(settings),
            stop_after_row(settings.stop_after_minutes),
        ]),
    ]
}

/// Whether `VinPUT` is there when Windows starts.
fn startup(settings: &Settings) -> [Node; 2] {
    [
        header("Startup"),
        card([setting(Icon::Power, "Start with Windows")
            .detail("Ready in the tray when you sign in")
            .control(toggle(settings.autostart, |on| {
                Action::send(Command::Set(Change::Autostart(on)))
            }))
            .into()]),
    ]
}

/// The times a dictation may be given before it stops for want of a sound,
/// in minutes.
const STOP_AFTER: [u32; 5] = [1, 5, 15, 30, 60];

/// The row that says how long a dictation nobody speaks into is left
/// listening. A time from the settings file that is not on offer is shown
/// as it is, so that the list never names one that is not in use.
fn stop_after_row(chosen: u32) -> Node {
    let mut minutes = STOP_AFTER.to_vec();
    let selected = minutes
        .iter()
        .position(|&offered| offered == chosen)
        .unwrap_or_else(|| {
            minutes.push(chosen);
            minutes.len() - 1
        });
    let names = minutes.iter().map(|&minutes| match minutes {
        1 => "1 minute".to_owned(),
        60 => "1 hour".to_owned(),
        minutes => format!("{minutes} minutes"),
    });
    setting(Icon::Timer, "Stop after silence")
        .detail("Ends a dictation you forgot")
        .control(choice(
            "stop-after-silence",
            names.collect::<Vec<_>>(),
            selected,
            move |index| match minutes.get(index) {
                Some(&minutes) if index != selected => {
                    Action::send(Command::Set(Change::StopAfterSilence(minutes)))
                }
                _ => Action::None,
            },
        ))
        .into()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::super::common::probe::{Seen, all, command, flipped, keys, scene, set, setting_row};
    use crate::node::Expander;

    /// The row that opens titled `title`.
    fn opener<'a>(page: &'a Content, title: &str) -> &'a Expander {
        fn find<'a>(nodes: &'a [Node], title: &str) -> Option<&'a Expander> {
            nodes.iter().find_map(|node| match node {
                Node::Expander(row) if row.title == title => Some(row),
                Node::Card(children) => find(children, title),
                _ => None,
            })
        }
        let Some(row) = find(&page.body, title) else {
            unreachable!("the page has no row that opens called {title:?}");
        };
        row
    }
    use super::*;
    use crate::mock;

    fn bars<'a>(nodes: &[&'a Node]) -> Vec<&'a Node> {
        nodes
            .iter()
            .copied()
            .filter(|node| matches!(node, Node::InfoBar { .. }))
            .collect()
    }

    #[test]
    fn the_groups_and_rows_are_all_there() {
        let page = content(&scene("settings"));
        let nodes = all(&page.body);
        assert_eq!(page.title, "General");
        let headers: Vec<_> = page
            .body
            .iter()
            .filter_map(|node| match node {
                Node::Header(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(headers, ["Speech", "Dictation", "Startup"]);
        for title in [
            "Microphone",
            "Start with Windows",
            "When listening starts",
            "When listening stops",
            "Stop after silence",
            "Type words as you say them",
        ] {
            let row = setting_row(&nodes, title);
            assert!(row.control.is_some(), "{title} has no control");
        }
        assert!(bars(&nodes).is_empty(), "a settled user sees no bars");
        // What has more to it than a switch is closed until it is asked for,
        // and says what is chosen meanwhile.
        for (title, value) in [
            (
                "Languages",
                "English (United States), Sloven\u{161}\u{10d}ina",
            ),
            ("Sounds", "Start and stop"),
        ] {
            let row = opener(&page, title);
            assert!(!row.open, "{title} is open");
            assert_eq!(row.value, value);
        }
    }

    #[test]
    fn someone_with_one_language_and_vinput_pro_is_told_once_to_add_the_others() {
        let says_so = |view: &View| {
            let page = content(view);
            Seen::of(&opener(&page, "Languages").children).says("more than one")
        };
        assert!(says_so(&scene("one-language")));
        // Not to someone who has named two, nor to someone who dictates
        // with a model that takes one language at a time.
        assert!(!says_so(&scene("pro-subscribed")));
        let mut local = scene("settings");
        "en-US".clone_into(&mut local.settings.languages);
        assert!(!says_so(&local));
        // It goes as soon as a second language is there.
        let mut view = scene("one-language");
        "en-US, en-GB".clone_into(&mut view.settings.languages);
        assert!(says_so(&view));
        "en-US, sl".clone_into(&mut view.settings.languages);
        assert!(!says_so(&view));
        // No bar, no dialog: the page is otherwise as it was.
        assert!(bars(&all(&content(&scene("one-language")).body)).is_empty());
    }

    #[test]
    fn the_row_of_the_sounds_says_which_are_played() {
        let played = |start: bool, stop: bool| {
            let mut view = scene("settings");
            view.settings.ready_sound = start;
            view.settings.stop_sound = stop;
            opener(&content(&view), "Sounds").value.clone()
        };
        assert_eq!(played(true, true), "Start and stop");
        assert_eq!(played(true, false), "Start");
        assert_eq!(played(false, true), "Stop");
        assert_eq!(played(false, false), "Off");
    }

    #[test]
    fn the_languages_of_windows_are_offered_first() {
        let page = content(&scene("settings"));
        let seen = Seen::of(&opener(&page, "Languages").children);
        assert!(seen.says("From Windows"));
        let mut view = scene("settings");
        view.system_languages.clear();
        let page = content(&view);
        assert!(!Seen::of(&opener(&page, "Languages").children).says("From Windows"));
    }

    #[test]
    fn keys_are_unique() {
        for name in mock::SCENES {
            let page = content(&scene(name));
            let keys = keys(&all(&page.body));
            let unique: HashSet<_> = keys.iter().collect();
            assert_eq!(keys.len(), 5, "{name}");
            assert_eq!(unique.len(), keys.len(), "{name}: {keys:?}");
        }
    }

    #[test]
    fn the_keys_are_on_a_page_of_their_own() {
        let page = content(&scene("settings"));
        let nodes = all(&page.body);
        assert!(!nodes.iter().any(|node| matches!(node, Node::Hotkey { .. })));
    }

    #[test]
    fn switches_send_their_new_position() {
        let page = content(&scene("settings"));
        let nodes = all(&page.body);
        assert_eq!(
            flipped(setting_row(&nodes, "Start with Windows"), false),
            Some(Change::Autostart(false))
        );
        assert_eq!(
            flipped(setting_row(&nodes, "When listening starts"), true),
            Some(Change::ReadySound(true))
        );
        assert_eq!(
            flipped(setting_row(&nodes, "When listening stops"), false),
            Some(Change::StopSound(false))
        );
        let on = |title| {
            matches!(
                setting_row(&nodes, title).control.as_deref(),
                Some(Node::Toggle {
                    on: true,
                    enabled: true,
                    ..
                })
            )
        };
        assert!(on("Start with Windows") && on("When listening starts"));
    }

    #[test]
    fn the_time_a_silent_dictation_is_given_is_picked_from_a_list() {
        let stop_after = |minutes: u32| {
            let mut view = scene("settings");
            view.settings.stop_after_minutes = minutes;
            let page = content(&view);
            let nodes = all(&page.body);
            match setting_row(&nodes, "Stop after silence").control.as_deref() {
                Some(Node::Choice {
                    items,
                    selected,
                    action,
                    ..
                }) => {
                    let sent = (0..items.len()).map(|index| command(&action(index)));
                    (items.clone(), *selected, sent.collect::<Vec<_>>())
                }
                _ => (Vec::new(), 0, Vec::new()),
            }
        };
        let picks = |minutes| Some(Command::Set(Change::StopAfterSilence(minutes)));
        let (items, selected, sent) = stop_after(30);
        assert_eq!(
            items,
            [
                "1 minute",
                "5 minutes",
                "15 minutes",
                "30 minutes",
                "1 hour"
            ]
        );
        assert_eq!(selected, 3);
        // Picking what is picked already sends nothing.
        assert_eq!(sent, [picks(1), picks(5), picks(15), None, picks(60)]);
        // A time only the settings file could name is shown as what it is.
        let (items, selected, sent) = stop_after(7);
        assert_eq!(items.last().map(String::as_str), Some("7 minutes"));
        assert_eq!((selected, sent.len()), (5, 6));
        assert_eq!(sent[5], None);
    }

    /// The list of audio inputs: its items, the one picked, and what picking
    /// each index sends.
    fn microphones(view: &View) -> (Vec<String>, usize, Vec<Option<Change>>, Tone) {
        let page = content(view);
        let nodes = all(&page.body);
        let row = setting_row(&nodes, "Microphone");
        let Some(Node::Choice {
            items,
            selected,
            action,
            ..
        }) = row.control.as_deref()
        else {
            unreachable!("the audio input is a list");
        };
        let picks = (0..=items.len()).map(|index| set(&action(index))).collect();
        (items.clone(), *selected, picks, row.detail_tone)
    }

    #[test]
    fn the_audio_input_lists_the_default_and_what_windows_has() {
        let mut view = scene("settings");
        let (items, selected, picks, tone) = microphones(&view);
        assert_eq!(
            items,
            [
                "Default (system)",
                "Microphone (USB Audio Device)",
                "Headset (WH-1000XM5)"
            ]
        );
        assert_eq!((selected, tone), (0, Tone::Secondary));
        assert_eq!(
            picks,
            [
                None,
                Some(Change::Microphone("{0.0.1.00000000}.{a}".to_owned())),
                Some(Change::Microphone("{0.0.1.00000000}.{b}".to_owned())),
                None,
            ]
        );

        "{0.0.1.00000000}.{b}".clone_into(&mut view.settings.microphone);
        let (_, selected, picks, _) = microphones(&view);
        assert_eq!(selected, 2);
        assert_eq!(picks[0], Some(Change::Microphone(String::new())));
        assert_eq!(picks[2], None);
    }

    #[test]
    fn an_audio_input_that_is_gone_stays_chosen_and_says_so() {
        let mut view = scene("settings");
        "{unplugged}".clone_into(&mut view.settings.microphone);
        let (items, selected, picks, tone) = microphones(&view);
        assert_eq!(items.len(), 4);
        assert_eq!(items[3], "Not connected");
        assert_eq!((selected, tone), (3, Tone::Caution));
        assert_eq!(picks[0], Some(Change::Microphone(String::new())));
        assert_eq!(picks[3], None);
    }

    #[test]
    fn a_new_user_is_shown_the_way_to_a_model() {
        let page = content(&scene("first-run"));
        let nodes = all(&page.body);
        let [
            Node::InfoBar {
                severity,
                text,
                action: Some((caption, Action::Go(Page::Models))),
                dismiss: None,
            },
        ] = bars(&nodes).as_slice()
        else {
            unreachable!("one bar that leads to the Models page was expected");
        };
        assert_eq!(*severity, Severity::Caution);
        assert_eq!(text, "Choose a speech model to start dictating");
        assert_eq!(caption, "Choose a model");
        assert!(
            matches!(page.body.first(), Some(Node::InfoBar { .. })),
            "the prompt comes first"
        );
    }

    #[test]
    fn a_notice_for_this_page_is_shown_and_can_be_dismissed() {
        let mut view = scene("notice");
        if let Some(notice) = &mut view.notice {
            notice.page = Page::General;
        }
        let page = content(&view);
        let [
            Node::InfoBar {
                severity: Severity::Critical,
                text,
                action: None,
                dismiss: Some(dismiss),
            },
        ] = bars(&all(&page.body)).as_slice()
        else {
            unreachable!("one bar with a cross was expected");
        };
        assert_eq!(
            text,
            "Ctrl+C is taken by another program. Pick another key."
        );
        assert_eq!(command(dismiss), Some(Command::DismissNotice));

        if let Some(notice) = &mut view.notice {
            notice.page = Page::Advanced;
        }
        assert!(bars(&all(&content(&view).body)).is_empty());
    }
}
