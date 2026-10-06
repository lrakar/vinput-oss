//! The Advanced page: the local API, voice memos and web search.

use vinput_view::{Api, Change, Command, Page, Settings, View};

use super::common::{notice_bar, set_text};
use crate::node::{
    Action, Content, Icon, Node, TextStyle, Tone, button, card, field, header, icon_button, link,
    row, setting, styled, toggle,
};

/// The manual on the site, which covers the local API.
const DOCUMENTATION: &str = "https://vinput-dasvcr.web.app/docs";
/// The search a dictated search opens while the setting is empty.
const DEFAULT_SEARCH: &str = "https://www.google.com/search?q=%s";

/// What the page shows for `view`.
#[must_use]
pub fn content(view: &View) -> Content {
    let mut body = Vec::new();
    body.extend(notice_bar(view, Page::Advanced));
    body.extend(local_api(&view.settings, &view.api));
    body.extend(voice_memos(&view.settings, &view.default_memo_dir));
    body.extend(web_search(&view.settings));
    Content {
        pinned: Vec::new(),
        title: "Advanced".to_owned(),
        body,
        footer: Vec::new(),
        back: None,
    }
}

/// The field for a setting that is text.
fn text_field(
    key: &'static str,
    current: &str,
    change: fn(String) -> Change,
) -> crate::node::Field {
    let before = current.to_owned();
    field(key, current, move |typed| set_text(&before, &typed, change))
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

/// The row for the voice memo folder: where it is, and the ways to change
/// it, look into it and go back to the default one.
fn memo_folder_row(settings: &Settings, default_dir: &str) -> Node {
    let custom = !settings.memo_dir.trim().is_empty();
    let folder = if custom {
        settings.memo_dir.as_str()
    } else if default_dir.is_empty() {
        "The default folder"
    } else {
        default_dir
    };
    let folder_row = setting(Icon::Folder, "Folder").detail(folder).control(row([
        button("Open folder", Action::send(Command::OpenMemoDir)).into(),
        button("Browse", Action::send(Command::BrowseMemoDir)).into(),
    ]));
    if custom {
        let reset = Command::Set(Change::MemoDir(String::new()));
        folder_row
            .below(link("Use the default folder", Action::send(reset)))
            .into()
    } else {
        folder_row.into()
    }
}

/// Where voice memos go.
fn voice_memos(settings: &Settings, default_dir: &str) -> [Node; 2] {
    [
        header("Voice memos"),
        card([
            memo_folder_row(settings, default_dir),
            setting(Icon::Clipboard, "Copy each memo to the clipboard")
                .detail("Paste a new memo straight into a chat, an email or a folder")
                .control(toggle(settings.memo_to_clipboard, |on| {
                    Action::send(Command::Set(Change::MemoToClipboard(on)))
                }))
                .into(),
        ]),
    ]
}

/// The search a dictated search opens.
fn web_search(settings: &Settings) -> [Node; 2] {
    [
        header("Web search"),
        card([setting(Icon::Search, "Search address")
            .detail(
                "The search that opens when you dictate one. Put %s where your words go, or \
                 leave it empty to search with Google.",
            )
            .below(
                text_field("search-url", &settings.search_url, Change::SearchUrl)
                    .placeholder(DEFAULT_SEARCH)
                    .wide()
                    .into(),
            )
            .into()]),
    ]
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use vinput_view::NoticeKind;

    use super::super::common::probe::{
        all, buttons, command, flipped, keys, scene, setting_row, typed,
    };
    use super::*;
    use crate::mock;
    use crate::node::{Field, Severity};

    fn text_field<'a>(nodes: &[&'a Node], key: &str) -> &'a Field {
        let found = nodes.iter().find_map(|node| match node {
            Node::Field(field) if field.key == key => Some(field),
            _ => None,
        });
        let Some(field) = found else {
            unreachable!("the page has no field called {key:?}");
        };
        field
    }

    /// What pressing the button captioned `label` sends.
    fn pressed(nodes: &[&Node], label: &str) -> Option<Command> {
        buttons(nodes)
            .into_iter()
            .find(|button| button.label == label)
            .and_then(|button| command(&button.action))
    }

    fn links<'a>(nodes: &[&'a Node]) -> Vec<(&'a str, Option<Command>)> {
        nodes
            .iter()
            .filter_map(|node| match node {
                Node::Link { label, action } => Some((label.as_str(), command(action))),
                _ => None,
            })
            .collect()
    }

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
        assert_eq!(headers, ["Local API", "Voice memos", "Web search"]);
        for title in [
            "Let other programs connect",
            "Pipe name",
            "Connected programs",
            "Documentation",
            "Folder",
            "Copy each memo to the clipboard",
            "Search address",
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
            assert_eq!(keys.len(), 1, "{name}");
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
            "Control Deck"
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
    fn voice_memos_show_the_folder_in_force() {
        let mut view = scene("settings");
        let page = content(&view);
        let nodes = all(&page.body);
        assert_eq!(
            setting_row(&nodes, "Folder").detail,
            r"C:\Users\you\Documents\Vinput memos"
        );
        assert_eq!(pressed(&nodes, "Browse"), Some(Command::BrowseMemoDir));
        assert_eq!(pressed(&nodes, "Open folder"), Some(Command::OpenMemoDir));
        assert!(links(&nodes).is_empty(), "the default needs no way back");
        let copy = setting_row(&nodes, "Copy each memo to the clipboard");
        assert_eq!(flipped(copy, false), Some(Change::MemoToClipboard(false)));

        r"D:\Recordings".clone_into(&mut view.settings.memo_dir);
        let page = content(&view);
        let nodes = all(&page.body);
        assert_eq!(setting_row(&nodes, "Folder").detail, r"D:\Recordings");
        assert_eq!(
            links(&nodes),
            [(
                "Use the default folder",
                Some(Command::Set(Change::MemoDir(String::new())))
            )]
        );
    }

    #[test]
    fn the_search_address_is_wide_and_empty_means_the_default() {
        let page = content(&scene("settings"));
        let nodes = all(&page.body);
        let address = text_field(&nodes, "search-url");
        assert!(address.wide && !address.secret);
        assert_eq!(address.text, "https://www.google.com/search?q=%s");
        assert_eq!(address.placeholder, DEFAULT_SEARCH);
        assert!(setting_row(&nodes, "Search address").detail.contains("%s"));
        assert_eq!(
            typed(&nodes, "search-url", "https://duckduckgo.com/?q=%s"),
            Some(Change::SearchUrl("https://duckduckgo.com/?q=%s".to_owned()))
        );
        assert_eq!(
            typed(&nodes, "search-url", ""),
            Some(Change::SearchUrl(String::new()))
        );
        assert_eq!(typed(&nodes, "search-url", address.text.as_str()), None);
    }

    #[test]
    fn a_notice_for_this_page_is_shown_and_can_be_dismissed() {
        let mut view = scene("notice");
        assert!(!has_bar(&all(&content(&view).body)), "it is about General");
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
