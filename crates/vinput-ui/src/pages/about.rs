//! The About page: version, updates and links.

use vinput_view::{Change, Command, Download, Page, Release, Update, UpdateCheck, View};

use super::common::{SiteLink, fraction, identity, links, notice_bar, related, so_far};
use crate::node::{
    Action, Content, Icon, Node, Setting, TextStyle, Tone, accent_button, bullets, button, card,
    header, human_size, setting, styled, toggle,
};

/// The site.
const WEBSITE: &str = "https://vinput-dasvcr.web.app";
/// The manual on the site.
const DOCUMENTATION: &str = "https://vinput-dasvcr.web.app/docs";
/// What each version brought, on the site.
const RELEASE_NOTES: &str = "https://vinput-dasvcr.web.app/changelog";

/// What the page shows for `view`.
#[must_use]
pub fn content(view: &View) -> Content {
    let mut body = Vec::new();
    body.extend(notice_bar(view, Page::About));
    body.push(identity(
        Icon::Microphone,
        "VinPUT",
        "Type with your voice in any app. By PUTLAB",
        &view.version,
    ));
    body.extend(updates(view));
    body.extend(links(&LINKS));
    body.extend(related(view));
    Content {
        pinned: Vec::new(),
        title: "About".to_owned(),
        body,
        footer: Vec::new(),
        back: None,
    }
}

/// Where updates stand, and whether the app looks for them by itself.
fn updates(view: &View) -> [Node; 2] {
    [
        header("Updates"),
        card([
            update_row(&view.update, &view.version).into(),
            setting(Icon::Sync, "Check for updates automatically")
                .detail("Looks for a newer version when VinPUT starts and every few hours")
                .control(toggle(view.settings.check_updates, |on| {
                    Action::send(Command::Set(Change::CheckUpdates(on)))
                }))
                .into(),
        ]),
    ]
}

/// The row that says where updates stand and offers the next thing to do. A
/// download that has started says more than the check that led to it.
fn update_row(update: &Update, running: &str) -> Setting {
    let release = update.release.as_ref();
    match &update.download {
        Download::Running { done, total } => downloading(release, *done, *total),
        Download::Installing => setting(Icon::Download, named("Installing", release))
            .detail("VinPUT restarts in a moment")
            .below(Node::Progress(None)),
        Download::Failed { reason } => offer(release, Some(reason)),
        Download::Idle => checked(update, running),
    }
}

/// `Downloading version 0.3.1`, or `Downloading the update` when the version
/// is not known.
fn named(doing: &str, release: Option<&Release>) -> String {
    match release {
        Some(release) => format!("{doing} version {}", release.version),
        None => format!("{doing} the update"),
    }
}

/// The row while the update is being downloaded.
fn downloading(release: Option<&Release>, done: u64, total: u64) -> Setting {
    let detail = if total == 0 {
        "Starting the download".to_owned()
    } else {
        so_far(done, total)
    };
    setting(Icon::Download, named("Downloading", release))
        .detail(detail)
        .control(button("Cancel", Action::send(Command::CancelUpdate)).into())
        .below(Node::Progress(fraction(done, total)))
}

/// The row that offers a newer version: what it is, how big, what is new.
/// `failure` is why the last attempt to install it did not work.
fn offer(release: Option<&Release>, failure: Option<&str>) -> Setting {
    let title = release.map_or_else(
        || "A newer version is available".to_owned(),
        |release| format!("Version {} is available", release.version),
    );
    let install = accent_button("Install update", Action::send(Command::ShowUpdate));
    let mut row = setting(Icon::Download, title).control(install.into());
    row = match (failure, release) {
        (Some(reason), _) => row.detail(reason).detail_tone(Tone::Critical),
        (None, Some(release)) if release.bytes > 0 => {
            row.detail(format!("A {} download", human_size(release.bytes)))
        }
        (None, _) => row,
    };
    let notes = release.map_or(&[][..], |release| release.notes.as_slice());
    if notes.iter().any(|note| !note.trim().is_empty()) {
        row = row
            .below(styled("What is new", TextStyle::BodyStrong, Tone::Primary))
            .below(bullets(notes));
    }
    row
}

/// The row while nothing is being downloaded: what the last check found.
fn checked(update: &Update, running: &str) -> Setting {
    let check = || button("Check for updates", Action::send(Command::CheckForUpdates));
    match &update.check {
        UpdateCheck::Unchecked => setting(Icon::Sync, "VinPUT has not checked for updates yet")
            .detail("Look now to see whether a newer version is out")
            .control(check().into()),
        UpdateCheck::Checking => setting(Icon::Sync, "Checking for updates")
            .control(check().enabled(false).into())
            .below(Node::Progress(None)),
        UpdateCheck::UpToDate => setting(Icon::Check, "VinPUT is up to date")
            .detail(if running.is_empty() {
                "You have the newest version".to_owned()
            } else {
                format!("Version {running} is the newest")
            })
            .control(check().into()),
        UpdateCheck::Available => offer(update.release.as_ref(), None),
        UpdateCheck::Failed { reason } => setting(Icon::Warning, "Could not check for updates")
            .detail(reason)
            .detail_tone(Tone::Critical)
            .control(button("Try again", Action::send(Command::CheckForUpdates)).into()),
    }
}

/// Where to read more.
const LINKS: [SiteLink; 3] = [
    (Icon::Globe, "Website", "vinput-dasvcr.web.app", WEBSITE),
    (
        Icon::Info,
        "Documentation",
        "How to use VinPUT and build on it",
        DOCUMENTATION,
    ),
    (
        Icon::Clipboard,
        "Release notes",
        "What each version brought",
        RELEASE_NOTES,
    ),
];

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::super::common::probe::{
        all, buttons, command, flipped, keys, scene, setting_row, settings,
    };
    use super::*;
    use crate::mock;
    use crate::node::{ButtonKind, Severity};

    /// The captions of the buttons and what pressing each sends.
    fn offered(nodes: &[&Node]) -> Vec<(String, Option<Command>)> {
        buttons(nodes)
            .into_iter()
            .map(|button| (button.label.clone(), command(&button.action)))
            .collect()
    }

    fn texts<'a>(nodes: &[&'a Node]) -> Vec<&'a str> {
        nodes
            .iter()
            .filter_map(|node| match node {
                Node::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    fn progress(nodes: &[&Node]) -> Vec<Option<f32>> {
        nodes
            .iter()
            .filter_map(|node| match node {
                Node::Progress(fraction) => Some(*fraction),
                _ => None,
            })
            .collect()
    }

    /// The page with updates in the state `update`.
    fn with(update: Update) -> Content {
        let mut view = scene("settings");
        view.update = update;
        content(&view)
    }

    #[test]
    fn it_says_what_this_is_and_which_version() {
        let page = content(&scene("settings"));
        let nodes = all(&page.body);
        assert_eq!(page.title, "About");
        let app = setting_row(&nodes, "VinPUT");
        assert_eq!(app.detail, "Type with your voice in any app. By PUTLAB");
        assert!(matches!(
            app.control.as_deref(),
            Some(Node::Text { text, .. }) if text == "Version 0.3.0"
        ));
        let headers: Vec<_> = nodes
            .iter()
            .filter_map(|node| match node {
                Node::Header(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(headers, ["Updates", "Links", "Related"]);
        // The way to the hub opens it, like the links, outside this window.
        assert_eq!(
            command(&setting_row(&nodes, "Open PUTLAB").action),
            Some(Command::OpenHub)
        );
    }

    #[test]
    fn nothing_on_the_page_needs_a_key() {
        for name in mock::SCENES {
            let page = content(&scene(name));
            let keys = keys(&all(&page.body));
            let unique: HashSet<_> = keys.iter().collect();
            assert_eq!(unique.len(), keys.len(), "{name}: {keys:?}");
            let titles: Vec<_> = settings(&page.body)
                .iter()
                .map(|row| row.title.clone())
                .collect();
            let unique: HashSet<_> = titles.iter().collect();
            assert_eq!(unique.len(), titles.len(), "{name}: {titles:?}");
        }
    }

    #[test]
    fn before_a_check_it_offers_one() {
        let page = content(&scene("settings"));
        let nodes = all(&page.body);
        setting_row(&nodes, "VinPUT has not checked for updates yet");
        assert_eq!(
            offered(&nodes),
            [(
                "Check for updates".to_owned(),
                Some(Command::CheckForUpdates)
            )]
        );
        assert!(progress(&nodes).is_empty());
    }

    #[test]
    fn the_switch_for_automatic_checks_sends_its_new_position() {
        let page = content(&scene("settings"));
        let nodes = all(&page.body);
        let switch = setting_row(&nodes, "Check for updates automatically");
        assert!(matches!(
            switch.control.as_deref(),
            Some(Node::Toggle { on: true, .. })
        ));
        assert_eq!(flipped(switch, false), Some(Change::CheckUpdates(false)));
        assert_eq!(flipped(switch, true), Some(Change::CheckUpdates(true)));
    }

    #[test]
    fn every_outcome_of_a_check_has_its_own_line() {
        let state = |check| {
            let page = with(Update {
                check,
                ..Update::default()
            });
            let first = settings(&page.body)[1];
            (first.title.clone(), first.detail.clone(), first.detail_tone)
        };
        let failed = UpdateCheck::Failed {
            reason: "No connection to the site".to_owned(),
        };
        let lines = [
            state(UpdateCheck::Unchecked),
            state(UpdateCheck::Checking),
            state(UpdateCheck::UpToDate),
            state(UpdateCheck::Available),
            state(failed),
        ];
        let titles: HashSet<_> = lines.iter().map(|line| line.0.as_str()).collect();
        assert_eq!(titles.len(), lines.len(), "{lines:?}");
        assert_eq!(lines[1].0, "Checking for updates");
        assert_eq!(lines[2].0, "VinPUT is up to date");
        assert_eq!(lines[2].1, "Version 0.3.0 is the newest");
        assert_eq!(lines[3].0, "A newer version is available");
        assert_eq!(
            (lines[4].1.as_str(), lines[4].2),
            ("No connection to the site", Tone::Critical)
        );
    }

    #[test]
    fn a_check_under_way_cannot_be_started_again() {
        let page = with(Update {
            check: UpdateCheck::Checking,
            ..Update::default()
        });
        let nodes = all(&page.body);
        assert!(matches!(buttons(&nodes).as_slice(), [button] if !button.enabled));
        assert_eq!(progress(&nodes), [None]);
    }

    #[test]
    fn a_failed_check_can_be_tried_again() {
        let page = with(Update {
            check: UpdateCheck::Failed {
                reason: "No connection to the site".to_owned(),
            },
            ..Update::default()
        });
        assert_eq!(
            offered(&all(&page.body)),
            [("Try again".to_owned(), Some(Command::CheckForUpdates))]
        );
    }

    #[test]
    fn a_newer_version_is_offered_with_what_is_new() {
        let page = content(&scene("update"));
        let nodes = all(&page.body);
        let offer = setting_row(&nodes, "Version 0.3.1 is available");
        assert_eq!(offer.detail, "A 6 MB download");
        let [install] = buttons(&nodes)[..] else {
            unreachable!("one button was expected");
        };
        assert_eq!(install.label, "Install update");
        assert_eq!(install.kind, ButtonKind::Accent);
        assert_eq!(command(&install.action), Some(Command::ShowUpdate));
        assert!(texts(&nodes).contains(&"What is new"));
        let [
            Node::Text {
                style: TextStyle::BodyStrong,
                ..
            },
            Node::List(points),
        ] = offer.below.as_slice()
        else {
            unreachable!("a heading and one list of notes were expected");
        };
        assert_eq!(points.len(), 3);
        assert!(points.contains(&"Settings take effect as you change them".to_owned()));
    }

    #[test]
    fn a_download_shows_how_far_it_is_and_can_be_cancelled() {
        let page = content(&scene("update-downloading"));
        let nodes = all(&page.body);
        let download = setting_row(&nodes, "Downloading version 0.3.1");
        assert_eq!(download.detail, "4 MB of 6 MB");
        assert_eq!(
            offered(&nodes),
            [("Cancel".to_owned(), Some(Command::CancelUpdate))]
        );
        let [Some(fraction)] = progress(&nodes)[..] else {
            unreachable!("one bar that knows how far it is was expected");
        };
        assert!((0.61..0.63).contains(&fraction), "{fraction}");
    }

    #[test]
    fn the_end_of_a_download_is_told_either_way() {
        let found = scene("update").update.release;
        let page = with(Update {
            check: UpdateCheck::Available,
            release: found.clone(),
            download: Download::Installing,
        });
        let nodes = all(&page.body);
        setting_row(&nodes, "Installing version 0.3.1");
        assert!(buttons(&nodes).is_empty());
        assert_eq!(progress(&nodes), [None]);

        let page = with(Update {
            check: UpdateCheck::Available,
            release: found,
            download: Download::Failed {
                reason: "The download did not match its checksum".to_owned(),
            },
        });
        let nodes = all(&page.body);
        let offer = setting_row(&nodes, "Version 0.3.1 is available");
        assert_eq!(offer.detail, "The download did not match its checksum");
        assert_eq!(offer.detail_tone, Tone::Critical);
        assert_eq!(
            offered(&nodes),
            [("Install update".to_owned(), Some(Command::ShowUpdate))]
        );
    }

    #[test]
    fn the_links_open_pages_of_the_site() {
        let page = content(&scene("settings"));
        let nodes = all(&page.body);
        let opened: Vec<_> = ["Website", "Documentation", "Release notes"]
            .into_iter()
            .map(|title| command(&setting_row(&nodes, title).action))
            .collect();
        assert_eq!(
            opened,
            [
                Some(Command::OpenLink(
                    "https://vinput-dasvcr.web.app".to_owned()
                )),
                Some(Command::OpenLink(
                    "https://vinput-dasvcr.web.app/docs".to_owned()
                )),
                Some(Command::OpenLink(
                    "https://vinput-dasvcr.web.app/changelog".to_owned()
                )),
            ]
        );
    }

    #[test]
    fn a_notice_for_this_page_is_shown_and_can_be_dismissed() {
        let mut view = scene("notice");
        let bar = |view: &View| matches!(content(view).body.first(), Some(Node::InfoBar { .. }));
        assert!(!bar(&view), "it is about General");
        if let Some(notice) = &mut view.notice {
            notice.page = Page::About;
        }
        assert!(bar(&view));
        let page = content(&view);
        let Some(Node::InfoBar {
            severity: Severity::Critical,
            dismiss: Some(dismiss),
            ..
        }) = page.body.first()
        else {
            unreachable!("the notice comes first, with a cross");
        };
        assert_eq!(command(dismiss), Some(Command::DismissNotice));
    }
}
