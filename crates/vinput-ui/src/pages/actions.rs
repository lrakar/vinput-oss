//! The actions of the user's own, on the Shortcuts page: keys that hand what
//! was said to a program of their choice.
//!
//! Each action is a row that opens onto what it is made of, each part
//! changed as it is typed or picked, like every other setting. The row at
//! the end adds one, set up as an example that works as it is. A row and
//! its fields are named by the place of the action on the list, so the
//! names are fixed: there are at most [`MAX`] of them.

use vinput_view::{Change, Command, CustomAction, Then, Tidy};

use crate::node::{
    Action, Icon, Key, Node, button, card, choice, expander, field, header, hotkey, icon_button,
    note, plain_setting, row, setting,
};

/// The most actions there can be; the engine keeps to the same number.
pub(super) const MAX: usize = 8;

/// The names of one kind of node, one for each place on the list.
macro_rules! keys {
    ($prefix:literal) => {
        [
            concat!($prefix, "-0"),
            concat!($prefix, "-1"),
            concat!($prefix, "-2"),
            concat!($prefix, "-3"),
            concat!($prefix, "-4"),
            concat!($prefix, "-5"),
            concat!($prefix, "-6"),
            concat!($prefix, "-7"),
        ]
    };
}

const ROW: [Key; MAX] = keys!("action");
const NAME: [Key; MAX] = keys!("action-name");
const HOTKEY: [Key; MAX] = keys!("action-key");
const PROGRAM: [Key; MAX] = keys!("action-program");
const ARGUMENTS: [Key; MAX] = keys!("action-arguments");
const CLEANUP: [Key; MAX] = keys!("action-cleanup");
const THEN: [Key; MAX] = keys!("action-then");
const TIMEOUT: [Key; MAX] = keys!("action-timeout");

/// The nodes with a name that each action has.
#[cfg(test)]
pub(super) const KEYS_EACH: usize = 8;

/// What is done with the dictation first, as the Cleanup page says it.
const CLEANUPS: [(Tidy, &str); 4] = [
    (Tidy::Plain, "As said"),
    (Tidy::Clean, "Clean"),
    (Tidy::Prompt, "Coding prompt"),
    (Tidy::Email, "Email"),
];

/// What is done with what the program writes.
const THENS: [(Then, &str); 3] = [
    (Then::Nothing, "Nothing"),
    (Then::Type, "Type it"),
    (Then::Copy, "Copy it"),
];

/// The time a program may take, in seconds; 0 is no limit.
const TIMEOUTS: [(u32, &str); 5] = [
    (10, "10 seconds"),
    (30, "30 seconds"),
    (60, "1 minute"),
    (300, "5 minutes"),
    (0, "No limit"),
];

/// The group of the actions: one row for each, and the row that adds one.
pub(super) fn section(actions: &[CustomAction]) -> [Node; 3] {
    let rows = actions
        .iter()
        .take(MAX)
        .enumerate()
        .map(|(index, action)| action_row(index, action));
    let full = actions.len() >= MAX;
    let add = setting(Icon::Add, "Add an action")
        .detail(if full {
            "There is no room for more. Remove one to add another."
        } else {
            "Run a program or a script with what you say"
        })
        .control(
            button("Add", Action::send(Command::Set(Change::AddAction)))
                .icon(Icon::Add)
                .enabled(!full)
                .hint("Add an action")
                .into(),
        )
        .into();
    [
        header("Your actions"),
        card(rows.chain([add])),
        note(
            "A program gets what you said on its standard input, in the variable VINPUT_TEXT, \
             and in place of {text} in its arguments. A new action starts as an example that \
             adds what you say to Notes.txt in your user folder.",
        ),
    ]
}

/// The row of one action, which opens onto its parts. One without a key
/// yet, as a new one is, starts open.
fn action_row(index: usize, action: &CustomAction) -> Node {
    let value = if action.hotkey.is_empty() {
        "No key".to_owned()
    } else {
        action.hotkey.clone()
    };
    expander(
        ROW[index],
        Icon::Program,
        &action.name,
        [
            text_row(NAME[index], "Name", &action.name, index, Change::ActionName),
            plain_setting("Key")
                .indented()
                .compact()
                .control({
                    let before = action.hotkey.clone();
                    hotkey(HOTKEY[index], &action.hotkey, true, move |typed| {
                        changed(&before, &typed, index, Change::ActionHotkey)
                    })
                })
                .into(),
            wide_row(
                (PROGRAM[index], "Program"),
                "A name such as powershell.exe, or the whole path to a program",
                (&action.program, "powershell.exe"),
                index,
                Change::ActionProgram,
            ),
            wide_row(
                (ARGUMENTS[index], "Arguments"),
                "Put {text} where what you said goes, apart from other text",
                (&action.arguments, "{text}"),
                index,
                Change::ActionArguments,
            ),
            pick_row(
                (CLEANUP[index], "Before it is handed over"),
                &CLEANUPS,
                action.cleanup,
                move |tidy| Change::ActionCleanup(index, tidy),
            ),
            pick_row(
                (THEN[index], "With what the program writes"),
                &THENS,
                action.then,
                move |then| Change::ActionThen(index, then),
            ),
            pick_row(
                (TIMEOUT[index], "Stop it after"),
                &TIMEOUTS,
                action.timeout,
                move |seconds| Change::ActionTimeout(index, seconds),
            ),
            plain_setting("Try it")
                .indented()
                .compact()
                .detail("Runs it with a sample sentence")
                .control(row([
                    button("Test", Action::send(Command::TestAction(index))).into(),
                    icon_button(
                        Icon::Delete,
                        format!("Remove {}", action.name),
                        Action::send(Command::Set(Change::RemoveAction(index))),
                    )
                    .into(),
                ]))
                .into(),
        ],
    )
    .value(value)
    .open(action.hotkey.is_empty())
    .into()
}

/// What finishing an edit of a part of the action at `index` sends: the
/// change, if the text is not what it was.
fn changed(before: &str, typed: &str, index: usize, change: fn(usize, String) -> Change) -> Action {
    let typed = typed.trim();
    if typed == before.trim() {
        Action::None
    } else {
        Action::send(Command::Set(change(index, typed.to_owned())))
    }
}

/// A row with a field at its right edge.
fn text_row(
    key: Key,
    title: &str,
    text: &str,
    index: usize,
    change: fn(usize, String) -> Change,
) -> Node {
    let before = text.to_owned();
    plain_setting(title)
        .indented()
        .compact()
        .control(
            field(key, text, move |typed| {
                changed(&before, &typed, index, change)
            })
            .into(),
        )
        .into()
}

/// A row with a field as wide as the row under it.
fn wide_row(
    (key, title): (Key, &str),
    detail: &str,
    (text, placeholder): (&str, &str),
    index: usize,
    change: fn(usize, String) -> Change,
) -> Node {
    let before = text.to_owned();
    plain_setting(title)
        .indented()
        .detail(detail)
        .below(
            field(key, text, move |typed| {
                changed(&before, &typed, index, change)
            })
            .placeholder(placeholder)
            .wide()
            .into(),
        )
        .into()
}

/// A row that picks one of `choices` from a list. A value that is not on
/// offer shows as the first.
fn pick_row<T: Copy + PartialEq + 'static>(
    (key, title): (Key, &str),
    choices: &'static [(T, &'static str)],
    chosen: T,
    change: impl Fn(T) -> Change + 'static,
) -> Node {
    let selected = choices
        .iter()
        .position(|(value, _)| *value == chosen)
        .unwrap_or(0);
    plain_setting(title)
        .indented()
        .compact()
        .control(choice(
            key,
            choices.iter().map(|(_, name)| (*name).to_owned()),
            selected,
            move |index| match choices.get(index) {
                Some((value, _)) if index != selected => Action::send(Command::Set(change(*value))),
                _ => Action::None,
            },
        ))
        .into()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::super::common::probe::{Seen, all, buttons, command, keys, setting_row, typed};
    use super::*;
    use crate::node::Expander;

    fn notes() -> CustomAction {
        CustomAction {
            name: "Add to my notes".to_owned(),
            hotkey: "Ctrl+Alt+N".to_owned(),
            program: "powershell.exe".to_owned(),
            arguments: "-NoProfile -Command \"Add-Content -Path $HOME\\Notes.txt -Value \
                        $env:VINPUT_TEXT\""
                .to_owned(),
            then: Then::Nothing,
            cleanup: Tidy::Plain,
            timeout: 30,
        }
    }

    fn rows(nodes: &[&Node]) -> Vec<(String, String, bool)> {
        nodes
            .iter()
            .filter_map(|node| match node {
                Node::Expander(Expander {
                    title, value, open, ..
                }) => Some((title.clone(), value.clone(), *open)),
                _ => None,
            })
            .collect()
    }

    /// What picking item `index` of the list called `key` sends.
    fn picked(nodes: &[&Node], key: &str, index: usize) -> Option<Command> {
        nodes.iter().find_map(|node| match node {
            Node::Choice {
                key: name, action, ..
            } if *name == key => command(&action(index)),
            _ => None,
        })
    }

    #[test]
    fn each_action_is_a_row_that_opens_and_the_last_row_adds_one() {
        let fresh = CustomAction {
            name: "Translate".to_owned(),
            ..notes()
        };
        let fresh = CustomAction {
            hotkey: String::new(),
            ..fresh
        };
        let section = section(&[notes(), fresh]);
        let nodes = all(&section);
        assert_eq!(
            rows(&nodes),
            [
                ("Add to my notes".to_owned(), "Ctrl+Alt+N".to_owned(), false),
                ("Translate".to_owned(), "No key".to_owned(), true),
            ]
        );
        let add = buttons(&nodes)
            .into_iter()
            .find(|button| button.label == "Add");
        assert_eq!(
            add.and_then(|button| command(&button.action)),
            Some(Command::Set(Change::AddAction))
        );
        assert!(Seen::of(&section).says("VINPUT_TEXT"));
        assert!(Seen::of(&section).says("{text}"));
        let keys = keys(&nodes);
        assert_eq!(keys.len(), 2 * KEYS_EACH);
        assert_eq!(keys.iter().collect::<HashSet<_>>().len(), keys.len());
    }

    #[test]
    fn there_is_no_room_for_more_than_there_can_be() {
        let section = section(&vec![notes(); MAX]);
        let nodes = all(&section);
        assert_eq!(rows(&nodes).len(), MAX);
        assert!(
            !setting_row(&nodes, "Add an action")
                .detail
                .contains("Run a")
        );
        let add = buttons(&nodes)
            .into_iter()
            .find(|button| button.label == "Add");
        assert!(add.is_some_and(|button| !button.enabled));
        let keys = keys(&nodes);
        assert_eq!(keys.iter().collect::<HashSet<_>>().len(), MAX * KEYS_EACH);
    }

    #[test]
    fn every_part_sends_its_change_with_the_place_of_the_action() {
        let section = section(&[notes(), notes()]);
        let nodes = all(&section);
        assert_eq!(
            typed(&nodes, "action-name-1", " Notes "),
            Some(Change::ActionName(1, "Notes".to_owned()))
        );
        assert_eq!(typed(&nodes, "action-name-1", "Add to my notes"), None);
        assert_eq!(
            typed(&nodes, "action-key-0", "F7"),
            Some(Change::ActionHotkey(0, "F7".to_owned()))
        );
        assert_eq!(
            typed(&nodes, "action-key-1", ""),
            Some(Change::ActionHotkey(1, String::new()))
        );
        assert_eq!(
            typed(&nodes, "action-program-1", "pwsh.exe"),
            Some(Change::ActionProgram(1, "pwsh.exe".to_owned()))
        );
        assert_eq!(
            typed(&nodes, "action-arguments-0", "-c {text}"),
            Some(Change::ActionArguments(0, "-c {text}".to_owned()))
        );
        assert_eq!(
            picked(&nodes, "action-then-1", 2),
            Some(Command::Set(Change::ActionThen(1, Then::Copy)))
        );
        assert_eq!(picked(&nodes, "action-then-1", 0), None, "it is chosen");
        assert_eq!(
            picked(&nodes, "action-cleanup-0", 1),
            Some(Command::Set(Change::ActionCleanup(0, Tidy::Clean)))
        );
        assert_eq!(
            picked(&nodes, "action-timeout-1", 4),
            Some(Command::Set(Change::ActionTimeout(1, 0)))
        );
        let pressed: Vec<_> = buttons(&nodes)
            .into_iter()
            .filter_map(|button| command(&button.action))
            .collect();
        for wanted in [
            Command::TestAction(0),
            Command::TestAction(1),
            Command::Set(Change::RemoveAction(0)),
            Command::Set(Change::RemoveAction(1)),
        ] {
            assert!(pressed.contains(&wanted), "{wanted:?}");
        }
    }
}
