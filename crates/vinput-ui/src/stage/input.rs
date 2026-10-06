//! The mouse and the keyboard.

use vinput_view::Command;

use super::{Effect, Stage, vk};
use crate::geometry::Point;
use crate::hotkey::{self, Modifiers};
use crate::layout::{Kind, Region};
use crate::look::ScrollBar;
use crate::node::{Action, ButtonKind, Icon};
use crate::shell::Chrome;
use crate::theme::metric;

impl Stage {
    // ---- The mouse ----------------------------------------------------

    /// The control at `point`, the topmost if several are.
    pub(super) fn at(&self, point: Point) -> Option<usize> {
        let in_body = self.frame.viewport.contains(point);
        self.frame.controls.iter().rposition(|control| {
            let body = control.region == Region::Body;
            (!body || in_body) && self.placed(control).contains(point)
        })
    }

    pub(super) fn rehover(&mut self) {
        let before = (self.hover, self.bar, self.over_clear);
        self.hover = self.mouse.and_then(|point| self.at(point));
        self.over_clear = match (self.mouse, self.hover) {
            (Some(point), Some(index)) => {
                let control = &self.frame.controls[index];
                let dy = self.placed(control).y - control.rect.y;
                control
                    .clear_rect()
                    .is_some_and(|rect| rect.moved(0.0, dy).contains(point))
            }
            _ => false,
        };
        let view = self.frame.viewport;
        self.bar = match self.mouse {
            _ if self.drag.is_some() => ScrollBar::Wide,
            Some(point) if view.contains(point) && self.frame.scroll_range() > 0.0 => {
                if point.x >= view.right() - metric::SCROLL_LANE {
                    ScrollBar::Wide
                } else {
                    ScrollBar::Thin
                }
            }
            _ => ScrollBar::Hidden,
        };
        self.dirty |= before != (self.hover, self.bar, self.over_clear);
    }

    /// The mouse moved to `point`.
    pub fn mouse_move(&mut self, point: Point) {
        self.mouse = Some(point);
        if let Some(grab) = self.drag {
            self.drag_to(point.y - grab);
        }
        self.rehover();
    }

    /// The mouse left the window.
    pub fn mouse_leave(&mut self) {
        self.mouse = None;
        self.rehover();
    }

    pub(super) fn drag_to(&mut self, thumb_top: f32) {
        let Some(thumb) = crate::look::thumb(&self.frame, 0.0, true) else {
            return;
        };
        let lane = self.frame.viewport.inset_xy(0.0, metric::GRID);
        let travel = (lane.h - thumb.h).max(1.0);
        let part = ((thumb_top - lane.y) / travel).clamp(0.0, 1.0);
        self.scroll_to(part * self.frame.scroll_range());
    }

    /// The left button went down at `point`.
    pub fn mouse_down(&mut self, point: Point) -> Vec<Effect> {
        self.mouse = Some(point);
        // A tap arrives without the mouse having moved there first.
        self.rehover();
        let mut effects = self.close_choice();
        if !effects.is_empty() {
            // A click while a drop-down is open only closes it.
            self.rehover();
            return effects;
        }
        if self.bar == ScrollBar::Wide
            && let Some(thumb) = crate::look::thumb(&self.frame, self.scroll, true)
            && point.x >= self.frame.viewport.right() - metric::SCROLL_LANE
        {
            if point.y < thumb.y {
                self.scroll_by(-self.frame.viewport.h);
            } else if point.y >= thumb.bottom() {
                self.scroll_by(self.frame.viewport.h);
            } else {
                self.drag = Some(point.y - thumb.y);
            }
            self.rehover();
            return effects;
        }
        self.rehover();
        self.pressed = self.hover;
        self.dirty = true;
        match self.hover {
            Some(index) if self.frame.controls[index].enabled => {
                // A field is typed in from the moment it is clicked; a
                // hotkey field waits for the click to end.
                effects.extend(self.set_focus(Some(index), false));
            }
            _ => effects.extend(self.set_focus(None, false)),
        }
        effects
    }

    /// The left button came up at `point`.
    pub fn mouse_up(&mut self, point: Point) -> Vec<Effect> {
        self.mouse = Some(point);
        self.drag = None;
        let pressed = self.pressed.take();
        self.rehover();
        self.dirty = true;
        let Some(index) = pressed.filter(|index| self.hover == Some(*index)) else {
            return Vec::new();
        };
        let control = &self.frame.controls[index];
        match &control.kind {
            // Clicking a field already started the edit.
            Kind::Field { .. } => Vec::new(),
            Kind::Hotkey { text, action, .. } if self.over_clear => {
                let action = if text.is_empty() {
                    Action::None
                } else {
                    action(String::new())
                };
                let mut effects = self.end_capture();
                self.set_hotkey_text(index, String::new());
                effects.extend(self.run(action));
                effects
            }
            _ => self.activate(index, false),
        }
    }

    /// The wheel turned by `notches`, away from the user if positive.
    pub fn wheel(&mut self, notches: f32) -> Vec<Effect> {
        let effects = self.close_choice();
        self.scroll_by(-notches * metric::WHEEL);
        effects
    }

    pub(super) fn scroll_by(&mut self, by: f32) {
        self.scroll_to(self.scroll + by);
    }

    pub(super) fn scroll_to(&mut self, to: f32) {
        let to = to.clamp(0.0, self.frame.scroll_range());
        if (to - self.scroll).abs() > f32::EPSILON {
            self.scroll = to;
            self.dirty = true;
            self.rehover();
        }
    }

    // ---- The keyboard -------------------------------------------------

    pub(super) fn end_capture(&mut self) -> Vec<Effect> {
        if self.capture.take().is_some() {
            self.dirty = true;
            vec![Effect::Send(Command::CapturingHotkey(false))]
        } else {
            Vec::new()
        }
    }

    pub(super) fn set_hotkey_text(&mut self, index: usize, new: String) {
        if let Some(Kind::Hotkey { text, .. }) = self
            .frame
            .controls
            .get_mut(index)
            .map(|control| &mut control.kind)
        {
            *text = new;
        }
    }

    /// A key went down while a hotkey field waits for one.
    pub(super) fn capture_key(&mut self, vk: u16, modifiers: Modifiers) -> Vec<Effect> {
        let Some(index) = self
            .capture
            .as_ref()
            .and_then(|capture| self.keyed(capture.key))
        else {
            return self.end_capture();
        };
        let Kind::Hotkey {
            text,
            clearable,
            action,
        } = &self.frame.controls[index].kind
        else {
            return self.end_capture();
        };
        let plain = modifiers.is_empty();
        if vk == vk::TAB && !(modifiers.ctrl || modifiers.alt || modifiers.win) {
            // Tab is the way out by keyboard, as from any other control.
            let mut effects = self.end_capture();
            effects.extend(self.step_focus(!modifiers.shift));
            return effects;
        }
        let new = if plain && vk == hotkey::VK_ESCAPE {
            None
        } else if plain && (vk == hotkey::VK_BACK || vk == hotkey::VK_DELETE) {
            clearable.then(String::new)
        } else if let Some(name) =
            hotkey::name(modifiers, vk).filter(|_| hotkey::capturable(modifiers, vk))
        {
            Some(name)
        } else {
            // A modifier alone, or a key that cannot be a hotkey as it is
            // held: what is held is shown, and the field waits on.
            if let Some(capture) = &mut self.capture {
                capture.held = modifiers.prefix();
            }
            self.dirty = true;
            return Vec::new();
        };
        let action = match &new {
            Some(new) if new != text => action(new.clone()),
            _ => Action::None,
        };
        let mut effects = self.end_capture();
        if let Some(new) = new {
            self.set_hotkey_text(index, new);
        }
        self.ring = true;
        effects.extend(self.run(action));
        effects
    }

    /// A key came up. Only a waiting hotkey field cares: it shows the
    /// modifiers that are still held.
    pub fn key_up(&mut self, modifiers: Modifiers) {
        if let Some(capture) = &mut self.capture {
            capture.held = modifiers.prefix();
            self.dirty = true;
        }
    }

    /// Whether a hotkey field is waiting for keys.
    #[must_use]
    pub fn capturing(&self) -> bool {
        self.capture.is_some()
    }

    /// The button Enter presses when nothing has the focus: the accent one
    /// at the bottom of a small window.
    pub(super) fn default_button(&self) -> Option<usize> {
        self.frame.controls.iter().rposition(|control| {
            control.enabled
                && control.region == Region::Fixed
                && matches!(
                    control.kind,
                    Kind::Button {
                        kind: ButtonKind::Accent,
                        ..
                    }
                )
        })
    }

    /// The way back, if the page has one.
    pub(super) fn back_button(&self) -> Option<usize> {
        self.frame.controls.iter().position(|control| {
            matches!(
                control.kind,
                Kind::Button {
                    icon: Some(Icon::Back),
                    ..
                }
            ) && control.region == Region::Fixed
        })
    }

    /// The control at `point`, the topmost if several are.
    #[must_use]
    pub fn control_at(&self, point: Point) -> Option<usize> {
        self.at(point)
    }

    /// Gives the control with this index the keyboard focus, as Tab does.
    pub fn focus_on(&mut self, index: usize) -> Vec<Effect> {
        match self.frame.controls.get(index) {
            Some(control) if control.enabled => self.set_focus(Some(index), true),
            _ => Vec::new(),
        }
    }

    /// Works the control with this index, as focusing it and pressing Enter
    /// does.
    pub fn press(&mut self, index: usize) -> Vec<Effect> {
        let mut effects = self.focus_on(index);
        effects.extend(self.activate(index, true));
        effects
    }

    /// Types `text` into the field with this index and hands it in, as
    /// typing it and pressing Enter does.
    pub fn write(&mut self, index: usize, text: String) -> Vec<Effect> {
        let is_field = matches!(
            self.frame.controls.get(index).map(|control| &control.kind),
            Some(Kind::Field { .. })
        );
        if !is_field {
            return Vec::new();
        }
        let mut effects = self.focus_on(index);
        effects.push(Effect::Retype(text.clone()));
        self.typed(text);
        effects.extend(self.commit());
        effects
    }

    /// Goes back, as the arrow beside the title does.
    pub fn back(&mut self) -> Vec<Effect> {
        match self.back_button() {
            Some(index) => self.activate(index, true),
            None => Vec::new(),
        }
    }

    /// A key went down. Returns whether it was used, and what follows.
    pub fn key_down(&mut self, key: u16, modifiers: Modifiers) -> (bool, Vec<Effect>) {
        if self.capture.is_some() {
            if modifiers.alt && key == vk::F4 {
                return (false, self.end_capture());
            }
            return (true, self.capture_key(key, modifiers));
        }
        let focused = self.focused();
        let kind = focused.map(|index| &self.frame.controls[index].kind);
        let page = self.frame.viewport.h - metric::ROW_MIN;
        let line = metric::ROW_MIN / 2.0;
        let effects = match key {
            vk::TAB if !modifiers.ctrl && !modifiers.alt => self.step_focus(!modifiers.shift),
            vk::ENTER | vk::SPACE if modifiers.is_empty() => {
                match focused.or_else(|| self.default_button().filter(|_| key == vk::ENTER)) {
                    Some(index) => {
                        self.ring = true;
                        self.activate(index, true)
                    }
                    None => return (false, Vec::new()),
                }
            }
            vk::ESCAPE => {
                if !matches!(self.showing.chrome(&self.view), Chrome::Compact) {
                    return (false, Vec::new());
                }
                vec![Effect::Dismiss]
            }
            vk::LEFT if modifiers.alt => self.back(),
            // The keys that open a list, besides Enter and Space.
            vk::F4 | vk::DOWN | vk::UP
                if matches!(kind, Some(Kind::Choice { .. }))
                    && (key == vk::F4) != modifiers.alt
                    && !(modifiers.ctrl || modifiers.shift || modifiers.win) =>
            {
                self.ring = true;
                self.activate(focused.unwrap_or(0), true)
            }
            vk::UP | vk::DOWN | vk::LEFT | vk::RIGHT if !modifiers.is_empty() => {
                return (false, Vec::new());
            }
            vk::UP | vk::DOWN if matches!(kind, Some(Kind::Nav { .. })) => {
                self.step_pane(focused.unwrap_or(0), key == vk::DOWN)
            }
            vk::UP | vk::DOWN if matches!(kind, Some(Kind::Choice { .. })) => {
                let index = focused.unwrap_or(0);
                let Some(Kind::Choice { selected, .. }) = kind else {
                    return (false, Vec::new());
                };
                let next = if key == vk::DOWN {
                    selected + 1
                } else {
                    selected.saturating_sub(1)
                };
                self.pick(index, next)
            }
            vk::UP => self.scrolled(-line),
            vk::DOWN => self.scrolled(line),
            vk::PAGE_UP => self.scrolled(-page),
            vk::PAGE_DOWN => self.scrolled(page),
            vk::HOME => self.scrolled(-f32::MAX),
            vk::END => self.scrolled(f32::MAX),
            _ => return (false, Vec::new()),
        };
        (true, effects)
    }

    pub(super) fn scrolled(&mut self, by: f32) -> Vec<Effect> {
        self.scroll_to((self.scroll + by).clamp(0.0, self.frame.scroll_range()));
        Vec::new()
    }
}
