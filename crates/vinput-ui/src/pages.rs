//! The pages: what each window shows for a given [`View`].
//!
//! Every page is one function from the view to a [`Content`]. To change what
//! a page shows, change its file and look at the result:
//!
//! ```text
//! vinput ui models --mock downloading --picture models.png
//! ```
//!
//! The pages of each program of the family are behind a feature of that
//! program's name, so an executable carries only its own.

#[cfg(feature = "vinput")]
pub mod about;
#[cfg(feature = "vinput")]
mod actions;
#[cfg(feature = "vinput")]
pub mod add_model;
#[cfg(feature = "vinput")]
pub mod advanced;
#[cfg(feature = "vinput")]
pub mod cleanup;
mod common;
pub mod dialog;
#[cfg(feature = "vinput")]
pub mod general;
#[cfg(feature = "vinput")]
pub mod google;
#[cfg(feature = "hub")]
pub mod hub;
#[cfg(feature = "vinput")]
mod languages;
#[cfg(feature = "vinput")]
pub mod models;
#[cfg(feature = "vinput")]
pub mod pro;
#[cfg(feature = "voutput")]
pub mod reader;
#[cfg(feature = "vinput")]
pub mod shortcuts;
#[cfg(feature = "vinput")]
pub mod update;
#[cfg(feature = "vinput")]
pub mod welcome;
#[cfg(feature = "vinput")]
pub mod words;

use vinput_view::{App, Page, View};

use crate::node::{Content, Icon};

/// An entry of a navigation pane: the page, its name and its icon.
pub type Entry = (Page, &'static str, Icon);

/// The pages listed in Vinput's navigation pane, top to bottom, with their
/// names and icons. A page that is not listed is reached from one that is.
#[cfg(feature = "vinput")]
pub const NAVIGATION: [Entry; 8] = [
    (Page::General, "General", Icon::Settings),
    (Page::Shortcuts, "Shortcuts", Icon::Keyboard),
    (Page::Models, "Models", Icon::Model),
    (Page::Words, "Words", Icon::Dictionary),
    (Page::Cleanup, "Cleanup", Icon::Edit),
    (Page::Pro, "VinPUT Pro", Icon::Star),
    (Page::Advanced, "Advanced", Icon::Plug),
    (Page::About, "About", Icon::Info),
];

/// The pages listed in the navigation pane of `app`, top to bottom.
#[must_use]
pub fn navigation(app: App) -> &'static [Entry] {
    match app {
        #[cfg(feature = "vinput")]
        App::Vinput => &NAVIGATION,
        #[cfg(feature = "hub")]
        App::Hub => &hub::NAVIGATION,
        #[cfg(feature = "voutput")]
        App::Voutput => &reader::NAVIGATION,
        #[allow(unreachable_patterns)]
        _ => &[],
    }
}

/// What `app` is called at the head of its navigation pane and in the
/// titles of its windows.
#[must_use]
pub fn name(app: App) -> &'static str {
    match app {
        App::Vinput => "VinPUT",
        #[cfg(feature = "hub")]
        App::Hub => "PUTLAB",
        #[cfg(feature = "voutput")]
        App::Voutput => "Voutput",
    }
}

/// The entry of the navigation pane that is shown as selected while `page`
/// is open.
#[must_use]
pub fn navigation_home(page: Page) -> Page {
    match page {
        Page::AddModel | Page::Google => Page::Models,
        other => other,
    }
}

/// A page of a program whose pages this executable does not carry.
#[allow(dead_code)]
fn absent() -> Content {
    Content {
        title: String::new(),
        pinned: Vec::new(),
        body: Vec::new(),
        footer: Vec::new(),
        back: None,
    }
}

/// What `page` of the Settings window shows for `view`.
#[must_use]
pub fn content(page: Page, view: &View) -> Content {
    match page {
        #[cfg(feature = "vinput")]
        Page::General => general::content(view),
        #[cfg(feature = "vinput")]
        Page::Shortcuts => shortcuts::content(view),
        #[cfg(feature = "vinput")]
        Page::Models => models::content(view),
        #[cfg(feature = "vinput")]
        Page::AddModel => add_model::content(view),
        #[cfg(feature = "vinput")]
        Page::Google => google::content(view),
        #[cfg(feature = "vinput")]
        Page::Pro => pro::content(view),
        #[cfg(feature = "vinput")]
        Page::Words => words::content(view),
        #[cfg(feature = "vinput")]
        Page::Cleanup => cleanup::content(view),
        #[cfg(feature = "vinput")]
        Page::Advanced => advanced::content(view),
        #[cfg(feature = "vinput")]
        Page::About => about::content(view),
        #[cfg(feature = "vinput")]
        Page::Welcome | Page::WelcomePro | Page::WelcomeLocal => welcome::content(page, view),
        #[cfg(feature = "hub")]
        Page::Products => hub::products(view),
        #[cfg(feature = "hub")]
        Page::Account => hub::account(view),
        #[cfg(feature = "hub")]
        Page::HubAbout => hub::about(view),
        #[cfg(feature = "voutput")]
        Page::Reading => reader::reading(view),
        #[cfg(feature = "voutput")]
        Page::Voices => reader::voices(view),
        #[cfg(feature = "voutput")]
        Page::Summaries => reader::summaries(view),
        #[cfg(feature = "voutput")]
        Page::ReaderAdvanced => reader::advanced(view),
        #[cfg(feature = "voutput")]
        Page::ReaderAbout => reader::about(view),
        #[cfg(not(feature = "vinput"))]
        _ => {
            let _ = view;
            absent()
        }
    }
}

/// What the window that offers a newer version shows for `view`.
#[must_use]
pub fn update(view: &View) -> Content {
    #[cfg(feature = "vinput")]
    {
        update::content(view)
    }
    #[cfg(not(feature = "vinput"))]
    {
        let _ = view;
        absent()
    }
}
