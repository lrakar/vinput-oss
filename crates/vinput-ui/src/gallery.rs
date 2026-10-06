//! A test card: one page with every kind of node on it, in every state that
//! looks different. It is not part of the app; `vinput ui settings --mock
//! gallery` shows it, so that work on the layer under the pages has
//! something to look at that does not change when a page does.

use vinput_view::{Command, Page};

use crate::node::{
    Action, Content, Icon, Node, Severity, TextStyle, Tone, accent_button, badge, bullets, button,
    card, choice, column, field, header, hotkey, icon_button, info_bar, link, mono, note, row,
    setting, styled, text, toggle,
};

/// The name of the scene that shows the gallery.
pub const SCENE: &str = "gallery";

fn nothing() -> Action {
    Action::None
}

fn messages() -> Vec<Node> {
    vec![
        Node::InfoBar {
            severity: Severity::Info,
            text: "A message for information, with a button and a cross.".to_owned(),
            action: Some(("Do it".to_owned(), nothing())),
            dismiss: Some(nothing()),
        },
        info_bar(Severity::Success, "It worked."),
        info_bar(
            Severity::Caution,
            "This one needs attention and has enough to say that it has to wrap onto a second \
             line at the width a page usually has, which makes the bar taller than the others.",
        ),
        Node::InfoBar {
            severity: Severity::Critical,
            text: "It failed.".to_owned(),
            action: None,
            dismiss: Some(nothing()),
        },
    ]
}

fn buttons() -> Node {
    card([
        row([
            button("Standard", nothing()).into(),
            accent_button("Accent", nothing()).into(),
            button("Subtle", nothing())
                .kind(crate::node::ButtonKind::Subtle)
                .into(),
            button("Download", nothing()).icon(Icon::Download).into(),
            icon_button(Icon::Delete, "Remove", nothing()).into(),
            Node::Spring,
            link("A link", nothing()),
        ]),
        row([
            button("Standard", nothing()).enabled(false).into(),
            accent_button("Accent", nothing()).enabled(false).into(),
            button("Download", nothing())
                .icon(Icon::Download)
                .enabled(false)
                .into(),
            icon_button(Icon::Delete, "Remove", nothing())
                .enabled(false)
                .into(),
            Node::Spring,
            badge("In use", Tone::Accent),
            badge("Ready", Tone::Success),
            badge("Old", Tone::Caution),
            badge("Broken", Tone::Critical),
            badge("Plain", Tone::Secondary),
        ]),
    ])
}

fn rows() -> Node {
    let names = [
        "Default (system)",
        "Microphone (USB Audio Device)",
        "Headset",
    ];
    card([
        setting(Icon::Power, "A switch that is on")
            .detail("With a line of detail under the title")
            .control(toggle(true, |_| nothing()))
            .into(),
        setting(Icon::Volume, "A switch that is off")
            .control(toggle(false, |_| nothing()))
            .into(),
        setting(Icon::Sync, "A switch that cannot be used")
            .control(Node::Toggle {
                on: true,
                enabled: false,
                action: Box::new(|_| nothing()),
            })
            .into(),
        setting(Icon::Microphone, "A list")
            .detail("Opens a drop-down with the choices")
            .control(choice("gallery-list", names.map(str::to_owned), 1, |_| {
                nothing()
            }))
            .into(),
        setting(Icon::Globe, "A field")
            .detail("Becomes the system's own edit control while it has the focus")
            .control(
                field("gallery-field", "en-US, sl-SI", |_| nothing())
                    .placeholder("en-US")
                    .into(),
            )
            .into(),
        setting(Icon::Key, "A secret, an empty and a disabled field")
            .control(row([
                field("gallery-secret", "hunter2", |_| nothing())
                    .secret()
                    .into(),
                field("gallery-empty", "", |_| nothing())
                    .placeholder("Paste a key")
                    .into(),
            ]))
            .below(
                field("gallery-disabled", "Out of reach", |_| nothing())
                    .enabled(false)
                    .wide()
                    .into(),
            )
            .into(),
        setting(Icon::Keyboard, "Hotkeys")
            .detail("One that is set and may be cleared, one that is not set")
            .control(row([
                hotkey("gallery-hotkey", "Ctrl+Shift+M", true, |_| nothing()),
                hotkey("gallery-unset", "", true, |_| nothing()),
            ]))
            .into(),
    ])
}

fn more_rows() -> Node {
    card([
        setting(Icon::Model, "The chosen row of its card")
            .detail("148 MB \u{b7} 99 languages")
            .selected(true)
            .control(row([badge("In use", Tone::Accent)]))
            .into(),
        setting(Icon::Download, "A row with something under it")
            .detail("Downloading \u{b7} 412 MB of 669 MB")
            .control(button("Cancel", nothing()).into())
            .below(Node::Progress(Some(0.62)))
            .into(),
        setting(Icon::Sync, "A row whose bar moves by itself")
            .below(Node::Progress(None))
            .below(note("A note under the bar"))
            .into(),
        setting(Icon::Add, "A row that goes to another page")
            .detail("The whole row can be clicked")
            .action(Action::Go(Page::AddModel))
            .into(),
        setting(Icon::Globe, "A row that opens the browser")
            .action(Action::send(Command::OpenLink(
                "https://example.com".to_owned(),
            )))
            .into(),
        setting(Icon::Warning, "A row that cannot be used")
            .detail("Not a format VinPUT can run")
            .enabled(false)
            .control(button("Add", nothing()).enabled(false).into())
            .into(),
        setting(Icon::Plug, "A row with text for a control")
            .detail("And a button that copies it")
            .control(row([
                mono(r"\\.\pipe\vinput"),
                icon_button(
                    Icon::Copy,
                    "Copy",
                    Action::Copy(r"\\.\pipe\vinput".to_owned()),
                )
                .into(),
            ]))
            .into(),
        setting(
            Icon::Warning,
            "A row whose detail is a warning, long enough that it wraps",
        )
        .detail(
            "The microphone you chose is not connected. VinPUT uses the Windows default \
                 until it is back, and this sentence goes on so that the detail needs more \
                 than one line next to a control.",
        )
        .detail_tone(Tone::Caution)
        .icon_tone(Tone::Caution)
        .control(button("Fix", nothing()).into())
        .into(),
    ])
}

fn words() -> Node {
    card([
        column([
            styled("Title", TextStyle::Title, Tone::Primary),
            styled("Subtitle", TextStyle::Subtitle, Tone::Primary),
            styled("Body strong", TextStyle::BodyStrong, Tone::Primary),
            text("Body: the quick brown fox jumps over the lazy dog."),
            note("Caption: a note in small, quiet type."),
            mono(r"Mono: C:\Users\you\Documents\Vinput memos"),
        ]),
        row([
            styled("Accent", TextStyle::Body, Tone::Accent),
            styled("Success", TextStyle::Body, Tone::Success),
            styled("Caution", TextStyle::Body, Tone::Caution),
            styled("Critical", TextStyle::Body, Tone::Critical),
            styled("Secondary", TextStyle::Body, Tone::Secondary),
            styled("Disabled", TextStyle::Body, Tone::Disabled),
            Node::Spring,
            Node::Icon(Icon::Check, Tone::Success),
            Node::Icon(Icon::Warning, Tone::Caution),
            Node::Icon(Icon::Error, Tone::Critical),
        ]),
        bullets([
            "A point of a list",
            "Another, which is long enough to wrap onto a second line when the window is at \
             the size it opens with, to show that the text lines up behind the bullet",
        ]),
    ])
}

fn icons() -> Node {
    use Icon::{
        Add, Back, Check, ChevronRight, Chip, Clipboard, Cloud, Copy, Delete, Dismiss, Download,
        Error, Folder, Globe, Info, Key, Keyboard, Memo, Microphone, Model, More, OpenExternal,
        Plug, Power, Search, Settings, Storage, Sync, Volume, Warning,
    };
    let all = [
        Settings,
        Keyboard,
        Microphone,
        Globe,
        Power,
        Volume,
        Sync,
        Model,
        Download,
        Chip,
        Storage,
        Folder,
        Plug,
        Cloud,
        Key,
        Memo,
        Search,
        Clipboard,
        OpenExternal,
        Info,
        Add,
        Delete,
        Back,
        Check,
        Dismiss,
        Warning,
        Error,
        ChevronRight,
        More,
        Copy,
    ];
    let (first, second) = all.split_at(all.len() / 2);
    let line = |icons: &[Icon]| row(icons.iter().map(|icon| Node::Icon(*icon, Tone::Primary)));
    card([line(first), line(second)])
}

/// The page.
#[must_use]
pub fn content() -> Content {
    let mut body = messages();
    body.push(header("Buttons, links and badges"));
    body.push(buttons());
    body.push(header("Rows"));
    body.push(note("A note between a heading and its card."));
    body.push(rows());
    body.push(more_rows());
    body.push(header("Text"));
    body.push(words());
    body.push(header("Icons"));
    body.push(icons());
    body.push(header("Things outside a card"));
    body.push(
        setting(Icon::Info, "A row on its own")
            .detail("Drawn as a card of one")
            .into(),
    );
    body.push(Node::Progress(Some(0.3)));
    body.push(Node::Gauge {
        value: 1.0,
        tone: Tone::Caution,
    });
    body.push(Node::Busy("Something is being waited for.".to_owned()));
    body.push(card([
        crate::node::plain_setting("A step of an instruction")
            .numbered(1)
            .detail("Its number stands where an icon would")
            .into(),
        crate::node::plain_setting("A row without an icon that lines up with those that have one")
            .indented()
            .into(),
    ]));
    body.push(row([
        text("A row of text and controls on the page itself"),
        Node::Spring,
        toggle(true, |_| nothing()),
    ]));
    Content {
        pinned: Vec::new(),
        title: "Gallery".to_owned(),
        body,
        footer: vec![
            accent_button("Accept", nothing()).into(),
            button("Close", Action::Close).into(),
        ],
        back: Some(Action::Go(Page::General)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gallery_has_every_kind_of_node() {
        fn mark(node: &Node, seen: &mut [bool; 18]) {
            let slot = match node {
                Node::Column(children) | Node::Row(children) | Node::Card(children) => {
                    for child in children {
                        mark(child, seen);
                    }
                    match node {
                        Node::Column(_) => 0,
                        Node::Row(_) => 1,
                        _ => 2,
                    }
                }
                Node::Spring => 3,
                Node::Header(_) => 4,
                Node::Text { .. } => 5,
                Node::List(_) => 6,
                Node::Icon(..) => 7,
                Node::Setting(setting) => {
                    for child in setting
                        .control
                        .iter()
                        .map(AsRef::as_ref)
                        .chain(&setting.below)
                    {
                        mark(child, seen);
                    }
                    8
                }
                Node::Button(_) => 9,
                Node::Link { .. } => 10,
                Node::Toggle { .. } => 11,
                Node::Choice { .. } => 12,
                Node::Field(_) => 13,
                Node::Hotkey { .. } => 14,
                Node::Progress(_) | Node::Badge { .. } | Node::InfoBar { .. } => 15,
                Node::Gauge { .. } => 16,
                Node::Busy(_) => 17,
            };
            seen[slot] = true;
        }
        let mut seen = [false; 18];
        for node in &content().body {
            mark(node, &mut seen);
        }
        assert_eq!(seen, [true; 18]);
    }
}
