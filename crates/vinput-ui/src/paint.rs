//! Painting a laid-out window.
//!
//! [`frame`] draws a [`Frame`] the way Windows 11 draws its own Settings:
//! given the frame, the theme and a [`Look`] (what the mouse is over, what
//! has the focus, how far the page is scrolled) it makes the same picture
//! every time, on a window or on a bitmap alike.

use crate::geometry::{Point, Rect};
use crate::gfx::{Align, Canvas, TextBox};
use crate::icons;
use crate::layout::{Control, Ink, Item, Kind, Measure, Region, SWITCH_WORDS};
use crate::look::{Look, ScrollBar, thumb};
use crate::node::{ButtonKind, TextStyle, Tone};
use crate::shell::Frame;
use crate::theme::{Color, Palette, Theme, metric, shade};

/// The words a hotkey field shows when it holds nothing.
const NOT_SET: &str = "Not set";
/// The words a hotkey field shows while it waits for keys.
const PRESS_KEYS: &str = "Press a key";
/// What stands in for each character of a secret.
const HIDDEN: char = '\u{25CF}';

/// What a control is going through.
#[derive(Debug, Clone, Copy, Default)]
struct State {
    hover: bool,
    pressed: bool,
    enabled: bool,
}

impl State {
    /// One of three colors: at rest, under the mouse, held down.
    fn pick(self, rest: Color, hover: Color, pressed: Color) -> Color {
        if self.pressed {
            pressed
        } else if self.hover {
            hover
        } else {
            rest
        }
    }
}

struct Painter<'a> {
    canvas: &'a Canvas,
    theme: &'a Theme,
    colors: &'a Palette,
    look: &'a Look<'a>,
    frame: &'a Frame,
}

/// Paints `frame` on `canvas`. The caller starts and finishes the drawing.
pub fn frame(canvas: &Canvas, frame: &Frame, theme: &Theme, look: &Look<'_>) {
    let painter = Painter {
        canvas,
        theme,
        colors: &theme.palette,
        look,
        frame,
    };
    painter.backdrop();
    for item in &frame.fixed {
        painter.item(item, 0.0);
    }
    canvas.clip(frame.viewport);
    for item in &frame.body {
        painter.item(item, -look.scroll);
    }
    canvas.unclip();
    painter.scroll_bar();
    painter.focus();
}

/// The color to wipe the canvas with before [`frame`].
#[must_use]
pub fn background(frame: &Frame, theme: &Theme, look: &Look<'_>) -> Color {
    if frame.pane.is_some() && look.mica {
        Color::CLEAR
    } else {
        theme.palette.window
    }
}

impl Painter<'_> {
    fn backdrop(&self) {
        let colors = self.colors;
        let sheet = self.frame.sheet;
        if self.frame.pane.is_some() {
            // The page lies on a sheet whose top left corner is rounded; it
            // reaches past the right and bottom edges so only that corner
            // shows.
            let radius = metric::RADIUS_FLYOUT;
            let grown = Rect::new(sheet.x, sheet.y, sheet.w + radius, sheet.h + radius);
            self.canvas.fill(grown, radius, colors.window);
            self.canvas
                .stroke(grown, radius, colors.card_stroke, metric::LINE);
        } else {
            let top = self.frame.strip.map_or(sheet.h, |strip| strip.y);
            self.canvas
                .fill(Rect::new(sheet.x, sheet.y, sheet.w, top), 0.0, colors.sheet);
        }
    }

    fn state(&self, index: usize, control: &Control) -> State {
        State {
            hover: control.enabled && self.look.hover == Some(index),
            pressed: control.enabled
                && (self.look.pressed == Some(index) || self.look.open == Some(index)),
            enabled: control.enabled,
        }
    }

    fn ink(&self, ink: Ink) -> Color {
        match ink {
            Ink::Tone(tone) => self.colors.tone(tone),
            Ink::Severity(severity) => self.colors.severity(severity).1,
        }
    }

    fn item(&self, item: &Item, dy: f32) {
        let colors = self.colors;
        let radius = metric::RADIUS;
        let seen = |rect: Rect| {
            let rect = rect.moved(0.0, dy);
            (dy == 0.0 || rect.touches(self.frame.viewport)).then_some(rect)
        };
        match item {
            Item::Card(rect) => {
                if let Some(rect) = seen(*rect) {
                    self.canvas.fill(rect, radius, colors.card);
                    self.canvas
                        .stroke(rect, radius, colors.card_stroke, metric::LINE);
                }
            }
            Item::Divider(rect) => {
                if let Some(rect) = seen(*rect) {
                    self.canvas.fill(rect, 0.0, colors.divider);
                }
            }
            Item::Text {
                rect,
                text,
                style,
                tone,
                trim,
            } => {
                if let Some(rect) = seen(*rect) {
                    let how = TextBox {
                        trim: *trim,
                        ..TextBox::default()
                    };
                    self.canvas
                        .text(text, rect, *style, colors.tone(*tone), how);
                }
            }
            Item::Glyph {
                rect,
                glyph,
                size,
                ink,
            } => {
                if let Some(rect) = seen(*rect) {
                    self.canvas.glyph(*glyph, rect, *size, self.ink(*ink));
                }
            }
            Item::Badge { rect, text, tone } => {
                if let Some(rect) = seen(*rect) {
                    let color = colors.tone(*tone);
                    self.canvas
                        .fill(rect, radius, color.faded(metric::BADGE_TINT));
                    if self.theme.contrast {
                        self.canvas.stroke(rect, radius, color, metric::LINE);
                    }
                    self.canvas
                        .text(text, rect, TextStyle::Caption, color, CENTERED);
                }
            }
            Item::Progress { rect, value, tone } => {
                if let Some(rect) = seen(*rect) {
                    self.progress(rect, *value, *tone);
                }
            }
            Item::Bar { rect, severity } => {
                if let Some(rect) = seen(*rect) {
                    self.canvas.fill(rect, radius, colors.severity(*severity).0);
                    self.canvas
                        .stroke(rect, radius, colors.card_stroke, metric::LINE);
                }
            }
            Item::Selected { rect, corners } => {
                if let Some(rect) = seen(*rect) {
                    self.canvas
                        .fill_corners(rect, radius, colors.subtle_hover, *corners);
                    self.pill(rect.x, rect.y + rect.h.min(metric::ROW_MIN) / 2.0);
                }
            }
            Item::Dot { rect, tone } => {
                if let Some(rect) = seen(*rect) {
                    self.canvas
                        .disc(rect.center(), rect.w / 2.0, colors.tone(*tone));
                }
            }
            Item::Logo(rect) => {
                if !self.canvas.logo(*rect) {
                    self.canvas.fill(*rect, radius, colors.accent);
                    self.canvas
                        .glyph(icons::LOGO, *rect, metric::ICON_SMALL, colors.on_accent);
                }
            }
            Item::Control(index) => {
                if let Some(control) = self.frame.controls.get(*index)
                    && let Some(rect) = seen(control.rect)
                {
                    self.control(*index, control, rect);
                }
            }
        }
    }

    /// The accent pill that marks what is selected, at the left edge `x`
    /// around the height `middle`.
    fn pill(&self, x: f32, middle: f32) {
        self.canvas.fill(
            Rect::new(
                x,
                middle - metric::PILL_H / 2.0,
                metric::PILL_W,
                metric::PILL_H,
            ),
            metric::PILL_W / 2.0,
            self.colors.accent,
        );
    }

    fn progress(&self, rect: Rect, value: Option<f32>, tone: Tone) {
        let colors = self.colors;
        let ink = match tone {
            Tone::Accent => colors.accent,
            other => colors.tone(other),
        };
        let track = rect.inset_xy(0.0, (rect.h - metric::LINE) / 2.0);
        self.canvas.fill(track, 0.0, colors.strong_stroke);
        let bar = rect.inset_xy(0.0, (rect.h - metric::PROGRESS_BAR) / 2.0);
        let radius = metric::PROGRESS_BAR / 2.0;
        if let Some(value) = value {
            let filled = Rect::new(bar.x, bar.y, bar.w * value, bar.h);
            if filled.w >= metric::PROGRESS_BAR {
                self.canvas.fill(filled, radius, ink);
            }
        } else {
            // A piece of bar that enters at the left and leaves at the
            // right, over and over.
            let length = bar.w * metric::PROGRESS_SWEEP;
            let x = bar.x - length + (bar.w + length) * self.look.phase;
            self.canvas.clip(bar);
            self.canvas
                .fill(Rect::new(x, bar.y, length, bar.h), radius, ink);
            self.canvas.unclip();
        }
    }

    fn control(&self, index: usize, control: &Control, rect: Rect) {
        let state = self.state(index, control);
        match &control.kind {
            Kind::Button {
                label, icon, kind, ..
            } => self.button(rect, label, icon.map(icons::glyph), *kind, state),
            Kind::Link { label, .. } => self.link(rect, label, state),
            Kind::Toggle { on, .. } => self.switch(rect, *on, state),
            Kind::Choice {
                items, selected, ..
            } => self.choice(rect, items.get(*selected).map_or("", String::as_str), state),
            Kind::Field {
                text,
                placeholder,
                secret,
                ..
            } => {
                let active = self.look.editing == Some(index);
                self.field_face(rect, state, active);
                if !active {
                    let shown: String = if *secret {
                        text.chars().map(|_| HIDDEN).collect()
                    } else {
                        text.clone()
                    };
                    self.field_text(rect, &shown, placeholder, state.enabled);
                }
            }
            Kind::Hotkey { text, .. } => self.hotkey(index, control, rect, text, state),
            Kind::Row { .. } => {
                let fill = state.pick(
                    Color::CLEAR,
                    self.colors.subtle_hover,
                    self.colors.subtle_pressed,
                );
                self.canvas
                    .fill_corners(rect, metric::RADIUS, fill, control.corners);
                if self.theme.contrast && state.hover {
                    self.canvas
                        .stroke(rect, metric::RADIUS, self.colors.accent, metric::FOCUS);
                }
            }
            Kind::Nav {
                label,
                icon,
                selected,
                ..
            } => self.nav(rect, label, icons::glyph(*icon), *selected, state),
        }
    }

    fn link(&self, rect: Rect, label: &str, state: State) {
        let colors = self.colors;
        let color = if state.enabled {
            state.pick(
                colors.accent_text,
                colors.accent_text,
                colors.accent_text.faded(shade::LINK_PRESSED),
            )
        } else {
            colors.text_disabled
        };
        let body = self.canvas.fonts().text("", TextStyle::Body, f32::MAX).h;
        let style = if rect.h < body {
            TextStyle::Caption
        } else {
            TextStyle::Body
        };
        let how = TextBox {
            middle: true,
            trim: true,
            underline: state.hover,
            ..TextBox::default()
        };
        self.canvas.text(label, rect, style, color, how);
    }

    fn choice(&self, rect: Rect, picked: &str, state: State) {
        self.button_face(rect, ButtonKind::Standard, state);
        let pad = metric::FIELD_PAD;
        let chevron = Rect::new(
            rect.right() - pad - metric::ICON_SMALL,
            rect.y,
            metric::ICON_SMALL,
            rect.h,
        );
        let text = Rect::new(
            rect.x + pad,
            rect.y,
            (chevron.x - metric::GAP - rect.x - pad).max(0.0),
            rect.h,
        );
        let color = self.button_text(ButtonKind::Standard, state);
        self.canvas.text(picked, text, TextStyle::Body, color, LINE);
        let quiet = if state.enabled {
            self.colors.text_secondary
        } else {
            self.colors.text_disabled
        };
        self.canvas
            .glyph(icons::CHEVRON_DOWN, chevron, metric::ICON_SMALL, quiet);
    }

    fn hotkey(&self, index: usize, control: &Control, rect: Rect, text: &str, state: State) {
        let waiting = self
            .look
            .capture
            .filter(|(capturing, _)| *capturing == index);
        self.field_face(rect, state, waiting.is_some());
        let clear = control
            .clear_rect()
            .filter(|_| waiting.is_none())
            .map(|clear| clear.moved(0.0, rect.y - control.rect.y));
        // The text stops short of the cross that clears the field.
        let words = clear.map_or(rect, |clear| {
            let room = (clear.x + metric::FIELD_PAD - rect.x).max(0.0);
            Rect::new(rect.x, rect.y, room, rect.h)
        });
        match waiting {
            Some((_, held)) => self.field_text(words, held, PRESS_KEYS, true),
            None => self.field_text(words, text, NOT_SET, state.enabled),
        }
        if let Some(clear) = clear {
            if state.hover && self.look.over_clear {
                self.canvas.fill(
                    clear.inset(metric::GRID),
                    metric::RADIUS,
                    self.colors.subtle_hover,
                );
            }
            self.canvas.glyph(
                icons::CLEAR,
                clear,
                metric::ICON_SMALL,
                self.colors.text_secondary,
            );
        }
    }

    fn button_text(&self, kind: ButtonKind, state: State) -> Color {
        let colors = self.colors;
        match (kind, state.enabled) {
            (ButtonKind::Accent, true) => state.pick(
                colors.on_accent,
                colors.on_accent,
                colors.on_accent.faded(shade::ON_ACCENT_PRESSED),
            ),
            (ButtonKind::Accent, false) => colors.on_accent_disabled,
            (_, true) => state.pick(colors.text, colors.text, colors.text_secondary),
            (_, false) => colors.text_disabled,
        }
    }

    /// The face of a button: its fill and the line around it.
    fn button_face(&self, rect: Rect, kind: ButtonKind, state: State) {
        let colors = self.colors;
        let radius = metric::RADIUS;
        match kind {
            ButtonKind::Accent => {
                let fill = if state.enabled {
                    state.pick(colors.accent, colors.accent_hover, colors.accent_pressed)
                } else {
                    colors.accent_disabled
                };
                self.canvas.fill(rect, radius, fill);
                if self.theme.contrast && !state.enabled {
                    self.canvas
                        .stroke(rect, radius, colors.text_disabled, metric::LINE);
                }
            }
            ButtonKind::Standard => {
                let fill = if state.enabled {
                    state.pick(colors.control, colors.control_hover, colors.control_pressed)
                } else {
                    colors.control_disabled
                };
                self.canvas.fill(rect, radius, fill);
                if state.enabled && !state.pressed && !self.theme.contrast {
                    self.canvas.stroke_lifted(
                        rect,
                        radius,
                        colors.control_stroke,
                        colors.control_edge,
                        self.theme.dark,
                    );
                } else {
                    self.canvas
                        .stroke(rect, radius, colors.control_stroke, metric::LINE);
                }
            }
            ButtonKind::Subtle => {
                let fill = state.pick(Color::CLEAR, colors.subtle_hover, colors.subtle_pressed);
                self.canvas.fill(rect, radius, fill);
            }
        }
        if self.theme.contrast && state.hover {
            self.canvas
                .stroke(rect, radius, colors.accent, metric::FOCUS);
        }
    }

    fn button(&self, rect: Rect, label: &str, glyph: Option<char>, kind: ButtonKind, state: State) {
        self.button_face(rect, kind, state);
        let color = self.button_text(kind, state);
        let Some(glyph) = glyph else {
            self.canvas
                .text(label, rect, TextStyle::Body, color, CENTERED);
            return;
        };
        if label.is_empty() {
            self.canvas.glyph(glyph, rect, metric::ICON, color);
            return;
        }
        // An icon and a caption sit together in the middle.
        let fonts = self.canvas.fonts();
        let words = fonts.text(label, TextStyle::Body, f32::MAX).w;
        let both = metric::ICON + metric::GAP + words;
        let x = rect.x + ((rect.w - both) / 2.0).max(metric::GRID);
        self.canvas.glyph(
            glyph,
            Rect::new(x, rect.y, metric::ICON, rect.h),
            metric::ICON,
            color,
        );
        let text_x = x + metric::ICON + metric::GAP;
        self.canvas.text(
            label,
            Rect::new(text_x, rect.y, (rect.right() - text_x).max(0.0), rect.h),
            TextStyle::Body,
            color,
            LINE,
        );
    }

    fn switch(&self, rect: Rect, on: bool, state: State) {
        let colors = self.colors;
        let track = Rect::new(
            rect.right() - metric::SWITCH_W,
            rect.y + (rect.h - metric::SWITCH_H) / 2.0,
            metric::SWITCH_W,
            metric::SWITCH_H,
        );
        let word = Rect::new(
            rect.x,
            rect.y,
            (track.x - metric::SWITCH_GAP - rect.x).max(0.0),
            rect.h,
        );
        let text = if state.enabled {
            colors.text
        } else {
            colors.text_disabled
        };
        let how = TextBox {
            align: Align::Right,
            ..LINE
        };
        self.canvas.text(
            SWITCH_WORDS[usize::from(on)],
            word,
            TextStyle::Body,
            text,
            how,
        );

        let radius = metric::SWITCH_H / 2.0;
        let knob = if on {
            let fill = if state.enabled {
                state.pick(colors.accent, colors.accent_hover, colors.accent_pressed)
            } else {
                colors.accent_disabled
            };
            self.canvas.fill(track, radius, fill);
            if state.enabled {
                colors.on_accent
            } else {
                colors.on_accent_disabled
            }
        } else {
            let fill = state.pick(
                colors.switch_off,
                colors.subtle_hover,
                colors.subtle_pressed,
            );
            let line = if state.enabled {
                colors.strong_stroke
            } else {
                colors.text_disabled
            };
            self.canvas.fill(track, radius, fill);
            self.canvas.stroke(track, radius, line, metric::LINE);
            if state.enabled {
                colors.knob
            } else {
                colors.text_disabled
            }
        };
        if self.theme.contrast && on {
            let line = if state.enabled {
                colors.on_accent
            } else {
                colors.text_disabled
            };
            self.canvas.stroke(track, radius, line, metric::LINE);
        }
        let size = if state.hover || state.pressed {
            metric::KNOB_HOVER
        } else {
            metric::KNOB
        };
        let x = if on {
            track.right() - radius
        } else {
            track.x + radius
        };
        self.canvas
            .disc(Point::new(x, track.center().y), size / 2.0, knob);
    }

    /// The face of a text field: its fill, the line around it and the
    /// stronger line under it, which is the accent color while it is typed
    /// in.
    fn field_face(&self, rect: Rect, state: State, active: bool) {
        let colors = self.colors;
        let radius = metric::RADIUS;
        let fill = if active {
            colors.field_active
        } else if state.enabled {
            state.pick(colors.control, colors.control_hover, colors.control_hover)
        } else {
            colors.control_disabled
        };
        self.canvas.fill(rect, radius, fill);
        self.canvas
            .stroke(rect, radius, colors.control_stroke, metric::LINE);
        let (edge, thickness) = if active {
            (colors.accent, metric::FIELD_FOCUS)
        } else if state.enabled {
            (colors.field_edge, metric::LINE)
        } else {
            (colors.control_stroke, metric::LINE)
        };
        // The line under the field follows the rounded corners: it is the
        // bottom slice of a rounded shape of the field's size.
        self.canvas.clip(Rect::new(
            rect.x,
            rect.bottom() - thickness,
            rect.w,
            thickness,
        ));
        self.canvas.fill(rect, radius, edge);
        self.canvas.unclip();
        if self.theme.contrast && (state.hover || active) {
            self.canvas
                .stroke(rect, radius, colors.accent, metric::FOCUS);
        }
    }

    fn field_text(&self, rect: Rect, text: &str, placeholder: &str, enabled: bool) {
        let colors = self.colors;
        let inner = rect.inset_xy(metric::FIELD_PAD, 0.0);
        let (shown, color) = match (text.is_empty(), enabled) {
            (true, true) => (placeholder, colors.text_secondary),
            (true, false) => (placeholder, colors.text_disabled),
            (false, true) => (text, colors.text),
            (false, false) => (text, colors.text_disabled),
        };
        self.canvas.text(shown, inner, TextStyle::Body, color, LINE);
    }

    fn nav(&self, rect: Rect, label: &str, glyph: char, selected: bool, state: State) {
        let colors = self.colors;
        let fill = if selected {
            state.pick(
                colors.subtle_hover,
                colors.subtle_pressed,
                colors.subtle_pressed,
            )
        } else {
            state.pick(Color::CLEAR, colors.subtle_hover, colors.subtle_pressed)
        };
        self.canvas.fill(rect, metric::RADIUS, fill);
        if selected {
            self.pill(rect.x, rect.center().y);
            if self.theme.contrast {
                self.canvas
                    .stroke(rect, metric::RADIUS, colors.accent, metric::FOCUS);
            }
        }
        self.canvas.glyph(
            glyph,
            Rect::new(rect.x + metric::NAV_ICON_X, rect.y, metric::ICON, rect.h),
            metric::ICON,
            colors.text,
        );
        let text_x = rect.x + metric::NAV_TEXT_X;
        self.canvas.text(
            label,
            Rect::new(text_x, rect.y, (rect.right() - text_x).max(0.0), rect.h),
            TextStyle::Body,
            colors.text,
            LINE,
        );
    }

    fn scroll_bar(&self) {
        let wide = match self.look.bar {
            ScrollBar::Hidden => return,
            ScrollBar::Thin => false,
            ScrollBar::Wide => true,
        };
        if let Some(thumb) = thumb(self.frame, self.look.scroll, wide) {
            self.canvas
                .fill(thumb, thumb.w / 2.0, self.colors.scroll_thumb);
        }
    }

    /// The two rings around what has the keyboard focus.
    fn focus(&self) {
        if !self.look.ring {
            return;
        }
        let Some(index) = self.look.focus else {
            return;
        };
        let Some(control) = self.frame.controls.get(index) else {
            return;
        };
        if self.look.editing == Some(index) {
            return;
        }
        let body = control.region == Region::Body;
        let rect = if body {
            control.rect.moved(0.0, -self.look.scroll)
        } else {
            control.rect
        };
        // Rings go around a control, but inside a row or a pane entry, whose
        // neighbors leave no room outside.
        let inside = matches!(control.kind, Kind::Row { .. } | Kind::Nav { .. });
        let gap = if inside {
            metric::FOCUS
        } else {
            -metric::FOCUS_GAP - metric::FOCUS
        };
        let outer = rect.inset(gap);
        let radius = (metric::RADIUS - gap).max(metric::RADIUS / 2.0);
        if body {
            self.canvas.clip(self.frame.viewport);
        }
        self.canvas
            .stroke(outer, radius, self.colors.focus_outer, metric::FOCUS);
        self.canvas.stroke(
            outer.inset(metric::FOCUS),
            (radius - metric::FOCUS).max(0.0),
            self.colors.focus_inner,
            metric::LINE,
        );
        if body {
            self.canvas.unclip();
        }
    }
}

/// One line in the middle of its rectangle, cut off if it is too long.
const LINE: TextBox = TextBox {
    align: Align::Left,
    middle: true,
    trim: true,
    underline: false,
};

/// The same, centered sideways as well.
const CENTERED: TextBox = TextBox {
    align: Align::Center,
    ..LINE
};
