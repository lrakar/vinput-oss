//! The Words page: the dictionary, the snippets and spoken formatting.
//!
//! Each list is added to on its first row. What is on it is a row that
//! opens, open until the user closes it, onto a list that scrolls by
//! itself, so a long dictionary does not make the page longer. What can be
//! said to format is a row that opens as well.

use vinput_view::{Change, Command, Page, Settings, Snippet, View, Word};

use super::common::notice_bar;
use crate::node::{
    Action, Content, Icon, Key, Node, button, card, expander, field, header, icon_button, note,
    plain_setting, row, scroll, setting, toggle,
};
use crate::theme::metric;

/// The fields a word of the dictionary is typed in.
const WORD: Key = "word";
const SOUNDS_LIKE: Key = "word-sounds-like";
/// The fields a snippet is typed in.
const TRIGGER: Key = "snippet-trigger";
const TEXT: Key = "snippet-text";

/// What the page shows for `view`.
#[must_use]
pub fn content(view: &View) -> Content {
    let mut body = Vec::new();
    body.extend(notice_bar(view, Page::Words));
    body.extend(dictionary(&view.settings.dictionary));
    body.extend(snippets(&view.settings.snippets));
    body.extend(formatting(&view.settings));
    Content {
        pinned: Vec::new(),
        title: "Words".to_owned(),
        body,
        footer: Vec::new(),
        back: None,
    }
}

/// Two fields and Add: the way an entry gets on a list. Enter in either
/// field does what Add does.
fn entry(fields: [(Key, &str); 2], make: fn(String, String) -> Command, hint: &str) -> Node {
    let add = || Action::Enter {
        fields: [fields[0].0, fields[1].0],
        make,
    };
    let [first, second] = fields.map(|(key, placeholder)| {
        field(key, "", |_| Action::None)
            .placeholder(placeholder)
            .wide()
            .on_enter(add())
            .into()
    });
    row([first, second, button("Add", add()).hint(hint).into()])
}

/// The bin at the right edge of an entry.
fn remove(what: &str, change: Change) -> Node {
    icon_button(
        Icon::Delete,
        format!("Remove {what}"),
        Action::send(Command::Set(change)),
    )
    .into()
}

fn add_word(word: String, sounds_like: String) -> Command {
    Command::Set(Change::AddWord(word, sounds_like))
}

fn add_snippet(trigger: String, text: String) -> Command {
    Command::Set(Change::AddSnippet(trigger, text))
}

/// The row that opens onto what is on a list, saying how many there are;
/// nothing while the list is empty.
fn saved(
    (key, list): (Key, Key),
    icon: Icon,
    title: &str,
    (one, many): (&str, &str),
    rows: Vec<Node>,
) -> Option<Node> {
    let count = match rows.len() {
        0 => return None,
        1 => format!("1 {one}"),
        n => format!("{n} {many}"),
    };
    Some(
        expander(key, icon, title, [scroll(list, metric::LIST_MAX, rows)])
            .value(count)
            .open(true)
            .into(),
    )
}

/// The words `VinPUT` listens for and writes the user's way.
fn dictionary(words: &[Word]) -> [Node; 3] {
    let add = setting(Icon::Dictionary, "Add a word")
        .detail(
            "If a word still comes out wrong, add it again with what VinPUT types \
             instead, such as vin put",
        )
        .below(entry(
            [
                (WORD, "Word or name"),
                (SOUNDS_LIKE, "Sounds like (optional)"),
            ],
            add_word,
            "Add the word",
        ))
        .into();
    let rows = words
        .iter()
        .map(|known| {
            let detail = if known.sounds_like.is_empty() {
                String::new()
            } else {
                format!("Sounds like {}", known.sounds_like)
            };
            plain_setting(&known.word)
                .indented()
                .detail(detail)
                .control(remove(&known.word, Change::RemoveWord(known.word.clone())))
                .into()
        })
        .collect();
    let list = saved(
        ("dictionary", "dictionary-words"),
        Icon::Dictionary,
        "Your words",
        ("word", "words"),
        rows,
    );
    [
        header("Dictionary"),
        note("Names and terms you use. VinPUT listens for them and spells them your way."),
        card([add].into_iter().chain(list)),
    ]
}

/// The phrases that stand for a longer text.
fn snippets(snippets: &[Snippet]) -> [Node; 3] {
    let add = setting(Icon::Snippet, "Add a snippet")
        .detail("To change a snippet, add it again with the same phrase")
        .below(entry(
            [(TRIGGER, "What you say"), (TEXT, "What VinPUT types")],
            add_snippet,
            "Add the snippet",
        ))
        .into();
    let rows = snippets
        .iter()
        .map(|snippet| {
            plain_setting(&snippet.trigger)
                .indented()
                .detail(snippet.text.replace('\n', " "))
                .control(remove(
                    &snippet.trigger,
                    Change::RemoveSnippet(snippet.trigger.clone()),
                ))
                .into()
        })
        .collect();
    let list = saved(
        ("snippets", "snippets-list"),
        Icon::Snippet,
        "Your snippets",
        ("snippet", "snippets"),
        rows,
    );
    [
        header("Snippets"),
        note("Say a short phrase, such as my email, and VinPUT types the text you saved for it."),
        card([add].into_iter().chain(list)),
    ]
}

/// The switch for spoken formatting, and what can be said.
fn formatting(settings: &Settings) -> [Node; 2] {
    let mut rows = vec![
        setting(Icon::Formatting, "Format by voice")
            .detail("Say a command after a short pause, the way you would start a sentence")
            .control(toggle(settings.spoken_formatting, |on| {
                Action::send(Command::Set(Change::SpokenFormatting(on)))
            }))
            .into(),
    ];
    if settings.spoken_formatting {
        rows.push(
            expander(
                "formatting",
                Icon::Info,
                "What you can say",
                [
                    plain_setting("New line, new paragraph")
                        .indented()
                        .detail("Say it on its own, then pause, to start a new line")
                        .into(),
                    plain_setting("Snake case, camel case, kebab case, all caps")
                        .indented()
                        .detail(
                            "Say it before the words to write that way, up to your next \
                             pause. Snake case user id types user_id",
                        )
                        .into(),
                ],
            )
            .into(),
        );
    }
    [header("Spoken formatting"), card(rows)]
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::super::common::probe::{all, buttons, command, flipped, keys, scene, setting_row};
    use super::*;
    use crate::mock;

    /// What the fields with these keys do when Enter is pressed in them.
    fn on_enter(nodes: &[&Node], key: Key) -> Option<Command> {
        nodes.iter().find_map(|node| match node {
            Node::Field(field) if field.key == key => command(&field.on_enter),
            _ => None,
        })
    }

    #[test]
    fn the_groups_are_all_there_and_say_what_they_are_for() {
        let page = content(&scene("settings"));
        let nodes = all(&page.body);
        assert_eq!(page.title, "Words");
        let headers: Vec<_> = nodes
            .iter()
            .filter_map(|node| match node {
                Node::Header(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(headers, ["Dictionary", "Snippets", "Spoken formatting"]);
        for title in ["Add a word", "Add a snippet", "Format by voice"] {
            assert!(!setting_row(&nodes, title).detail.is_empty(), "{title}");
        }
    }

    #[test]
    fn keys_are_unique() {
        for name in mock::SCENES {
            let page = content(&scene(name));
            let keys = keys(&all(&page.body));
            let unique: HashSet<_> = keys.iter().collect();
            for key in [WORD, SOUNDS_LIKE, TRIGGER, TEXT] {
                assert!(keys.contains(&key), "{name}: {key}");
            }
            assert_eq!(unique.len(), keys.len(), "{name}");
        }
    }

    #[test]
    fn what_is_on_a_list_opens_and_says_how_many() {
        let mut view = scene("settings");
        view.settings.dictionary = vec![
            Word {
                word: "VinPUT".to_owned(),
                sounds_like: "vin put".to_owned(),
            },
            Word {
                word: "PUTLAB".to_owned(),
                sounds_like: String::new(),
            },
        ];
        view.settings.snippets.clear();
        let page = content(&view);
        let nodes = all(&page.body);
        let opens: Vec<_> = nodes
            .iter()
            .filter_map(|node| match node {
                Node::Expander(expander) => {
                    Some((expander.title.as_str(), expander.value.as_str()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(opens[0], ("Your words", "2 words"));
        assert!(opens.iter().all(|(title, _)| *title != "Your snippets"));
        assert!(nodes.iter().any(|node| matches!(
            node,
            Node::Scroll {
                key: "dictionary-words",
                ..
            }
        )));
    }

    #[test]
    fn add_and_enter_send_what_is_typed_in_both_fields() {
        let page = content(&scene("settings"));
        let nodes = all(&page.body);
        let typed = || super::super::common::probe::TYPED.to_owned();
        let word = Command::Set(Change::AddWord(typed(), typed()));
        let snippet = Command::Set(Change::AddSnippet(typed(), typed()));
        assert_eq!(on_enter(&nodes, WORD), Some(word.clone()));
        assert_eq!(on_enter(&nodes, SOUNDS_LIKE), Some(word.clone()));
        assert_eq!(on_enter(&nodes, TRIGGER), Some(snippet.clone()));
        assert_eq!(on_enter(&nodes, TEXT), Some(snippet.clone()));
        let added: Vec<_> = buttons(&nodes)
            .into_iter()
            .filter(|button| button.label == "Add")
            .map(|button| command(&button.action))
            .collect();
        assert_eq!(added, [Some(word), Some(snippet)]);
    }

    #[test]
    fn every_entry_is_listed_and_can_be_removed() {
        let view = scene("settings");
        let page = content(&view);
        let nodes = all(&page.body);
        let known = &view.settings.dictionary;
        let saved = &view.settings.snippets;
        assert!(
            !known.is_empty() && !saved.is_empty(),
            "the scene has entries"
        );
        let removed = |label: String| {
            buttons(&nodes)
                .into_iter()
                .find(|button| button.hint == label)
                .and_then(|button| command(&button.action))
        };
        for word in known {
            let row = setting_row(&nodes, &word.word);
            assert_eq!(row.detail.is_empty(), word.sounds_like.is_empty());
            assert!(row.detail.ends_with(&word.sounds_like));
            assert_eq!(
                removed(format!("Remove {}", word.word)),
                Some(Command::Set(Change::RemoveWord(word.word.clone())))
            );
        }
        for snippet in saved {
            let row = setting_row(&nodes, &snippet.trigger);
            assert!(!row.detail.is_empty() && !row.detail.contains('\n'));
            assert_eq!(
                removed(format!("Remove {}", snippet.trigger)),
                Some(Command::Set(Change::RemoveSnippet(snippet.trigger.clone())))
            );
        }
    }

    #[test]
    fn the_commands_are_listed_only_while_they_are_listened_for() {
        let mut view = scene("settings");
        let page = content(&view);
        let nodes = all(&page.body);
        let switch = setting_row(&nodes, "Format by voice");
        assert_eq!(
            flipped(switch, false),
            Some(Change::SpokenFormatting(false))
        );
        assert!(
            !setting_row(&nodes, "New line, new paragraph")
                .detail
                .is_empty()
        );

        view.settings.spoken_formatting = false;
        let page = content(&view);
        let titles: Vec<_> = all(&page.body)
            .into_iter()
            .filter_map(|node| match node {
                Node::Setting(row) => Some(row.title.clone()),
                _ => None,
            })
            .collect();
        assert!(!titles.iter().any(|title| title.starts_with("New line")));
        assert!(titles.iter().any(|title| title == "Format by voice"));
    }
}
