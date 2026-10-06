//! The pages: what each window shows for a given [`View`].
//!
//! Every page is one file with one function from the view to a [`Content`].
//! To change what a page shows, change its file and look at the result:
//!
//! ```text
//! vinput ui models --mock downloading --picture models.png
//! ```

pub mod about;
pub mod add_model;
pub mod advanced;
mod common;
pub mod dialog;
pub mod general;
pub mod google;
pub mod models;
pub mod pro;
pub mod update;

use vinput_view::{Page, View};

use crate::node::{Content, Icon};

/// The pages listed in the navigation pane, top to bottom, with their names
/// and icons. A page that is not listed is reached from one that is.
pub const NAVIGATION: [(Page, &str, Icon); 5] = [
    (Page::General, "General", Icon::Settings),
    (Page::Models, "Models", Icon::Model),
    (Page::Pro, "VinPUT Pro", Icon::Star),
    (Page::Advanced, "Advanced", Icon::Plug),
    (Page::About, "About", Icon::Info),
];

/// The entry of [`NAVIGATION`] that is shown as selected while `page` is open.
#[must_use]
pub fn navigation_home(page: Page) -> Page {
    match page {
        Page::AddModel | Page::Google => Page::Models,
        other => other,
    }
}

/// What `page` of the Settings window shows for `view`.
#[must_use]
pub fn content(page: Page, view: &View) -> Content {
    match page {
        Page::General => general::content(view),
        Page::Models => models::content(view),
        Page::AddModel => add_model::content(view),
        Page::Google => google::content(view),
        Page::Pro => pro::content(view),
        Page::Advanced => advanced::content(view),
        Page::About => about::content(view),
    }
}
