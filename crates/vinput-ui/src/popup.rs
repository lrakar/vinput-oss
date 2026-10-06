//! Small windows over the main one: the drop-down of a list and tooltips.
//!
//! Each is a window of its own, so it can reach past the edge of the window
//! it belongs to, and Windows 11 gives it rounded corners and a shadow. It
//! never takes the keyboard: the main window keeps it and passes the keys
//! on, and is told with a message when an entry is picked.
#![allow(unsafe_code)]

use std::cell::RefCell;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DWMWCP_ROUNDSMALL,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromRect, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CS_DROPSHADOW, CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA,
    GetWindowLongPtrW, HTTRANSPARENT, IDC_ARROW, LoadCursorW, MA_NOACTIVATE, PostMessageW,
    RegisterClassExW, SW_SHOWNOACTIVATE, SetWindowLongPtrW, ShowWindow, WM_APP, WM_ERASEBKGND,
    WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NCHITTEST, WM_PAINT, WNDCLASSEXW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{BOOL, w};

use crate::geometry::{Rect, Size};
use crate::gfx::{Align, Canvas, Gfx, TextBox};
use crate::layout::Measure;
use crate::node::TextStyle;
use crate::theme::{Theme, face, metric, timing};

/// Sent to the owner when an entry of a drop-down is picked; its index is in
/// `wParam`.
pub(crate) const POPUP_PICK: u32 = WM_APP + 20;

const VK_RETURN: u16 = 0x0D;
const VK_SPACE: u16 = 0x20;
const VK_END: u16 = 0x23;
const VK_HOME: u16 = 0x24;
const VK_UP: u16 = 0x26;
const VK_DOWN: u16 = 0x28;

/// What a pop-up shows.
enum What {
    /// The entries of a list, the picked one and the one under the mouse or
    /// the arrow keys.
    Menu {
        items: Vec<String>,
        selected: usize,
        hot: usize,
        /// The first entry in sight, when there are more than fit.
        top: usize,
    },
    /// A line of help.
    Tip(String),
}

struct State {
    what: What,
    theme: Theme,
    /// What the canvas is made with, and made again with if Windows takes
    /// its surface away.
    gfx: Gfx,
    scale: f32,
    canvas: Option<Canvas>,
    hwnd: HWND,
    owner: HWND,
    size: Size,
    /// The height of an entry of a menu.
    item: f32,
    /// What was typed to find an entry, and when its last letter was.
    typed: Option<(String, Instant)>,
}

/// A pop-up window. Goes away when dropped.
pub(crate) struct Popup {
    hwnd: HWND,
    state: Box<RefCell<State>>,
}

/// Where a pop-up is anchored and how it is drawn.
pub(crate) struct Place<'a> {
    /// The window it belongs to.
    pub(crate) owner: HWND,
    /// What draws it.
    pub(crate) gfx: &'a Gfx,
    /// The theme.
    pub(crate) theme: Theme,
    /// How many pixels a unit is.
    pub(crate) scale: f32,
    /// The control it belongs to, in pixels of the screen.
    pub(crate) anchor: RECT,
}

fn rows(count: usize) -> usize {
    count.clamp(1, metric::MENU_ROWS)
}

fn count_f32(count: usize) -> f32 {
    f32::from(u16::try_from(count).unwrap_or(u16::MAX))
}

/// The entry of a menu at the height `y`, counted from the first in sight,
/// where an entry is `item` tall.
fn row_at(y: f32, item: f32) -> Option<usize> {
    let row = ((y - metric::MENU_PAD) / item.max(1.0)).floor();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked to be a small whole number that is not negative"
    )]
    (0.0..1000.0).contains(&row).then_some(row as usize)
}

/// The entry of `items` that typing `typed` goes to from the entry `from`:
/// the next one that starts with it, whatever the case, looking past the end
/// and around. One letter typed again moves on to the next entry that
/// starts with it; more letters stay on an entry for as long as it matches.
fn type_ahead(items: &[String], from: usize, typed: &str) -> Option<usize> {
    let typed = typed.to_lowercase();
    let count = items.len();
    if typed.is_empty() || count == 0 {
        return None;
    }
    let first = if typed.chars().count() == 1 {
        from + 1
    } else {
        from
    };
    (0..count)
        .map(|step| (first + step) % count)
        .find(|index| items[*index].to_lowercase().starts_with(&typed))
}

impl State {
    fn paint(&mut self) {
        if self.canvas.is_none() {
            self.canvas = self.gfx.window_canvas(self.hwnd, self.scale).ok();
        }
        let Some(canvas) = &self.canvas else {
            return;
        };
        let colors = &self.theme.palette;
        let all = Rect::new(0.0, 0.0, self.size.w, self.size.h);
        canvas.begin(colors.flyout);
        canvas.stroke(all, 0.0, colors.flyout_stroke, metric::LINE);
        match &self.what {
            What::Tip(text) => {
                let how = TextBox {
                    align: Align::Left,
                    ..TextBox::default()
                };
                canvas.text(
                    text,
                    all.inset_xy(metric::TIP_PAD_X, metric::TIP_PAD_Y),
                    TextStyle::Caption,
                    colors.text,
                    how,
                );
            }
            What::Menu {
                items,
                selected,
                hot,
                top,
            } => {
                let pad = metric::MENU_PAD;
                for (row, (index, item)) in items
                    .iter()
                    .enumerate()
                    .skip(*top)
                    .take(metric::MENU_ROWS)
                    .enumerate()
                {
                    let rect = Rect::new(
                        pad,
                        pad + count_f32(row) * self.item,
                        self.size.w - 2.0 * pad,
                        self.item,
                    );
                    let inner = rect.inset_xy(0.0, metric::GRID / 2.0);
                    if index == *hot || index == *selected {
                        canvas.fill(inner, metric::RADIUS, colors.subtle_hover);
                    }
                    if self.theme.contrast && index == *hot {
                        canvas.stroke(inner, metric::RADIUS, colors.accent, metric::FOCUS);
                    }
                    if index == *selected {
                        canvas.fill(
                            Rect::new(
                                inner.x,
                                inner.center().y - metric::PILL_H / 2.0,
                                metric::PILL_W,
                                metric::PILL_H,
                            ),
                            metric::PILL_W / 2.0,
                            colors.accent,
                        );
                    }
                    let how = TextBox {
                        middle: true,
                        trim: true,
                        ..TextBox::default()
                    };
                    canvas.text(
                        item,
                        inner.inset_xy(metric::FIELD_PAD, 0.0),
                        TextStyle::Body,
                        colors.text,
                        how,
                    );
                }
            }
        }
        // A lost surface is made again by the next paint.
        if !matches!(canvas.end(), Ok(true)) {
            self.canvas = None;
        }
    }
}

unsafe extern "system" fn proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: the user data is either zero or points at the `RefCell` owned
    // by the `Popup` that owns this window, which destroys the window before
    // it frees the cell.
    let state =
        unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<State>).as_ref() };
    let Some(state) = state else {
        // SAFETY: passing on a message this window was sent.
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    };
    match message {
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            // SAFETY: `paint` is valid for both calls and `hwnd` is live.
            unsafe {
                BeginPaint(hwnd, &raw mut paint);
                if let Ok(mut state) = state.try_borrow_mut() {
                    state.paint();
                }
                let _ = EndPaint(hwnd, &raw const paint);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEACTIVATE => LRESULT(isize::try_from(MA_NOACTIVATE).unwrap_or(3)),
        WM_NCHITTEST => match state.try_borrow().as_deref().map(|state| &state.what) {
            // A tooltip is not there as far as the mouse goes.
            Ok(What::Tip(_)) => LRESULT(HTTRANSPARENT as isize),
            // SAFETY: passing on a message this window was sent.
            _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
        },
        WM_MOUSEMOVE | WM_LBUTTONUP => {
            let Ok(mut state) = state.try_borrow_mut() else {
                return LRESULT(0);
            };
            // The high half of `lparam` is the height, a signed number.
            let high = u16::try_from((lparam.0 >> 16) & 0xFFFF).unwrap_or(0);
            let y = f32::from(high.cast_signed()) / state.scale;
            let (owner, item) = (state.owner, state.item);
            if let What::Menu {
                items, hot, top, ..
            } = &mut state.what
                && let Some(index) = row_at(y, item).map(|row| row + *top)
                && index < items.len()
            {
                if message == WM_LBUTTONUP {
                    // SAFETY: the owner is a live window; a failed post only
                    // loses the pick.
                    let _ =
                        unsafe { PostMessageW(Some(owner), POPUP_PICK, WPARAM(index), LPARAM(0)) };
                } else if *hot != index {
                    *hot = index;
                    // SAFETY: `hwnd` is live.
                    let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
                }
            }
            LRESULT(0)
        }
        // SAFETY: passing on a message this window was sent.
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

/// The part of the screen a pop-up near `anchor` may use, in pixels.
fn work_area(anchor: RECT) -> RECT {
    let mut info = MONITORINFO {
        cbSize: u32::try_from(size_of::<MONITORINFO>()).unwrap_or(0),
        ..Default::default()
    };
    // SAFETY: `info` is valid for the call and says its own size.
    unsafe {
        let monitor = MonitorFromRect(&raw const anchor, MONITOR_DEFAULTTONEAREST);
        if GetMonitorInfoW(monitor, &raw mut info).as_bool() {
            info.rcWork
        } else {
            anchor
        }
    }
}

/// Moves `top`, the first entry in sight of a list of `count`, so that the
/// entry `to` is in sight.
fn reveal(top: &mut usize, to: usize, count: usize) {
    let shown = rows(count);
    if to < *top {
        *top = to;
    } else if to >= *top + shown {
        *top = to + 1 - shown;
    }
}

fn px(units: f32, scale: f32) -> i32 {
    #[expect(clippy::cast_possible_truncation, reason = "sizes on a screen fit")]
    let pixels = (units * scale).round() as i32;
    pixels
}

impl Popup {
    fn open(
        place: &Place<'_>,
        what: What,
        (size, item): (Size, f32),
        at: POINT,
        round_small: bool,
    ) -> Option<Self> {
        static CLASS: std::sync::Once = std::sync::Once::new();
        let class = w!("VinputPopup");
        let (width, height) = (px(size.w, place.scale), px(size.h, place.scale));
        let area = work_area(place.anchor);
        let x = at.x.min(area.right - width).max(area.left);
        let y = at.y.min(area.bottom - height).max(area.top);
        // SAFETY: the class is registered before it is used, every pointer
        // passed is valid for its call, and the window is checked before
        // use. The state the window points at is boxed, so it stays where
        // it is for as long as the `Popup` that owns both lives.
        unsafe {
            let module = GetModuleHandleW(None).ok()?;
            CLASS.call_once(|| {
                let class = WNDCLASSEXW {
                    cbSize: u32::try_from(size_of::<WNDCLASSEXW>()).unwrap_or(0),
                    style: CS_DROPSHADOW,
                    lpfnWndProc: Some(proc),
                    hInstance: module.into(),
                    hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                    lpszClassName: class,
                    ..Default::default()
                };
                RegisterClassExW(&raw const class);
            });
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                class,
                w!(""),
                WS_POPUP,
                x,
                y,
                width,
                height,
                Some(place.owner),
                None,
                Some(module.into()),
                None,
            )
            .ok()?;
            let dark = BOOL::from(place.theme.dark);
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                (&raw const dark).cast(),
                4,
            );
            let corners = if round_small {
                DWMWCP_ROUNDSMALL
            } else {
                DWMWCP_ROUND
            };
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                (&raw const corners).cast(),
                4,
            );
            let state = Box::new(RefCell::new(State {
                what,
                theme: place.theme,
                gfx: place.gfx.clone(),
                scale: place.scale,
                canvas: None,
                hwnd,
                owner: place.owner,
                size,
                item,
                typed: None,
            }));
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, &raw const *state as isize);
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            Some(Self { hwnd, state })
        }
    }

    /// Opens the drop-down of a list over the list itself, the picked entry
    /// where the list shows it.
    pub(crate) fn menu(place: &Place<'_>, items: Vec<String>, selected: usize) -> Option<Self> {
        let fonts = place.gfx.fonts();
        let widest = items
            .iter()
            .map(|item| fonts.text(item, TextStyle::Body, f32::MAX).w)
            .fold(0.0, f32::max);
        let anchor = place.anchor;
        let anchor_w =
            f32::from(i16::try_from(anchor.right - anchor.left).unwrap_or(0)) / place.scale;
        let anchor_h =
            f32::from(i16::try_from(anchor.bottom - anchor.top).unwrap_or(0)) / place.scale;
        let pad = metric::MENU_PAD;
        let width =
            (widest + 2.0 * (pad + metric::FIELD_PAD) + metric::GAP).max(anchor_w + 2.0 * pad);
        let shown = rows(items.len());
        let selected = selected.min(items.len().saturating_sub(1));
        // An entry is as much taller than a line of its text as it usually
        // is, so larger text gets taller entries.
        let line = fonts.text("", TextStyle::Body, f32::MAX).h;
        let item = metric::MENU_ITEM.max(line + metric::MENU_ITEM - face(TextStyle::Body).line);
        let size = Size::new(width, count_f32(shown) * item + 2.0 * pad);
        let top = selected
            .saturating_sub(shown / 2)
            .min(items.len().saturating_sub(shown));
        let row = count_f32(selected.saturating_sub(top));
        let lift = pad + row * item + (item - anchor_h) / 2.0;
        let at = POINT {
            x: anchor.left - px(pad, place.scale),
            y: anchor.top - px(lift, place.scale),
        };
        let what = What::Menu {
            hot: selected,
            items,
            selected,
            top,
        };
        Self::open(place, what, (size, item), at, false)
    }

    /// Shows a line of help over the control it is about, or under it if
    /// there is no room above.
    pub(crate) fn tip(place: &Place<'_>, text: &str) -> Option<Self> {
        let fonts = place.gfx.fonts();
        let room = metric::TIP_MAX - 2.0 * metric::TIP_PAD_X;
        let words = fonts.text(text, TextStyle::Caption, room);
        let size = Size::new(
            words.w + 2.0 * metric::TIP_PAD_X + 1.0,
            words.h + 2.0 * metric::TIP_PAD_Y,
        );
        let anchor = place.anchor;
        let gap = px(metric::GRID, place.scale);
        let (width, height) = (px(size.w, place.scale), px(size.h, place.scale));
        let above = anchor.top - gap - height;
        let at = POINT {
            x: (anchor.left + anchor.right - width) / 2,
            y: if above >= work_area(anchor).top {
                above
            } else {
                anchor.bottom + gap
            },
        };
        Self::open(place, What::Tip(text.to_owned()), (size, 0.0), at, true)
    }

    fn repaint(&self) {
        // SAFETY: the window is live for as long as `self` is.
        let _ = unsafe { InvalidateRect(Some(self.hwnd), None, false) };
    }

    /// Passes a key to a drop-down. Returns the entry it picks, if it picks
    /// one, and whether the key was for the drop-down at all.
    pub(crate) fn key(&self, key: u16) -> (bool, Option<usize>) {
        let Ok(mut state) = self.state.try_borrow_mut() else {
            return (false, None);
        };
        let What::Menu {
            items, hot, top, ..
        } = &mut state.what
        else {
            return (false, None);
        };
        let last = items.len().saturating_sub(1);
        let to = match key {
            VK_UP => hot.saturating_sub(1),
            VK_DOWN => (*hot + 1).min(last),
            VK_HOME => 0,
            VK_END => last,
            VK_RETURN | VK_SPACE => return (true, Some(*hot)),
            _ => return (false, None),
        };
        *hot = to;
        reveal(top, to, items.len());
        drop(state);
        self.repaint();
        (true, None)
    }

    /// Passes a typed character to a drop-down, which goes to the entry
    /// that starts with what was typed in quick succession.
    pub(crate) fn typed(&self, character: char) {
        if character.is_control() {
            return;
        }
        let Ok(mut state) = self.state.try_borrow_mut() else {
            return;
        };
        let now = Instant::now();
        let pause = Duration::from_millis(u64::from(timing::TYPE_AHEAD));
        let mut word = match state.typed.take() {
            Some((word, at)) if now.duration_since(at) < pause => word,
            _ => String::new(),
        };
        // The same letter again asks for the next entry that starts with it.
        if !(word.chars().count() == 1 && word.chars().eq([character])) {
            word.push(character);
        }
        if let What::Menu {
            items, hot, top, ..
        } = &mut state.what
            && let Some(to) = type_ahead(items, *hot, &word)
        {
            *hot = to;
            reveal(top, to, items.len());
        }
        state.typed = Some((word, now));
        drop(state);
        self.repaint();
    }

    /// Scrolls a drop-down that has more entries than fit.
    pub(crate) fn wheel(&self, notches: f32) {
        if let Ok(mut state) = self.state.try_borrow_mut()
            && let What::Menu { items, top, .. } = &mut state.what
        {
            let most = items.len().saturating_sub(rows(items.len()));
            *top = if notches > 0.0 {
                top.saturating_sub(1)
            } else {
                (*top + 1).min(most)
            };
        }
        self.repaint();
    }
}

impl Drop for Popup {
    fn drop(&mut self) {
        // SAFETY: the window was made in `open` and is not used again. It is
        // cut loose from the state first, which is freed after this.
        unsafe {
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Vec<String> {
        [
            "Default (system)",
            "Headset",
            "headphones",
            "Microphone",
            "Mixer",
        ]
        .map(str::to_owned)
        .to_vec()
    }

    #[test]
    fn typing_goes_to_the_next_entry_that_starts_with_it() {
        let items = names();
        assert_eq!(type_ahead(&items, 0, "m"), Some(3));
        // The same letter again moves on, and around the end.
        assert_eq!(type_ahead(&items, 3, "m"), Some(4));
        assert_eq!(type_ahead(&items, 4, "m"), Some(3));
        // More letters narrow it down, and stay where they match.
        assert_eq!(type_ahead(&items, 1, "he"), Some(1));
        assert_eq!(type_ahead(&items, 1, "HEADP"), Some(2));
        assert_eq!(type_ahead(&items, 0, "x"), None);
        assert_eq!(type_ahead(&items, 0, ""), None);
        assert_eq!(type_ahead(&[], 0, "a"), None);
    }

    #[test]
    fn the_entry_the_keys_are_on_stays_in_sight() {
        let mut top = 0;
        reveal(&mut top, metric::MENU_ROWS + 2, 40);
        assert_eq!(top, 3);
        reveal(&mut top, 1, 40);
        assert_eq!(top, 1);
        reveal(&mut top, 5, 40);
        assert_eq!(top, 1);
    }

    #[test]
    fn a_height_in_a_menu_is_an_entry_or_none() {
        let item = metric::MENU_ITEM;
        assert_eq!(row_at(0.0, item), None);
        assert_eq!(row_at(metric::MENU_PAD + 1.0, item), Some(0));
        assert_eq!(row_at(metric::MENU_PAD + item * 2.5, item), Some(2));
        assert_eq!(row_at(-50.0, item), None);
    }
}
