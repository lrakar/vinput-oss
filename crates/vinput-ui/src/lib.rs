//! The Vinput windows: Settings, the update window and setup.
//!
//! This crate draws what a [`vinput_view::View`] says and reports what the
//! user does as [`vinput_view::Command`]s. It knows nothing about the engine:
//! everything it shows arrives through a [`Backend`], which is the background
//! process in real use ([`link`]) and a fake with canned pictures ([`mock`])
//! when a window is being worked on or photographed.
//!
//! ```text
//! vinput ui settings                 the real thing, started by the background process
//! vinput ui settings --mock models   the Models page over canned data, to look at and click through
//! vinput ui settings --mock models --picture models.png    the same, saved as a picture
//! vinput ui about --mock update --picture about.png --theme light --scale 1.5 --size 1200x800
//! ```
//!
//! # How it is put together
//!
//! A page is a function from the view to a tree of [`node`]s ([`pages`]).
//! Everything under that is written once, for all pages:
//!
//! ```text
//! pages     view -> nodes                      what is on a page
//! layout    nodes + width -> items, controls   where everything goes
//! shell     + navigation pane, title, footer   a whole window, laid out
//! stage     + mouse, keyboard, focus, scroll   what the user is doing
//! paint     -> Direct2D                        how it looks (theme)
//! window    the Win32 window around it all
//! picture   the same painting, onto a PNG
//! ```
//!
//! [`theme`] holds every color and size. [`dialog`] shows the small windows
//! that ask or tell, for setup and removal, which have no background
//! process behind them.
//!
//! # What ships and what does not
//!
//! The canned pictures ([`mock`]), the test card ([`gallery`]) and taking a
//! picture instead of showing a window ([`picture`]) are for working on the
//! windows. They are part of a build for development and of the tests, and
//! are left out of the executable that ships ([`CANNED`]): nobody who
//! installed Vinput has a use for them, and they are a good many bytes. In
//! such a build `--mock` and `--picture` are answered with why not.

pub mod error;
#[cfg(any(debug_assertions, test))]
pub mod gallery;
pub mod geometry;
pub mod hotkey;
pub mod icons;
pub mod layout;
pub mod link;
pub mod look;
#[cfg(any(debug_assertions, test))]
pub mod mock;
pub mod node;
pub mod pages;
pub mod scene;
pub mod shell;
pub mod stage;
pub mod theme;

#[cfg(windows)]
mod a11y;
#[cfg(windows)]
mod app;
#[cfg(windows)]
mod clipboard;
#[cfg(windows)]
pub mod dialog;
#[cfg(windows)]
mod edit;
#[cfg(windows)]
pub mod gfx;
#[cfg(windows)]
pub mod imaging;
#[cfg(windows)]
pub mod paint;
#[cfg(all(windows, any(debug_assertions, test)))]
pub mod picture;
#[cfg(windows)]
mod popup;
#[cfg(windows)]
pub mod system;
#[cfg(windows)]
mod window;

use std::path::PathBuf;

use vinput_view::{Command, Event, Surface};

pub use error::{Error, Result};

/// Whether this build has the canned pictures and can take a picture of a
/// window: a build for development or for tests does, the one that ships
/// does not.
pub const CANNED: bool = cfg!(any(debug_assertions, test));

/// Where a window gets what it shows, and where it sends what the user does.
pub trait Backend: Send {
    /// Tells the other side what the user did.
    fn send(&self, command: Command);
}

/// Receives what the other side says. Called on a thread of the backend's
/// own, never on the thread the windows run on.
pub type Listener = Box<dyn Fn(Event) + Send>;

/// Where the windows of this process get their data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// From the background process that started this one, over standard
    /// input and output.
    Host,
    /// From canned data: the name of a scene in [`mock`].
    Mock(String),
}

/// The theme a window is drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Appearance {
    /// What Windows is set to, followed as it changes.
    #[default]
    System,
    /// Light, whatever Windows is set to.
    Light,
    /// Dark, whatever Windows is set to.
    Dark,
    /// A high-contrast theme, whatever Windows is set to: for pictures.
    Contrast,
}

/// What a window process is asked to do.
#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    /// The window to show.
    pub surface: Surface,
    /// Where its data comes from.
    pub source: Source,
    /// Save a picture of the window here instead of showing it, then leave.
    pub picture: Option<PathBuf>,
    /// The theme to draw in.
    pub appearance: Appearance,
    /// How many pixels a unit is in a picture: 1 for a display at 100 %.
    pub scale: f32,
    /// The size of the inside of the window, in units, instead of the size
    /// the window opens with.
    pub size: Option<(u16, u16)>,
    /// The control, counted from zero in the order Tab walks them, that a
    /// picture shows with the keyboard focus.
    pub focus: Option<usize>,
    /// The control a picture shows with the mouse over it.
    pub hover: Option<usize>,
    /// How far down a picture is scrolled, in units.
    pub scroll: u16,
    /// How much larger than usual text is in a picture, instead of what the
    /// user asked of Windows: 1 to 2.25.
    pub text_scale: Option<f32>,
}

impl Options {
    /// Options for showing `surface` from `source` as it is.
    #[must_use]
    pub fn new(surface: Surface, source: Source) -> Self {
        Self {
            surface,
            source,
            picture: None,
            appearance: Appearance::System,
            scale: 1.0,
            size: None,
            focus: None,
            hover: None,
            scroll: 0,
            text_scale: None,
        }
    }

    /// Reads what follows `vinput ui` on the command line:
    ///
    /// ```text
    /// <settings|models|add-model|google|pro|advanced|about|update>
    ///     [--mock <scene>] [--picture <file.png>]
    ///     [--theme light|dark|contrast] [--scale 1.5] [--size 940x660]
    ///     [--focus <n>] [--hover <n>] [--scroll <units>] [--text-scale 1.5]
    /// ```
    ///
    /// The last line is for pictures: which control has the focus or the
    /// mouse over it, how far the page is scrolled, and how large text is.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Arguments`] with what is wrong, in words for a
    /// terminal.
    pub fn parse(args: &[&str]) -> Result<Self> {
        use vinput_view::Page;
        let wrong = |what: String| Error::Arguments(what);
        let (surface, mut rest) = match args {
            ["settings" | "general", rest @ ..] => (Surface::Settings(Page::General), rest),
            ["models", rest @ ..] => (Surface::Settings(Page::Models), rest),
            ["add-model", rest @ ..] => (Surface::Settings(Page::AddModel), rest),
            ["google", rest @ ..] => (Surface::Settings(Page::Google), rest),
            ["pro", rest @ ..] => (Surface::Settings(Page::Pro), rest),
            ["advanced", rest @ ..] => (Surface::Settings(Page::Advanced), rest),
            ["about", rest @ ..] => (Surface::Settings(Page::About), rest),
            ["update", rest @ ..] => (Surface::Update, rest),
            _ => {
                return Err(wrong(
                    "which window? settings, models, add-model, google, pro, advanced, about or \
                     update"
                        .to_owned(),
                ));
            }
        };
        let mut options = Self::new(surface, Source::Host);
        loop {
            let (name, value, more) = match rest {
                [] => return Ok(options),
                [name, value, more @ ..] => (*name, *value, more),
                [name] => return Err(wrong(format!("{name} needs a value"))),
            };
            let bad = || wrong(format!("{value:?} is not a value for {name}"));
            match name {
                "--mock" | "--picture" if !CANNED => {
                    return Err(wrong(format!(
                        "{name} is for working on the windows; this build shows only the \
                         real ones"
                    )));
                }
                "--mock" => options.source = Source::Mock(value.to_owned()),
                "--picture" => options.picture = Some(PathBuf::from(value)),
                "--theme" => {
                    options.appearance = match value {
                        "light" => Appearance::Light,
                        "dark" => Appearance::Dark,
                        "system" => Appearance::System,
                        "contrast" => Appearance::Contrast,
                        _ => return Err(bad()),
                    };
                }
                "--scale" => {
                    options.scale = value
                        .parse()
                        .ok()
                        .filter(|scale| (0.5..=4.0).contains(scale))
                        .ok_or_else(bad)?;
                }
                "--size" => {
                    let size = value.split_once('x').and_then(|(width, height)| {
                        Some((width.parse().ok()?, height.parse().ok()?))
                    });
                    options.size = Some(size.ok_or_else(bad)?);
                }
                "--focus" => options.focus = Some(value.parse().map_err(|_| bad())?),
                "--hover" => options.hover = Some(value.parse().map_err(|_| bad())?),
                "--scroll" => options.scroll = value.parse().map_err(|_| bad())?,
                "--text-scale" => {
                    let scale = value
                        .parse()
                        .ok()
                        .filter(|scale| (1.0..=2.25).contains(scale));
                    options.text_scale = Some(scale.ok_or_else(bad)?);
                }
                other => return Err(wrong(format!("unknown argument {other:?}"))),
            }
            rest = more;
        }
    }
}

/// Runs a window process until its window is closed, or saves the picture
/// that was asked for.
///
/// # Errors
///
/// Returns why the window could not be shown or the picture not saved.
pub fn run(options: &Options) -> Result<()> {
    #[cfg(windows)]
    {
        app::run(options)
    }
    #[cfg(not(windows))]
    {
        let _ = options;
        Err(Error::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use vinput_view::Page;

    use super::*;

    #[test]
    fn the_command_line_names_a_window_and_its_source() {
        assert_eq!(
            Options::parse(&["settings"]),
            Ok(Options::new(Surface::Settings(Page::General), Source::Host))
        );
        assert_eq!(
            Options::parse(&["models", "--mock", "downloading", "--picture", "a.png"]),
            Ok(Options {
                picture: Some(PathBuf::from("a.png")),
                ..Options::new(
                    Surface::Settings(Page::Models),
                    Source::Mock("downloading".to_owned())
                )
            })
        );
        assert!(Options::parse(&[]).is_err());
        assert!(Options::parse(&["update", "--loud"]).is_err());
        assert!(Options::parse(&["update", "--loud", "very"]).is_err());
        // Tests, like a build for development, have the canned pictures.
        const { assert!(CANNED) };
    }

    #[test]
    fn a_picture_can_be_asked_for_in_a_theme_a_scale_and_a_size() {
        let options = Options::parse(&[
            "about", "--theme", "light", "--scale", "1.5", "--size", "1200x800", "--focus", "3",
            "--scroll", "120",
        ]);
        let Ok(options) = options else {
            unreachable!("the arguments were expected to parse");
        };
        assert_eq!(options.appearance, Appearance::Light);
        assert!((options.scale - 1.5).abs() < f32::EPSILON);
        assert_eq!(options.size, Some((1200, 800)));
        assert_eq!(
            (options.focus, options.hover, options.scroll),
            (Some(3), None, 120)
        );
        for bad in [
            ["update", "--theme", "blue"],
            ["update", "--scale", "9"],
            ["update", "--size", "big"],
        ] {
            assert!(matches!(Options::parse(&bad), Err(Error::Arguments(_))));
        }
    }
}
