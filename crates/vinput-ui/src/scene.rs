//! What a window is showing: which page of which window, and how its
//! content is made from the latest view.

use vinput_view::{Dialog, Page, Surface, View};

#[cfg(any(debug_assertions, test))]
use crate::gallery;
use crate::node::{Button, ButtonKind, Content, Node, Severity, info_bar, note};
use crate::pages;
use crate::shell::Chrome;
use crate::theme::metric;

/// The canned scenes that show a window which asks or tells, besides the
/// scenes of [`crate::mock::SCENES`].
pub const DIALOG_SCENES: [&str; 5] = [
    "dialog-setup",
    "dialog-working",
    "dialog-failed",
    "dialog-question",
    "dialog-message",
];

/// What a window is showing.
#[derive(Debug, Clone, PartialEq)]
pub enum Showing {
    /// The Settings window on a page.
    Settings(Page),
    /// The update window.
    Update,
    /// The test card, inside the Settings window.
    Gallery,
    /// A window that asks or tells.
    Asking(Asking),
}

/// A window that asks or tells, and where its answer stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asking {
    /// What it asks.
    pub dialog: Dialog,
    /// How far the thing it asked about is.
    pub phase: Phase,
}

/// How far the work is that a yes sets off.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Phase {
    /// Nobody has answered yet.
    #[default]
    Asking,
    /// The work is under way; these words say so.
    Working(String),
    /// The work failed, for this reason, and can be tried again.
    Failed(String),
}

/// The caption of the button that says yes after the work failed once.
const TRY_AGAIN: &str = "Try again";

impl Asking {
    /// A window that asks `dialog` and has no answer yet.
    #[must_use]
    pub fn new(dialog: Dialog) -> Self {
        Self {
            dialog,
            phase: Phase::Asking,
        }
    }

    /// What the window shows.
    #[must_use]
    pub fn content(&self) -> Content {
        let mut content = pages::dialog::content(&self.dialog);
        let accent = |node: &mut Node, change: &dyn Fn(&mut Button)| {
            if let Node::Button(button) = node
                && button.kind == ButtonKind::Accent
            {
                change(button);
            }
        };
        match &self.phase {
            Phase::Asking => {}
            Phase::Working(words) => {
                content.body.push(Node::Progress(None));
                content.body.push(note(words));
                for node in &mut content.footer {
                    if let Node::Button(button) = node {
                        button.enabled = false;
                    }
                }
            }
            Phase::Failed(reason) => {
                content.body.push(info_bar(Severity::Critical, reason));
                for node in &mut content.footer {
                    accent(node, &|button| TRY_AGAIN.clone_into(&mut button.label));
                }
            }
        }
        content
    }
}

impl Showing {
    /// What a window process shows first for `surface`.
    #[must_use]
    pub fn of(surface: Surface) -> Self {
        match surface {
            Surface::Settings(page) => Self::Settings(page),
            Surface::Update => Self::Update,
        }
    }

    /// The content for `view`.
    #[must_use]
    pub fn content(&self, view: &View) -> Content {
        match self {
            Self::Settings(page) => pages::content(*page, view),
            Self::Update => pages::update::content(view),
            #[cfg(any(debug_assertions, test))]
            Self::Gallery => gallery::content(),
            // The build that ships has no test card and never shows one.
            #[cfg(not(any(debug_assertions, test)))]
            Self::Gallery => pages::content(Page::General, view),
            Self::Asking(asking) => asking.content(),
        }
    }

    /// What is around the content.
    #[must_use]
    pub fn chrome<'a>(&self, view: &'a View) -> Chrome<'a> {
        match self {
            Self::Settings(page) if page.is_welcome() => Chrome::Focused,
            Self::Settings(page) => Chrome::Settings {
                page: *page,
                status: &view.status,
            },
            Self::Gallery => Chrome::Settings {
                page: Page::General,
                status: &view.status,
            },
            Self::Update | Self::Asking(_) => Chrome::Compact,
        }
    }

    /// The title of the window.
    #[must_use]
    pub fn title(&self) -> &str {
        match self {
            Self::Settings(_) | Self::Gallery => "VinPUT settings",
            Self::Update => "VinPUT update",
            Self::Asking(asking) if asking.dialog.title.is_empty() => "VinPUT",
            Self::Asking(asking) => &asking.dialog.title,
        }
    }

    /// Whether the window can be resized by the user.
    #[must_use]
    pub fn resizable(&self) -> bool {
        matches!(self, Self::Settings(_) | Self::Gallery)
    }

    /// The size the window opens with, if it does not depend on what it
    /// says; a window that asks is as tall as its content.
    #[must_use]
    pub fn size(&self) -> Option<(f32, f32)> {
        match self {
            Self::Settings(_) | Self::Gallery => Some(metric::SETTINGS),
            Self::Update => Some(metric::UPDATE),
            Self::Asking(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Asking {
        Asking::new(Dialog {
            title: "VinPUT setup".to_owned(),
            heading: "Install VinPUT".to_owned(),
            primary: "Install".to_owned(),
            secondary: "Cancel".to_owned(),
            ..Dialog::default()
        })
    }

    fn buttons(content: &Content) -> Vec<(String, bool)> {
        content
            .footer
            .iter()
            .filter_map(|node| match node {
                Node::Button(button) => Some((button.label.clone(), button.enabled)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn work_under_way_holds_the_buttons_and_shows_a_moving_bar() {
        let mut asking = setup();
        let rest = asking.content().body.len();
        asking.phase = Phase::Working("Installing".to_owned());
        let content = asking.content();
        assert_eq!(content.body.len(), rest + 2);
        assert!(matches!(content.body[rest], Node::Progress(None)));
        assert!(buttons(&content).iter().all(|(_, enabled)| !enabled));
    }

    #[test]
    fn a_failure_gives_its_reason_and_offers_another_try() {
        let mut asking = setup();
        asking.phase = Phase::Failed("The disk is full.".to_owned());
        let content = asking.content();
        assert!(matches!(
            content.body.last(),
            Some(Node::InfoBar { severity: Severity::Critical, text, .. }) if text == "The disk is full."
        ));
        assert_eq!(
            buttons(&content),
            [(TRY_AGAIN.to_owned(), true), ("Cancel".to_owned(), true)]
        );
    }

    #[test]
    fn each_window_knows_its_title_and_its_frame() {
        let view = View::default();
        assert_eq!(Showing::of(Surface::Update), Showing::Update);
        assert_eq!(Showing::Update.title(), "VinPUT update");
        assert_eq!(Showing::Asking(setup()).title(), "VinPUT setup");
        assert!(Showing::Settings(Page::About).resizable());
        assert!(!Showing::Update.resizable());
        assert!(matches!(Showing::Update.chrome(&view), Chrome::Compact));
        assert!(Showing::Asking(setup()).size().is_none());
    }
}
