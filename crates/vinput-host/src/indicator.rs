//! The listening indicator: a small capsule of sound bars that floats under
//! the text caret, or a larger one at the bottom of the screen when the text
//! is not typed at the caret.
//!
//! It is a layered window that the mouse and the keyboard pass straight
//! through, so it never disturbs the app being dictated into. The picture
//! comes from [`crate::draw`], at the scale of whichever monitor the
//! indicator is on, and is kept inside that monitor's work area.
//!
//! While Vinput listens the host redraws the indicator, by calling
//! [`Indicator::update`]: once when the session starts, so that the key
//! press is answered at once, and then from its own tick. The two things
//! that happen after the host has stopped ticking, the glow while the last
//! words are typed and the fade at the end, run on a timer of the
//! indicator's own, which exists only for as long as they last: a hidden
//! indicator costs nothing.
//!
//! Where the caret is, is asked of the program in the foreground, which may
//! take its time or never answer. That is done by a [`caret::Watch`] on a
//! thread of its own, for as long as the indicator follows the caret; this
//! thread only takes the last answer, so nothing here waits for another
//! program beyond the few milliseconds the first frame gives it.
//!
//! Being a topmost window does not keep the indicator on top for good. In a
//! Vinput that has run for a while, Windows is found to have let it sink
//! below the windows used since, while it still says it is topmost: it was
//! shown where it belonged and nobody saw it. So the indicator is put back
//! above every other window each time it is shown, and again if the window
//! being dictated into gets above it while it shows.
#![allow(unsafe_code)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    reason = "Win32 structure sizes, flags and coordinates are small values in fixed-width fields"
)]

use std::ptr::{NonNull, null, null_mut};
use std::time::Duration;

use windows_sys::Win32::Foundation::{HWND, POINT, RECT, SIZE};
use windows_sys::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetMonitorInfoW,
    HBITMAP, HDC, HGDIOBJ, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
    MONITORINFO, MonitorFromPoint, MonitorFromWindow, SelectObject,
};
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RegGetValueW};
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, GW_HWNDPREV, GWLP_USERDATA, GetForegroundWindow, GetWindow,
    GetWindowLongPtrW, HWND_TOPMOST, KillTimer, SW_HIDE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SWP_SHOWWINDOW, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow, ULW_ALPHA,
    UpdateLayeredWindow, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::caret::{self, Anchor};
use crate::draw::{self, Artwork, Bar, INDICATOR_BARS, Levels, Rgb};
use crate::windows::{last_error, wide};
use vinput_core::Result;

/// The caret is looked up again every this many redraws, since it moves as text
/// is typed but looking it up is the expensive part of a redraw.
const REPOSITION_EVERY: u32 = 4;
/// How long the first frame waits for the caret to be found, so that the
/// indicator appears where it belongs straight away. The caret is asked for
/// by [`Indicator::begin`], before the host tells the tray, the state file
/// and the API that it is listening, so the answer, which takes a
/// millisecond or two, is usually there when the frame is drawn and nothing
/// is waited for. This is the most a press pays over a program that is slow
/// or hangs; for such a one the frame is left to the
/// next redraw, which draws under the caret if it is known by then and at
/// the bottom of the window if not.
const FIRST_WAIT: Duration = Duration::from_millis(5);
/// How much larger the screen indicator is than the one under the caret.
const SCREEN_SIZE_FACTOR: f32 = 2.0;
/// Gap between the screen indicator and the taskbar, in 96-DPI pixels.
const SCREEN_GAP: f32 = 48.0;
/// Gap between the caret and the indicator, in 96-DPI pixels.
const CARET_GAP: f32 = 6.0;
/// Gap between the indicator and the bottom of a window whose caret is not
/// known, in 96-DPI pixels.
const WINDOW_GAP: f32 = 28.0;
/// The closest the indicator comes to the edge of the work area, in 96-DPI
/// pixels.
const EDGE: f32 = 8.0;
/// How far the indicator travels while it fades in or out, in 96-DPI pixels.
const SLIDE: f32 = 4.0;
/// How many redraws the indicator takes to fade in.
const APPEAR_STEPS: f32 = 4.0;

/// The timer that runs while the indicator is finishing or leaving.
const TIMER_ID: usize = 1;
/// How often the indicator is redrawn while finishing, in milliseconds.
const FINISH_MS: u32 = 33;
/// How many of those redraws the glow takes to cross the indicator once.
const SWEEP_STEPS: f32 = 34.0;
/// How many of those redraws the indicator stays at most if nobody hides it:
/// about half a minute.
const FINISH_LIMIT: u32 = 900;
/// How often the indicator is redrawn while it fades out, in milliseconds,
/// and how many redraws that takes.
const LEAVE_MS: u32 = 16;
const LEAVE_STEPS: f32 = 7.0;
/// How many windows above the indicator are looked at, at most, to find out
/// whether the one being dictated into is among them. The indicator belongs
/// at the very top, so one that has not sunk has only a handful above it.
const MOST_ABOVE: usize = 256;

/// Where an indicator sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Placement {
    /// Directly under the text caret, following it.
    Caret,
    /// Centered at the bottom of the screen the user is working on, and larger.
    Screen,
}

/// What the indicator is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Hidden,
    /// Following the voice, redrawn by the host.
    Listening,
    /// Glowing while the last words are typed, for this many redraws so far.
    Finishing(u32),
    /// Fading out.
    Leaving,
}

/// The listening indicator. It is created hidden.
pub(super) struct Indicator {
    /// Lives on the heap, where the window's timer finds it as well. It is
    /// only ever touched from the thread that created it, by one of the
    /// methods below or by [`on_timer`], and never by both at once.
    state: NonNull<State>,
}

impl Indicator {
    pub(super) fn new(placement: Placement) -> Result<Self> {
        let state = Box::new(State::new(placement)?);
        let hwnd = state.window.0;
        let state = NonNull::from(Box::leak(state));
        // SAFETY: `hwnd` is the live window `state` owns. The pointer stored
        // with it stays valid until `drop`, which destroys the window, and
        // with it the timer that reads the pointer, before freeing the state.
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, state.as_ptr() as isize) };
        Ok(Self { state })
    }

    /// Gets ready to listen without drawing anything yet: the caret is
    /// asked for, which takes the program in the foreground a moment and
    /// this call none. [`Indicator::update`] draws the first frame.
    pub(super) fn begin(&mut self) {
        self.state().begin();
    }

    /// Feeds the latest loudness and redraws, showing the window if needed.
    /// The first frame is already clearly there: the fade starts fast.
    pub(super) fn update(&mut self, level: f32) {
        self.state().listen(level);
    }

    /// Shows that listening is over but the last words are still on their
    /// way: the bars settle and a glow travels along them, until
    /// [`Indicator::hide`] or [`Indicator::update`] is called. An indicator
    /// that was not showing stays hidden.
    pub(super) fn finish(&mut self) {
        self.state().finish();
    }

    /// Fades the indicator out.
    pub(super) fn hide(&mut self) {
        self.state().leave();
    }

    fn state(&mut self) -> &mut State {
        // SAFETY: the state was leaked from a `Box` in `new` and is freed
        // only in `drop`. Nothing else holds a reference to it now: the
        // timer procedure only runs when this thread dispatches messages,
        // which no method of the indicator does.
        unsafe { self.state.as_mut() }
    }
}

impl Drop for Indicator {
    fn drop(&mut self) {
        // SAFETY: the pointer came from `Box::leak` in `new` and is not used
        // again. Dropping the state destroys the window, which ends its timer.
        drop(unsafe { Box::from_raw(self.state.as_ptr()) });
    }
}

/// Redraws an indicator that is finishing or leaving.
unsafe extern "system" fn on_timer(hwnd: HWND, _message: u32, _id: usize, _time: u32) {
    // SAFETY: the window's user data is null or the pointer `Indicator::new`
    // stored, which outlives the window. Timer procedures run from this
    // thread's message loop, where no method of the indicator is running, so
    // this is the only reference to the state.
    unsafe {
        let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
        if let Some(state) = state.as_mut() {
            state.on_timer();
        }
    }
}

/// Everything an indicator has. Fields are dropped in this order: the window
/// first, so that nothing draws once the bitmap is gone.
struct State {
    window: Window,
    surface: Surface,
    artwork: Artwork,
    accent: Rgb,
    placement: Placement,
    /// Scale of the monitor the indicator was last on: 1.0 at 96 DPI.
    display_scale: f32,
    levels: Levels,
    phase: Phase,
    /// How far the indicator has faded in, from 0.0 to 1.0.
    shown: f32,
    /// Where the capsule goes on screen, and from which side it slides in.
    spot: Spot,
    /// Whether `spot` has been worked out since listening last began.
    placed: bool,
    /// Looks for the caret while the indicator follows it, and at no other
    /// time.
    watch: Option<caret::Watch>,
    /// Whether the window is showing, at whatever opacity.
    visible: bool,
    redraws: u32,
}

impl State {
    fn new(placement: Placement) -> Result<Self> {
        let accent = accent();
        // The right scale is not known until there is a caret to go to; the
        // main screen's is the likeliest.
        let display_scale = monitor_of_window(null_mut()).map_or(1.0, |monitor| monitor.scale);
        let artwork = Artwork::new(scale_for(placement, display_scale), accent);
        let (width, height) = artwork.size();
        Ok(Self {
            window: Window::new()?,
            surface: Surface::new(width, height)?,
            artwork,
            accent,
            placement,
            display_scale,
            levels: Levels::default(),
            phase: Phase::Hidden,
            shown: 0.0,
            spot: Spot::default(),
            placed: false,
            watch: None,
            visible: false,
            redraws: 0,
        })
    }

    /// Turns to listening, if the indicator is not listening already, and
    /// has the caret looked for. Nothing is drawn.
    fn begin(&mut self) {
        if self.phase != Phase::Listening {
            self.stop_timer();
            if self.phase == Phase::Hidden {
                self.appear();
            }
            self.phase = Phase::Listening;
            self.redraws = 0;
            self.placed = false;
            if self.placement == Placement::Caret {
                // Without a thread the indicator goes by the window alone.
                self.watch = caret::Watch::start();
            }
            if let Some(watch) = &self.watch {
                watch.ask();
            }
        }
    }

    fn listen(&mut self, level: f32) {
        self.begin();
        self.levels.push(level);
        let first = self.redraws == 0;
        if self.redraws.is_multiple_of(REPOSITION_EVERY) {
            self.look();
        }
        self.redraws = self.redraws.wrapping_add(1);
        if !self.follow(first) {
            return;
        }
        self.shown = (self.shown + 1.0 / APPEAR_STEPS).min(1.0);
        self.draw(&draw::listening(&self.levels.bars()));
    }

    fn finish(&mut self) {
        if matches!(self.phase, Phase::Listening | Phase::Leaving) {
            self.phase = Phase::Finishing(0);
            self.watch = None;
            self.start_timer(FINISH_MS);
        }
    }

    fn leave(&mut self) {
        if matches!(self.phase, Phase::Listening | Phase::Finishing(_)) {
            self.phase = Phase::Leaving;
            self.watch = None;
            self.start_timer(LEAVE_MS);
        }
    }

    fn on_timer(&mut self) {
        match self.phase {
            Phase::Finishing(steps) if steps < FINISH_LIMIT => {
                self.phase = Phase::Finishing(steps + 1);
                self.levels.push(0.0);
                self.shown = (self.shown + 1.0 / APPEAR_STEPS).min(1.0);
                let sweep = steps as f32 / SWEEP_STEPS;
                self.draw(&draw::finishing(&self.levels.bars(), sweep));
            }
            Phase::Finishing(_) => self.leave(),
            Phase::Leaving => {
                self.shown -= 1.0 / LEAVE_STEPS;
                if self.shown > 0.0 {
                    // The picture stays as it is; only its opacity changes.
                    self.present();
                } else {
                    self.conceal();
                }
            }
            Phase::Hidden | Phase::Listening => self.stop_timer(),
        }
    }

    /// Gets ready to be shown again: nothing is left of the last time, and
    /// the accent color is the one Windows has now.
    fn appear(&mut self) {
        self.levels = Levels::default();
        self.shown = 0.0;
        let accent = accent();
        if accent != self.accent {
            self.accent = accent;
            self.artwork = Artwork::new(scale_for(self.placement, self.display_scale), accent);
        }
    }

    /// Hides the window and stops everything.
    fn conceal(&mut self) {
        self.stop_timer();
        self.watch = None;
        if self.visible {
            // SAFETY: the window is live.
            unsafe { ShowWindow(self.window.0, SW_HIDE) };
        }
        self.visible = false;
        self.shown = 0.0;
        self.phase = Phase::Hidden;
    }

    fn start_timer(&mut self, milliseconds: u32) {
        // SAFETY: the window is live. A timer with this id that is already
        // running is replaced, so there is never more than one.
        unsafe { SetTimer(self.window.0, TIMER_ID, milliseconds, Some(on_timer)) };
    }

    fn stop_timer(&mut self) {
        // SAFETY: the window is live; stopping a timer that is not running
        // does nothing.
        unsafe { KillTimer(self.window.0, TIMER_ID) };
    }

    /// Paints a frame and puts it on screen.
    fn draw(&mut self, bars: &[Bar; INDICATOR_BARS]) {
        self.artwork.paint(bars, self.surface.pixels());
        self.present();
    }

    /// Puts the picture on screen where it belongs, as opaque as it has
    /// faded in so far, and shows the window if it is hidden.
    fn present(&mut self) {
        let eased = opacity(self.shown);
        let slide = ((1.0 - eased) * SLIDE * self.display_scale).round() as i32;
        let (left, top, _, _) = self.artwork.capsule();
        let origin = POINT {
            x: self.spot.x - left,
            y: self.spot.y - top + slide * self.spot.enters_from,
        };
        let size = SIZE {
            cx: self.surface.width.cast_signed(),
            cy: self.surface.height.cast_signed(),
        };
        let source = POINT::default();
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: (eased * 255.0).round() as u8,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        // SAFETY: the window and the surface's device context are live, the
        // surface holds a bitmap of exactly `size`, and every pointer refers
        // to a local that outlives the call.
        unsafe {
            UpdateLayeredWindow(
                self.window.0,
                null_mut(),
                &raw const origin,
                &raw const size,
                self.surface.dc,
                &raw const source,
                0,
                &raw const blend,
                ULW_ALPHA,
            );
        }
        if !self.visible {
            self.window.show();
        }
        self.visible = true;
    }

    /// Puts an indicator that is showing back on top, if the window in the
    /// foreground has got above it. A hidden one is put on top when shown.
    fn surface_again(&self) {
        // SAFETY: no arguments.
        let foreground = unsafe { GetForegroundWindow() };
        if self.visible && self.window.is_under(foreground) {
            self.window.show();
        }
    }

    /// Finds out again where the indicator goes, as far as that takes no
    /// waiting. A screen indicator sits centered above the taskbar of the
    /// screen the user is working on, which Windows knows. One at the caret
    /// has the caret looked for, and [`State::follow`] takes the answer when
    /// it is there; without a thread to look, it goes by the window.
    fn look(&mut self) {
        self.surface_again();
        if self.placement == Placement::Screen {
            // SAFETY: no arguments.
            let monitor = monitor_of_window(unsafe { GetForegroundWindow() });
            self.place(monitor, None);
        } else if let Some(watch) = &self.watch {
            watch.ask();
        } else if let Some(anchor) = caret::window() {
            self.go_to(anchor);
        }
    }

    /// Moves the indicator to where the caret has been found since the last
    /// redraw, if it has. Returns whether there is a frame to draw now:
    /// there is none only for the `first` frame of a session, when the
    /// program asked has not said where its caret is within
    /// [`FIRST_WAIT`]. The next redraw then draws at the bottom of the
    /// window, and the indicator moves to the caret when the answer comes.
    fn follow(&mut self, first: bool) -> bool {
        let Some(watch) = &self.watch else {
            return true;
        };
        let unanswered = first && !self.placed && !watch.wait(FIRST_WAIT);
        match watch.take() {
            Some(anchor) => self.go_to(anchor),
            None if self.placed => {}
            None if unanswered => return false,
            None => {
                if let Some(anchor) = caret::window() {
                    self.go_to(anchor);
                }
            }
        }
        true
    }

    /// Puts the indicator under the caret, or at the bottom of the
    /// foreground window when the app does not say where its caret is.
    fn go_to(&mut self, anchor: Anchor) {
        let (monitor, anchor) = match anchor {
            Anchor::Caret(rect) => (monitor_at(rect.left, rect.bottom), (rect_of(&rect), true)),
            Anchor::Window(rect) => (
                monitor_at(rect.left + (rect.right - rect.left) / 2, rect.bottom),
                (rect_of(&rect), false),
            ),
        };
        self.place(monitor, Some(anchor));
    }

    /// Works out the spot on `monitor`: beside the caret or at the bottom
    /// of the window given, or else centered above the taskbar. Where the
    /// monitor is not known the indicator stays where it was.
    fn place(&mut self, monitor: Option<Monitor>, anchor: Option<(Rect, bool)>) {
        let Some(monitor) = monitor else {
            return;
        };
        self.rescale(monitor.scale);
        let (_, _, width, height) = self.artwork.capsule();
        let size = (width, height);
        let px = |value: f32| (value * self.display_scale).round() as i32;
        self.spot = match anchor {
            Some((caret, true)) => {
                Spot::beside_caret(&caret, size, &monitor.work, px(CARET_GAP), px(EDGE))
            }
            Some((window, false)) => {
                Spot::at_bottom(&window, size, &monitor.work, px(WINDOW_GAP), px(EDGE))
            }
            None => Spot::at_bottom(&monitor.work, size, &monitor.work, px(SCREEN_GAP), px(EDGE)),
        };
        self.placed = true;
    }

    /// Redraws the artwork for a monitor of another scale. If Windows cannot
    /// give a bitmap of the new size, the indicator stays as it was.
    fn rescale(&mut self, display_scale: f32) {
        if (display_scale - self.display_scale).abs() < 0.01 {
            return;
        }
        let artwork = Artwork::new(scale_for(self.placement, display_scale), self.accent);
        let (width, height) = artwork.size();
        if let Ok(surface) = Surface::new(width, height) {
            self.surface = surface;
            self.artwork = artwork;
            self.display_scale = display_scale;
        }
    }
}

/// How opaque the indicator is, and how far it has slid into place, when it
/// has faded in to `shown`: fast at first and settling at the end, as things
/// move in Windows. That is also what makes the first frame count: a
/// quarter of the way in, the indicator is already more than half there.
fn opacity(shown: f32) -> f32 {
    1.0 - (1.0 - shown.clamp(0.0, 1.0)).powi(3)
}

/// The scale an indicator is drawn at on a monitor of the given scale.
fn scale_for(placement: Placement, display_scale: f32) -> f32 {
    match placement {
        Placement::Caret => display_scale,
        Placement::Screen => display_scale * SCREEN_SIZE_FACTOR,
    }
}

/// A rectangle on screen: left, top, right, bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

fn rect_of(rect: &::windows::Win32::Foundation::RECT) -> Rect {
    Rect {
        left: rect.left,
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
    }
}

/// Where the capsule goes on screen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Spot {
    x: i32,
    y: i32,
    /// The side the capsule slides in from: -1 from above, 1 from below.
    enters_from: i32,
}

impl Spot {
    /// Just under the caret and starting where it is, or just above it if
    /// there is no room below. Always at least `edge` inside `work`, the
    /// part of the monitor that is free, as far as its size allows.
    fn beside_caret(
        caret: &Rect,
        (width, height): (i32, i32),
        work: &Rect,
        gap: i32,
        edge: i32,
    ) -> Self {
        let below = caret.bottom + gap;
        let above = caret.top - gap - height;
        let fits_below = below + height <= work.bottom - edge;
        let fits_above = above >= work.top + edge;
        let (y, enters_from) = if fits_below || !fits_above {
            (below, -1)
        } else {
            (above, 1)
        };
        Self {
            x: within(caret.left, work.left + edge, work.right - edge - width),
            y: within(y, work.top + edge, work.bottom - edge - height),
            enters_from,
        }
    }

    /// Centered in `area` and `gap` above its bottom, kept inside `work`.
    fn at_bottom(
        area: &Rect,
        (width, height): (i32, i32),
        work: &Rect,
        gap: i32,
        edge: i32,
    ) -> Self {
        let x = area.left + (area.right - area.left - width) / 2;
        let y = area.bottom - gap - height;
        Self {
            x: within(x, work.left + edge, work.right - edge - width),
            y: within(y, work.top + edge, work.bottom - edge - height),
            enters_from: 1,
        }
    }
}

/// `value` kept between `low` and `high`; `low` wins if there is no room.
fn within(value: i32, low: i32, high: i32) -> i32 {
    value.min(high).max(low)
}

/// What the indicator needs to know about a monitor.
#[derive(Debug, Clone, Copy)]
struct Monitor {
    /// The part of it that the taskbar leaves free.
    work: Rect,
    /// 1.0 at 96 DPI.
    scale: f32,
}

/// The monitor the point is on, or the nearest one.
fn monitor_at(x: i32, y: i32) -> Option<Monitor> {
    // SAFETY: plain value arguments.
    monitor(unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) })
}

/// The monitor most of the window is on, or the main one if `hwnd` is null
/// or no longer a window.
fn monitor_of_window(hwnd: HWND) -> Option<Monitor> {
    // SAFETY: any value is accepted for the window; one that is no window
    // gives the main monitor.
    monitor(unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY) })
}

fn monitor(handle: HMONITOR) -> Option<Monitor> {
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let (mut dpi, mut dpi_y) = (96u32, 96u32);
    // SAFETY: `info` has its size set and all three out-pointers refer to
    // live locals. A monitor that has just been unplugged fails both calls.
    unsafe {
        if GetMonitorInfoW(handle, &raw mut info) == 0 {
            return None;
        }
        if GetDpiForMonitor(handle, MDT_EFFECTIVE_DPI, &raw mut dpi, &raw mut dpi_y) != 0 {
            dpi = 96;
        }
    }
    let RECT {
        left,
        top,
        right,
        bottom,
    } = info.rcWork;
    Some(Monitor {
        work: Rect {
            left,
            top,
            right,
            bottom,
        },
        scale: dpi.clamp(96, 960) as f32 / 96.0,
    })
}

/// The user's accent color in the light shade Windows uses on dark surfaces.
fn accent() -> Rgb {
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent");
    let value = wide("AccentPalette");
    // Eight colors of four bytes each, from the lightest shade to the darkest.
    let mut palette = [0u8; 32];
    let mut size = palette.len() as u32;
    // SAFETY: both names are NUL-terminated, and `size` tells Windows how
    // many bytes `palette` has room for.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_BINARY,
            null_mut(),
            palette.as_mut_ptr().cast(),
            &raw mut size,
        )
    };
    if status == 0 && size as usize == palette.len() {
        // The second lightest.
        (palette[4], palette[5], palette[6])
    } else {
        draw::DEFAULT_ACCENT
    }
}

/// The indicator's window, destroyed when dropped.
struct Window(HWND);

impl Window {
    fn new() -> Result<Self> {
        let class = wide("STATIC");
        // SAFETY: the class name is NUL-terminated. The styles make the window
        // per-pixel transparent, always on top, invisible to the mouse and unable
        // to take focus, so it never disturbs the app being dictated into. It
        // gets its size and place when it is first drawn.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_LAYERED
                    | WS_EX_TRANSPARENT
                    | WS_EX_TOPMOST
                    | WS_EX_NOACTIVATE
                    | WS_EX_TOOLWINDOW,
                class.as_ptr(),
                null(),
                WS_POPUP,
                0,
                0,
                0,
                0,
                null_mut(),
                null_mut(),
                null_mut(),
                null(),
            )
        };
        if hwnd.is_null() {
            return Err(last_error("creating the indicator window"));
        }
        Ok(Self(hwnd))
    }

    /// Shows the window above every other one, topmost ones included,
    /// without giving it the focus. `ShowWindow` would show it wherever it
    /// is among the other windows, which for a window that has sunk is
    /// behind the one being dictated into.
    fn show(&self) {
        // SAFETY: the window is live; neither its place nor its size, which
        // `UpdateLayeredWindow` sets, is touched.
        unsafe {
            SetWindowPos(
                self.0,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
    }

    /// Whether `other` is above this window, and so covers it where the two
    /// overlap. No window, or this one itself, is not.
    fn is_under(&self, other: HWND) -> bool {
        if other.is_null() || other == self.0 {
            return false;
        }
        let mut above = self.0;
        for _ in 0..MOST_ABOVE {
            // SAFETY: a plain query; a window that has gone gives null.
            above = unsafe { GetWindow(above, GW_HWNDPREV) };
            if above.is_null() {
                return false;
            }
            if above == other {
                return true;
            }
        }
        false
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: the window was created in `new` and is destroyed only here.
        unsafe { DestroyWindow(self.0) };
    }
}

/// A bitmap with per-pixel transparency and the device context a layered
/// window is drawn from. Both are released when it is dropped.
struct Surface {
    dc: HDC,
    bitmap: HBITMAP,
    /// The bitmap the device context came with, put back before it is deleted.
    original: HGDIOBJ,
    /// The bitmap's pixel memory: `width * height` premultiplied ARGB values.
    bits: NonNull<u32>,
    width: u32,
    height: u32,
}

impl Surface {
    fn new(width: u32, height: u32) -> Result<Self> {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width.cast_signed(),
                // Negative height makes the rows run top to bottom.
                biHeight: -height.cast_signed(),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = null_mut();
        // SAFETY: `info` describes a 32-bit bitmap; on success `bits` points at
        // `width * height` pixels owned by `bitmap` until it is deleted in `Drop`.
        let (dc, bitmap) = unsafe {
            let dc = CreateCompatibleDC(null_mut());
            let bitmap = CreateDIBSection(
                dc,
                &raw const info,
                DIB_RGB_COLORS,
                &raw mut bits,
                null_mut(),
                0,
            );
            (dc, bitmap)
        };
        let Some(bits) = NonNull::new(bits.cast::<u32>()).filter(|_| !dc.is_null()) else {
            let error = last_error("creating the indicator bitmap");
            // SAFETY: each handle is released once; null handles are ignored by GDI.
            unsafe {
                DeleteObject(bitmap);
                DeleteDC(dc);
            }
            return Err(error);
        };
        // SAFETY: both handles are live.
        let original = unsafe { SelectObject(dc, bitmap) };
        Ok(Self {
            dc,
            bitmap,
            original,
            bits,
            width,
            height,
        })
    }

    /// The pixels, row by row from the top.
    fn pixels(&mut self) -> &mut [u32] {
        // SAFETY: `bits` points at `width * height` pixels that live as long
        // as the bitmap, which is as long as `self`; borrowing `self` mutably
        // makes this the only reference to them.
        unsafe {
            std::slice::from_raw_parts_mut(self.bits.as_ptr(), (self.width * self.height) as usize)
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        // SAFETY: each handle was created in `new` and is released exactly
        // once; the bitmap is taken out of the device context first, as GDI
        // asks.
        unsafe {
            SelectObject(self.dc, self.original);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}

#[cfg(test)]
mod tests {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GWL_EXSTYLE, IsWindowVisible};

    use super::*;

    const WORK: Rect = Rect {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1040,
    };
    const SIZE: (i32, i32) = (51, 21);

    fn caret(left: i32, top: i32) -> Rect {
        Rect {
            left,
            top,
            right: left + 1,
            bottom: top + 18,
        }
    }

    #[test]
    fn sits_under_the_caret_where_there_is_room() {
        let spot = Spot::beside_caret(&caret(400, 300), SIZE, &WORK, 6, 8);
        assert_eq!((spot.x, spot.y, spot.enters_from), (400, 324, -1));
    }

    #[test]
    fn flips_above_the_caret_at_the_bottom_of_the_screen() {
        let spot = Spot::beside_caret(&caret(400, 1010), SIZE, &WORK, 6, 8);
        assert_eq!((spot.x, spot.y, spot.enters_from), (400, 1010 - 6 - 21, 1));
    }

    #[test]
    fn never_leaves_the_work_area() {
        let inside = |spot: Spot| {
            spot.x >= WORK.left
                && spot.y >= WORK.top
                && spot.x + SIZE.0 <= WORK.right
                && spot.y + SIZE.1 <= WORK.bottom
        };
        for (left, top) in [
            (-500, -500),
            (1915, 500),
            (5000, 5000),
            (-3, 1030),
            (1919, 0),
            (900, 1039),
        ] {
            let spot = Spot::beside_caret(&caret(left, top), SIZE, &WORK, 6, 8);
            assert!(inside(spot), "caret at {left}, {top}: {spot:?}");
        }
        // A window that hangs off the screen, as a maximized one does.
        let window = Rect {
            left: 1500,
            top: -8,
            right: 2600,
            bottom: 1048,
        };
        assert!(inside(Spot::at_bottom(&window, SIZE, &WORK, 28, 8)));
    }

    #[test]
    fn works_on_a_monitor_left_of_and_above_the_main_one() {
        let work = Rect {
            left: -2560,
            top: -1440,
            right: 0,
            bottom: -48,
        };
        let spot = Spot::beside_caret(&caret(-9, -80), SIZE, &work, 6, 8);
        assert_eq!((spot.x, spot.y, spot.enters_from), (-59, -107, 1));
    }

    #[test]
    fn a_work_area_too_small_keeps_the_top_left_corner_in_view() {
        let work = Rect {
            left: 100,
            top: 100,
            right: 120,
            bottom: 110,
        };
        let spot = Spot::beside_caret(&caret(0, 0), SIZE, &work, 6, 8);
        assert_eq!((spot.x, spot.y), (108, 108));
    }

    #[test]
    fn the_screen_indicator_is_centered_above_the_taskbar() {
        let spot = Spot::at_bottom(&WORK, (102, 42), &WORK, 48, 8);
        assert_eq!((spot.x, spot.y, spot.enters_from), (909, 950, 1));
    }

    #[test]
    fn the_first_frame_is_clearly_visible() {
        // The frame drawn on the key press is the only feedback until the
        // next tick: it must not be a faint one.
        let first = opacity(1.0 / APPEAR_STEPS);
        assert!(first > 0.5, "{first}");
        // The fade still runs its course from nothing to all there.
        assert!(opacity(0.0).abs() < f32::EPSILON);
        assert!((opacity(1.0) - 1.0).abs() < f32::EPSILON);
        let mut last = 0.0;
        for step in 1..=4u8 {
            let now = opacity(f32::from(step) / APPEAR_STEPS);
            assert!(now > last);
            last = now;
        }
    }

    /// A window like any program's: hidden, and not topmost.
    fn plain_window() -> Result<Window> {
        let class = wide("STATIC");
        // SAFETY: the class name is NUL-terminated; the window is never shown.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                null(),
                WS_POPUP,
                0,
                0,
                0,
                0,
                null_mut(),
                null_mut(),
                null_mut(),
                null(),
            )
        };
        if hwnd.is_null() {
            return Err(last_error("creating a window to test with"));
        }
        Ok(Window(hwnd))
    }

    fn is_topmost(window: &Window) -> bool {
        // SAFETY: the window is live.
        let styles = unsafe { GetWindowLongPtrW(window.0, GWL_EXSTYLE) };
        styles as u32 & WS_EX_TOPMOST != 0
    }

    #[test]
    fn an_indicator_that_sank_behind_another_window_is_shown_above_it() -> Result<()> {
        // Nothing of this is seen: neither window has a size, and a layered
        // window that was never drawn into shows nothing.
        let other = plain_window()?;
        let indicator = Window::new()?;
        assert!(is_topmost(&indicator) && !indicator.is_under(other.0));
        // It sinks behind the window of another program.
        // SAFETY: both windows are live; only the order of the two changes.
        unsafe {
            SetWindowPos(
                indicator.0,
                other.0,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
        assert!(indicator.is_under(other.0));
        // SAFETY: no arguments.
        let foreground = unsafe { GetForegroundWindow() };
        indicator.show();
        // Shown, it is above that window again, and topmost, and the
        // program being dictated into still has the keyboard.
        assert!(!indicator.is_under(other.0));
        assert!(is_topmost(&indicator));
        // SAFETY: plain queries on a live window.
        unsafe {
            assert_ne!(IsWindowVisible(indicator.0), 0);
            assert_eq!(GetForegroundWindow(), foreground);
        }
        // Whatever is above it now, it is not that window, however often
        // it is asked; and no window at all covers nothing.
        assert!(!indicator.is_under(null_mut()));
        assert!(!indicator.is_under(indicator.0));
        assert!(other.is_under(indicator.0));
        Ok(())
    }

    #[test]
    fn the_screen_indicator_is_larger() {
        assert!(scale_for(Placement::Screen, 1.5) > scale_for(Placement::Caret, 1.5));
    }
}
