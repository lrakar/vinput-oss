//! The `VinPUT` Pro page: the account, the subscription and what was used.
//!
//! What it shows follows the account: a pitch and a way to sign in for
//! someone who is signed out, the code to finish a sign-in with, the plans
//! for someone who has none, and the plan, its dates and the month's use
//! for a subscriber. Paying happens in the browser; every button that goes
//! there says what it opens, and the page follows what comes of it by
//! itself. Prices and the allowance are the ones in the view, which are the
//! server's once it has said.

use vinput_view::{
    Command, PRO_MODEL, PRO_SOON, Page, Pro, ProDoing, ProPlan, ProTier, ProWho, View,
};

use super::common::{
    DOT, ProTrouble, date, day, fraction, hours_low, least_a_month, least_first, notice_bar, owing,
    price, pro_in_use, pro_trouble,
};
use crate::node::{
    Action, ButtonKind, Content, Icon, Node, Severity, TextStyle, Tone, accent_button, badge,
    bullets, button, card, header, icon_button, link, note, plain_setting, row, setting, styled,
};

/// What both tiers have in common, and what happens when a month's hours
/// are used up: said once, under the tiers.
const SAME: &str = "Both use the same recognition. When the month's hours run out, VinPUT \
                    carries on with a model on this PC.";

/// What a price for the first month means, said once under the tiers that
/// have one: what is paid afterwards, and for how long.
const INTRO: &str = "The first month is at the introductory price. After that the regular price \
                     applies until you cancel.";

/// The way to a model for the time the hours are used up, for someone who
/// has none on this PC.
pub(super) const DOWNLOAD: &str = "Download a model for when the hours run out";

/// Why Pro is worth having, a point each; the allowance follows them.
const PITCH: [&str; 3] = [
    "Text appears while you speak",
    "More than 60 languages, also mixed in one sentence",
    "Nothing to download, no graphics card needed",
];

/// What the page shows for `view`.
#[must_use]
pub fn content(view: &View) -> Content {
    let pro = &view.pro;
    let mut body = Vec::new();
    body.extend(notice_bar(view, Page::Pro));
    body.extend(problem_bar(pro));
    match pro.who {
        ProWho::SignedOut => body.extend(signed_out(pro)),
        ProWho::SigningIn => body.extend(signing_in(pro)),
        ProWho::SignedIn => body.extend(signed_in(pro, pro_in_use(view))),
    }
    Content {
        pinned: Vec::new(),
        title: "VinPUT Pro".to_owned(),
        body,
        footer: Vec::new(),
        back: None,
    }
}

/// Why the server could not be asked, with a way to ask again. That no plan
/// can be subscribed to yet is news, and asking again would not change it.
fn problem_bar(pro: &Pro) -> Option<Node> {
    let problem = pro.problem.as_ref()?;
    let soon = problem == PRO_SOON;
    let retry = pro.who == ProWho::SignedIn && !soon;
    Some(Node::InfoBar {
        severity: if soon {
            Severity::Info
        } else {
            Severity::Critical
        },
        text: problem.clone(),
        action: retry.then(|| ("Try again".to_owned(), Action::send(Command::ProRefresh))),
        dismiss: None,
    })
}

/// Seconds as hours for people: `40 hours`, `1 hour`, `12 h 30 min`.
fn hours(seconds: u64) -> String {
    match (seconds / 3600, seconds % 3600 / 60) {
        (1, 0) => "1 hour".to_owned(),
        (hours, 0) => format!("{hours} hours"),
        (hours, minutes) => format!("{hours} h {minutes} min"),
    }
}

/// Seconds that were used, briefly: `12 h 30 min`, `12 h`, `30 min`.
fn used(seconds: u64) -> String {
    match (seconds / 3600, seconds % 3600 / 60) {
        (0, minutes) => format!("{minutes} min"),
        (hours, 0) => format!("{hours} h"),
        (hours, minutes) => format!("{hours} h {minutes} min"),
    }
}

/// How much of the month's allowance is used: `12 h 30 min of 40 hours`.
pub(super) fn usage(pro: &Pro) -> String {
    format!("{} of {}", used(pro.used_seconds), hours(pro.cap_seconds))
}

/// The hours a month the tiers come with, as one reads them: `8 or 20
/// hours`.
fn hours_on_offer(pro: &Pro) -> String {
    let mut each: Vec<String> = pro
        .offers
        .iter()
        .map(|offer| hours(offer.seconds))
        .collect();
    let last = each.len().saturating_sub(1);
    for hours in &mut each[..last] {
        if let Some(number) = hours.strip_suffix(" hours") {
            *hours = number.to_owned();
        }
    }
    each.join(" or ")
}

/// The pitch, as the row that opens the page for someone who has not
/// bought. It names the least it costs where the tiers under it do not.
fn pitch(pro: &Pro, with_prices: bool, control: Option<Node>) -> Node {
    let mut points: Vec<String> = PITCH.iter().map(|point| (*point).to_owned()).collect();
    points.push(format!("{} of dictation a month", hours_on_offer(pro)));
    let mut about = setting(Icon::Star, "Dictate with VinPUT Pro").below(bullets(points));
    if with_prices {
        let from = least_a_month(pro);
        about = about.detail(match least_first(pro) {
            Some(first) => format!("From {first} for the first month, then from {from}"),
            None => format!("From {from}"),
        });
    }
    if let Some(control) = control {
        about = about.control(control);
    }
    card([about.into()])
}

/// For someone who is signed out: what Pro is, what it costs, and the way in.
fn signed_out(pro: &Pro) -> Vec<Node> {
    let sign_in = accent_button("Sign in with Google", Action::send(Command::ProSignIn))
        .icon(Icon::OpenExternal)
        .enabled(pro.doing == ProDoing::Nothing);
    vec![
        pitch(pro, true, Some(sign_in.into())),
        note(
            "Signing in opens your browser. Then pick a plan here, pay by card, cancel whenever \
             you like.",
        ),
    ]
}

/// A small number in words, as it reads in a sentence.
fn count(number: u32) -> String {
    match number {
        5 => "five".to_owned(),
        10 => "ten".to_owned(),
        15 => "fifteen".to_owned(),
        other => other.to_string(),
    }
}

/// While a sign-in waits to be finished in the browser. Usually there is
/// nothing to do here: the browser brings the sign-in back by itself. A
/// sign-in that has a code shows it large and on a line of its own, since
/// it is then what the page is there for.
fn signing_in(pro: &Pro) -> Vec<Node> {
    let cancel = button("Cancel", Action::send(Command::ProCancel));
    if pro.code.is_empty() && pro.in_browser {
        let waiting = setting(Icon::OpenExternal, "Finish in your browser")
            .detail("Sign in with Google there. VinPUT signs in by itself when you are done.")
            .control(cancel.into())
            .below(Node::Progress(None));
        return vec![card([waiting.into()])];
    }
    if pro.code.is_empty() {
        let opening = setting(Icon::OpenExternal, "Opening your browser")
            .control(cancel.into())
            .below(Node::Progress(None));
        return vec![card([opening.into()])];
    }
    // Set apart letter by letter, to be read off and typed.
    let spaced: Vec<String> = pro.code.chars().map(String::from).collect();
    let copy = icon_button(Icon::Copy, "Copy the code", Action::Copy(pro.code.clone()));
    let waiting = setting(Icon::Key, "Finish in your browser")
        .detail("Sign in with Google, then type this code")
        .control(cancel.into())
        .below(row([
            styled(spaced.join(" "), TextStyle::Title, Tone::Primary),
            copy.into(),
        ]))
        .below(Node::Progress(None));
    let minutes = match pro.code_minutes {
        0 => "a few minutes".to_owned(),
        1 => "a minute".to_owned(),
        minutes => format!("{} minutes", count(minutes)),
    };
    vec![
        card([waiting.into()]),
        note(format!(
            "This code shows it is this PC signing in. It works for {minutes}."
        )),
    ]
}

/// For someone who is signed in: who they are, then what their account
/// calls for.
fn signed_in(pro: &Pro, in_use: bool) -> Vec<Node> {
    let mut body = Vec::new();
    if !pro.known {
        if pro.doing == ProDoing::Asking {
            body.push(card([setting(Icon::Sync, "Looking up your subscription")
                .below(Node::Progress(None))
                .into()]));
        } else if pro.problem.is_some() {
            // Nothing is known from before to show in its place.
            body.push(card([setting(
                Icon::Cloud,
                "Your subscription could not be checked",
            )
            .detail("Dictation with VinPUT Pro needs a connection")
            .into()]));
        }
        body.extend(account(pro));
        return body;
    }
    // A payment on its way says nothing yet about how it will end.
    if pro.doing == ProDoing::AwaitingPayment && !pro.entitled {
        body.extend(plans(pro));
        body.extend(account(pro));
        return body;
    }
    let stands = Standing::of(pro);
    body.extend(standing_bar(stands, pro));
    match stands {
        Standing::Active | Standing::Behind | Standing::UsedUp => {
            body.extend(subscribed(pro, stands, in_use));
        }
        Standing::Unpaid => body.extend(unpaid(pro)),
        Standing::Never | Standing::Ended => body.extend(plans(pro)),
    }
    body.extend(account(pro));
    body
}

/// Where an account that is signed in stands, which decides what the page
/// offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Standing {
    /// Never subscribed.
    Never,
    /// Subscribed once; it is over.
    Ended,
    /// A subscription whose payment is open or has failed, and that cannot
    /// be used until it is settled.
    Unpaid,
    /// Paid for and usable.
    Active,
    /// Usable still, but the last payment failed.
    Behind,
    /// Paid for, with the month's allowance used up.
    UsedUp,
}

impl Standing {
    fn of(pro: &Pro) -> Self {
        // Nothing is owed for what is given, whatever an old subscription
        // was left as.
        let owing = owing(&pro.status) && !pro.complimentary;
        match (pro.entitled, pro.status.is_empty()) {
            (true, _) if owing => Self::Behind,
            (true, _) if used_up(pro) => Self::UsedUp,
            (true, _) => Self::Active,
            (false, true) => Self::Never,
            (false, false) if owing => Self::Unpaid,
            (false, false) => Self::Ended,
        }
    }
}

fn used_up(pro: &Pro) -> bool {
    pro_trouble(pro) == Some(ProTrouble::UsedUp)
}

/// The day Pro works again after the month's allowance was used up, behind
/// `word`: `on 1 November`. If the server did not say when, `unknown`
/// stands for it.
pub(super) fn works_again(pro: &Pro, word: &str, unknown: &str) -> String {
    match pro.resets_at {
        0 => unknown.to_owned(),
        when => format!("{word} {}", day(when)),
    }
}

/// The bar that says what is wrong with the account, if anything is. What
/// to do about it is the accent button of the card below, said once.
fn standing_bar(stands: Standing, pro: &Pro) -> Option<Node> {
    let mut action = None;
    let (severity, text) = match stands {
        Standing::Never | Standing::Active => return None,
        Standing::Ended => {
            // When it ended, if the server knows; when it was to end else.
            let ended = [pro.ended_at, pro.ends_at, pro.period_end];
            let when = match ended.into_iter().find(|when| *when > 0) {
                Some(when) => format!(" on {}", date(when)),
                None => String::new(),
            };
            // Short enough for one line, so that the plans and the account
            // under it fit the window as it opens.
            let text = format!("Your subscription ended{when}. Subscribe again to use it.");
            (Severity::Info, text)
        }
        Standing::Unpaid => (
            Severity::Critical,
            "The payment did not go through, so VinPUT Pro is not active.".to_owned(),
        ),
        Standing::Behind => {
            let update = Action::send(Command::ProManage);
            action = Some(("Update payment method".to_owned(), update));
            let text = match pro.grace_until {
                0 => "The last payment did not go through. Update your payment method to keep \
                      VinPUT Pro."
                    .to_owned(),
                until => format!(
                    "The last payment did not go through. VinPUT Pro keeps working until {}. \
                     Update your payment method to keep it.",
                    date(until)
                ),
            };
            (Severity::Caution, text)
        }
        // Typing goes on: this is news, not a warning.
        Standing::UsedUp if pro.spare.is_some() => (
            Severity::Info,
            format!(
                "This month's {} are used up. Dictation carries on with {}.",
                hours(pro.cap_seconds),
                pro.spare.as_deref().unwrap_or_default()
            ),
        ),
        Standing::UsedUp => (
            Severity::Caution,
            format!(
                "This month's {} are used up. VinPUT Pro works again {}. Until then, choose \
                 another model.",
                hours(pro.cap_seconds),
                works_again(pro, "on", "next month")
            ),
        ),
    };
    Some(Node::InfoBar {
        severity,
        text,
        action,
        dismiss: None,
    })
}

/// What a tier is called beside the other.
fn tier_name(tier: ProTier) -> &'static str {
    match tier {
        ProTier::Pro => "Pro",
        ProTier::Plus => "Plus",
    }
}

/// The tiers, each a row of the same weight with its hours and its two
/// prices: the price of a month on the button that opens its checkout, the
/// price of a year as a quiet second way beside it. Or the wait for a
/// payment that is being made in the browser.
fn plans(pro: &Pro) -> Vec<Node> {
    if pro.doing == ProDoing::AwaitingPayment {
        let waiting = setting(Icon::OpenExternal, "Finish paying in your browser")
            .detail(
                "This page updates by itself when the payment arrives. Nothing is charged if \
                 you close the checkout.",
            )
            .control(button("Cancel", Action::send(Command::ProCancel)).into())
            .below(Node::Progress(None));
        return vec![header("Subscribe"), card([waiting.into()])];
    }
    let free = pro.doing == ProDoing::Nothing;
    // No icon of their own: the star of the pitch above stands for all.
    let mut rows = Vec::new();
    // Said only while it is true of every price the server gave.
    let mut two_free = !pro.offers.is_empty();
    for offer in &pro.offers {
        let subscribe = |plan| Action::send(Command::ProSubscribe(offer.tier, plan));
        let a_month = format!("{} a month", price(offer.monthly, &pro.currency));
        let a_year = format!("or {} a year", price(offer.yearly, &pro.currency));
        two_free &= u64::from(offer.yearly) <= 10 * u64::from(offer.monthly);
        let mut comes_with = format!("{} of dictation a month", hours(offer.seconds));
        let prices = if pro.open {
            row([
                accent_button(a_month, subscribe(ProPlan::Monthly))
                    .enabled(free)
                    .into(),
                button(a_year, subscribe(ProPlan::Yearly))
                    .kind(ButtonKind::Subtle)
                    .enabled(free)
                    .into(),
            ])
        } else {
            // Nothing to press yet: the prices move beside the hours.
            comes_with = format!("{comes_with}{DOT}{a_month} {a_year}");
            styled("Opening soon", TextStyle::Body, Tone::Disabled)
        };
        // The button stays what a month costs for as long as one pays; what
        // the first one costs someone new is said beside the hours.
        if offer.first > 0 {
            let first = price(offer.first, &pro.currency);
            comes_with = format!("{comes_with}{DOT}first month {first}");
        }
        rows.push(
            plain_setting(tier_name(offer.tier))
                .indented()
                .detail(comes_with)
                .control(prices)
                .into(),
        );
    }
    if pro.doing == ProDoing::Opening {
        rows.push(
            setting(Icon::OpenExternal, "Opening the checkout in your browser")
                .below(Node::Progress(None))
                .into(),
        );
    }
    let mut paying = "You pay by card on a page of our payment provider, which opens in your \
                      browser. Cancel at any time."
        .to_owned();
    if !pro.open {
        "Subscribing opens shortly. Until then, the models under Models are free to use."
            .clone_into(&mut paying);
    } else if two_free {
        paying.insert_str(0, "Paid by the year, two months are free. ");
    }
    let mut page = vec![pitch(pro, false, None), header("Choose a plan"), card(rows)];
    if least_first(pro).is_some() {
        page.push(note(INTRO));
    }
    if pro.offers.len() > 1 {
        page.push(note(SAME));
    }
    page.push(note(paying));
    page
}

/// A subscription that cannot be used until its payment is settled.
fn unpaid(pro: &Pro) -> Vec<Node> {
    let manage = accent_button("Update payment method", Action::send(Command::ProManage))
        .icon(Icon::OpenExternal)
        .enabled(pro.doing == ProDoing::Nothing);
    vec![
        header("Subscription"),
        card([setting(Icon::Warning, plan_name(pro))
            .detail("Update your card, or try the same one again")
            .icon_tone(Tone::Critical)
            .control(manage.into())
            .into()]),
    ]
}

/// How long ago something was, from its minutes: `just now`, `7 minutes
/// ago`, `2 hours ago`.
fn ago(minutes: u32) -> String {
    match (minutes / 60, minutes) {
        (0, 0) => "just now".to_owned(),
        (0, 1) => "1 minute ago".to_owned(),
        (0, minutes) => format!("{minutes} minutes ago"),
        (1, _) => "1 hour ago".to_owned(),
        (hours, _) => format!("{hours} hours ago"),
    }
}

/// What a subscriber sees: the plan and its dates, the month's use, and
/// whether dictation uses Pro. While the server cannot be asked, it is
/// what was known last, drawn quietly and dated. An account that has Pro
/// at no charge has nothing to manage, so it is offered nothing of the kind.
fn subscribed(pro: &Pro, stands: Standing, in_use: bool) -> Vec<Node> {
    let current = pro.checked_minutes.is_none();
    let caption = if pro.ends {
        "Keep subscription"
    } else {
        "Manage subscription"
    };
    let manage = button(caption, Action::send(Command::ProManage))
        .icon(Icon::OpenExternal)
        .hint("Change the card or the plan, cancel, or get invoices")
        .enabled(current);
    let mut plan = setting(Icon::Star, plan_name(pro)).enabled(current);
    if !pro.complimentary {
        plan = plan.control(manage.into());
    }
    let ends_at = if pro.ends_at > 0 {
        pro.ends_at
    } else {
        pro.period_end
    };
    plan = match (pro.checked_minutes, pro.ends) {
        (Some(minutes), _) => plan.detail(format!("Last checked {}", ago(minutes))),
        (None, _) if pro.complimentary => {
            plan.detail("VinPUT Pro is on for this account at no charge")
        }
        (None, true) if ends_at > 0 => plan
            .detail(format!("Ends on {} and is not renewed", date(ends_at)))
            .detail_tone(Tone::Caution),
        (None, false) if pro.period_end > 0 => plan.detail(format!(
            "Renews on {}{DOT}change card, plan or cancel",
            date(pro.period_end)
        )),
        (None, _) => plan,
    };
    let full = stands == Standing::UsedUp;
    let (text_tone, bar_tone) = match (current, full) {
        (false, _) => (Tone::Secondary, Tone::Disabled),
        // A warning only where typing stops.
        (true, true) if pro.spare.is_none() => (Tone::Caution, Tone::Caution),
        (true, _) => (Tone::Secondary, Tone::Accent),
    };
    let in_use_tone = if current {
        Tone::Accent
    } else {
        Tone::Secondary
    };
    let usage = setting(Icon::Microphone, "This month")
        .detail(usage(pro))
        .detail_tone(text_tone)
        .enabled(current)
        .below(Node::Gauge {
            value: fraction(pro.used_seconds, pro.cap_seconds).unwrap_or(0.0),
            tone: bar_tone,
        });
    let model = setting(Icon::Model, "Speech model").enabled(current);
    let model = if let (true, Some(spare)) = (full, &pro.spare) {
        // The choice stays VinPUT Pro; what types meanwhile is said.
        model
            .detail(format!(
                "Hours used up{DOT}using {spare} {}",
                works_again(pro, "until", "until next month")
            ))
            .control(button("Choose a model", Action::Go(Page::Models)).into())
    } else if full {
        // Whatever the settings say, nothing is typed with Pro for now.
        model
            .detail(format!(
                "VinPUT Pro is paused {}",
                works_again(pro, "until", "until next month")
            ))
            .control(button("Choose a model", Action::Go(Page::Models)).into())
    } else if in_use {
        model
            .detail("Dictation uses VinPUT Pro")
            .control(badge("In use", in_use_tone))
    } else {
        let choose = Action::send(Command::UseModel(PRO_MODEL.to_owned()));
        model
            .detail("Dictation uses another model")
            .control(button("Use VinPUT Pro", choose).enabled(current).into())
    };
    let mut page = vec![
        header("Subscription"),
        card([plan.into(), usage.into(), model.into()]),
    ];
    // Said only when it matters, and to someone it helps: the hours are
    // nearly gone or gone, and no model is on this PC to carry on with.
    if current && pro.spare.is_none() && hours_low(pro) {
        page.push(link(DOWNLOAD, Action::Go(Page::Models)));
    }
    if current && pro.tier == ProTier::Plus && !pro.complimentary {
        page.push(note(
            "Need more hours? Switch to Pro under Manage subscription.",
        ));
    }
    page
}

/// Who is signed in, and the way out.
fn account(pro: &Pro) -> [Node; 2] {
    let name = if pro.email.is_empty() {
        "Signed in"
    } else {
        &pro.email
    };
    [
        header("Account"),
        card([setting(Icon::Person, name)
            .detail("Signed in with Google")
            .control(button("Sign out", Action::send(Command::ProSignOut)).into())
            .into()]),
    ]
}

/// The tier and how it is paid, `VinPUT Plus · Monthly`, or `Complimentary`
/// for an account that has Pro at no charge.
fn plan_name(pro: &Pro) -> String {
    let tier = tier_name(pro.tier);
    match pro.plan {
        _ if pro.complimentary => "Complimentary".to_owned(),
        Some(ProPlan::Monthly) => format!("VinPUT {tier}{DOT}Monthly"),
        Some(ProPlan::Yearly) => format!("VinPUT {tier}{DOT}Yearly"),
        None => format!("VinPUT {tier}"),
    }
}

#[cfg(test)]
mod tests {
    use super::super::common::probe::{Seen, scene, settings};
    use super::*;
    use crate::node::ButtonKind;

    fn seen(name: &str) -> Seen {
        Seen::of(&content(&scene(name)).body)
    }

    /// The buttons of the accent color among what can be pressed.
    fn accents(seen: &Seen) -> Vec<&str> {
        let accent = seen
            .presses
            .iter()
            .filter(|press| press.kind == ButtonKind::Accent);
        accent.map(|press| press.label.as_str()).collect()
    }

    /// The detail of the row titled `title`.
    fn detail_of(view: &View, title: &str) -> Option<(String, Tone)> {
        let content = content(view);
        let rows = settings(&content.body);
        let row = rows.iter().find(|row| row.title == title)?;
        Some((row.detail.clone(), row.detail_tone))
    }

    #[test]
    fn someone_signed_out_is_told_what_pro_is_and_how_to_get_in() {
        let seen = seen("pro-signed-out");
        assert!(
            seen.texts
                .iter()
                .any(|text| text == "From \u{20ac}2.50 a month")
        );
        for point in [
            "Text appears while you speak",
            "More than 60 languages, also mixed in one sentence",
            "Nothing to download, no graphics card needed",
            "8 or 25 hours of dictation a month",
        ] {
            assert!(seen.texts.iter().any(|text| text == point), "{point}");
        }
        assert!(seen.says("Then pick a plan here, pay by card, cancel whenever you like."));
        assert_eq!(seen.labels(), ["Sign in with Google"]);
        assert_eq!(seen.commands(), [Command::ProSignIn]);
        assert_eq!(accents(&seen), ["Sign in with Google"]);
    }

    #[test]
    fn a_sign_in_shows_its_code_large_and_can_be_copied_and_called_off() {
        let content = content(&scene("pro-signing-in"));
        let seen = Seen::of(&content.body);
        assert!(seen.says("Finish in your browser"));
        assert!(seen.says("Sign in with Google, then type this code"));
        // The code, letter by letter, in the largest type of the page.
        let nodes = super::super::common::probe::all(&content.body);
        let code = nodes.iter().find_map(|node| match node {
            Node::Text { text, style, .. } if text == "K 7 M 2 Q X" => Some(*style),
            _ => None,
        });
        assert_eq!(code, Some(TextStyle::Title));
        // What is copied is the code itself.
        let copy = seen
            .presses
            .iter()
            .find(|press| press.label == "Copy the code");
        assert!(matches!(
            copy.map(|press| &press.action),
            Some(Action::Copy(code)) if code == "K7M2QX"
        ));
        assert_eq!(seen.commands(), [Command::ProCancel]);
        assert_eq!(seen.progress, [None]);
        assert!(seen.says("This code shows it is this PC signing in. It works for ten minutes."));

        // How long is the server's to say.
        let mut view = scene("pro-signing-in");
        view.pro.code_minutes = 3;
        assert!(Seen::of(&super::content(&view).body).says("It works for 3 minutes."));

        view.pro.code = String::new();
        view.pro.in_browser = false;
        let seen = Seen::of(&super::content(&view).body);
        assert!(seen.says("Opening your browser"));
        assert_eq!(seen.commands(), [Command::ProCancel]);
    }

    #[test]
    fn a_sign_in_the_browser_brings_back_asks_for_nothing_and_can_be_called_off() {
        let mut view = scene("pro-signing-in");
        view.pro.code = String::new();
        view.pro.in_browser = true;
        let seen = Seen::of(&content(&view).body);
        assert!(seen.says("Finish in your browser"));
        assert!(
            seen.says("Sign in with Google there. VinPUT signs in by itself when you are done.")
        );
        // No code, nothing to copy, nothing said about one.
        assert!(!seen.texts.iter().any(|text| text.contains("code")));
        assert_eq!(seen.labels(), ["Cancel"]);
        assert_eq!(seen.commands(), [Command::ProCancel]);
        assert_eq!(seen.progress, [None]);
    }

    #[test]
    fn someone_without_a_subscription_is_one_press_from_the_checkout_of_either_tier() {
        let view = scene("pro-not-subscribed");
        let seen = Seen::of(&content(&view).body);
        // The price of a month is the button, the price of a year the quiet
        // way beside it; the smaller tier first, neither picked for anyone.
        assert_eq!(
            seen.labels(),
            [
                "\u{20ac}2.50 a month",
                "or \u{20ac}24 a year",
                "\u{20ac}5 a month",
                "or \u{20ac}49 a year",
                "Sign out"
            ]
        );
        assert_eq!(
            seen.commands(),
            [
                Command::ProSubscribe(ProTier::Plus, ProPlan::Monthly),
                Command::ProSubscribe(ProTier::Plus, ProPlan::Yearly),
                Command::ProSubscribe(ProTier::Pro, ProPlan::Monthly),
                Command::ProSubscribe(ProTier::Pro, ProPlan::Yearly),
                Command::ProSignOut,
            ]
        );
        // Both tiers weigh the same.
        assert_eq!(
            accents(&seen),
            ["\u{20ac}2.50 a month", "\u{20ac}5 a month"]
        );
        let kinds: Vec<_> = seen.presses.iter().map(|press| press.kind).collect();
        assert_eq!(kinds[..2], kinds[2..4]);
        assert!(seen.says("ana@example.com"));
        assert!(seen.says("Text appears while you speak"));
        assert!(seen.bars.is_empty() && seen.badges.is_empty());
        for (tier, comes_with) in [
            ("Plus", "8 hours of dictation a month"),
            ("Pro", "25 hours of dictation a month"),
        ] {
            let detail = detail_of(&view, tier).map(|(detail, _)| detail);
            assert_eq!(detail.as_deref(), Some(comes_with));
        }
        // What they share and what happens when the hours run out, once.
        assert_eq!(seen.texts.iter().filter(|text| *text == SAME).count(), 1);
        assert!(seen.says("Paid by the year, two months are free. You pay by card"));
        assert!(seen.says("Cancel at any time."));
        // The star is the pitch's alone; the tiers line up under it.
        let page = content(&view);
        let rows = settings(&page.body);
        let starred = rows.iter().filter(|row| row.icon == Some(Icon::Star));
        assert_eq!(starred.count(), 1);
        for tier in ["Plus", "Pro"] {
            let row = rows.iter().find(|row| row.title == tier);
            assert_eq!(row.map(|row| (row.icon, row.number)), Some((None, Some(0))));
        }
    }

    #[test]
    fn nothing_on_the_page_pushes_one_tier_over_the_other() {
        for name in crate::mock::SCENES {
            for text in seen(name).texts {
                let lower = text.to_lowercase();
                for word in [
                    "popular",
                    "best value",
                    "recommended",
                    "only ",
                    "hurry",
                    "save ",
                ] {
                    assert!(!lower.contains(word), "{name}: {text}");
                }
            }
        }
    }

    #[test]
    fn the_prices_and_the_allowance_are_the_ones_the_server_gave() {
        let mut view = scene("pro-not-subscribed");
        view.pro.offers[1].monthly = 750;
        view.pro.offers[1].yearly = 8000;
        view.pro.offers[1].seconds = 30 * 3600;
        let seen = Seen::of(&content(&view).body);
        assert!(seen.labels().contains(&"\u{20ac}7.50 a month"));
        assert!(seen.labels().contains(&"or \u{20ac}80 a year"));
        assert!(seen.says("8 or 30 hours of dictation a month"));
        assert!(seen.says("30 hours of dictation a month"));
        // Two months free is said only while a year of every tier costs
        // ten months of it at most.
        assert!(!seen.says("two months are free"));
        assert!(seen.says("You pay by card"));
        view.pro.offers[1].yearly = 7500;
        assert!(Seen::of(&content(&view).body).says("two months are free"));

        // A server from before there were tiers offers the one it has, at
        // the hours it gives, and nothing is said about "both".
        let seen = self::seen("pro-one-tier");
        assert_eq!(
            seen.labels(),
            ["\u{20ac}5 a month", "or \u{20ac}49 a year", "Sign out"]
        );
        assert!(seen.says("40 hours of dictation a month"));
        assert!(!seen.says("Both use"));

        let mut view = scene("pro-signed-out");
        view.pro.offers[0].monthly = 250;
        view.pro.currency = "usd".to_owned();
        assert!(Seen::of(&content(&view).body).says("From $2.50 a month"));
    }

    #[test]
    fn a_first_month_at_less_is_said_beside_the_regular_price_and_never_in_its_place() {
        let plain = seen("pro-not-subscribed");
        let view = scene("pro-intro");
        let seen = Seen::of(&content(&view).body);
        // The buttons say what a month and a year cost for as long as one
        // pays, and lead where they led.
        assert_eq!(seen.labels(), plain.labels());
        assert_eq!(seen.commands(), plain.commands());
        assert_eq!(
            accents(&seen),
            ["\u{20ac}2.50 a month", "\u{20ac}5 a month"]
        );
        // What the first month costs is beside the hours of its tier.
        for (tier, comes_with) in [
            (
                "Plus",
                "8 hours of dictation a month \u{b7} first month \u{20ac}1.49",
            ),
            (
                "Pro",
                "25 hours of dictation a month \u{b7} first month \u{20ac}3.49",
            ),
        ] {
            let detail = detail_of(&view, tier).map(|(detail, _)| detail);
            assert_eq!(detail.as_deref(), Some(comes_with));
        }
        // And what follows it is said once, under the tiers.
        let said = |seen: &Seen| seen.texts.iter().filter(|text| *text == INTRO).count();
        assert_eq!(said(&seen), 1);
        assert_eq!(
            INTRO,
            "The first month is at the introductory price. After that the regular price applies \
             until you cancel."
        );

        // An account the server does not call new is shown none of it.
        assert!(!plain.says("first month") && !plain.says("introductory"));
        // A tier without such a price says nothing of one beside a tier
        // that has it.
        let mut view = view;
        view.pro.offers[0].first = 0;
        let seen = Seen::of(&content(&view).body);
        let plus = detail_of(&view, "Plus").map(|(detail, _)| detail);
        assert_eq!(plus.as_deref(), Some("8 hours of dictation a month"));
        assert!(seen.says("first month \u{20ac}3.49") && said(&seen) == 1);
        view.pro.offers[1].first = 0;
        assert_eq!(Seen::of(&content(&view).body).texts, plain.texts);

        // Someone signed out, who may be new, reads both prices in the
        // pitch; without such a price it is the one it was.
        let mut view = scene("pro-intro-signed-out");
        let seen = Seen::of(&content(&view).body);
        let pitch = "From \u{20ac}1.49 for the first month, then from \u{20ac}2.50 a month";
        assert!(seen.texts.iter().any(|text| text == pitch));
        assert_eq!(seen.labels(), ["Sign in with Google"]);
        view.pro.currency = "usd".to_owned();
        assert!(Seen::of(&content(&view).body).says("From $1.49 for the first month"));
        assert!(!self::seen("pro-signed-out").says("first month"));

        // Said calmly wherever it is said.
        for name in crate::mock::SCENES {
            for text in self::seen(name).texts {
                let lower = text.to_lowercase();
                for word in ["limited", "hurry", "today", "offer", "discount", "%"] {
                    assert!(!lower.contains(word), "{name}: {text}");
                }
            }
        }
    }

    #[test]
    fn while_no_plan_can_be_subscribed_to_the_tiers_are_shown_with_nothing_to_press() {
        let mut view = scene("pro-opening-soon");
        let seen = Seen::of(&content(&view).body);
        // Both tiers with their hours and prices, and no way to a checkout.
        assert_eq!(seen.labels(), ["Sign out"]);
        assert!(accents(&seen).is_empty() && seen.bars.is_empty());
        for (tier, comes_with) in [
            (
                "Plus",
                "8 hours of dictation a month \u{b7} \u{20ac}2.50 a month or \u{20ac}24 a year",
            ),
            (
                "Pro",
                "25 hours of dictation a month \u{b7} \u{20ac}5 a month or \u{20ac}49 a year",
            ),
        ] {
            let detail = detail_of(&view, tier).map(|(detail, _)| detail);
            assert_eq!(detail.as_deref(), Some(comes_with));
        }
        let soon = seen.texts.iter().filter(|text| *text == "Opening soon");
        assert_eq!(soon.count(), 2);
        assert!(seen.says(
            "Subscribing opens shortly. Until then, the models under Models are free to use."
        ));
        // Nothing is said of paying by card, or by the year.
        assert!(!seen.says("You pay by card") && !seen.says("two months"));
        // The price of a first month keeps its place, after the others.
        view.pro.offers[0].first = 149;
        let plus = detail_of(&view, "Plus").map(|(detail, _)| detail);
        assert!(plus.is_some_and(|plus| plus.ends_with("a year \u{b7} first month \u{20ac}1.49")));
        // The day the server says it can be, the buttons are back.
        view.pro.open = true;
        assert_eq!(Seen::of(&content(&view).body).labels().len(), 5);
        // The step of the welcome is this page.
        let mut first = scene("first-run");
        first.pro = view.pro;
        first.pro.open = false;
        let welcome = super::super::welcome::content(Page::WelcomePro, &first);
        assert!(Seen::of(&welcome.body).says("Opening soon"));
    }

    #[test]
    fn a_press_the_server_cannot_take_yet_is_answered_calmly_and_the_plans_stay() {
        let seen = seen("pro-not-yet");
        assert_eq!(
            seen.bars,
            [(
                Severity::Info,
                "Subscribing opens shortly. Nothing was charged. Your free options under Models \
                 keep working."
                    .to_owned()
            )]
        );
        // Nothing to try again, no wait, and the plans as they were.
        assert_eq!(seen.labels(), self::seen("pro-not-subscribed").labels());
        assert!(seen.progress.is_empty());
        assert!(!seen.says("could not do that"));
    }

    #[test]
    fn a_checkout_under_way_is_waited_for_and_can_be_left() {
        let seen = seen("pro-awaiting-payment");
        assert!(seen.says("Finish paying in your browser"));
        assert!(seen.says(
            "This page updates by itself when the payment arrives. Nothing is charged if you \
             close the checkout."
        ));
        assert_eq!(seen.labels(), ["Cancel", "Sign out"]);
        assert_eq!(seen.commands()[0], Command::ProCancel);
        assert_eq!(seen.progress, [None]);

        let mut view = scene("pro-not-subscribed");
        view.pro.doing = ProDoing::Opening;
        let seen = Seen::of(&content(&view).body);
        // The same press twice would open two checkouts.
        assert_eq!(seen.labels(), ["Sign out"]);
        assert!(seen.says("Opening the checkout"));
    }

    #[test]
    fn a_subscriber_sees_the_plan_its_date_and_the_use_of_the_month() {
        let view = scene("pro-subscribed");
        let seen = Seen::of(&content(&view).body);
        assert!(seen.bars.is_empty());
        // Where to cancel is said in sight, not in a tooltip.
        assert_eq!(
            detail_of(&view, "VinPUT Pro \u{b7} Monthly"),
            Some((
                "Renews on 5 November 2026 \u{b7} change card, plan or cancel".to_owned(),
                Tone::Secondary
            ))
        );
        assert_eq!(
            detail_of(&view, "This month"),
            Some(("12 h 30 min of 25 hours".to_owned(), Tone::Secondary))
        );
        // Nobody on the larger tier is told about a larger one.
        assert!(!seen.says("Need more hours"));
        assert!(matches!(seen.progress.as_slice(), [Some(part)] if (0.49..0.51).contains(part)));
        assert_eq!(seen.gauges, [Tone::Accent]);
        assert_eq!(seen.labels(), ["Manage subscription", "Sign out"]);
        assert_eq!(seen.commands(), [Command::ProManage, Command::ProSignOut]);
        assert_eq!(seen.badges, ["In use"]);
        assert!(accents(&seen).is_empty());
    }

    #[test]
    fn a_subscriber_to_plus_sees_that_tier_its_hours_and_the_way_to_more() {
        let view = scene("pro-plus");
        let seen = Seen::of(&content(&view).body);
        assert!(detail_of(&view, "VinPUT Plus \u{b7} Monthly").is_some());
        assert_eq!(
            detail_of(&view, "This month"),
            Some(("3 h 10 min of 8 hours".to_owned(), Tone::Secondary))
        );
        assert!(seen.says("Need more hours? Switch to Pro under Manage subscription."));
        assert_eq!(seen.labels(), ["Manage subscription", "Sign out"]);
        assert_eq!(seen.badges, ["In use"]);
        // The title of the page stays what the service is called.
        assert_eq!(content(&view).title, "VinPUT Pro");
        let mut view = view;
        view.pro.plan = Some(ProPlan::Yearly);
        assert!(detail_of(&view, "VinPUT Plus \u{b7} Yearly").is_some());
    }

    #[test]
    fn an_account_given_pro_sees_that_and_its_use_and_nothing_to_pay_or_manage() {
        let mut view = scene("pro-complimentary");
        let seen = Seen::of(&content(&view).body);
        assert!(seen.bars.is_empty());
        assert_eq!(
            detail_of(&view, "Complimentary"),
            Some((
                "VinPUT Pro is on for this account at no charge".to_owned(),
                Tone::Secondary
            ))
        );
        assert_eq!(
            detail_of(&view, "This month"),
            Some(("12 h 30 min of 25 hours".to_owned(), Tone::Secondary))
        );
        assert_eq!(seen.gauges, [Tone::Accent]);
        assert_eq!(seen.badges, ["In use"]);
        // Nothing leads to the payment service.
        assert_eq!(seen.labels(), ["Sign out"]);
        assert_eq!(seen.commands(), [Command::ProSignOut]);
        assert!(!seen.says("Renews") && !seen.says("Subscribe"));

        // Whatever became of a subscription the account once had, nothing
        // is owed and nothing has ended.
        for status in ["canceled", "past_due", "incomplete"] {
            view.pro.status = status.to_owned();
            view.pro.plan = Some(ProPlan::Monthly);
            view.pro.period_end = 1_793_837_814;
            let seen = Seen::of(&content(&view).body);
            assert!(seen.bars.is_empty(), "{status}");
            assert!(seen.says("Complimentary") && !seen.says("Monthly"));
            assert_eq!(seen.labels(), ["Sign out"], "{status}");
        }

        // Dictating with another model, it is one press away as for anyone.
        let mut view = scene("pro-complimentary");
        for model in &mut view.models.list {
            model.in_use = model.id == "ggml-base";
        }
        let seen = Seen::of(&content(&view).body);
        assert_eq!(seen.labels(), ["Use VinPUT Pro", "Sign out"]);

        // The allowance is the same as a subscriber's, and runs out alike.
        view.pro.used_seconds = view.pro.cap_seconds;
        let seen = Seen::of(&content(&view).body);
        assert!(matches!(seen.bars.as_slice(), [(Severity::Info, _)]));
        assert_eq!(seen.labels(), ["Choose a model", "Sign out"]);
        // And nobody who is given Pro is pointed at a tier to pay for.
        view.pro.tier = ProTier::Plus;
        assert!(!Seen::of(&content(&view).body).says("Need more hours"));

        // Out of reach, it is dated like any account.
        let mut view = scene("pro-complimentary");
        view.pro.checked_minutes = Some(7);
        let dated = detail_of(&view, "Complimentary").map(|(detail, _)| detail);
        assert_eq!(dated.as_deref(), Some("Last checked 7 minutes ago"));
    }

    #[test]
    fn a_subscriber_who_dictates_with_another_model_can_switch_to_pro() {
        let mut view = scene("pro-subscribed");
        for model in &mut view.models.list {
            model.in_use = model.id == "ggml-base";
        }
        let seen = Seen::of(&content(&view).body);
        assert!(seen.badges.is_empty());
        assert!(
            seen.commands()
                .contains(&Command::UseModel(PRO_MODEL.to_owned()))
        );
    }

    #[test]
    fn a_subscription_that_ends_says_when_and_offers_the_way_back() {
        let mut view = scene("pro-ending");
        assert_eq!(
            detail_of(&view, "VinPUT Pro \u{b7} Yearly"),
            Some((
                "Ends on 5 November 2026 and is not renewed".to_owned(),
                Tone::Caution
            ))
        );
        let seen = Seen::of(&content(&view).body);
        assert_eq!(seen.labels(), ["Keep subscription", "Sign out"]);
        assert_eq!(seen.commands()[0], Command::ProManage);
        // The day it was set to end on, where that is not the end of the
        // period.
        view.pro.ends_at = 1_794_009_600;
        assert!(Seen::of(&content(&view).body).says("Ends on 7 November 2026"));
    }

    #[test]
    fn used_up_hours_say_what_dictation_carries_on_with_and_until_when() {
        for (name, plan, hours, used) in [
            (
                "pro-over-allowance",
                "VinPUT Pro \u{b7} Monthly",
                "25 hours",
                "25 h of 25 hours",
            ),
            (
                "pro-plus-used-up",
                "VinPUT Plus \u{b7} Monthly",
                "8 hours",
                "8 h of 8 hours",
            ),
        ] {
            let view = scene(name);
            let seen = Seen::of(&content(&view).body);
            // News, not a warning: typing goes on.
            assert_eq!(
                seen.bars,
                [(
                    Severity::Info,
                    format!(
                        "This month's {hours} are used up. Dictation carries on with Whisper \
                         base."
                    )
                )]
            );
            assert!(seen.bars[0].1.chars().count() <= 76);
            assert!(detail_of(&view, plan).is_some(), "{name}");
            assert_eq!(
                detail_of(&view, "This month"),
                Some((used.to_owned(), Tone::Secondary))
            );
            assert_eq!(seen.gauges, [Tone::Accent]);
            assert_eq!(
                detail_of(&view, "Speech model")
                    .map(|(detail, _)| detail)
                    .as_deref(),
                Some("Hours used up \u{b7} using Whisper base until 1 November")
            );
            // Pro types nothing, so it wears no badge; and nobody who has a
            // model to carry on with is told to download one.
            assert!(seen.badges.is_empty());
            assert!(!seen.says(DOWNLOAD));
            assert_eq!(
                seen.labels(),
                ["Manage subscription", "Choose a model", "Sign out"]
            );
        }
        let mut view = scene("pro-over-allowance");
        view.pro.resets_at = 0;
        let seen = Seen::of(&content(&view).body);
        assert!(seen.says("using Whisper base until next month"));
    }

    #[test]
    fn a_model_to_carry_on_with_is_offered_only_when_the_hours_run_low_and_there_is_none() {
        let low = scene("pro-low-no-model");
        let seen = Seen::of(&content(&low).body);
        assert_eq!(seen.labels(), ["Manage subscription", DOWNLOAD, "Sign out"]);
        let link = seen.presses.iter().find(|press| press.label == DOWNLOAD);
        assert!(matches!(
            link.map(|press| &press.action),
            Some(Action::Go(Page::Models))
        ));
        // Not before nine tenths are used, and never with a model here.
        let mut view = low.clone();
        view.pro.used_seconds = view.pro.cap_seconds * 8 / 10;
        assert!(!Seen::of(&content(&view).body).says(DOWNLOAD));
        let mut view = low;
        view.pro.spare = Some("Whisper base".to_owned());
        assert!(!Seen::of(&content(&view).body).says(DOWNLOAD));
        for name in [
            "pro-subscribed",
            "pro-plus",
            "pro-not-subscribed",
            "pro-signed-out",
        ] {
            assert!(!self::seen(name).says(DOWNLOAD), "{name}");
        }
    }

    #[test]
    fn a_used_up_allowance_with_no_model_says_when_pro_works_again_and_leads_to_the_models() {
        let view = scene("pro-used-up-no-model");
        let seen = Seen::of(&content(&view).body);
        assert_eq!(
            seen.bars,
            [(
                Severity::Caution,
                "This month's 25 hours are used up. VinPUT Pro works again on 1 November. \
                 Until then, choose another model."
                    .to_owned()
            )]
        );
        assert_eq!(
            detail_of(&view, "This month"),
            Some(("25 h of 25 hours".to_owned(), Tone::Caution))
        );
        assert_eq!(seen.progress, [Some(1.0)]);
        assert_eq!(seen.gauges, [Tone::Caution]);
        // The row does not claim Pro is in use while nothing is typed with
        // it, and the one way on is in it, once.
        assert!(seen.badges.is_empty());
        assert!(!seen.says("Dictation uses VinPUT Pro"));
        assert_eq!(
            detail_of(&view, "Speech model")
                .map(|(detail, _)| detail)
                .as_deref(),
            Some("VinPUT Pro is paused until 1 November")
        );
        assert_eq!(
            seen.labels(),
            [
                "Manage subscription",
                "Choose a model",
                DOWNLOAD,
                "Sign out"
            ]
        );

        // A server that does not say when: no date is made up.
        let mut view = view;
        view.pro.resets_at = 0;
        let seen = Seen::of(&content(&view).body);
        assert!(seen.says("VinPUT Pro works again next month. Until then"));
        assert!(seen.says("VinPUT Pro is paused until next month"));
    }

    #[test]
    fn a_failed_renewal_says_until_when_pro_works_and_offers_the_way_to_the_card() {
        let view = scene("pro-payment-problem");
        let seen = Seen::of(&content(&view).body);
        assert_eq!(
            seen.bars,
            [(
                Severity::Caution,
                "The last payment did not go through. VinPUT Pro keeps working until 8 October \
                 2026. Update your payment method to keep it."
                    .to_owned()
            )]
        );
        let labels = seen.labels();
        assert_eq!(labels[0], "Update payment method");
        assert_eq!(seen.commands()[0], Command::ProManage);
        // Still usable: the plan and the use are shown as for any subscriber.
        assert!(seen.says("This month"));
        // Without a date from the server nothing vague is put in its place.
        let mut view = view;
        view.pro.grace_until = 0;
        let seen = Seen::of(&content(&view).body);
        assert!(seen.says("Update your payment method to keep VinPUT Pro."));
        assert!(!seen.says("a few days"));
    }

    #[test]
    fn a_payment_that_never_went_through_can_be_tried_again_from_one_button() {
        let view = scene("pro-payment-failed");
        let seen = Seen::of(&content(&view).body);
        assert_eq!(
            seen.bars,
            [(
                Severity::Critical,
                "The payment did not go through, so VinPUT Pro is not active.".to_owned()
            )]
        );
        // A second subscription is never offered beside one that is open.
        assert!(
            !seen
                .labels()
                .iter()
                .any(|label| label.starts_with("Subscribe"))
        );
        // One way to the card, not two a hand apart.
        assert_eq!(seen.commands(), [Command::ProManage, Command::ProSignOut]);
        assert_eq!(accents(&seen), ["Update payment method"]);
        assert_eq!(
            detail_of(&view, "VinPUT Pro \u{b7} Monthly")
                .map(|(detail, _)| detail)
                .as_deref(),
            Some("Update your card, or try the same one again")
        );
    }

    #[test]
    fn an_ended_subscription_says_when_it_ended_and_offers_the_plans_again() {
        let mut view = scene("pro-expired");
        let seen = Seen::of(&content(&view).body);
        assert!(
            matches!(seen.bars.as_slice(), [(Severity::Info, text)] if text.contains("ended on 5 November 2026"))
        );
        assert!(seen.labels().contains(&"\u{20ac}5 a month"));
        // The day it did end counts before the day it was to.
        view.pro.ended_at = 1_792_000_000;
        assert!(Seen::of(&content(&view).body).says("ended on 14 October 2026"));
        view.pro.ended_at = 0;
        view.pro.ends_at = 0;
        view.pro.period_end = 0;
        assert!(Seen::of(&content(&view).body).says("Your subscription ended. Subscribe again"));
        // Nothing of the page is cut off in the window as it opens: the
        // bar over the plans is one line.
        let bar = &Seen::of(&content(&scene("pro-expired")).body).bars[0].1;
        assert!(bar.chars().count() <= 76, "{bar}");
    }

    #[test]
    fn a_server_that_cannot_be_reached_is_said_with_a_way_to_ask_again() {
        let seen = seen("pro-offline");
        assert!(
            matches!(seen.bars.as_slice(), [(Severity::Critical, text)] if text.contains("could not be reached"))
        );
        assert_eq!(seen.labels(), ["Try again", "Sign out"]);
        assert_eq!(seen.commands()[0], Command::ProRefresh);
        assert!(seen.says("ana@example.com"));
        // Nothing known from before: one line says what that means.
        assert!(seen.says("Your subscription could not be checked"));
        assert!(seen.says("Dictation with VinPUT Pro needs a connection"));

        // Signed out there is nothing to ask again; signing in is the retry.
        let mut view = scene("pro-signed-out");
        view.pro.problem = Some("VinPUT Pro could not be reached.".to_owned());
        let seen = Seen::of(&content(&view).body);
        assert_eq!(seen.labels(), ["Sign in with Google"]);
        assert_eq!(seen.bars.len(), 1);
    }

    #[test]
    fn what_was_known_before_the_server_went_out_of_reach_stays_dated_and_quiet() {
        let view = scene("pro-offline-known");
        let page = content(&view);
        let seen = Seen::of(&page.body);
        assert_eq!(seen.bars.len(), 1);
        assert_eq!(
            detail_of(&view, "VinPUT Pro \u{b7} Monthly")
                .map(|(detail, _)| detail)
                .as_deref(),
            Some("Last checked 7 minutes ago")
        );
        // The card is there, drawn as out of reach; nothing on it leads to
        // the server.
        let rows = settings(&page.body);
        for title in ["VinPUT Pro \u{b7} Monthly", "This month", "Speech model"] {
            let row = rows.iter().find(|row| row.title == title);
            assert_eq!(row.map(|row| row.enabled), Some(false), "{title}");
        }
        assert!(seen.says("12 h 30 min of 25 hours"));
        assert_eq!(seen.gauges, [Tone::Disabled]);
        assert_eq!(seen.labels(), ["Try again", "Sign out"]);
        assert!(!seen.says("could not be checked"));
    }

    #[test]
    fn an_account_not_heard_about_yet_is_looked_up() {
        let mut view = scene("pro-offline");
        view.pro.problem = None;
        view.pro.doing = ProDoing::Asking;
        let seen = Seen::of(&content(&view).body);
        assert!(seen.says("Looking up your subscription"));
        assert_eq!(seen.labels(), ["Sign out"]);
    }

    #[test]
    fn the_page_never_shouts_promises_no_refund_and_names_no_one_else() {
        for name in crate::mock::SCENES {
            let seen = seen(name);
            for text in &seen.texts {
                assert!(!text.contains('!'), "{name}: {text}");
                let lower = text.to_lowercase();
                for word in ["refund", "money back", "stripe", "google cloud speech"] {
                    assert!(!lower.contains(word), "{name}: {text}");
                }
            }
        }
    }

    #[test]
    fn hours_and_times_read_naturally() {
        assert_eq!(hours(144_000), "40 hours");
        assert_eq!(hours(3600), "1 hour");
        assert_eq!(hours(0), "0 hours");
        assert_eq!(hours(5400), "1 h 30 min");
        assert_eq!(used(0), "0 min");
        assert_eq!(used(5210), "1 h 26 min");
        assert_eq!(used(45_000), "12 h 30 min");
        assert_eq!(used(144_000), "40 h");
        assert_eq!(used(59), "0 min");
        assert_eq!(ago(0), "just now");
        assert_eq!(ago(1), "1 minute ago");
        assert_eq!(ago(59), "59 minutes ago");
        assert_eq!(ago(60), "1 hour ago");
        assert_eq!(ago(119), "1 hour ago");
        assert_eq!(ago(300), "5 hours ago");
        assert_eq!(count(10), "ten");
        assert_eq!(count(12), "12");
    }
}
