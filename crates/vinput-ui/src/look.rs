//! What a window looks like beyond its layout: the state painting needs
//! that is not in the [`Frame`]. Kept apart from the painting itself so
//! that everything which decides it can be tested without drawing.

use crate::geometry::Rect;
use crate::layout::{Control, Item, Pane, Region};
use crate::shell::Frame;
use crate::theme::metric;

/// How much of the scroll bar shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScrollBar {
    /// None: the mouse is elsewhere.
    #[default]
    Hidden,
    /// A thin line: the mouse is over the page.
    Thin,
    /// The thumb to drag: the mouse is over the bar.
    Wide,
}

/// Everything about a window that is not in its [`Frame`]: what the user is
/// in the middle of.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Look<'a> {
    /// The control under the mouse.
    pub hover: Option<usize>,
    /// The control held down.
    pub pressed: Option<usize>,
    /// The control with the keyboard focus.
    pub focus: Option<usize>,
    /// Whether the focus shows its rings: it got there by keyboard.
    pub ring: bool,
    /// The field the system's edit control is lying over right now, which
    /// draws the text itself.
    pub editing: Option<usize>,
    /// The hotkey field that is waiting for keys, with the modifiers held so
    /// far as text.
    pub capture: Option<(usize, &'a str)>,
    /// The list whose drop-down is open.
    pub open: Option<usize>,
    /// Whether the mouse is over the cross that clears the hovered field.
    pub over_clear: bool,
    /// How far the body is scrolled.
    pub scroll: f32,
    /// How much of the scroll bar shows.
    pub bar: ScrollBar,
    /// How far each area that scrolls by itself is scrolled, in the order
    /// of the frame's. One that is missing is not scrolled.
    pub panes: &'a [f32],
    /// The area of those whose scroll bar the mouse is over or drags.
    pub pane_bar: Option<usize>,
    /// Where a progress bar that moves by itself is in its sweep, 0 to 1.
    pub phase: f32,
    /// Whether Windows shows its wallpaper tint behind the navigation pane,
    /// so the pane must be left unpainted.
    pub mica: bool,
}

impl Look<'_> {
    /// How far the area with this index is scrolled.
    #[must_use]
    pub fn pane(&self, index: usize) -> f32 {
        self.panes.get(index).copied().unwrap_or(0.0)
    }

    /// How far down something in `region` and in the area `pane` is moved
    /// from where the layout put it: by the scrolling of the page and of
    /// its area.
    #[must_use]
    pub fn shift(&self, region: Region, pane: Option<usize>) -> f32 {
        let body = match region {
            Region::Fixed => 0.0,
            Region::Body => self.scroll,
        };
        -body - pane.map_or(0.0, |index| self.pane(index))
    }

    /// Where `control` is in the window right now.
    #[must_use]
    pub fn placed(&self, control: &Control) -> Rect {
        control
            .rect
            .moved(0.0, self.shift(control.region, control.pane()))
    }

    /// Where the area that scrolls by itself is in the window right now.
    #[must_use]
    pub fn pane_rect(&self, pane: &Pane) -> Rect {
        pane.rect.moved(0.0, self.shift(pane.region, None))
    }
}

/// The area that scrolls by itself which the item at `index` of `items` is
/// in, if it is in one.
#[must_use]
pub fn pane_of(items: &[Item], index: usize) -> Option<usize> {
    let mut before = items.get(..index)?.iter().rev();
    before
        .find_map(|item| match item {
            Item::Clip(pane, _) => Some(Some(*pane)),
            Item::Unclip => Some(None),
            _ => None,
        })
        .flatten()
}

/// Whether something at `rect`, in `region` and in the area `pane`, is out
/// of sight: scrolled out of the page, or out of the area it is in.
#[must_use]
pub fn out_of_sight(
    frame: &Frame,
    look: &Look<'_>,
    rect: Rect,
    region: Region,
    pane: Option<usize>,
) -> bool {
    let area = pane.and_then(|pane| frame.panes().get(pane));
    (region == Region::Body && !rect.touches(frame.viewport))
        || area.is_some_and(|area| !rect.touches(look.pane_rect(area)))
}

/// Where the thumb of the scroll bar of an area that scrolls by itself is,
/// given where the area is right now and how far it is scrolled.
#[must_use]
pub fn pane_thumb(pane: &Pane, at: Rect, scroll: f32, wide: bool) -> Option<Rect> {
    let range = pane.range();
    if range <= 0.0 || at.h <= 0.0 {
        return None;
    }
    let lane = at.inset_xy(0.0, metric::GRID);
    let length = (lane.h * at.h / pane.content)
        .max(metric::SCROLL_MIN)
        .min(lane.h);
    let width = if wide {
        metric::SCROLL_WIDE
    } else {
        metric::SCROLL_THIN
    };
    let travel = (lane.h - length).max(0.0);
    Some(Rect::new(
        lane.right() - metric::GRID - width,
        lane.y + travel * (scroll / range).clamp(0.0, 1.0),
        width,
        length,
    ))
}

/// Where the thumb of the scroll bar is, if the body scrolls at all.
#[must_use]
pub fn thumb(frame: &Frame, scroll: f32, wide: bool) -> Option<Rect> {
    let range = frame.scroll_range();
    if range <= 0.0 || frame.viewport.h <= 0.0 {
        return None;
    }
    let lane = frame.viewport.inset_xy(0.0, metric::GRID);
    let length = (lane.h * frame.viewport.h / frame.content_height).max(metric::SCROLL_MIN);
    let width = if wide {
        metric::SCROLL_WIDE
    } else {
        metric::SCROLL_THIN
    };
    let travel = (lane.h - length).max(0.0);
    Some(Rect::new(
        lane.right() - metric::GRID - width,
        lane.y + travel * (scroll / range).clamp(0.0, 1.0),
        width,
        length,
    ))
}
