//! The picture the windows show, drawn from what the background process
//! knows.
//!
//! [`Facts`] is everything that goes into a picture, gathered by the host
//! from the settings, the installer, the disk and Windows. [`build`] turns
//! the facts into a [`wire::View`] and looks at nothing else, so the same facts
//! always give the same picture and every part of it can be tested. A view
//! carries facts, not sentences: words are written here only where the
//! contract says a field holds words.

use std::time::Duration;
use vinput_core::audio::Microphone;

use vinput_core::cloud::{AccountInfo, Plan, Prices, Tier};
use vinput_core::models::{self, EngineState, Model, Status};
use vinput_core::settings::Settings;
use vinput_core::stt::ProviderKind;
use vinput_core::update;
use vinput_core::update::Release;
use vinput_view as wire;

pub(crate) use vinput_view::ProDoing;

use crate::finder::{self, Snapshot};

/// The outcome of the last look for a newer version.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum LastCheck {
    /// Nobody has looked since Vinput started.
    #[default]
    Never,
    /// The running version was the newest.
    UpToDate,
    /// The check failed, and why.
    Failed(String),
}

/// What is known about newer versions.
#[derive(Debug, Clone, Copy)]
pub(crate) struct UpdateFacts<'a> {
    /// Whether a check is under way.
    pub(crate) checking: bool,
    /// How the last check ended, if it found nothing to install.
    pub(crate) last_check: &'a LastCheck,
    /// The newer release that was found.
    pub(crate) release: Option<&'a Release>,
    /// Bytes of it downloaded so far, while its download runs.
    pub(crate) downloaded: Option<u64>,
    /// Whether the download is complete and setup has been started.
    pub(crate) installing: bool,
    /// Why the last download failed.
    pub(crate) failed: Option<&'a str>,
}

/// What is known about the account of `VinPUT` Pro.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ProFacts<'a> {
    /// The address that is signed in.
    pub(crate) email: Option<&'a str>,
    /// The code of the sign-in under way, empty until the server has
    /// handed it out, and how many minutes it works for.
    pub(crate) signing_in: Option<(&'a str, u32)>,
    /// How the account stood when the server last said.
    pub(crate) info: Option<&'a AccountInfo>,
    /// How long ago that was, while the server cannot be asked now.
    pub(crate) checked: Option<Duration>,
    /// What is under way.
    pub(crate) doing: ProDoing,
    /// Why the last request failed, in words for the user.
    pub(crate) problem: Option<&'a str>,
    /// What the plans cost.
    pub(crate) prices: &'a Prices,
    /// Whether the model on this PC serves in place of `VinPUT` Pro right
    /// now, because the month's hours are used up.
    pub(crate) carrying: bool,
}

/// What is known about the setup of a Google service of one's own.
#[derive(Debug, Clone, Copy)]
pub(crate) struct GoogleFacts<'a> {
    /// The step the setup is at.
    pub(crate) step: wire::GoogleStep,
    /// The service being set up.
    pub(crate) service: wire::GoogleService,
    /// What Google made of what is entered.
    pub(crate) check: &'a wire::GoogleCheck,
    /// The Google Cloud project and location, as entered.
    pub(crate) project: &'a str,
    pub(crate) location: &'a str,
    /// How long the Gemini API key is that was entered and is not kept yet.
    pub(crate) key_length: u8,
    /// Whether the key that is kept is being replaced.
    pub(crate) replacing: bool,
    /// The address of the service account whose key is used, if one is.
    pub(crate) account: Option<&'a str>,
    /// Whether this PC has a Google sign-in made with the Google Cloud CLI.
    pub(crate) sign_in: bool,
    /// Whether Google Cloud Speech has a project and a way to sign in.
    pub(crate) ready: bool,
    /// Name of the model the setup has put to use.
    pub(crate) model: &'a str,
}

/// Everything a picture is drawn from.
#[derive(Debug, Clone)]
pub(crate) struct Facts<'a> {
    /// The settings in force.
    pub(crate) settings: &'a Settings,
    /// Whether a dictation is running.
    pub(crate) listening: bool,
    /// Name of the graphics card that can run local models, if there is one.
    pub(crate) graphics_card: Option<&'a str>,
    /// Whether the engine that runs models on a graphics card is installed.
    pub(crate) card_engine: bool,
    /// Every model in the order to list them, with where each stands.
    pub(crate) models: Vec<(&'static Model, Status)>,
    /// Where the engine the settings ask for stands.
    pub(crate) engine: EngineState,
    /// Bytes the downloaded models and engines take on disk.
    pub(crate) disk_bytes: u64,
    /// The audio inputs that are connected.
    pub(crate) microphones: &'a [Microphone],
    /// Names of the programs connected to the local API.
    pub(crate) clients: Vec<String>,
    /// Whether a Gemini API key is in the credential store.
    pub(crate) gemini_key_stored: bool,
    /// The setup of a Google service of one's own.
    pub(crate) google: GoogleFacts<'a>,
    /// Where voice memos go when no folder is set.
    pub(crate) default_memo_dir: String,
    /// What is known about newer versions.
    pub(crate) update: UpdateFacts<'a>,
    /// The account of `VinPUT` Pro.
    pub(crate) pro: ProFacts<'a>,
    /// Where the search for a model to add stands.
    pub(crate) finder: Snapshot,
    /// What there is to tell the user about the last thing they asked for.
    pub(crate) notice: Option<wire::Notice>,
}

/// Whether `model` is the one `settings` choose.
pub(crate) fn is_in_use(settings: &Settings, model: &Model) -> bool {
    model.provider == settings.provider && model.id.eq_ignore_ascii_case(&settings.model)
}

/// Whether a model can be put to use: it is here, or needs nothing here.
pub(crate) fn can_use(status: &Status) -> bool {
    matches!(status, Status::Installed | Status::Online)
}

/// Whether a model can be removed. One that is in use, on its way or online
/// cannot. A model someone added can be taken off the list before it is
/// downloaded and after its download failed as well.
pub(crate) fn can_remove(status: &Status, in_use: bool, custom: bool) -> bool {
    match status {
        Status::Missing { .. } | Status::Failed(_) => custom && !in_use,
        Status::Installing { .. } | Status::Online => false,
        Status::Installed => !in_use,
    }
}

fn settings(settings: &Settings, graphics_card: bool) -> wire::Settings {
    let optional = |hotkey: Option<vinput_core::keys::Hotkey>| {
        hotkey.map(|hotkey| hotkey.to_string()).unwrap_or_default()
    };
    wire::Settings {
        hotkey: settings.hotkey.to_string(),
        hotkey_enter: optional(settings.hotkey_enter),
        hotkey_search: optional(settings.hotkey_search),
        hotkey_memo: optional(settings.hotkey_memo),
        microphone: settings.microphone.clone(),
        languages: settings.languages.join(", "),
        autostart: settings.autostart,
        ready_sound: settings.ready_sound,
        stop_sound: settings.stop_sound,
        stop_after_minutes: settings.stop_after_minutes,
        check_updates: settings.check_updates,
        // What the user wants only counts on a PC that has a card.
        graphics_card: settings.local_gpu && graphics_card,
        api_enabled: settings.api_enabled,
        memo_dir: settings.memo_dir.clone(),
        memo_to_clipboard: settings.memo_to_clipboard,
        search_url: settings.search_url.clone(),
    }
}

/// The model in use and where it runs.
fn status(facts: &Facts<'_>) -> wire::Status {
    let chosen = facts
        .models
        .iter()
        .map(|(model, _)| *model)
        .find(|model| is_in_use(facts.settings, model));
    // A model set by hand in the settings file is not on the list; it is
    // shown under the name it was written with, as an online one.
    let unlisted = Some(&facts.settings.model).filter(|name| !name.is_empty());
    let carrying = facts.pro.carrying && facts.settings.provider == ProviderKind::Cloud;
    let runs_on = match (chosen, facts.graphics_card) {
        (Some(_), _) if carrying && spare(facts).is_some() => {
            wire::RunsOn::Spare(spare(facts).unwrap_or_default().to_owned())
        }
        (None, _) if unlisted.is_none() => wire::RunsOn::Nowhere,
        (None, _) => wire::RunsOn::Online,
        (Some(model), _) if model.local.is_none() => wire::RunsOn::Online,
        // The graphics card is only at work once its engine is here, which
        // for a model that comes as two files is the only engine it has.
        (Some(model), Some(card))
            if facts.settings.local_gpu && (facts.card_engine || model.files().count() > 1) =>
        {
            wire::RunsOn::GraphicsCard(card.to_owned())
        }
        (Some(_), _) => wire::RunsOn::Processor,
    };
    wire::Status {
        listening: facts.listening,
        model: chosen
            .map(|model| model.name.to_owned())
            .or_else(|| unlisted.cloned()),
        runs_on,
    }
}

fn model(settings: &Settings, model: &Model, status: &Status) -> wire::Model {
    let file_bytes = model.bytes();
    wire::Model {
        lacks: model.lacks(&settings.languages),
        knows_none: model.reach(&settings.languages) == 0,
        id: model.id.to_owned(),
        name: model.name.to_owned(),
        summary: model.summary.to_owned(),
        detail: model.detail.to_owned(),
        local: model.local.is_some(),
        bytes: match status {
            // What is still to be downloaded can include the engine.
            Status::Missing { bytes } => *bytes,
            _ => file_bytes,
        },
        custom: model.is_custom(),
        google: match model.provider {
            ProviderKind::Gemini => Some(wire::GoogleService::Gemini),
            ProviderKind::GoogleCloud => Some(wire::GoogleService::Speech),
            _ => None,
        },
        in_use: is_in_use(settings, model),
        state: match status {
            Status::Online => wire::ModelState::Online,
            Status::Missing { .. } => wire::ModelState::Missing,
            Status::Installing { done, total } => wire::ModelState::Installing {
                done: *done,
                total: *total,
            },
            Status::Installed => wire::ModelState::Installed,
            Status::Failed(reason) => wire::ModelState::Failed {
                reason: reason.clone(),
            },
        },
    }
}

fn engine(state: &EngineState) -> wire::Engine {
    match state {
        EngineState::Ready => wire::Engine::Ready,
        EngineState::Missing => wire::Engine::Missing,
        EngineState::Fetching { done, total } => wire::Engine::Fetching {
            done: *done,
            total: *total,
        },
        EngineState::Failed(reason) => wire::Engine::Failed {
            reason: reason.clone(),
        },
    }
}

fn update(facts: &UpdateFacts<'_>) -> wire::Update {
    let check = match (facts.checking, facts.release, facts.last_check) {
        (true, _, _) => wire::UpdateCheck::Checking,
        (false, Some(_), _) => wire::UpdateCheck::Available,
        (false, None, LastCheck::Never) => wire::UpdateCheck::Unchecked,
        (false, None, LastCheck::UpToDate) => wire::UpdateCheck::UpToDate,
        (false, None, LastCheck::Failed(reason)) => wire::UpdateCheck::Failed {
            reason: reason.clone(),
        },
    };
    let download = match (facts.downloaded, facts.release) {
        (_, _) if facts.installing => wire::Download::Installing,
        (Some(done), Some(release)) => wire::Download::Running {
            done,
            total: release.bytes,
        },
        _ => match facts.failed {
            Some(reason) => wire::Download::Failed {
                reason: reason.to_owned(),
            },
            None => wire::Download::Idle,
        },
    };
    wire::Update {
        check,
        release: facts.release.map(|release| wire::Release {
            version: release.version.clone(),
            notes: release.notes.clone(),
            bytes: release.bytes,
        }),
        download,
    }
}

/// Name of the model on this PC that dictation carries on with when the
/// hours of `VinPUT` Pro are used up, if one is downloaded that knows a
/// language of the settings.
fn spare(facts: &Facts<'_>) -> Option<&'static str> {
    let here = facts
        .models
        .iter()
        .filter(|(model, status)| model.local.is_some() && matches!(status, Status::Installed));
    let settings = facts.settings;
    let here = here.map(|(model, _)| *model);
    models::pick(&settings.last_local_model, &settings.languages, here).map(|model| model.name)
}

fn tier(tier: Tier) -> wire::ProTier {
    match tier {
        Tier::Pro => wire::ProTier::Pro,
        Tier::Plus => wire::ProTier::Plus,
    }
}

fn pro(all: &Facts<'_>) -> wire::Pro {
    let facts = &all.pro;
    let offers = facts.prices.offers.iter();
    // The price of a first month is for someone new: whoever is signed out
    // may be, and of an account the server says.
    let new = facts.email.is_none() || facts.info.is_some_and(|info| info.intro_eligible);
    let mut shown = wire::Pro {
        doing: facts.doing,
        problem: facts.problem.map(str::to_owned),
        offers: offers
            .map(|offer| wire::ProOffer {
                tier: tier(offer.tier),
                monthly: offer.monthly,
                first: if new { offer.first } else { 0 },
                yearly: offer.yearly,
                seconds: offer.cap_seconds,
            })
            .collect(),
        currency: facts.prices.currency.clone(),
        open: facts.prices.open,
        spare: spare(all).map(str::to_owned),
        ..wire::Pro::default()
    };
    // A sign-in under way shows its code, whoever was signed in before.
    let email = match (facts.signing_in, facts.email) {
        (Some((code, minutes)), _) => {
            shown.who = wire::ProWho::SigningIn;
            code.clone_into(&mut shown.code);
            shown.code_minutes = minutes;
            return shown;
        }
        (None, None) => return shown,
        (None, Some(email)) => email,
    };
    shown.who = wire::ProWho::SignedIn;
    email.clone_into(&mut shown.email);
    let Some(info) = facts.info else {
        return shown;
    };
    shown.known = true;
    // An account that is given Pro can use it, whatever became of a
    // subscription it once had.
    shown.complimentary = info.at_no_charge();
    shown.entitled = info.entitled || shown.complimentary;
    shown.used_seconds = info.used_seconds;
    shown.cap_seconds = info.cap_seconds;
    shown.tier = tier(info.tier);
    shown.resets_at = info.resets_at;
    shown.checked_minutes = facts
        .checked
        .map(|ago| u32::try_from(ago.as_secs() / 60).unwrap_or(u32::MAX));
    if let Some(subscription) = &info.subscription {
        shown.status.clone_from(&subscription.status);
        shown.plan = subscription.plan.map(|plan| match plan {
            Plan::Monthly => wire::ProPlan::Monthly,
            Plan::Yearly => wire::ProPlan::Yearly,
        });
        shown.period_end = subscription.period_end;
        shown.ends = subscription.ends;
        shown.ends_at = subscription.ends_at;
        shown.ended_at = subscription.ended_at;
        shown.grace_until = subscription.grace_until;
    }
    shown
}

/// Draws the picture the facts describe.
pub(crate) fn build(facts: &Facts<'_>) -> wire::View {
    wire::View {
        version: update::CURRENT_VERSION.to_owned(),
        status: status(facts),
        settings: settings(facts.settings, facts.graphics_card.is_some()),
        microphones: facts
            .microphones
            .iter()
            .map(|microphone| wire::Microphone {
                id: microphone.id.clone(),
                name: microphone.name.clone(),
            })
            .collect(),
        models: wire::Models {
            list: facts
                .models
                .iter()
                .map(|(listed, status)| model(facts.settings, listed, status))
                .collect(),
            disk_bytes: facts.disk_bytes,
            graphics_card: facts.graphics_card.map(str::to_owned),
            engine: engine(&facts.engine),
        },
        finder: wire::Finder {
            busy: facts.finder.busy.map(str::to_owned),
            files: facts
                .finder
                .files
                .iter()
                .map(|(choice, added)| finder::found_file(choice, *added))
                .collect(),
            error: facts.finder.error.clone(),
            added: facts.finder.added.clone(),
        },
        api: wire::Api {
            pipe: vinput_core::instance::pipe_name(),
            clients: facts.clients.clone(),
        },
        gemini_key_stored: facts.gemini_key_stored,
        google: wire::Google {
            step: facts.google.step,
            service: facts.google.service,
            check: facts.google.check.clone(),
            project_id: facts.google.project.to_owned(),
            location: facts.google.location.to_owned(),
            account: facts.google.account.unwrap_or_default().to_owned(),
            sign_in_found: facts.google.sign_in,
            speech_ready: facts.google.ready,
            model: facts.google.model.to_owned(),
            key_length: facts.google.key_length,
            replacing: facts.google.replacing,
        },
        default_memo_dir: facts.default_memo_dir.clone(),
        update: update(&facts.update),
        pro: pro(facts),
        notice: facts.notice.clone(),
    }
}

#[cfg(test)]
mod tests {
    use vinput_core::models::CATALOG;
    use vinput_core::models::custom::{Choice, Origin};

    use super::*;

    const CARD: &str = "NVIDIA GeForce RTX 3090";

    fn online() -> &'static Model {
        CATALOG
            .iter()
            .find(|model| model.local.is_none())
            .unwrap_or(&CATALOG[0])
    }

    fn local() -> &'static Model {
        CATALOG
            .iter()
            .find(|model| model.local.is_some())
            .unwrap_or(&CATALOG[0])
    }

    /// Settings that choose `model`.
    fn choosing(model: &Model) -> Settings {
        Settings {
            provider: model.provider,
            model: model.id.to_owned(),
            ..Settings::default()
        }
    }

    const NEVER: LastCheck = LastCheck::Never;

    /// Nobody signed in, at the built-in prices.
    fn no_pro(prices: &Prices) -> ProFacts<'_> {
        ProFacts {
            email: None,
            signing_in: None,
            info: None,
            checked: None,
            doing: ProDoing::Nothing,
            problem: None,
            prices,
            carrying: false,
        }
    }

    fn built_in() -> &'static Prices {
        static PRICES: std::sync::OnceLock<Prices> = std::sync::OnceLock::new();
        PRICES.get_or_init(Prices::default)
    }

    fn no_update() -> UpdateFacts<'static> {
        UpdateFacts {
            checking: false,
            last_check: &NEVER,
            release: None,
            downloaded: None,
            installing: false,
            failed: None,
        }
    }

    /// A PC with a graphics card and every model of the catalog, none of
    /// the local ones downloaded.
    fn facts(settings: &Settings) -> Facts<'_> {
        Facts {
            settings,
            listening: false,
            graphics_card: Some(CARD),
            card_engine: true,
            models: CATALOG
                .iter()
                .map(|model| {
                    let status = match &model.local {
                        Some(_) => Status::Missing {
                            bytes: model.bytes() + 1000,
                        },
                        None => Status::Online,
                    };
                    (model, status)
                })
                .collect(),
            engine: EngineState::Ready,
            disk_bytes: 0,
            microphones: &[],
            clients: Vec::new(),
            gemini_key_stored: false,
            google: GoogleFacts {
                step: wire::GoogleStep::Service,
                service: wire::GoogleService::Gemini,
                check: &wire::GoogleCheck::Idle,
                project: "",
                location: "",
                key_length: 0,
                replacing: false,
                account: None,
                sign_in: false,
                ready: false,
                model: "",
            },
            default_memo_dir: r"C:\Users\you\Documents\Vinput memos".to_owned(),
            update: no_update(),
            pro: no_pro(built_in()),
            finder: Snapshot::default(),
            notice: None,
        }
    }

    fn release() -> Release {
        Release {
            version: "9.9.9".into(),
            notes: vec!["Faster".into(), "Smaller".into()],
            url: "https://example.com/setup.exe".into(),
            sha256: "0".repeat(64),
            bytes: 6_000_000,
        }
    }

    #[test]
    fn a_new_user_sees_no_model_and_the_settings_they_start_with() {
        let settings = Settings::default();
        let view = build(&facts(&settings));
        assert_eq!(view.version, update::CURRENT_VERSION);
        assert_eq!(view.status, wire::Status::default());
        assert_eq!(view.settings.hotkey, "F1");
        assert_eq!(view.settings.hotkey_enter, "F2");
        assert_eq!(view.settings.hotkey_search, "");
        assert_eq!(view.settings.hotkey_memo, "");
        assert_eq!(view.settings.languages, "en-US");
        assert_eq!(view.google, wire::Google::default());
        assert_eq!(view.settings.memo_dir, "");
        assert!(view.settings.autostart && view.settings.graphics_card);
        assert_eq!(view.models.list.len(), CATALOG.len());
        assert!(view.models.list.iter().all(|model| !model.in_use));
        assert_eq!(view.models.graphics_card.as_deref(), Some(CARD));
        assert_eq!(view.api.pipe, vinput_core::instance::pipe_name());
        assert_eq!(view.update, wire::Update::default());
        assert_eq!(view.finder, wire::Finder::default());
        assert_eq!(view.notice, None);
    }

    #[test]
    fn the_account_of_pro_is_shown_as_far_as_it_is_known() {
        let settings = Settings::default();
        let mut facts = facts(&settings);
        assert_eq!(build(&facts).pro, wire::Pro::default());

        // A sign-in under way shows its code, whoever was signed in before.
        facts.pro.email = Some("before@example.com");
        facts.pro.signing_in = Some(("K7M2QX", 10));
        let shown = build(&facts).pro;
        assert_eq!(shown.who, wire::ProWho::SigningIn);
        assert_eq!((shown.code.as_str(), shown.code_minutes), ("K7M2QX", 10));
        assert_eq!(shown.email, "");

        // Signed in, and the server has not said anything yet.
        facts.pro = ProFacts {
            email: Some("ana@example.com"),
            doing: ProDoing::Asking,
            problem: Some("No connection."),
            ..no_pro(built_in())
        };
        let shown = build(&facts).pro;
        assert_eq!(shown.who, wire::ProWho::SignedIn);
        assert_eq!(shown.doing, ProDoing::Asking);
        assert_eq!(shown.problem.as_deref(), Some("No connection."));
        assert_eq!(shown.email, "ana@example.com");
        assert!(!shown.known && !shown.entitled && shown.status.is_empty());
        assert_eq!(shown.checked_minutes, None);

        let info = AccountInfo {
            email: "ana@example.com".into(),
            entitled: true,
            subscription: Some(vinput_core::cloud::Subscription {
                status: "past_due".into(),
                plan: Some(Plan::Yearly),
                period_end: 1_793_837_814,
                ends: true,
                grace_until: 1_794_097_014,
                ends_at: 1_793_837_815,
                ended_at: 1_793_837_816,
            }),
            used_seconds: 5210,
            cap_seconds: 144_000,
            tier: Tier::Plus,
            resets_at: 1_793_491_200,
            complimentary: false,
            intro_eligible: false,
        };
        facts.pro.info = Some(&info);
        // Known from a while ago, with the server out of reach now.
        facts.pro.checked = Some(Duration::from_secs(7 * 60 + 59));
        let shown = build(&facts).pro;
        assert!(shown.known && shown.entitled);
        assert_eq!((shown.used_seconds, shown.cap_seconds), (5210, 144_000));
        assert_eq!(shown.resets_at, 1_793_491_200);
        assert_eq!(shown.checked_minutes, Some(7));
        assert_eq!(shown.status, "past_due");
        assert_eq!(shown.plan, Some(wire::ProPlan::Yearly));
        assert_eq!(shown.tier, wire::ProTier::Plus);
        assert!(shown.ends);
        assert_eq!(
            (
                shown.period_end,
                shown.grace_until,
                shown.ends_at,
                shown.ended_at
            ),
            (1_793_837_814, 1_794_097_014, 1_793_837_815, 1_793_837_816)
        );
        assert!(!shown.complimentary);

        // An account that is given Pro can use it and is shown as such,
        // unless a subscription of its own is paying.
        let given = |status: Option<&str>| AccountInfo {
            entitled: false,
            complimentary: true,
            subscription: status.and_then(|status| {
                let mut subscription = info.subscription.clone()?;
                subscription.status = status.into();
                Some(subscription)
            }),
            ..info.clone()
        };
        let accounts = [
            (given(None), true),
            (given(Some("canceled")), true),
            (given(Some("active")), false),
        ];
        for (given, free) in &accounts {
            facts.pro.info = Some(given);
            let shown = build(&facts).pro;
            assert_eq!((shown.complimentary, shown.entitled), (*free, *free));
        }
    }

    #[test]
    fn the_prices_shown_are_the_ones_the_host_was_given() {
        let settings = Settings::default();
        let mut facts = facts(&settings);
        let shown = build(&facts).pro;
        // Until the server says: both tiers, the smaller first, as the
        // window shows them when it is told nothing.
        assert_eq!(shown.offers, wire::Pro::default().offers);
        assert_eq!(shown.currency, "eur");
        let said = Prices {
            offers: vec![vinput_core::cloud::Offer {
                tier: Tier::Pro,
                monthly: 700,
                first: 490,
                yearly: 6900,
                cap_seconds: 144_000,
            }],
            currency: "usd".into(),
            open: false,
        };
        // To someone signed out as to anyone else.
        facts.pro = no_pro(&said);
        let shown = build(&facts).pro;
        assert_eq!(
            shown.offers,
            [wire::ProOffer {
                tier: wire::ProTier::Pro,
                monthly: 700,
                first: 490,
                yearly: 6900,
                seconds: 144_000,
            }]
        );
        assert_eq!(shown.currency, "usd");
        assert!(!shown.open && wire::Pro::default().open);

        // The price of a first month is shown to whoever is signed out, who
        // may be new, and to an account only once the server has said it
        // is: not before it has said anything, and not on a no.
        let first = |facts: &Facts<'_>| build(facts).pro.offers[0].first;
        facts.pro.email = Some("ana@example.com");
        assert_eq!(first(&facts), 0);
        let info = AccountInfo {
            email: "ana@example.com".into(),
            entitled: false,
            subscription: None,
            used_seconds: 0,
            cap_seconds: 144_000,
            tier: Tier::Pro,
            resets_at: 0,
            complimentary: false,
            intro_eligible: false,
        };
        facts.pro.info = Some(&info);
        assert_eq!(first(&facts), 0);
        let new = AccountInfo {
            intro_eligible: true,
            ..info.clone()
        };
        facts.pro.info = Some(&new);
        assert_eq!(first(&facts), 490);
        // The prices themselves are everyone's.
        assert_eq!(build(&facts).pro.offers[0].monthly, 700);
    }

    #[test]
    fn the_model_to_carry_on_with_is_the_one_last_used_or_else_the_first_that_is_here() {
        let pro = CATALOG
            .iter()
            .find(|model| model.provider == ProviderKind::Cloud)
            .unwrap_or(&CATALOG[0]);
        let mut settings = choosing(pro);
        let here: Vec<&Model> = CATALOG
            .iter()
            .filter(|model| model.local.is_some())
            .collect();
        let (first, last) = (here[0], here[here.len() - 1]);
        let download = |facts: &mut Facts<'_>, which: &Model| {
            for (model, status) in &mut facts.models {
                if model.id == which.id {
                    *status = Status::Installed;
                }
            }
        };
        // Nothing downloaded: nothing to carry on with, whatever was used.
        last.id.clone_into(&mut settings.last_local_model);
        let mut all = facts(&settings);
        assert_eq!(build(&all).pro.spare, None);
        // One that is being downloaded is not one that is here.
        all.models[here.len()].1 = Status::Installing { done: 1, total: 2 };
        assert_eq!(build(&all).pro.spare, None);
        // The first of the catalog that is here, until the one last used is.
        download(&mut all, first);
        assert_eq!(build(&all).pro.spare.as_deref(), Some(first.name));
        download(&mut all, last);
        assert_eq!(build(&all).pro.spare.as_deref(), Some(last.name));
        // Never one that knows none of the languages: the first of the
        // catalog cannot do Slovenian, the last one can.
        let slovenian = Settings {
            languages: vec!["sl-SI".to_owned()],
            last_local_model: first.id.to_owned(),
            ..settings.clone()
        };
        assert_eq!(first.reach(&slovenian.languages), 0);
        let mut others = facts(&slovenian);
        download(&mut others, first);
        assert_eq!(build(&others).pro.spare, None);
        download(&mut others, last);
        assert_eq!(build(&others).pro.spare.as_deref(), Some(last.name));
        // Its row says which languages it lacks, in words.
        let row = |view: &wire::View, id: &str| {
            let found = view.models.list.iter().find(|model| model.id == id);
            found.map(|model| (model.lacks.clone(), model.knows_none))
        };
        let shown = build(&others);
        assert_eq!(
            row(&shown, first.id),
            Some((vec!["Slovenian".to_owned()], true))
        );
        assert_eq!(row(&shown, last.id), Some((Vec::new(), false)));
        let both = Settings {
            languages: vec!["sl-SI".to_owned(), "en-US".to_owned()],
            ..slovenian.clone()
        };
        assert_eq!(
            row(&build(&facts(&both)), first.id),
            Some((vec!["Slovenian".to_owned()], false))
        );

        // While it serves in place of VinPUT Pro, the status says so; the
        // model the settings choose is still VinPUT Pro.
        assert_eq!(build(&all).status.runs_on, wire::RunsOn::Online);
        all.pro.carrying = true;
        let shown = build(&all);
        assert_eq!(shown.status.model.as_deref(), Some(pro.name));
        assert_eq!(
            shown.status.runs_on,
            wire::RunsOn::Spare(last.name.to_owned())
        );
        assert!(
            shown
                .models
                .list
                .iter()
                .all(|model| model.in_use == (model.id == pro.id))
        );
        // Someone who dictates with another model is not told about hours.
        let other = choosing(first);
        let mut all = facts(&other);
        download(&mut all, first);
        all.pro.carrying = true;
        assert_ne!(
            build(&all).status.runs_on,
            wire::RunsOn::Spare(first.name.to_owned())
        );
    }

    #[test]
    fn the_settings_are_shown_as_text_the_window_can_send_back() {
        let mut settings = Settings::parse(
            "hotkey = Ctrl+Shift+Space\nhotkey_memo = F9\nlanguages = en-US, sl-SI\n\
             project_id = my-project\nmemo_dir = D:\\Memos\nmicrophone = {usb}\n\
             graphics_card = false\n",
        );
        settings.hotkey_enter = None;
        let view = build(&facts(&settings)).settings;
        assert_eq!(view.hotkey, "Ctrl+Shift+Space");
        assert_eq!(view.hotkey_enter, "");
        assert_eq!(view.hotkey_memo, "F9");
        assert_eq!(view.languages, "en-US, sl-SI");
        assert_eq!(view.memo_dir, r"D:\Memos");
        assert_eq!(view.microphone, "{usb}");
        assert!(!view.graphics_card);
    }

    #[test]
    fn without_a_graphics_card_the_switch_is_off_whatever_the_file_says() {
        let settings = Settings::default();
        assert!(settings.local_gpu);
        let mut facts = facts(&settings);
        facts.graphics_card = None;
        let view = build(&facts);
        assert!(!view.settings.graphics_card);
        assert_eq!(view.models.graphics_card, None);
    }

    #[test]
    fn the_header_says_which_model_is_at_work_and_where() {
        let settings = choosing(online());
        let view = build(&facts(&settings));
        assert_eq!(view.status.model.as_deref(), Some(online().name));
        assert_eq!(view.status.runs_on, wire::RunsOn::Online);

        let settings = choosing(local());
        let mut on_pc = facts(&settings);
        on_pc.listening = true;
        let view = build(&on_pc);
        assert!(view.status.listening);
        assert_eq!(view.status.model.as_deref(), Some(local().name));
        assert_eq!(
            view.status.runs_on,
            wire::RunsOn::GraphicsCard(CARD.to_owned())
        );
        // Until the engine for the card is here, the processor does the
        // work, but for a model whose only engine runs on either.
        on_pc.card_engine = false;
        assert_eq!(
            build(&on_pc).status.runs_on,
            wire::RunsOn::GraphicsCard(CARD.to_owned())
        );
        let one_file = CATALOG.iter().find(|model| model.files().count() == 1);
        let whisper = choosing(one_file.unwrap_or(&CATALOG[0]));
        let mut waiting = facts(&whisper);
        waiting.card_engine = false;
        assert_eq!(build(&waiting).status.runs_on, wire::RunsOn::Processor);
        on_pc.card_engine = true;
        on_pc.graphics_card = None;
        assert_eq!(build(&on_pc).status.runs_on, wire::RunsOn::Processor);

        let mut by_choice = settings.clone();
        by_choice.local_gpu = false;
        assert_eq!(
            build(&facts(&by_choice)).status.runs_on,
            wire::RunsOn::Processor
        );
    }

    #[test]
    fn a_model_set_by_hand_is_named_as_it_was_written() {
        let settings = Settings {
            model: "chirp_9".into(),
            ..Settings::default()
        };
        let view = build(&facts(&settings));
        assert_eq!(view.status.model.as_deref(), Some("chirp_9"));
        assert_eq!(view.status.runs_on, wire::RunsOn::Online);
        assert!(view.models.list.iter().all(|model| !model.in_use));
    }

    #[test]
    fn each_model_is_listed_with_where_it_stands() {
        let settings = choosing(local());
        let mut facts = facts(&settings);
        // A model that comes as two files is as large as both.
        let file_bytes = local().bytes();
        assert_eq!(local().files().count(), 2);
        assert!(local().files().all(|file| file.source.bytes < file_bytes));
        let row = |facts: &Facts<'_>| {
            build(facts)
                .models
                .list
                .into_iter()
                .find(|model| model.id == local().id)
                .unwrap_or_default()
        };
        let set = |facts: &mut Facts<'_>, status: Status| {
            for (model, stands) in &mut facts.models {
                if model.id == local().id {
                    *stands = status.clone();
                }
            }
        };

        let missing = row(&facts);
        assert_eq!(missing.name, local().name);
        assert_eq!(missing.summary, local().summary);
        assert_eq!(missing.detail, local().detail);
        assert!(missing.local && missing.in_use && !missing.custom);
        assert_eq!(missing.state, wire::ModelState::Missing);
        // A download that is still to come counts the engine it needs.
        assert_eq!(missing.bytes, file_bytes + 1000);

        set(&mut facts, Status::Installing { done: 1, total: 4 });
        let installing = row(&facts);
        assert_eq!(
            installing.state,
            wire::ModelState::Installing { done: 1, total: 4 }
        );
        assert_eq!(installing.bytes, file_bytes);

        set(&mut facts, Status::Failed("no connection".into()));
        assert_eq!(
            row(&facts).state,
            wire::ModelState::Failed {
                reason: "no connection".into()
            }
        );
        set(&mut facts, Status::Installed);
        assert_eq!(row(&facts).state, wire::ModelState::Installed);

        let listed = build(&facts).models.list;
        let online = listed.iter().find(|model| model.id == online().id);
        assert_eq!(
            online.map(|model| (model.local, model.bytes, model.in_use, &model.state)),
            Some((false, 0, false, &wire::ModelState::Online))
        );
    }

    #[test]
    fn the_engine_and_the_disk_space_are_passed_on_as_they_are() {
        let settings = Settings::default();
        let mut facts = facts(&settings);
        facts.disk_bytes = 1_990_000_000;
        facts.engine = EngineState::Fetching { done: 9, total: 31 };
        let models = build(&facts).models;
        assert_eq!(models.disk_bytes, 1_990_000_000);
        assert_eq!(models.engine, wire::Engine::Fetching { done: 9, total: 31 });
        facts.engine = EngineState::Failed("no connection".into());
        assert_eq!(
            build(&facts).models.engine,
            wire::Engine::Failed {
                reason: "no connection".into()
            }
        );
        facts.engine = EngineState::Missing;
        assert_eq!(build(&facts).models.engine, wire::Engine::Missing);
    }

    #[test]
    fn a_model_can_be_used_or_removed_only_when_that_makes_sense() {
        let missing = Status::Missing { bytes: 1 };
        let installing = Status::Installing { done: 1, total: 4 };
        let failed = Status::Failed("no connection".into());
        // Status, in use, added by the user: can it be used, and removed?
        let cases = [
            (&missing, false, false, false, false),
            (&installing, true, false, false, false),
            (&failed, false, false, false, false),
            (&Status::Installed, false, false, true, true),
            // The model in use cannot be removed from under dictation.
            (&Status::Installed, true, false, true, false),
            (&Status::Online, false, false, true, false),
            (&Status::Online, true, false, true, false),
            // A model someone added can also be taken off the list before it
            // is downloaded, or after its download failed, unless it is in use.
            (&missing, false, true, false, true),
            (&failed, false, true, false, true),
            (&missing, true, true, false, false),
            (&installing, false, true, false, false),
            (&Status::Installed, true, true, true, false),
        ];
        for (status, in_use, custom, used, removed) in cases {
            assert_eq!(can_use(status), used, "{status:?}");
            assert_eq!(
                can_remove(status, in_use, custom),
                removed,
                "{status:?} {in_use} {custom}"
            );
        }
    }

    #[test]
    fn the_inputs_the_api_and_the_key_are_shown() {
        let settings = Settings::default();
        let microphones = [Microphone {
            id: "usb".into(),
            name: "Microphone (USB Audio)".into(),
        }];
        let mut facts = facts(&settings);
        facts.microphones = &microphones;
        facts.clients = vec!["Control Deck".into()];
        facts.gemini_key_stored = true;
        let view = build(&facts);
        assert_eq!(
            view.microphones,
            [wire::Microphone {
                id: "usb".into(),
                name: "Microphone (USB Audio)".into()
            }]
        );
        assert_eq!(view.api.clients, ["Control Deck"]);
        assert!(view.gemini_key_stored);
        assert_eq!(view.default_memo_dir, facts.default_memo_dir);
    }

    #[test]
    fn the_setup_of_a_google_service_is_shown_as_it_stands() {
        let settings = Settings::default();
        let check = wire::GoogleCheck::Failed {
            problem: wire::GoogleProblem::Billing,
            said: String::new(),
        };
        let mut facts = facts(&settings);
        facts.google = GoogleFacts {
            step: wire::GoogleStep::Enter,
            service: wire::GoogleService::Speech,
            check: &check,
            project: "demo",
            location: "eu",
            key_length: 39,
            replacing: true,
            account: Some("vinput@demo.iam.gserviceaccount.com"),
            sign_in: true,
            ready: true,
            model: "",
        };
        let view = build(&facts);
        assert_eq!(
            view.google,
            wire::Google {
                step: wire::GoogleStep::Enter,
                service: wire::GoogleService::Speech,
                check,
                project_id: "demo".into(),
                location: "eu".into(),
                account: "vinput@demo.iam.gserviceaccount.com".into(),
                sign_in_found: true,
                speech_ready: true,
                model: String::new(),
                key_length: 39,
                replacing: true,
            }
        );
        // The models of Google's services say which setup is theirs.
        let service = |provider| {
            let listed = CATALOG.iter().position(|model| model.provider == provider);
            listed.and_then(|index| view.models.list[index].google)
        };
        assert_eq!(
            service(ProviderKind::Gemini),
            Some(wire::GoogleService::Gemini)
        );
        assert_eq!(
            service(ProviderKind::GoogleCloud),
            Some(wire::GoogleService::Speech)
        );
        assert_eq!(service(ProviderKind::Local), None);
        assert_eq!(service(ProviderKind::Cloud), None);
    }

    #[test]
    fn the_update_follows_the_check_and_the_download() {
        let settings = Settings::default();
        let release = release();
        let failed = LastCheck::Failed("no connection".into());
        let up_to_date = LastCheck::UpToDate;
        let show = |update: UpdateFacts<'_>| {
            let mut facts = facts(&settings);
            facts.update = update;
            build(&facts).update
        };

        let checking = show(UpdateFacts {
            checking: true,
            ..no_update()
        });
        assert_eq!(checking.check, wire::UpdateCheck::Checking);
        assert_eq!(
            show(UpdateFacts {
                last_check: &up_to_date,
                ..no_update()
            })
            .check,
            wire::UpdateCheck::UpToDate
        );
        assert_eq!(
            show(UpdateFacts {
                last_check: &failed,
                ..no_update()
            })
            .check,
            wire::UpdateCheck::Failed {
                reason: "no connection".into()
            }
        );

        let found = UpdateFacts {
            release: Some(&release),
            // A later check that failed does not take the release away.
            last_check: &failed,
            ..no_update()
        };
        let available = show(found);
        assert_eq!(available.check, wire::UpdateCheck::Available);
        assert_eq!(available.download, wire::Download::Idle);
        assert_eq!(
            available.release,
            Some(wire::Release {
                version: "9.9.9".into(),
                notes: vec!["Faster".into(), "Smaller".into()],
                bytes: 6_000_000,
            })
        );
        assert_eq!(
            show(UpdateFacts {
                downloaded: Some(1_000_000),
                ..found
            })
            .download,
            wire::Download::Running {
                done: 1_000_000,
                total: 6_000_000
            }
        );
        assert_eq!(
            show(UpdateFacts {
                failed: Some("the disk is full"),
                ..found
            })
            .download,
            wire::Download::Failed {
                reason: "the disk is full".into()
            }
        );
        assert_eq!(
            show(UpdateFacts {
                installing: true,
                ..found
            })
            .download,
            wire::Download::Installing
        );
    }

    #[test]
    fn the_finder_and_the_notice_are_shown_as_they_stand() {
        let settings = Settings::default();
        let mut facts = facts(&settings);
        let choice = Choice {
            path: "ggml-medium.bin".into(),
            url: "https://huggingface.co/org/model/resolve/0/ggml-medium.bin".into(),
            sha256: None,
            bytes: 1_533_763_059,
            origin: Origin::Hub("org/model".into()),
            whisper: None,
        };
        facts.finder = Snapshot {
            busy: Some("Looking it up"),
            files: vec![(choice, false)],
            error: Some("This is a GGUF file.".into()),
            added: Some("custom-medium".into()),
        };
        let notice = wire::Notice {
            kind: wire::NoticeKind::Error,
            text: "F2 is taken.".into(),
            page: wire::Page::General,
        };
        facts.notice = Some(notice.clone());
        let view = build(&facts);
        assert_eq!(view.finder.busy.as_deref(), Some("Looking it up"));
        assert_eq!(view.finder.error.as_deref(), Some("This is a GGUF file."));
        assert_eq!(view.finder.added.as_deref(), Some("custom-medium"));
        assert_eq!(
            view.finder.files,
            [wire::FoundFile {
                path: "ggml-medium.bin".into(),
                bytes: 1_533_763_059,
                detail: "huggingface.co/org/model".into(),
                usable: true,
            }]
        );
        assert_eq!(view.notice, Some(notice));
    }

    #[test]
    fn the_same_facts_always_give_the_same_picture() {
        let settings = choosing(local());
        assert_eq!(build(&facts(&settings)), build(&facts(&settings)));
    }
}
