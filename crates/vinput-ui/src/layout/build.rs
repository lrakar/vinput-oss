//! The builder that turns nodes into items and controls.

use vinput_view::{Command, Setup};

use super::{Control, Corners, Ink, Item, Kind, Measure, Region};
use crate::geometry::{Rect, Size};
use crate::icons;
use crate::node::{
    Action, Button, ButtonKind, Field, Icon, Key, Node, Setting, Severity, TextStyle, Tone,
};
use crate::theme::{face, metric};

/// The two words a switch says.
pub const SWITCH_WORDS: [&str; 2] = ["Off", "On"];
/// The bullet before a point of a list.
pub(super) const BULLET: &str = "\u{2022}";
/// The room a bullet takes, with the gap after it.
const BULLET_ROOM: f32 = metric::ROW_PAD;
/// The gap between the points of a list.
pub(super) const LIST_GAP: f32 = metric::GRID;
/// The gap between a title and its line of detail.
const DETAIL_GAP: f32 = 0.0;

/// The glyph at the right edge of a row that goes somewhere: a chevron for
/// another page or the next step of a setup, the open-elsewhere arrow for
/// the browser.
fn trailing(action: &Action) -> Option<char> {
    match action {
        Action::Go(_) | Action::Send(Command::Google(Setup::Open(_))) => {
            Some(icons::glyph(Icon::ChevronRight))
        }
        Action::Send(Command::OpenLink(_)) => Some(icons::glyph(Icon::OpenExternal)),
        Action::Both(first, second) => trailing(second).or_else(|| trailing(first)),
        _ => None,
    }
}

/// Where the parts of a settings row go, worked out once for measuring and
/// for placing.
struct RowPlan {
    /// Left edge of the title, from the row's left edge.
    text_x: f32,
    /// Width of the title and the detail.
    text_w: f32,
    /// Height of the title.
    title_h: f32,
    /// Height of the detail, zero without one.
    detail_h: f32,
    /// Size of the control at the right edge, zero without one.
    control: Size,
    /// Height of the part with the title and the control.
    main_h: f32,
    /// Width of what is under the row.
    below_w: f32,
    /// Height of what is under the row, with the room around it.
    below_h: f32,
}

/// Lays nodes out into items and controls.
pub struct Builder<'a> {
    measure: &'a dyn Measure,
    fixed: Vec<Item>,
    body: Vec<Item>,
    controls: Vec<Control>,
    region: Region,
    label: String,
    footer: bool,
    /// How much larger than usual text is, going by what `measure` says of
    /// a line of body text: what the user asked of Windows.
    text_scale: f32,
    /// The height of a button, a list or a field: the usual, or more where
    /// larger text needs it.
    control: f32,
    /// The height of a badge, likewise.
    badge: f32,
}

/// What a [`Builder`] made.
pub struct Laid {
    /// What is painted in the parts that stay put.
    pub fixed: Vec<Item>,
    /// What is painted in the part that scrolls.
    pub body: Vec<Item>,
    /// Everything that can be worked, in visual order.
    pub controls: Vec<Control>,
}

impl<'a> Builder<'a> {
    /// A builder that measures text with `measure`.
    #[must_use]
    pub fn new(measure: &'a dyn Measure) -> Self {
        let body = face(TextStyle::Body).line;
        let caption = face(TextStyle::Caption).line;
        let grown = |style: TextStyle, usual: f32, line: f32| {
            usual.max(measure.text("", style, f32::MAX).h + usual - line)
        };
        let control = grown(TextStyle::Body, metric::CONTROL, body);
        Self {
            text_scale: (measure.text("", TextStyle::Body, f32::MAX).h / body).max(1.0),
            control,
            badge: grown(TextStyle::Caption, metric::BADGE_H, caption),
            measure,
            fixed: Vec::new(),
            body: Vec::new(),
            controls: Vec::new(),
            region: Region::Fixed,
            label: String::new(),
            footer: false,
        }
    }

    /// Says which part of the window what follows is in.
    pub fn region(&mut self, region: Region) {
        self.region = region;
    }

    /// What was laid out.
    #[must_use]
    pub fn finish(self) -> Laid {
        Laid {
            fixed: self.fixed,
            body: self.body,
            controls: self.controls,
        }
    }

    /// Adds something that is only painted.
    pub fn item(&mut self, item: Item) {
        match self.region {
            Region::Fixed => self.fixed.push(item),
            Region::Body => self.body.push(item),
        }
    }

    /// Adds a control and paints it here.
    pub fn control(&mut self, rect: Rect, kind: Kind, enabled: bool, key: Option<Key>) -> usize {
        self.control_with(rect, kind, enabled, key, Corners::ALL)
    }

    fn control_with(
        &mut self,
        rect: Rect,
        kind: Kind,
        enabled: bool,
        key: Option<Key>,
        corners: Corners,
    ) -> usize {
        let index = self.controls.len();
        self.controls.push(Control {
            rect,
            kind,
            enabled,
            key,
            region: self.region,
            label: self.label.clone(),
            corners,
        });
        self.item(Item::Control(index));
        index
    }

    /// Adds a line of text that is cut off rather than wrapped.
    pub fn line(&mut self, rect: Rect, text: impl Into<String>, style: TextStyle, tone: Tone) {
        self.item(Item::Text {
            rect,
            text: text.into(),
            style,
            tone,
            trim: true,
        });
    }

    /// The height of a button, a list or a field with text as large as the
    /// user asked for.
    #[must_use]
    pub fn control_height(&self) -> f32 {
        self.control
    }

    /// The height of one line of `style`.
    #[must_use]
    pub fn line_height(&self, style: TextStyle) -> f32 {
        self.measure.text("", style, f32::MAX).h
    }

    /// The room `text` takes in `style` at up to `max_width`.
    #[must_use]
    pub fn text_size(&self, text: &str, style: TextStyle, max_width: f32) -> Size {
        self.measure.text(text, style, max_width.max(1.0))
    }

    /// Lays `nodes` out one under another from `x`, `y` at width `w`.
    /// Returns the height they took.
    pub fn column(&mut self, nodes: Vec<Node>, x: f32, y: f32, w: f32) -> f32 {
        let mut used = 0.0;
        for (index, node) in nodes.into_iter().enumerate() {
            used += Self::gap_before(&node, index);
            used += self.place(node, x, y + used, w);
        }
        used
    }

    /// Lays the buttons of a footer out in `rect`: at the right edge, the
    /// accent one first, as Windows orders them. Anything that is not a
    /// plain list of buttons is laid out as a row instead.
    pub fn footer(&mut self, mut nodes: Vec<Node>, rect: Rect) {
        self.footer = true;
        let plain = nodes.iter().all(|node| matches!(node, Node::Button(_)));
        if plain {
            nodes.sort_by_key(
                |node| !matches!(node, Node::Button(button) if button.kind == ButtonKind::Accent),
            );
            nodes.insert(0, Node::Spring);
        } else if !nodes.iter().any(|node| matches!(node, Node::Spring)) {
            nodes.insert(0, Node::Spring);
        }
        let row = Node::Row(nodes);
        let height = self.size(&row, rect.w).h;
        self.place(row, rect.x, rect.y + (rect.h - height) / 2.0, rect.w);
        self.footer = false;
    }

    /// The height a footer with these nodes needs at width `w`.
    #[must_use]
    pub fn footer_height(&self, nodes: &[Node], w: f32) -> f32 {
        nodes
            .iter()
            .map(|node| self.size(node, w).h)
            .fold(0.0, f32::max)
    }

    /// The height `nodes` take one under another at width `w`.
    #[must_use]
    pub fn column_height(&self, nodes: &[Node], w: f32) -> f32 {
        nodes
            .iter()
            .enumerate()
            .map(|(index, node)| Self::gap_before(node, index) + self.size(node, w).h)
            .sum()
    }

    fn gap_before(node: &Node, index: usize) -> f32 {
        match (index, node) {
            (0, _) => 0.0,
            (_, Node::Header(_)) => metric::GAP + metric::HEADER_ABOVE,
            _ => metric::GAP,
        }
    }

    fn button_size(&self, button: &Button) -> Size {
        if button.label.is_empty() {
            return Size::new(self.control, self.control);
        }
        let label = self.text_size(&button.label, TextStyle::Body, f32::MAX).w;
        let icon = button.icon.map_or(0.0, |_| metric::ICON + metric::GAP);
        let least = if self.footer {
            metric::BUTTON_MIN_FOOTER
        } else {
            metric::BUTTON_MIN
        };
        Size::new(
            (label + icon + 2.0 * metric::BUTTON_PAD).max(least),
            self.control,
        )
    }

    fn switch_size(&self) -> Size {
        let words = SWITCH_WORDS
            .iter()
            .map(|word| self.text_size(word, TextStyle::Body, f32::MAX).w)
            .fold(0.0, f32::max);
        Size::new(words + metric::SWITCH_GAP + metric::SWITCH_W, self.control)
    }

    fn choice_width(&self, items: &[String]) -> f32 {
        let widest = items
            .iter()
            .map(|item| self.text_size(item, TextStyle::Body, f32::MAX).w)
            .fold(0.0, f32::max);
        (widest + 2.0 * metric::FIELD_PAD + metric::ICON_SMALL + metric::GAP)
            .clamp(metric::FIELD_MIN, metric::CHOICE_MAX * self.text_scale)
    }

    /// Whether a node in a row grows into the room that is left over.
    fn grows(node: &Node) -> bool {
        match node {
            Node::Spring | Node::Progress(_) | Node::Gauge { .. } => true,
            Node::Field(field) => field.wide,
            _ => false,
        }
    }

    /// Whether a node in a row that is too full gives way.
    fn shrinks(node: &Node) -> bool {
        matches!(
            node,
            Node::Text { .. } | Node::Column(_) | Node::List(_) | Node::Choice { .. }
        ) || matches!(node, Node::Field(field) if !field.wide)
    }

    /// The width a node would like in a row.
    fn wish(&self, node: &Node, avail: f32) -> f32 {
        match node {
            Node::Spring => 0.0,
            Node::Column(children) => children
                .iter()
                .map(|child| self.wish(child, avail))
                .fold(0.0, f32::max),
            Node::Row(children) => {
                let gaps = gaps(children.len());
                children
                    .iter()
                    .map(|child| self.wish(child, avail))
                    .sum::<f32>()
                    + gaps
            }
            Node::Header(text) => self.text_size(text, TextStyle::BodyStrong, f32::MAX).w,
            Node::Text { text, style, .. } => self.text_size(text, *style, f32::MAX).w,
            Node::List(points) => points
                .iter()
                .map(|point| BULLET_ROOM + self.text_size(point, TextStyle::Body, f32::MAX).w)
                .fold(0.0, f32::max),
            Node::Progress(_) | Node::Gauge { .. } => metric::PROGRESS_MIN,
            Node::Field(field) if field.wide => metric::FIELD_MIN,
            Node::Card(_) | Node::Setting(_) | Node::InfoBar { .. } | Node::Busy(_) => avail,
            other => self.size(other, avail).w,
        }
        .min(avail)
    }

    /// The widths the children of a row get at width `w`.
    fn row_widths(&self, children: &[Node], w: f32) -> Vec<f32> {
        let room = (w - gaps(children.len())).max(0.0);
        let mut widths: Vec<f32> = children
            .iter()
            .map(|child| self.wish(child, room))
            .collect();
        let wanted: f32 = widths.iter().sum();
        if wanted > room {
            let soft: f32 = children
                .iter()
                .zip(&widths)
                .filter(|(child, _)| Self::shrinks(child))
                .map(|(_, width)| *width)
                .sum();
            if soft > 0.0 {
                let keep = ((soft - (wanted - room)) / soft).max(0.0);
                for (child, width) in children.iter().zip(&mut widths) {
                    if Self::shrinks(child) {
                        *width = (*width * keep).max(metric::TEXT_MIN.min(*width));
                    }
                }
            }
        } else {
            // Springs take what is left before anything else does.
            let springs = children
                .iter()
                .filter(|child| matches!(child, Node::Spring))
                .count();
            let growers = children.iter().filter(|child| Self::grows(child)).count();
            let (count, only_springs) = if springs > 0 {
                (springs, true)
            } else {
                (growers, false)
            };
            if count > 0 {
                let share = (room - wanted) / count_f32(count);
                for (child, width) in children.iter().zip(&mut widths) {
                    let takes = if only_springs {
                        matches!(child, Node::Spring)
                    } else {
                        Self::grows(child)
                    };
                    if takes {
                        *width += share;
                    }
                }
            }
        }
        widths
    }

    fn plan(&self, setting: &Setting, w: f32) -> RowPlan {
        let pad = metric::ROW_PAD;
        let led = setting.icon.is_some() || setting.number.is_some();
        let text_x = if led {
            pad + metric::ICON_ROW + pad
        } else {
            pad
        };
        let tail = trailing(&setting.action).map_or(0.0, |_| metric::ICON_SMALL + pad);
        let room = (w - text_x - pad - tail).max(0.0);
        let control = setting.control.as_deref().map_or(Size::default(), |node| {
            // Larger text needs more room before the control may take it.
            let least = metric::ROW_TEXT_MIN * self.text_scale;
            let most = (room - least - pad).max(self.control);
            let width = self.wish(node, most);
            let size = self.size(node, width);
            Size::new(size.w.min(most), size.h)
        });
        let text_w = if control.w > 0.0 {
            (room - control.w - pad).max(metric::TEXT_MIN)
        } else {
            room
        };
        let title_h = self.text_size(&setting.title, TextStyle::Body, text_w).h;
        let detail_h = if setting.detail.is_empty() {
            0.0
        } else {
            DETAIL_GAP
                + self
                    .text_size(&setting.detail, TextStyle::Caption, text_w)
                    .h
        };
        let main_h = (title_h + detail_h + 2.0 * metric::ROW_PAD_Y)
            .max(control.h + 2.0 * metric::GAP)
            .max(metric::ROW_MIN);
        let below_w = (w - text_x - pad).max(0.0);
        let below_h = if setting.below.is_empty() {
            0.0
        } else {
            self.column_height(&setting.below, below_w) + pad
        };
        RowPlan {
            text_x,
            text_w,
            title_h,
            detail_h,
            control,
            main_h,
            below_w,
            below_h,
        }
    }

    fn bar_parts(
        &self,
        text: &str,
        action: Option<&(String, Action)>,
        dismiss: bool,
        w: f32,
    ) -> (f32, f32, f32) {
        let button = action.map_or(0.0, |(label, _)| {
            let label = self.text_size(label, TextStyle::Body, f32::MAX).w;
            (label + 2.0 * metric::BUTTON_PAD).max(metric::BUTTON_MIN)
        });
        let mut right = if dismiss {
            metric::GAP + self.control + metric::GRID
        } else {
            metric::ROW_PAD
        };
        if button > 0.0 {
            right += button + metric::BAR_ICON_GAP;
        }
        let left = metric::ROW_PAD + metric::ICON + metric::BAR_ICON_GAP;
        let text_w = (w - left - right).max(metric::TEXT_MIN);
        let text_h = self.text_size(text, TextStyle::Body, text_w).h;
        let height = (text_h + 2.0 * metric::BAR_PAD_Y).max(metric::BAR_MIN);
        (button, text_w, height)
    }

    /// The room `node` takes when it is offered the width `w`.
    #[must_use]
    pub fn size(&self, node: &Node, w: f32) -> Size {
        match node {
            Node::Column(children) => Size::new(w, self.column_height(children, w)),
            Node::Row(children) => {
                let widths = self.row_widths(children, w);
                let height = children
                    .iter()
                    .zip(&widths)
                    .map(|(child, width)| self.size(child, *width).h)
                    .fold(0.0, f32::max);
                Size::new(w, height)
            }
            Node::Spring => Size::default(),
            Node::Header(text) => Size::new(w, self.text_size(text, TextStyle::BodyStrong, w).h),
            Node::Text { text, style, .. } => Size::new(w, self.text_size(text, *style, w).h),
            Node::List(points) => {
                let room = (w - BULLET_ROOM).max(metric::TEXT_MIN);
                let lines: f32 = points
                    .iter()
                    .map(|point| self.text_size(point, TextStyle::Body, room).h)
                    .sum();
                Size::new(
                    w,
                    lines + LIST_GAP * count_f32(points.len().saturating_sub(1)),
                )
            }
            Node::Icon(..) => Size::new(metric::ICON_ROW, metric::ICON_ROW),
            Node::Card(children) => {
                let rows: f32 = children.iter().map(|child| self.card_child(child, w)).sum();
                let lines = metric::LINE * count_f32(children.len().saturating_sub(1));
                Size::new(w, rows + lines)
            }
            Node::Setting(setting) => {
                let plan = self.plan(setting, w);
                Size::new(w, plan.main_h + plan.below_h)
            }
            Node::Button(button) => self.button_size(button),
            Node::Link { label, .. } => {
                let size = self.text_size(label, TextStyle::Body, f32::MAX);
                Size::new(size.w.min(w), size.h)
            }
            Node::Toggle { .. } => self.switch_size(),
            Node::Choice { items, .. } => Size::new(
                self.choice_width(items).min(w.max(metric::TEXT_MIN)),
                self.control,
            ),
            Node::Field(field) => {
                let width = if field.wide {
                    w
                } else {
                    (metric::FIELD * self.text_scale).min(w.max(metric::TEXT_MIN))
                };
                Size::new(width, self.control)
            }
            // As wide as a combination needs, less where the row is short of room.
            Node::Hotkey { .. } => Size::new(
                (metric::HOTKEY * self.text_scale).min(w.max(metric::TEXT_MIN)),
                self.control,
            ),
            Node::Progress(_) | Node::Gauge { .. } => Size::new(w, metric::PROGRESS_H),
            Node::Badge { text, .. } => Size::new(
                self.text_size(text, TextStyle::Caption, f32::MAX).w + 2.0 * metric::BADGE_PAD,
                self.badge,
            ),
            Node::InfoBar {
                text,
                action,
                dismiss,
                ..
            } => {
                let (_, _, height) = self.bar_parts(text, action.as_ref(), dismiss.is_some(), w);
                Size::new(w, height)
            }
            Node::Busy(text) => Size::new(w, self.bar_parts(text, None, false, w).2),
        }
    }

    /// The height a child of a card takes at the card's width.
    fn card_child(&self, child: &Node, w: f32) -> f32 {
        match child {
            Node::Setting(_) => self.size(child, w).h,
            other => {
                self.size(other, (w - 2.0 * metric::ROW_PAD).max(0.0)).h + 2.0 * metric::ROW_PAD
            }
        }
    }

    /// Places `node` with its top left corner at `x`, `y`, offered the width
    /// `w`. Returns the height it took.
    pub fn place(&mut self, node: Node, x: f32, y: f32, w: f32) -> f32 {
        let size = self.size(&node, w);
        let rect = Rect::new(x, y, size.w, size.h);
        match node {
            Node::Column(children) => {
                self.column(children, x, y, w);
            }
            Node::Row(children) => self.row(children, rect),
            Node::Spring => {}
            Node::Header(text) => self.item(Item::Text {
                rect,
                text,
                style: TextStyle::BodyStrong,
                tone: Tone::Primary,
                trim: false,
            }),
            Node::Text { text, style, tone } => self.item(Item::Text {
                rect,
                text,
                style,
                tone,
                trim: false,
            }),
            Node::List(points) => self.list(points, rect),
            Node::Icon(icon, tone) => self.item(Item::Glyph {
                rect,
                glyph: icons::glyph(icon),
                size: metric::ICON,
                ink: Ink::Tone(tone),
            }),
            Node::Card(children) => self.card(children, rect),
            Node::Setting(setting) => {
                self.item(Item::Card(rect));
                self.setting(setting, rect, Corners::ALL);
            }
            Node::Progress(value) => self.item(Item::Progress {
                rect,
                value: value.map(|value| value.clamp(0.0, 1.0)),
                tone: Tone::Accent,
            }),
            Node::Gauge { value, tone } => self.item(Item::Progress {
                rect,
                value: Some(value.clamp(0.0, 1.0)),
                tone,
            }),
            Node::Busy(text) => {
                self.bar(Severity::Info, text, None, None, rect);
                // Along the bottom edge, inside the rounded corners.
                let inset = metric::ROW_PAD;
                self.item(Item::Progress {
                    rect: Rect::new(
                        rect.x + inset,
                        rect.bottom() - metric::PROGRESS_H,
                        (rect.w - 2.0 * inset).max(0.0),
                        metric::PROGRESS_H,
                    ),
                    value: None,
                    tone: Tone::Accent,
                });
            }
            Node::Badge { text, tone } => self.item(Item::Badge { rect, text, tone }),
            Node::InfoBar {
                severity,
                text,
                action,
                dismiss,
            } => self.bar(severity, text, action, dismiss, rect),
            control => self.leaf(control, rect),
        }
        size.h
    }

    /// Places a node that is one control.
    fn leaf(&mut self, node: Node, rect: Rect) {
        match node {
            Node::Button(button) => {
                self.control(
                    rect,
                    Kind::Button {
                        label: button.label,
                        icon: button.icon,
                        kind: button.kind,
                        action: button.action,
                        hint: button.hint,
                    },
                    button.enabled,
                    None,
                );
            }
            Node::Link { label, action } => {
                self.control(rect, Kind::Link { label, action }, true, None);
            }
            Node::Toggle {
                on,
                enabled,
                action,
            } => {
                self.control(rect, Kind::Toggle { on, action }, enabled, None);
            }
            Node::Choice {
                key,
                items,
                selected,
                action,
            } => {
                let enabled = !items.is_empty();
                self.control(
                    rect,
                    Kind::Choice {
                        items,
                        selected,
                        action,
                    },
                    enabled,
                    Some(key),
                );
            }
            Node::Field(field) => self.field(field, rect),
            Node::Hotkey {
                key,
                text,
                clearable,
                action,
            } => {
                self.control(
                    rect,
                    Kind::Hotkey {
                        text,
                        clearable,
                        action,
                    },
                    true,
                    Some(key),
                );
            }
            _ => {}
        }
    }

    fn field(&mut self, field: Field, rect: Rect) {
        self.control(
            rect,
            Kind::Field {
                text: field.text,
                placeholder: field.placeholder,
                secret: field.secret,
                eager: field.eager,
                action: field.action,
                on_enter: field.on_enter,
            },
            field.enabled,
            Some(field.key),
        );
    }

    fn row(&mut self, children: Vec<Node>, rect: Rect) {
        let widths = self.row_widths(&children, rect.w);
        let mut x = rect.x;
        for (child, width) in children.into_iter().zip(widths) {
            let size = self.size(&child, width);
            // A child that does not fill its share sits at the right of it
            // only if a spring pushed it there, which the widths already
            // say; within its share it starts at the left.
            self.place(child, x, rect.y + (rect.h - size.h) / 2.0, width);
            x += width + metric::GAP;
        }
    }

    fn list(&mut self, points: Vec<String>, rect: Rect) {
        let room = (rect.w - BULLET_ROOM).max(metric::TEXT_MIN);
        let mut y = rect.y;
        for point in points {
            let height = self.text_size(&point, TextStyle::Body, room).h;
            self.item(Item::Text {
                rect: Rect::new(rect.x, y, BULLET_ROOM, height),
                text: BULLET.to_owned(),
                style: TextStyle::Body,
                tone: Tone::Secondary,
                trim: false,
            });
            self.item(Item::Text {
                rect: Rect::new(rect.x + BULLET_ROOM, y, room, height),
                text: point,
                style: TextStyle::Body,
                tone: Tone::Primary,
                trim: false,
            });
            y += height + LIST_GAP;
        }
    }

    fn card(&mut self, children: Vec<Node>, rect: Rect) {
        self.item(Item::Card(rect));
        let last = children.len().saturating_sub(1);
        let mut y = rect.y;
        for (index, child) in children.into_iter().enumerate() {
            if index > 0 {
                self.item(Item::Divider(Rect::new(rect.x, y, rect.w, metric::LINE)));
                y += metric::LINE;
            }
            let height = self.card_child(&child, rect.w);
            let corners = Corners {
                top: index == 0,
                bottom: index == last,
            };
            match child {
                Node::Setting(setting) => {
                    self.setting(setting, Rect::new(rect.x, y, rect.w, height), corners);
                }
                other => {
                    let pad = metric::ROW_PAD;
                    self.place(other, rect.x + pad, y + pad, (rect.w - 2.0 * pad).max(0.0));
                }
            }
            y += height;
        }
    }

    fn setting(&mut self, setting: Setting, rect: Rect, corners: Corners) {
        let plan = self.plan(&setting, rect.w);
        let pad = metric::ROW_PAD;
        let main = Rect::new(rect.x, rect.y, rect.w, plan.main_h);
        if setting.selected {
            self.item(Item::Selected { rect, corners });
        }
        let tail = trailing(&setting.action);
        self.label.clone_from(&setting.title);
        if setting.enabled && !setting.action.is_none() {
            self.control_with(
                rect,
                Kind::Row {
                    action: setting.action,
                },
                true,
                None,
                corners,
            );
        }
        let quiet = |tone: Tone| {
            if setting.enabled {
                tone
            } else {
                Tone::Disabled
            }
        };
        let box_size = Size::new(metric::ICON_ROW, metric::ICON_ROW);
        let lead = Rect::new(main.x + pad, main.y, 0.0, main.h).left_center(box_size);
        if let Some(icon) = setting.icon {
            self.item(Item::Glyph {
                rect: lead,
                glyph: icons::glyph(icon),
                size: metric::ICON_ROW,
                ink: Ink::Tone(quiet(setting.icon_tone)),
            });
        } else if let Some(number) = setting.number.filter(|number| *number > 0) {
            // In the middle of where an icon would be.
            let digits = number.to_string();
            let size = self.text_size(&digits, TextStyle::BodyStrong, f32::MAX);
            self.item(Item::Text {
                rect: Rect::new(
                    lead.x + (lead.w - size.w) / 2.0,
                    lead.y + (lead.h - size.h) / 2.0,
                    size.w,
                    size.h,
                ),
                text: digits,
                style: TextStyle::BodyStrong,
                tone: quiet(Tone::Secondary),
                trim: false,
            });
        }
        let text_h = plan.title_h + plan.detail_h;
        let text_y = main.y + (main.h - text_h) / 2.0;
        let text_x = rect.x + plan.text_x;
        self.item(Item::Text {
            rect: Rect::new(text_x, text_y, plan.text_w, plan.title_h),
            text: setting.title,
            style: TextStyle::Body,
            tone: quiet(Tone::Primary),
            trim: false,
        });
        if !setting.detail.is_empty() {
            self.item(Item::Text {
                rect: Rect::new(
                    text_x,
                    text_y + plan.title_h + DETAIL_GAP,
                    plan.text_w,
                    plan.detail_h - DETAIL_GAP,
                ),
                text: setting.detail,
                style: TextStyle::Caption,
                tone: quiet(setting.detail_tone),
                trim: false,
            });
        }
        let mut right = rect.right() - pad;
        if let Some(glyph) = tail {
            let size = Size::new(metric::ICON_SMALL, metric::ICON_SMALL);
            right -= metric::ICON_SMALL;
            self.item(Item::Glyph {
                rect: Rect::new(right, main.y, 0.0, main.h).left_center(size),
                glyph,
                size: metric::ICON_SMALL,
                ink: Ink::Tone(quiet(Tone::Secondary)),
            });
            right -= pad;
        }
        if let Some(control) = setting.control {
            let x = right - plan.control.w;
            let y = main.y + (main.h - plan.control.h) / 2.0;
            self.place(*control, x, y, plan.control.w);
        }
        if !setting.below.is_empty() {
            // The row above already left room under its text.
            self.column(setting.below, text_x, main.bottom(), plan.below_w);
        }
        self.label.clear();
    }

    fn bar(
        &mut self,
        severity: Severity,
        text: String,
        action: Option<(String, Action)>,
        dismiss: Option<Action>,
        rect: Rect,
    ) {
        let (button_w, text_w, _) =
            self.bar_parts(&text, action.as_ref(), dismiss.is_some(), rect.w);
        self.item(Item::Bar { rect, severity });
        let first_line = Rect::new(rect.x, rect.y, rect.w, metric::BAR_MIN);
        self.item(Item::Glyph {
            rect: Rect::new(rect.x + metric::ROW_PAD, first_line.y, 0.0, first_line.h)
                .left_center(Size::new(metric::ICON, metric::ICON)),
            glyph: icons::severity(severity),
            size: metric::ICON,
            ink: Ink::Severity(severity),
        });
        let text_x = rect.x + metric::ROW_PAD + metric::ICON + metric::BAR_ICON_GAP;
        let text_h = self.text_size(&text, TextStyle::Body, text_w).h;
        self.item(Item::Text {
            rect: Rect::new(text_x, rect.y + (rect.h - text_h) / 2.0, text_w, text_h),
            text,
            style: TextStyle::Body,
            tone: Tone::Primary,
            trim: false,
        });
        let button_y = first_line.y + (first_line.h - self.control) / 2.0;
        let mut right = rect.right();
        let dismiss_rect = dismiss.as_ref().map(|_| {
            right -= metric::GAP + self.control;
            Rect::new(right, button_y, self.control, self.control)
        });
        if dismiss_rect.is_some() {
            right -= metric::GRID;
        } else {
            right -= metric::ROW_PAD;
        }
        if let Some((label, action)) = action {
            self.control(
                Rect::new(right - button_w, button_y, button_w, self.control),
                Kind::Button {
                    label,
                    icon: None,
                    kind: ButtonKind::Standard,
                    action,
                    hint: String::new(),
                },
                true,
                None,
            );
        }
        if let (Some(action), Some(rect)) = (dismiss, dismiss_rect) {
            self.control(
                rect,
                Kind::Button {
                    label: String::new(),
                    icon: Some(Icon::Dismiss),
                    kind: ButtonKind::Subtle,
                    action,
                    hint: "Dismiss".to_owned(),
                },
                true,
                None,
            );
        }
    }
}

/// The room the gaps between `count` things in a row take.
fn gaps(count: usize) -> f32 {
    metric::GAP * count_f32(count.saturating_sub(1))
}

/// A small count as a float.
pub(super) fn count_f32(count: usize) -> f32 {
    f32::from(u16::try_from(count).unwrap_or(u16::MAX))
}
