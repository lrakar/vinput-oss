//! What the user is in the middle of: where the mouse is, what has the
//! focus, how far the page is scrolled, what is typed in a field.
//!
//! A [`Stage`] holds the latest view, the laid-out window and all of that
//! state. The window tells it what happened (the mouse moved, a key went
//! down, a new view arrived) and gets back [`Effect`]s: things only a real
//! window can do, such as telling the background process or putting the
//! system's edit control over a field. Nothing here touches Windows, so the
//! whole of the behavior runs in tests.
//!
//! Pages are rebuilt from scratch whenever the view changes. What must
//! survive that is found again by the [`Key`] of its node, or failing a key,
//! by its place among the controls.

use std::collections::HashMap;

use vinput_view::{Command, Page, Setup, View};

use crate::geometry::{Point, Rect, Size};
use crate::layout::{Control, Kind, Measure, Region};
use crate::look::{Look, ScrollBar};
use crate::node::{Action, Key};
use crate::scene::{Phase, Showing};
use crate::shell::{self, Frame};
use crate::theme::metric;

/// Virtual-key codes the stage acts on.
pub mod vk {
    /// Tab.
    pub const TAB: u16 = 0x09;
    /// Enter.
    pub const ENTER: u16 = 0x0D;
    /// Esc.
    pub const ESCAPE: u16 = 0x1B;
    /// Space.
    pub const SPACE: u16 = 0x20;
    /// Page Up.
    pub const PAGE_UP: u16 = 0x21;
    /// Page Down.
    pub const PAGE_DOWN: u16 = 0x22;
    /// End.
    pub const END: u16 = 0x23;
    /// Home.
    pub const HOME: u16 = 0x24;
    /// The left arrow.
    pub const LEFT: u16 = 0x25;
    /// The up arrow.
    pub const UP: u16 = 0x26;
    /// The right arrow.
    pub const RIGHT: u16 = 0x27;
    /// The down arrow.
    pub const DOWN: u16 = 0x28;
    /// F4.
    pub const F4: u16 = 0x73;
}

/// Something only the real window can do.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Tell the background process.
    Send(Command),
    /// Close the window; in a window that asks, with the answer no.
    Close,
    /// Close the window without an answer: Esc.
    Dismiss,
    /// Close a window that asks with the answer yes.
    Accept,
    /// Put this text on the clipboard.
    Copy(String),
    /// Put what is on the clipboard into the edit control, in place of
    /// what it holds.
    Paste,
    /// Put the system's edit control over the field with this key.
    Edit(Editing),
    /// Take the edit control away again.
    EndEdit,
    /// Put this text into the edit control, all of it selected.
    Retype(String),
    /// Open the drop-down of a list.
    OpenChoice(Choosing),
    /// Close the drop-down that is open.
    CloseChoice,
    /// Bring the window to the front.
    Front,
}

/// What the edit control needs to know to lie over a field.
#[derive(Debug, Clone, PartialEq)]
pub struct Editing {
    /// The field.
    pub key: Key,
    /// The text to start with.
    pub text: String,
    /// Whether what is typed is hidden.
    pub secret: bool,
    /// Whether to select everything: the field was reached by keyboard.
    pub select_all: bool,
}

/// What a drop-down needs to know to open.
#[derive(Debug, Clone, PartialEq)]
pub struct Choosing {
    /// The list, where it is in the window right now.
    pub anchor: Rect,
    /// What can be picked.
    pub items: Vec<String>,
    /// The one that is picked.
    pub selected: usize,
}

/// The shape of the mouse pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cursor {
    /// The arrow.
    Arrow,
    /// The hand that links get.
    Hand,
    /// The beam that text gets.
    Text,
}

/// Finds a control again after a rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Mark {
    key: Option<Key>,
    index: usize,
    rank: u8,
}

/// Text typed into a field that the view does not know yet.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Draft {
    /// What the field reads now.
    text: String,
    /// What it read when the edit began, or after the last commit.
    origin: String,
    /// What the view said the field held when the draft was made.
    model: String,
    /// Whether it was handed to the page and led to something: the view is
    /// about to say what became of it.
    sent: bool,
}

/// A hotkey field waiting for keys.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Capture {
    key: Key,
    held: String,
}

/// See the module.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts about the window"
)]
pub struct Stage {
    measure: Box<dyn Measure>,
    showing: Showing,
    view: View,
    frame: Frame,
    mouse: Option<Point>,
    hover: Option<usize>,
    pressed: Option<usize>,
    focus: Option<Mark>,
    ring: bool,
    scroll: f32,
    bar: ScrollBar,
    /// While the thumb is dragged: how far under its top it was grabbed.
    drag: Option<f32>,
    drafts: HashMap<Key, Draft>,
    editing: Option<Key>,
    capture: Option<Capture>,
    open: Option<Key>,
    over_clear: bool,
    phase: f32,
    mica: bool,
    dirty: bool,
    /// Counts the times the page became another one; see [`Stage::shape`].
    shape: u64,
}

/// What a frame is made of, without what it says: enough to tell whether a
/// place in it still stands for the same thing.
#[derive(PartialEq, Eq)]
struct Outline {
    /// How many things are painted in the parts that stay put, and in the
    /// part that scrolls.
    items: (usize, usize),
    /// The kind, the key and the part of the window of every control.
    controls: Vec<(u8, Option<Key>, Region)>,
}

impl Outline {
    fn of(frame: &Frame) -> Self {
        Self {
            items: (frame.fixed.len(), frame.body.len()),
            controls: frame
                .controls
                .iter()
                .map(|control| (control.kind.rank(), control.key, control.region))
                .collect(),
        }
    }
}

impl Stage {
    /// A stage that shows `showing` for `view` in a window of `size`.
    #[must_use]
    pub fn new(showing: Showing, view: View, size: Size, measure: Box<dyn Measure>) -> Self {
        let frame = Self::lay(&showing, &view, size, measure.as_ref());
        Self {
            measure,
            showing,
            view,
            frame,
            mouse: None,
            hover: None,
            pressed: None,
            focus: None,
            ring: false,
            scroll: 0.0,
            bar: ScrollBar::Hidden,
            drag: None,
            drafts: HashMap::new(),
            editing: None,
            capture: None,
            open: None,
            over_clear: false,
            phase: 0.0,
            mica: false,
            dirty: true,
            shape: 0,
        }
    }

    fn lay(showing: &Showing, view: &View, size: Size, measure: &dyn Measure) -> Frame {
        shell::frame(showing.content(view), showing.chrome(view), size, measure)
    }

    // ---- What the window reads ----------------------------------------

    /// The window, laid out.
    #[must_use]
    pub fn frame(&self) -> &Frame {
        &self.frame
    }

    /// What is being shown.
    #[must_use]
    pub fn showing(&self) -> &Showing {
        &self.showing
    }

    /// What a window says to the background process when it is up: that it
    /// is ready for its first view, and, if it opens on the page of the
    /// account, that that page is in sight.
    #[must_use]
    pub fn first_words(&self) -> Vec<Command> {
        let mut words = vec![Command::Ready];
        if self.showing == Showing::Settings(Page::Pro) {
            words.push(Command::ProShown);
        }
        words
    }

    /// The latest view.
    #[must_use]
    pub fn view(&self) -> &View {
        &self.view
    }

    /// What painting needs to know besides the frame.
    #[must_use]
    pub fn look(&self) -> Look<'_> {
        Look {
            hover: self.hover,
            pressed: self.pressed,
            focus: self.focused(),
            ring: self.ring,
            // A field scrolled partly out of sight has no edit control over
            // it for the time being, and draws its text itself.
            editing: self
                .edit_rect()
                .filter(|(_, seen)| *seen)
                .and_then(|_| self.keyed(self.editing?)),
            capture: self
                .capture
                .as_ref()
                .and_then(|capture| Some((self.keyed(capture.key)?, capture.held.as_str()))),
            open: self.open.and_then(|key| self.keyed(key)),
            over_clear: self.over_clear,
            scroll: self.scroll,
            bar: self.bar,
            phase: self.phase,
            mica: self.mica,
        }
    }

    /// A number that changes whenever the window comes to be made of other
    /// things than before: another page, a row more or less. While it stays
    /// the same, the control with a given index is the same control, however
    /// much what it says may have changed. Assistive tools, which hold on to
    /// controls by their place, are told when it changes.
    #[must_use]
    pub fn shape(&self) -> u64 {
        self.shape
    }

    /// Whether anything changed since the last call, so the window has to
    /// be painted again.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// Whether something on it moves by itself and needs a timer.
    #[must_use]
    pub fn moving(&self) -> bool {
        self.frame.moving
    }

    /// Moves what moves by itself on by a part of its sweep.
    pub fn advance(&mut self, part: f32) {
        self.phase = (self.phase + part).fract();
        self.dirty = true;
    }

    /// Says whether Windows shows its wallpaper tint behind the pane.
    pub fn set_mica(&mut self, mica: bool) {
        self.dirty |= self.mica != mica;
        self.mica = mica;
    }

    /// The index of the control with the keyboard focus.
    #[must_use]
    pub fn focused(&self) -> Option<usize> {
        let mark = self.focus?;
        let control = self.frame.controls.get(mark.index)?;
        (control.kind.rank() == mark.rank && control.key == mark.key).then_some(mark.index)
    }

    /// Where the control with this index is in the window right now, with
    /// scrolling taken into account.
    #[must_use]
    pub fn rect_of(&self, index: usize) -> Option<Rect> {
        let control = self.frame.controls.get(index)?;
        Some(self.placed(control))
    }

    /// Where the edit control belongs, and whether it is in sight: a field
    /// that was scrolled out of the page is not.
    #[must_use]
    pub fn edit_rect(&self) -> Option<(Rect, bool)> {
        let control = self.frame.controls.get(self.keyed(self.editing?)?)?;
        let rect = self.placed(control);
        let inner = rect.inset_xy(metric::FIELD_PAD, metric::FIELD_FOCUS);
        let view = self.frame.viewport;
        let seen =
            control.region == Region::Fixed || (rect.y >= view.y && rect.bottom() <= view.bottom());
        Some((inner, seen))
    }

    /// The shape the mouse pointer has where it is.
    #[must_use]
    pub fn cursor(&self) -> Cursor {
        match self.hover.and_then(|index| self.frame.controls.get(index)) {
            Some(control) if !control.enabled => Cursor::Arrow,
            Some(Control {
                kind: Kind::Link { .. },
                ..
            }) => Cursor::Hand,
            Some(Control {
                kind: Kind::Field { .. },
                ..
            }) => Cursor::Text,
            _ => Cursor::Arrow,
        }
    }

    /// The tooltip of the control under the mouse and where that control is,
    /// if it has one and is not held down.
    #[must_use]
    pub fn tip(&self) -> Option<(&str, Rect)> {
        if self.pressed.is_some() {
            return None;
        }
        let control = self.frame.controls.get(self.hover?)?;
        Some((control.tip()?, self.placed(control)))
    }

    fn placed(&self, control: &Control) -> Rect {
        match control.region {
            Region::Fixed => control.rect,
            Region::Body => control.rect.moved(0.0, -self.scroll),
        }
    }

    fn keyed(&self, key: Key) -> Option<usize> {
        self.frame
            .controls
            .iter()
            .position(|control| control.key == Some(key))
    }

    fn mark(&self, index: usize) -> Option<Mark> {
        let control = self.frame.controls.get(index)?;
        Some(Mark {
            key: control.key,
            index,
            rank: control.kind.rank(),
        })
    }

    // ---- Rebuilding ---------------------------------------------------

    /// Lays the window out again and finds everything the user was in the
    /// middle of.
    fn rebuild(&mut self) -> Vec<Effect> {
        let mut effects = Vec::new();
        let size = self.frame.size;
        let before = Outline::of(&self.frame);
        self.frame = Self::lay(&self.showing, &self.view, size, self.measure.as_ref());
        if Outline::of(&self.frame) != before {
            self.shape = self.shape.wrapping_add(1);
        }
        self.patch_drafts();

        // Focus: by key if it has one, else by place and kind.
        if let Some(mark) = self.focus {
            let found = match mark.key {
                Some(key) => self.keyed(key),
                None => self
                    .frame
                    .controls
                    .get(mark.index)
                    .filter(|control| control.kind.rank() == mark.rank && control.key.is_none())
                    .map(|_| mark.index),
            };
            self.focus = found
                .filter(|index| self.frame.controls[*index].enabled)
                .and_then(|index| self.mark(index));
        }
        // A field that is gone, or cannot be typed in any more, loses its
        // edit control.
        let typable = |key: Key| {
            self.keyed(key)
                .is_some_and(|index| self.frame.controls[index].enabled)
        };
        if self.editing.is_some_and(|key| !typable(key)) {
            self.editing = None;
            effects.push(Effect::EndEdit);
        }
        if self
            .capture
            .as_ref()
            .is_some_and(|capture| self.keyed(capture.key).is_none())
        {
            self.capture = None;
            effects.push(Effect::Send(Command::CapturingHotkey(false)));
        }
        if self.open.is_some_and(|key| self.keyed(key).is_none()) {
            self.open = None;
            effects.push(Effect::CloseChoice);
        }
        self.pressed = None;
        self.scroll = self.scroll.clamp(0.0, self.frame.scroll_range());
        self.rehover();
        self.dirty = true;
        effects
    }

    /// Shows what is typed instead of what the view says, in every field
    /// that has a draft, and drops the drafts the view has caught up with.
    fn patch_drafts(&mut self) {
        let editing = self.editing;
        let controls = &mut self.frame.controls;
        self.drafts.retain(|key, draft| {
            let Some(Kind::Field { text, .. }) = controls
                .iter_mut()
                .find(|control| control.key == Some(*key))
                .map(|control| &mut control.kind)
            else {
                return false;
            };
            let stale = draft.sent || *text != draft.model;
            if stale && editing != Some(*key) {
                return false;
            }
            text.clone_from(&draft.text);
            true
        });
    }

    /// The window is now `size` large.
    pub fn resize(&mut self, size: Size) -> Vec<Effect> {
        if size == self.frame.size {
            return Vec::new();
        }
        self.frame.size = size;
        let mut effects = self.close_choice();
        effects.extend(self.rebuild());
        effects
    }

    /// Text is measured differently from now on: the display or the user's
    /// text size changed.
    pub fn remeasure(&mut self, measure: Box<dyn Measure>) -> Vec<Effect> {
        self.measure = measure;
        self.rebuild()
    }

    /// A new view arrived.
    pub fn set_view(&mut self, view: View) -> Vec<Effect> {
        self.view = view;
        self.rebuild()
    }

    /// The background process asks for a page, in front of everything.
    pub fn show(&mut self, page: Page) -> Vec<Effect> {
        let mut effects = self.go(page, true);
        effects.push(Effect::Front);
        effects
    }

    /// Where the work of a window that asks stands.
    pub fn set_phase(&mut self, phase: Phase) -> Vec<Effect> {
        if let Showing::Asking(asking) = &mut self.showing {
            asking.phase = phase;
        }
        self.rebuild()
    }

    // ---- Doing things -------------------------------------------------

    /// Turns to `page`. `around` says the page being left had no say in it:
    /// the navigation pane or the background process asked.
    fn go(&mut self, page: Page, around: bool) -> Vec<Effect> {
        let Showing::Settings(open) = self.showing else {
            return Vec::new();
        };
        if open == page {
            return Vec::new();
        }
        let from_pane = self
            .focused()
            .and_then(|index| self.frame.controls.get(index))
            .is_some_and(|control| matches!(control.kind, Kind::Nav { .. }));
        let ring = self.ring;
        let mut effects = self.set_focus(None, false);
        effects.extend(self.close_choice());
        match open {
            Page::AddModel if around => effects.push(Effect::Send(Command::ClearFinder)),
            Page::Google if around => effects.push(Effect::Send(Command::Google(Setup::Close))),
            _ => {}
        }
        self.showing = Showing::Settings(page);
        self.drafts.clear();
        self.scroll = 0.0;
        // The page of the account shows what the server says, the prices
        // included; it is asked when the page comes into sight.
        if page == Page::Pro {
            effects.push(Effect::Send(Command::ProShown));
        }
        effects.extend(self.rebuild());
        if from_pane {
            // The keyboard stays in the pane, on the entry that is now open.
            let home =
                self.frame.controls.iter().position(
                    |control| matches!(control.kind, Kind::Nav { selected, .. } if selected),
                );
            self.focus = home.and_then(|index| self.mark(index));
            self.ring = ring;
        }
        effects
    }

    fn run(&mut self, action: Action) -> Vec<Effect> {
        match action {
            Action::None => Vec::new(),
            Action::Send(command) => vec![Effect::Send(command)],
            Action::Go(page) => self.go(page, false),
            Action::Submit { field, make } => vec![Effect::Send(make(self.text_of(field)))],
            Action::Close => vec![Effect::Close],
            Action::Accept => vec![Effect::Accept],
            Action::Copy(text) => vec![Effect::Copy(text)],
            Action::Paste(field) => {
                let Some(index) = self.keyed(field) else {
                    return Vec::new();
                };
                // The field is typed in from here on, as after a click.
                let mut effects = self.set_focus(Some(index), false);
                effects.push(Effect::Paste);
                effects
            }
            Action::Both(first, second) => {
                let mut effects = self.run(*first);
                effects.extend(self.run(*second));
                effects
            }
        }
    }

    /// What the field with this key reads right now.
    fn text_of(&self, key: Key) -> String {
        if let Some(draft) = self.drafts.get(key) {
            return draft.text.clone();
        }
        match self
            .keyed(key)
            .map(|index| &self.frame.controls[index].kind)
        {
            Some(Kind::Field { text, .. }) => text.clone(),
            _ => String::new(),
        }
    }

    /// Works the control with this index, as a click or Enter does.
    fn activate(&mut self, index: usize, by_keyboard: bool) -> Vec<Effect> {
        let Some(control) = self.frame.controls.get_mut(index) else {
            return Vec::new();
        };
        if !control.enabled {
            return Vec::new();
        }
        let anchor = match control.region {
            Region::Fixed => control.rect,
            Region::Body => control.rect.moved(0.0, -self.scroll),
        };
        self.dirty = true;
        match &mut control.kind {
            Kind::Button { action, .. } | Kind::Link { action, .. } | Kind::Row { action } => {
                let action = action.clone();
                self.run(action)
            }
            Kind::Toggle { on, action } => {
                // The switch moves at once; the view confirms it or puts it
                // back.
                *on = !*on;
                let action = action(*on);
                self.run(action)
            }
            Kind::Choice {
                items, selected, ..
            } => {
                let choosing = Choosing {
                    anchor,
                    items: items.clone(),
                    selected: *selected,
                };
                self.open = control.key;
                vec![Effect::OpenChoice(choosing)]
            }
            Kind::Field { .. } => self.begin_edit(index, by_keyboard),
            Kind::Hotkey { .. } => {
                let Some(key) = control.key else {
                    return Vec::new();
                };
                // The background process is told once that a field waits,
                // whichever field it is.
                let waiting = self.capture.replace(Capture {
                    key,
                    held: String::new(),
                });
                if waiting.is_some() {
                    Vec::new()
                } else {
                    vec![Effect::Send(Command::CapturingHotkey(true))]
                }
            }
            Kind::Nav { page, .. } => {
                let page = *page;
                self.go(page, true)
            }
        }
    }
}

mod focus;
mod input;
#[cfg(test)]
mod tests;
