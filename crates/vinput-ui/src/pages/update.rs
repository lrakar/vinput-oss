//! The update window: a newer version and its download.
//!
//! A small window of its own, with no navigation. It says which version is
//! on offer and what is new in it, and its buttons follow the download:
//! install or later, cancel while it downloads, try again if it failed.

use vinput_view::{Command, Download, Release, UpdateCheck, View};

use super::common::{fraction, so_far};
use crate::node::{
    Action, Content, Icon, Node, Severity, TextStyle, Tone, accent_button, bullets, button, card,
    header, human_size, info_bar, note, setting, styled,
};

/// What the page shows for `view`.
#[must_use]
pub fn content(view: &View) -> Content {
    match &view.update.release {
        Some(release) => offer(release, &view.version, &view.update.download),
        None => nothing_to_install(&view.update.check, &view.version),
    }
}

/// A newer version, what is in it and how far installing it is.
fn offer(release: &Release, running: &str, download: &Download) -> Content {
    let mut body = vec![quiet(format!(
        "You have version {running}. Your settings and speech models are kept."
    ))];
    body.extend(whats_new(&release.notes));
    body.push(progress(download, release.bytes));
    Content {
        pinned: Vec::new(),
        title: format!("Version {} is available", release.version),
        body,
        footer: buttons(download),
        back: None,
    }
}

/// Text that explains, in the quieter color.
fn quiet(words: impl Into<String>) -> Node {
    styled(words, TextStyle::Body, Tone::Secondary)
}

/// What is new in the release, as a list in a card; nothing when no note
/// says anything.
fn whats_new(notes: &[String]) -> Vec<Node> {
    if notes.iter().all(|note| note.trim().is_empty()) {
        Vec::new()
    } else {
        vec![header("What is new"), card([bullets(notes)])]
    }
}

/// Where the download stands: its size before it starts, then how far it is.
fn progress(download: &Download, bytes: u64) -> Node {
    match download {
        Download::Idle => note(format!("Download size: {}", human_size(bytes))),
        Download::Running { done, total } => card([setting(Icon::Download, "Downloading")
            .detail(so_far(*done, *total))
            .below(Node::Progress(fraction(*done, *total)))
            .into()]),
        Download::Installing => card([setting(Icon::Sync, "Installing")
            .detail("VinPUT will restart in a moment")
            .below(Node::Progress(None))
            .into()]),
        Download::Failed { reason } => info_bar(
            Severity::Critical,
            format!("The update was not installed. {reason}"),
        ),
    }
}

/// The buttons at the bottom, for where the download stands.
fn buttons(download: &Download) -> Vec<Node> {
    let install = |label: &str| accent_button(label, Action::send(Command::InstallUpdate)).into();
    let later = || button("Later", Action::Close).into();
    match download {
        Download::Idle => vec![install("Install update"), later()],
        Download::Running { .. } => {
            vec![button("Cancel", Action::send(Command::CancelUpdate)).into()]
        }
        // Setup has taken over; there is nothing left to decide.
        Download::Installing => Vec::new(),
        Download::Failed { .. } => vec![install("Try again"), later()],
    }
}

/// The window when there is no newer version to install.
fn nothing_to_install(check: &UpdateCheck, running: &str) -> Content {
    let close = || button("Close", Action::Close).into();
    let (title, body, footer) = match check {
        UpdateCheck::Checking => (
            "Looking for a newer version",
            vec![
                quiet(format!("You have version {running}.")),
                Node::Progress(None),
            ],
            vec![close()],
        ),
        UpdateCheck::Failed { reason } => (
            "Could not look for a newer version",
            vec![
                info_bar(Severity::Critical, reason),
                quiet(format!("You have version {running}.")),
            ],
            vec![
                accent_button("Try again", Action::send(Command::CheckForUpdates)).into(),
                close(),
            ],
        ),
        UpdateCheck::Unchecked | UpdateCheck::UpToDate | UpdateCheck::Available => (
            "You are up to date",
            vec![quiet(format!(
                "Version {running} is the newest version of VinPUT."
            ))],
            vec![accent_button("Close", Action::Close).into()],
        ),
    };
    Content {
        pinned: Vec::new(),
        title: title.to_owned(),
        body,
        footer,
        back: None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::common::probe::{Seen, command, scene};
    use super::*;
    use crate::node::ButtonKind;

    /// The buttons of the footer: caption, whether it is the accent one and
    /// what it does (a command, or closing the window).
    fn footer(content: &Content) -> Vec<(String, bool, Option<Command>, bool)> {
        let seen = Seen::of(&content.footer);
        seen.presses
            .iter()
            .map(|press| {
                (
                    press.label.clone(),
                    press.kind == ButtonKind::Accent,
                    command(&press.action),
                    matches!(press.action, Action::Close),
                )
            })
            .collect()
    }

    fn name(text: &str) -> String {
        text.to_owned()
    }

    #[test]
    fn an_update_on_offer_can_be_installed_or_left_for_later() {
        let content = content(&scene("update"));
        assert_eq!(content.title, "Version 0.3.1 is available");
        assert!(content.back.is_none());
        let seen = Seen::of(&content.body);
        assert!(seen.says("You have version 0.3.0."));
        assert!(seen.says("settings and speech models are kept"));
        assert!(seen.says("Lighter in the background"));
        assert!(matches!(
            content.body.as_slice(),
            [_, Node::Header(heading), Node::Card(inside), _]
                if heading == "What is new"
                    && matches!(inside.as_slice(), [Node::List(points)] if points.len() == 3)
        ));
        assert!(seen.says("Download size: 6 MB"));
        assert!(seen.progress.is_empty() && seen.presses.is_empty());
        assert_eq!(
            footer(&content),
            [
                (
                    name("Install update"),
                    true,
                    Some(Command::InstallUpdate),
                    false
                ),
                (name("Later"), false, None, true),
            ]
        );
    }

    #[test]
    fn a_download_under_way_can_only_be_cancelled() {
        let content = content(&scene("update-downloading"));
        assert_eq!(content.title, "Version 0.3.1 is available");
        let seen = Seen::of(&content.body);
        assert!(seen.says("4 MB of 6 MB"));
        assert!(matches!(seen.progress.as_slice(), [Some(part)] if (0.61..0.63).contains(part)));
        assert!(!seen.says("Download size"));
        assert_eq!(
            footer(&content),
            [(name("Cancel"), false, Some(Command::CancelUpdate), false)]
        );
    }

    #[test]
    fn installing_leaves_nothing_to_press() {
        let mut view = scene("update");
        view.update.download = Download::Installing;
        let content = content(&view);
        let seen = Seen::of(&content.body);
        assert!(seen.says("Installing") && seen.says("VinPUT will restart"));
        assert_eq!(seen.progress, [None]);
        assert!(footer(&content).is_empty());
    }

    #[test]
    fn a_failed_download_gives_its_reason_and_can_be_tried_again() {
        let mut view = scene("update");
        view.update.download = Download::Failed {
            reason: name("The connection was lost."),
        };
        let content = content(&view);
        let bars = Seen::of(&content.body).bars;
        assert!(
            matches!(bars.as_slice(), [(Severity::Critical, text)] if text.ends_with("The connection was lost."))
        );
        assert_eq!(
            footer(&content),
            [
                (name("Try again"), true, Some(Command::InstallUpdate), false),
                (name("Later"), false, None, true),
            ]
        );
    }

    #[test]
    fn without_a_release_the_window_says_where_the_check_stands() {
        let mut view = scene("settings");
        let up_to_date = content(&view);
        assert_eq!(up_to_date.title, "You are up to date");
        assert!(Seen::of(&up_to_date.body).says("Version 0.3.0 is the newest"));
        assert_eq!(footer(&up_to_date), [(name("Close"), true, None, true)]);

        view.update.check = UpdateCheck::Checking;
        let checking = content(&view);
        assert_eq!(Seen::of(&checking.body).progress, [None]);
        assert_eq!(footer(&checking), [(name("Close"), false, None, true)]);

        view.update.check = UpdateCheck::Failed {
            reason: name("No connection."),
        };
        let failed = content(&view);
        assert_eq!(
            Seen::of(&failed.body).bars,
            [(Severity::Critical, name("No connection."))]
        );
        assert_eq!(
            footer(&failed),
            [
                (
                    name("Try again"),
                    true,
                    Some(Command::CheckForUpdates),
                    false
                ),
                (name("Close"), false, None, true),
            ]
        );
    }

    #[test]
    fn a_release_without_notes_has_no_empty_list() {
        let mut view = scene("update");
        if let Some(release) = &mut view.update.release {
            release.notes = vec![name("  ")];
        }
        let content = content(&view);
        assert!(!Seen::of(&content.body).says("What is new"));
        assert_eq!(content.body.len(), 2, "what you have and the download size");
    }
}
