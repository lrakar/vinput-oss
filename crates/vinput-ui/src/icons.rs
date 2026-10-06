//! The glyph each icon is drawn with.
//!
//! The codes are those of Segoe Fluent Icons, the icon font of Windows 11.
//! Segoe MDL2 Assets, its predecessor on Windows 10, has the same glyphs at
//! the same codes, so one table serves both.

use crate::node::{Icon, Severity};

/// The chevron of a drop-down list.
pub const CHEVRON_DOWN: char = '\u{E70D}';
/// The cross that clears a field.
pub const CLEAR: char = '\u{E894}';
/// What the logo falls back to when the program has no icon of its own.
pub const LOGO: char = '\u{E720}';

/// The glyph for `icon`.
#[must_use]
pub const fn glyph(icon: Icon) -> char {
    match icon {
        Icon::Settings => '\u{E713}',
        Icon::Keyboard => '\u{E765}',
        Icon::Microphone => '\u{E720}',
        Icon::Globe => '\u{E774}',
        Icon::Power => '\u{E7E8}',
        Icon::Volume => '\u{E767}',
        Icon::Sync => '\u{E895}',
        Icon::Timer => '\u{E916}',
        Icon::Model => '\u{F158}',
        Icon::Download => '\u{E896}',
        Icon::Chip => '\u{E950}',
        Icon::Storage => '\u{EDA2}',
        Icon::Folder => '\u{E8B7}',
        Icon::Plug => '\u{E8CE}',
        Icon::Cloud => '\u{E753}',
        Icon::Key => '\u{E8D7}',
        Icon::Memo => '\u{E8D6}',
        Icon::Search => '\u{E721}',
        Icon::Clipboard => '\u{E77F}',
        Icon::OpenExternal => '\u{E8A7}',
        Icon::Info => '\u{E946}',
        Icon::Add => '\u{E710}',
        Icon::Delete => '\u{E74D}',
        Icon::Back => '\u{E72B}',
        Icon::Check => '\u{E73E}',
        Icon::Dismiss => '\u{E711}',
        Icon::Warning => '\u{E7BA}',
        Icon::Error => '\u{EA39}',
        Icon::ChevronRight => '\u{E76C}',
        Icon::More => '\u{E712}',
        Icon::Copy => '\u{E8C8}',
        Icon::Star => '\u{E734}',
        Icon::Person => '\u{E77B}',
    }
}

/// The glyph at the left edge of a message bar.
#[must_use]
pub const fn severity(severity: Severity) -> char {
    match severity {
        Severity::Info => '\u{E946}',
        Severity::Success => '\u{EC61}',
        Severity::Caution => '\u{E7BA}',
        Severity::Critical => '\u{EB90}',
    }
}
