//! The page that sets up a Google service of one's own, a step at a time.
//!
//! Four steps, one in sight: which service, how to get what it needs from
//! Google, entering it, and done. What is entered is checked with Google
//! as it is entered and the answer is said under it, in words about what to
//! do next. The step and everything entered belong to the background
//! process; this page only says what it is told. It is reached from the
//! online models of the Models page, each of which opens it at its service.

use vinput_view::{
    Command, Google, GoogleCheck, GoogleProblem, GoogleService, GoogleStep, Page, Setup, View,
};

use crate::node::{
    Action, ButtonKind, Content, Icon, Key, Node, Severity, Tone, accent_button, badge, button,
    card, choice, field, icon_button, info_bar, link, mono, note, plain_setting, row, setting,
};

/// Where a Gemini API key is made.
const AI_STUDIO: &str = "https://aistudio.google.com/apikey";
/// Where the Speech-to-Text API is switched on for a project.
const CLOUD_CONSOLE: &str = "https://console.cloud.google.com/apis/library/speech.googleapis.com";

/// What signs this PC in to Google Cloud, typed into a terminal.
const SIGN_IN_COMMAND: &str = "gcloud auth application-default login";

/// The field a Gemini API key is pasted in.
const KEY: Key = "google-key";
/// What stands in the field for each character of a key that was entered:
/// the page is told how long the key is and never the key. The field hides
/// what it holds anyway; this only has to be something no key contains.
const KEPT_BACK: char = '\u{2022}';
/// The field for the Google Cloud project.
const PROJECT: Key = "google-project";
/// The list of locations.
const LOCATION: Key = "google-location";

/// The Speech-to-Text locations offered, as Google names them and as
/// people do. One that was set by hand and is not among them is listed as
/// it is written.
const LOCATIONS: [(&str, &str); 8] = [
    ("eu", "European Union"),
    ("us", "United States"),
    ("europe-west2", "London"),
    ("europe-west3", "Frankfurt"),
    ("northamerica-northeast1", "Montr\u{e9}al"),
    ("asia-northeast1", "Tokyo"),
    ("asia-southeast1", "Singapore"),
    ("asia-south1", "Mumbai"),
];

fn ask(what: Setup) -> Action {
    Action::send(Command::Google(what))
}

/// Leaving the setup for the list of models.
fn leave() -> Action {
    ask(Setup::Close).then(Action::Go(Page::Models))
}

/// Whether the key that is kept is what the third step shows, rather than
/// the field for one: a key is stored, is not being replaced, and no other
/// was entered.
fn shows_stored(google: &Google, stored: bool) -> bool {
    stored && !google.replacing && google.key_length == 0
}

/// What the page shows for `view`.
#[must_use]
pub fn content(view: &View) -> Content {
    let google = &view.google;
    let gemini = google.service == GoogleService::Gemini;
    let stored = view.gemini_key_stored;
    let (number, title, mut body) = match (google.step, gemini) {
        (GoogleStep::Service, _) => (1, "Use your own Google service", service()),
        (GoogleStep::Get, true) => (2, "Get a Gemini API key", get_key()),
        (GoogleStep::Get, false) => (2, "Prepare your Google Cloud project", prepare()),
        (GoogleStep::Enter, true) if shows_stored(google, stored) => {
            (3, "Your Gemini API key", enter_key(google, stored))
        }
        (GoogleStep::Enter, true) => (3, "Paste your key", enter_key(google, stored)),
        (GoogleStep::Enter, false) => (3, "Connect your project", enter_project(google)),
        (GoogleStep::Done, _) => (4, "Ready to dictate", done(view)),
    };
    body.insert(0, note(format!("Step {number} of 4")));
    Content {
        pinned: Vec::new(),
        title: title.to_owned(),
        body,
        footer: forward(google),
        back: Some(back(google.step)),
    }
}

/// What the arrow beside the title does: one step back, and from the first
/// step, and the last, out of the setup. It is the one way back there is.
fn back(step: GoogleStep) -> Action {
    match step {
        GoogleStep::Service | GoogleStep::Done => leave(),
        GoogleStep::Get => ask(Setup::Step(GoogleStep::Service)),
        GoogleStep::Enter => ask(Setup::Step(GoogleStep::Get)),
    }
}

/// The button at the bottom, which is the way on. The way on from entering
/// is open once Google has said it works.
fn forward(google: &Google) -> Vec<Node> {
    let on = match google.step {
        GoogleStep::Service => return Vec::new(),
        GoogleStep::Get => accent_button("Next", ask(Setup::Step(GoogleStep::Enter))),
        GoogleStep::Enter => {
            accent_button("Finish", ask(Setup::Finish)).enabled(google.check == GoogleCheck::Works)
        }
        GoogleStep::Done => accent_button("Done", leave()),
    };
    vec![on.into()]
}

/// The first step: the two services, each in a sentence on what it is and
/// what trying it costs.
fn service() -> Vec<Node> {
    let open = |service| ask(Setup::Open(service));
    vec![
        card([
            setting(Icon::Key, "Gemini API key")
                .detail(
                    "A key from Google AI Studio, made in a minute. Free to try within \
                     Google's limits.",
                )
                .control(badge("Recommended", Tone::Accent))
                .action(open(GoogleService::Gemini))
                .into(),
            setting(Icon::Cloud, "Google Cloud Speech-to-Text")
                .detail("For people who already use Google Cloud. Needs a project with billing.")
                .action(open(GoogleService::Speech))
                .into(),
        ]),
        note(
            "Either way your speech goes to Google under your own key, and the cost is on your \
             Google bill.",
        ),
    ]
}

/// A step of an instruction: its number, where the other rows have their
/// icon, what to do, and a word more.
fn point(number: u8, what: &str, more: &str) -> crate::node::Setting {
    plain_setting(what).numbered(number).detail(more)
}

/// The button that opens a page of Google's in the browser.
fn browser(label: &str, address: &str) -> Node {
    button(label, Action::send(Command::OpenLink(address.to_owned())))
        .icon(Icon::OpenExternal)
        .into()
}

/// The second step for Gemini: where a key comes from.
fn get_key() -> Vec<Node> {
    vec![card([
        point(1, "Sign in to Google AI Studio", "With any Google account")
            .control(browser("Open Google AI Studio", AI_STUDIO))
            .into(),
        point(
            2,
            "Choose Create API key",
            "If it asks for a project, let it make one",
        )
        .into(),
        point(3, "Copy the key", "You paste it here in the next step").into(),
    ])]
}

/// The second step for Google Cloud: what the project has to have.
fn prepare() -> Vec<Node> {
    vec![card([
        point(
            1,
            "Switch on the Speech-to-Text API",
            "In the Google Cloud console, for the project you want to use",
        )
        .control(browser("Open the Cloud console", CLOUD_CONSOLE))
        .into(),
        point(
            2,
            "See that the project has billing",
            "Google does not recognize speech for a project without it",
        )
        .into(),
        point(3, "Have a way to sign in", "Run this on this PC")
            .below(row([
                mono(SIGN_IN_COMMAND),
                icon_button(
                    Icon::Copy,
                    "Copy the command",
                    Action::Copy(SIGN_IN_COMMAND.to_owned()),
                )
                .into(),
            ]))
            .below(note(
                "or download a service account key (role Cloud Speech Client) as JSON",
            ))
            .into(),
    ])]
}

/// The third step for Gemini: the key that is stored, on one line with
/// what can be done about it, or one field for a key, which goes on showing
/// that it holds one while Google is asked and after it has answered; and
/// what Google makes of the key.
fn enter_key(google: &Google, stored: bool) -> Vec<Node> {
    let line = setting(Icon::Key, "Gemini API key");
    let line = if shows_stored(google, stored) {
        let remove = button("Remove", ask(Setup::RemoveKey)).kind(ButtonKind::Subtle);
        line.detail("Stored in Windows Credential Manager")
            .control(row([
                button("Replace", ask(Setup::Replace)).into(),
                remove.into(),
            ]))
    } else {
        // Whatever of the field is still what stood in for the key that
        // was entered is not a key, and is not handed in again.
        let key = field(KEY, entered(google), |typed| {
            let key: String = typed.chars().filter(|c| *c != KEPT_BACK).collect();
            match key.trim() {
                "" => Action::None,
                key => ask(Setup::Key(key.to_owned())),
            }
        })
        .placeholder("Paste the key here")
        .secret()
        .wide()
        .eager();
        let paste = button("Paste", Action::Paste(KEY)).icon(Icon::Clipboard);
        // Replacing a key takes nothing away until the new one works.
        let detail = if stored {
            "The stored key stays until you finish with a new one that works"
        } else {
            "Kept in Windows Credential Manager, never in a file"
        };
        line.detail(detail).below(row([key.into(), paste.into()]))
    };
    let mut body = vec![card([line.into()])];
    body.extend(answer(google));
    body
}

/// What the field of a key holds for the key that was entered and is not
/// kept yet: as many characters as it has, and none of them its own.
fn entered(google: &Google) -> String {
    std::iter::repeat_n(KEPT_BACK, usize::from(google.key_length)).collect()
}

/// The third step for Google Cloud: the project, where it is handled and
/// how Vinput signs in, on one card, and what Google makes of them.
fn enter_project(google: &Google) -> Vec<Node> {
    let before = google.project_id.clone();
    let project = field(PROJECT, &google.project_id, move |typed| {
        let typed = typed.trim();
        if typed == before {
            Action::None
        } else {
            ask(Setup::Project(typed.to_owned()))
        }
    })
    .placeholder("Project ID")
    .eager();

    let mut names: Vec<String> = LOCATIONS
        .iter()
        .map(|(code, name)| format!("{name} ({code})"))
        .collect();
    let chosen = LOCATIONS
        .iter()
        .position(|(code, _)| *code == google.location);
    // One that was set by hand is listed last, as it is written.
    let by_hand = google.location.clone();
    let selected = chosen.unwrap_or_else(|| {
        names.push(by_hand.clone());
        LOCATIONS.len()
    });
    let location = choice(LOCATION, names, selected, move |index| {
        let code = LOCATIONS
            .get(index)
            .map_or(by_hand.as_str(), |(code, _)| code);
        ask(Setup::Location(code.to_owned()))
    });

    let (how, tone) = match (google.account.as_str(), google.sign_in_found) {
        ("", true) => (
            "The Google sign-in on this PC, made with gcloud".to_owned(),
            Tone::Secondary,
        ),
        ("", false) => (
            "No Google sign-in on this PC yet. Choose a key file, or sign in with gcloud"
                .to_owned(),
            Tone::Secondary,
        ),
        (account, _) => (
            format!("The key of {account}, kept in Windows Credential Manager"),
            Tone::Secondary,
        ),
    };
    let mut credentials = setting(Icon::Key, "Credentials")
        .detail(how)
        .detail_tone(tone)
        .control(button("Choose a key file", ask(Setup::PickKey)).into());
    if !google.account.is_empty() {
        credentials = credentials.below(link(
            "Use the Google sign-in on this PC instead",
            ask(Setup::UseSignIn),
        ));
    }

    let mut body = vec![card([
        setting(Icon::Cloud, "Project ID")
            .detail("The project Google bills, by its ID rather than its name")
            .control(project.into())
            .into(),
        setting(Icon::Globe, "Region")
            .detail("Where Google handles your speech")
            .control(location)
            .into(),
        credentials.into(),
    ])];
    body.extend(answer(google));
    body
}

/// What Google made of what is entered: that it is being asked, that it
/// works, or what is wrong and what to do about it.
fn answer(google: &Google) -> Option<Node> {
    let gemini = google.service == GoogleService::Gemini;
    let (problem, said) = match &google.check {
        GoogleCheck::Idle => return None,
        GoogleCheck::Checking => return Some(Node::Busy("Checking with Google".to_owned())),
        GoogleCheck::Works => {
            let text = if gemini {
                "Google accepts this key."
            } else {
                "Google accepts the project and its credentials."
            };
            return Some(info_bar(Severity::Success, text));
        }
        GoogleCheck::Failed { problem, said } => (*problem, said.as_str()),
    };
    let text = match (problem, gemini) {
        (GoogleProblem::WrongKey, true) => {
            "Google does not know this key. Copy all of it again in Google AI Studio and paste \
             it here."
        }
        (GoogleProblem::WrongKey, false) => {
            "Google refused the credentials. Sign in again with gcloud, or choose a new key file."
        }
        (GoogleProblem::NoAccess, true) => {
            "The key is real, but it may not use the Gemini API. Make a new key in Google AI \
             Studio, or lift the restriction on this one."
        }
        (GoogleProblem::NoAccess, false) => {
            "The account may not use Speech-to-Text in this project. Switch the Speech-to-Text \
             API on for it, and give the account the role Cloud Speech Client."
        }
        (GoogleProblem::Billing, true) => {
            "Google wants billing set up for this key before it can be used. Set it up in \
             Google AI Studio, then check again."
        }
        (GoogleProblem::Billing, false) => {
            "This project has no billing. Link a billing account to it in the Google Cloud \
             console, then check again."
        }
        (GoogleProblem::Quota, _) => {
            "Google accepts this, but the allowance is used up for now. Try again in a while, \
             or raise the limit at Google."
        }
        (GoogleProblem::NotFound, _) => {
            "Google does not know this project in this region. Check the project ID, which is \
             not its name or its number, and the region."
        }
        (GoogleProblem::NoSignIn, _) => {
            "There is no Google sign-in on this PC. Run gcloud auth application-default login, \
             or choose a key file."
        }
        (GoogleProblem::BadFile, _) => {
            "That file is not a service account key. In the Google Cloud console, open the \
             service account, add a key and download it as JSON."
        }
        (GoogleProblem::Offline, true) => {
            "Google could not be reached. Check your internet connection, then check again."
        }
        // A region Google does not serve has no server to reach either.
        (GoogleProblem::Offline, false) => {
            "Google could not be reached. Check your internet connection, or try another \
             region, then check again."
        }
        (GoogleProblem::Other, _) => "Google did not give a clear answer.",
    };
    let text = if problem == GoogleProblem::Other && !said.is_empty() {
        format!("{text} It said: {said}")
    } else {
        text.to_owned()
    };
    // A wrong key is put right by pasting another, not by asking again.
    let again = !matches!(
        (problem, gemini),
        (GoogleProblem::WrongKey, true) | (GoogleProblem::BadFile, _)
    );
    Some(Node::InfoBar {
        severity: Severity::Critical,
        text,
        action: again.then(|| ("Check again".to_owned(), ask(Setup::Check))),
        dismiss: None,
    })
}

/// The last step: what dictation uses now, and where to change it.
fn done(view: &View) -> Vec<Node> {
    let model = match view.google.model.as_str() {
        "" => "The Google model",
        model => model,
    };
    vec![
        card([
            setting(Icon::Check, format!("{model} is your speech model"))
                .detail(format!("Hold {} and speak to try it", view.settings.hotkey))
                .icon_tone(Tone::Success)
                .into(),
        ]),
        note("To change the key or the project later, open Models and choose Change on the model."),
    ]
}

#[cfg(test)]
mod tests {
    use super::super::common::probe::{Seen, all, command, scene};
    use super::*;
    use crate::mock;
    use crate::node::Field;

    fn sent(action: &Action) -> Option<Setup> {
        match command(action) {
            Some(Command::Google(what)) => Some(what),
            _ => None,
        }
    }

    /// What pressing the thing captioned `label` asks of the setup.
    fn pressed(seen: &Seen, label: &str) -> Option<Setup> {
        let press = seen.presses.iter().find(|press| press.label == label)?;
        sent(&press.action)
    }

    fn text_field(content: &Content, key: Key) -> &Field {
        let found = all(&content.body).into_iter().find_map(|node| match node {
            Node::Field(field) if field.key == key => Some(field),
            _ => None,
        });
        let Some(field) = found else {
            unreachable!("the page has no field called {key}");
        };
        field
    }

    /// The rows of the page that are steps of an instruction, by number.
    fn numbers(page: &Content) -> Vec<u8> {
        let rows = super::super::common::probe::settings(&page.body);
        rows.iter().filter_map(|row| row.number).collect()
    }

    /// Whether `action` leaves the setup and turns to the Models page.
    fn leaves(action: &Action) -> bool {
        let Action::Both(first, second) = action else {
            return false;
        };
        sent(first) == Some(Setup::Close) && matches!(**second, Action::Go(Page::Models))
    }

    #[test]
    fn every_step_says_where_it_is_and_has_a_way_out() {
        let mut titles = Vec::new();
        for name in mock::SCENES
            .iter()
            .filter(|name| name.starts_with("google-"))
        {
            let view = scene(name);
            let page = content(&view);
            // One way back: the arrow, which goes a step back, and out of
            // the setup from its first step and its last.
            let back = page.back.as_ref();
            match view.google.step {
                GoogleStep::Service | GoogleStep::Done => {
                    assert!(back.is_some_and(leaves), "{name}");
                }
                GoogleStep::Get => assert_eq!(
                    back.and_then(sent),
                    Some(Setup::Step(GoogleStep::Service)),
                    "{name}"
                ),
                GoogleStep::Enter => assert_eq!(
                    back.and_then(sent),
                    Some(Setup::Step(GoogleStep::Get)),
                    "{name}"
                ),
            }
            let footer = Seen::of(&page.footer);
            assert!(!footer.says("Back"), "{name}");
            assert!(footer.presses.len() <= 1, "{name}");
            let seen = Seen::of(&page.body);
            let step = seen.texts.iter().find(|text| text.starts_with("Step "));
            assert!(step.is_some_and(|step| step.ends_with(" of 4")), "{name}");
            for text in seen.texts.iter().chain(&Seen::of(&page.footer).texts) {
                assert!(!text.contains('!'), "{name}: {text}");
            }
            titles.push(page.title);
        }
        titles.dedup();
        assert!(titles.len() >= 6, "{titles:?}");
    }

    #[test]
    fn the_first_step_offers_both_services_and_recommends_the_simple_one() {
        let page = content(&scene("google-service"));
        assert!(page.footer.is_empty());
        let rows = super::super::common::probe::settings(&page.body);
        let [gemini, speech] = rows.as_slice() else {
            unreachable!("two services were expected");
        };
        assert_eq!(Seen::of_setting(gemini).badges, ["Recommended"]);
        assert_eq!(
            sent(&gemini.action),
            Some(Setup::Open(GoogleService::Gemini))
        );
        assert_eq!(
            sent(&speech.action),
            Some(Setup::Open(GoogleService::Speech))
        );
        assert_eq!(
            gemini.detail,
            "A key from Google AI Studio, made in a minute. Free to try within Google's limits."
        );
        assert_eq!(
            speech.detail,
            "For people who already use Google Cloud. Needs a project with billing."
        );
        assert!(Seen::of(&page.body).says(
            "Either way your speech goes to Google under your own key, and the cost is on your \
             Google bill."
        ));
    }

    #[test]
    fn getting_a_key_is_three_points_and_a_button_to_the_right_page() {
        for (name, address) in [
            ("google-gemini-get", AI_STUDIO),
            ("google-speech-get", CLOUD_CONSOLE),
        ] {
            let page = content(&scene(name));
            let seen = Seen::of(&page.body);
            // The numbers stand where the icons of other rows do, not in
            // the words.
            assert_eq!(numbers(&page), [1, 2, 3], "{name}");
            assert!(!seen.texts.iter().any(|text| text.starts_with("1.")));
            assert_eq!(
                seen.commands(),
                [Command::OpenLink(address.to_owned())],
                "{name}"
            );
            assert!(address.starts_with("https://"));
            let footer = Seen::of(&page.footer);
            assert_eq!(footer.labels(), ["Next"]);
            assert_eq!(
                pressed(&footer, "Next"),
                Some(Setup::Step(GoogleStep::Enter))
            );
        }
        // A row does not say twice what its button says.
        let page = content(&scene("google-gemini-get"));
        let rows = super::super::common::probe::settings(&page.body);
        assert_eq!(rows[0].title, "Sign in to Google AI Studio");
        assert_eq!(
            Seen::of_setting(rows[0]).labels(),
            ["Open Google AI Studio"]
        );
        // The command to type is set apart, and can be copied.
        let seen = Seen::of(&content(&scene("google-speech-get")).body);
        assert!(seen.texts.iter().any(|text| text == SIGN_IN_COMMAND));
        let copy = seen
            .presses
            .iter()
            .find(|press| press.label == "Copy the command");
        assert!(matches!(
            copy.map(|press| &press.action),
            Some(Action::Copy(command)) if command == SIGN_IN_COMMAND
        ));
        assert!(seen.says("or download a service account key (role Cloud Speech Client) as JSON"));
    }

    #[test]
    fn a_key_is_pasted_in_one_field_and_asked_about_at_once() {
        let page = content(&scene("google-gemini-enter"));
        let key = text_field(&page, KEY);
        assert!(key.secret && key.eager && key.wide && key.text.is_empty());
        assert_eq!(
            sent(&(key.action)(" AIzaExample \n".to_owned())),
            Some(Setup::Key("AIzaExample".to_owned()))
        );
        assert!((key.action)("  ".to_owned()).is_none());
        let seen = Seen::of(&page.body);
        assert_eq!(seen.fields.len(), 1);
        let paste = seen.presses.iter().find(|press| press.label == "Paste");
        assert!(matches!(
            paste.map(|press| &press.action),
            Some(Action::Paste(KEY))
        ));
        assert!(seen.bars.is_empty() && seen.progress.is_empty());
        assert!(!seen.says("Remove") && !seen.says("Replace"));
        // Nothing to finish with until Google has said yes.
        let footer = Seen::of(&page.footer);
        assert!(footer.labels().is_empty());
        assert_eq!(footer.presses.len(), 1);
    }

    #[test]
    fn a_key_that_was_entered_stays_in_its_field_hidden_while_google_is_asked_and_after() {
        for (name, finish) in [
            ("google-gemini-checking", false),
            ("google-gemini-entered", true),
            ("google-gemini-wrong-key", false),
            ("google-gemini-other", false),
        ] {
            let page = content(&scene(name));
            let key = text_field(&page, KEY);
            // As long as the key, hidden, and none of it the key.
            assert!(key.secret, "{name}");
            assert_eq!(key.text.chars().count(), 39, "{name}");
            assert!(key.text.chars().all(|c| c == KEPT_BACK), "{name}");
            // What stands in for it is never handed in as a key: only
            // what is typed or pasted over it is.
            assert!((key.action)(key.text.clone()).is_none(), "{name}");
            let mixed = format!("{}AIzaNew", key.text);
            assert_eq!(
                sent(&(key.action)(mixed)),
                Some(Setup::Key("AIzaNew".to_owned())),
                "{name}"
            );
            let footer = Seen::of(&page.footer);
            assert_eq!(footer.labels() == ["Finish"], finish, "{name}");
        }
        // Asking is said in a bar as tall as the answer that follows it.
        let seen = Seen::of(&content(&scene("google-gemini-checking")).body);
        assert_eq!(seen.busy, ["Checking with Google"]);
        assert!(seen.bars.is_empty());
        let seen = Seen::of(&content(&scene("google-gemini-entered")).body);
        assert_eq!(
            seen.bars,
            [(Severity::Success, "Google accepts this key.".to_owned())]
        );
    }

    #[test]
    fn a_stored_key_is_one_line_that_says_so_and_can_be_replaced_or_removed() {
        let page = content(&scene("google-gemini-works"));
        assert_eq!(page.title, "Your Gemini API key");
        let seen = Seen::of(&page.body);
        // No field that looks empty over the words that a key is accepted.
        assert!(seen.fields.is_empty());
        let rows = super::super::common::probe::settings(&page.body);
        let [line] = rows.as_slice() else {
            unreachable!("one row was expected");
        };
        assert_eq!(
            (line.title.as_str(), line.detail.as_str()),
            ("Gemini API key", "Stored in Windows Credential Manager")
        );
        assert_eq!(
            seen.bars,
            [(Severity::Success, "Google accepts this key.".to_owned())]
        );
        assert_eq!(seen.labels(), ["Replace", "Remove"]);
        assert_eq!(pressed(&seen, "Replace"), Some(Setup::Replace));
        assert_eq!(pressed(&seen, "Remove"), Some(Setup::RemoveKey));
        let remove = seen.presses.iter().find(|press| press.label == "Remove");
        assert_eq!(remove.map(|press| press.kind), Some(ButtonKind::Subtle));
        let footer = Seen::of(&page.footer);
        assert_eq!(footer.labels(), ["Finish"]);
        assert_eq!(pressed(&footer, "Finish"), Some(Setup::Finish));

        // Replacing it brings the field back, empty, with nothing to finish.
        let page = content(&scene("google-gemini-replacing"));
        assert_eq!(page.title, "Paste your key");
        assert!(text_field(&page, KEY).text.is_empty());
        let seen = Seen::of(&page.body);
        assert!(seen.says("The stored key stays until you finish with a new one that works"));
        assert!(seen.bars.is_empty() && !seen.says("Replace"));
        assert!(Seen::of(&page.footer).labels().is_empty());
    }

    #[test]
    fn every_answer_of_google_is_said_with_what_to_do_next() {
        let mut said = Vec::new();
        for (name, part, again) in [
            ("google-gemini-wrong-key", "does not know this key", false),
            (
                "google-gemini-no-access",
                "may not use the Gemini API",
                true,
            ),
            ("google-gemini-billing", "billing", true),
            ("google-gemini-quota", "used up for now", true),
            ("google-gemini-offline", "internet connection", true),
            ("google-gemini-other", "It said: HTTP 503", true),
            ("google-speech-wrong-key", "refused the credentials", true),
            ("google-speech-no-access", "Cloud Speech Client", true),
            ("google-speech-billing", "billing account", true),
            ("google-speech-quota", "used up for now", true),
            ("google-speech-not-found", "project ID", true),
            ("google-speech-no-sign-in", "gcloud auth", true),
            ("google-speech-bad-file", "not a service account key", false),
            ("google-speech-offline", "internet connection", true),
        ] {
            let page = content(&scene(name));
            let seen = Seen::of(&page.body);
            let [(Severity::Critical, text)] = seen.bars.as_slice() else {
                unreachable!("{name}: one bar that says what is wrong was expected");
            };
            assert!(text.contains(part), "{name}: {text}");
            assert_eq!(pressed(&seen, "Check again"), again.then_some(Setup::Check));
            assert!(
                !Seen::of(&page.footer).labels().contains(&"Finish"),
                "{name}"
            );
            said.push(text.clone());
        }
        // The two services are told apart where what to do differs.
        assert_ne!(said[0], said[6]);
        assert_ne!(said[1], said[7]);
        for name in ["google-gemini-checking", "google-speech-checking"] {
            let seen = Seen::of(&content(&scene(name)).body);
            assert_eq!(seen.busy, ["Checking with Google"], "{name}");
        }
        assert_eq!(
            said[6],
            "Google refused the credentials. Sign in again with gcloud, or choose a new key file."
        );
    }

    #[test]
    fn google_cloud_is_one_card_of_project_region_and_credentials() {
        let page = content(&scene("google-speech-works"));
        let project = text_field(&page, PROJECT);
        assert_eq!(project.text, "my-project");
        assert!(!project.secret && project.eager);
        assert_eq!(
            sent(&(project.action)(" other-project ".to_owned())),
            Some(Setup::Project("other-project".to_owned()))
        );
        assert!((project.action)("my-project".to_owned()).is_none());

        let nodes = all(&page.body);
        let list = nodes.iter().find_map(|node| match node {
            Node::Choice {
                items,
                selected,
                action,
                ..
            } => Some((items, *selected, action)),
            _ => None,
        });
        let Some((items, selected, pick)) = list else {
            unreachable!("a list of regions was expected");
        };
        assert_eq!(items[selected], "European Union (eu)");
        assert_eq!(sent(&pick(1)), Some(Setup::Location("us".to_owned())));

        let seen = Seen::of(&page.body);
        assert!(seen.says("The Google sign-in on this PC"));
        assert_eq!(pressed(&seen, "Choose a key file"), Some(Setup::PickKey));
        assert!(!seen.says("instead"));
        assert_eq!(seen.bars.len(), 1);
        assert_eq!(seen.bars[0].0, Severity::Success);
        assert_eq!(
            pressed(&Seen::of(&page.footer), "Finish"),
            Some(Setup::Finish)
        );
    }

    #[test]
    fn a_key_file_is_named_by_its_account_and_can_be_given_up_for_the_sign_in() {
        let seen = Seen::of(&content(&scene("google-speech-key-file")).body);
        assert!(seen.says("The key of vinput@my-project.iam.gserviceaccount.com"));
        assert_eq!(
            pressed(&seen, "Use the Google sign-in on this PC instead"),
            Some(Setup::UseSignIn)
        );
        // Nothing to sign in with yet is said before Google is asked.
        let seen = Seen::of(&content(&scene("google-speech-enter")).body);
        assert!(seen.says("No Google sign-in on this PC yet"));
        assert!(seen.bars.is_empty());
    }

    #[test]
    fn a_region_set_by_hand_stays_on_the_list() {
        let mut view = scene("google-speech-works");
        "europe-north1".clone_into(&mut view.google.location);
        let page = content(&view);
        let found = all(&page.body).into_iter().find_map(|node| match node {
            Node::Choice {
                items, selected, ..
            } => Some(items[*selected].clone()),
            _ => None,
        });
        assert_eq!(found.as_deref(), Some("europe-north1"));
    }

    #[test]
    fn the_last_step_names_the_model_and_where_to_change_it() {
        for (name, model) in [
            ("google-gemini-done", "Google Gemini 3.5 Transcribe"),
            ("google-speech-done", "Google Cloud Speech (Chirp 3)"),
        ] {
            let page = content(&scene(name));
            let seen = Seen::of(&page.body);
            assert!(
                seen.says(&format!("{model} is your speech model")),
                "{name}"
            );
            assert!(seen.says("Hold F1 and speak to try it"));
            assert!(seen.says("open Models and choose Change"));
            let footer = Seen::of(&page.footer);
            let [done] = footer.presses.as_slice() else {
                unreachable!("one button was expected");
            };
            assert!(done.label == "Done" && leaves(&done.action));
        }
    }
}
