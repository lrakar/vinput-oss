//! What the window does with each message Windows sends it.
#![allow(unsafe_code)]

use std::cell::RefCell;
use std::time::Instant;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, HDC, PAINTSTRUCT, SetBkColor, SetTextColor,
};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Accessibility::UiaRootObjectId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, DefWindowProcW, DestroyWindow, EN_CHANGE, GWLP_USERDATA, GetForegroundWindow,
    GetWindowLongPtrW, GetWindowThreadProcessId, HTCLIENT, IDC_ARROW, IDC_HAND, IDC_IBEAM,
    IsIconic, KillTimer, LoadCursorW, MINMAXINFO, PostQuitMessage, SW_RESTORE, SWP_NOACTIVATE,
    SWP_NOZORDER, SetCursor, SetForegroundWindow, SetWindowPos, ShowWindow, WM_ACTIVATE, WM_CHAR,
    WM_CLOSE, WM_COMMAND, WM_CTLCOLOREDIT, WM_DESTROY, WM_DPICHANGED,
    WM_DWMCOLORIZATIONCOLORCHANGED, WM_ENTERSIZEMOVE, WM_ERASEBKGND, WM_GETMINMAXINFO,
    WM_GETOBJECT, WM_KEYDOWN, WM_KEYUP, WM_KILLFOCUS, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_MOVE, WM_NCLBUTTONDOWN, WM_NCRBUTTONDOWN, WM_PAINT,
    WM_RBUTTONDOWN, WM_SETCURSOR, WM_SETFOCUS, WM_SETTINGCHANGE, WM_SIZE, WM_SYSCHAR,
    WM_SYSCOLORCHANGE, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_THEMECHANGED, WM_TIMER, WM_XBUTTONUP,
};

use crate::a11y;
use crate::edit::{EDIT_BLUR, EDIT_KEY, EDIT_PASTE};
use crate::popup::{POPUP_PICK, Popup};
use crate::stage::{Cursor, Effect, vk};
use crate::theme::{metric, timing};

use super::launch::fit;
use super::{
    APP_EVENT, APP_FINISH, APP_FIT, APP_FRONT, APP_TELL, APP_WORK, APP_WORKED, Closed, Host,
    TIMER_MOVE, TIMER_TIP, WHEEL_DELTA, XBUTTON1, modifiers, point, px, units,
};

impl Host {
    /// Handles a message. `None` leaves it to Windows.
    #[expect(clippy::too_many_lines, reason = "one arm per message; each is short")]
    pub(super) fn handle(
        &mut self,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Option<LRESULT> {
        match message {
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                // SAFETY: `paint` is valid for both calls; the window is live.
                unsafe {
                    BeginPaint(self.hwnd, &raw mut paint);
                    self.paint();
                    let _ = EndPaint(self.hwnd, &raw const paint);
                }
            }
            WM_SIZE => self.resized(),
            WM_MOUSEMOVE => {
                if !self.tracking {
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: u32::try_from(size_of::<TRACKMOUSEEVENT>()).unwrap_or(0),
                        dwFlags: TME_LEAVE,
                        hwndTrack: self.hwnd,
                        dwHoverTime: 0,
                    };
                    // SAFETY: `track` is valid for the call and says its size.
                    self.tracking = unsafe { TrackMouseEvent(&raw mut track) }.is_ok();
                }
                self.stage.mouse_move(self.mouse(lparam));
                self.settle();
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                self.stage.mouse_leave();
                self.settle();
            }
            WM_LBUTTONDOWN => {
                // SAFETY: the window is live and of this thread.
                unsafe { SetCapture(self.hwnd) };
                let effects = self.stage.mouse_down(self.mouse(lparam));
                let typing = effects
                    .iter()
                    .any(|effect| matches!(effect, Effect::Edit(_)));
                self.perform(effects);
                if !typing && self.edit.is_none() {
                    // SAFETY: the window is live and of this thread.
                    let _ = unsafe { SetFocus(Some(self.hwnd)) };
                }
            }
            WM_LBUTTONUP => {
                // SAFETY: gives back what `WM_LBUTTONDOWN` took.
                let _ = unsafe { ReleaseCapture() };
                let effects = self.stage.mouse_up(self.mouse(lparam));
                self.perform(effects);
            }
            WM_XBUTTONUP if (wparam.0 >> 16) & 0xFFFF == XBUTTON1 => {
                let effects = self.stage.back();
                self.perform(effects);
            }
            WM_MOUSEWHEEL => {
                let turned = point(LPARAM(isize::try_from(wparam.0 & 0xFFFF_FFFF).unwrap_or(0))).1;
                let notches = units(turned, WHEEL_DELTA);
                if let Some(menu) = &self.menu {
                    menu.wheel(notches);
                } else {
                    let effects = self.stage.wheel(notches);
                    self.perform(effects);
                }
            }
            WM_SETCURSOR if u32::try_from(lparam.0 & 0xFFFF) == Ok(HTCLIENT) => {
                let shape = match self.stage.cursor() {
                    Cursor::Arrow => IDC_ARROW,
                    Cursor::Hand => IDC_HAND,
                    Cursor::Text => IDC_IBEAM,
                };
                // SAFETY: loading and setting a cursor Windows comes with.
                unsafe { SetCursor(LoadCursorW(None, shape).ok()) };
                return Some(LRESULT(1));
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                let key = u16::try_from(wparam.0).unwrap_or(0);
                if !self.key_down(key) {
                    return None;
                }
            }
            WM_KEYUP | WM_SYSKEYUP => {
                self.stage.key_up(modifiers());
                self.settle();
                return None;
            }
            // While a hotkey field waits, Alt with a letter is a hotkey,
            // not a way into a menu.
            WM_SYSCHAR if self.stage.capturing() => {}
            WM_SETFOCUS => {
                let effects = self.stage.refocus();
                self.perform(effects);
            }
            WM_KILLFOCUS => {
                let to = HWND(wparam.0 as *mut _);
                if !self.edit.as_ref().is_some_and(|edit| edit.is(to)) {
                    let effects = self.stage.blur();
                    self.perform(effects);
                }
            }
            WM_ACTIVATE if u16::try_from(wparam.0 & 0xFFFF) == Ok(0) => {
                let effects = self.stage.blur();
                self.perform(effects);
                self.tip = None;
                return None;
            }
            // A drop-down belongs to where its list was when it opened, and
            // a click anywhere but on it puts it away.
            WM_NCLBUTTONDOWN | WM_NCRBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN
            | WM_ENTERSIZEMOVE | WM_MOVE => {
                let effects = self.stage.close_menu();
                self.perform(effects);
                self.tip = None;
                return None;
            }
            // Typing while a drop-down is open goes to the entry that starts
            // with what was typed.
            WM_CHAR if self.menu.is_some() => {
                let typed = u32::try_from(wparam.0).ok().and_then(char::from_u32);
                if let (Some(menu), Some(typed)) = (&self.menu, typed) {
                    menu.typed(typed);
                }
            }
            WM_COMMAND if (wparam.0 >> 16) & 0xFFFF == EN_CHANGE as usize => {
                if let Some(edit) = &self.edit {
                    let effects = self.stage.typed(edit.text());
                    self.perform(effects);
                }
            }
            WM_CTLCOLOREDIT => {
                let edit = self.edit.as_ref()?;
                let (brush, fill, text) = edit.colors();
                let dc = HDC(wparam.0 as *mut _);
                // SAFETY: Windows passed a live device context for the edit
                // control it is about to paint.
                unsafe {
                    SetBkColor(dc, COLORREF(fill));
                    SetTextColor(dc, COLORREF(text));
                }
                return Some(LRESULT(brush.0 as isize));
            }
            EDIT_KEY => {
                let key = u16::try_from(wparam.0).unwrap_or(0);
                let effects = match key {
                    vk::ENTER => self.stage.edit_enter(),
                    vk::ESCAPE => self.stage.edit_escape(),
                    _ => self.stage.key_down(key, modifiers()).1,
                };
                self.perform(effects);
            }
            EDIT_PASTE => {
                let effects = self.stage.edit_pasted();
                self.perform(effects);
            }
            EDIT_BLUR => {
                if self.edit.as_ref().is_some_and(|edit| !edit.focused()) {
                    let effects = self.stage.edit_lost();
                    self.perform(effects);
                }
            }
            POPUP_PICK => {
                let effects = self.stage.picked(wparam.0);
                self.perform(effects);
            }
            APP_EVENT => self.drain(),
            WM_TIMER if wparam.0 == TIMER_MOVE => {
                let now = Instant::now();
                let elapsed = now.duration_since(self.moved).as_secs_f32() * 1000.0;
                self.moved = now;
                // SAFETY: a plain query of a live window.
                if !unsafe { IsIconic(self.hwnd) }.as_bool() {
                    #[expect(clippy::cast_precision_loss, reason = "a small constant")]
                    self.stage.advance(elapsed / timing::SWEEP as f32);
                    self.settle();
                }
            }
            WM_TIMER if wparam.0 == TIMER_TIP => {
                // SAFETY: the window is live; the timer belongs to it.
                let _ = unsafe { KillTimer(Some(self.hwnd), TIMER_TIP) };
                if let Some((text, rect)) = &self.tip_for {
                    self.tip = Popup::tip(&self.place(*rect), text);
                }
            }
            WM_SETTINGCHANGE
            | WM_THEMECHANGED
            | WM_SYSCOLORCHANGE
            | WM_DWMCOLORIZATIONCOLORCHANGED => {
                self.retheme();
                return None;
            }
            WM_GETMINMAXINFO if self.stage.showing().resizable() => {
                let least = metric::SETTINGS_MIN;
                // SAFETY: for this message `lparam` points at a `MINMAXINFO`
                // that is valid and writable for the call.
                if let Some(info) = unsafe { (lparam.0 as *mut MINMAXINFO).as_mut() } {
                    info.ptMinTrackSize = POINT {
                        x: px(least.0, self.scale),
                        y: px(least.1, self.scale),
                    };
                }
            }
            WM_CLOSE => self.finish(Closed::Dismissed),
            _ => return None,
        }
        Some(LRESULT(0))
    }
}

/// The message that says the mouse left the window.
const WM_MOUSELEAVE: u32 = 0x02A3;

pub(super) unsafe extern "system" fn proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: passing on a message this window was sent.
    let default = || unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    // SAFETY: the user data is either zero or points at the `RefCell` that
    // `open` keeps alive until the window is gone and the pointer cleared.
    let host = unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<Host>).as_ref() };
    match message {
        WM_ERASEBKGND => return LRESULT(1),
        WM_DESTROY => {
            if let Some(Ok(mut host)) = host.map(RefCell::try_borrow_mut) {
                host.torn_down();
            }
            a11y::disconnect(hwnd);
            // SAFETY: ends the message loop of this thread, which is the one
            // that opened the window.
            unsafe { PostQuitMessage(0) };
            return LRESULT(0);
        }
        _ => {}
    }
    let Some(host) = host else {
        return default();
    };
    if message == WM_GETOBJECT
        && u32::try_from(lparam.0 & 0xFFFF_FFFF).map(u32::cast_signed) == Ok(UiaRootObjectId)
    {
        // Asked for outside any borrow: UI Automation calls back into the
        // provider, which borrows the host itself.
        let me = host
            .try_borrow()
            .ok()
            .filter(|host| host.accessible())
            .map(|host| host.me.clone());
        return me.map_or_else(default, |me| a11y::answer(&me, hwnd, wparam, lparam));
    }
    if let Some(result) = outside(hwnd, host, message) {
        return result;
    }
    let handled = match host.try_borrow_mut() {
        Ok(mut host) => {
            if message == WM_DPICHANGED {
                let dpi = u16::try_from(wparam.0 & 0xFFFF).unwrap_or(96);
                host.rescale(f32::from(dpi) / 96.0);
            }
            host.handle(message, wparam, lparam)
        }
        Err(_) => None,
    };
    if message == WM_DPICHANGED {
        // Windows suggests where the window goes on the new display. Moving
        // it sends `WM_SIZE`, which needs the cell free again.
        // SAFETY: for this message `lparam` points at a valid `RECT`.
        if let Some(to) = unsafe { (lparam.0 as *const RECT).as_ref() } {
            // SAFETY: the window is live and of this thread.
            let _ = unsafe {
                SetWindowPos(
                    hwnd,
                    None,
                    to.left,
                    to.top,
                    to.right - to.left,
                    to.bottom - to.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                )
            };
        }
        // The same number of pixels is another size in units now, also when
        // Windows saw no reason to say the size changed.
        if let Ok(mut host) = host.try_borrow_mut() {
            host.resized();
        }
        return LRESULT(0);
    }
    handled.unwrap_or_else(default)
}

/// Brings the window in front of every other and gives it the keyboard.
///
/// Windows grants that only to the program the person is using, and lets
/// any other merely flash its button on the taskbar. When this window is
/// asked forward the person is often somewhere else: in the browser, where
/// they have just signed in. So where the plain request is not granted, it
/// is made again as part of the input queue of the window that is in front,
/// which is allowed to hand the foreground on.
///
/// # Safety
///
/// `hwnd` must be a live window of the calling thread.
unsafe fn take_foreground(hwnd: HWND) {
    // SAFETY: `hwnd` is live and of this thread, as the caller promises;
    // the other calls take any window or thread, gone or not.
    unsafe {
        let _ = SetForegroundWindow(hwnd);
        let front = GetForegroundWindow();
        if front == hwnd || front.is_invalid() {
            return;
        }
        let (ours, theirs) = (GetCurrentThreadId(), GetWindowThreadProcessId(front, None));
        if theirs == 0 || theirs == ours {
            return;
        }
        if AttachThreadInput(ours, theirs, true).as_bool() {
            let _ = BringWindowToTop(hwnd);
            let _ = SetForegroundWindow(hwnd);
            let _ = AttachThreadInput(ours, theirs, false);
        }
    }
}

/// Does what must be done outside any borrow of the host, because it sends
/// the window messages of its own or calls back into it: destroying the
/// window, bringing it to the front, fitting it to what it says, telling
/// assistive tools. `None` if `message` is not one of those.
///
/// These are posted to the window and handled here, in its procedure, rather
/// than picked out of the message loop: a loop Windows runs itself (while
/// the window is dragged, while a menu is open) hands them to the procedure
/// too, so a window that is told to close while it is being moved closes.
fn outside(hwnd: HWND, host: &RefCell<Host>, message: u32) -> Option<LRESULT> {
    match message {
        APP_FINISH => {
            if let Ok(mut host) = host.try_borrow_mut() {
                host.closing();
            }
            // SAFETY: the window is live and of this thread.
            let _ = unsafe { DestroyWindow(hwnd) };
        }
        APP_FRONT => {
            // SAFETY: the window is live and of this thread.
            unsafe {
                if IsIconic(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                }
                take_foreground(hwnd);
            }
        }
        APP_FIT => {
            let size = host.try_borrow().ok().and_then(|host| host.fitting());
            if let Some((width, height)) = size {
                fit(hwnd, width, height);
            }
        }
        APP_WORKED => {
            if let Ok(mut host) = host.try_borrow_mut() {
                host.collect_work();
            }
        }
        APP_TELL => {
            let news = host
                .try_borrow_mut()
                .map(|mut host| (host.accessible(), host.news()));
            if let Ok((true, (me, reshaped, focused))) = news {
                if reshaped {
                    a11y::reshaped(&me, hwnd);
                }
                if let Some(index) = focused {
                    a11y::focus_moved(&me, hwnd, index);
                }
            }
        }
        // Only wakes the message loop, which starts the work.
        APP_WORK => {}
        _ => return None,
    }
    Some(LRESULT(0))
}
