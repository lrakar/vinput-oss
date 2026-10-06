//! Canned data for looking at the windows without the engine.
//!
//! A [`scene`] is a [`View`] that puts a window in one particular state: a
//! new user, a download under way, an update on offer. [`Fake`] is a
//! [`Backend`] over a scene that answers commands the way the real one
//! would, as far as the picture goes, so a window can be clicked through.
//! To see a state that has no scene yet, add one here.

use std::sync::Mutex;

use vinput_view::{
    Api, Change, Command, Download, Engine, Event, Finder, FoundFile, Google, GoogleCheck,
    GoogleProblem, GoogleService, GoogleStep, Microphone, Model, ModelState, Models, Notice,
    NoticeKind, PRO_MODEL, PRO_SOON, Page, Pro, ProDoing, ProPlan, ProTier, ProWho, Release,
    RunsOn, Settings, Setup, Status, Update, UpdateCheck, View,
};

use crate::{Backend, Listener};

/// The names [`scene`] knows.
pub const SCENES: [&str; 67] = [
    "settings",
    "first-run",
    "welcome-signing-in",
    "welcome-plans",
    "welcome-intro",
    "welcome-downloading",
    "listening",
    "downloading",
    "add-model",
    "found",
    "notice",
    "update",
    "update-downloading",
    "many-models",
    "long-names",
    "one-language",
    "pro-signed-out",
    "pro-intro-signed-out",
    "pro-signing-in",
    "pro-not-subscribed",
    "pro-intro",
    "pro-opening-soon",
    "pro-not-yet",
    "pro-awaiting-payment",
    "pro-subscribed",
    "pro-subscribed-elsewhere",
    "pro-complimentary",
    "pro-ending",
    "pro-over-allowance",
    "pro-plus",
    "pro-plus-used-up",
    "pro-used-up-no-model",
    "pro-low-no-model",
    "pro-one-tier",
    "pro-payment-problem",
    "pro-payment-failed",
    "pro-expired",
    "pro-offline",
    "pro-offline-known",
    "google-service",
    "google-gemini-get",
    "google-gemini-enter",
    "google-gemini-checking",
    "google-gemini-entered",
    "google-gemini-works",
    "google-gemini-replacing",
    "google-gemini-wrong-key",
    "google-gemini-no-access",
    "google-gemini-billing",
    "google-gemini-quota",
    "google-gemini-offline",
    "google-gemini-other",
    "google-gemini-done",
    "google-speech-get",
    "google-speech-enter",
    "google-speech-checking",
    "google-speech-works",
    "google-speech-key-file",
    "google-speech-wrong-key",
    "google-speech-no-access",
    "google-speech-billing",
    "google-speech-quota",
    "google-speech-not-found",
    "google-speech-no-sign-in",
    "google-speech-bad-file",
    "google-speech-offline",
    "google-speech-done",
];

const MEGABYTE: u64 = 1 << 20;

fn model(id: &str, name: &str, summary: &str, detail: &str, megabytes: u64) -> Model {
    Model {
        id: id.to_owned(),
        name: name.to_owned(),
        summary: summary.to_owned(),
        detail: detail.to_owned(),
        local: megabytes > 0,
        bytes: megabytes * MEGABYTE,
        custom: false,
        google: None,
        in_use: false,
        state: if megabytes > 0 {
            ModelState::Missing
        } else {
            ModelState::Online
        },
    }
}

fn catalog() -> Vec<Model> {
    let mut list = vec![
        model(
            PRO_MODEL,
            "VinPUT Pro",
            "Online. Types as you speak, in 60+ languages.",
            "Needs a subscription",
            0,
        ),
        model(
            "chirp_3",
            "Google Cloud Speech (Chirp 3)",
            "Online. Types as you speak.",
            "Needs a Google Cloud project",
            0,
        ),
        model(
            "gemini-3.5-transcribe",
            "Google Gemini 3.5 Transcribe",
            "Online. Types as you speak.",
            "Needs a Gemini API key",
            0,
        ),
        model(
            "parakeet-tdt-v3",
            "NVIDIA Parakeet TDT v3",
            "On this PC. Fast and accurate for European languages.",
            "25 European languages",
            669,
        ),
        model(
            "ggml-large-v3-turbo",
            "Whisper large-v3 turbo",
            "On this PC. The most accurate; best with a graphics card.",
            "99 languages",
            1638,
        ),
        model(
            "ggml-large-v3-turbo-q5_0",
            "Whisper large-v3 turbo (compact)",
            "On this PC. Nearly as accurate at a third of the size.",
            "99 languages",
            574,
        ),
        model(
            "ggml-base",
            "Whisper base",
            "On this PC. Small and fast on any processor, lower accuracy.",
            "99 languages",
            148,
        ),
    ];
    list[1].google = Some(GoogleService::Speech);
    list[2].google = Some(GoogleService::Gemini);
    list
}

/// A user who has been using Vinput for a while: the starting point of
/// every scene but the first run.
fn settled() -> View {
    let mut list = catalog();
    list[4].state = ModelState::Installed;
    list[6].state = ModelState::Installed;
    list[6].in_use = true;
    View {
        version: "0.3.0".to_owned(),
        status: Status {
            listening: false,
            model: Some("Whisper base".to_owned()),
            runs_on: RunsOn::GraphicsCard("NVIDIA GeForce RTX 3090".to_owned()),
        },
        settings: Settings {
            hotkey: "F1".to_owned(),
            hotkey_enter: "F2".to_owned(),
            hotkey_search: String::new(),
            hotkey_memo: "Ctrl+Shift+M".to_owned(),
            microphone: String::new(),
            languages: "en-US, sl-SI".to_owned(),
            autostart: true,
            ready_sound: true,
            stop_sound: true,
            check_updates: true,
            graphics_card: true,
            api_enabled: true,
            memo_dir: String::new(),
            memo_to_clipboard: true,
            search_url: "https://www.google.com/search?q=%s".to_owned(),
        },
        microphones: vec![
            Microphone {
                id: "{0.0.1.00000000}.{a}".to_owned(),
                name: "Microphone (USB Audio Device)".to_owned(),
            },
            Microphone {
                id: "{0.0.1.00000000}.{b}".to_owned(),
                name: "Headset (WH-1000XM5)".to_owned(),
            },
        ],
        models: Models {
            list,
            disk_bytes: 1_990 * MEGABYTE,
            graphics_card: Some("NVIDIA GeForce RTX 3090".to_owned()),
            engine: Engine::Ready,
        },
        finder: Finder::default(),
        google: Google {
            project_id: "my-project".to_owned(),
            location: "eu".to_owned(),
            sign_in_found: true,
            speech_ready: true,
            ..Google::default()
        },
        api: Api {
            pipe: r"\\.\pipe\vinput".to_owned(),
            clients: vec!["Control Deck".to_owned()],
        },
        gemini_key_stored: true,
        default_memo_dir: r"C:\Users\you\Documents\Vinput memos".to_owned(),
        update: Update::default(),
        pro: Pro {
            spare: Some(SPARE.to_owned()),
            ..Pro::default()
        },
        notice: None,
    }
}

/// The model on this PC that the scenes carry on with when the hours of
/// `VinPUT` Pro are used up: the one that was in use before it.
const SPARE: &str = "Whisper base";
/// What a month of Pro, and of Plus, comes with in the scenes.
const PRO_SECONDS: u64 = 25 * 3600;
const PLUS_SECONDS: u64 = 8 * 3600;

/// When the subscription of the scenes renews or ends: 5 November 2026.
const PERIOD_END: u64 = 1_793_837_814;
/// When the month of the scenes is over: 1 November 2026.
const RESETS_AT: u64 = 1_793_491_200;
/// Until when a failed renewal is put up with in the scenes: 8 October 2026.
const GRACE_UNTIL: u64 = 1_791_460_800;
/// The code of the sign-in of the scenes.
const CODE: &str = "K7M2QX";
/// How long the Gemini API key of the scenes is.
const KEY_LENGTH: u8 = 39;

/// Someone signed in to `VinPUT` Pro whose account the server has described:
/// with a subscription in the state `status`, or with none if that is empty.
fn signed_in(entitled: bool, status: &str, plan: ProPlan) -> Pro {
    let subscribed = !status.is_empty();
    Pro {
        who: ProWho::SignedIn,
        email: "ana@example.com".to_owned(),
        known: true,
        entitled,
        status: status.to_owned(),
        plan: subscribed.then_some(plan),
        period_end: if subscribed { PERIOD_END } else { 0 },
        used_seconds: 45_000,
        cap_seconds: PRO_SECONDS,
        resets_at: RESETS_AT,
        spare: Some(SPARE.to_owned()),
        ..Pro::default()
    }
}

/// A subscriber to Plus who has used this much of the month.
fn on_plus(used_seconds: u64) -> Pro {
    Pro {
        tier: ProTier::Plus,
        used_seconds,
        cap_seconds: PLUS_SECONDS,
        ..signed_in(true, "active", ProPlan::Monthly)
    }
}

/// Gives the tiers the price of a first month, as the server does for
/// whoever is signed out and for an account it says is new.
fn with_intro(pro: &mut Pro) {
    for (offer, first) in pro.offers.iter_mut().zip([149, 349]) {
        offer.first = first;
    }
}

/// Takes every model off this PC, so that there is none to carry on with.
fn without_models(view: &mut View) {
    for model in &mut view.models.list {
        if model.local {
            model.state = ModelState::Missing;
        }
    }
    view.models.disk_bytes = 0;
    view.pro.spare = None;
}

/// A sign-in that waits to be finished in the browser.
fn signing_in() -> Pro {
    Pro {
        who: ProWho::SigningIn,
        code: CODE.to_owned(),
        code_minutes: 10,
        ..Pro::default()
    }
}

/// Makes `VinPUT` Pro the model dictation uses.
fn dictate_with_pro(view: &mut View) {
    for model in &mut view.models.list {
        model.in_use = model.id == PRO_MODEL;
    }
    view.status.model = Some("VinPUT Pro".to_owned());
    view.status.runs_on = RunsOn::Online;
}

/// The scenes of what the server says about subscribing, beside the prices:
/// a price for the first month, and that no plan can be subscribed to yet.
/// Returns whether `name` is one of them.
fn offer_scene(view: &mut View, name: &str) -> bool {
    match name {
        // For whoever is signed out, and for an account it says is new.
        "pro-intro-signed-out" | "pro-intro" => {
            if name == "pro-intro" {
                view.pro = signed_in(false, "", ProPlan::Monthly);
            }
            with_intro(&mut view.pro);
        }
        // No plan can be subscribed to yet: as the server says beforehand,
        // and as it answers a press it had not said so to.
        "pro-opening-soon" | "pro-not-yet" => {
            view.pro = signed_in(false, "", ProPlan::Monthly);
            if name == "pro-not-yet" {
                view.pro.problem = Some(PRO_SOON.to_owned());
            } else {
                view.pro.open = false;
            }
        }
        _ => return false,
    }
    true
}

/// The scenes of the `VinPUT` Pro page, one for every state of the account.
/// Returns whether `name` is one of them.
fn pro_scene(view: &mut View, name: &str) -> bool {
    let subscribed = || signed_in(true, "active", ProPlan::Monthly);
    let unreachable =
        || Some("VinPUT Pro could not be reached. Check your internet connection.".to_owned());
    match name {
        "pro-signed-out" => {}
        "pro-signing-in" => view.pro = signing_in(),
        "pro-not-subscribed" => view.pro = signed_in(false, "", ProPlan::Monthly),
        "pro-awaiting-payment" => {
            view.pro = signed_in(false, "", ProPlan::Monthly);
            view.pro.doing = ProDoing::AwaitingPayment;
        }
        "pro-subscribed" => {
            view.pro = subscribed();
            dictate_with_pro(view);
        }
        // A subscriber who dictates with a model on this PC all the same.
        "pro-subscribed-elsewhere" => view.pro = subscribed(),
        // Someone the server gives Pro to: no subscription, nothing to pay.
        "pro-complimentary" => {
            view.pro = signed_in(true, "", ProPlan::Monthly);
            view.pro.complimentary = true;
            dictate_with_pro(view);
        }
        "pro-ending" => {
            view.pro = signed_in(true, "active", ProPlan::Yearly);
            view.pro.ends = true;
            view.pro.ends_at = PERIOD_END;
            dictate_with_pro(view);
        }
        // The hours of the month are used up, and the model that was in
        // use before carries on.
        "pro-over-allowance" => {
            view.pro = subscribed();
            view.pro.used_seconds = view.pro.cap_seconds;
            dictate_with_pro(view);
            view.status.runs_on = RunsOn::Spare(SPARE.to_owned());
        }
        "pro-plus" => {
            view.pro = on_plus(11_400);
            dictate_with_pro(view);
        }
        "pro-plus-used-up" => {
            view.pro = on_plus(PLUS_SECONDS);
            dictate_with_pro(view);
            view.status.runs_on = RunsOn::Spare(SPARE.to_owned());
        }
        // Used up, with no model on this PC: nothing can be dictated.
        "pro-used-up-no-model" => {
            view.pro = subscribed();
            view.pro.used_seconds = view.pro.cap_seconds;
            dictate_with_pro(view);
            without_models(view);
        }
        // Nearly used up, with no model on this PC to carry on with.
        "pro-low-no-model" => {
            view.pro = on_plus(PLUS_SECONDS - 1500);
            dictate_with_pro(view);
            without_models(view);
        }
        // A server from before there were tiers: one, with forty hours.
        "pro-one-tier" => {
            view.pro = signed_in(false, "", ProPlan::Monthly);
            view.pro.offers.remove(0);
            view.pro.offers[0].seconds = 144_000;
        }
        "pro-payment-problem" => {
            view.pro = signed_in(true, "past_due", ProPlan::Monthly);
            view.pro.grace_until = GRACE_UNTIL;
            dictate_with_pro(view);
        }
        "pro-payment-failed" => {
            view.pro = signed_in(false, "incomplete", ProPlan::Monthly);
        }
        // Someone whose subscription ran out while they dictated with it:
        // the model is still the chosen one, and no longer works.
        "pro-expired" => {
            view.pro = signed_in(false, "canceled", ProPlan::Monthly);
            view.pro.ends = true;
            view.pro.ends_at = PERIOD_END;
            dictate_with_pro(view);
        }
        // Started without a connection: nothing is known about the account.
        "pro-offline" => {
            view.pro = Pro {
                who: ProWho::SignedIn,
                email: "ana@example.com".to_owned(),
                problem: unreachable(),
                ..Pro::default()
            };
            dictate_with_pro(view);
        }
        // The connection went while the window was open: what was known
        // seven minutes ago is still there to show.
        "pro-offline-known" => {
            view.pro = subscribed();
            view.pro.problem = unreachable();
            view.pro.checked_minutes = Some(7);
            dictate_with_pro(view);
        }
        _ => return false,
    }
    true
}

/// The service account of the scenes.
const ACCOUNT: &str = "vinput@my-project.iam.gserviceaccount.com";

/// Someone who has nothing of Google's yet: no key, no project, no sign-in.
fn without_google(view: &mut View) {
    view.gemini_key_stored = false;
    view.google = Google {
        location: "eu".to_owned(),
        ..Google::default()
    };
}

/// The scenes of the setup of a Google service: its first step, and for
/// either service every later step and every answer Google can give, as
/// `google-<service>-<state>`. Returns whether `name` is one of them.
fn google_scene(view: &mut View, name: &str) -> bool {
    let Some(rest) = name.strip_prefix("google-") else {
        return false;
    };
    let (service, state) = match rest.split_once('-') {
        Some(("gemini", state)) => (GoogleService::Gemini, state),
        Some(("speech", state)) => (GoogleService::Speech, state),
        _ => {
            without_google(view);
            return rest == "service";
        }
    };
    let failed = |problem| GoogleCheck::Failed {
        problem,
        said: String::new(),
    };
    let mut step = GoogleStep::Enter;
    // Someone who is setting it up for the first time, except where the
    // scene shows what is set up already.
    let gemini = service == GoogleService::Gemini;
    let set_up = matches!(state, "works" | "key-file" | "done" | "replacing")
        || (!gemini && !matches!(state, "get" | "enter"));
    if !set_up {
        without_google(view);
    }
    // For Gemini every answer but these is about a key that was just
    // entered and is not kept yet.
    if gemini && !matches!(state, "get" | "enter" | "works" | "done" | "replacing") {
        view.google.key_length = KEY_LENGTH;
    }
    let check = match state {
        "get" => {
            step = GoogleStep::Get;
            GoogleCheck::Idle
        }
        "enter" => GoogleCheck::Idle,
        "checking" => GoogleCheck::Checking,
        "works" | "entered" => GoogleCheck::Works,
        "replacing" => {
            view.google.replacing = true;
            GoogleCheck::Idle
        }
        "key-file" => {
            ACCOUNT.clone_into(&mut view.google.account);
            GoogleCheck::Works
        }
        "done" => {
            step = GoogleStep::Done;
            finish(view, service);
            GoogleCheck::Idle
        }
        "wrong-key" => failed(GoogleProblem::WrongKey),
        "no-access" => failed(GoogleProblem::NoAccess),
        "billing" => failed(GoogleProblem::Billing),
        "quota" => failed(GoogleProblem::Quota),
        "not-found" => failed(GoogleProblem::NotFound),
        "no-sign-in" => {
            view.google.sign_in_found = false;
            failed(GoogleProblem::NoSignIn)
        }
        "bad-file" => failed(GoogleProblem::BadFile),
        "offline" => failed(GoogleProblem::Offline),
        "other" => GoogleCheck::Failed {
            problem: GoogleProblem::Other,
            said: "HTTP 503".to_owned(),
        },
        _ => return false,
    };
    view.google.service = service;
    view.google.step = step;
    view.google.check = check;
    true
}

/// Dictates with the model of `service`, as finishing its setup does.
fn finish(view: &mut View, service: GoogleService) {
    for model in &mut view.models.list {
        model.in_use = model.google == Some(service);
        if model.in_use {
            view.status.model = Some(model.name.clone());
            view.google.model.clone_from(&model.name);
        }
    }
    view.status.runs_on = RunsOn::Online;
    match service {
        GoogleService::Gemini => view.gemini_key_stored = true,
        GoogleService::Speech => view.google.speech_ready = true,
    }
    view.google.service = service;
    view.google.step = GoogleStep::Done;
    view.google.check = GoogleCheck::Idle;
    view.google.key_length = 0;
    view.google.replacing = false;
}

/// Changes the setup of a Google service the way `what` would, as far as
/// the picture goes. The fake's Google takes any key but one with the word
/// `wrong` in it.
fn google(view: &mut View, what: Setup) {
    let works = GoogleCheck::Works;
    match what {
        Setup::Open(service) => {
            let ready = match service {
                GoogleService::Gemini => view.gemini_key_stored,
                GoogleService::Speech => view.google.speech_ready,
            };
            view.google.service = service;
            (view.google.step, view.google.check) = if ready {
                (GoogleStep::Enter, works)
            } else {
                (GoogleStep::Get, GoogleCheck::Idle)
            };
        }
        Setup::Step(step) => {
            view.google.step = step;
            view.google.check = GoogleCheck::Idle;
            view.google.key_length = 0;
            view.google.replacing = false;
        }
        Setup::Key(key) => {
            view.google.key_length = u8::try_from(key.chars().count()).unwrap_or(u8::MAX);
            view.google.check = if key.contains("wrong") {
                GoogleCheck::Failed {
                    problem: GoogleProblem::WrongKey,
                    said: String::new(),
                }
            } else {
                works
            };
        }
        Setup::Replace => {
            view.google.replacing = true;
            view.google.key_length = 0;
            view.google.check = GoogleCheck::Idle;
        }
        Setup::Finish => finish(view, view.google.service),
        Setup::Project(project) => {
            view.google.project_id = project;
            view.google.check = works;
        }
        Setup::Location(location) => {
            view.google.location = location;
            view.google.check = works;
        }
        Setup::PickKey => {
            ACCOUNT.clone_into(&mut view.google.account);
            view.google.check = works;
        }
        Setup::UseSignIn => {
            view.google.account.clear();
            view.google.check = works;
        }
        Setup::Check => view.google.check = works,
        Setup::RemoveKey => {
            view.gemini_key_stored = false;
            view.google.replacing = false;
            view.google.check = GoogleCheck::Idle;
        }
        Setup::Close => {
            view.google.step = GoogleStep::Service;
            view.google.check = GoogleCheck::Idle;
        }
    }
}

fn found_files() -> Vec<FoundFile> {
    vec![
        FoundFile {
            path: "ggml-model-q5_0.bin".to_owned(),
            bytes: 547 * MEGABYTE,
            detail: "Whisper large-v3, 99 languages".to_owned(),
            usable: true,
        },
        FoundFile {
            path: "ggml-model.bin".to_owned(),
            bytes: 1_520 * MEGABYTE,
            detail: "Whisper large-v3, 99 languages".to_owned(),
            usable: true,
        },
        FoundFile {
            path: "model.safetensors".to_owned(),
            bytes: 3_090 * MEGABYTE,
            detail: "Not a format VinPUT can run".to_owned(),
            usable: false,
        },
    ]
}

fn release() -> Release {
    Release {
        version: "0.3.1".to_owned(),
        notes: vec![
            "A new look for Settings, in the style of Windows 11".to_owned(),
            "Settings take effect as you change them".to_owned(),
            "Lighter in the background".to_owned(),
        ],
        bytes: 6 * MEGABYTE + 150_000,
    }
}

/// The view that puts a window in the state `name` names, or `None` if no
/// scene has that name; see [`SCENES`].
#[must_use]
pub fn scene(name: &str) -> Option<View> {
    let mut view = settled();
    match name {
        "settings" => {}
        "first-run" => {
            view.status = Status::default();
            view.settings.hotkey_memo = String::new();
            without_google(&mut view);
            "en-US".clone_into(&mut view.settings.languages);
            view.models.list = catalog();
            view.models.disk_bytes = 0;
            view.models.engine = Engine::Missing;
            view.pro.spare = None;
            view.api.clients = Vec::new();
        }
        "listening" => view.status.listening = true,
        // Someone who dictates with VinPUT Pro and has named one language.
        "one-language" => {
            view.pro = signed_in(true, "active", ProPlan::Monthly);
            "en-US".clone_into(&mut view.settings.languages);
            dictate_with_pro(&mut view);
        }
        "downloading" => {
            view.models.list[3].state = ModelState::Installing {
                done: 412 * MEGABYTE,
                total: 669 * MEGABYTE,
            };
            view.models.list[5].state = ModelState::Failed {
                reason: "The download stopped: no connection to huggingface.co".to_owned(),
            };
            view.models.engine = Engine::Fetching {
                done: 9 * MEGABYTE,
                total: 31 * MEGABYTE,
            };
        }
        "add-model" => {
            view.finder.busy = Some("Looking at the repository".to_owned());
        }
        "found" => view.finder.files = found_files(),
        "notice" => {
            view.notice = Some(Notice {
                kind: NoticeKind::Error,
                text: "Ctrl+C is taken by another program. Pick another key.".to_owned(),
                page: Page::General,
            });
            view.finder.error =
                Some("That link does not lead to a Hugging Face repository".to_owned());
        }
        "update" => {
            view.update = Update {
                check: UpdateCheck::Available,
                release: Some(release()),
                download: Download::Idle,
            };
        }
        "update-downloading" => {
            let release = release();
            view.update = Update {
                check: UpdateCheck::Available,
                download: Download::Running {
                    done: release.bytes * 62 / 100,
                    total: release.bytes,
                },
                release: Some(release),
            };
        }
        // Someone who has just installed, part of the way through the
        // welcome: signing in, picking a plan, downloading a model.
        "welcome-signing-in" | "welcome-plans" | "welcome-intro" | "welcome-downloading" => {
            view = scene("first-run")?;
            match name {
                "welcome-signing-in" => view.pro = signing_in(),
                "welcome-plans" => view.pro = signed_in(false, "", ProPlan::Monthly),
                "welcome-intro" => {
                    view.pro = signed_in(false, "", ProPlan::Monthly);
                    with_intro(&mut view.pro);
                }
                _ => {
                    // The host puts the first model someone downloads to
                    // use at once, and fetches the engine it runs on.
                    if let Some(model) = view.models.list.iter_mut().find(|model| model.local) {
                        model.in_use = true;
                        model.state = ModelState::Installing {
                            done: model.bytes * 2 / 5,
                            total: model.bytes,
                        };
                    }
                    view.models.engine = Engine::Fetching {
                        done: 9 * MEGABYTE,
                        total: 31 * MEGABYTE,
                    };
                }
            }
        }
        "many-models" => view.models.list.extend(crowd()),
        "long-names" => lengthen(&mut view),
        name if offer_scene(&mut view, name) => {}
        name if pro_scene(&mut view, name) => {}
        name if google_scene(&mut view, name) => {}
        _ => return None,
    }
    Some(view)
}

/// Models a keen user might have added, in every state a model can be in:
/// enough of them that the page scrolls a long way.
fn crowd() -> Vec<Model> {
    (1..=34_u64)
        .map(|number| {
            let megabytes = 80 + number * 45;
            let mut added = model(
                &format!("custom-{number}"),
                &format!("ggml-model-{number:02}-q5_0.bin"),
                "Added by you.",
                "99 languages",
                megabytes,
            );
            added.custom = true;
            added.state = match number % 4 {
                0 => ModelState::Missing,
                1 => ModelState::Installed,
                2 => ModelState::Installing {
                    done: megabytes * MEGABYTE * number / 40,
                    total: megabytes * MEGABYTE,
                },
                _ => ModelState::Failed {
                    reason: "The download stopped: no connection to huggingface.co".to_owned(),
                },
            };
            added
        })
        .collect()
}

/// Makes every name in `view` that comes from outside as long as such names
/// get, so it shows where a page gives way: wrapping, an ellipsis, never an
/// overlap.
fn lengthen(view: &mut View) {
    const DEVICE: &str = "Microphone Array (Realtek High Definition Audio with Intel Smart Sound \
                          Technology for Digital Microphones, second generation)";
    const FILE: &str = "ggml-whisper-large-v3-turbo-german-swiss-dialects-finetuned-2026-\
                        quantized-q5_0-experimental-build.bin";
    const CARD: &str = "NVIDIA GeForce RTX 5090 Laptop GPU with Max-Q Design (Founders Edition, \
                        24 GB)";
    view.status.model = Some(FILE.to_owned());
    view.status.runs_on = RunsOn::GraphicsCard(CARD.to_owned());
    view.microphones.push(Microphone {
        id: "{0.0.1.00000000}.{c}".to_owned(),
        name: DEVICE.to_owned(),
    });
    view.settings = Settings {
        microphone: "{0.0.1.00000000}.{c}".to_owned(),
        languages: "en-US, en-GB, sl-SI, de-DE, de-CH, fr-FR, it-IT, es-ES, pt-PT, nl-NL, pl-PL"
            .to_owned(),
        hotkey_memo: "Ctrl+Alt+Shift+Win+PrintScreen".to_owned(),
        memo_dir: r"D:\Users\a-rather-long-user-name\OneDrive - A Company With A Long Name\Recordings\Voice memos\2026"
            .to_owned(),
        search_url:
            "https://search.example.com/a/very/long/path/that/goes/on?language=en&region=si&q=%s"
                .to_owned(),
        ..view.settings.clone()
    };
    "a-google-cloud-project-with-quite-a-long-identifier".clone_into(&mut view.google.project_id);
    view.models.graphics_card = Some(CARD.to_owned());
    view.api = Api {
        pipe: r"\\.\pipe\vinput-a-rather-long-user-name-session-1".to_owned(),
        clients: vec![
            "Control Deck".to_owned(),
            "A Program With A Remarkably Long Window Title.exe".to_owned(),
            "another-helper-process-with-a-long-name.exe".to_owned(),
        ],
    };
    let mut long = model(
        "custom-long",
        FILE,
        "Added by you.",
        "German, Swiss German, Austrian German, Italian, French, Romansh and 93 more languages",
        4096,
    );
    long.custom = true;
    long.state = ModelState::Installed;
    let mut failed = long.clone();
    "custom-long-failed".clone_into(&mut failed.id);
    failed.state = ModelState::Failed {
        reason: "The download stopped: the connection to huggingface.co was reset by the other \
                 side after 3 tries, and the part that had arrived did not match its checksum"
            .to_owned(),
    };
    let mut busy = long.clone();
    "custom-long-busy".clone_into(&mut busy.id);
    busy.state = ModelState::Installing {
        done: 1024 * MEGABYTE,
        total: 4096 * MEGABYTE,
    };
    view.models.list.extend([long, failed, busy]);
    view.finder.files = vec![FoundFile {
        path: format!("models/experimental/german/{FILE}"),
        bytes: 4096 * MEGABYTE,
        detail: "Whisper large-v3 turbo, 99 languages, quantized to five bits".to_owned(),
        usable: true,
    }];
    view.notice = Some(Notice {
        kind: NoticeKind::Error,
        text: "Ctrl+Alt+Shift+Win+PrintScreen is taken by another program, or Windows keeps it \
               for itself. Pick another key, or close the program that has it and try again."
            .to_owned(),
        page: Page::General,
    });
    view.update = Update {
        check: UpdateCheck::Available,
        release: Some(Release {
            version: "0.3.1".to_owned(),
            notes: vec![
                "A new look for Settings, in the style of Windows 11, with every page drawn by \
                 VinPUT itself and no web view behind it"
                    .to_owned(),
                "Settings take effect as you change them".to_owned(),
                "Lighter in the background".to_owned(),
                "Local models on graphics cards of any make".to_owned(),
                "A key that opens what you say as a web search".to_owned(),
                "Fewer surprises when the microphone is unplugged while you speak".to_owned(),
                "Voice memos can go to a folder of your choice".to_owned(),
                "The update window shows how far a download is".to_owned(),
            ],
            bytes: 6 * MEGABYTE,
        }),
        download: Download::Failed {
            reason: "The download stopped: no connection to github.com".to_owned(),
        },
    };
}

/// A backend with no engine behind it: it keeps a [`View`], changes it the
/// way the real backend would change the picture, and sends it back.
pub struct Fake {
    view: Mutex<View>,
    listener: Listener,
}

impl std::fmt::Debug for Fake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fake").finish_non_exhaustive()
    }
}

impl Fake {
    /// A fake that starts from `view` and reports to `listener`.
    #[must_use]
    pub fn new(view: View, listener: Listener) -> Self {
        Self {
            view: Mutex::new(view),
            listener,
        }
    }
}

fn set(settings: &mut Settings, change: Change) {
    match change {
        Change::Hotkey(key) => settings.hotkey = key,
        Change::HotkeyEnter(key) => settings.hotkey_enter = key,
        Change::HotkeySearch(key) => settings.hotkey_search = key,
        Change::HotkeyMemo(key) => settings.hotkey_memo = key,
        Change::Microphone(id) => settings.microphone = id,
        Change::Languages(languages) => settings.languages = languages,
        Change::Autostart(on) => settings.autostart = on,
        Change::ReadySound(on) => settings.ready_sound = on,
        Change::StopSound(on) => settings.stop_sound = on,
        Change::CheckUpdates(on) => settings.check_updates = on,
        Change::GraphicsCard(on) => settings.graphics_card = on,
        Change::ApiEnabled(on) => settings.api_enabled = on,
        Change::MemoDir(dir) => settings.memo_dir = dir,
        Change::MemoToClipboard(on) => settings.memo_to_clipboard = on,
        Change::SearchUrl(url) => settings.search_url = url,
    }
}

fn find<'a>(view: &'a mut View, id: &str) -> Option<&'a mut Model> {
    view.models.list.iter_mut().find(|model| model.id == id)
}

/// Changes `view` the way the real backend would after `command`. Returns
/// whether the window should close.
fn apply(view: &mut View, command: Command) -> bool {
    match command {
        Command::Set(change) => set(&mut view.settings, change),
        Command::Google(what) => google(view, what),
        Command::UseModel(id) => {
            for model in &mut view.models.list {
                model.in_use = model.id == id;
                if model.in_use {
                    view.status.model = Some(model.name.clone());
                    view.status.runs_on = if model.local {
                        RunsOn::Processor
                    } else {
                        RunsOn::Online
                    };
                }
            }
        }
        Command::InstallModel(id) => {
            if let Some(model) = find(view, &id) {
                model.state = ModelState::Installing {
                    done: model.bytes / 3,
                    total: model.bytes,
                };
            }
        }
        Command::CancelInstall(id) => {
            if let Some(model) = find(view, &id) {
                model.state = ModelState::Missing;
            }
        }
        Command::RemoveModel(id) => {
            if find(view, &id).is_some_and(|model| model.custom) {
                view.models.list.retain(|model| model.id != id);
            } else if let Some(model) = find(view, &id) {
                model.state = ModelState::Missing;
            }
        }
        Command::FindModel(_) | Command::PickModelFile => {
            view.finder = Finder {
                files: found_files(),
                ..Finder::default()
            };
        }
        Command::AddModel(index) => {
            if let Some(file) = view.finder.files.get(index).filter(|file| file.usable) {
                let id = format!("custom-{index}");
                let mut added = model(&id, &file.path, "Added by you.", "99 languages", 1);
                added.bytes = file.bytes;
                added.custom = true;
                added.state = ModelState::Installing {
                    done: 0,
                    total: file.bytes,
                };
                view.models.list.push(added);
                view.finder = Finder {
                    added: Some(id),
                    ..Finder::default()
                };
            }
        }
        Command::ClearFinder => view.finder = Finder::default(),
        Command::BrowseMemoDir => {
            r"D:\Recordings".clone_into(&mut view.settings.memo_dir);
        }
        Command::CheckForUpdates => {
            view.update.check = UpdateCheck::Available;
            view.update.release = Some(release());
        }
        Command::InstallUpdate => {
            let total = view.update.release.as_ref().map_or(0, |found| found.bytes);
            view.update.download = Download::Running {
                done: total / 2,
                total,
            };
        }
        Command::CancelUpdate => view.update.download = Download::Idle,
        Command::DismissNotice => view.notice = None,
        Command::ProSignIn => view.pro = signing_in(),
        // What the real backend learns from the server, the fake is told
        // by asking again: a sign-in is finished, a payment has arrived.
        Command::ProRefresh
        | Command::ProCancel
        | Command::ProSubscribe(..)
        | Command::ProSignOut => {
            pro(view, &command);
        }
        Command::Closed => return true,
        Command::Ready
        | Command::CapturingHotkey(_)
        | Command::OpenMemoDir
        | Command::OpenModelsDir
        | Command::ShowUpdate
        | Command::ProManage
        | Command::ProShown
        | Command::WelcomeDone
        | Command::OpenLink(_) => {}
    }
    false
}

/// Changes the account of `VinPUT` Pro the way `command` would, as far as
/// the picture goes.
fn pro(view: &mut View, command: &Command) {
    let waiting = view.pro.doing == ProDoing::AwaitingPayment;
    match (command, view.pro.who) {
        (Command::ProSignOut, _) | (Command::ProCancel, ProWho::SigningIn) => {
            view.pro = Pro::default();
        }
        (Command::ProCancel, _) => view.pro.doing = ProDoing::Nothing,
        (Command::ProSubscribe(..), ProWho::SignedIn) => {
            view.pro.doing = ProDoing::AwaitingPayment;
        }
        (Command::ProRefresh, ProWho::SigningIn) => {
            view.pro = signed_in(false, "", ProPlan::Monthly);
        }
        (Command::ProRefresh, ProWho::SignedIn) if waiting => {
            view.pro = signed_in(true, "active", ProPlan::Monthly);
            dictate_with_pro(view);
        }
        (Command::ProRefresh, ProWho::SignedIn) => {
            view.pro.problem = None;
            view.pro.checked_minutes = None;
        }
        _ => {}
    }
}

impl Backend for Fake {
    fn send(&self, command: Command) {
        let Ok(mut view) = self.view.lock() else {
            return;
        };
        if apply(&mut view, command) {
            (self.listener)(Event::Close);
        } else {
            (self.listener)(Event::State(Box::new(view.clone())));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    #[test]
    fn every_named_scene_exists() {
        for name in SCENES {
            assert!(scene(name).is_some(), "no scene called {name}");
        }
        assert!(scene("no such scene").is_none());
    }

    #[test]
    fn the_fake_answers_commands_with_a_changed_view() {
        let (seen, events) = mpsc::channel();
        let fake = Fake::new(
            scene("settings").unwrap_or_default(),
            Box::new(move |event| {
                let _ = seen.send(event);
            }),
        );
        fake.send(Command::Set(Change::Autostart(false)));
        fake.send(Command::UseModel("ggml-large-v3-turbo".to_owned()));
        fake.send(Command::Closed);
        let views: Vec<_> = events.try_iter().collect();
        let [Event::State(first), Event::State(second), Event::Close] = views.as_slice() else {
            unreachable!("two views and a close were expected, not {views:?}");
        };
        assert!(!first.settings.autostart);
        assert_eq!(
            second.status.model.as_deref(),
            Some("Whisper large-v3 turbo")
        );
        assert_eq!(
            second
                .models
                .list
                .iter()
                .filter(|model| model.in_use)
                .count(),
            1
        );
    }
}
