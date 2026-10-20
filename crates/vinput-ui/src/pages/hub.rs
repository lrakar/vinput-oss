//! The pages of the PUTLAB hub: the programs of the family, the one account
//! they share, and the hub itself.
//!
//! A program is a card that carries its own actions, so nothing has to be
//! selected first: `Install` on one that is not on this PC, `Settings` and, when
//! there is one, `Update` on one that is. The hub manages; it is not needed
//! for anything to run, and every program keeps itself up to date without
//! it.

use vinput_view::{
    Catalog, Command, HubAsk, Page, Pro, ProDoing, ProTier, ProWho, Product, ProductState, View,
};

use super::Entry;
use super::common::{DOT, date, day, fraction, identity, links, notice_bar, price, so_far};
use crate::node::{
    Action, ButtonKind, Content, Icon, Node, Severity, Tone, accent_button, badge, button, card,
    header, human_size, icon_button, info_bar, link, mono, note, row, setting,
};

/// The pages listed in the hub's navigation pane, top to bottom.
pub const NAVIGATION: [Entry; 3] = [
    (Page::Products, "Apps", Icon::Download),
    (Page::Account, "Account", Icon::Person),
    (Page::HubAbout, "About", Icon::Info),
];

/// Where the programs are described and can be downloaded one by one.
const SITE: &str = "https://vinput-dasvcr.web.app";

fn ask(ask: HubAsk) -> Action {
    Action::send(Command::Hub(ask))
}

fn page(title: &str, body: Vec<Node>) -> Content {
    Content {
        title: title.to_owned(),
        pinned: Vec::new(),
        body,
        footer: Vec::new(),
        back: None,
    }
}

/// The Apps page: every program of the family and what can be done with it.
#[must_use]
pub fn products(view: &View) -> Content {
    let hub = &view.hub;
    let mut body = Vec::new();
    body.extend(notice_bar(view, Page::Products));
    if let Catalog::Failed { reason } = &hub.catalog {
        body.push(Node::InfoBar {
            severity: Severity::Caution,
            text: format!(
                "The list of apps could not be fetched. {reason} What is installed is shown as \
                 it is."
            ),
            action: Some(("Try again".to_owned(), ask(HubAsk::Refresh))),
            dismiss: None,
        });
    }
    let (installed, rest): (Vec<&Product>, Vec<&Product>) = hub
        .products
        .iter()
        .partition(|product| product.installed.is_some());
    if !installed.is_empty() {
        body.push(header("On this PC"));
        body.extend(installed.into_iter().map(product_card));
    }
    if !rest.is_empty() {
        body.push(header("Available"));
        body.extend(rest.into_iter().map(product_card));
    }
    if hub.products.is_empty() && hub.catalog == Catalog::Loading {
        body.push(Node::Busy("Looking for apps".to_owned()));
    }
    body.push(note(
        "Each app works on its own and keeps itself up to date. The hub is only where they are \
         installed from.",
    ));
    page("Apps", body)
}

/// The icon a program goes by.
fn icon_of(product: &Product) -> Icon {
    match product.id.as_str() {
        "vinput" => Icon::Microphone,
        "voutput" => Icon::Volume,
        _ => Icon::Model,
    }
}

/// The line under a program's name: what is being done with it, or what is
/// on this PC and what is on offer.
fn standing(product: &Product) -> (String, Tone) {
    match &product.state {
        ProductState::Failed { reason } => return (reason.clone(), Tone::Critical),
        ProductState::Downloading { done, total } => {
            let line = format!("Downloading{DOT}{}", so_far(*done, *total));
            return (line, Tone::Secondary);
        }
        ProductState::Installing => return ("Installing".to_owned(), Tone::Secondary),
        ProductState::Removing => return ("Removing".to_owned(), Tone::Secondary),
        ProductState::Idle => {}
    }
    let mut parts = Vec::new();
    match (&product.installed, &product.latest) {
        (Some(installed), Some(latest)) if product.has_update() => {
            parts.push(format!("Version {installed}"));
            parts.push(format!("{latest} is available"));
        }
        (Some(installed), _) if installed.is_empty() => parts.push("Installed".to_owned()),
        (Some(installed), _) => parts.push(format!("Version {installed}")),
        (None, _) if product.soon => parts.push("Not available yet".to_owned()),
        (None, Some(latest)) => {
            parts.push(format!("Version {latest}"));
            if product.bytes > 0 {
                parts.push(human_size(product.bytes));
            }
        }
        (None, None) => {}
    }
    parts.push(product.summary.trim_end_matches('.').to_owned());
    (parts.join(DOT), Tone::Secondary)
}

/// What is at the right edge of a program's card, as it stands.
fn controls(product: &Product) -> Vec<Node> {
    let id = || product.id.clone();
    let mut controls: Vec<Node> = Vec::new();
    match &product.state {
        ProductState::Downloading { .. } => {
            controls.push(button("Cancel", ask(HubAsk::Cancel(id()))).into());
            return controls;
        }
        ProductState::Installing | ProductState::Removing => return controls,
        ProductState::Failed { .. } => {
            controls.push(button("Try again", ask(HubAsk::Install(id()))).into());
        }
        ProductState::Idle => {}
    }
    let failed = matches!(product.state, ProductState::Failed { .. });
    match &product.installed {
        Some(_) => {
            if product.has_update() && !failed {
                controls.push(accent_button("Update", ask(HubAsk::Install(id()))).into());
            }
            // A program of the family lives in the tray; opening it shows its
            // Settings, so that is what the button is called.
            controls.push(button("Settings", ask(HubAsk::Open(id()))).into());
            controls
                .push(icon_button(Icon::Delete, "Uninstall", ask(HubAsk::Uninstall(id()))).into());
        }
        None if product.soon => controls.push(badge("Coming soon", Tone::Secondary)),
        None if failed => {}
        None => {
            let install = button("Install", ask(HubAsk::Install(id())))
                .icon(Icon::Download)
                .enabled(product.latest.is_some());
            controls.push(install.into());
        }
    }
    controls
}

/// One program: what it is, where it stands and what can be done with it.
fn product_card(product: &Product) -> Node {
    let (detail, tone) = standing(product);
    let mut line = setting(icon_of(product), &product.name)
        .detail(detail)
        .detail_tone(tone)
        .control(row(controls(product)));
    match product.state {
        ProductState::Downloading { done, total } => {
            line = line.below(Node::Progress(fraction(done, total)));
        }
        ProductState::Installing | ProductState::Removing => {
            line = line.below(Node::Progress(None));
        }
        ProductState::Idle | ProductState::Failed { .. } => {}
    }
    card([line.into()])
}

/// Seconds as hours one reads: `25 hours`, `1 h 30 min`.
fn hours(seconds: u64) -> String {
    match (seconds / 3600, seconds % 3600 / 60) {
        (1, 0) => "1 hour".to_owned(),
        (hours, 0) => format!("{hours} hours"),
        (0, minutes) => format!("{minutes} min"),
        (hours, minutes) => format!("{hours} h {minutes} min"),
    }
}

/// What the tier of a plan is called.
fn tier_name(tier: ProTier) -> &'static str {
    match tier {
        ProTier::Pro => "Pro",
        ProTier::Plus => "Plus",
    }
}

/// The Account page: who is signed in, and what their plan gives them.
#[must_use]
pub fn account(view: &View) -> Content {
    let pro = &view.pro;
    let mut body = Vec::new();
    body.extend(notice_bar(view, Page::Account));
    if let Some(problem) = &pro.problem {
        body.push(info_bar(Severity::Caution, problem));
    }
    match pro.who {
        ProWho::SignedOut => body.push(card([setting(Icon::Person, "Sign in to PUTLAB")
            .detail(
                "One account for every app. You sign in with Google, in your browser, and each \
                 app on this PC is signed in with it.",
            )
            .control(accent_button("Sign in", Action::send(Command::ProSignIn)).into())
            .into()])),
        ProWho::SigningIn => body.extend(signing_in(pro)),
        ProWho::SignedIn => body.extend(signed_in(pro)),
    }
    page("Account", body)
}

/// A sign-in that waits to be finished in the browser.
fn signing_in(pro: &Pro) -> Vec<Node> {
    let mut shown = vec![Node::Busy(if pro.in_browser {
        "Waiting for you to sign in, in your browser".to_owned()
    } else {
        "Starting the sign-in".to_owned()
    })];
    let mut waiting = setting(Icon::Person, "Finish signing in in your browser").detail(format!(
        "The page stays open for {} minutes. This window carries on by itself when you are done.",
        pro.code_minutes
    ));
    if !pro.code.is_empty() {
        waiting = waiting.below(row([note("The page asks for this code:"), mono(&pro.code)]));
    }
    shown.push(card([waiting
        .control(button("Cancel", Action::send(Command::ProCancel)).into())
        .into()]));
    shown
}

/// Someone who is signed in: who, their plan, and what of it is used.
fn signed_in(pro: &Pro) -> Vec<Node> {
    let sign_out = button("Sign out", Action::send(Command::ProSignOut)).kind(ButtonKind::Subtle);
    let who = setting(Icon::Person, &pro.email)
        .detail("Signed in on this PC, for every PUTLAB app")
        .control(sign_out.into());
    let mut shown = vec![card([who.into()])];
    shown.push(header("Plan"));
    if !pro.known {
        let asking = pro.doing == ProDoing::Asking && pro.problem.is_none();
        let line = setting(Icon::Star, "Your plan")
            .detail(if asking {
                "Checking"
            } else {
                "Could not be checked just now"
            })
            .control(button("Check again", Action::send(Command::ProRefresh)).into());
        shown.push(card([line.into()]));
        return shown;
    }
    shown.push(card(plan(pro)));
    if let Some(minutes) = pro.checked_minutes {
        shown.push(note(format!(
            "As of {minutes} minutes ago; the server cannot be reached right now."
        )));
    }
    shown
}

/// The rows of the plan: what it is and until when, and the hours of the
/// month.
fn plan(pro: &Pro) -> Vec<Node> {
    let manage = || button("Manage plan", Action::send(Command::ProManage));
    if !pro.entitled {
        let least = pro.offers.iter().map(|offer| offer.monthly).min();
        let from = least.map_or_else(String::new, |least| {
            format!(" Plans start at {} a month.", price(least, &pro.currency))
        });
        let none = setting(Icon::Star, "No plan")
            .detail(format!(
                "Everything that runs on this PC is free. A plan adds the online \
                 services.{from}"
            ))
            .control(link(
                "See plans",
                Action::send(Command::OpenLink(SITE.to_owned())),
            ));
        return vec![none.into()];
    }
    let mut detail = Vec::new();
    if pro.complimentary {
        detail.push("At no charge".to_owned());
    } else if pro.ends && pro.ends_at > 0 {
        detail.push(format!("Ends on {}", date(pro.ends_at)));
    } else if pro.period_end > 0 {
        detail.push(format!("Renews on {}", date(pro.period_end)));
    }
    let mut what = setting(Icon::Star, format!("{} plan", tier_name(pro.tier)))
        .icon_tone(Tone::Accent)
        .detail(detail.join(DOT));
    if !pro.complimentary {
        what = what.control(manage().into());
    }
    let mut rows = vec![what.into()];
    if pro.cap_seconds > 0 {
        let used = pro.used_seconds.min(pro.cap_seconds);
        let full = pro.used_seconds >= pro.cap_seconds;
        let mut line = format!(
            "{} of {} used this month",
            hours(pro.used_seconds),
            hours(pro.cap_seconds)
        );
        if pro.resets_at > 0 {
            line = format!("{line}{DOT}starts again on {}", day(pro.resets_at));
        }
        rows.push(
            setting(Icon::Microphone, "Dictation in VinPUT")
                .detail(line)
                .below(Node::Gauge {
                    value: fraction(used, pro.cap_seconds).unwrap_or(0.0),
                    tone: if full { Tone::Caution } else { Tone::Accent },
                })
                .into(),
        );
    }
    rows
}

/// The About page of the hub.
#[must_use]
pub fn about(view: &View) -> Content {
    let mut body = vec![identity(
        Icon::Download,
        "PUTLAB hub",
        "Installs, updates and opens the PUTLAB apps, and holds the account they share. It does \
         not run in the background",
        &view.version,
    )];
    body.extend(links(&[(
        Icon::Globe,
        "Website",
        "vinput-dasvcr.web.app",
        SITE,
    )]));
    page("About", body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock;
    use crate::pages::common::probe::Seen;

    fn scene(name: &str) -> View {
        mock::scene(name).unwrap_or_default()
    }

    fn install(id: &str) -> Command {
        Command::Hub(HubAsk::Install(id.to_owned()))
    }

    #[test]
    fn an_installed_app_can_be_opened_and_is_offered_no_update_it_does_not_have() {
        let seen = Seen::of(&products(&scene("hub-apps")).body);
        assert!(seen.says("VinPUT") && seen.says("Voutput"));
        assert!(seen.says("Version 0.4.8"));
        let open = Command::Hub(HubAsk::Open("vinput".to_owned()));
        assert!(seen.commands().contains(&open));
        assert!(!seen.labels().contains(&"Update"));
        assert!(!seen.commands().contains(&install("vinput")));
    }

    #[test]
    fn a_newer_version_is_offered_as_an_update() {
        let seen = Seen::of(&products(&scene("hub-update")).body);
        assert!(seen.labels().contains(&"Update"));
        assert!(seen.commands().contains(&install("vinput")));
        assert!(seen.says("0.4.9 is available"));
    }

    #[test]
    fn an_app_that_is_announced_cannot_be_installed_yet() {
        let seen = Seen::of(&products(&scene("hub-apps")).body);
        assert!(!seen.commands().contains(&install("voutput")));
        assert_eq!(seen.badges, ["Coming soon"]);
    }

    #[test]
    fn an_app_that_is_not_on_this_pc_can_be_installed() {
        let seen = Seen::of(&products(&scene("hub-fresh")).body);
        assert!(seen.commands().contains(&install("vinput")));
        assert!(!seen.labels().contains(&"Settings"));
    }

    #[test]
    fn a_download_shows_how_far_it_is_and_can_be_stopped() {
        let seen = Seen::of(&products(&scene("hub-downloading")).body);
        let cancel = Command::Hub(HubAsk::Cancel("vinput".to_owned()));
        assert!(seen.commands().contains(&cancel));
        assert!(seen.says("Downloading"));
        assert_eq!(seen.progress.len(), 1);
        assert!(!seen.commands().contains(&install("vinput")));
    }

    #[test]
    fn a_failed_install_gives_its_reason_and_can_be_tried_again() {
        let seen = Seen::of(&products(&scene("hub-failed")).body);
        assert!(seen.says("does not match"));
        assert!(seen.labels().contains(&"Try again"));
        assert!(seen.commands().contains(&install("vinput")));
    }

    #[test]
    fn a_list_that_could_not_be_fetched_says_so_and_still_shows_what_is_installed() {
        let seen = Seen::of(&products(&scene("hub-offline")).body);
        assert_eq!(seen.bars.len(), 1);
        assert!(seen.bars[0].1.contains("could not be fetched"));
        assert!(seen.says("Version 0.4.8"));
        assert!(seen.commands().contains(&Command::Hub(HubAsk::Refresh)));
    }

    #[test]
    fn the_account_page_signs_in_and_shows_the_plan() {
        let out = Seen::of(&account(&scene("hub-signed-out")).body);
        assert!(out.commands().contains(&Command::ProSignIn));
        let waiting = Seen::of(&account(&scene("hub-signing-in")).body);
        assert_eq!(waiting.busy.len(), 1);
        assert!(waiting.commands().contains(&Command::ProCancel));
        let seen = Seen::of(&account(&scene("hub-account")).body);
        assert!(seen.says("Pro plan"));
        assert!(seen.says("used this month"));
        assert_eq!(seen.gauges, [Tone::Accent]);
        assert!(seen.commands().contains(&Command::ProManage));
        assert!(seen.commands().contains(&Command::ProSignOut));
    }

    #[test]
    fn someone_without_a_plan_is_told_what_is_free_and_nothing_is_pushed() {
        let seen = Seen::of(&account(&scene("hub-no-plan")).body);
        assert!(seen.says("No plan"));
        assert!(seen.says("free"));
        assert!(!seen.commands().contains(&Command::ProManage));
        assert!(seen.gauges.is_empty());
    }

    #[test]
    fn hours_read_naturally() {
        assert_eq!(hours(25 * 3600), "25 hours");
        assert_eq!(hours(3600), "1 hour");
        assert_eq!(hours(5400), "1 h 30 min");
        assert_eq!(hours(1200), "20 min");
    }
}
