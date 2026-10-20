//! The system's edit control, laid over a field while it is typed in.
//!
//! A field is drawn by the layer like everything else, but typing is left
//! to Windows: the caret, selection, the clipboard, input methods for other
//! scripts and screen readers all come with its own edit control. One such
//! control exists at a time, without a border, exactly over the text of the
//! field that has the focus.
#![allow(unsafe_code)]

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    ANTIALIASED_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreateSolidBrush, DEFAULT_CHARSET,
    DeleteObject, FF_DONTCARE, FW_NORMAL, GetDC, GetTextMetricsW, HBRUSH, HFONT, HGDIOBJ,
    OUT_DEFAULT_PRECIS, ReleaseDC, SelectObject, TEXTMETRICW,
};
use windows::Win32::UI::Controls::{EM_SETMARGINS, EM_SETPASSWORDCHAR, EM_SETSEL};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, GetKeyState, SetFocus, VK_CONTROL, VK_MENU,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, CreateWindowExW, DefWindowProcW, DestroyWindow, ES_AUTOHSCROLL,
    ES_AUTOVSCROLL, ES_MULTILINE, ES_PASSWORD, ES_WANTRETURN, GWL_STYLE, GWLP_USERDATA,
    GWLP_WNDPROC, GetParent, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, PostMessageW,
    SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOZORDER, SWP_SHOWWINDOW, SendMessageW, SetWindowLongPtrW,
    SetWindowPos, SetWindowTextW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CHAR, WM_KEYDOWN,
    WM_KILLFOCUS, WM_MOUSEWHEEL, WM_PASTE, WM_SETFONT, WNDPROC, WS_CHILD,
};
use windows::core::{HSTRING, w};

use crate::theme::Color;

/// Sent to the parent when Enter, Esc or Tab is pressed in the edit control;
/// the key is in `wParam`.
pub(crate) const EDIT_KEY: u32 = WM_APP + 10;
/// Sent to the parent when the edit control loses the keyboard.
pub(crate) const EDIT_BLUR: u32 = WM_APP + 11;
/// Sent to the parent when something was pasted into the edit control. It
/// arrives after the paste, and after the parent has heard of the new text.
pub(crate) const EDIT_PASTE: u32 = WM_APP + 12;

/// What stands in for each character of a secret.
const HIDDEN: u16 = 0x25CF;
/// The margins of an edit control, both at once.
const EC_BOTH_MARGINS: usize = 3;
/// The letter A, which selects everything when pressed with Ctrl.
const VK_A: usize = 0x41;
/// The characters Ctrl+A and Ctrl+Backspace make, which an edit control
/// without the newer look would beep at or type as a box.
const CHAR_SELECT_ALL: usize = 0x01;
const CHAR_DELETE_WORD: usize = 0x7F;
const VK_TAB: usize = 0x09;
const VK_RETURN: usize = 0x0D;
const VK_ESCAPE: usize = 0x1B;

/// How the edit control is to look.
pub(crate) struct Style<'a> {
    /// The font family.
    pub(crate) family: &'a str,
    /// The height of the type in pixels.
    pub(crate) pixels: i32,
    /// The color of the text.
    pub(crate) text: Color,
    /// The color behind it.
    pub(crate) fill: Color,
    /// How far the control reaches past the text on either side, in pixels.
    /// Windows cuts a glyph off at the edge of an edit control's text, and
    /// the first letter of many a word starts a little before its place
    /// (a `J`, an `f`, anything smoothed), so the control is that much wider
    /// than the text and keeps the same margin inside.
    pub(crate) bleed: i32,
}

/// The edit control over a field. Goes away when dropped.
pub(crate) struct Edit {
    hwnd: HWND,
    font: HFONT,
    brush: HBRUSH,
    text: u32,
    fill: u32,
    line: i32,
    bleed: i32,
    /// How many lines it shows; with more than one, Enter starts a new line.
    lines: i32,
}

/// Sees the keys that mean something to the window before the edit control
/// swallows them, and tells the window when the control loses the keyboard.
unsafe extern "system" fn proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: `hwnd` is the live edit control this procedure was installed
    // on; its user data holds the procedure it had before, put there by
    // `Edit::create` before this one could be called.
    unsafe {
        let parent = GetParent(hwnd).unwrap_or_default();
        // In a control of more than one line, Enter starts a new one.
        let multiline =
            GetWindowLongPtrW(hwnd, GWL_STYLE) & isize::try_from(ES_MULTILINE).unwrap_or(0) != 0;
        let special = match wparam.0 {
            VK_TAB | VK_ESCAPE => true,
            VK_RETURN => !multiline,
            _ => false,
        };
        match message {
            WM_KEYDOWN if special => {
                let _ = PostMessageW(Some(parent), EDIT_KEY, wparam, LPARAM(0));
                return LRESULT(0);
            }
            // The characters those keys make would only beep.
            WM_CHAR if special => return LRESULT(0),
            WM_KEYDOWN if wparam.0 == VK_A && only_ctrl() => {
                SendMessageW(hwnd, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
                return LRESULT(0);
            }
            WM_CHAR if matches!(wparam.0, CHAR_SELECT_ALL | CHAR_DELETE_WORD) => {
                return LRESULT(0);
            }
            WM_MOUSEWHEEL => {
                let _ = PostMessageW(Some(parent), message, wparam, lparam);
                return LRESULT(0);
            }
            WM_KILLFOCUS => {
                let _ = PostMessageW(Some(parent), EDIT_BLUR, WPARAM(0), LPARAM(0));
            }
            WM_PASTE => {
                let _ = PostMessageW(Some(parent), EDIT_PASTE, WPARAM(0), LPARAM(0));
            }
            _ => {}
        }
        let before = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
        if before == 0 {
            return DefWindowProcW(hwnd, message, wparam, lparam);
        }
        let before: WNDPROC = std::mem::transmute::<isize, WNDPROC>(before);
        CallWindowProcW(before, hwnd, message, wparam, lparam)
    }
}

/// Whether Ctrl is held and Alt is not: Ctrl with Alt is how some keyboards
/// type a letter.
fn only_ctrl() -> bool {
    // SAFETY: plain queries of the keyboard state of this thread.
    unsafe { GetKeyState(i32::from(VK_CONTROL.0)) < 0 && GetKeyState(i32::from(VK_MENU.0)) >= 0 }
}

impl Edit {
    /// Puts an edit control holding `text` into `parent` and gives it the
    /// keyboard. It is not placed yet; see [`Self::place`] and [`Self::select`].
    pub(crate) fn create(
        parent: HWND,
        text: &str,
        secret: bool,
        lines: u8,
        style: &Style<'_>,
    ) -> Option<Self> {
        let multiline = lines > 1;
        let scrolls = if multiline {
            ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN
        } else {
            ES_AUTOHSCROLL
        };
        let mut flags = WS_CHILD.0 | u32::try_from(scrolls).unwrap_or(0);
        if secret {
            flags |= u32::try_from(ES_PASSWORD).unwrap_or(0);
        }
        let text = if multiline {
            to_windows_lines(text)
        } else {
            text.to_owned()
        };
        // SAFETY: the class and the strings are valid for the calls, the
        // parent is a live window of this thread, and every handle is
        // checked before use. What is created here is released in `Drop`.
        unsafe {
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("EDIT"),
                &HSTRING::from(text.as_str()),
                WINDOW_STYLE(flags),
                0,
                0,
                0,
                0,
                Some(parent),
                None,
                None,
                None,
            )
            .ok()?;
            let font = CreateFontW(
                -style.pixels,
                0,
                0,
                0,
                FW_NORMAL.0.cast_signed(),
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                ANTIALIASED_QUALITY,
                u32::from(FF_DONTCARE.0),
                &HSTRING::from(style.family),
            );
            let mut metrics = TEXTMETRICW::default();
            let dc = GetDC(Some(hwnd));
            let before = SelectObject(dc, HGDIOBJ(font.0));
            let _ = GetTextMetricsW(dc, &raw mut metrics);
            SelectObject(dc, before);
            ReleaseDC(Some(hwnd), dc);

            SendMessageW(
                hwnd,
                WM_SETFONT,
                Some(WPARAM(font.0 as usize)),
                Some(LPARAM(1)),
            );
            // Set after the font, which would put margins of its own back.
            let bleed = style.bleed.clamp(0, 0x7FFF);
            let both = isize::try_from((bleed << 16) | bleed).unwrap_or(0);
            SendMessageW(
                hwnd,
                EM_SETMARGINS,
                Some(WPARAM(EC_BOTH_MARGINS)),
                Some(LPARAM(both)),
            );
            if secret {
                SendMessageW(
                    hwnd,
                    EM_SETPASSWORDCHAR,
                    Some(WPARAM(usize::from(HIDDEN))),
                    Some(LPARAM(0)),
                );
            }
            let before = SetWindowLongPtrW(
                hwnd,
                GWLP_WNDPROC,
                (proc as *const () as usize).cast_signed(),
            );
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, before);
            Some(Self {
                hwnd,
                font,
                brush: CreateSolidBrush(windows::Win32::Foundation::COLORREF(
                    style.fill.colorref(),
                )),
                text: style.text.colorref(),
                fill: style.fill.colorref(),
                line: metrics.tmHeight.max(1),
                bleed,
                lines: i32::from(lines.max(1)),
            })
        }
    }

    /// Puts what is on the clipboard in place of all of its text.
    pub(crate) fn paste(&self) {
        self.select(true);
        // SAFETY: the control is a live window of this thread.
        unsafe { SendMessageW(self.hwnd, WM_PASTE, None, None) };
    }

    /// Selects all of its text, or puts the caret at the end of it. Done
    /// once the control has its place, so the text is scrolled to fit that.
    pub(crate) fn select(&self, all: bool) {
        // SAFETY: the control is a live window of this thread.
        unsafe {
            let end = GetWindowTextLengthW(self.hwnd);
            let start = if all { 0 } else { end };
            SendMessageW(self.hwnd, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(0)));
            SendMessageW(
                self.hwnd,
                EM_SETSEL,
                Some(WPARAM(usize::try_from(start).unwrap_or(0))),
                Some(LPARAM(isize::try_from(end).unwrap_or(0))),
            );
        }
    }

    /// Whether `hwnd` is this control.
    pub(crate) fn is(&self, hwnd: HWND) -> bool {
        self.hwnd == hwnd
    }

    /// Whether it has the keyboard.
    pub(crate) fn focused(&self) -> bool {
        // SAFETY: a plain query.
        unsafe { GetFocus() == self.hwnd }
    }

    /// Gives it the keyboard.
    pub(crate) fn focus(&self) {
        // SAFETY: the control is a live window of this thread.
        let _ = unsafe { SetFocus(Some(self.hwnd)) };
    }

    /// The brush and the colors Windows paints it with: fill, then text.
    pub(crate) fn colors(&self) -> (HBRUSH, u32, u32) {
        (self.brush, self.fill, self.text)
    }

    /// Puts it over `field`, the rectangle the text of the field has in
    /// pixels of the parent, in the middle of its height; or hides it if the
    /// field is out of sight. Its lines together are what is put in the
    /// middle, so the first sits where the field shows it.
    pub(crate) fn place(&self, field: RECT, seen: bool) {
        let room = field.bottom - field.top;
        let height = (self.line * self.lines).min(room.max(self.line));
        let top = field.top + (room - height) / 2;
        let show = if seen { SWP_SHOWWINDOW } else { SWP_HIDEWINDOW };
        // SAFETY: the control is a live window of this thread.
        let _ = unsafe {
            SetWindowPos(
                self.hwnd,
                None,
                field.left - self.bleed,
                top,
                (field.right - field.left + 2 * self.bleed).max(1),
                height,
                SWP_NOZORDER | SWP_NOACTIVATE | show,
            )
        };
    }

    /// What it reads.
    pub(crate) fn text(&self) -> String {
        // SAFETY: the buffer is as long as the call is told.
        unsafe {
            let length = usize::try_from(GetWindowTextLengthW(self.hwnd)).unwrap_or(0);
            let mut buffer = vec![0_u16; length + 1];
            let read = usize::try_from(GetWindowTextW(self.hwnd, &mut buffer)).unwrap_or(0);
            String::from_utf16_lossy(&buffer[..read.min(length)]).replace("\r\n", "\n")
        }
    }

    /// Makes it read `text`, all of it selected.
    pub(crate) fn retype(&self, text: &str) {
        let text = if self.lines > 1 {
            to_windows_lines(text)
        } else {
            text.to_owned()
        };
        // SAFETY: the string is NUL-terminated and outlives the call.
        unsafe {
            let _ = SetWindowTextW(self.hwnd, &HSTRING::from(text.as_str()));
            SendMessageW(self.hwnd, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
        }
    }
}

/// `text` with its lines ended the way an edit control of more than one
/// line needs them: a carriage return before each line feed. What it reads
/// back has them taken out again (`Edit::text`).
fn to_windows_lines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r\n")
}

impl Drop for Edit {
    fn drop(&mut self) {
        // SAFETY: the handles were made in `create` and are not used again.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
            let _ = DeleteObject(HGDIOBJ(self.font.0));
            let _ = DeleteObject(HGDIOBJ(self.brush.0));
        }
    }
}
