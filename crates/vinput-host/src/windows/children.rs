//! The windows, as the background process keeps them: Settings and the
//! update window, each in a process of its own for as long as it is open.
//!
//! A window process is started when its window is asked for and let go of
//! when the window closes; in between it is shown a picture of how things
//! stand whenever that picture has changed, and what the user does in it
//! comes back as commands. A window is told nothing before it has said that
//! it is up. With no window open nothing here runs: no process, no timer and
//! no picture.
#![allow(unsafe_code)]

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{KillTimer, PostMessageW, SetTimer};

use super::tray::About;
use super::{Host, WINDOW_MS, WINDOW_SPOKE, WINDOW_TIMER_ID};
use crate::link::Link;
use vinput_core::log;
use vinput_core::session::Controller;
use vinput_view::{self as wire, Page, Surface};

/// The two windows, each of which has a process of its own while it is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Which {
    /// The Settings window.
    Settings,
    /// The window that offers a newer version.
    Update,
}

impl Which {
    pub(super) const ALL: [Self; 2] = [Self::Settings, Self::Update];

    pub(super) fn of(surface: Surface) -> Self {
        match surface {
            Surface::Settings(_) => Self::Settings,
            Surface::Update => Self::Update,
        }
    }
}

/// A window that is open.
struct Window {
    /// The line to its process.
    link: Link,
    /// Whether it has said that it is up. Until then it is told nothing.
    ready: bool,
    /// The picture it was shown last, so that it is only sent a new one
    /// when something in it has changed.
    shown: Option<wire::View>,
    /// The page it was asked to turn to before it was up, which it is told
    /// once it has a picture to show there.
    turn_to: Option<Page>,
}

impl Window {
    fn new(link: Link) -> Self {
        Self {
            link,
            ready: false,
            shown: None,
            turn_to: None,
        }
    }

    /// Has the window come to the front on `page`, now or as soon as it is up.
    fn show(&mut self, page: Page) {
        if self.ready {
            self.link.send(wire::Event::Show(page));
        } else {
            self.turn_to = Some(page);
        }
    }

    /// Has the window come to the front on the page it shows. One that is
    /// not up yet is about to come to the front anyway.
    fn front(&mut self) {
        if self.ready {
            self.link.send(wire::Event::Front);
        }
    }

    /// Shows the window `picture` unless that is what it shows already, and
    /// then the page it was asked for while it was not up yet.
    fn picture(&mut self, picture: &wire::View) {
        if !self.ready {
            return;
        }
        if self.shown.as_ref() != Some(picture) {
            self.link
                .send(wire::Event::State(Box::new(picture.clone())));
            self.shown = Some(picture.clone());
        }
        if let Some(page) = self.turn_to.take() {
            self.link.send(wire::Event::Show(page));
        }
    }
}

/// What a window has said since it was last heard, and whether it is gone.
pub(super) struct Heard {
    /// What the user did, oldest first.
    pub(super) commands: Vec<wire::Command>,
    /// Whether the window's process has ended.
    pub(super) ended: bool,
}

/// The windows that are open: at most one of each.
#[derive(Default)]
pub(super) struct Windows {
    settings: Option<Window>,
    update: Option<Window>,
}

impl Windows {
    fn slot(&mut self, which: Which) -> &mut Option<Window> {
        match which {
            Which::Settings => &mut self.settings,
            Which::Update => &mut self.update,
        }
    }

    fn get(&self, which: Which) -> Option<&Window> {
        match which {
            Which::Settings => self.settings.as_ref(),
            Which::Update => self.update.as_ref(),
        }
    }

    /// Whether any window is open.
    pub(super) fn any(&self) -> bool {
        self.settings.is_some() || self.update.is_some()
    }

    /// Whether this window is open.
    pub(super) fn is_open(&self, which: Which) -> bool {
        self.get(which).is_some()
    }

    /// Whether this window is open and has said that it is up.
    pub(super) fn is_ready(&self, which: Which) -> bool {
        self.get(which).is_some_and(|window| window.ready)
    }

    /// What Windows knows the process of this window by.
    pub(super) fn process_id(&self, which: Which) -> Option<u32> {
        self.get(which).and_then(|window| window.link.process_id())
    }

    /// Has the window come to the front on `page` if its process is still
    /// there. Returns whether it is; if not, it has to be started.
    pub(super) fn show(&mut self, which: Which, page: Page) -> bool {
        match self.slot(which) {
            Some(window) if !window.link.has_ended() => {
                window.show(page);
                true
            }
            _ => false,
        }
    }

    /// Has the window come to the front as it is, if its process is still
    /// there. One that is closed stays closed.
    pub(super) fn front(&mut self, which: Which) {
        if let Some(window) = self.slot(which).as_mut()
            && !window.link.has_ended()
        {
            window.front();
        }
    }

    /// Keeps a window whose process was just started.
    pub(super) fn put(&mut self, which: Which, link: Link) {
        *self.slot(which) = Some(Window::new(link));
    }

    /// Lets go of a window, which tells it to close and sees its process
    /// off in the background. Returns whether it was open.
    pub(super) fn close(&mut self, which: Which) -> bool {
        self.slot(which).take().is_some()
    }

    /// The window has said that it is up: from now on it is shown pictures,
    /// starting with one whatever it was shown before.
    pub(super) fn ready(&mut self, which: Which) {
        if let Some(window) = self.slot(which) {
            window.ready = true;
            window.shown = None;
        }
    }

    /// What this window has said and whether it is gone, if it is open.
    /// Whether it is gone is looked at before its commands are taken, so
    /// that the last words of a window that has gone are still heard.
    pub(super) fn hear(&self, which: Which) -> Option<Heard> {
        let window = self.get(which)?;
        let ended = window.link.has_ended();
        Some(Heard {
            commands: window.link.commands(),
            ended,
        })
    }

    /// Shows every window that is up `picture`, unless it shows it already.
    pub(super) fn picture(&mut self, picture: &wire::View) {
        for window in [&mut self.settings, &mut self.update].into_iter().flatten() {
            window.picture(picture);
        }
    }

    /// Closes every window and waits until their processes are gone, for
    /// when this process is about to exit. Each is told to close before any
    /// is waited for, so that they leave together.
    pub(super) fn finish(&mut self) {
        let windows = [self.settings.take(), self.update.take()];
        for window in windows.iter().flatten() {
            window.link.close();
        }
        for window in windows.into_iter().flatten() {
            window.link.finish();
        }
    }
}

impl Host {
    /// Shows the Settings window on `page`: turns the one that is open to
    /// it, or starts the window process.
    pub(super) fn open_settings(&mut self, page: Page) {
        self.open_window(Surface::Settings(page));
    }

    /// Shows the Settings window where someone who asks for it without
    /// naming a page starts: on the welcome while that is owed, which it
    /// never is after an update, and on the General page otherwise.
    pub(super) fn open_home(&mut self) {
        let page = if self.settings.owes_welcome() {
            Page::Welcome
        } else {
            Page::General
        };
        self.open_settings(page);
    }

    /// Brings the window for `surface` to the front if its process is
    /// running, and starts that process otherwise. There is never more than
    /// one of each window.
    pub(super) fn open_window(&mut self, surface: Surface) {
        let which = Which::of(surface);
        // Either window comes to the front when it is told to show a page;
        // only Settings has pages to turn to.
        let page = match surface {
            Surface::Settings(page) => page,
            Surface::Update => Page::About,
        };
        if self.windows.show(which, page) {
            return;
        }
        // A window that has gone, and was not seen to go yet, makes way.
        self.close_window(which);
        // A window handle is an address that any thread may post to.
        let hwnd = self.hwnd as usize;
        let opened = Link::open(surface, move || {
            // SAFETY: posting to a window that has gone away fails harmlessly.
            unsafe { PostMessageW(hwnd as HWND, WINDOW_SPOKE, 0, 0) };
        });
        match opened {
            Ok(link) => {
                self.windows.put(which, link);
                self.slow.make_stale();
                // SAFETY: `hwnd` is our live window; the timer id is ours.
                unsafe { SetTimer(self.hwnd, WINDOW_TIMER_ID, WINDOW_MS, None) };
            }
            Err(err) => {
                log::write(&format!("starting a window process failed: {err}"));
                self.tray.warn(
                    About::Nothing,
                    "Window not opened",
                    &format!("Vinput could not open its window: {err}"),
                );
            }
        }
    }

    /// Lets go of a window: tells it to close if it is still there, and
    /// undoes what only lasts while it is open.
    pub(super) fn close_window(&mut self, which: Which) {
        if !self.windows.close(which) {
            return;
        }
        match which {
            Which::Settings => {
                self.stop_capturing();
                self.led_to = None;
                self.notice = None;
                self.finder.clear();
                self.google_close();
            }
            // Closing the update window says "later", also to a download.
            Which::Update => self.cancel_update(),
        }
        if !self.windows.any() {
            // SAFETY: `hwnd` is our live window; the timer id is ours.
            unsafe { KillTimer(self.hwnd, WINDOW_TIMER_ID) };
        }
    }

    /// Hears what the windows have said, lets go of the ones that are gone,
    /// and shows the ones that are left how things stand now.
    pub(super) fn on_windows(&mut self, controller: &mut dyn Controller) {
        self.hear_windows(controller);
        self.show_state();
    }

    /// Does what the windows have asked for and lets go of the ones that
    /// are gone. Returns whether there was anything to do.
    fn hear_windows(&mut self, controller: &mut dyn Controller) -> bool {
        let mut heard_any = false;
        for which in Which::ALL {
            let Some(heard) = self.windows.hear(which) else {
                continue;
            };
            heard_any |= heard.ended || !heard.commands.is_empty();
            for command in heard.commands {
                self.on_window_command(which, command, controller);
            }
            if !heard.ended {
                continue;
            }
            // Still open, so it did not say that it was closing; never up,
            // so the user saw nothing of it.
            if self.windows.is_open(which) && !self.windows.is_ready(which) {
                log::write("a window process ended before its window was up");
                self.tray.warn(
                    About::Nothing,
                    "Window not opened",
                    "Vinput could not open its window.",
                );
            }
            self.close_window(which);
        }
        heard_any
    }

    /// Shows the windows how things stand, if that is not what they were
    /// shown last. Without a window this does nothing at all. It is called
    /// when something has happened, so what is on disk is looked at anew.
    pub(super) fn show_state(&mut self) {
        self.slow.forget_on_disk();
        self.show_state_as_known();
    }

    /// Shows the windows how things stand when nothing is known to have
    /// happened: what was read off the folders for the last picture is
    /// taken to hold still.
    fn show_state_as_known(&mut self) {
        if !self.windows.any() {
            return;
        }
        if self.slow.is_stale() {
            self.slow.refresh();
        }
        let picture = self.picture();
        self.windows.picture(&picture);
    }

    /// The timer that runs while a window is open: downloads move and
    /// microphones come and go without anyone saying so, and a window that
    /// went without a word is noticed.
    pub(super) fn on_window_tick(&mut self, controller: &mut dyn Controller) {
        self.slow.tick();
        self.pro_keep_current();
        self.use_text_model_that_arrived(controller);
        if self.hear_windows(controller) {
            self.show_state();
        } else {
            self.show_state_as_known();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, BufRead, BufReader, Write};
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    const WAIT: Duration = Duration::from_secs(10);

    /// The far end of a line: where the window writes, where it reads, and
    /// the wakes of the host.
    struct Far {
        says: io::PipeWriter,
        hears: BufReader<io::PipeReader>,
        wakes: mpsc::Receiver<()>,
    }

    impl Far {
        fn next(&mut self) -> Option<wire::Event> {
            let mut line = String::new();
            match self.hears.read_line(&mut line) {
                Ok(read) if read > 0 => wire::Event::from_line(&line),
                _ => None,
            }
        }
    }

    fn line() -> io::Result<(Link, Far)> {
        let (from_window, says) = io::pipe()?;
        let (hears, to_window) = io::pipe()?;
        let (woken, wakes) = mpsc::channel();
        let link = Link::connect(BufReader::new(from_window), to_window, move || {
            let _ = woken.send(());
        })?;
        let far = Far {
            says,
            hears: BufReader::new(hears),
            wakes,
        };
        Ok((link, far))
    }

    fn picture(version: &str) -> wire::View {
        wire::View {
            version: version.to_owned(),
            ..wire::View::default()
        }
    }

    fn state(version: &str) -> wire::Event {
        wire::Event::State(Box::new(picture(version)))
    }

    #[test]
    fn there_is_one_window_of_each_kind_whatever_page_it_opens_on() {
        for page in [
            Page::General,
            Page::Models,
            Page::AddModel,
            Page::Google,
            Page::Pro,
            Page::Advanced,
            Page::About,
            Page::Welcome,
            Page::WelcomePro,
            Page::WelcomeLocal,
        ] {
            assert_eq!(Which::of(Surface::Settings(page)), Which::Settings);
        }
        assert_eq!(Which::of(Surface::Update), Which::Update);
        let mut windows = Windows::default();
        assert!(!windows.any());
        assert!(!windows.is_open(Which::Settings));
        assert!(!windows.show(Which::Update, Page::About));
        assert!(!windows.close(Which::Update));
        assert!(windows.hear(Which::Settings).is_none());
        assert_eq!(windows.process_id(Which::Settings), None);
    }

    #[test]
    fn a_window_is_told_nothing_until_it_is_up() -> io::Result<()> {
        let (link, mut far) = line()?;
        let mut windows = Windows::default();
        windows.put(Which::Settings, link);
        assert!(windows.any() && windows.is_open(Which::Settings));
        assert!(!windows.is_ready(Which::Settings));

        // Asked for twice more before it is up: no second process, and the
        // page asked for last is the one it turns to.
        assert!(windows.show(Which::Settings, Page::Models));
        assert!(windows.show(Which::Settings, Page::Advanced));
        windows.picture(&picture("early"));

        windows.ready(Which::Settings);
        assert!(windows.is_ready(Which::Settings));
        windows.picture(&picture("1"));
        // The first thing it hears is the picture, then where to turn.
        assert_eq!(far.next(), Some(state("1")));
        assert_eq!(far.next(), Some(wire::Event::Show(Page::Advanced)));

        // Once it is up it is turned at once.
        assert!(windows.show(Which::Settings, Page::About));
        assert_eq!(far.next(), Some(wire::Event::Show(Page::About)));
        Ok(())
    }

    #[test]
    fn a_picture_is_sent_only_when_it_has_changed() -> io::Result<()> {
        let (link, mut far) = line()?;
        let mut windows = Windows::default();
        windows.put(Which::Update, link);
        windows.ready(Which::Update);
        windows.picture(&picture("1"));
        assert_eq!(far.next(), Some(state("1")));
        // The same picture again is not sent: the next thing on the line is
        // the one that differs.
        windows.picture(&picture("1"));
        windows.picture(&picture("2"));
        assert_eq!(far.next(), Some(state("2")));
        // A window that says it is up again is shown what it was shown.
        windows.ready(Which::Update);
        windows.picture(&picture("2"));
        assert_eq!(far.next(), Some(state("2")));
        Ok(())
    }

    #[test]
    fn the_last_words_of_a_window_that_has_gone_are_heard() -> io::Result<()> {
        let (link, mut far) = line()?;
        let mut windows = Windows::default();
        windows.put(Which::Settings, link);
        writeln!(far.says, "{}", wire::Command::Ready.to_line())?;
        assert_eq!(far.wakes.recv_timeout(WAIT), Ok(()));
        let heard = windows.hear(Which::Settings);
        assert_eq!(
            heard.as_ref().map(|heard| heard.commands.as_slice()),
            Some([wire::Command::Ready].as_slice())
        );
        assert_eq!(heard.map(|heard| heard.ended), Some(false));

        writeln!(far.says, "{}", wire::Command::Closed.to_line())?;
        drop(far.says);
        assert_eq!(far.wakes.recv_timeout(WAIT), Ok(()));
        assert_eq!(far.wakes.recv_timeout(WAIT), Ok(()));
        let heard = windows.hear(Which::Settings);
        assert_eq!(
            heard.as_ref().map(|heard| heard.commands.as_slice()),
            Some([wire::Command::Closed].as_slice())
        );
        assert_eq!(heard.map(|heard| heard.ended), Some(true));
        // A window that has gone is not turned to a page: it is started anew.
        assert!(!windows.show(Which::Settings, Page::General));
        assert!(windows.close(Which::Settings));
        assert!(!windows.any());
        Ok(())
    }

    #[test]
    fn letting_go_of_a_window_tells_it_to_close() -> io::Result<()> {
        let (settings, mut far_settings) = line()?;
        let (update, mut far_update) = line()?;
        let mut windows = Windows::default();
        windows.put(Which::Settings, settings);
        windows.put(Which::Update, update);
        assert!(windows.close(Which::Settings));
        assert_eq!(far_settings.next(), Some(wire::Event::Close));
        assert_eq!(far_settings.next(), None);
        assert!(windows.any());
        windows.finish();
        assert_eq!(far_update.next(), Some(wire::Event::Close));
        assert_eq!(far_update.next(), None);
        assert!(!windows.any());
        Ok(())
    }
}
