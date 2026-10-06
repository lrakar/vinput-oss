//! What more than one page needs: the bar for a notice, how far along a
//! download is, the edit of a setting that is text, and what the tests of
//! the pages read a page with.

use vinput_view::{Change, Command, NoticeKind, PRO_MODEL, Page, Pro, ProWho, View};

use crate::node::{Action, Node, Severity, human_size};

/// Goes between the facts on a line of detail.
pub(super) const DOT: &str = " \u{b7} ";

/// The bar for the notice in `view`, if it is about `page`, with a cross
/// that puts it away.
pub(super) fn notice_bar(view: &View, page: Page) -> Option<Node> {
    let notice = view.notice.as_ref().filter(|notice| notice.page == page)?;
    Some(Node::InfoBar {
        severity: match notice.kind {
            NoticeKind::Info => Severity::Info,
            NoticeKind::Success => Severity::Success,
            NoticeKind::Error => Severity::Critical,
        },
        text: notice.text.clone(),
        action: None,
        dismiss: Some(Action::send(Command::DismissNotice)),
    })
}

/// Why `VinPUT` Pro cannot be dictated with right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProTrouble {
    /// Nobody is signed in.
    SignedOut,
    /// Whoever is signed in never subscribed.
    NoSubscription,
    /// Their subscription is over.
    Ended,
    /// A payment is open or has failed.
    Unpaid,
    /// The month's allowance is used up.
    UsedUp,
}

impl ProTrouble {
    /// What it is, as the start of a sentence.
    pub(super) fn words(self) -> &'static str {
        match self {
            Self::SignedOut => "You are signed out",
            Self::NoSubscription => "You have no subscription yet",
            Self::Ended => "Your subscription has ended",
            Self::Unpaid => "The payment did not go through",
            Self::UsedUp => "This month's allowance is used up",
        }
    }
}

/// Whether a subscription in this state, in the payment service's word, has
/// a payment open or failed.
pub(super) fn owing(status: &str) -> bool {
    matches!(status, "past_due" | "unpaid" | "incomplete" | "paused")
}

/// Why `VinPUT` Pro cannot be dictated with right now, for the pages on which
/// the model is chosen; `None` while it can, and while the server has not
/// said yet how the account stands.
pub(super) fn pro_trouble(pro: &Pro) -> Option<ProTrouble> {
    match pro.who {
        ProWho::SignedOut | ProWho::SigningIn => return Some(ProTrouble::SignedOut),
        ProWho::SignedIn if !pro.known => return None,
        ProWho::SignedIn => {}
    }
    match (pro.entitled, pro.status.as_str()) {
        (true, _) if pro.cap_seconds > 0 && pro.used_seconds >= pro.cap_seconds => {
            Some(ProTrouble::UsedUp)
        }
        (true, _) => None,
        (false, "") => Some(ProTrouble::NoSubscription),
        (false, status) if owing(status) => Some(ProTrouble::Unpaid),
        (false, _) => Some(ProTrouble::Ended),
    }
}

/// Whether the month's hours are used up, or nine tenths of them are: when
/// it is worth knowing what happens once they are.
pub(super) fn hours_low(pro: &Pro) -> bool {
    pro.who == ProWho::SignedIn
        && pro.known
        && pro.entitled
        && pro.cap_seconds > 0
        && pro.used_seconds.saturating_mul(10) >= pro.cap_seconds.saturating_mul(9)
}

/// The least a month of `VinPUT` Pro costs, whichever tier that is:
/// `\u{20ac}2.50 a month`.
pub(super) fn least_a_month(pro: &Pro) -> String {
    let least = pro.offers.iter().map(|offer| offer.monthly).min();
    format!(
        "{} a month",
        price(least.unwrap_or_default(), &pro.currency)
    )
}

/// The least a first month costs, where the server has a price for it that
/// whoever is looking would pay: `\u{20ac}1.49`.
pub(super) fn least_first(pro: &Pro) -> Option<String> {
    let firsts = pro.offers.iter().map(|offer| offer.first);
    let least = firsts.filter(|first| *first > 0).min()?;
    Some(price(least, &pro.currency))
}

/// An amount in hundredths of `currency` as people write it: `\u{20ac}6`,
/// `\u{20ac}5.90`, `CHF 6`.
pub(super) fn price(hundredths: u32, currency: &str) -> String {
    let (whole, rest) = (hundredths / 100, hundredths % 100);
    let amount = if rest == 0 {
        whole.to_string()
    } else {
        format!("{whole}.{rest:02}")
    };
    match currency {
        "eur" => format!("\u{20ac}{amount}"),
        "usd" => format!("${amount}"),
        "gbp" => format!("\u{a3}{amount}"),
        other => format!("{} {amount}", other.to_ascii_uppercase()),
    }
}

/// A moment, in seconds since 1970, as its day and month: `5 November`.
pub(super) fn day(seconds: u64) -> String {
    let (day, month, _) = calendar(seconds);
    format!("{day} {month}")
}

/// A moment, in seconds since 1970, as its day: `5 November 2026`.
pub(super) fn date(seconds: u64) -> String {
    let (day, month, year) = calendar(seconds);
    format!("{day} {month} {year}")
}

/// The day, the name of the month and the year of a moment, in seconds
/// since 1970.
fn calendar(seconds: u64) -> (u64, &'static str, u64) {
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    // Days since 1970 to a date of the calendar, after Howard Hinnant's
    // `civil_from_days`: years are counted from a first of March, so that
    // the leap day is the last day of one.
    let days = seconds / 86_400 + 719_468;
    let era = days / 146_097;
    let day_of_era = days % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = (shifted_month + 2) % 12;
    let year = year_of_era + era * 400 + u64::from(month < 2);
    let name = usize::try_from(month)
        .ok()
        .and_then(|month| MONTHS.get(month))
        .copied()
        .unwrap_or_default();
    (day, name, year)
}

/// Whether dictation uses `VinPUT` Pro.
pub(super) fn pro_in_use(view: &View) -> bool {
    let mut models = view.models.list.iter();
    models.any(|model| model.id == PRO_MODEL && model.in_use)
}

/// How far along a download is, for a progress bar: `done` of `total` as a
/// part of one, never more than all of it. `None`, a bar that only moves,
/// while the size is not known.
pub(super) fn fraction(done: u64, total: u64) -> Option<f32> {
    if total == 0 {
        return None;
    }
    let thousandths = u128::from(done.min(total)) * 1000 / u128::from(total);
    u16::try_from(thousandths)
        .ok()
        .map(|thousandths| f32::from(thousandths) / 1000.0)
}

/// How far along a download is, in words: `412 MB of 669 MB`, never more
/// than all of it, and `Starting` while the size is not known.
pub(super) fn so_far(done: u64, total: u64) -> String {
    if total == 0 {
        "Starting".to_owned()
    } else {
        format!("{} of {}", human_size(done.min(total)), human_size(total))
    }
}

/// What finishing an edit of a setting that is text does: nothing if it
/// reads as it did, otherwise the change goes to the background process.
pub(super) fn set_text(current: &str, typed: &str, change: fn(String) -> Change) -> Action {
    let typed = typed.trim();
    if typed == current.trim() {
        Action::None
    } else {
        Action::send(Command::Set(change(typed.to_owned())))
    }
}

/// Reads a tree of nodes the way a test wants to: what it says, what can be
/// pressed and what pressing it would do.
#[cfg(test)]
pub(crate) mod probe {
    use vinput_view::{Change, Command, View};

    use crate::mock;
    use crate::node::{Action, Button, ButtonKind, Node, Setting, Severity};

    /// The text a [`Action::Submit`] is tried with.
    pub(crate) const TYPED: &str = "typed";

    /// Something that can be pressed: a button, a link, the button or the
    /// cross of a message bar.
    pub(crate) struct Press {
        /// Its caption, or its hint if it has none.
        pub(crate) label: String,
        /// Whether it can be pressed.
        pub(crate) enabled: bool,
        /// How much it stands out.
        pub(crate) kind: ButtonKind,
        /// What pressing it does.
        pub(crate) action: Action,
    }

    /// Everything found in a tree of nodes.
    #[derive(Default)]
    pub(crate) struct Seen {
        /// Every piece of text, captions and the points of lists included,
        /// in the order it appears.
        pub(crate) texts: Vec<String>,
        /// Everything that can be pressed.
        pub(crate) presses: Vec<Press>,
        /// The captions of the badges.
        pub(crate) badges: Vec<String>,
        /// The message bars.
        pub(crate) bars: Vec<(Severity, String)>,
        /// The progress bars.
        pub(crate) progress: Vec<Option<f32>>,
        /// The colors of the bars that show how much of something is used.
        pub(crate) gauges: Vec<crate::node::Tone>,
        /// The bars that say something is being waited for.
        pub(crate) busy: Vec<String>,
        /// The switches: whether each is on, and what flipping it does.
        pub(crate) toggles: Vec<(bool, Action)>,
        /// The text fields: their keys and what they show while empty.
        pub(crate) fields: Vec<(&'static str, String)>,
    }

    impl Seen {
        /// Everything in `nodes` and under them.
        pub(crate) fn of(nodes: &[Node]) -> Self {
            let mut seen = Self::default();
            for node in nodes {
                seen.visit(node);
            }
            seen
        }

        /// Everything in one settings row.
        pub(crate) fn of_setting(setting: &Setting) -> Self {
            let mut seen = Self::default();
            seen.setting(setting);
            seen
        }

        /// The captions of what can be pressed right now.
        pub(crate) fn labels(&self) -> Vec<&str> {
            let enabled = self.presses.iter().filter(|press| press.enabled);
            enabled.map(|press| press.label.as_str()).collect()
        }

        /// The commands that pressing each enabled thing would send.
        pub(crate) fn commands(&self) -> Vec<Command> {
            let enabled = self.presses.iter().filter(|press| press.enabled);
            enabled.filter_map(|press| command(&press.action)).collect()
        }

        /// Whether any text contains `part`.
        pub(crate) fn says(&self, part: &str) -> bool {
            self.texts.iter().any(|text| text.contains(part))
        }

        fn press(&mut self, label: &str, enabled: bool, kind: ButtonKind, action: &Action) {
            self.texts.push(label.to_owned());
            self.presses.push(Press {
                label: label.to_owned(),
                enabled,
                kind,
                action: action.clone(),
            });
        }

        fn button(&mut self, button: &Button) {
            let label = if button.label.is_empty() {
                &button.hint
            } else {
                &button.label
            };
            self.press(label, button.enabled, button.kind, &button.action);
        }

        fn setting(&mut self, setting: &Setting) {
            self.texts.push(setting.title.clone());
            self.texts.push(setting.detail.clone());
            if let Some(control) = &setting.control {
                self.visit(control);
            }
            for node in &setting.below {
                self.visit(node);
            }
        }

        fn visit(&mut self, node: &Node) {
            match node {
                Node::Column(children) | Node::Row(children) | Node::Card(children) => {
                    for child in children {
                        self.visit(child);
                    }
                }
                Node::Header(text) | Node::Text { text, .. } => self.texts.push(text.clone()),
                Node::List(points) => self.texts.extend(points.iter().cloned()),
                Node::Setting(setting) => self.setting(setting),
                Node::Button(button) => self.button(button),
                Node::Link { label, action } => {
                    self.press(label, true, ButtonKind::Subtle, action);
                }
                Node::Toggle { on, action, .. } => self.toggles.push((*on, action(!*on))),
                Node::Field(field) => {
                    self.fields.push((field.key, field.placeholder.clone()));
                }
                Node::Progress(fraction) => self.progress.push(*fraction),
                Node::Gauge { value, tone } => {
                    self.progress.push(Some(*value));
                    self.gauges.push(*tone);
                }
                Node::Busy(text) => {
                    self.texts.push(text.clone());
                    self.busy.push(text.clone());
                }
                Node::Badge { text, .. } => {
                    self.texts.push(text.clone());
                    self.badges.push(text.clone());
                }
                Node::InfoBar {
                    severity,
                    text,
                    action,
                    dismiss,
                } => {
                    self.texts.push(text.clone());
                    self.bars.push((*severity, text.clone()));
                    if let Some((label, action)) = action {
                        self.press(label, true, ButtonKind::Standard, action);
                    }
                    if let Some(dismiss) = dismiss {
                        self.press("Dismiss", true, ButtonKind::Subtle, dismiss);
                    }
                }
                Node::Spring | Node::Icon(..) | Node::Choice { .. } | Node::Hotkey { .. } => {}
            }
        }
    }

    /// The command `action` would send; a submitted field holds [`TYPED`],
    /// and of two things done one after the other it is the first command.
    pub(crate) fn command(action: &Action) -> Option<Command> {
        match action {
            Action::Send(command) => Some(command.clone()),
            Action::Submit { make, .. } => Some(make(TYPED.to_owned())),
            Action::Both(first, second) => command(first).or_else(|| command(second)),
            Action::None
            | Action::Go(_)
            | Action::Close
            | Action::Accept
            | Action::Copy(_)
            | Action::Paste(_) => None,
        }
    }

    /// The setting `action` changes, if that is what it does.
    pub(crate) fn set(action: &Action) -> Option<Change> {
        match command(action) {
            Some(Command::Set(change)) => Some(change),
            _ => None,
        }
    }

    /// Every node in `nodes` and under them, parents before their children.
    pub(crate) fn all(nodes: &[Node]) -> Vec<&Node> {
        let mut found = Vec::new();
        for node in nodes {
            found.push(node);
            match node {
                Node::Column(children) | Node::Row(children) | Node::Card(children) => {
                    found.extend(all(children));
                }
                Node::Setting(row) => {
                    if let Some(control) = &row.control {
                        found.extend(all(std::slice::from_ref(&**control)));
                    }
                    found.extend(all(&row.below));
                }
                _ => {}
            }
        }
        found
    }

    /// Every settings row in `nodes` and under them, top to bottom.
    pub(crate) fn settings(nodes: &[Node]) -> Vec<&Setting> {
        all(nodes)
            .into_iter()
            .filter_map(|node| match node {
                Node::Setting(setting) => Some(setting),
                _ => None,
            })
            .collect()
    }

    /// The settings row titled `title` among the nodes [`all`] found.
    pub(crate) fn setting_row<'a>(nodes: &[&'a Node], title: &str) -> &'a Setting {
        let found = nodes.iter().find_map(|node| match node {
            Node::Setting(row) if row.title == title => Some(row),
            _ => None,
        });
        let Some(row) = found else {
            unreachable!("the page has no row called {title:?}");
        };
        row
    }

    /// The buttons among the nodes [`all`] found.
    pub(crate) fn buttons<'a>(nodes: &[&'a Node]) -> Vec<&'a Button> {
        nodes
            .iter()
            .filter_map(|node| match node {
                Node::Button(button) => Some(button),
                _ => None,
            })
            .collect()
    }

    /// The keys of everything that keeps something between rebuilds.
    pub(crate) fn keys(nodes: &[&Node]) -> Vec<&'static str> {
        nodes
            .iter()
            .filter_map(|node| match node {
                Node::Field(field) => Some(field.key),
                Node::Hotkey { key, .. } | Node::Choice { key, .. } => Some(*key),
                _ => None,
            })
            .collect()
    }

    /// What finishing an edit of the field or hotkey field called `key`
    /// with `text` in it changes.
    pub(crate) fn typed(nodes: &[&Node], key: &str, text: &str) -> Option<Change> {
        nodes.iter().find_map(|node| match node {
            Node::Field(field) if field.key == key => set(&(field.action)(text.to_owned())),
            Node::Hotkey {
                key: name, action, ..
            } if *name == key => set(&action(text.to_owned())),
            _ => None,
        })
    }

    /// What flipping the switch of `row` to `on` changes.
    pub(crate) fn flipped(row: &Setting, on: bool) -> Option<Change> {
        match row.control.as_deref() {
            Some(Node::Toggle { action, .. }) => set(&action(on)),
            _ => None,
        }
    }

    /// The canned scene called `name`.
    pub(crate) fn scene(name: &str) -> View {
        let view = mock::scene(name);
        assert!(view.is_some(), "no scene called {name}");
        view.unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use vinput_view::{Notice, NoticeKind};

    use super::probe::{command, scene, set};
    use super::*;

    #[test]
    fn dates_and_prices_read_the_way_people_write_them() {
        assert_eq!(date(0), "1 January 1970");
        assert_eq!(date(1_793_837_814), "5 November 2026");
        assert_eq!(date(951_782_400), "29 February 2000");
        assert_eq!(date(1_709_251_199), "29 February 2024");
        assert_eq!(date(1_709_251_200), "1 March 2024");
        assert_eq!(date(1_767_225_599), "31 December 2025");
        assert_eq!(day(1_793_491_200), "1 November");
        assert_eq!(price(600, "eur"), "\u{20ac}6");
        // Cents are written only where there are any, and then both.
        assert_eq!(price(500, "eur"), "\u{20ac}5");
        assert_eq!(price(250, "eur"), "\u{20ac}2.50");
        assert_eq!(price(205, "eur"), "\u{20ac}2.05");
        assert_eq!(price(2400, "eur"), "\u{20ac}24");
        assert_eq!(price(4900, "eur"), "\u{20ac}49");
        assert_eq!(price(5900, "eur"), "\u{20ac}59");
        assert_eq!(price(590, "eur"), "\u{20ac}5.90");
        assert_eq!(price(605, "usd"), "$6.05");
        assert_eq!(price(500, "gbp"), "\u{a3}5");
        assert_eq!(price(600, "chf"), "CHF 6");
        assert_eq!(price(349, "eur"), "\u{20ac}3.49");
        assert_eq!(price(149, "eur"), "\u{20ac}1.49");
        // The least a first month costs, among the tiers that have such a
        // price; none where no tier has.
        let mut pro = Pro::default();
        assert_eq!(least_first(&pro), None);
        pro.offers[1].first = 349;
        assert_eq!(least_first(&pro).as_deref(), Some("\u{20ac}3.49"));
        pro.offers[0].first = 149;
        assert_eq!(least_first(&pro).as_deref(), Some("\u{20ac}1.49"));
    }

    #[test]
    fn what_keeps_pro_from_being_used_is_told_apart() {
        let signed_in = |entitled: bool, status: &str| Pro {
            who: ProWho::SignedIn,
            known: true,
            entitled,
            status: status.to_owned(),
            used_seconds: 10,
            cap_seconds: 100,
            ..Pro::default()
        };
        assert_eq!(pro_trouble(&Pro::default()), Some(ProTrouble::SignedOut));
        assert_eq!(pro_trouble(&signed_in(true, "active")), None);
        // Behind with a payment, and usable still.
        assert_eq!(pro_trouble(&signed_in(true, "past_due")), None);
        assert_eq!(
            pro_trouble(&signed_in(false, "")),
            Some(ProTrouble::NoSubscription)
        );
        for status in ["past_due", "unpaid", "incomplete", "paused"] {
            assert_eq!(
                pro_trouble(&signed_in(false, status)),
                Some(ProTrouble::Unpaid)
            );
        }
        assert_eq!(
            pro_trouble(&signed_in(false, "canceled")),
            Some(ProTrouble::Ended)
        );
        let mut full = signed_in(true, "active");
        full.used_seconds = 100;
        assert_eq!(pro_trouble(&full), Some(ProTrouble::UsedUp));
        // Until the server has said, nothing is held against the account.
        let mut unknown = signed_in(false, "");
        unknown.known = false;
        assert_eq!(pro_trouble(&unknown), None);
        for trouble in [ProTrouble::NoSubscription, ProTrouble::UsedUp] {
            assert!(!trouble.words().ends_with('.'));
        }
    }

    #[test]
    fn progress_stays_between_nothing_and_everything() {
        assert_eq!(fraction(0, 0), None);
        assert_eq!(fraction(5, 0), None);
        assert_eq!(fraction(0, 10), Some(0.0));
        assert_eq!(fraction(5, 10), Some(0.5));
        assert_eq!(fraction(20, 10), Some(1.0));
        assert_eq!(fraction(u64::MAX, u64::MAX), Some(1.0));
    }

    #[test]
    fn how_far_is_said_in_sizes_and_never_past_the_end() {
        assert_eq!(so_far(0, 0), "Starting");
        assert_eq!(so_far(412 << 20, 669 << 20), "412 MB of 669 MB");
        assert_eq!(so_far(9 << 20, 6 << 20), "6 MB of 6 MB");
    }

    #[test]
    fn text_is_sent_trimmed_and_only_when_it_changed() {
        assert!(set_text("en-US", " en-US ", Change::Languages).is_none());
        assert_eq!(
            set(&set_text("en-US", " sl-SI ", Change::Languages)),
            Some(Change::Languages("sl-SI".to_owned()))
        );
    }

    #[test]
    fn a_notice_is_a_bar_on_its_own_page_only() {
        let mut view = scene("settings");
        view.notice = Some(Notice {
            kind: NoticeKind::Success,
            text: "Saved.".to_owned(),
            page: Page::Advanced,
        });
        assert!(notice_bar(&view, Page::General).is_none());
        let Some(Node::InfoBar {
            severity: Severity::Success,
            text,
            action: None,
            dismiss: Some(dismiss),
        }) = notice_bar(&view, Page::Advanced)
        else {
            unreachable!("a bar with a cross was expected");
        };
        assert_eq!(text, "Saved.");
        assert_eq!(command(&dismiss), Some(Command::DismissNotice));
    }

    #[test]
    fn the_probe_reads_what_two_actions_in_a_row_send() {
        let both = Action::Go(Page::Models).then(Action::send(Command::ClearFinder));
        assert_eq!(command(&both), Some(Command::ClearFinder));
        assert_eq!(command(&Action::Accept), None);
        assert_eq!(command(&Action::Copy("text".to_owned())), None);
    }
}
