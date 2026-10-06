//! What a window looks like beyond its layout: the state painting needs
//! that is not in the [`Frame`]. Kept apart from the painting itself so
//! that everything which decides it can be tested without drawing.

use crate::geometry::Rect;
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
    /// Where a progress bar that moves by itself is in its sweep, 0 to 1.
    pub phase: f32,
    /// Whether Windows shows its wallpaper tint behind the navigation pane,
    /// so the pane must be left unpainted.
    pub mica: bool,
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
