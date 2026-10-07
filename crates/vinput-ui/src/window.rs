//! The Win32 window around a [`Stage`].
//!
//! The window turns messages into calls on the stage (the mouse moved, a key
//! went down, a view arrived) and carries out the [`Effect`]s the stage
//! answers with. It owns everything that is a handle: the window itself,
//! the canvas, the edit control over a field, the pop-ups.
//!
//! All state lives in one [`Host`] behind a `RefCell`, borrowed once per
//! message. A message that arrives while another is being handled (Windows
//! sends some from inside calls such as `SetFocus`) finds the cell taken and
//! gets the default treatment; nothing that matters arrives that way,
//! because whatever could (closing, coming to the front) is posted to the
//! message loop instead and done there, outside any borrow.
#![allow(unsafe_code)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Weak;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use vinput_view::{Command, Event, View};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{ClientToScreen, InvalidateRect};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, SetFocus, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, IsIconic, KillTimer, PostMessageW, SetTimer, USER_DEFAULT_SCREEN_DPI, WM_APP,
};

use crate::edit::{Edit, Style};
use crate::geometry::{Point, Rect, Size};
use crate::gfx::{Canvas, Gfx};
use crate::hotkey::Modifiers;
use crate::imaging::Imaging;
use crate::popup::{Place, Popup};
use crate::scene::{Phase, Showing};
use crate::stage::{Effect, Stage, vk};
use crate::theme::{Theme, metric, timing};
use crate::{Appearance, Backend, Source, app, clipboard, paint};

/// An event from the backend is waiting in the inbox.
const APP_EVENT: u32 = WM_APP + 1;
/// The window is done and is to be destroyed.
const APP_FINISH: u32 = WM_APP + 2;
/// The window is to come to the front.
const APP_FRONT: u32 = WM_APP + 3;
/// Wakes the message loop, which starts the work of a window that asks.
const APP_WORK: u32 = WM_APP + 4;
/// That work is done; its result is waiting.
const APP_WORKED: u32 = WM_APP + 5;
/// A window that asks has more or less to say now and is to fit it again.
const APP_FIT: u32 = WM_APP + 6;
/// Assistive tools are to be told what changed: the focus, or the whole page.
const APP_TELL: u32 = WM_APP + 7;

/// The timer that moves what moves by itself.
const TIMER_MOVE: usize = 1;
/// The timer that waits before a tooltip shows.
const TIMER_TIP: usize = 2;
/// How many times in a row a lost drawing surface is made again at once,
/// before the window waits for the next reason to paint.
const REPAINTS: u8 = 3;
/// The resource the program's icon is stored under.
const APP_ICON: u16 = 1;
/// The extra mouse button that means "back".
const XBUTTON1: usize = 1;
/// How far one notch of the wheel is, as Windows counts it.
const WHEEL_DELTA: f32 = 120.0;

/// How a window ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Closed {
    /// With a yes: [`crate::node::Action::Accept`].
    Accepted,
    /// With a no: [`crate::node::Action::Close`].
    Refused,
    /// Without an answer: the close button of the title bar, or Esc.
    Dismissed,
}

/// Work a window that asks does when the answer is yes, while it stays
/// open; it runs on a thread of its own.
pub(crate) type Work<'a> = &'a mut (dyn FnMut() -> Result<(), String> + Send);

/// What a window is opened with.
pub(crate) struct Spec<'a> {
    /// What it shows.
    pub(crate) showing: Showing,
    /// The view it starts from.
    pub(crate) view: View,
    /// The theme it is drawn in.
    pub(crate) appearance: Appearance,
    /// Where its data comes from; `None` for a window that only asks.
    pub(crate) source: Option<&'a Source>,
    /// The words it shows while its work runs.
    pub(crate) working: &'a str,
}

/// Everything a window has and is.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts about the window"
)]
pub(crate) struct Host {
    /// Finds this host again from a provider handed to another process.
    me: Weak<RefCell<Host>>,
    /// The control assistive tools were last told has the focus.
    told_focus: Option<usize>,
    /// The shape of the page assistive tools were last told about.
    told_shape: u64,
    /// Whether assistive tools may be handed providers: the thread of the
    /// window is one COM calls back on.
    accessible: bool,
    /// Where the thread of the backend finds the window; emptied when the
    /// window goes.
    target: Arc<AtomicIsize>,
    hwnd: HWND,
    gfx: Gfx,
    imaging: Option<Imaging>,
    canvas: Option<Canvas>,
    /// The icons of the title bar and the taskbar, for the display the
    /// window is on.
    icons: Option<Icons>,
    /// How many paints in a row lost their surface.
    lost: u8,
    theme: Theme,
    appearance: Appearance,
    scale: f32,
    /// What the window shows and what the user is doing in it.
    pub(crate) stage: Stage,
    backend: Option<Box<dyn Backend>>,
    inbox: Arc<Mutex<VecDeque<Event>>>,
    edit: Option<Edit>,
    /// Whether the frame is dressed for a navigation pane.
    paned: bool,
    menu: Option<Popup>,
    tip: Option<Popup>,
    /// The control the tooltip is, or is about to be, shown for.
    tip_for: Option<(String, Rect)>,
    tracking: bool,
    moving: bool,
    moved: Instant,
    outcome: Closed,
    has_work: bool,
    /// The message loop is to start the work.
    work_wanted: bool,
    working: bool,
    /// Where the thread that does the work leaves its result.
    worked: Arc<Mutex<Option<Result<(), String>>>>,
    working_words: String,
}

fn px(units: f32, scale: f32) -> i32 {
    #[expect(clippy::cast_possible_truncation, reason = "sizes on a screen fit")]
    let pixels = (units * scale).round() as i32;
    pixels
}

fn units(pixels: i32, scale: f32) -> f32 {
    #[expect(clippy::cast_precision_loss, reason = "sizes on a screen fit")]
    let units = pixels as f32 / scale;
    units
}

/// The low and high halves of an `LPARAM` as signed numbers: a position.
fn point(lparam: LPARAM) -> (i32, i32) {
    let half = |shift: u32| {
        let bits = u16::try_from((lparam.0 >> shift) & 0xFFFF).unwrap_or(0);
        i32::from(bits.cast_signed())
    };
    (half(0), half(16))
}

fn modifiers() -> Modifiers {
    // SAFETY: plain queries of the keyboard state of this thread.
    let down = |key: u16| unsafe { GetKeyState(i32::from(key)) } < 0;
    Modifiers {
        ctrl: down(VK_CONTROL.0),
        alt: down(VK_MENU.0),
        shift: down(VK_SHIFT.0),
        win: down(VK_LWIN.0) || down(VK_RWIN.0),
    }
}

fn client_pixels(hwnd: HWND) -> (i32, i32) {
    let mut rect = RECT::default();
    // SAFETY: `rect` is valid for the call.
    let _ = unsafe { GetClientRect(hwnd, &raw mut rect) };
    (rect.right.max(1), rect.bottom.max(1))
}

fn scale_of(hwnd: HWND) -> f32 {
    // SAFETY: a plain query of a live window.
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(USER_DEFAULT_SCREEN_DPI / 2);
    f32::from(u16::try_from(dpi).unwrap_or(96)) / 96.0
}

impl Host {
    fn size(&self) -> Size {
        let (width, height) = client_pixels(self.hwnd);
        Size::new(units(width, self.scale), units(height, self.scale))
    }

    fn rect_px(&self, rect: Rect) -> RECT {
        RECT {
            left: px(rect.x, self.scale),
            top: px(rect.y, self.scale),
            right: px(rect.right(), self.scale),
            bottom: px(rect.bottom(), self.scale),
        }
    }

    /// A point in pixels of the window, in units.
    pub(crate) fn units(&self, point: POINT) -> Point {
        Point::new(units(point.x, self.scale), units(point.y, self.scale))
    }

    /// Where a rectangle of the window is on the screen, in pixels.
    pub(crate) fn on_screen(&self, rect: Rect) -> RECT {
        let rect = self.rect_px(rect);
        let mut corner = POINT {
            x: rect.left,
            y: rect.top,
        };
        // SAFETY: `corner` is valid for the call.
        let _ = unsafe { ClientToScreen(self.hwnd, &raw mut corner) };
        RECT {
            left: corner.x,
            top: corner.y,
            right: corner.x + rect.right - rect.left,
            bottom: corner.y + rect.bottom - rect.top,
        }
    }

    fn place(&self, anchor: Rect) -> Place<'_> {
        Place {
            owner: self.hwnd,
            gfx: &self.gfx,
            theme: self.theme,
            scale: self.scale,
            anchor: self.on_screen(anchor),
        }
    }

    fn make_canvas(&mut self) {
        self.canvas = self.gfx.window_canvas(self.hwnd, self.scale).ok();
        if let (Some(canvas), Some(imaging)) = (&mut self.canvas, &self.imaging) {
            let side = u32::try_from(px(metric::LOGO, self.scale)).unwrap_or(20);
            if let Some(icon) = imaging.app_icon(side) {
                canvas.set_logo(&icon);
            }
        }
    }

    fn paint(&mut self) {
        if self.canvas.is_none() {
            self.make_canvas();
        }
        let Some(canvas) = &self.canvas else {
            return;
        };
        let look = self.stage.look();
        let frame = self.stage.frame();
        canvas.begin(paint::background(frame, &self.theme, &look));
        paint::frame(canvas, frame, &self.theme, &look);
        if matches!(canvas.end(), Ok(true)) {
            self.lost = 0;
            return;
        }
        // Windows took the surface away (the display driver restarted, the
        // session was locked): the next paint makes a new one. It is asked
        // for at once, but only a few times in a row, so a surface that
        // cannot be had does not keep the window painting for ever.
        self.canvas = None;
        if self.lost < REPAINTS {
            self.lost += 1;
            self.invalidate();
        }
    }

    /// Lets go of everything that is a handle of its own. Called when the
    /// window is destroyed, after which nothing here may be used again.
    pub(super) fn torn_down(&mut self) {
        self.target.store(0, Ordering::Release);
        self.edit = None;
        self.menu = None;
        self.tip = None;
        self.tip_for = None;
        self.canvas = None;
        self.icons = None;
    }

    /// What the window does last, before it is destroyed: a hotkey field
    /// that was waiting stops, and the background process is told.
    pub(super) fn closing(&mut self) {
        let effects = self.stage.blur();
        self.perform(effects);
        self.send(Command::Closed);
    }

    /// What assistive tools have not been told yet: whether the page is
    /// another one now, and the control that has the focus if that changed.
    pub(super) fn news(&mut self) -> (Weak<RefCell<Self>>, bool, Option<usize>) {
        let shape = self.stage.shape();
        let reshaped = std::mem::replace(&mut self.told_shape, shape) != shape;
        let focused = self.stage.focused();
        let moved = std::mem::replace(&mut self.told_focus, focused) != focused || reshaped;
        (self.me.clone(), reshaped, focused.filter(|_| moved))
    }

    /// Whether assistive tools may be handed providers for this window.
    pub(super) fn accessible(&self) -> bool {
        self.accessible
    }

    /// Whether the message loop is to start the work now. Says so once.
    pub(super) fn take_work_wanted(&mut self) -> bool {
        std::mem::take(&mut self.work_wanted)
    }

    /// Takes up the result of the work, if the thread that did it has left
    /// one.
    pub(super) fn collect_work(&mut self) {
        let outcome = self.worked.lock().ok().and_then(|mut slot| slot.take());
        if let Some(outcome) = outcome {
            self.worked(outcome);
        }
    }

    fn invalidate(&self) {
        // SAFETY: the window is live.
        let _ = unsafe { InvalidateRect(Some(self.hwnd), None, false) };
    }

    fn send(&self, command: Command) {
        if let Some(backend) = &self.backend {
            backend.send(command);
        }
    }

    fn post(&self, message: u32) {
        // SAFETY: the window is live; a failed post means it is going away.
        let _ = unsafe { PostMessageW(Some(self.hwnd), message, WPARAM(0), LPARAM(0)) };
    }

    /// Carries out what the stage asked for.
    pub(crate) fn perform(&mut self, effects: Vec<Effect>) {
        let mut queue = VecDeque::from(effects);
        while let Some(effect) = queue.pop_front() {
            match effect {
                Effect::Send(command) => self.send(command),
                Effect::Close => self.finish(Closed::Refused),
                Effect::Dismiss => self.finish(Closed::Dismissed),
                Effect::Accept => self.accept(),
                Effect::Copy(text) => {
                    clipboard::copy(self.hwnd, &text);
                }
                Effect::Paste => {
                    if let Some(edit) = &self.edit {
                        edit.paste();
                    }
                }
                Effect::Edit(editing) => {
                    let style = Style {
                        family: &self.gfx.fonts().text_family(),
                        pixels: px(self.gfx.fonts().text_size(), self.scale),
                        text: self.theme.palette.text,
                        fill: self.theme.palette.field_active,
                        bleed: px(metric::EDIT_BLEED, self.scale),
                    };
                    self.edit = Edit::create(self.hwnd, &editing.text, editing.secret, &style);
                    self.place_edit();
                    if let Some(edit) = &self.edit {
                        edit.select(editing.select_all);
                        edit.focus();
                    } else {
                        // Without an edit control nothing can be typed, and
                        // the field must not go on looking as if it could.
                        queue.extend(self.stage.edit_lost());
                    }
                }
                Effect::EndEdit => {
                    if let Some(edit) = self.edit.take()
                        && edit.focused()
                    {
                        // SAFETY: the window is live and of this thread.
                        let _ = unsafe { SetFocus(Some(self.hwnd)) };
                    }
                }
                Effect::Retype(text) => {
                    if let Some(edit) = &self.edit {
                        edit.retype(&text);
                    }
                }
                Effect::OpenChoice(choosing) => {
                    self.tip = None;
                    self.menu = Popup::menu(
                        &self.place(choosing.anchor),
                        choosing.items,
                        choosing.selected,
                    );
                    if self.menu.is_none() {
                        self.stage.choice_closed();
                    }
                }
                Effect::CloseChoice => self.menu = None,
                Effect::Front => self.post(APP_FRONT),
            }
        }
        self.settle();
    }

    /// Brings the handles in line with the stage after anything happened.
    fn settle(&mut self) {
        // The welcome has no navigation pane and the pages after it do.
        if self.stage.frame().pane.is_some() != self.paned {
            self.dress_frame();
        }
        if self.stage.take_dirty() {
            self.invalidate();
        }
        // Assistive tools are told from the message loop, outside any borrow
        // of this: they ask the providers questions while they are told.
        if self.stage.focused() != self.told_focus || self.stage.shape() != self.told_shape {
            self.post(APP_TELL);
        }
        self.place_edit();
        if !self.stage.choosing() {
            self.menu = None;
        }
        let moving = self.stage.moving();
        if moving != self.moving {
            self.moving = moving;
            self.moved = Instant::now();
            // SAFETY: the window is live; the timer belongs to it.
            unsafe {
                if moving {
                    SetTimer(Some(self.hwnd), TIMER_MOVE, timing::FRAME, None);
                } else {
                    let _ = KillTimer(Some(self.hwnd), TIMER_MOVE);
                }
            }
        }
        let wanted = self.stage.tip().map(|(text, rect)| (text.to_owned(), rect));
        if wanted != self.tip_for {
            self.tip = None;
            self.tip_for = wanted;
            // SAFETY: the window is live; the timer belongs to it.
            unsafe {
                if self.tip_for.is_some() {
                    SetTimer(Some(self.hwnd), TIMER_TIP, timing::TIP_DELAY, None);
                } else {
                    let _ = KillTimer(Some(self.hwnd), TIMER_TIP);
                }
            }
        }
    }

    fn place_edit(&self) {
        if let (Some(edit), Some((rect, seen))) = (&self.edit, self.stage.edit_rect()) {
            edit.place(self.rect_px(rect), seen);
        }
    }

    fn finish(&mut self, outcome: Closed) {
        if self.working {
            return;
        }
        self.outcome = outcome;
        self.post(APP_FINISH);
    }

    fn accept(&mut self) {
        if !self.has_work {
            self.finish(Closed::Accepted);
        } else if !self.working {
            self.working = true;
            self.work_wanted = true;
            let words = self.working_words.clone();
            let effects = self.stage.set_phase(Phase::Working(words));
            self.perform(effects);
            self.post(APP_FIT);
            self.post(APP_WORK);
        }
    }

    /// The size the inside of a window that asks needs for what it says now,
    /// in pixels.
    fn fitting(&self) -> Option<(i32, i32)> {
        if !matches!(self.stage.showing(), Showing::Asking(_)) {
            return None;
        }
        let content = self.stage.showing().content(self.stage.view());
        let height = crate::shell::compact_height(&content, metric::DIALOG_W, self.gfx.fonts());
        Some((px(metric::DIALOG_W, self.scale), px(height, self.scale)))
    }

    /// The work a yes set off is done.
    fn worked(&mut self, result: Result<(), String>) {
        self.working = false;
        match result {
            Ok(()) => self.finish(Closed::Accepted),
            Err(reason) => {
                let effects = self.stage.set_phase(Phase::Failed(reason));
                self.perform(effects);
                self.post(APP_FIT);
            }
        }
    }

    fn drain(&mut self) {
        let waiting = self
            .inbox
            .lock()
            .map(|mut inbox| std::mem::take(&mut *inbox))
            .unwrap_or_default();
        // A view replaces the one before it whole, so of several that piled
        // up (a download reports often) only the last is worth laying out.
        let last_view = waiting
            .iter()
            .rposition(|event| matches!(event, Event::State(_)));
        for (index, event) in waiting.into_iter().enumerate() {
            let effects = match event {
                Event::State(_) if Some(index) != last_view => continue,
                Event::State(view) => self.stage.set_view(*view),
                Event::Show(page) => self.stage.show(page),
                Event::Front => self.stage.front(),
                Event::Close => {
                    self.finish(Closed::Dismissed);
                    Vec::new()
                }
            };
            self.perform(effects);
        }
    }

    /// Windows changed its colors, its theme or the size of its text.
    fn retheme(&mut self) {
        let theme = app::theme(self.appearance);
        if theme == self.theme {
            return;
        }
        // Pop-ups keep the colors they opened with.
        self.menu = None;
        self.tip = None;
        self.stage.choice_closed();
        let rescaled = (theme.text_scale - self.theme.text_scale).abs() > f32::EPSILON;
        self.theme = theme;
        if rescaled && let Ok(gfx) = Gfx::new(theme.text_scale) {
            self.gfx = gfx;
            self.canvas = None;
            let effects = self.stage.remeasure(Box::new(self.gfx.fonts().clone()));
            self.perform(effects);
        }
        self.redress();
        // An edit control keeps the colors and the font it was made with.
        let effects = self.stage.edit_lost();
        self.perform(effects);
        let effects = self.stage.refocus();
        self.perform(effects);
        self.invalidate();
    }

    /// Dresses the frame for the theme and the display the window is on.
    pub(super) fn redress(&mut self) {
        self.dress_frame();
        // The window has the new icons before the old ones are given back.
        let icons = Icons::give(self.hwnd);
        self.icons = Some(icons);
    }

    /// Dresses the frame for what the window shows now: the tint reaches
    /// under the navigation pane and nowhere else, so it comes and goes with
    /// the pane.
    fn dress_frame(&mut self) {
        let pane = self.stage.frame().pane.is_some();
        let showing = self.stage.showing();
        let mica = frame::dress(self.hwnd, &self.theme, showing, pane, self.scale);
        self.stage.set_mica(mica);
        self.paned = pane;
    }

    fn rescale(&mut self, scale: f32) {
        self.scale = scale;
        self.canvas = None;
        self.menu = None;
        self.tip = None;
        self.stage.choice_closed();
        self.redress();
        let effects = self.stage.edit_lost();
        self.perform(effects);
    }

    fn resized(&mut self) {
        // SAFETY: a plain query of a live window.
        if unsafe { IsIconic(self.hwnd) }.as_bool() {
            // A window nobody sees needs nothing to draw on; the next paint
            // makes it again.
            self.canvas = None;
            return;
        }
        let effects = self.stage.resize(self.size());
        self.perform(effects);
        self.invalidate();
    }

    fn mouse(&self, lparam: LPARAM) -> Point {
        let (x, y) = point(lparam);
        Point::new(units(x, self.scale), units(y, self.scale))
    }

    fn key_down(&mut self, key: u16) -> bool {
        if let Some(menu) = &self.menu {
            if key == vk::ESCAPE || key == vk::TAB {
                let effects = self.stage.close_menu();
                self.perform(effects);
                return key == vk::ESCAPE;
            }
            let (used, pick) = menu.key(key);
            if let Some(index) = pick {
                let effects = self.stage.picked(index);
                self.perform(effects);
            }
            return used;
        }
        let (used, effects) = self.stage.key_down(key, modifiers());
        self.perform(effects);
        used
    }
}

mod frame;
mod launch;
mod messages;

use frame::Icons;

pub(crate) use launch::{open, run};
