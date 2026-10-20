//! The Shortcuts page: every key of `VinPUT` in one place, each with what
//! belongs to it.
//!
//! A key and its settings are found together: the key that searches the
//! web with the search it opens, the key that records a memo with where
//! memos go. What only some people change (the address of the search)
//! stays closed in a row that says what is chosen.

use vinput_view::{Change, Command, Page, Settings, View};

use super::actions;
use super::common::{notice_bar, set_text};
use crate::node::{
    Action, Content, Icon, Node, button, card, expander, field, header, hotkey, link, note,
    plain_setting, row, setting, toggle,
};

/// The search a dictated search opens while the setting is empty.
const DEFAULT_SEARCH: &str = "https://www.google.com/search?q=%s";

/// What the page shows for `view`.
#[must_use]
pub fn content(view: &View) -> Content {
    let settings = &view.settings;
    let mut body = Vec::new();
    body.extend(notice_bar(view, Page::Shortcuts));
    body.extend(dictation(settings));
    body.extend(web_search(settings));
    body.extend(voice_memos(settings, &view.default_memo_dir));
    body.extend(edit_by_voice(settings));
    body.extend(actions::section(&settings.actions));
    Content {
        pinned: Vec::new(),
        title: "Shortcuts".to_owned(),
        body,
        footer: Vec::new(),
        back: None,
    }
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

/// The two keys that type what is said.
fn dictation(settings: &Settings) -> [Node; 3] {
    [
        header("Dictation"),
        card([
            setting(Icon::Keyboard, "Dictate")
                .detail("Types what you say wherever the cursor is")
                .control(hotkey_field(
                    "hotkey",
                    &settings.hotkey,
                    false,
                    Change::Hotkey,
                ))
                .into(),
            setting(Icon::Keyboard, "Dictate, then press Enter")
                .detail("Presses Enter when you finish, to send a message")
                .control(hotkey_field(
                    "hotkey-enter",
                    &settings.hotkey_enter,
                    true,
                    Change::HotkeyEnter,
                ))
                .into(),
        ]),
        note("Tap a key to start and again to stop, or hold it while you speak."),
    ]
}

/// The site a search goes to, as a closed row says it: the name of the
/// site, without the rest of the address.
fn search_site(address: &str) -> String {
    let address = address.trim();
    if address.is_empty() || address == DEFAULT_SEARCH {
        return "Google".to_owned();
    }
    let rest = address.split_once("://").map_or(address, |(_, rest)| rest);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    host.strip_prefix("www.").unwrap_or(host).to_owned()
}

/// The key that searches the web, and the search it opens.
fn web_search(settings: &Settings) -> [Node; 2] {
    let before = settings.search_url.clone();
    let address = field("search-url", &settings.search_url, move |typed| {
        set_text(&before, &typed, Change::SearchUrl)
    })
    .placeholder(DEFAULT_SEARCH)
    .wide();
    [
        header("Web search"),
        card([
            setting(Icon::Search, "Dictate, then search the web")
                .detail("Opens a search for what you said in your browser")
                .control(hotkey_field(
                    "hotkey-search",
                    &settings.hotkey_search,
                    true,
                    Change::HotkeySearch,
                ))
                .into(),
            expander(
                "search-with",
                Icon::Globe,
                "Search with",
                [plain_setting("Search address")
                    .indented()
                    .detail("Put %s where your words go, or leave it empty to search with Google")
                    .below(address.into())
                    .into()],
            )
            .value(search_site(&settings.search_url))
            .into(),
        ]),
    ]
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
    let folder_row = setting(Icon::Folder, "Save memos in")
        .detail(folder)
        .control(row([
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

/// The key that records a memo, where memos go and what is done with each.
fn voice_memos(settings: &Settings, default_dir: &str) -> [Node; 2] {
    [
        header("Voice memos"),
        card([
            setting(Icon::Memo, "Record a voice memo")
                .detail("Saves what you say as an MP3 file")
                .control(hotkey_field(
                    "hotkey-memo",
                    &settings.hotkey_memo,
                    true,
                    Change::HotkeyMemo,
                ))
                .into(),
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

/// The key that edits the selected text, and the way to the model that
/// does it.
fn edit_by_voice(settings: &Settings) -> [Node; 2] {
    [
        header("Edit by voice"),
        card([
            setting(Icon::Edit, "Edit the selected text")
                .detail("Select text, hold the key and say what to change")
                .control(hotkey_field(
                    "hotkey-command",
                    &settings.hotkey_command,
                    true,
                    Change::HotkeyCommand,
                ))
                .into(),
            setting(Icon::Model, "Text model")
                .detail("The model that edits is chosen on the Cleanup page")
                .action(Action::Go(Page::Cleanup))
                .into(),
        ]),
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
    use crate::node::{Expander, Field, Severity};

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

    /// The row that opens titled `title`.
    fn opener<'a>(nodes: &[&'a Node], title: &str) -> &'a Expander {
        let found = nodes.iter().find_map(|node| match node {
            Node::Expander(row) if row.title == title => Some(row),
            _ => None,
        });
        let Some(row) = found else {
            unreachable!("the page has no row that opens called {title:?}");
        };
        row
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

    fn bars<'a>(nodes: &[&'a Node]) -> Vec<&'a Node> {
        nodes
            .iter()
            .copied()
            .filter(|node| matches!(node, Node::InfoBar { .. }))
            .collect()
    }

    #[test]
    fn every_key_is_here_with_what_belongs_to_it() {
        let page = content(&scene("settings"));
        assert_eq!(page.title, "Shortcuts");
        let headers: Vec<_> = page
            .body
            .iter()
            .filter_map(|node| match node {
                Node::Header(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            headers,
            [
                "Dictation",
                "Web search",
                "Voice memos",
                "Edit by voice",
                "Your actions"
            ]
        );
        let nodes = all(&page.body);
        for title in [
            "Dictate",
            "Dictate, then press Enter",
            "Dictate, then search the web",
            "Record a voice memo",
            "Edit the selected text",
            "Save memos in",
            "Copy each memo to the clipboard",
        ] {
            assert!(
                setting_row(&nodes, title).control.is_some(),
                "{title} has no control"
            );
        }
        assert!(bars(&nodes).is_empty(), "a settled user sees no bars");
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
                } if !key.starts_with("action") => Some((*key, text.as_str(), *clearable)),
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
                ("hotkey-command", "", true),
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
        assert_eq!(
            typed(&nodes, "hotkey-command", "F9"),
            Some(Change::HotkeyCommand("F9".to_owned()))
        );
        assert_eq!(typed(&nodes, "hotkey", "F1"), None, "nothing changed");
    }

    #[test]
    fn keys_are_unique() {
        for name in mock::SCENES {
            let view = scene(name);
            let page = content(&view);
            let keys = keys(&all(&page.body));
            let unique: HashSet<_> = keys.iter().collect();
            let actions = view.settings.actions.len();
            assert_eq!(keys.len(), 7 + actions * actions::KEYS_EACH, "{name}");
            assert_eq!(unique.len(), keys.len(), "{name}: {keys:?}");
        }
    }

    #[test]
    fn the_search_says_its_site_and_opens_onto_its_address() {
        assert_eq!(search_site(""), "Google");
        assert_eq!(search_site(DEFAULT_SEARCH), "Google");
        assert_eq!(
            search_site("https://duckduckgo.com/?q=%s"),
            "duckduckgo.com"
        );
        assert_eq!(search_site("https://www.bing.com/search?q=%s"), "bing.com");
        let mut view = scene("settings");
        let page = content(&view);
        let nodes = all(&page.body);
        let row = opener(&nodes, "Search with");
        assert!(!row.open, "the address is for the few who change it");
        assert_eq!(row.value, "Google");
        let field = text_field(&nodes, "search-url");
        assert!(field.wide);
        assert_eq!(field.placeholder, DEFAULT_SEARCH);
        assert_eq!(
            typed(&nodes, "search-url", "https://duckduckgo.com/?q=%s"),
            Some(Change::SearchUrl("https://duckduckgo.com/?q=%s".to_owned()))
        );
        "https://duckduckgo.com/?q=%s".clone_into(&mut view.settings.search_url);
        let page = content(&view);
        assert_eq!(
            opener(&all(&page.body), "Search with").value,
            "duckduckgo.com"
        );
    }

    #[test]
    fn the_memo_folder_can_be_opened_changed_and_put_back() {
        let mut view = scene("settings");
        view.settings.memo_dir.clear();
        let page = content(&view);
        let nodes = all(&page.body);
        assert_eq!(
            setting_row(&nodes, "Save memos in").detail,
            view.default_memo_dir
        );
        assert_eq!(pressed(&nodes, "Open folder"), Some(Command::OpenMemoDir));
        assert_eq!(pressed(&nodes, "Browse"), Some(Command::BrowseMemoDir));
        assert!(links(&nodes).is_empty(), "the default needs no way back");

        "D:\\Memos".clone_into(&mut view.settings.memo_dir);
        let page = content(&view);
        let nodes = all(&page.body);
        assert_eq!(setting_row(&nodes, "Save memos in").detail, "D:\\Memos");
        assert_eq!(
            links(&nodes),
            [(
                "Use the default folder",
                Some(Command::Set(Change::MemoDir(String::new())))
            )]
        );
        assert_eq!(
            flipped(
                setting_row(&nodes, "Copy each memo to the clipboard"),
                false
            ),
            Some(Change::MemoToClipboard(false))
        );
    }

    #[test]
    fn the_key_that_edits_points_at_the_model_that_does_it() {
        let page = content(&scene("settings"));
        let nodes = all(&page.body);
        assert!(matches!(
            setting_row(&nodes, "Text model").action,
            Action::Go(Page::Cleanup)
        ));
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
            notice.kind = NoticeKind::Error;
        }
        assert!(bars(&all(&content(&view).body)).is_empty());
    }
}
