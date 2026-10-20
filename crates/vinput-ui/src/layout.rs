//! Where everything goes.
//!
//! Layout turns a tree of [`Node`]s into two flat lists: [`Item`]s, which are
//! only painted, and [`Control`]s, which the mouse and the keyboard work. Both
//! carry finished rectangles in device-independent units. Nothing here
//! touches Windows: text is measured through [`Measure`], so the whole of it
//! runs in a test with a ruler made of arithmetic.
//!
//! The lists are in visual order, which is also the order Tab walks the
//! controls in and the order things are painted in: a row that can be
//! clicked comes before the button that sits on it.

use vinput_view::Page;

use crate::geometry::{Rect, Size};
use crate::node::{
    Action, ButtonKind, Icon, Key, OnPick, OnText, OnToggle, Pickers, Severity, TextStyle, Tone,
};
use crate::theme::metric;

/// Measures text. The real one asks `DirectWrite`; tests count characters.
pub trait Measure {
    /// The room `text` takes in `style` when it may be at most `max_width`
    /// wide and wraps where it has to.
    fn text(&self, text: &str, style: TextStyle, max_width: f32) -> Size;
}

/// What color something that is only painted has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ink {
    /// The color of a tone of text.
    Tone(Tone),
    /// The color that goes with a message bar of this severity.
    Severity(Severity),
}

/// Which corners of a rectangle inside a card are the card's own rounded
/// ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Corners {
    /// The two at the top.
    pub top: bool,
    /// The two at the bottom.
    pub bottom: bool,
}

impl Corners {
    /// All four: something that stands alone.
    pub const ALL: Self = Self {
        top: true,
        bottom: true,
    };
}

/// Something that is painted and never reacts.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    /// The panel of a card.
    Card(Rect),
    /// A hairline.
    Divider(Rect),
    /// Text.
    Text {
        /// Where. It wraps at the width of this.
        rect: Rect,
        /// What it says.
        text: String,
        /// Its size and weight.
        style: TextStyle,
        /// Its color.
        tone: Tone,
        /// Whether it is cut off with an ellipsis instead of wrapping.
        trim: bool,
    },
    /// A glyph of the icon font, in the middle of its rectangle.
    Glyph {
        /// Where.
        rect: Rect,
        /// Which.
        glyph: char,
        /// How large.
        size: f32,
        /// Its color.
        ink: Ink,
    },
    /// A small rounded label.
    Badge {
        /// Where.
        rect: Rect,
        /// What it says.
        text: String,
        /// Its color.
        tone: Tone,
    },
    /// A progress bar; `None` moves by itself.
    Progress {
        /// Where.
        rect: Rect,
        /// How far along it is, from 0 to 1.
        value: Option<f32>,
        /// The color of what is filled.
        tone: Tone,
    },
    /// The panel of a message bar.
    Bar {
        /// Where.
        rect: Rect,
        /// How serious it is.
        severity: Severity,
    },
    /// The tint and the accent pill of the chosen row of a card.
    Selected {
        /// The row.
        rect: Rect,
        /// Which of the card's corners it reaches.
        corners: Corners,
    },
    /// A small round dot, such as the one that says Vinput is listening.
    Dot {
        /// Where.
        rect: Rect,
        /// Its color.
        tone: Tone,
    },
    /// The logo of the app.
    Logo(Rect),
    /// The control with this index in the list of controls, painted here.
    Control(usize),
    /// What follows, up to the next [`Item::Unclip`], is in the area with
    /// this index among those that scroll by themselves: it is cut off at
    /// the edges of that area and moves as it is scrolled.
    Clip(usize, Pickers),
    /// The end of what an [`Item::Clip`] began.
    Unclip,
}

/// What kind of control something is, with what it shows and does.
pub enum Kind {
    /// A button.
    Button {
        /// Its caption; may be empty.
        label: String,
        /// The icon before the caption.
        icon: Option<Icon>,
        /// How much it stands out.
        kind: ButtonKind,
        /// What pressing it does.
        action: Action,
        /// What it is for, when it has no caption.
        hint: String,
    },
    /// Text that acts like a link.
    Link {
        /// What it says.
        label: String,
        /// What clicking it does.
        action: Action,
    },
    /// A switch.
    Toggle {
        /// Whether it is on.
        on: bool,
        /// What flipping it does.
        action: OnToggle,
    },
    /// A drop-down list.
    Choice {
        /// What can be picked.
        items: Vec<String>,
        /// The one that is picked.
        selected: usize,
        /// What picking one does.
        action: OnPick,
    },
    /// A line of text to edit.
    Field {
        /// What it holds.
        text: String,
        /// What it shows while empty.
        placeholder: String,
        /// Whether what is typed is hidden.
        secret: bool,
        /// How many lines of text it shows.
        lines: u8,
        /// Whether what is pasted is handed in at once.
        eager: bool,
        /// Set if the page is laid out again at every key, which only the
        /// field of a list that is searched asks for.
        live: Option<Pickers>,
        /// What finishing an edit does.
        action: OnText,
        /// What Enter does besides.
        on_enter: Action,
    },
    /// A field that takes a key combination.
    Hotkey {
        /// The combination that is set, or empty.
        text: String,
        /// Whether it may be cleared.
        clearable: bool,
        /// What setting one does.
        action: OnText,
    },
    /// A whole settings row that can be clicked.
    Row {
        /// What clicking it does.
        action: Action,
    },
    /// An entry of the navigation pane.
    Nav {
        /// The page it opens.
        page: Page,
        /// Its name.
        label: String,
        /// Its icon.
        icon: Icon,
        /// Whether its page, or one reached from it, is open.
        selected: bool,
    },
    /// The row of a setting that opens and closes what is under it.
    Expander {
        /// Whether it is open.
        open: bool,
        /// That this program has rows that open.
        pickers: Pickers,
    },
    /// A chip, with a cross that takes it away if it can be.
    Chip {
        /// What it says.
        label: String,
        /// What the cross does. A chip without one cannot be used.
        remove: Option<Action>,
        /// That this program has chips.
        pickers: Pickers,
    },
}

impl Kind {
    /// A number for the kind, to tell whether two controls are alike.
    #[must_use]
    pub const fn rank(&self) -> u8 {
        match self {
            Self::Button { .. } => 0,
            Self::Link { .. } => 1,
            Self::Toggle { .. } => 2,
            Self::Choice { .. } => 3,
            Self::Field { .. } => 4,
            Self::Hotkey { .. } => 5,
            Self::Row { .. } => 6,
            Self::Nav { .. } => 7,
            Self::Expander { .. } => 8,
            Self::Chip { .. } => 9,
        }
    }
}

/// Which part of a window something is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    /// A part that stays put: the navigation pane, the title, what a page
    /// pins under it, the footer.
    Fixed,
    /// The part that scrolls.
    Body,
}

/// Something the mouse and the keyboard work.
pub struct Control {
    /// Where it is. For one in the body, where it is before any scrolling.
    pub rect: Rect,
    /// What it is.
    pub kind: Kind,
    /// Whether it can be used.
    pub enabled: bool,
    /// The name that finds it again after the page is rebuilt.
    pub key: Option<Key>,
    /// The part of the window it is in.
    pub region: Region,
    /// What it belongs to, for a control that says nothing itself: the title
    /// of the settings row a switch sits on.
    pub label: String,
    /// For a row, which of its card's rounded corners it reaches.
    pub corners: Corners,
    /// The area that scrolls by itself it is in, by its index, if it is in
    /// one; see [`Control::pane`].
    pub area: Option<(usize, Pickers)>,
}

/// An area that scrolls by itself, inside a page.
#[derive(Debug, Clone, PartialEq)]
pub struct Pane {
    /// The name that finds how far it is scrolled again after a rebuild.
    pub key: Key,
    /// Where it is. For one in the body, where it is before any scrolling
    /// of the page.
    pub rect: Rect,
    /// How tall what it holds is in all.
    pub content: f32,
    /// The part of the window it is in.
    pub region: Region,
}

impl Pane {
    /// How far it can be scrolled.
    #[must_use]
    pub fn range(&self) -> f32 {
        (self.content - self.rect.h).max(0.0)
    }
}

/// What the layer remembers of a page that decides how it is laid out:
/// which rows the user opened or closed, and what is typed in the fields
/// that something on the page follows.
///
/// A page has a handful of such rows and fields at most, so both are short
/// lists that are looked through.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Memory {
    open: Vec<(String, bool)>,
    typed: Vec<(String, String)>,
}

impl Memory {
    /// Whether the row with this key is open, where the user had a say.
    #[must_use]
    pub fn open(&self, key: &str) -> Option<bool> {
        let found = self.open.iter().find(|(known, _)| known == key);
        found.map(|(_, open)| *open)
    }

    /// Says that the user opened or closed the row with this key.
    pub fn set_open(&mut self, key: &str, open: bool) {
        match self.open.iter_mut().find(|(known, _)| known == key) {
            Some((_, was)) => *was = open,
            None => self.open.push((key.to_owned(), open)),
        }
    }

    /// What the field with this key reads, if anything was typed in it.
    #[must_use]
    pub fn typed(&self, key: &str) -> Option<&str> {
        let found = self.typed.iter().find(|(known, _)| known == key);
        found.map(|(_, typed)| typed.as_str())
    }

    /// Says what the field with this key reads now.
    pub fn set_typed(&mut self, key: &str, typed: String) {
        match self.typed.iter_mut().find(|(known, _)| known == key) {
            Some((_, was)) => *was = typed,
            None => self.typed.push((key.to_owned(), typed)),
        }
    }
}

impl Control {
    /// The area that scrolls by itself it is in, by its index, if it is in
    /// one.
    #[must_use]
    pub fn pane(&self) -> Option<usize> {
        self.area.map(|(index, _)| index)
    }

    /// What a screen reader calls it.
    #[must_use]
    pub fn name(&self) -> &str {
        match &self.kind {
            Kind::Button { label, hint, .. } if label.is_empty() => hint,
            Kind::Button { label, .. } | Kind::Link { label, .. } | Kind::Nav { label, .. } => {
                label
            }
            _ => &self.label,
        }
    }

    /// The tooltip it shows, if it has one: what a button without a caption
    /// is for.
    #[must_use]
    pub fn tip(&self) -> Option<&str> {
        match &self.kind {
            Kind::Button { label, hint, .. } if label.is_empty() && !hint.is_empty() => Some(hint),
            _ => None,
        }
    }

    /// The part of a hotkey field that clears it, if it has one right now.
    #[must_use]
    pub fn clear_rect(&self) -> Option<Rect> {
        match &self.kind {
            Kind::Hotkey {
                text, clearable, ..
            } if *clearable && !text.is_empty() && self.enabled => Some(Rect::new(
                self.rect.right() - metric::CONTROL,
                self.rect.y,
                metric::CONTROL,
                self.rect.h,
            )),
            _ => None,
        }
    }
}

/// A ruler for tests: every character is as wide as half the size of its
/// type, and text wraps between words.
#[cfg(test)]
pub(crate) mod ruler {
    use super::{Measure, Size, TextStyle};
    use crate::theme::face;

    /// See the module.
    pub(crate) struct Ruler;

    impl Measure for Ruler {
        fn text(&self, text: &str, style: TextStyle, max_width: f32) -> Size {
            let face = face(style);
            let advance = face.size / 2.0;
            let (mut lines, mut line, mut widest) = (1.0_f32, 0.0_f32, 0.0_f32);
            for word in text.split_inclusive(' ') {
                let width = advance * super::build::count_f32(word.chars().count());
                if line > 0.0 && line + width > max_width {
                    lines += 1.0;
                    line = 0.0;
                }
                line += width;
                widest = widest.max(line.min(max_width));
            }
            Size::new(widest, lines * face.line)
        }
    }
}

mod build;
#[cfg(test)]
mod tests;

pub use build::{Builder, Laid, REMOVE, SWITCH_WORDS};
