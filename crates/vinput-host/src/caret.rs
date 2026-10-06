//! Finding where the text caret is on screen.
//!
//! Most programs only say where their caret is when asked through
//! accessibility, and that question is answered by the program itself: one
//! that hangs never answers. So the thread with the message loop does not
//! ask. A [`Watch`] asks on a thread of its own, which exists only while an
//! indicator follows the caret, and the indicator goes by the last answer.
//! With no indicator showing there is no thread and nothing wakes up.
#![allow(unsafe_code)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "Win32 structure sizes, flags and coordinates are small values in fixed-width fields"
)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{AccessibleObjectFromWindow, IAccessible};
use windows::Win32::UI::WindowsAndMessaging::{
    GUITHREADINFO, GetForegroundWindow, GetGUIThreadInfo, GetWindowRect, GetWindowThreadProcessId,
    OBJID_CARET,
};
use windows::core::Interface;

/// Where to place the indicator, in screen pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Anchor {
    /// The caret itself was found.
    Caret(RECT),
    /// Only the foreground window is known; the app does not expose its caret.
    Window(RECT),
}

/// The most threads that may be looking for a caret, or be stuck doing so,
/// at one time. A program that hangs keeps the thread that asked it, and a
/// new session gets a new thread; past this many the indicator does without
/// the caret until one of them has come back.
const MOST_LOOKUPS: usize = 4;

/// How many threads are looking for a caret, or stuck doing so, right now.
static LOOKUPS: AtomicUsize = AtomicUsize::new(0);

/// Looks for the caret on a thread of its own, for as long as it lives.
///
/// [`Watch::ask`] has the caret looked for and returns at once;
/// [`Watch::take`] gives the place that was found since it was last taken.
/// However long the program asked takes to answer, neither waits for it.
/// Asking while a lookup is under way has one more made after it, however
/// often it was asked. Dropping the watch lets go of the thread without
/// waiting for it: it ends as soon as the lookup it is in returns, which
/// for a program that hangs is when that program answers or is closed.
pub(super) struct Watch {
    shared: Arc<Shared>,
}

/// What a [`Watch`] and its thread share.
#[derive(Default)]
struct Shared {
    slot: Mutex<Slot>,
    /// Signalled when anything in the slot changes.
    changed: Condvar,
}

#[derive(Default)]
struct Slot {
    /// Whether a lookup has been asked for and not begun.
    asked: bool,
    /// The place found last, until it is taken.
    found: Option<Anchor>,
    /// Whether any lookup has come back, with a place or without.
    answered: bool,
    /// Whether the watch has gone, and the thread is to end.
    closed: bool,
}

impl Shared {
    fn slot(&self) -> MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// One thread among those counted, for as long as this lives.
struct Counted(&'static AtomicUsize);

impl Drop for Counted {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// COM on the thread that asks programs for their caret, for as long as
/// this lives. A thread that only makes calls and has no windows belongs
/// in the multithreaded apartment, where it owes nobody a message loop.
struct Com(bool);

impl Com {
    fn enter() -> Self {
        // SAFETY: a plain call; it is paired with `CoUninitialize` in `drop`
        // if and only if it succeeded.
        Self(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: pairs with the successful `CoInitializeEx` in `enter`,
            // on the same thread: the lookup that owns this never leaves it.
            unsafe { CoUninitialize() };
        }
    }
}

impl Watch {
    /// Starts a thread that looks for the caret when asked. `None` if
    /// Windows gives no thread, or too many earlier ones are still stuck in
    /// programs that do not answer.
    pub(super) fn start() -> Option<Self> {
        // COM is entered on the thread itself, at its first lookup, and
        // left when the thread ends and drops what it was given.
        let mut com = None;
        Self::with(&LOOKUPS, move || {
            com.get_or_insert_with(Com::enter);
            locate()
        })
    }

    /// A watch that looks with `lookup`, on a thread counted in `live`.
    fn with(
        live: &'static AtomicUsize,
        lookup: impl FnMut() -> Option<Anchor> + Send + 'static,
    ) -> Option<Self> {
        let earlier = live.fetch_add(1, Ordering::AcqRel);
        let counted = Counted(live);
        if earlier >= MOST_LOOKUPS {
            return None;
        }
        let shared = Arc::new(Shared::default());
        let theirs = Arc::clone(&shared);
        thread::Builder::new()
            .name("vinput-caret".into())
            .spawn(move || {
                // Dropped last: the thread counts until all it had is gone.
                let _counted = counted;
                work(&theirs, lookup);
            })
            .ok()?;
        Some(Self { shared })
    }

    /// Has the caret looked for. Returns at once, whatever the thread is
    /// doing.
    pub(super) fn ask(&self) {
        self.shared.slot().asked = true;
        self.shared.changed.notify_all();
    }

    /// The place found since this was last called, if there is one.
    pub(super) fn take(&self) -> Option<Anchor> {
        self.shared.slot().found.take()
    }

    /// Waits until a lookup has come back, but no longer than `limit`.
    /// Returns whether one has, since the watch was started.
    pub(super) fn wait(&self, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        let mut slot = self.shared.slot();
        while !slot.answered {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            slot = self
                .shared
                .changed
                .wait_timeout(slot, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        slot.answered
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        // The thread is not waited for: it may be inside a program that
        // does not answer.
        self.shared.slot().closed = true;
        self.shared.changed.notify_all();
    }
}

/// What the thread of a [`Watch`] does: sleeps until it is asked, looks,
/// leaves what it found, and ends when the watch has gone.
fn work(shared: &Shared, mut lookup: impl FnMut() -> Option<Anchor>) {
    let mut slot = shared.slot();
    loop {
        while !slot.asked && !slot.closed {
            slot = shared
                .changed
                .wait(slot)
                .unwrap_or_else(PoisonError::into_inner);
        }
        if slot.closed {
            return;
        }
        slot.asked = false;
        drop(slot);
        // Nothing is held here: this is where a program may keep the thread.
        let found = lookup();
        slot = shared.slot();
        // Where nothing was found, the place found before is still the best.
        if found.is_some() {
            slot.found = found;
        }
        slot.answered = true;
        shared.changed.notify_all();
    }
}

/// Where the foreground window is, which Windows itself knows: unlike
/// [`locate`], this never waits for the program the window belongs to.
pub(super) fn window() -> Option<Anchor> {
    let mut window = RECT::default();
    // SAFETY: plain queries on a window handle the system just gave us; the
    // out-pointer refers to a live local.
    unsafe {
        let foreground = GetForegroundWindow();
        if foreground.is_invalid() {
            return None;
        }
        GetWindowRect(foreground, &raw mut window).ok()?;
    }
    Some(Anchor::Window(window))
}

/// Locates the caret of the focused control, falling back to the foreground window.
///
/// The program in the foreground is asked, and this returns when it has
/// answered: it is for the thread of a [`Watch`], never for the thread with
/// the message loop.
fn locate() -> Option<Anchor> {
    // SAFETY: plain queries on window handles the system just gave us; every
    // out-pointer refers to a live local.
    unsafe {
        let foreground = GetForegroundWindow();
        if foreground.is_invalid() {
            return None;
        }
        let thread = GetWindowThreadProcessId(foreground, None);
        let mut info = GUITHREADINFO {
            cbSize: size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        let have_info = GetGUIThreadInfo(thread, &raw mut info).is_ok();

        // Classic Win32 controls report their caret directly.
        if have_info
            && !info.hwndCaret.is_invalid()
            && !is_empty(&info.rcCaret)
            && let Some(rect) = client_to_screen(info.hwndCaret, info.rcCaret)
        {
            return Some(Anchor::Caret(rect));
        }

        // Browsers and most modern apps report it through accessibility instead.
        let focus = if have_info && !info.hwndFocus.is_invalid() {
            info.hwndFocus
        } else {
            foreground
        };
        if let Some(rect) = accessible_caret(focus) {
            return Some(Anchor::Caret(rect));
        }

        let mut window = RECT::default();
        GetWindowRect(foreground, &raw mut window).ok()?;
        Some(Anchor::Window(window))
    }
}

fn is_empty(rect: &RECT) -> bool {
    rect.right <= rect.left && rect.bottom <= rect.top
}

unsafe fn client_to_screen(hwnd: HWND, rect: RECT) -> Option<RECT> {
    let mut top_left = POINT {
        x: rect.left,
        y: rect.top,
    };
    let mut bottom_right = POINT {
        x: rect.right,
        y: rect.bottom,
    };
    // SAFETY: the caller passes a window handle from the system; the points are locals.
    let converted = unsafe {
        ClientToScreen(hwnd, &raw mut top_left).as_bool()
            && ClientToScreen(hwnd, &raw mut bottom_right).as_bool()
    };
    converted.then_some(RECT {
        left: top_left.x,
        top: top_left.y,
        right: bottom_right.x,
        bottom: bottom_right.y,
    })
}

unsafe fn accessible_caret(hwnd: HWND) -> Option<RECT> {
    let mut raw = std::ptr::null_mut();
    // SAFETY: on success `raw` holds an owned `IAccessible` pointer, which
    // `from_raw` takes ownership of; `accLocation` writes to live locals.
    unsafe {
        AccessibleObjectFromWindow(hwnd, OBJID_CARET.0 as u32, &IAccessible::IID, &raw mut raw)
            .ok()?;
        if raw.is_null() {
            return None;
        }
        let caret = IAccessible::from_raw(raw);
        let (mut x, mut y, mut width, mut height) = (0, 0, 0, 0);
        caret
            .accLocation(
                &raw mut x,
                &raw mut y,
                &raw mut width,
                &raw mut height,
                &VARIANT::from(0),
            )
            .ok()?;
        // Apps without a caret answer with an all-zero rectangle.
        (height > 0 && (x != 0 || y != 0)).then_some(RECT {
            left: x,
            top: y,
            right: x + width,
            bottom: y + height,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    /// Long enough for anything that is going to happen on this PC.
    const WAIT: Duration = Duration::from_secs(10);
    /// Long enough to see that something which must not happen does not.
    const BRIEFLY: Duration = Duration::from_millis(50);

    type Tested = Result<(), &'static str>;

    fn caret(left: i32) -> Anchor {
        Anchor::Caret(RECT {
            left,
            top: 10,
            right: left + 1,
            bottom: 28,
        })
    }

    /// Waits until this many threads are counted in `live`.
    fn settles_at(live: &AtomicUsize, count: usize) -> bool {
        let deadline = Instant::now() + WAIT;
        while live.load(Ordering::Acquire) != count {
            if Instant::now() > deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(1));
        }
        true
    }

    /// Takes what a watch has found, waiting for it to be this.
    fn finds(watch: &Watch, place: Anchor) -> bool {
        let deadline = Instant::now() + WAIT;
        loop {
            match watch.take() {
                Some(found) if found == place => return true,
                _ if Instant::now() > deadline => return false,
                _ => thread::sleep(Duration::from_millis(1)),
            }
        }
    }

    /// The far end of a lookup that says when it has begun and then does
    /// not come back until it is let go, as inside a program that hangs.
    struct Hung {
        /// The number of each lookup, as it begins.
        begun: mpsc::Receiver<i32>,
        let_go: mpsc::Sender<()>,
    }

    /// A watch over such a lookup, which answers with a caret at the number
    /// of the call once it is let go, or once nobody is left to do so.
    fn hung(live: &'static AtomicUsize) -> Result<(Watch, Hung), &'static str> {
        let (starts, begun) = mpsc::channel();
        let (let_go, held) = mpsc::channel::<()>();
        let mut calls = 0;
        let watch = Watch::with(live, move || {
            calls += 1;
            let _ = starts.send(calls);
            let _ = held.recv();
            Some(caret(calls))
        })
        .ok_or("no thread")?;
        Ok((watch, Hung { begun, let_go }))
    }

    #[test]
    fn the_place_found_is_there_to_take_once() -> Tested {
        static LIVE: AtomicUsize = AtomicUsize::new(0);
        let mut places = [None, Some(caret(7))].into_iter();
        let watch = Watch::with(&LIVE, move || places.next().flatten()).ok_or("no thread")?;
        // Nothing is looked for before it is asked.
        assert!(!watch.wait(BRIEFLY));
        assert_eq!(watch.take(), None);

        // A lookup that finds nothing has still come back.
        watch.ask();
        assert!(watch.wait(WAIT));
        assert_eq!(watch.take(), None);

        watch.ask();
        assert!(finds(&watch, caret(7)));
        assert_eq!(watch.take(), None);

        drop(watch);
        assert!(settles_at(&LIVE, 0), "the thread ends with the watch");
        Ok(())
    }

    #[test]
    fn a_lookup_that_hangs_holds_nobody_up_and_is_left_behind() -> Tested {
        static LIVE: AtomicUsize = AtomicUsize::new(0);
        let (watch, stuck) = hung(&LIVE)?;
        watch.ask();
        assert_eq!(stuck.begun.recv_timeout(WAIT), Ok(1));

        // The program does not answer. Asking, taking and waiting all come
        // back, and the wait gives up when its time is over.
        let began = Instant::now();
        for _ in 0..1000 {
            watch.ask();
            assert_eq!(watch.take(), None);
        }
        assert!(!watch.wait(BRIEFLY));
        assert!(began.elapsed() < WAIT);

        // The session ends: the watch goes without waiting for its thread,
        // which is still inside the program.
        let began = Instant::now();
        drop(watch);
        assert!(began.elapsed() < WAIT);
        assert_eq!(LIVE.load(Ordering::Acquire), 1);

        // The next session has a watch of its own, which works.
        let later = Watch::with(&LIVE, || Some(caret(42))).ok_or("no thread")?;
        later.ask();
        assert!(later.wait(WAIT));
        assert_eq!(later.take(), Some(caret(42)));
        assert_eq!(LIVE.load(Ordering::Acquire), 2);
        drop(later);
        assert!(settles_at(&LIVE, 1));

        // The program answers at last. The thousand requests that came in
        // behind the lookup are not made: the thread ends.
        assert_eq!(stuck.let_go.send(()), Ok(()));
        assert!(settles_at(&LIVE, 0), "the abandoned thread ends");
        assert!(stuck.begun.try_recv().is_err(), "no second lookup");
        Ok(())
    }

    #[test]
    fn asking_often_during_a_lookup_comes_to_one_more() -> Tested {
        static LIVE: AtomicUsize = AtomicUsize::new(0);
        let (watch, slow) = hung(&LIVE)?;
        watch.ask();
        assert_eq!(slow.begun.recv_timeout(WAIT), Ok(1));
        for _ in 0..50 {
            watch.ask();
        }
        assert_eq!(slow.let_go.send(()), Ok(()));
        // One more lookup for the fifty requests, and no third.
        assert_eq!(slow.begun.recv_timeout(WAIT), Ok(2));
        assert_eq!(slow.let_go.send(()), Ok(()));
        assert!(finds(&watch, caret(2)));
        assert!(slow.begun.recv_timeout(BRIEFLY).is_err());
        drop(watch);
        assert!(settles_at(&LIVE, 0));
        Ok(())
    }

    #[test]
    fn stuck_lookups_do_not_pile_up_without_end() -> Tested {
        static LIVE: AtomicUsize = AtomicUsize::new(0);
        // One session after another over a program that hangs.
        let mut stuck = Vec::new();
        for _ in 0..MOST_LOOKUPS {
            let (watch, held) = hung(&LIVE)?;
            watch.ask();
            assert_eq!(held.begun.recv_timeout(WAIT), Ok(1));
            drop(watch);
            stuck.push(held);
        }
        // The next one gets no thread, and is not counted.
        assert!(Watch::with(&LIVE, || None).is_none());
        assert_eq!(LIVE.load(Ordering::Acquire), MOST_LOOKUPS);
        // When the program is closed its threads come back, and the
        // session after that follows the caret again.
        drop(stuck);
        assert!(settles_at(&LIVE, 0));
        let watch = Watch::with(&LIVE, || Some(caret(3))).ok_or("no thread")?;
        watch.ask();
        assert!(watch.wait(WAIT));
        assert_eq!(watch.take(), Some(caret(3)));
        Ok(())
    }

    #[test]
    fn a_thread_looks_for_nothing_until_it_is_asked() -> Tested {
        static LIVE: AtomicUsize = AtomicUsize::new(0);
        let (watch, never) = hung(&LIVE)?;
        assert!(never.begun.recv_timeout(BRIEFLY).is_err());
        drop(watch);
        assert!(settles_at(&LIVE, 0));
        assert!(never.begun.try_recv().is_err());
        Ok(())
    }
}
