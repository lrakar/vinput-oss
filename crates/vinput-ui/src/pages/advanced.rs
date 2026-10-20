//! The Advanced page: the local API, for other programs on this PC.
//!
//! What belongs to a key (where memos go, the search a dictated search
//! opens) is on the Shortcuts page, beside its key.

use vinput_view::{Api, Change, Command, Page, Settings, View};

use super::common::notice_bar;
use crate::node::{
    Action, Content, Icon, Node, TextStyle, Tone, card, header, icon_button, plain_setting, row,
    setting, styled, toggle,
};

/// The manual on the site, which covers the local API.
const DOCUMENTATION: &str = "https://vinput-dasvcr.web.app/docs";

/// What the page shows for `view`.
#[must_use]
pub fn content(view: &View) -> Content {
    let mut body = Vec::new();
    body.extend(notice_bar(view, Page::Advanced));
    body.extend(local_api(&view.settings, &view.api));
    Content {
        pinned: Vec::new(),
        title: "Advanced".to_owned(),
        body,
        footer: Vec::new(),
        back: None,
    }
}

/// The switch for other programs, where they connect and which ones have.
fn local_api(settings: &Settings, api: &Api) -> [Node; 2] {
    let mut rows = vec![
        setting(Icon::Plug, "Let other programs connect")
            .detail("Programs on this PC can start and stop dictation and receive what you say")
            .control(toggle(settings.api_enabled, |on| {
                Action::send(Command::Set(Change::ApiEnabled(on)))
            }))
            .into(),
        // What is dictated goes to nobody unless the user says so, and to
        // nobody while no program may connect: a row under the one that
        // lets them connect, as the sounds are under theirs.
        plain_setting("Send typed text to connected programs")
            .indented()
            .detail("Connected programs also receive what a dictation types, as it is typed")
            .enabled(settings.api_enabled)
            .control(Node::Toggle {
                on: settings.api_enabled && settings.api_transcript,
                enabled: settings.api_enabled,
                action: Box::new(|on| Action::send(Command::Set(Change::ApiTranscript(on)))),
            })
            .into(),
    ];
    if !api.pipe.is_empty() {
        let tone = if settings.api_enabled {
            Tone::Primary
        } else {
            Tone::Disabled
        };
        rows.push(
            setting(Icon::Plug, "Pipe name")
                .detail("Where programs find VinPUT on this PC")
                .control(row([
                    styled(&api.pipe, TextStyle::Mono, tone),
                    icon_button(
                        Icon::Copy,
                        "Copy the pipe name",
                        Action::Copy(api.pipe.clone()),
                    )
                    .into(),
                ]))
                .into(),
        );
    }
    let connected = if api.clients.is_empty() {
        "No programs connected".to_owned()
    } else {
        api.clients.join(", ")
    };
    rows.push(
        setting(Icon::Plug, "Connected programs")
            .detail(connected)
            .into(),
    );
    rows.push(
        setting(Icon::Info, "Documentation")
            .detail("How to make a program work with VinPUT")
            .action(Action::send(Command::OpenLink(DOCUMENTATION.to_owned())))
            .into(),
    );
    [header("Local API"), card(rows)]
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use vinput_view::NoticeKind;

    use super::super::common::probe::{all, command, flipped, keys, scene, setting_row};
    use super::*;
    use crate::mock;
    use crate::node::Severity;

    fn has_bar(nodes: &[&Node]) -> bool {
        nodes
            .iter()
            .any(|node| matches!(node, Node::InfoBar { .. }))
    }

    #[test]
    fn the_groups_and_rows_are_all_there() {
        let page = content(&scene("settings"));
        let nodes = all(&page.body);
        assert_eq!(page.title, "Advanced");
        let headers: Vec<_> = nodes
            .iter()
            .filter_map(|node| match node {
                Node::Header(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(headers, ["Local API"]);
        for title in [
            "Let other programs connect",
            "Send typed text to connected programs",
            "Pipe name",
            "Connected programs",
            "Documentation",
        ] {
            let row = setting_row(&nodes, title);
            assert!(!row.detail.is_empty(), "{title} does not say what it does");
        }
        assert!(!has_bar(&nodes), "a notice for another page is not shown");
    }

    #[test]
    fn keys_are_unique() {
        for name in mock::SCENES {
            let page = content(&scene(name));
            let keys = keys(&all(&page.body));
            let unique: HashSet<_> = keys.iter().collect();
            assert!(keys.is_empty(), "{name}: {keys:?}");
            assert_eq!(unique.len(), keys.len(), "{name}: {keys:?}");
        }
    }

    #[test]
    fn the_local_api_says_who_is_connected_and_where() {
        let page = content(&scene("settings"));
        let nodes = all(&page.body);
        let switch = setting_row(&nodes, "Let other programs connect");
        assert_eq!(flipped(switch, false), Some(Change::ApiEnabled(false)));
        let Some(Node::Row(pipe)) = setting_row(&nodes, "Pipe name").control.as_deref() else {
            unreachable!("the name of the pipe and the button that copies it were expected");
        };
        let [
            Node::Text {
                text,
                style: TextStyle::Mono,
                tone: Tone::Primary,
            },
            Node::Button(copy),
        ] = pipe.as_slice()
        else {
            unreachable!("the name in the font of names, then a button, was expected");
        };
        assert_eq!(text, r"\\.\pipe\vinput");
        assert_eq!(copy.icon, Some(Icon::Copy));
        assert_eq!(copy.hint, "Copy the pipe name");
        assert!(matches!(&copy.action, Action::Copy(copied) if copied == text));
        assert_eq!(
            setting_row(&nodes, "Connected programs").detail,
            "Stream Deck"
        );
        assert_eq!(
            command(&setting_row(&nodes, "Documentation").action),
            Some(Command::OpenLink(
                "https://vinput-dasvcr.web.app/docs".to_owned()
            ))
        );

        let mut view = scene("first-run");
        view.settings.api_enabled = false;
        let page = content(&view);
        let nodes = all(&page.body);
        assert_eq!(
            setting_row(&nodes, "Connected programs").detail,
            "No programs connected"
        );
        assert!(matches!(
            setting_row(&nodes, "Pipe name").control.as_deref(),
            Some(Node::Row(pipe)) if matches!(
                pipe.as_slice(),
                [
                    Node::Text {
                        style: TextStyle::Mono,
                        tone: Tone::Disabled,
                        ..
                    },
                    Node::Button(_),
                ]
            )
        ));
        let switch = setting_row(&nodes, "Let other programs connect");
        assert_eq!(flipped(switch, true), Some(Change::ApiEnabled(true)));
    }

    #[test]
    fn typed_text_is_sent_only_when_switched_on_and_programs_may_connect() {
        const TITLE: &str = "Send typed text to connected programs";
        let shown = |view: &View| {
            let page = content(view);
            let nodes = all(&page.body);
            let row = setting_row(&nodes, TITLE);
            let Some(Node::Toggle { on, enabled, .. }) = row.control.as_deref() else {
                unreachable!("a switch was expected");
            };
            (*on, *enabled, row.enabled, flipped(row, !*on))
        };
        // Someone new has not switched it on.
        let mut view = scene("first-run");
        assert!(!view.settings.api_transcript);
        assert_eq!(
            shown(&view),
            (false, true, true, Some(Change::ApiTranscript(true)))
        );
        view.settings.api_transcript = true;
        assert_eq!(
            shown(&view),
            (true, true, true, Some(Change::ApiTranscript(false)))
        );
        // While no program may connect nothing is sent, and the switch
        // says so whatever it is set to.
        view.settings.api_enabled = false;
        let (on, enabled, row_enabled, _) = shown(&view);
        assert!(!on && !enabled && !row_enabled);
    }

    #[test]
    fn a_notice_for_this_page_is_shown_and_can_be_dismissed() {
        let mut view = scene("notice");
        assert!(
            !has_bar(&all(&content(&view).body)),
            "it is about another page"
        );
        if let Some(notice) = &mut view.notice {
            notice.page = Page::Advanced;
            notice.kind = NoticeKind::Success;
        }
        let page = content(&view);
        let Some(Node::InfoBar {
            severity: Severity::Success,
            dismiss: Some(dismiss),
            ..
        }) = page.body.first()
        else {
            unreachable!("the notice comes first, with a cross");
        };
        assert_eq!(command(dismiss), Some(Command::DismissNotice));
    }
}
