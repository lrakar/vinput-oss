//! The General page: hotkeys and the everyday switches.

use vinput_view::{Change, Command, Microphone, Page, Settings, View};

use super::common::{notice_bar, pro_in_use, pro_trouble, set_text};
use crate::node::{
    Action, Content, Icon, Node, Severity, Tone, card, choice, field, header, hotkey, note,
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
    body.extend(hotkeys(&view.settings));
    body.extend(input(view));
    body.extend(behavior(&view.settings));
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

/// The field for one of the hotkeys.
fn hotkey_field(
    key: &'static str,
    current: &str,
    clearable: bool,
    change: fn(String) -> Change,
) -> Node {
    let before = current.to_owned();
    hotkey(key, current, clearable, move |typed| {
        set_text(&before, &typed, change)
    })
}

/// The keys that start and stop listening.
fn hotkeys(settings: &Settings) -> [Node; 3] {
    [
        header("Hotkeys"),
        note(
            "Tap a key to start and tap it again to stop, or hold it while you speak and let go \
             when you are done.",
        ),
        card([
            setting(Icon::Keyboard, "Dictate")
                .detail("Types what you say into the window you are working in")
                .control(hotkey_field(
                    "hotkey",
                    &settings.hotkey,
                    false,
                    Change::Hotkey,
                ))
                .into(),
            setting(Icon::Keyboard, "Dictate, then press Enter")
                .detail("Types what you say and sends it, for chats and prompts")
                .control(hotkey_field(
                    "hotkey-enter",
                    &settings.hotkey_enter,
                    true,
                    Change::HotkeyEnter,
                ))
                .into(),
            setting(Icon::Search, "Dictate, then search the web")
                .detail("Opens what you say as a web search in your browser")
                .control(hotkey_field(
                    "hotkey-search",
                    &settings.hotkey_search,
                    true,
                    Change::HotkeySearch,
                ))
                .into(),
            setting(Icon::Memo, "Record a voice memo")
                .detail("Saves what you say as an audio file")
                .control(hotkey_field(
                    "hotkey-memo",
                    &settings.hotkey_memo,
                    true,
                    Change::HotkeyMemo,
                ))
                .into(),
        ]),
    ]
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
        ("The microphone VinPUT listens with", Tone::Secondary)
    } else {
        names.push(MISSING_MICROPHONE.to_owned());
        ids.push(chosen.to_owned());
        (
            "The microphone you chose is not connected. VinPUT uses the Windows default until \
             it is back.",
            Tone::Caution,
        )
    };
    setting(Icon::Microphone, "Audio input")
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

/// What `VinPUT` Pro can do for someone who has named one language.
const MORE_LANGUAGES: &str = "Speak more than one language? Add each of them, and VinPUT Pro \
                              follows you when you switch.";

/// What the app listens with and what it listens for.
fn input(view: &View) -> [Node; 2] {
    let settings = &view.settings;
    let languages = settings.languages.clone();
    let mut row = setting(Icon::Globe, "Languages")
        .detail("The languages you speak, as codes separated by commas, such as en-US, sl-SI")
        .control(
            field("languages", &settings.languages, move |typed| {
                set_text(&languages, &typed, Change::Languages)
            })
            .placeholder("en-US")
            .into(),
        );
    // VinPUT Pro leans towards the languages it is told of, and can take
    // speech in another one for a third: worth one quiet line, here, to
    // whoever has named a single language.
    if pro_in_use(view) && spoken_languages(&settings.languages) == 1 {
        row = row.below(note(MORE_LANGUAGES));
    }
    [
        header("Input"),
        card([
            microphone_row(&settings.microphone, &view.microphones),
            row.into(),
        ]),
    ]
}

/// How many languages `codes` names. Two regions of one language, such as
/// en-US and en-GB, are one.
fn spoken_languages(codes: &str) -> usize {
    let mut languages: Vec<String> = codes
        .split(',')
        .filter_map(|code| code.split(['-', '_']).next())
        .map(|language| language.trim().to_ascii_lowercase())
        .filter(|language| !language.is_empty())
        .collect();
    languages.sort_unstable();
    languages.dedup();
    languages.len()
}

/// The everyday switches.
fn behavior(settings: &Settings) -> [Node; 2] {
    [
        header("Behavior"),
        card([
            setting(Icon::Power, "Start with Windows")
                .detail("VinPUT waits in the tray from the moment you sign in")
                .control(toggle(settings.autostart, |on| {
                    Action::send(Command::Set(Change::Autostart(on)))
                }))
                .into(),
            setting(Icon::Volume, "Play a sound when listening starts")
                .detail("A soft click as soon as you press the key")
                .control(toggle(settings.ready_sound, |on| {
                    Action::send(Command::Set(Change::ReadySound(on)))
                }))
                .into(),
            setting(Icon::Volume, "Play a sound when listening stops")
                .detail("A lower click as soon as you are done")
                .control(toggle(settings.stop_sound, |on| {
                    Action::send(Command::Set(Change::StopSound(on)))
                }))
                .into(),
        ]),
    ]
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::super::common::probe::{
        all, command, flipped, keys, scene, set, setting_row, typed,
    };
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
        let headers: Vec<_> = nodes
            .iter()
            .filter_map(|node| match node {
                Node::Header(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(headers, ["Hotkeys", "Input", "Behavior"]);
        for title in [
            "Dictate",
            "Dictate, then press Enter",
            "Dictate, then search the web",
            "Record a voice memo",
            "Audio input",
            "Languages",
            "Start with Windows",
            "Play a sound when listening starts",
        ] {
            let row = setting_row(&nodes, title);
            assert!(row.control.is_some(), "{title} has no control");
            assert!(!row.detail.is_empty(), "{title} does not say what it does");
        }
        assert!(bars(&nodes).is_empty(), "a settled user sees no bars");
    }

    #[test]
    fn someone_with_one_language_and_vinput_pro_is_told_once_to_add_the_others() {
        let says_so = |view: &View| {
            let page = content(view);
            let nodes = all(&page.body);
            let below = &setting_row(&nodes, "Languages").below;
            below.iter().any(
                |node| matches!(node, Node::Text { text, .. } if text.contains("more than one")),
            )
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
    fn languages_are_counted_by_what_is_spoken() {
        assert_eq!(spoken_languages(""), 0);
        assert_eq!(spoken_languages(" , "), 0);
        assert_eq!(spoken_languages("en-US"), 1);
        assert_eq!(spoken_languages("en-US, EN_gb,en"), 1);
        assert_eq!(spoken_languages("en-US, sl-SI, de"), 3);
    }

    #[test]
    fn keys_are_unique() {
        for name in mock::SCENES {
            let page = content(&scene(name));
            let keys = keys(&all(&page.body));
            let unique: HashSet<_> = keys.iter().collect();
            assert_eq!(keys.len(), 6, "{name}");
            assert_eq!(unique.len(), keys.len(), "{name}: {keys:?}");
        }
    }

    #[test]
    fn hotkeys_show_what_is_set_and_send_what_is_pressed() {
        let page = content(&scene("settings"));
        let nodes = all(&page.body);
        let shown: Vec<_> = nodes
            .iter()
            .filter_map(|node| match node {
                Node::Hotkey {
                    key,
                    text,
                    clearable,
                    ..
                } => Some((*key, text.as_str(), *clearable)),
                _ => None,
            })
            .collect();
        assert_eq!(
            shown,
            [
                ("hotkey", "F1", false),
                ("hotkey-enter", "F2", true),
                ("hotkey-search", "", true),
                ("hotkey-memo", "Ctrl+Shift+M", true),
            ]
        );
        assert_eq!(
            typed(&nodes, "hotkey", "F8"),
            Some(Change::Hotkey("F8".to_owned()))
        );
        assert_eq!(
            typed(&nodes, "hotkey-enter", "Ctrl+F2"),
            Some(Change::HotkeyEnter("Ctrl+F2".to_owned()))
        );
        assert_eq!(
            typed(&nodes, "hotkey-search", "F4"),
            Some(Change::HotkeySearch("F4".to_owned()))
        );
        assert_eq!(
            typed(&nodes, "hotkey-memo", ""),
            Some(Change::HotkeyMemo(String::new()))
        );
        assert_eq!(typed(&nodes, "hotkey", "F1"), None, "nothing changed");
    }

    #[test]
    fn languages_are_sent_trimmed_and_only_when_changed() {
        let page = content(&scene("settings"));
        let nodes = all(&page.body);
        assert_eq!(
            typed(&nodes, "languages", " de-DE, en-US "),
            Some(Change::Languages("de-DE, en-US".to_owned()))
        );
        assert_eq!(typed(&nodes, "languages", "en-US, sl-SI "), None);
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
            flipped(
                setting_row(&nodes, "Play a sound when listening starts"),
                true
            ),
            Some(Change::ReadySound(true))
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
        assert!(on("Start with Windows") && on("Play a sound when listening starts"));
    }

    /// The list of audio inputs: its items, the one picked, and what picking
    /// each index sends.
    fn microphones(view: &View) -> (Vec<String>, usize, Vec<Option<Change>>, Tone) {
        let page = content(view);
        let nodes = all(&page.body);
        let row = setting_row(&nodes, "Audio input");
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
