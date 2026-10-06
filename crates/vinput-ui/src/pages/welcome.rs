//! The welcome: what someone who has just installed `VinPUT` sees first.
//!
//! Nothing can be dictated before there is something to dictate with, so
//! the welcome is about that one choice and nothing else: `VinPUT` Pro,
//! which works at once on any PC, or a model that runs on this one. Each
//! choice is a page of its own with the way back to the first; none of them
//! has the navigation pane beside it. The page of the account and the rows
//! of the models are the ones Settings shows, so what is offered here is
//! what is offered there.
//!
//! Whichever page of the welcome is open, it says that `VinPUT` is ready
//! as soon as it is: with the key to hold, and a field to try it in.

use vinput_view::{
    Command, Engine, GoogleService, Model, ModelState, PRO_MODEL, Page, Pro, Setup, View,
};

use super::common::{least_a_month, pro_trouble};
use super::{models, pro};
use crate::node::{
    Action, Content, Icon, Node, TextStyle, Tone, accent_button, badge, bullets, button, card,
    field, header, human_size, link, note, row, setting, styled,
};

/// Why `VinPUT` Pro is the way to start, a point each.
const PRO_POINTS: [&str; 3] = [
    "Text appears while you speak",
    "More than 60 languages, also mixed in one sentence",
    "Nothing to download, and as fast on any PC",
];

/// What a model on this PC is like, a point each.
const LOCAL_POINTS: [&str; 2] = [
    "Your voice stays on this PC, and it works offline",
    "Text appears when you stop speaking",
];

/// What `page` of the welcome shows for `view`.
#[must_use]
pub fn content(page: Page, view: &View) -> Content {
    if let Some(model) = ready(view) {
        return all_set(view, model);
    }
    match page {
        Page::WelcomePro => account(view),
        Page::WelcomeLocal => on_this_pc(view),
        _ => choose(view),
    }
}

/// The model dictation uses, once it can be dictated with: a model on this
/// PC that is downloaded, `VinPUT` Pro with an account in order, or another
/// online model.
fn ready(view: &View) -> Option<&Model> {
    let model = view.models.list.iter().find(|model| model.in_use)?;
    let usable = match model.state {
        ModelState::Online if model.id == PRO_MODEL => pro_trouble(&view.pro).is_none(),
        ModelState::Online | ModelState::Installed => true,
        ModelState::Missing | ModelState::Installing { .. } | ModelState::Failed { .. } => false,
    };
    usable.then_some(model)
}

/// Text that explains, in the quieter color.
fn quiet(words: impl Into<String>) -> Node {
    styled(words, TextStyle::Body, Tone::Secondary)
}

/// The cheapest way to have `VinPUT` Pro: `From €2.50 a month`.
fn from_price(pro: &Pro) -> String {
    format!("From {}", least_a_month(pro))
}

/// How large the models that run on this PC are, smallest to largest.
fn download_sizes(list: &[Model]) -> Option<String> {
    let sizes = || list.iter().filter(|model| model.local && !model.custom);
    let smallest = sizes().map(|model| model.bytes).min()?;
    let largest = sizes().map(|model| model.bytes).max()?;
    Some(if smallest == largest {
        format!("A download of {}", human_size(smallest))
    } else {
        format!(
            "A download of {} to {}",
            human_size(smallest),
            human_size(largest)
        )
    })
}

/// The first page: what `VinPUT` does, and the two ways to dictate.
fn choose(view: &View) -> Content {
    let key = &view.settings.hotkey;
    let subscription = setting(Icon::Star, "VinPUT Pro")
        .detail(format!("Fast and reliable. {}", from_price(&view.pro)))
        .control(row([
            badge("Recommended", Tone::Accent),
            accent_button("Get started", Action::Go(Page::WelcomePro)).into(),
        ]))
        .below(bullets(PRO_POINTS));
    let mut free = vec!["Free".to_owned()];
    free.extend(download_sizes(&view.models.list));
    let mut points: Vec<String> = LOCAL_POINTS
        .iter()
        .map(|point| (*point).to_owned())
        .collect();
    points.push(match &view.models.graphics_card {
        Some(card) => format!("Runs on your {card}"),
        None => "Slower on this PC, which has no graphics card it can use".to_owned(),
    });
    let local = setting(Icon::Model, "A model on this PC")
        .detail(free.join(". "))
        .control(button("Choose a model", Action::Go(Page::WelcomeLocal)).into())
        .below(bullets(points));
    // Whoever takes one of the ways out has said what they want: the
    // welcome is over for them.
    let over = || Action::send(Command::WelcomeDone);
    let own_key = over()
        .then(Action::send(Command::Google(Setup::Open(
            GoogleService::Gemini,
        ))))
        .then(Action::Go(Page::Google));
    Content {
        pinned: Vec::new(),
        title: "Welcome to VinPUT".to_owned(),
        body: vec![
            quiet(format!(
                "Hold {key} and speak. Your words are typed wherever the cursor is, in any \
                 program. First, choose what turns your speech into text."
            )),
            card([subscription.into()]),
            card([local.into()]),
            row([
                link("Use my own Google API key", own_key),
                Node::Spring,
                link("Set up later", over().then(Action::Go(Page::General))),
            ]),
        ],
        footer: Vec::new(),
        back: None,
    }
}

/// `VinPUT` Pro as a step of the welcome: the page of the account, which
/// follows the sign-in and the payment by itself.
fn account(view: &View) -> Content {
    Content {
        back: Some(Action::Go(Page::Welcome)),
        ..pro::content(view)
    }
}

/// The models that run on this PC, each with its download.
fn on_this_pc(view: &View) -> Content {
    let list = &view.models;
    let rows: Vec<Node> = models::engine_download(&list.engine)
        .into_iter()
        .chain(
            list.list
                .iter()
                .filter(|model| model.local)
                .map(models::model_row),
        )
        .collect();
    let speed = match &list.graphics_card {
        Some(card) => format!("They run on your {card}, which is fast with every one of them."),
        None => "This PC has no graphics card they can use, so the smaller ones answer sooner."
            .to_owned(),
    };
    let mut body = vec![quiet(format!(
        "Pick one to download. {speed} You can change it, or add others, in Settings at any time."
    ))];
    body.extend(models::engine_failure(&list.engine));
    body.push(card(rows));
    if matches!(list.engine, Engine::Ready | Engine::Missing)
        && !list.list.iter().any(|model| model.in_use)
    {
        body.push(note(
            "Downloads come from Hugging Face. Nothing you say is sent anywhere.",
        ));
    }
    Content {
        pinned: Vec::new(),
        title: "A model on this PC".to_owned(),
        body,
        footer: Vec::new(),
        back: Some(Action::Go(Page::Welcome)),
    }
}

/// The end of the welcome: what to press, a field to try it in, and where
/// `VinPUT` is from now on.
fn all_set(view: &View, model: &Model) -> Content {
    let keys = &view.settings;
    let mut rows: Vec<Node> = vec![
        setting(Icon::Keyboard, format!("Hold {} and speak", keys.hotkey))
            .detail("Let go when you are done. Or press it once to start and once to stop")
            .into(),
    ];
    if !keys.hotkey_enter.is_empty() {
        rows.push(
            setting(
                Icon::Keyboard,
                format!("{} also presses Enter for you", keys.hotkey_enter),
            )
            .detail("For chats and search boxes: speak, let go, and it is sent")
            .into(),
        );
    }
    let try_it = field("welcome-try", "", |_| Action::None)
        .placeholder(format!(
            "Click here, hold {} and say something",
            keys.hotkey
        ))
        .wide();
    Content {
        pinned: Vec::new(),
        title: "VinPUT is ready".to_owned(),
        body: vec![
            quiet(format!(
                "Dictation uses {}. It works in every program, wherever the cursor is.",
                model.name
            )),
            card(rows),
            header("Try it"),
            try_it.into(),
            header("From now on"),
            quiet(
                "VinPUT keeps running in the notification area, beside the clock. Its icon \
                 there opens Settings, where you change the keys, the microphone and the model.",
            ),
            row([
                accent_button("Done", Action::Close).into(),
                button("Open Settings", Action::Go(Page::General)).into(),
            ]),
        ],
        footer: Vec::new(),
        back: None,
    }
}

#[cfg(test)]
mod tests {
    use vinput_view::{ProPlan, ProTier, ProWho};

    use super::super::common::probe::{Seen, command, scene};
    use super::*;
    use crate::node::ButtonKind;

    fn seen(page: Page, view: &View) -> Seen {
        Seen::of(&content(page, view).body)
    }

    /// What pressing the thing captioned `label` does.
    fn press<'a>(seen: &'a Seen, label: &str) -> &'a Action {
        let found = seen.presses.iter().find(|press| press.label == label);
        let Some(press) = found else {
            unreachable!("nothing to press is called {label:?}");
        };
        &press.action
    }

    /// Every command and every turn of the page in `action`, in order.
    fn steps(action: &Action) -> (Vec<Command>, Option<Page>) {
        match action {
            Action::Send(command) => (vec![command.clone()], None),
            Action::Go(page) => (Vec::new(), Some(*page)),
            Action::Both(first, second) => {
                let (mut commands, page) = steps(first);
                let (more, later) = steps(second);
                commands.extend(more);
                (commands, later.or(page))
            }
            _ => (Vec::new(), None),
        }
    }

    #[test]
    fn the_first_page_offers_the_two_ways_and_prefers_the_subscription() {
        let view = scene("first-run");
        let page = content(Page::Welcome, &view);
        assert_eq!(page.title, "Welcome to VinPUT");
        assert!(page.back.is_none() && page.footer.is_empty());
        let seen = Seen::of(&page.body);
        assert!(seen.says("Hold F1 and speak"));
        assert!(seen.says("Fast and reliable. From \u{20ac}2.50 a month"));
        assert_eq!(seen.badges, ["Recommended"]);
        // One thing stands out, and it is the subscription.
        let accents: Vec<&str> = seen
            .presses
            .iter()
            .filter(|press| press.kind == ButtonKind::Accent)
            .map(|press| press.label.as_str())
            .collect();
        assert_eq!(accents, ["Get started"]);
        assert_eq!(
            steps(press(&seen, "Get started")),
            (Vec::new(), Some(Page::WelcomePro))
        );
        assert_eq!(
            steps(press(&seen, "Choose a model")),
            (Vec::new(), Some(Page::WelcomeLocal))
        );
        assert!(seen.says("Free. A download of 148 MB to 2.3 GB"));
        // Nothing is bought, downloaded or chosen by the first page itself.
        assert!(
            seen.commands()
                .iter()
                .all(|command| matches!(command, Command::WelcomeDone))
        );
    }

    #[test]
    fn the_ways_out_end_the_welcome() {
        let seen = seen(Page::Welcome, &scene("first-run"));
        assert_eq!(
            steps(press(&seen, "Set up later")),
            (vec![Command::WelcomeDone], Some(Page::General))
        );
        assert_eq!(
            steps(press(&seen, "Use my own Google API key")),
            (
                vec![
                    Command::WelcomeDone,
                    Command::Google(Setup::Open(GoogleService::Gemini))
                ],
                Some(Page::Google)
            )
        );
    }

    #[test]
    fn the_first_page_says_how_local_models_will_run_on_this_pc() {
        let mut view = scene("first-run");
        view.models.graphics_card = Some("NVIDIA GeForce RTX 4070".to_owned());
        assert!(seen(Page::Welcome, &view).says("Runs on your NVIDIA GeForce RTX 4070"));
        view.models.graphics_card = None;
        assert!(seen(Page::Welcome, &view).says("Slower on this PC"));
    }

    #[test]
    fn the_subscription_step_is_the_page_of_the_account_with_a_way_back() {
        let mut view = scene("first-run");
        let page = content(Page::WelcomePro, &view);
        assert_eq!(page.title, "VinPUT Pro");
        assert!(matches!(page.back, Some(Action::Go(Page::Welcome))));
        let signed_out = Seen::of(&page.body);
        assert_eq!(
            command(press(&signed_out, "Sign in with Google")),
            Some(Command::ProSignIn)
        );
        // Signed in without a subscription, it is the plans, of both tiers.
        view.pro.who = ProWho::SignedIn;
        view.pro.known = true;
        let plans = seen(Page::WelcomePro, &view);
        for tier in [ProTier::Plus, ProTier::Pro] {
            let monthly = Command::ProSubscribe(tier, ProPlan::Monthly);
            assert!(plans.commands().contains(&monthly), "{tier:?}");
        }
        // With whatever the page of the account says about a first month.
        let intro = seen(Page::WelcomePro, &scene("welcome-intro"));
        assert!(intro.says("first month \u{20ac}1.49") && intro.says("introductory price"));
        assert!(!plans.says("first month"));
    }

    #[test]
    fn the_local_step_lists_the_models_of_this_pc_each_with_its_download() {
        let view = scene("first-run");
        let page = content(Page::WelcomeLocal, &view);
        assert!(matches!(page.back, Some(Action::Go(Page::Welcome))));
        let seen = Seen::of(&page.body);
        let local: Vec<&Model> = view
            .models
            .list
            .iter()
            .filter(|model| model.local)
            .collect();
        assert!(!local.is_empty());
        for model in &local {
            assert!(seen.says(&model.name), "{}", model.name);
            assert!(
                seen.commands()
                    .contains(&Command::InstallModel(model.id.clone()))
            );
        }
        // Nothing that runs online is offered here.
        assert!(!seen.says("VinPUT Pro") && !seen.says("Gemini"));
    }

    #[test]
    fn the_local_step_says_which_models_do_not_know_the_language() {
        let seen = seen(Page::WelcomeLocal, &scene("welcome-other-language"));
        let lacking: Vec<&String> = seen
            .texts
            .iter()
            .filter(|text| text.starts_with("Does not know Slovenian"))
            .collect();
        assert_eq!(lacking.len(), 2, "{:?}", seen.texts);
        // In English there is nothing to say.
        assert!(!self::seen(Page::WelcomeLocal, &scene("first-run")).says("not know"));
    }

    #[test]
    fn a_download_under_way_is_not_ready_yet() {
        let mut view = scene("first-run");
        let Some(model) = view.models.list.iter_mut().find(|model| model.local) else {
            unreachable!("the catalog has local models");
        };
        // The host puts the first model someone downloads to use at once.
        model.in_use = true;
        model.state = ModelState::Installing { done: 1, total: 3 };
        let id = model.id.clone();
        for page in [Page::Welcome, Page::WelcomeLocal] {
            assert_ne!(content(page, &view).title, "VinPUT is ready");
        }
        let waiting = seen(Page::WelcomeLocal, &view);
        assert!(waiting.commands().contains(&Command::CancelInstall(id)));
        assert_eq!(waiting.progress.len(), 1);
    }

    #[test]
    fn every_page_of_the_welcome_says_so_once_dictation_works() {
        // A model on this PC that is downloaded and in use.
        let view = scene("settings");
        for page in [Page::Welcome, Page::WelcomePro, Page::WelcomeLocal] {
            let page = content(page, &view);
            assert_eq!(page.title, "VinPUT is ready");
            assert!(page.back.is_none());
            let seen = Seen::of(&page.body);
            assert!(seen.says("Hold F1 and speak"));
            assert!(seen.says("F2 also presses Enter"));
            assert_eq!(seen.fields.len(), 1);
            assert!(matches!(press(&seen, "Done"), Action::Close));
            assert!(matches!(
                press(&seen, "Open Settings"),
                Action::Go(Page::General)
            ));
            assert!(seen.commands().is_empty());
        }
        // A subscription in order and in use.
        let subscribed = scene("pro-subscribed");
        assert!(seen(Page::WelcomePro, &subscribed).says("Dictation uses VinPUT Pro"));
    }

    #[test]
    fn pro_that_cannot_be_dictated_with_is_not_ready() {
        // Chosen, with a subscription that ran out: the account says why.
        let view = scene("pro-expired");
        let page = content(Page::WelcomePro, &view);
        assert_eq!(page.title, "VinPUT Pro");
        assert!(page.back.is_some());
    }

    #[test]
    fn without_a_second_key_nothing_is_said_of_one() {
        let mut view = scene("settings");
        view.settings.hotkey_enter = String::new();
        "Ctrl+Space".clone_into(&mut view.settings.hotkey);
        let seen = seen(Page::Welcome, &view);
        assert!(seen.says("Hold Ctrl+Space and speak"));
        assert!(!seen.says("presses Enter"));
    }
}
