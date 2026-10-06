//! Shows the real listening indicator for a few seconds, fed with a made-up
//! voice: it fades in, follows the voice, glows while "finishing", and fades
//! out. It never takes the focus, and the program ends by itself.
//!
//! ```text
//! cargo run -p vinput-host --example indicator -- [caret|screen] [seconds]
//! ```
//!
//! `caret`, the default, puts it under the caret of whatever window has the
//! focus; `screen` puts the larger one at the bottom of the screen.
//!
//! With `measure` it times how long a key press waits for the first frame
//! instead, a number of times over, and prints each time and the median in
//! microseconds: `at-once` draws the frame as the session starts, which is
//! what the host does, and `at-tick` leaves it to the first tick of the
//! timer, which is what it used to do.
//!
//! ```text
//! cargo run -p vinput-host --example indicator -- measure [caret|screen] [at-once|at-tick] [rounds]
//! ```
//!
//! The indicator is private to the host, so its source is compiled into this
//! example as it stands.
#![allow(unsafe_code)]

#[cfg(windows)]
#[allow(dead_code, unreachable_pub)]
#[path = "../src/caret.rs"]
mod caret;
#[cfg(windows)]
#[allow(dead_code, unreachable_pub)]
#[path = "../src/indicator.rs"]
mod indicator;

#[cfg(windows)]
use vinput_host::draw;

/// The two helpers the indicator takes from the host's main module.
#[cfg(windows)]
mod windows {
    pub(crate) fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub(crate) fn last_error(action: &str) -> vinput_core::Error {
        let error = std::io::Error::last_os_error();
        vinput_core::Error::Host(format!("{action} failed: {error}"))
    }
}

#[cfg(windows)]
mod demo {
    use std::ptr::null_mut;
    use std::time::{Duration, Instant};

    use windows_sys::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, KillTimer, MSG, PostQuitMessage, SetTimer, WM_TIMER,
    };

    use crate::indicator::{Indicator, Placement};

    /// How often the host redraws the indicator, in milliseconds.
    const TICK_MS: u32 = 33;
    /// How long the indicator finishes for, and how long it is given to fade
    /// out, in ticks.
    const FINISHING: u32 = 60;
    const LEAVING: u32 = 12;

    /// A loudness that rises and falls like a few words with pauses between.
    #[expect(clippy::cast_precision_loss, reason = "tick counts are small")]
    fn voice(tick: u32) -> f32 {
        let t = tick as f32 * 0.033;
        let syllables = (t * 9.0).sin().abs() * (t * 2.3).sin().max(0.0);
        syllables * syllables * 0.5
    }

    /// How long the indicator is given to fade out between two rounds of
    /// measuring, in milliseconds.
    const BETWEEN_ROUNDS_MS: u32 = 300;

    /// Runs this thread's messages until a timer of the thread itself, one
    /// without a window, has ticked.
    fn until_tick() {
        let mut message = MSG::default();
        // SAFETY: `message` is writable, and is passed on as it was received.
        while unsafe { GetMessageW(&raw mut message, null_mut(), 0, 0) } > 0 {
            if message.message == WM_TIMER && message.hwnd.is_null() {
                return;
            }
            // SAFETY: as above.
            unsafe { DispatchMessageW(&raw const message) };
        }
    }

    /// Times the first frame `rounds` times over and prints what it took.
    #[expect(clippy::print_stdout, reason = "the numbers are what this is run for")]
    fn measure(placement: Placement, at_tick: bool, rounds: usize) -> vinput_core::Result<()> {
        let mut indicator = Indicator::new(placement)?;
        let mut waits = Vec::with_capacity(rounds);
        for _ in 0..rounds {
            let pressed = Instant::now();
            if at_tick {
                // SAFETY: a timer without a window posts its ticks to this thread.
                let timer = unsafe { SetTimer(null_mut(), 0, TICK_MS, None) };
                until_tick();
                // SAFETY: the timer was set above.
                unsafe { KillTimer(null_mut(), timer) };
            }
            indicator.update(0.0);
            waits.push(pressed.elapsed());
            indicator.hide();
            // SAFETY: as above.
            let timer = unsafe { SetTimer(null_mut(), 0, BETWEEN_ROUNDS_MS, None) };
            until_tick();
            // SAFETY: the timer was set above.
            unsafe { KillTimer(null_mut(), timer) };
        }
        let each: Vec<String> = waits
            .iter()
            .map(|time| time.as_micros().to_string())
            .collect();
        waits.sort_unstable();
        let median = waits.get(waits.len() / 2).map_or(0, Duration::as_micros);
        println!("first frame after, in microseconds: {}", each.join(" "));
        println!("median: {median}");
        Ok(())
    }

    pub(crate) fn run() -> vinput_core::Result<()> {
        let mut arguments = std::env::args().skip(1).peekable();
        let measuring = arguments
            .next_if(|argument| argument == "measure")
            .is_some();
        let placement = match arguments.next_if(|argument| !argument.starts_with("at-")) {
            Some(placement) if placement == "screen" => Placement::Screen,
            _ => Placement::Caret,
        };
        if measuring {
            let at_tick = arguments.next().as_deref() == Some("at-tick");
            let rounds = arguments
                .next()
                .and_then(|rounds| rounds.parse().ok())
                .unwrap_or(9);
            // SAFETY: called once, before any window exists, as the host
            // does; like the host, this thread has COM for as long as it lives.
            unsafe {
                SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
                let _ = windows::Win32::System::Com::CoInitializeEx(
                    None,
                    windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
                );
            }
            return measure(placement, at_tick, rounds);
        }
        let seconds: u32 = arguments
            .next()
            .and_then(|seconds| seconds.parse().ok())
            .unwrap_or(4);
        let listening = seconds.clamp(1, 60) * 1000 / TICK_MS;

        // SAFETY: called once, before any window exists, as the host does.
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        let mut indicator = Indicator::new(placement)?;
        // SAFETY: a timer without a window posts its ticks to this thread.
        let timer = unsafe { SetTimer(null_mut(), 0, TICK_MS, None) };
        let mut tick = 0u32;
        let mut message = MSG::default();
        // SAFETY: `message` is writable, and is passed on as it was received.
        while unsafe { GetMessageW(&raw mut message, null_mut(), 0, 0) } > 0 {
            if message.message != WM_TIMER || !message.hwnd.is_null() {
                // The indicator's own timer, among others.
                // SAFETY: as above.
                unsafe { DispatchMessageW(&raw const message) };
                continue;
            }
            tick += 1;
            if tick < listening {
                indicator.update(voice(tick));
            } else if tick == listening {
                indicator.finish();
            } else if tick == listening + FINISHING {
                indicator.hide();
            } else if tick == listening + FINISHING + LEAVING {
                // SAFETY: no arguments to speak of.
                unsafe { PostQuitMessage(0) };
            }
        }
        // SAFETY: the timer was set above.
        unsafe { KillTimer(null_mut(), timer) };
        Ok(())
    }
}

#[cfg(windows)]
fn main() -> vinput_core::Result<()> {
    demo::run()
}

#[cfg(not(windows))]
fn main() {}
