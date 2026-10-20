//! What a page is made of.
//!
//! A page is a function from a [`vinput_view::View`] to a tree of [`Node`]s:
//! plain data that says what is on the page, never how it is drawn or where.
//! The layer under it (layout, painting, the mouse, the keyboard, focus,
//! scaling for the display) is written once and treats every page alike. So
//! changing a page is changing a few lines of data, and a page cannot get
//! spacing, colors or keyboard handling wrong, because it does not have any.
//!
//! ```ignore
//! card([
//!     setting(Icon::Microphone, "Audio input")
//!         .detail("The microphone VinPUT listens with")
//!         .control(choice("microphone", names, selected, |index| {
//!             Action::send(Command::Set(Change::Microphone(ids[index].clone())))
//!         })),
//!     setting(Icon::Power, "Start with Windows")
//!         .control(toggle(view.settings.autostart, |on| {
//!             Action::send(Command::Set(Change::Autostart(on)))
//!         })),
//! ])
//! ```
//!
//! Pages keep no state. What the user is in the middle of (the text in a
//! field, which list is open, where the page is scrolled to) belongs to the
//! layer, which finds it again after every rebuild by the [`Key`] of the
//! node it belongs to.

use vinput_view::{Command, Page};

/// What a control does when the user works it.
#[derive(Debug, Clone)]
pub enum Action {
    /// Nothing.
    None,
    /// Tell the background process.
    Send(Command),
    /// Turn to another page.
    Go(Page),
    /// Tell the background process what is typed in a field, for a button
    /// beside it such as Find. The command is made from the text.
    Submit {
        /// The field to read.
        field: Key,
        /// Makes the command from its text.
        make: fn(String) -> Command,
    },
    /// Tell the background process what is typed in two fields that make
    /// one entry of a list, for the Add button beside them. Both fields
    /// are then emptied and the first is typed in again, ready for the
    /// next entry.
    Enter {
        /// The fields to read, in the order the command takes them.
        fields: [Key; 2],
        /// Makes the command from their texts.
        make: fn(String, String) -> Command,
    },
    /// Close the window. In a window that asks, this is the answer no.
    Close,
    /// Close a window that asks with the answer yes: what the button that
    /// says Install or Yes does. Elsewhere it closes the window.
    Accept,
    /// Put this text on the clipboard. The layer does it; nothing is sent.
    Copy(String),
    /// Put what is on the clipboard into the field with this key, in place
    /// of what it holds, for a button beside it such as Paste.
    Paste(Key),
    /// Do one thing, then another: tell the background process to forget
    /// something and turn to another page, say.
    Both(Box<Action>, Box<Action>),
}

impl Action {
    /// Shorthand for [`Action::Send`].
    #[must_use]
    pub fn send(command: Command) -> Self {
        Self::Send(command)
    }

    /// Shorthand for [`Action::Both`]: this, then `next`.
    #[must_use]
    pub fn then(self, next: Self) -> Self {
        Self::Both(Box::new(self), Box::new(next))
    }

    /// Whether it does nothing at all.
    #[must_use]
    pub fn is_none(&self) -> bool {
        match self {
            Self::None => true,
            Self::Both(first, second) => first.is_none() && second.is_none(),
            _ => false,
        }
    }
}

/// Makes the action for a switch from its new position.
pub type OnToggle = Box<dyn Fn(bool) -> Action>;
/// Makes the action for a list from the index picked.
pub type OnPick = Box<dyn Fn(usize) -> Action>;
/// Makes the action for a field from its text, when the user is done with it
/// (Enter, or leaving the field).
pub type OnText = Box<dyn Fn(String) -> Action>;

/// A name for a node that keeps something between rebuilds: fields, lists
/// and expanders. It must be unique on its page and stay the same from one
/// rebuild to the next.
pub type Key = &'static str;

/// The sizes and weights of text, after the Windows 11 type ramp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextStyle {
    /// 12 px. Notes under things.
    Caption,
    /// 14 px. Almost everything.
    #[default]
    Body,
    /// 14 px semibold. Names of things.
    BodyStrong,
    /// 20 px semibold. Headings inside a page.
    Subtitle,
    /// 28 px semibold. The title of a page.
    Title,
    /// 14 px in a font where every character is as wide as the next: names
    /// a program reads, such as the name of a pipe or a path.
    Mono,
}

/// What a piece of text or an icon is saying, which decides its color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tone {
    /// Ordinary text.
    #[default]
    Primary,
    /// Less important: details, notes.
    Secondary,
    /// Cannot be used right now.
    Disabled,
    /// The accent color of Windows: in use, a link, under way.
    Accent,
    /// It worked.
    Success,
    /// Needs attention.
    Caution,
    /// It failed.
    Critical,
}

/// The icons, by what they mean. The layer draws each with a glyph of the
/// Segoe Fluent Icons font, which every Windows 11 has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    /// Settings in general.
    Settings,
    /// A key on the keyboard: hotkeys.
    Keyboard,
    /// A microphone: audio input, dictation.
    Microphone,
    /// Languages.
    Globe,
    /// Starting with Windows.
    Power,
    /// Sounds.
    Volume,
    /// Updates.
    Sync,
    /// A length of time.
    Timer,
    /// Speech models.
    Model,
    /// Something to download.
    Download,
    /// A graphics card or processor.
    Chip,
    /// Disk space.
    Storage,
    /// A folder.
    Folder,
    /// Other programs connecting: the local API.
    Plug,
    /// An online service.
    Cloud,
    /// An API key.
    Key,
    /// A voice memo.
    Memo,
    /// Searching the web.
    Search,
    /// The clipboard.
    Clipboard,
    /// A link that opens elsewhere.
    OpenExternal,
    /// About the app.
    Info,
    /// Adding something.
    Add,
    /// Removing something.
    Delete,
    /// Going back.
    Back,
    /// A tick: done, in use.
    Check,
    /// A cross: close, clear, cancel.
    Dismiss,
    /// Needs attention.
    Warning,
    /// It failed.
    Error,
    /// A chevron pointing right: goes somewhere.
    ChevronRight,
    /// More things to do with an item.
    More,
    /// Copying to the clipboard.
    Copy,
    /// A star: the subscription.
    Star,
    /// A person: an account.
    Person,
    /// A book of words: the dictionary.
    Dictionary,
    /// Text that stands for more text: snippets.
    Snippet,
    /// Letters and their case: spoken formatting.
    Formatting,
    /// A pen: text that is rewritten.
    Edit,
    /// A command prompt: a program of the user's own.
    Program,
}

/// How much a button stands out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ButtonKind {
    /// An ordinary button.
    #[default]
    Standard,
    /// The one thing to do next, in the accent color. At most one in sight.
    Accent,
    /// No frame until the mouse is over it: for the less important.
    Subtle,
}

/// How serious a message bar is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Severity {
    /// For information.
    #[default]
    Info,
    /// It worked.
    Success,
    /// Needs attention.
    Caution,
    /// It failed.
    Critical,
}

/// Stands in every part of a page that picks from a long list: a row that
/// opens, a list that scrolls by itself, chips, a list that is searched.
///
/// In a program that has such pages this is a thing of no size. In one
/// that has none (the feature `pickers` is off) it is a type without a
/// value, so no such part can ever be made there, and the compiler, which
/// knows that, leaves out all the code that would lay one out, paint it or
/// work it. That is what keeps the smallest program of the family small
/// without a second version of the layer.
#[cfg(feature = "pickers")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pickers;

/// See the other [`Pickers`]: here, where no page picks from a long list,
/// there is no such value.
#[cfg(not(feature = "pickers"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pickers {}

/// One thing on a page.
pub enum Node {
    /// Children one under another, with the standard gap between them.
    Column(Vec<Node>),
    /// Children side by side, centered on a line, with the standard gap.
    /// A [`Node::Spring`] among them pushes what follows to the right edge.
    Row(Vec<Node>),
    /// Takes up the room that is left over in a [`Node::Row`].
    Spring,
    /// A heading over a group of cards.
    Header(String),
    /// A line or paragraph of text. It wraps at the width it is given.
    Text {
        /// What it says.
        text: String,
        /// Its size and weight.
        style: TextStyle,
        /// Its color.
        tone: Tone,
    },
    /// Points one under another, each behind a bullet: what is new in a
    /// version, what setup is going to do. Each wraps at the width it gets.
    List(Vec<String>),
    /// An icon on its own.
    Icon(Icon, Tone),
    /// A rounded panel around rows that belong together; the layer draws a
    /// hairline between them. Children are usually [`Node::Setting`]s.
    Card(Vec<Node>),
    /// The row everything in Settings is made of: an icon, a title with a
    /// line of detail under it, and a control at the right edge.
    Setting(Setting),
    /// A button.
    Button(Button),
    /// Text that acts like a link.
    Link {
        /// What it says.
        label: String,
        /// What clicking it does.
        action: Action,
    },
    /// An on/off switch.
    Toggle {
        /// Whether it is on.
        on: bool,
        /// Whether it can be changed.
        enabled: bool,
        /// What flipping it does.
        action: OnToggle,
    },
    /// A drop-down list to pick one of several from.
    Choice {
        /// Keeps the list's place between rebuilds.
        key: Key,
        /// What can be picked.
        items: Vec<String>,
        /// The one that is picked.
        selected: usize,
        /// What picking one does.
        action: OnPick,
    },
    /// A line of text to edit.
    Field(Field),
    /// A field that takes a key combination instead of text: click it, press
    /// the keys. Its text is in the form `Ctrl+Shift+Space`.
    Hotkey {
        /// Keeps the field's place between rebuilds.
        key: Key,
        /// The combination that is set, or empty.
        text: String,
        /// Whether it may be cleared back to "not set".
        clearable: bool,
        /// What setting a combination does; it gets the new text, or an
        /// empty one when the field is cleared.
        action: OnText,
    },
    /// A bar that fills up. `None` moves without saying how far along it is.
    Progress(Option<f32>),
    /// A bar that shows how much of something is used, in the color of
    /// what that means: the accent while there is room, caution when not.
    Gauge {
        /// How much, from 0 to 1.
        value: f32,
        /// Its color.
        tone: Tone,
    },
    /// A message across the page that something is being waited for, with a
    /// line along its bottom edge that moves while it is. As tall as any
    /// other message bar, so the answer that takes its place does not make
    /// the page jump.
    Busy(String),
    /// A small rounded label, such as "In use".
    Badge {
        /// What it says.
        text: String,
        /// Its color.
        tone: Tone,
    },
    /// A message across the page about something that happened.
    InfoBar {
        /// How serious it is.
        severity: Severity,
        /// What it says.
        text: String,
        /// A button at its right edge: its caption and what it does.
        action: Option<(String, Action)>,
        /// What the cross at its right edge does, if it has one.
        dismiss: Option<Action>,
    },
    /// A settings row that opens: closed, it says what is chosen at its
    /// right edge; open, it shows what that is chosen from under it. A
    /// child of a [`Node::Card`], or a card by itself.
    Expander(Expander),
    /// Rows one under another in an area that is at most `max` tall and
    /// scrolls by itself, with a scroll bar of its own, so a long list does
    /// not make the page longer. Its children are laid out as those of a
    /// [`Node::Card`] are. One inside another is not scrolled apart.
    Scroll {
        /// Keeps how far it is scrolled between rebuilds.
        key: Key,
        /// The most height it takes.
        max: f32,
        /// What scrolls.
        children: Vec<Node>,
        /// That this program has such lists.
        pickers: Pickers,
    },
    /// Children side by side that go on in a new line where the width runs
    /// out: chips.
    Flow(Vec<Node>, Pickers),
    /// A small rounded label for one of several things that are chosen,
    /// with a cross that takes it away.
    Chip {
        /// What it says.
        label: String,
        /// What the cross does. Without one the chip only says what it says.
        remove: Option<Action>,
        /// That this program has chips.
        pickers: Pickers,
    },
    /// A field to search with over a list that scrolls by itself. The layer
    /// shows the rows that contain what is typed and no others, so a page
    /// lists everything and keeps no state.
    Search(Search),
}

/// A settings row that opens; see [`Node::Expander`].
pub struct Expander {
    /// Keeps whether it is open between rebuilds.
    pub key: Key,
    /// The icon at its left edge.
    pub icon: Option<Icon>,
    /// What the setting is.
    pub title: String,
    /// What is chosen, said at the right edge of the row.
    pub value: String,
    /// The color of that.
    pub value_tone: Tone,
    /// Whether it is open until the user says otherwise.
    pub open: bool,
    /// What it shows under the row while it is open, laid out as the
    /// children of a [`Node::Card`] are.
    pub children: Vec<Node>,
    /// That this program has rows that open.
    pub pickers: Pickers,
}

impl Expander {
    /// Says what is chosen.
    #[must_use]
    pub fn value(mut self, value: impl Into<String>) -> Self {
        self.value = value.into();
        self
    }

    /// Colors what is chosen.
    #[must_use]
    pub fn value_tone(mut self, tone: Tone) -> Self {
        self.value_tone = tone;
        self
    }

    /// Has it open until the user closes it.
    #[must_use]
    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }
}

impl From<Expander> for Node {
    fn from(expander: Expander) -> Self {
        Self::Expander(expander)
    }
}

/// A field to search with over a list; see [`Node::Search`].
pub struct Search {
    /// Keeps what is typed and how far the list is scrolled.
    pub key: Key,
    /// What the field shows while it is empty.
    pub placeholder: String,
    /// The most height the list takes.
    pub max: f32,
    /// The rows, each with the words it is found by.
    pub rows: Vec<(String, Node)>,
    /// What is said in place of the list when no row has what is typed.
    pub none: String,
    /// That this program has lists that are searched.
    pub pickers: Pickers,
}

impl From<Search> for Node {
    fn from(search: Search) -> Self {
        Self::Search(search)
    }
}

/// The row everything in Settings is made of; see [`Node::Setting`].
pub struct Setting {
    /// The icon at its left edge.
    pub icon: Option<Icon>,
    /// A number in the place of the icon, for a row that is a step of an
    /// instruction. Zero keeps the place of the icon empty, so that the
    /// text of a row without one lines up with the rows that have one.
    pub number: Option<u8>,
    /// What the setting is.
    pub title: String,
    /// A line about it, under the title, in smaller and quieter type.
    pub detail: String,
    /// The color of that line.
    pub detail_tone: Tone,
    /// What is at its right edge: a control, or a [`Node::Row`] of them.
    pub control: Option<Box<Node>>,
    /// Shown under the row, inside the same card and indented to the title:
    /// a progress bar, a note, further rows.
    pub below: Vec<Node>,
    /// What clicking anywhere on the row does. A row that goes somewhere
    /// shows a chevron at its right edge.
    pub action: Action,
    /// Whether the row stands out as the chosen one of its card.
    pub selected: bool,
    /// Whether the row can be used. One that cannot is drawn in the quiet
    /// color of things that are out of reach and does not react to a click;
    /// the controls on it say for themselves whether they can be used.
    pub enabled: bool,
    /// The color of the icon.
    pub icon_tone: Tone,
    /// Whether the row is as low as its text allows: a row of a long list.
    pub compact: bool,
}

impl Setting {
    /// Adds the line of detail under the title.
    #[must_use]
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    /// Colors the line of detail.
    #[must_use]
    pub fn detail_tone(mut self, tone: Tone) -> Self {
        self.detail_tone = tone;
        self
    }

    /// Puts a control at the right edge.
    #[must_use]
    pub fn control(mut self, control: Node) -> Self {
        self.control = Some(Box::new(control));
        self
    }

    /// Adds something under the row.
    #[must_use]
    pub fn below(mut self, node: Node) -> Self {
        self.below.push(node);
        self
    }

    /// Makes the whole row clickable.
    #[must_use]
    pub fn action(mut self, action: Action) -> Self {
        self.action = action;
        self
    }

    /// Marks the row as the chosen one.
    #[must_use]
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Sets whether the row can be used.
    #[must_use]
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Colors the icon.
    #[must_use]
    pub fn icon_tone(mut self, tone: Tone) -> Self {
        self.icon_tone = tone;
        self
    }

    /// Puts a number where the icon would be: the row is this step of an
    /// instruction.
    #[must_use]
    pub fn numbered(mut self, number: u8) -> Self {
        self.number = Some(number);
        self
    }

    /// Makes the row as low as its text allows.
    #[must_use]
    pub fn compact(mut self) -> Self {
        self.compact = true;
        self
    }

    /// Keeps the place of the icon empty, so that the text lines up with
    /// that of the rows around it that have one.
    #[must_use]
    pub fn indented(self) -> Self {
        self.numbered(0)
    }
}

impl From<Setting> for Node {
    fn from(setting: Setting) -> Self {
        Self::Setting(setting)
    }
}

/// A button; see [`Node::Button`].
pub struct Button {
    /// Its caption. May be empty for a button that is only an icon.
    pub label: String,
    /// An icon before the caption.
    pub icon: Option<Icon>,
    /// How much it stands out.
    pub kind: ButtonKind,
    /// Whether it can be pressed.
    pub enabled: bool,
    /// What pressing it does.
    pub action: Action,
    /// What a button without a caption is for, for screen readers and the
    /// tooltip.
    pub hint: String,
}

impl Button {
    /// Puts an icon before the caption.
    #[must_use]
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Sets how much it stands out.
    #[must_use]
    pub fn kind(mut self, kind: ButtonKind) -> Self {
        self.kind = kind;
        self
    }

    /// Sets whether it can be pressed.
    #[must_use]
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Says what a button without a caption is for.
    #[must_use]
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }
}

impl From<Button> for Node {
    fn from(button: Button) -> Self {
        Self::Button(button)
    }
}

/// A line of text to edit; see [`Node::Field`].
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts about a field"
)]
pub struct Field {
    /// Keeps the field's text and caret between rebuilds.
    pub key: Key,
    /// What it holds, as far as the background process knows. While the user
    /// is typing in it, the layer shows what they typed instead.
    pub text: String,
    /// Shown in quiet type while it is empty.
    pub placeholder: String,
    /// Whether what is typed is hidden, for a key or a password.
    pub secret: bool,
    /// Whether it takes all the width it can get instead of the standard
    /// width of a control.
    pub wide: bool,
    /// How many lines of text it shows. With more than one, the text wraps,
    /// Enter starts a new line, and leaving the field finishes the edit.
    pub lines: u8,
    /// Whether it can be typed in.
    pub enabled: bool,
    /// Whether what is pasted into it is handed in at once, as if Enter
    /// had followed: for a key, which is pasted whole.
    pub eager: bool,
    /// Set if the page is laid out again at every key, because something
    /// on it follows what is typed: the field of a [`Node::Search`].
    pub live: Option<Pickers>,
    /// What finishing an edit does: Enter, or leaving the field.
    pub action: OnText,
    /// What Enter does besides, and leaving the field does not: running the
    /// search the field is for, say. Usually an [`Action::Submit`] of this
    /// field.
    pub on_enter: Action,
}

impl Field {
    /// Sets what is shown while it is empty.
    #[must_use]
    pub fn placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// Hides what is typed.
    #[must_use]
    pub fn secret(mut self) -> Self {
        self.secret = true;
        self
    }

    /// Lets it take all the width it can get.
    #[must_use]
    pub fn wide(mut self) -> Self {
        self.wide = true;
        self
    }

    /// Makes it as tall as `lines` lines of text, and as wide as it can be.
    #[must_use]
    pub fn lines(mut self, lines: u8) -> Self {
        self.lines = lines.max(1);
        self.wide = true;
        self
    }

    /// Sets whether it can be typed in.
    #[must_use]
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Has what is pasted into it handed in at once.
    #[must_use]
    pub fn eager(mut self) -> Self {
        self.eager = true;
        self
    }

    /// Sets what Enter does besides finishing the edit.
    #[must_use]
    pub fn on_enter(mut self, action: Action) -> Self {
        self.on_enter = action;
        self
    }
}

impl From<Field> for Node {
    fn from(field: Field) -> Self {
        Self::Field(field)
    }
}

/// A page of the Settings window, or the whole of a smaller window.
pub struct Content {
    /// The title in large type at the top.
    pub title: String,
    /// What stays put under the title while the body scrolls under it.
    /// Empty on most pages.
    pub pinned: Vec<Node>,
    /// What scrolls under the title and what is pinned there.
    pub body: Vec<Node>,
    /// What stays at the bottom edge, such as the buttons of a dialog.
    pub footer: Vec<Node>,
    /// Where the arrow beside the title goes back to, for a page that was
    /// reached from another.
    pub back: Option<Action>,
}

/// Children one under another.
#[must_use]
pub fn column(children: impl IntoIterator<Item = Node>) -> Node {
    Node::Column(children.into_iter().collect())
}

/// Children side by side.
#[must_use]
pub fn row(children: impl IntoIterator<Item = Node>) -> Node {
    Node::Row(children.into_iter().collect())
}

/// A heading over a group of cards.
#[must_use]
pub fn header(text: impl Into<String>) -> Node {
    Node::Header(text.into())
}

/// Ordinary text.
#[must_use]
pub fn text(text: impl Into<String>) -> Node {
    styled(text, TextStyle::Body, Tone::Primary)
}

/// A note in small, quiet type.
#[must_use]
pub fn note(text: impl Into<String>) -> Node {
    styled(text, TextStyle::Caption, Tone::Secondary)
}

/// Text of any size and color.
#[must_use]
pub fn styled(text: impl Into<String>, style: TextStyle, tone: Tone) -> Node {
    Node::Text {
        text: text.into(),
        style,
        tone,
    }
}

/// Text in the font where every character is as wide as the next.
#[must_use]
pub fn mono(text: impl Into<String>) -> Node {
    styled(text, TextStyle::Mono, Tone::Primary)
}

/// Points one under another, each behind a bullet. Empty ones are left out.
#[must_use]
pub fn bullets<S: AsRef<str>>(points: impl IntoIterator<Item = S>) -> Node {
    Node::List(
        points
            .into_iter()
            .map(|point| point.as_ref().trim().to_owned())
            .filter(|point| !point.is_empty())
            .collect(),
    )
}

/// A rounded panel around rows that belong together.
#[must_use]
pub fn card(children: impl IntoIterator<Item = Node>) -> Node {
    Node::Card(children.into_iter().collect())
}

/// A settings row with an icon and a title; add the rest with its methods.
#[must_use]
pub fn setting(icon: Icon, title: impl Into<String>) -> Setting {
    Setting {
        icon: Some(icon),
        ..plain_setting(title)
    }
}

/// A settings row without an icon.
#[must_use]
pub fn plain_setting(title: impl Into<String>) -> Setting {
    Setting {
        icon: None,
        number: None,
        title: title.into(),
        detail: String::new(),
        detail_tone: Tone::Secondary,
        control: None,
        below: Vec::new(),
        action: Action::None,
        selected: false,
        enabled: true,
        icon_tone: Tone::Primary,
        compact: false,
    }
}

/// A settings row that opens, with what it shows while it is open.
#[cfg(feature = "pickers")]
#[must_use]
pub fn expander(
    key: Key,
    icon: Icon,
    title: impl Into<String>,
    children: impl IntoIterator<Item = Node>,
) -> Expander {
    Expander {
        key,
        icon: Some(icon),
        title: title.into(),
        value: String::new(),
        value_tone: Tone::Secondary,
        open: false,
        children: children.into_iter().collect(),
        pickers: Pickers,
    }
}

/// Rows in an area that is at most `max` tall and scrolls by itself.
#[cfg(feature = "pickers")]
#[must_use]
pub fn scroll(key: Key, max: f32, children: impl IntoIterator<Item = Node>) -> Node {
    Node::Scroll {
        key,
        max,
        children: children.into_iter().collect(),
        pickers: Pickers,
    }
}

/// Children side by side that go on in a new line where the width runs out.
#[cfg(feature = "pickers")]
#[must_use]
pub fn flow(children: impl IntoIterator<Item = Node>) -> Node {
    Node::Flow(children.into_iter().collect(), Pickers)
}

/// A chip; with `remove`, one with a cross that does it.
#[cfg(feature = "pickers")]
#[must_use]
pub fn chip(label: impl Into<String>, remove: Option<Action>) -> Node {
    Node::Chip {
        label: label.into(),
        remove,
        pickers: Pickers,
    }
}

/// An ordinary button.
#[must_use]
pub fn button(label: impl Into<String>, action: Action) -> Button {
    Button {
        label: label.into(),
        icon: None,
        kind: ButtonKind::Standard,
        enabled: true,
        action,
        hint: String::new(),
    }
}

/// The button for the one thing to do next.
#[must_use]
pub fn accent_button(label: impl Into<String>, action: Action) -> Button {
    button(label, action).kind(ButtonKind::Accent)
}

/// A button that is only an icon; `hint` says what it is for.
#[must_use]
pub fn icon_button(icon: Icon, hint: impl Into<String>, action: Action) -> Button {
    button("", action)
        .icon(icon)
        .kind(ButtonKind::Subtle)
        .hint(hint)
}

/// Text that acts like a link.
#[must_use]
pub fn link(label: impl Into<String>, action: Action) -> Node {
    Node::Link {
        label: label.into(),
        action,
    }
}

/// An on/off switch.
#[must_use]
pub fn toggle(on: bool, action: impl Fn(bool) -> Action + 'static) -> Node {
    Node::Toggle {
        on,
        enabled: true,
        action: Box::new(action),
    }
}

/// A drop-down list.
#[must_use]
pub fn choice(
    key: Key,
    items: impl IntoIterator<Item = String>,
    selected: usize,
    action: impl Fn(usize) -> Action + 'static,
) -> Node {
    Node::Choice {
        key,
        items: items.into_iter().collect(),
        selected,
        action: Box::new(action),
    }
}

/// A line of text to edit.
#[must_use]
pub fn field(
    key: Key,
    text: impl Into<String>,
    action: impl Fn(String) -> Action + 'static,
) -> Field {
    Field {
        key,
        text: text.into(),
        placeholder: String::new(),
        secret: false,
        wide: false,
        lines: 1,
        enabled: true,
        eager: false,
        live: None,
        action: Box::new(action),
        on_enter: Action::None,
    }
}

/// A field that takes a key combination.
#[must_use]
pub fn hotkey(
    key: Key,
    text: impl Into<String>,
    clearable: bool,
    action: impl Fn(String) -> Action + 'static,
) -> Node {
    Node::Hotkey {
        key,
        text: text.into(),
        clearable,
        action: Box::new(action),
    }
}

/// A small rounded label.
#[must_use]
pub fn badge(text: impl Into<String>, tone: Tone) -> Node {
    Node::Badge {
        text: text.into(),
        tone,
    }
}

/// A message across the page.
#[must_use]
pub fn info_bar(severity: Severity, text: impl Into<String>) -> Node {
    Node::InfoBar {
        severity,
        text: text.into(),
        action: None,
        dismiss: None,
    }
}

/// A size in bytes the way Windows says it: `148 MB`, `1.6 GB`.
#[must_use]
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    // Tenths of the unit, rounded to the nearest at every step.
    let mut tenths = (bytes.saturating_mul(10).saturating_add(512)) / 1024;
    let mut unit = 0;
    while tenths >= 10_240 && unit + 1 < UNITS.len() {
        tenths = (tenths + 512) / 1024;
        unit += 1;
    }
    if unit < 2 || tenths >= 100 {
        // Kilobytes and megabytes, and anything of three digits, need no decimals.
        format!("{} {}", (tenths + 5) / 10, UNITS[unit])
    } else {
        format!("{}.{} {}", tenths / 10, tenths % 10, UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_the_way_windows_writes_them() {
        assert_eq!(human_size(0), "0 KB");
        assert_eq!(human_size(148 << 20), "148 MB");
        assert_eq!(human_size(669 << 20), "669 MB");
        assert_eq!(human_size(1_638 << 20), "1.6 GB");
        assert_eq!(human_size(6 << 30), "6.0 GB");
        assert_eq!(human_size(120 << 30), "120 GB");
    }

    #[test]
    fn builders_fill_in_what_was_not_said() {
        let row = setting(Icon::Power, "Start with Windows")
            .detail("So it is ready when you are")
            .selected(true);
        assert_eq!(row.icon, Some(Icon::Power));
        assert_eq!(row.detail_tone, Tone::Secondary);
        assert!(row.selected && row.control.is_none());
        let find = icon_button(Icon::Search, "Find", Action::Close);
        assert_eq!(find.kind, ButtonKind::Subtle);
        assert!(find.label.is_empty() && find.enabled);
    }
}
