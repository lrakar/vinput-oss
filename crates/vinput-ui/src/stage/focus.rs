//! The keyboard focus, typing in a field, and the drop-down of a list.

use vinput_view::Command;

use super::{Draft, Editing, Effect, Stage};
use crate::layout::{Control, Kind, Region};
use crate::node::Action;
use crate::theme::metric;

impl Stage {
    // ---- Focus --------------------------------------------------------

    /// Whether Tab stops at this control.
    pub(super) fn tab_stop(control: &Control) -> bool {
        control.enabled
            && match control.kind {
                // The pane is one stop; the arrows move within it.
                Kind::Nav { selected, .. } => selected,
                _ => true,
            }
    }

    /// Moves the keyboard focus. Leaving a field hands in what was typed;
    /// arriving at one starts typing.
    pub(super) fn set_focus(&mut self, index: Option<usize>, by_keyboard: bool) -> Vec<Effect> {
        let mut effects = Vec::new();
        let before = self.focused();
        if before != index {
            if let Some(capture) = self.capture.take() {
                let _ = capture;
                effects.push(Effect::Send(Command::CapturingHotkey(false)));
            }
            if self.editing.is_some() {
                effects.extend(self.commit());
                self.editing = None;
                effects.push(Effect::EndEdit);
            }
        }
        // Handing in what was typed may have turned the page; a place on the
        // page that was is not a place on this one.
        let index = index.filter(|index| {
            self.frame
                .controls
                .get(*index)
                .is_some_and(|control| control.enabled)
        });
        self.focus = index.and_then(|index| self.mark(index));
        self.ring = by_keyboard && index.is_some();
        self.dirty = true;
        if let Some(index) = index {
            if by_keyboard {
                self.reveal(index);
            }
            let is_field = matches!(self.frame.controls[index].kind, Kind::Field { .. });
            if is_field && self.editing.is_none() {
                effects.extend(self.begin_edit(index, by_keyboard));
            }
        }
        effects
    }

    /// The window got the keyboard back: a field that had the focus is
    /// typed in again.
    pub fn refocus(&mut self) -> Vec<Effect> {
        match self.focused() {
            Some(index)
                if self.editing.is_none()
                    && matches!(self.frame.controls[index].kind, Kind::Field { .. }) =>
            {
                self.begin_edit(index, false)
            }
            _ => Vec::new(),
        }
    }

    /// The window lost the keyboard to another one.
    pub fn blur(&mut self) -> Vec<Effect> {
        let mut effects = self.close_choice();
        if self.capture.take().is_some() {
            effects.push(Effect::Send(Command::CapturingHotkey(false)));
        }
        self.pressed = None;
        self.drag = None;
        self.dirty = true;
        effects
    }

    /// Scrolls so that the control with this index is in sight.
    pub(super) fn reveal(&mut self, index: usize) {
        let control = &self.frame.controls[index];
        if control.region != Region::Body {
            return;
        }
        let view = self.frame.viewport;
        let margin = metric::GAP;
        let top = control.rect.y - margin - view.y;
        let bottom = control.rect.bottom() + margin - view.y;
        if top < self.scroll || control.rect.h + 2.0 * margin > view.h {
            self.scroll = top;
        } else if bottom > self.scroll + view.h {
            self.scroll = bottom - view.h;
        }
        self.scroll = self.scroll.clamp(0.0, self.frame.scroll_range());
    }

    pub(super) fn step_focus(&mut self, forward: bool) -> Vec<Effect> {
        let count = self.frame.controls.len();
        if count == 0 {
            return Vec::new();
        }
        let start = self.focused();
        let mut index = start.unwrap_or(if forward { count - 1 } else { 0 });
        for _ in 0..count {
            index = if forward {
                (index + 1) % count
            } else {
                (index + count - 1) % count
            };
            if Self::tab_stop(&self.frame.controls[index]) {
                return self.set_focus(Some(index), true);
            }
        }
        Vec::new()
    }

    /// Moves the focus within the navigation pane.
    pub(super) fn step_pane(&mut self, from: usize, forward: bool) -> Vec<Effect> {
        let is_nav = |control: &Control| matches!(control.kind, Kind::Nav { .. });
        let next = if forward {
            (from + 1..self.frame.controls.len()).find(|index| is_nav(&self.frame.controls[*index]))
        } else {
            (0..from)
                .rev()
                .find(|index| is_nav(&self.frame.controls[*index]))
        };
        match next {
            Some(index) => self.set_focus(Some(index), true),
            None => Vec::new(),
        }
    }

    // ---- Typing in a field --------------------------------------------

    pub(super) fn begin_edit(&mut self, index: usize, select_all: bool) -> Vec<Effect> {
        let control = &self.frame.controls[index];
        let (Some(key), Kind::Field { text, secret, .. }) = (control.key, &control.kind) else {
            return Vec::new();
        };
        let draft = self.drafts.entry(key).or_insert_with(|| Draft {
            text: text.clone(),
            origin: text.clone(),
            model: text.clone(),
            sent: false,
        });
        draft.origin.clone_from(&draft.text);
        let editing = Editing {
            key,
            text: draft.text.clone(),
            secret: *secret,
            select_all,
        };
        self.editing = Some(key);
        self.focus = self.mark(index);
        self.dirty = true;
        vec![Effect::Edit(editing)]
    }

    /// The edit control reads `text` now.
    pub fn typed(&mut self, text: String) {
        if let Some(draft) = self.editing.and_then(|key| self.drafts.get_mut(key)) {
            draft.text = text;
            draft.sent = false;
        }
    }

    /// Hands what was typed to the page, if it changed since the edit began.
    pub(super) fn commit(&mut self) -> Vec<Effect> {
        let Some(key) = self.editing else {
            return Vec::new();
        };
        let Some(draft) = self.drafts.get_mut(key) else {
            return Vec::new();
        };
        if draft.text == draft.origin {
            return Vec::new();
        }
        draft.origin.clone_from(&draft.text);
        let text = draft.text.clone();
        let action = match self
            .keyed(key)
            .map(|index| &self.frame.controls[index].kind)
        {
            Some(Kind::Field { action, .. }) => action(text),
            _ => Action::None,
        };
        if let Some(draft) = self.drafts.get_mut(key) {
            draft.sent = !action.is_none();
        }
        self.run(action)
    }

    /// Enter was pressed in the edit control.
    pub fn edit_enter(&mut self) -> Vec<Effect> {
        let mut effects = self.commit();
        let on_enter = self
            .editing
            .and_then(|key| self.keyed(key))
            .and_then(|index| match &self.frame.controls[index].kind {
                Kind::Field { on_enter, .. } => Some(on_enter.clone()),
                _ => None,
            });
        if let Some(action) = on_enter {
            effects.extend(self.run(action));
        }
        effects
    }

    /// Something was pasted into the edit control. A field that is eager
    /// hands it in as if Enter had followed.
    pub fn edit_pasted(&mut self) -> Vec<Effect> {
        let eager = self
            .editing
            .and_then(|key| self.keyed(key))
            .is_some_and(|index| {
                matches!(
                    self.frame.controls[index].kind,
                    Kind::Field { eager: true, .. }
                )
            });
        if !eager {
            return Vec::new();
        }
        let mut effects = self.edit_enter();
        // What was pasted stays, selected: another paste takes its place.
        if let Some(draft) = self.editing.and_then(|key| self.drafts.get(key)) {
            effects.push(Effect::Retype(draft.text.clone()));
        }
        effects
    }

    /// Esc was pressed in the edit control: the field goes back to what it
    /// read when the edit began.
    pub fn edit_escape(&mut self) -> Vec<Effect> {
        let Some(draft) = self.editing.and_then(|key| self.drafts.get_mut(key)) else {
            return Vec::new();
        };
        draft.text.clone_from(&draft.origin);
        vec![Effect::Retype(draft.text.clone())]
    }

    /// The edit control lost the keyboard to something outside the window.
    pub fn edit_lost(&mut self) -> Vec<Effect> {
        if self.editing.is_none() {
            return Vec::new();
        }
        let mut effects = self.commit();
        self.editing = None;
        self.dirty = true;
        effects.push(Effect::EndEdit);
        effects
    }

    // ---- The drop-down of a list --------------------------------------

    /// Closes the drop-down, if one is open.
    pub fn close_menu(&mut self) -> Vec<Effect> {
        self.close_choice()
    }

    pub(super) fn close_choice(&mut self) -> Vec<Effect> {
        if self.open.take().is_some() {
            self.dirty = true;
            vec![Effect::CloseChoice]
        } else {
            Vec::new()
        }
    }

    /// Whether a drop-down is open.
    #[must_use]
    pub fn choosing(&self) -> bool {
        self.open.is_some()
    }

    /// The drop-down closed without a pick.
    pub fn choice_closed(&mut self) {
        self.dirty |= self.open.take().is_some();
    }

    /// The entry at `index` of the open drop-down was picked.
    pub fn picked(&mut self, index: usize) -> Vec<Effect> {
        let Some(at) = self.open.take().and_then(|key| self.keyed(key)) else {
            return Vec::new();
        };
        self.dirty = true;
        self.pick(at, index)
    }

    pub(super) fn pick(&mut self, at: usize, index: usize) -> Vec<Effect> {
        let Some(Kind::Choice {
            items,
            selected,
            action,
        }) = self
            .frame
            .controls
            .get_mut(at)
            .map(|control| &mut control.kind)
        else {
            return Vec::new();
        };
        if index >= items.len() || index == *selected {
            return Vec::new();
        }
        let action = action(index);
        // The list shows the pick at once; the view confirms it.
        *selected = index;
        self.dirty = true;
        self.run(action)
    }
}
