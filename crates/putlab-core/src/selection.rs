//! The text that is selected in the program the user is working in.
//!
//! It is asked for the accessible way first: the focused control is asked
//! for its selection through UI Automation, which reads it and touches
//! nothing. Many programs answer that; the ones that do not (a good many
//! built on a browser, and older ones) are asked the way a person would
//! ask, by copying: Ctrl+C is sent, the clipboard is read, and what was on
//! the clipboard before is put back.
//!
//! The second way is taken with care, because the clipboard is the user's.
//! Everything on it is kept first, in every form it is there in (text with
//! its formatting, a picture, a list of files), and put back afterwards.
//! Where something on it cannot be kept whole, nothing is copied at all,
//! and there is then no selection to be had. Ctrl+C is pressed only once
//! the keys of the hotkey are let go of, so that it is Ctrl+C the program
//! sees and not Ctrl+Alt+C. And nothing is ever read aloud that was not
//! selected just now: if copying changes nothing, there is no selection,
//! and what was on the clipboard is not it.
//!
//! Text is put in a program the same careful way where it cannot simply be
//! typed ([`paste`]): the clipboard is kept, the text is placed on it and
//! Ctrl+V pressed, and what was there is put back. A line break that is
//! typed is the Enter key, which sends a message in many a program; one
//! that is pasted is a line break everywhere.
//!
//! Asking another program can take as long as that program likes, so it is
//! done on the thread of whoever wants the text, never on the one of the
//! message loop.

/// What the user has selected, tidied of nothing yet. `None` if nothing is
/// selected, or the program that has the focus does not say.
#[must_use]
pub fn selected(may_copy: bool) -> Option<String> {
    asked().or_else(|| may_copy.then(copied).flatten())
}

/// The selection as the program that has the focus says it when asked,
/// which touches nothing: no key is pressed, and the clipboard is left
/// alone. `None` if nothing is selected, or the program does not say.
#[must_use]
pub fn asked() -> Option<String> {
    platform::by_automation().filter(|text| !text.trim().is_empty())
}

/// The selection, taken by copying it: Ctrl+C once the keys of a hotkey
/// are let go of, with the clipboard put back as it was. `None` if nothing
/// is selected, or the clipboard holds something that could not be put
/// back.
#[must_use]
pub fn copied() -> Option<String> {
    platform::by_copying().filter(|text| !text.trim().is_empty())
}

/// Puts `text` where the caret is by pasting it, which replaces what is
/// selected there: the clipboard is kept, `text` is placed on it, Ctrl+V is
/// pressed once the keys of a hotkey are let go of, and what was on the
/// clipboard is put back. Says whether it was pasted. It is not where the
/// clipboard holds something that could not be put back, or cannot be
/// opened: the caller then has the text typed.
#[must_use]
pub fn paste(text: &str) -> bool {
    !text.is_empty() && platform::by_pasting(text)
}

/// The clipboard format of text.
const CF_UNICODETEXT: u32 = 13;
/// A picture as a handle, which Windows makes from [`CF_DIB`] when asked.
const CF_BITMAP: u32 = 2;
/// The colors of such a picture, made the same way.
const CF_PALETTE: u32 = 9;
/// A picture as memory, in its two forms.
const CF_DIB: u32 = 8;
const CF_DIBV5: u32 = 17;

/// What to do with one form of what is on the clipboard, to keep it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Keep {
    /// It is memory: copy it, and put the copy back.
    Copy,
    /// Windows makes it again from another form that is kept.
    Leave,
    /// It cannot be kept: the clipboard is left alone altogether.
    Cannot,
}

/// How the form `format` is kept. `has_picture` says whether a picture is
/// on the clipboard as memory, from which Windows makes the handle forms.
fn keeping(format: u32, has_picture: bool) -> Keep {
    match format {
        CF_BITMAP | CF_PALETTE if has_picture => Keep::Leave,
        // Handles to things that are not memory (a bitmap, a metafile, what
        // a program draws itself), and the forms private to one program.
        CF_BITMAP | CF_PALETTE | 3 | 14 | 0x80..=0x8E | 0x0200..=0x03FF => Keep::Cannot,
        _ => Keep::Copy,
    }
}

/// `text` with every line ended the way Windows ends one, however it was
/// ended before.
fn with_windows_line_ends(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r\n")
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod platform {
    use std::thread;
    use std::time::{Duration, Instant};

    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    };
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationTextPattern, UIA_TextPatternId,
    };
    use windows::core::Interface;
    use windows_sys::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
        GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
        RegisterClipboardFormatW, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
    };
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
        VK_CONTROL,
    };

    use super::{CF_DIB, CF_DIBV5, CF_UNICODETEXT, Keep, keeping};

    /// The keys C and V.
    const VK_C: u16 = 0x43;
    const VK_V: u16 = 0x56;
    /// Shift, Ctrl, Alt and the two Windows keys.
    const MODIFIERS: [i32; 5] = [0x10, 0x11, 0x12, 0x5B, 0x5C];
    /// How long the keys of the hotkey get to be let go of.
    const RELEASE_WAIT: Duration = Duration::from_millis(700);
    /// How long a program gets to answer Ctrl+C.
    const COPY_WAIT: Duration = Duration::from_millis(400);
    /// How long a program gets to take what Ctrl+V offers it, before the
    /// clipboard is put back. Nothing says when a program has read the
    /// clipboard; the ones built on a browser read it a moment after the
    /// key.
    const PASTE_WAIT: Duration = Duration::from_millis(300);
    /// The form by which a program says that what it puts on the clipboard
    /// is not to be kept in the clipboard history of Windows, nor sent to
    /// the user's other devices.
    const NOT_FOR_HISTORY: &str = "ExcludeClipboardContentFromMonitorProcessing";
    /// The most text taken from a selection, in UTF-16 units.
    const MOST: i32 = 200_000;
    /// The most that is kept of a clipboard, in bytes. One that holds more
    /// is left alone.
    const MOST_KEPT: usize = 64 * 1024 * 1024;

    /// The selection of the focused control, as it says it.
    pub(super) fn by_automation() -> Option<String> {
        // SAFETY: plain COM calls on objects created here, on a thread that
        // has COM set up for the length of the calls. Every interface is
        // released before COM is let go of again.
        unsafe {
            let ours = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
            let text = (|| {
                let automation: IUIAutomation =
                    CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
                let focused = automation.GetFocusedElement().ok()?;
                let pattern = focused.GetCurrentPattern(UIA_TextPatternId).ok()?;
                let pattern: IUIAutomationTextPattern = pattern.cast().ok()?;
                let ranges = pattern.GetSelection().ok()?;
                let mut text = String::new();
                for index in 0..ranges.Length().ok()? {
                    let range = ranges.GetElement(index).ok()?;
                    text.push_str(&range.GetText(MOST).ok()?.to_string());
                }
                Some(text)
            })();
            if ours {
                CoUninitialize();
            }
            text
        }
    }

    /// Opens the clipboard, which another program may hold for a moment.
    fn open_clipboard() -> bool {
        for _ in 0..10 {
            // SAFETY: no window owns the clipboard while it is only read or
            // restored; every success is paired with a close by the caller.
            if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        false
    }

    /// The bytes of the form `format` of what is on the clipboard.
    ///
    /// # Safety
    ///
    /// The clipboard must be open, and `format` a form that is memory.
    unsafe fn bytes_of(format: u32) -> Option<Vec<u8>> {
        // SAFETY: the caller holds the clipboard open. The handle it gives
        // is locked while it is read, for the size Windows says it has, and
        // stays the clipboard's own.
        unsafe {
            let handle: HANDLE = GetClipboardData(format);
            if handle.is_null() {
                return None;
            }
            let global = handle as HGLOBAL;
            let data = GlobalLock(global).cast::<u8>();
            if data.is_null() {
                return None;
            }
            let bytes = std::slice::from_raw_parts(data, GlobalSize(global)).to_vec();
            GlobalUnlock(global);
            Some(bytes)
        }
    }

    /// Everything on the clipboard, in every form it is there in, in the
    /// order it was put there. `None` if the clipboard cannot be opened, or
    /// something on it cannot be kept whole.
    fn keep() -> Option<Vec<(u32, Vec<u8>)>> {
        if !open_clipboard() {
            return None;
        }
        // SAFETY: the clipboard is open and is closed again here; only
        // forms that are memory are read as memory.
        unsafe {
            let has_picture = IsClipboardFormatAvailable(CF_DIB) != 0
                || IsClipboardFormatAvailable(CF_DIBV5) != 0;
            let mut kept = Vec::new();
            let mut total = 0usize;
            let mut whole = true;
            let mut format = EnumClipboardFormats(0);
            while format != 0 && whole {
                match keeping(format, has_picture) {
                    Keep::Leave => {}
                    Keep::Cannot => whole = false,
                    Keep::Copy => match bytes_of(format) {
                        Some(bytes) => {
                            total += bytes.len();
                            whole = total <= MOST_KEPT;
                            kept.push((format, bytes));
                        }
                        None => whole = false,
                    },
                }
                format = EnumClipboardFormats(format);
            }
            CloseClipboard();
            whole.then_some(kept)
        }
    }

    /// Puts `kept` back on the clipboard in place of what is there.
    fn put_back(kept: &[(u32, Vec<u8>)]) {
        if !open_clipboard() {
            return;
        }
        // SAFETY: the clipboard is open and is closed again here. Memory
        // handed to it becomes its own; memory it refuses is freed.
        unsafe {
            EmptyClipboard();
            for (format, bytes) in kept {
                let global = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1));
                if global.is_null() {
                    continue;
                }
                let data = GlobalLock(global).cast::<u8>();
                if data.is_null() {
                    GlobalFree(global);
                    continue;
                }
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
                GlobalUnlock(global);
                if SetClipboardData(*format, global as HANDLE).is_null() {
                    GlobalFree(global);
                }
            }
            CloseClipboard();
        }
    }

    /// The text on the clipboard now, if it holds text.
    fn text_now() -> Option<String> {
        if !open_clipboard() {
            return None;
        }
        // SAFETY: the clipboard is open and is closed again here, and text
        // is memory.
        let bytes = unsafe {
            let bytes = bytes_of(CF_UNICODETEXT);
            CloseClipboard();
            bytes
        }?;
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .take_while(|unit| *unit != 0)
            .collect();
        Some(String::from_utf16_lossy(&units))
    }

    /// Waits for Shift, Ctrl, Alt and the Windows keys to be let go of.
    /// Returns whether they were, in time.
    fn modifiers_released() -> bool {
        let began = Instant::now();
        loop {
            // SAFETY: plain value arguments. The sign bit is set while the
            // key is down.
            let held = MODIFIERS
                .iter()
                .any(|key| unsafe { GetAsyncKeyState(*key) } < 0);
            if !held {
                return true;
            }
            if began.elapsed() > RELEASE_WAIT {
                return false;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Presses and lets go of Ctrl and the key `vk`, as a keyboard would.
    fn press_with_ctrl(vk: u16) {
        let key = |vk: u16, up: bool| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let keys = [
            key(VK_CONTROL, false),
            key(vk, false),
            key(vk, true),
            key(VK_CONTROL, true),
        ];
        // SAFETY: the array holds four fully initialized inputs of the size
        // the call is told.
        unsafe {
            SendInput(
                4,
                keys.as_ptr(),
                i32::try_from(size_of::<INPUT>()).unwrap_or(0),
            );
        }
    }

    /// The selection, taken by copying it, with the clipboard put back as
    /// it was.
    pub(super) fn by_copying() -> Option<String> {
        // What cannot be put back whole is not replaced.
        let before = keep()?;
        // With a key of the hotkey still down the program would see another
        // combination than Ctrl+C.
        if !modifiers_released() {
            return None;
        }
        // SAFETY: a plain query.
        let mark = unsafe { GetClipboardSequenceNumber() };
        press_with_ctrl(VK_C);
        let started = Instant::now();
        // SAFETY: a plain query, as above.
        while unsafe { GetClipboardSequenceNumber() } == mark {
            if started.elapsed() > COPY_WAIT {
                // Nothing was copied: nothing is selected, and the
                // clipboard is as it was.
                return None;
            }
            thread::sleep(Duration::from_millis(5));
        }
        let copied = text_now();
        put_back(&before);
        copied
    }

    /// `text` as the clipboard holds text: UTF-16, little end first, with
    /// a zero after it.
    fn as_clipboard_text(text: &str) -> Vec<u8> {
        text.encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect()
    }

    /// Puts `text` where the caret is by pasting it, with the clipboard put
    /// back as it was.
    pub(super) fn by_pasting(text: &str) -> bool {
        // What cannot be put back whole is not replaced.
        let Some(before) = keep() else {
            return false;
        };
        // With a key of the hotkey still down the program would see another
        // combination than Ctrl+V.
        if !modifiers_released() {
            return false;
        }
        let name: Vec<u16> = NOT_FOR_HISTORY.encode_utf16().chain([0]).collect();
        // SAFETY: `name` is a live string that ends in a zero.
        let not_for_history = unsafe { RegisterClipboardFormatW(name.as_ptr()) };
        // Lines end on the clipboard the way Windows ends them.
        let text = super::with_windows_line_ends(text);
        let text = text.as_str();
        let mut ours = vec![(CF_UNICODETEXT, as_clipboard_text(text))];
        if not_for_history != 0 {
            ours.push((not_for_history, 0u32.to_le_bytes().to_vec()));
        }
        put_back(&ours);
        // SAFETY: a plain query.
        let mark = unsafe { GetClipboardSequenceNumber() };
        // Another program took the clipboard in between: what is on it is
        // not the text, and is not pasted.
        if text_now().as_deref() != Some(text) {
            return false;
        }
        press_with_ctrl(VK_V);
        thread::sleep(PASTE_WAIT);
        // What someone copied meanwhile is theirs, and stays.
        // SAFETY: a plain query, as above.
        if unsafe { GetClipboardSequenceNumber() } == mark {
            put_back(&before);
        }
        true
    }
}

#[cfg(not(windows))]
mod platform {
    pub(super) fn by_automation() -> Option<String> {
        None
    }

    pub(super) fn by_copying() -> Option<String> {
        None
    }

    pub(super) fn by_pasting(_text: &str) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_memory_is_kept_and_what_is_not_leaves_the_clipboard_alone() {
        // Text, formatted text and pictures as memory, lists of files, and
        // the forms programs register for themselves.
        for memory in [
            1,
            7,
            CF_UNICODETEXT,
            16,
            CF_DIB,
            CF_DIBV5,
            15,
            0xC000,
            0xC1A4,
        ] {
            assert_eq!(keeping(memory, false), Keep::Copy, "{memory}");
        }
        // The picture as a handle comes back with the picture as memory.
        assert_eq!(keeping(CF_BITMAP, true), Keep::Leave);
        assert_eq!(keeping(CF_PALETTE, true), Keep::Leave);
        // Without it, and for metafiles and what a program draws itself or
        // keeps private, there is nothing that could be put back.
        for handle in [
            CF_BITMAP, CF_PALETTE, 3, 14, 0x80, 0x82, 0x0200, 0x02FF, 0x0300, 0x03FF,
        ] {
            assert_eq!(keeping(handle, false), Keep::Cannot, "{handle}");
        }
        assert_eq!(keeping(14, true), Keep::Cannot);
    }

    #[test]
    fn lines_are_pasted_with_the_line_ends_of_windows() {
        assert_eq!(with_windows_line_ends("one\ntwo"), "one\r\ntwo");
        assert_eq!(with_windows_line_ends("one\r\ntwo\n"), "one\r\ntwo\r\n");
        assert_eq!(with_windows_line_ends("no line end"), "no line end");
    }

    #[test]
    fn nothing_is_pasted_for_no_text() {
        // Asked before the clipboard is looked at, so this touches nothing.
        assert!(!paste(""));
    }
}
