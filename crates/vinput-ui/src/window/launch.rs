//! Opening a window and running it until it is closed.
#![allow(unsafe_code)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use vinput_view::{Event, View};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow,
    SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CW_USEDEFAULT, CreateWindowExW, DispatchMessageW, GWL_STYLE, GWLP_USERDATA, GetMessageW,
    GetWindowLongW, HICON, IDC_ARROW, IMAGE_ICON, LR_DEFAULTSIZE, LR_SHARED, LoadCursorW,
    LoadImageW, MSG, PostMessageW, RegisterClassExW, SW_SHOW, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage, WINDOW_EX_STYLE,
    WINDOW_STYLE, WNDCLASSEXW, WS_CAPTION, WS_CLIPCHILDREN, WS_MINIMIZEBOX, WS_OVERLAPPED,
    WS_OVERLAPPEDWINDOW, WS_SYSMENU,
};
use windows::core::{HSTRING, PCWSTR, w};

use crate::error::Result;
use crate::geometry::Size;
use crate::gfx::{Gfx, failed};
use crate::imaging::{Com, Imaging};
use crate::layout::Measure;
#[cfg(any(debug_assertions, test))]
use crate::mock;
use crate::scene::Showing;
use crate::stage::Stage;
use crate::theme::metric;
use crate::{Backend, Listener, Options, Source, app, link};

use super::messages::proc;
use super::{
    APP_EVENT, APP_ICON, APP_WORKED, Closed, Host, Spec, Work, client_pixels, px, scale_of, units,
};

fn register(class: PCWSTR) -> Result<()> {
    static CLASS: std::sync::Once = std::sync::Once::new();
    // SAFETY: the class structure is valid for the call and the procedure
    // and names it points at live for the whole program.
    unsafe {
        let module = GetModuleHandleW(None).map_err(failed("finding the program"))?;
        CLASS.call_once(|| {
            let icon = LoadImageW(
                Some(module.into()),
                PCWSTR(usize::from(APP_ICON) as *const u16),
                IMAGE_ICON,
                0,
                0,
                LR_DEFAULTSIZE | LR_SHARED,
            )
            .map(|handle| HICON(handle.0))
            .unwrap_or_default();
            let class = WNDCLASSEXW {
                cbSize: u32::try_from(size_of::<WNDCLASSEXW>()).unwrap_or(0),
                lpfnWndProc: Some(proc),
                hInstance: module.into(),
                hIcon: icon,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpszClassName: class,
                ..Default::default()
            };
            RegisterClassExW(&raw const class);
        });
    }
    Ok(())
}

/// Sizes the window so that its inside is `size` units and puts it in the
/// middle of the display it is on.
fn arrange(hwnd: HWND, style: WINDOW_STYLE, size: Size, scale: f32) {
    let mut outer = RECT {
        left: 0,
        top: 0,
        right: px(size.w, scale),
        bottom: px(size.h, scale),
    };
    let mut info = MONITORINFO {
        cbSize: u32::try_from(size_of::<MONITORINFO>()).unwrap_or(0),
        ..Default::default()
    };
    // SAFETY: every structure is valid for its call; the window is live.
    unsafe {
        let dpi = GetDpiForWindow(hwnd);
        let _ = AdjustWindowRectExForDpi(&raw mut outer, style, false, WINDOW_EX_STYLE(0), dpi);
        let (mut width, mut height) = (outer.right - outer.left, outer.bottom - outer.top);
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let (mut x, mut y) = (0, 0);
        if GetMonitorInfoW(monitor, &raw mut info).as_bool() {
            let work = info.rcWork;
            width = width.min(work.right - work.left);
            height = height.min(work.bottom - work.top);
            x = work.left + (work.right - work.left - width) / 2;
            y = work.top + (work.bottom - work.top - height) / 2;
        }
        let _ = SetWindowPos(
            hwnd,
            None,
            x,
            y,
            width,
            height,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

/// Makes the inside of the window `width` by `height` pixels, where it is.
pub(super) fn fit(hwnd: HWND, width: i32, height: i32) {
    let mut outer = RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: height,
    };
    // SAFETY: `outer` is valid for the call; the window is live and of this
    // thread.
    unsafe {
        let style = WINDOW_STYLE(GetWindowLongW(hwnd, GWL_STYLE).cast_unsigned());
        let dpi = GetDpiForWindow(hwnd);
        let _ = AdjustWindowRectExForDpi(&raw mut outer, style, false, WINDOW_EX_STYLE(0), dpi);
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            outer.right - outer.left,
            outer.bottom - outer.top,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

/// Connects the backend the spec names. Events it reports wait in `inbox`,
/// and the window whose handle is in `target` is told there are some.
fn connect(
    source: &Source,
    view: &View,
    inbox: &Arc<Mutex<VecDeque<Event>>>,
    target: &Arc<AtomicIsize>,
) -> Box<dyn Backend> {
    let (inbox, target) = (Arc::clone(inbox), Arc::clone(target));
    let listener: Listener = Box::new(move |event| {
        if let Ok(mut inbox) = inbox.lock() {
            inbox.push_back(event);
        }
        let hwnd = HWND(target.load(Ordering::Acquire) as *mut _);
        if !hwnd.is_invalid() {
            // SAFETY: posting to a window handle is sound from any thread;
            // if the window is gone the post fails and nothing is lost that
            // anyone waits for.
            let _ = unsafe { PostMessageW(Some(hwnd), APP_EVENT, WPARAM(0), LPARAM(0)) };
        }
    });
    #[cfg(not(any(debug_assertions, test)))]
    let _ = view;
    match source {
        #[cfg(any(debug_assertions, test))]
        Source::Mock(_) => Box::new(mock::Fake::new(view.clone(), listener)),
        // The build that ships has no canned data to show a window over.
        _ => Box::new(link::connect_stdio(listener)),
    }
}

/// Makes the window for `showing`, not yet shown, sized or placed. Returns
/// it with its style.
fn create(showing: &Showing) -> Result<(HWND, WINDOW_STYLE)> {
    let class = w!("VinputWindow");
    register(class)?;
    let style = if showing.resizable() {
        WS_OVERLAPPEDWINDOW
    } else if matches!(showing, Showing::Asking(_)) {
        WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU
    } else {
        WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX
    } | WS_CLIPCHILDREN;
    // SAFETY: the class is registered, the strings are valid for the call
    // and the window is checked before use.
    let hwnd = unsafe {
        let module = GetModuleHandleW(None).map_err(failed("finding the program"))?;
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            &HSTRING::from(showing.title()),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            None,
            None,
            Some(module.into()),
            None,
        )
    }
    .map_err(failed("creating the window"))?;
    Ok((hwnd, style))
}

/// Opens a window and runs it until it is closed.
///
/// # Errors
///
/// Returns [`crate::Error::System`] if the window cannot be made.
pub(crate) fn open(spec: Spec<'_>, work: Option<Work<'_>>) -> Result<Closed> {
    // SAFETY: a plain call; it fails harmlessly if the process already said
    // how it deals with displays.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    // COM stays usable on this thread for as long as the window lives: the
    // imaging component needs it, and it is what makes assistive tools call
    // the providers of the window on this thread and no other.
    let com = Com::enter();
    let theme = app::theme(spec.appearance);
    let gfx = Gfx::new(theme.text_scale)?;
    let (hwnd, style) = create(&spec.showing)?;
    let scale = scale_of(hwnd);
    let (width, height) = spec.showing.size().unwrap_or_else(|| {
        let content = spec.showing.content(&spec.view);
        let fonts: &dyn Measure = gfx.fonts();
        (
            metric::DIALOG_W,
            crate::shell::compact_height(&content, metric::DIALOG_W, fonts),
        )
    });
    arrange(hwnd, style, Size::new(width, height), scale);

    let inbox = Arc::new(Mutex::new(VecDeque::new()));
    let target = Arc::new(AtomicIsize::new(0));
    let backend = spec
        .source
        .map(|source| connect(source, &spec.view, &inbox, &target));
    let (pixel_w, pixel_h) = client_pixels(hwnd);
    let size = Size::new(units(pixel_w, scale), units(pixel_h, scale));
    let stage = Stage::new(spec.showing, spec.view, size, Box::new(gfx.fonts().clone()));
    let host = Rc::new(RefCell::new(Host {
        me: Weak::new(),
        told_focus: None,
        told_shape: 0,
        accessible: com.apartment_threaded(),
        target: Arc::clone(&target),
        hwnd,
        gfx,
        imaging: Imaging::new().ok(),
        canvas: None,
        icons: None,
        lost: 0,
        theme,
        appearance: spec.appearance,
        scale,
        stage,
        backend,
        inbox,
        edit: None,
        paned: false,
        menu: None,
        tip: None,
        tip_for: None,
        tracking: false,
        moving: false,
        moved: Instant::now(),
        outcome: Closed::Dismissed,
        has_work: work.is_some(),
        work_wanted: false,
        working: false,
        worked: Arc::new(Mutex::new(None)),
        working_words: spec.working.to_owned(),
    }));
    if let Ok(mut inner) = host.try_borrow_mut() {
        inner.me = Rc::downgrade(&host);
    }
    if let Ok(mut host) = host.try_borrow_mut() {
        host.redress();
        host.settle();
        for command in host.stage.first_words() {
            host.send(command);
        }
    }
    // SAFETY: the cell is kept alive by `host` and outlives the window: the pointer is
    // cleared below before the box is dropped.
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, Rc::as_ptr(&host) as isize) };
    target.store(hwnd.0 as isize, Ordering::Release);
    // SAFETY: the window is live and of this thread.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = PostMessageW(Some(hwnd), APP_EVENT, WPARAM(0), LPARAM(0));
    }

    let outcome = pump(hwnd, &host, work);
    target.store(0, Ordering::Release);
    // SAFETY: the window may be gone already, which makes this a no-op that
    // fails; either way nothing points at the cell afterwards.
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
    // The window and everything it drew with go before COM does.
    drop(host);
    drop(com);
    Ok(outcome)
}

/// Runs the message loop until the window is gone, and starts the work of a
/// window that asks on a thread of its own when the user said yes.
///
/// Everything else that must not happen inside a borrow of the host
/// (destroying the window, bringing it to the front) is done by the window
/// procedure, so that it also happens while Windows runs a loop of its own:
/// while the window is being dragged, say.
fn pump(hwnd: HWND, host: &RefCell<Host>, work: Option<Work<'_>>) -> Closed {
    // The window asks for the work once at a time, so the lock is never
    // waited for; it is what lets one thread after another borrow the work.
    let work = work.map(Mutex::new);
    let worked = host.try_borrow().ok().map(|host| Arc::clone(&host.worked));
    let target = hwnd.0 as isize;
    std::thread::scope(|scope| {
        let mut message = MSG::default();
        // SAFETY: `message` is valid for every call; the window handles in
        // it are those of this thread.
        while unsafe { GetMessageW(&raw mut message, None, 0, 0) }.0 > 0 {
            // SAFETY: `message` was filled in by `GetMessageW`.
            unsafe {
                let _ = TranslateMessage(&raw const message);
                DispatchMessageW(&raw const message);
            }
            let wanted = host
                .try_borrow_mut()
                .is_ok_and(|mut host| host.take_work_wanted());
            if !wanted {
                continue;
            }
            let (Some(work), Some(worked)) = (work.as_ref(), worked.clone()) else {
                continue;
            };
            scope.spawn(move || {
                let outcome = match work.lock() {
                    Ok(mut work) => work(),
                    Err(_) => Err("the work could not be started".to_owned()),
                };
                if let Ok(mut slot) = worked.lock() {
                    *slot = Some(outcome);
                }
                // SAFETY: posting to a window handle is sound from any
                // thread; if the window is gone nobody waits for this.
                let _ = unsafe {
                    PostMessageW(
                        Some(HWND(target as *mut _)),
                        APP_WORKED,
                        WPARAM(0),
                        LPARAM(0),
                    )
                };
            });
        }
    });
    host.try_borrow()
        .map_or(Closed::Dismissed, |host| host.outcome)
}

/// Runs a window process: the window the options name, until it is closed.
///
/// # Errors
///
/// Returns [`crate::Error::System`] if the window cannot be made.
pub(crate) fn run(showing: Showing, view: View, options: &Options) -> Result<()> {
    let asking = matches!(showing, Showing::Asking(_));
    let spec = Spec {
        showing,
        view,
        appearance: options.appearance,
        source: (!asking).then_some(&options.source),
        working: "Installing",
    };
    if asking {
        // A canned window that asks pretends to work for a moment and then
        // fails once, so every state of it can be seen.
        let mut tries = 0;
        let mut work = move || {
            std::thread::sleep(std::time::Duration::from_millis(1500));
            tries += 1;
            if tries == 1 {
                Err("VinPUT is still running; exit it from its tray icon and try again".to_owned())
            } else {
                Ok(())
            }
        };
        return open(spec, Some(&mut work)).map(|_| ());
    }
    open(spec, None).map(|_| ())
}

impl From<Closed> for Option<bool> {
    fn from(closed: Closed) -> Self {
        match closed {
            Closed::Accepted => Some(true),
            Closed::Refused => Some(false),
            Closed::Dismissed => None,
        }
    }
}
