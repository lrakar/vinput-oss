//! The contract between the Vinput engine and its windows.
//!
//! The windows (Settings, the update window, setup) run in a process of their
//! own that exists only while one of them is open. The background process
//! that listens and types owns every piece of state. The two talk in lines of
//! JSON over the window process's standard input and output:
//!
//! ```text
//! background process                      window process
//!   (engine, tray, hotkeys)                 (Slint)
//!        |  -- Event::State(View) ------------>  |   everything to show
//!        |  -- Event::Show(Page) ------------->  |   turn to a page
//!        |  <------------- Command::... ------   |   what the user did
//! ```
//!
//! A [`View`] is a complete picture: the window shows what the latest one
//! says and keeps no state of its own beyond what is being typed. A
//! [`Command`] says what the user did, never how to do it. Nothing here knows
//! about Slint or about the engine, so either side can change, or be replaced
//! by a fake, without the other noticing.
//!
//! Text for people is written by the window, not here: the view carries
//! facts (`bytes`, a status, a name) and the window decides how to say them.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Sends a plain list of choices over the line as its number, which takes
/// far less code in the executable than its names would. Both ends of the
/// line are the same executable, so the numbers never have to mean the same
/// in two versions. A number that names nothing reads as the first choice.
macro_rules! sent_as_number {
    ($name:ident: $first:ident $(, $rest:ident)*) => {
        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_u8(*self as u8)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let number = u8::deserialize(deserializer)?;
                let all = [Self::$first $(, Self::$rest)*];
                let found = all.into_iter().find(|choice| *choice as u8 == number);
                Ok(found.unwrap_or(Self::$first))
            }
        }
    };
}

/// The pages of the Settings window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Page {
    /// Hotkeys and the everyday switches.
    #[default]
    General,
    /// The speech model and the downloads.
    Models,
    /// Adding a model of one's own, reached from the Models page.
    AddModel,
    /// Setting up a Google service of one's own, step by step, reached from
    /// the Models page.
    Google,
    /// The account and the subscription of `VinPUT` Pro.
    Pro,
    /// The local API, voice memos and web search.
    Advanced,
    /// Version, updates and links.
    About,
    /// What someone who has just installed sees first: the two ways to
    /// dictate, a subscription or a model on this PC. Like the two pages
    /// after it, it is shown without the navigation pane.
    Welcome,
    /// The account and the plans of `VinPUT` Pro, as a step of the welcome.
    WelcomePro,
    /// The models that run on this PC, as a step of the welcome.
    WelcomeLocal,
}

impl Page {
    /// Whether this is a page of the welcome a new user is shown, which
    /// has the window to itself.
    #[must_use]
    pub fn is_welcome(self) -> bool {
        matches!(self, Self::Welcome | Self::WelcomePro | Self::WelcomeLocal)
    }
}

/// Which window a window process shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Surface {
    /// The Settings window, opened on a page.
    Settings(Page),
    /// The window that offers a newer version and installs it.
    Update,
}

/// Everything the windows show. Sent whole whenever any of it changes.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct View {
    /// The version that is running, such as `0.3.0`.
    pub version: String,
    /// What Vinput is doing right now.
    pub status: Status,
    /// The settings as they are in force.
    pub settings: Settings,
    /// The audio inputs Windows has, besides its default.
    pub microphones: Vec<Microphone>,
    /// The speech models and what is on this PC.
    pub models: Models,
    /// The search for a model to add.
    pub finder: Finder,
    /// The setup of a Google service of one's own.
    pub google: Google,
    /// The local API.
    pub api: Api,
    /// Whether a Gemini API key is in the Windows credential store. The key
    /// itself never leaves the background process.
    pub gemini_key_stored: bool,
    /// Where voice memos go when [`Settings::memo_dir`] is empty.
    pub default_memo_dir: String,
    /// What is known about newer versions.
    pub update: Update,
    /// The account of `VinPUT` Pro.
    pub pro: Pro,
    /// Something to tell the user about the last thing they asked for.
    pub notice: Option<Notice>,
}

/// What Vinput is doing right now, for the header of every window.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Status {
    /// Whether a dictation is running.
    pub listening: bool,
    /// Name of the model dictation uses, or `None` before one is chosen.
    pub model: Option<String>,
    /// Where that model runs.
    pub runs_on: RunsOn,
}

/// Where the model in use runs.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunsOn {
    /// No model is in use.
    #[default]
    Nowhere,
    /// At an online service.
    Online,
    /// On this PC's processor.
    Processor,
    /// On this graphics card.
    GraphicsCard(String),
    /// On this PC, with the model of this name, in place of `VinPUT` Pro
    /// while the month's hours are used up.
    Spare(String),
}

/// The settings a user can change. Hotkeys are text such as `F1` or
/// `Ctrl+Shift+Space`; an empty one is not set.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
#[expect(clippy::struct_excessive_bools, reason = "independent on/off switches")]
pub struct Settings {
    /// Key that dictates into the focused window. Always set.
    pub hotkey: String,
    /// Key that dictates and then presses Enter.
    pub hotkey_enter: String,
    /// Key that dictates a web search.
    pub hotkey_search: String,
    /// Key that records a voice memo.
    pub hotkey_memo: String,
    /// Id of the audio input, or empty for the one Windows uses by default.
    pub microphone: String,
    /// Languages to recognize, as the user typed them: `en-US, sl-SI`.
    pub languages: String,
    /// Whether Vinput starts with Windows.
    pub autostart: bool,
    /// Whether a short sound plays when listening starts.
    pub ready_sound: bool,
    /// Whether a short sound plays when listening stops.
    pub stop_sound: bool,
    /// Whether Vinput looks for newer versions by itself.
    pub check_updates: bool,
    /// Whether local models run on the graphics card.
    pub graphics_card: bool,
    /// Whether other programs may connect to the local API.
    pub api_enabled: bool,
    /// Folder for voice memos, or empty for the default.
    pub memo_dir: String,
    /// Whether a saved voice memo is put on the clipboard.
    pub memo_to_clipboard: bool,
    /// The web search a dictated search opens, with `%s` for what was said.
    pub search_url: String,
}

/// One setting changed by the user. Each takes effect at once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Change {
    /// See [`Settings::hotkey`]. An empty or unusable key is refused with a
    /// [`Notice`].
    Hotkey(String),
    /// See [`Settings::hotkey_enter`].
    HotkeyEnter(String),
    /// See [`Settings::hotkey_search`].
    HotkeySearch(String),
    /// See [`Settings::hotkey_memo`].
    HotkeyMemo(String),
    /// See [`Settings::microphone`].
    Microphone(String),
    /// See [`Settings::languages`].
    Languages(String),
    /// See [`Settings::autostart`].
    Autostart(bool),
    /// See [`Settings::ready_sound`].
    ReadySound(bool),
    /// See [`Settings::stop_sound`].
    StopSound(bool),
    /// See [`Settings::check_updates`].
    CheckUpdates(bool),
    /// See [`Settings::graphics_card`].
    GraphicsCard(bool),
    /// See [`Settings::api_enabled`].
    ApiEnabled(bool),
    /// See [`Settings::memo_dir`].
    MemoDir(String),
    /// See [`Settings::memo_to_clipboard`].
    MemoToClipboard(bool),
    /// See [`Settings::search_url`]. Empty goes back to the default search.
    SearchUrl(String),
}

/// An audio input.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Microphone {
    /// What Windows knows it by; what [`Settings::microphone`] holds.
    pub id: String,
    /// The name Windows shows for it.
    pub name: String,
}

/// The Models page.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Models {
    /// Every model, in the order to list them.
    pub list: Vec<Model>,
    /// Bytes the downloaded models and their engine take on disk.
    pub disk_bytes: u64,
    /// Name of the graphics card that can run local models, if the PC has one.
    pub graphics_card: Option<String>,
    /// Where the engine that runs local models stands.
    pub engine: Engine,
}

/// One speech model.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Model {
    /// What [`Command::UseModel`] and its like name it by.
    pub id: String,
    /// Its name.
    pub name: String,
    /// One line about what it needs or is good at.
    pub summary: String,
    /// Its languages if it is local, what it needs if it is online.
    pub detail: String,
    /// Whether it runs on this PC rather than at an online service.
    pub local: bool,
    /// Bytes its download takes; zero for an online model.
    pub bytes: u64,
    /// Whether the user added it themselves.
    pub custom: bool,
    /// The Google service of the user's own that it runs at, if it is one
    /// of those: it is set up on the [`Page::Google`] page.
    pub google: Option<GoogleService>,
    /// Whether dictation uses it.
    pub in_use: bool,
    /// Where it stands.
    pub state: ModelState,
}

/// Where a model stands.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelState {
    /// An online model: nothing to install.
    #[default]
    Online,
    /// A local model that is not on this PC.
    Missing,
    /// Being downloaded.
    Installing {
        /// Bytes so far.
        done: u64,
        /// Bytes in total.
        total: u64,
    },
    /// On this PC and ready.
    Installed,
    /// The last download failed.
    Failed {
        /// Why, in words for the user.
        reason: String,
    },
}

/// Where the engine that runs local models stands.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Engine {
    /// Installed, or not needed.
    #[default]
    Ready,
    /// Not installed, and nothing is fetching it.
    Missing,
    /// Being downloaded.
    Fetching {
        /// Bytes so far.
        done: u64,
        /// Bytes in total.
        total: u64,
    },
    /// The last download failed.
    Failed {
        /// Why, in words for the user.
        reason: String,
    },
}

/// The search for a model to add: what [`Command::FindModel`] has found.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Finder {
    /// What it is doing while it is at work, such as `Looking at the repository`.
    pub busy: Option<String>,
    /// The files the last search found, to pick one from.
    pub files: Vec<FoundFile>,
    /// Why the last step failed.
    pub error: Option<String>,
    /// Id of the model just added, for the window to go back to the list and
    /// point at it.
    pub added: Option<String>,
}

/// A file that can be added as a model.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FoundFile {
    /// Its path in its repository, or on this PC.
    pub path: String,
    /// Its size in bytes.
    pub bytes: u64,
    /// What is known about it, such as `Whisper large-v3, 99 languages`, or
    /// why it cannot be used.
    pub detail: String,
    /// Whether it can be added.
    pub usable: bool,
}

/// The Google services someone can use with a key or a project of their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GoogleService {
    /// The Gemini API, with an API key from Google AI Studio.
    #[default]
    Gemini,
    /// Google Cloud Speech-to-Text, with a project and its credentials.
    Speech,
}

sent_as_number!(GoogleService: Gemini, Speech);

/// The steps of setting a Google service up, in their order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GoogleStep {
    /// Which service.
    #[default]
    Service,
    /// How to get what it needs from Google.
    Get,
    /// Entering it: a key, or a project, its location and its credentials.
    Enter,
    /// It works and is in use.
    Done,
}

sent_as_number!(GoogleStep: Service, Get, Enter, Done);

/// The setup of a Google service of one's own: where it stands, what was
/// entered, and what Google made of it. No key is ever part of it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Google {
    /// The step that is shown.
    pub step: GoogleStep,
    /// The service being set up.
    pub service: GoogleService,
    /// What Google made of what is entered.
    pub check: GoogleCheck,
    /// The Google Cloud project, as entered.
    pub project_id: String,
    /// The Speech-to-Text location, as picked.
    pub location: String,
    /// The address of the service account whose key is used for Google
    /// Cloud; empty while the Google sign-in of this PC is.
    pub account: String,
    /// Whether this PC has a Google sign-in made with the Google Cloud CLI.
    pub sign_in_found: bool,
    /// Whether Google Cloud Speech has what it needs to be dictated with: a
    /// project, and a key or a sign-in. For Gemini that is
    /// [`View::gemini_key_stored`].
    pub speech_ready: bool,
    /// Name of the model the setup has put to use, for its last step.
    pub model: String,
    /// How many characters the Gemini API key has that was entered and is
    /// not kept yet, so that its field can show that something is in it;
    /// zero when none was entered. Never the key.
    pub key_length: u8,
    /// Whether the key that is kept is being replaced: its field is shown
    /// again in place of the line that says a key is stored.
    pub replacing: bool,
}

/// What Google made of what was entered in the setup.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum GoogleCheck {
    /// Nothing was asked yet.
    #[default]
    Idle,
    /// Google is being asked.
    Checking,
    /// It works.
    Works,
    /// It does not.
    Failed {
        /// What is wrong.
        problem: GoogleProblem,
        /// What Google or Windows said about it, where that adds something.
        said: String,
    },
}

// Over the line a check is three plain values: where it stands, what is
// wrong, and what was said.
impl Serialize for GoogleCheck {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let (stands, problem, said) = match self {
            Self::Idle => (0u8, GoogleProblem::Other, ""),
            Self::Checking => (1, GoogleProblem::Other, ""),
            Self::Works => (2, GoogleProblem::Other, ""),
            Self::Failed { problem, said } => (3, *problem, said.as_str()),
        };
        (stands, problem as u8, said).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for GoogleCheck {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (stands, problem, said) = <(u8, u8, String)>::deserialize(deserializer)?;
        Ok(match stands {
            1 => Self::Checking,
            2 => Self::Works,
            3 => Self::Failed {
                problem: GoogleProblem::numbered(problem),
                said,
            },
            _ => Self::Idle,
        })
    }
}

/// What is wrong with what was entered for a Google service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GoogleProblem {
    /// Google does not know the key, or the sign-in is no longer good.
    WrongKey,
    /// The key or the account may not use this service.
    NoAccess,
    /// The project has no billing.
    Billing,
    /// The quota is used up for now.
    Quota,
    /// Google does not know the project, or not in this location.
    NotFound,
    /// No Google sign-in is on this PC and no key file was chosen.
    NoSignIn,
    /// The file chosen is not a service account key.
    BadFile,
    /// Google could not be reached.
    Offline,
    /// Something else; `said` has it.
    #[default]
    Other,
}

impl GoogleProblem {
    /// The problem with this number; one that names none is [`Self::Other`].
    fn numbered(number: u8) -> Self {
        let all = [
            Self::WrongKey,
            Self::NoAccess,
            Self::Billing,
            Self::Quota,
            Self::NotFound,
            Self::NoSignIn,
            Self::BadFile,
            Self::Offline,
        ];
        let found = all.into_iter().find(|problem| *problem as u8 == number);
        found.unwrap_or(Self::Other)
    }
}

/// What the user did in the setup of a Google service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Setup {
    /// Set this service up, or look at how it is set up: on to how to get
    /// what it needs, or straight to what is entered if something is.
    Open(GoogleService),
    /// Turn to this step.
    Step(GoogleStep),
    /// A Gemini API key was entered. Google is asked about it; if it
    /// works, finishing the setup keeps it.
    Key(String),
    /// Replace the Gemini API key that is kept: show its field again.
    Replace,
    /// The Google Cloud project was entered.
    Project(String),
    /// The Speech-to-Text location was picked.
    Location(String),
    /// Ask the user for a service account key file with the file picker.
    PickKey,
    /// Use the Google sign-in of this PC instead of a key file.
    UseSignIn,
    /// Ask Google again about what is entered.
    Check,
    /// Keep what was entered and found to work, and dictate with the
    /// model of the service.
    Finish,
    /// Remove the Gemini API key from the credential store.
    RemoveKey,
    /// Leave the setup. What was entered and not put to use is forgotten,
    /// and so is the answer to a question Google is still being asked.
    Close,
}

/// The local API.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Api {
    /// The name of the pipe, such as `\\.\pipe\vinput`.
    pub pipe: String,
    /// Names of the programs connected now.
    pub clients: Vec<String>,
}

/// What is known about newer versions, and how far installing one is.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Update {
    /// Where the check stands.
    pub check: UpdateCheck,
    /// The newer release, once one is found.
    pub release: Option<Release>,
    /// Where its download stands.
    pub download: Download,
}

/// Where the check for a newer version stands.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UpdateCheck {
    /// Nobody has looked yet.
    #[default]
    Unchecked,
    /// Looking now.
    Checking,
    /// The running version is the newest.
    UpToDate,
    /// [`Update::release`] can be installed.
    Available,
    /// The check failed.
    Failed {
        /// Why, in words for the user.
        reason: String,
    },
}

/// A newer release.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Release {
    /// Its version.
    pub version: String,
    /// What is new, a line each.
    pub notes: Vec<String>,
    /// Bytes its download takes.
    pub bytes: u64,
}

/// Where the download of an update stands.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Download {
    /// Not started.
    #[default]
    Idle,
    /// Under way.
    Running {
        /// Bytes so far.
        done: u64,
        /// Bytes in total.
        total: u64,
    },
    /// Done; setup is starting and Vinput is about to restart.
    Installing,
    /// It failed.
    Failed {
        /// Why, in words for the user.
        reason: String,
    },
}

/// The id of the model that is `VinPUT` Pro, in [`Models::list`].
pub const PRO_MODEL: &str = "vinput-pro";

/// What [`Pro::problem`] says when a plan cannot be subscribed to yet: news
/// to tell calmly, and nothing that went wrong or that asking again mends.
pub const PRO_SOON: &str = "Subscribing opens shortly. Nothing was charged. Your free options under \
                            Models keep working.";

/// `VinPUT` Pro: who is signed in, what they pay for and what they have
/// used. One flat record: which of it means something follows from
/// [`Pro::who`] and [`Pro::known`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts about an account"
)]
pub struct Pro {
    /// Whether anyone is signed in.
    pub who: ProWho,
    /// While a sign-in waits: the code the page in the browser asks for;
    /// empty until the server has handed it out.
    pub code: String,
    /// While a sign-in waits: how many minutes the code works for.
    pub code_minutes: u32,
    /// The Google address of whoever is signed in.
    pub email: String,
    /// Whether the server has said how the account stands since the app
    /// started. Until it has, nothing about the subscription is known.
    pub known: bool,
    /// Whether Pro can be used right now, in the server's judgment.
    pub entitled: bool,
    /// Whether the account has Pro at no charge, with no subscription that
    /// pays for it: there is then nothing to subscribe to or to manage.
    pub complimentary: bool,
    /// Where the subscription stands, in the payment service's word:
    /// `active`, `past_due`, `canceled`, `incomplete` and so on. Empty for
    /// someone who never had one.
    pub status: String,
    /// What is paid for, if it is a plan this version knows.
    pub plan: Option<ProPlan>,
    /// The tier in effect, or the one of a subscription that is not.
    pub tier: ProTier,
    /// When the period that is paid for ends, in seconds since 1970; zero
    /// if the server did not say. So are the dates below.
    pub period_end: u64,
    /// Whether the subscription ends instead of renewing.
    pub ends: bool,
    /// When it ends, if it is set to.
    pub ends_at: u64,
    /// When it ended, if it has.
    pub ended_at: u64,
    /// Until when Pro keeps working although a renewal failed.
    pub grace_until: u64,
    /// Seconds of speech recognized this month.
    pub used_seconds: u64,
    /// Seconds a month comes with.
    pub cap_seconds: u64,
    /// When the count of the month starts again.
    pub resets_at: u64,
    /// How many minutes ago the server last said how the account stands,
    /// while what is shown is that and the server cannot be asked now.
    pub checked_minutes: Option<u32>,
    /// What is under way.
    pub doing: ProDoing,
    /// Why the server could not be asked, or what it said, in words for the
    /// user. What is shown under it is then what was known before.
    pub problem: Option<String>,
    /// The tiers that can be subscribed to, the smaller first.
    pub offers: Vec<ProOffer>,
    /// The currency of the prices, as its three letters: `eur`.
    pub currency: String,
    /// Whether a plan can be subscribed to yet.
    pub open: bool,
    /// Name of the model on this PC that dictation carries on with when
    /// the month's hours are used up; `None` if no model is downloaded.
    pub spare: Option<String>,
}

/// One tier of `VinPUT` Pro on offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProOffer {
    /// Which tier.
    pub tier: ProTier,
    /// What a month costs, in hundredths of the currency.
    pub monthly: u32,
    /// What the first month costs whoever is looking, likewise, where that
    /// is less than a month: zero for a month like any other.
    #[serde(default)]
    pub first: u32,
    /// What a year costs, likewise.
    pub yearly: u32,
    /// Seconds of speech a month comes with.
    pub seconds: u64,
}

impl Default for Pro {
    /// Nobody signed in, and the prices as they are until the server says.
    fn default() -> Self {
        Self {
            who: ProWho::SignedOut,
            code: String::new(),
            code_minutes: 10,
            email: String::new(),
            known: false,
            entitled: false,
            complimentary: false,
            status: String::new(),
            plan: None,
            tier: ProTier::Pro,
            period_end: 0,
            ends: false,
            ends_at: 0,
            ended_at: 0,
            grace_until: 0,
            used_seconds: 0,
            cap_seconds: 0,
            resets_at: 0,
            checked_minutes: None,
            doing: ProDoing::Nothing,
            problem: None,
            offers: vec![
                ProOffer {
                    tier: ProTier::Plus,
                    monthly: 250,
                    first: 0,
                    yearly: 2400,
                    seconds: 8 * 3600,
                },
                ProOffer {
                    tier: ProTier::Pro,
                    monthly: 500,
                    first: 0,
                    yearly: 4900,
                    seconds: 25 * 3600,
                },
            ],
            currency: "eur".to_owned(),
            open: true,
            spare: None,
        }
    }
}

/// The tiers of `VinPUT` Pro: the same recognition, with more or fewer
/// hours a month.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProTier {
    /// The larger allowance.
    #[default]
    Pro,
    /// The smaller one.
    Plus,
}

sent_as_number!(ProTier: Pro, Plus);

/// Whether anyone is signed in to `VinPUT` Pro.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProWho {
    /// Nobody.
    #[default]
    SignedOut,
    /// A sign-in is waiting to be finished in the browser.
    SigningIn,
    /// Someone is.
    SignedIn,
}

sent_as_number!(ProWho: SignedOut, SigningIn, SignedIn);

/// The plans of `VinPUT` Pro.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProPlan {
    /// Paid by the month.
    Monthly,
    /// Paid by the year.
    Yearly,
}

sent_as_number!(ProPlan: Monthly, Yearly);

/// What is under way with the account of `VinPUT` Pro.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProDoing {
    /// Nothing.
    #[default]
    Nothing,
    /// The server is being asked how the account stands.
    Asking,
    /// A page of the payment service is being opened in the browser.
    Opening,
    /// A checkout is open in the browser and the payment is waited for.
    AwaitingPayment,
}

sent_as_number!(ProDoing: Nothing, Asking, Opening, AwaitingPayment);

/// Something to tell the user about the last thing they asked for.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Notice {
    /// How serious it is.
    pub kind: NoticeKind,
    /// What to say.
    pub text: String,
    /// The page it is about, so only that page shows it.
    pub page: Page,
}

/// How serious a [`Notice`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NoticeKind {
    /// For information.
    #[default]
    Info,
    /// It worked.
    Success,
    /// It did not work.
    Error,
}

/// What the background process tells a window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Event {
    /// Everything to show, replacing what was shown before.
    State(Box<View>),
    /// Turn the Settings window to this page and bring it to the front. The
    /// update window, which has no pages, only comes to the front.
    Show(Page),
    /// Close the window.
    Close,
}

/// What the user did in a window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Command {
    /// The window is up and wants its first [`Event::State`].
    Ready,
    /// A setting was changed.
    Set(Change),
    /// A hotkey field took or lost the focus. While one has it, the
    /// background process lets go of its hotkeys so the field can see them.
    CapturingHotkey(bool),
    /// Dictate with this model from now on.
    UseModel(String),
    /// Download this model.
    InstallModel(String),
    /// Stop downloading this model.
    CancelInstall(String),
    /// Delete this model from the PC, and from the list if the user added it.
    RemoveModel(String),
    /// Look at what a Hugging Face link or a path on this PC offers.
    FindModel(String),
    /// Ask the user for a model file with the Windows file picker, then look
    /// at it like [`Command::FindModel`].
    PickModelFile,
    /// Add the file at this index of [`Finder::files`] and download it.
    AddModel(usize),
    /// Forget what the finder found, on leaving the page.
    ClearFinder,
    /// Something done in the setup of a Google service.
    Google(Setup),
    /// Ask the user for the voice memo folder with the Windows folder picker.
    BrowseMemoDir,
    /// Open the voice memo folder in Explorer.
    OpenMemoDir,
    /// Open the folder the models are kept in.
    OpenModelsDir,
    /// Look for a newer version now.
    CheckForUpdates,
    /// Show the update window for the release that was found.
    ShowUpdate,
    /// Download and install the release.
    InstallUpdate,
    /// Stop the download of the release.
    CancelUpdate,
    /// The user has read the [`Notice`]; stop showing it.
    DismissNotice,
    /// Open a web page in the browser. Only `https://` addresses are opened.
    OpenLink(String),
    /// Start signing in to `VinPUT` Pro: the browser opens for it.
    ProSignIn,
    /// Stop waiting for a sign-in, or for a payment.
    ProCancel,
    /// Open the checkout for this plan of this tier in the browser.
    ProSubscribe(ProTier, ProPlan),
    /// Open the page on which the subscription is managed in the browser.
    ProManage,
    /// Ask the server again how the account stands.
    ProRefresh,
    /// The page of `VinPUT` Pro is in sight: what it shows is brought up to
    /// date, the prices included.
    ProShown,
    /// Sign out of `VinPUT` Pro.
    ProSignOut,
    /// The welcome is over, with or without a model chosen: it is not
    /// shown again.
    WelcomeDone,
    /// The window was closed.
    Closed,
}

impl Event {
    /// This event as one line of JSON, without the line break.
    #[must_use]
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Reads a line written by [`Event::to_line`]. `None` if it is not one.
    #[must_use]
    pub fn from_line(line: &str) -> Option<Self> {
        serde_json::from_str(line.trim()).ok()
    }
}

impl Command {
    /// This command as one line of JSON, without the line break.
    #[must_use]
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Reads a line written by [`Command::to_line`]. `None` if it is not one.
    #[must_use]
    pub fn from_line(line: &str) -> Option<Self> {
        serde_json::from_str(line.trim()).ok()
    }
}

/// What a window that only asks or tells shows: setup, a question, a message.
/// These run inside the process that needs the answer, with no engine behind
/// them.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Dialog {
    /// The title of the window.
    pub title: String,
    /// The line in large type.
    pub heading: String,
    /// The line under it.
    pub detail: String,
    /// Points listed under that, a line each.
    pub lines: Vec<String>,
    /// Caption of the button that says yes.
    pub primary: String,
    /// Caption of the button that says no; empty for a window with one button.
    pub secondary: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_and_commands_survive_the_wire() {
        let view = View {
            version: "0.3.0".to_owned(),
            status: Status {
                listening: true,
                model: Some("Whisper base".to_owned()),
                runs_on: RunsOn::GraphicsCard("NVIDIA GeForce RTX 3090".to_owned()),
            },
            models: Models {
                list: vec![Model {
                    id: "ggml-base".to_owned(),
                    state: ModelState::Installing { done: 1, total: 2 },
                    ..Model::default()
                }],
                ..Models::default()
            },
            ..View::default()
        };
        for event in [
            Event::State(Box::new(view)),
            Event::Show(Page::Models),
            Event::Close,
        ] {
            let line = event.to_line();
            assert!(!line.contains('\n'));
            assert_eq!(Event::from_line(&line), Some(event));
        }
        for command in [
            Command::Ready,
            Command::Set(Change::Hotkey("Ctrl+Shift+Space".to_owned())),
            Command::Set(Change::Autostart(false)),
            Command::AddModel(2),
            Command::ProSubscribe(ProTier::Plus, ProPlan::Yearly),
            Command::Google(Setup::Open(GoogleService::Speech)),
            Command::Google(Setup::Key("typed".to_owned())),
            Command::Google(Setup::Step(GoogleStep::Enter)),
            Command::Google(Setup::Close),
            Command::Google(Setup::Replace),
            Command::ProSubscribe(ProTier::Pro, ProPlan::Monthly),
            Command::ProSignIn,
            Command::ProShown,
            Command::WelcomeDone,
            Command::Closed,
        ] {
            let line = command.to_line();
            assert_eq!(Command::from_line(&line), Some(command));
        }
    }

    #[test]
    fn the_account_and_the_setup_survive_the_wire_as_numbers() {
        let failed = |problem| GoogleCheck::Failed {
            problem,
            said: "HTTP 503".to_owned(),
        };
        let mut checks = vec![GoogleCheck::Idle, GoogleCheck::Checking, GoogleCheck::Works];
        checks.extend(
            [
                GoogleProblem::WrongKey,
                GoogleProblem::NoAccess,
                GoogleProblem::Billing,
                GoogleProblem::Quota,
                GoogleProblem::NotFound,
                GoogleProblem::NoSignIn,
                GoogleProblem::BadFile,
                GoogleProblem::Offline,
                GoogleProblem::Other,
            ]
            .map(failed),
        );
        for check in checks {
            for (step, service) in [
                (GoogleStep::Service, GoogleService::Gemini),
                (GoogleStep::Get, GoogleService::Speech),
                (GoogleStep::Enter, GoogleService::Gemini),
                (GoogleStep::Done, GoogleService::Speech),
            ] {
                let google = Google {
                    step,
                    service,
                    check: check.clone(),
                    key_length: 39,
                    replacing: true,
                    ..Google::default()
                };
                let line = serde_json::to_string(&google).unwrap_or_default();
                assert_eq!(serde_json::from_str::<Google>(&line).ok(), Some(google));
            }
        }
        for (who, doing, plan) in [
            (ProWho::SignedOut, ProDoing::Nothing, None),
            (ProWho::SigningIn, ProDoing::Asking, Some(ProPlan::Monthly)),
            (ProWho::SignedIn, ProDoing::Opening, Some(ProPlan::Yearly)),
            (ProWho::SignedIn, ProDoing::AwaitingPayment, None),
        ] {
            let pro = Pro {
                who,
                doing,
                plan,
                email: "ana@example.com".to_owned(),
                status: "past_due".to_owned(),
                tier: if plan.is_some() {
                    ProTier::Plus
                } else {
                    ProTier::Pro
                },
                spare: plan.map(|_| "Whisper base".to_owned()),
                complimentary: who == ProWho::SignedIn,
                grace_until: 1_794_097_014,
                checked_minutes: Some(7),
                problem: Some("No connection.".to_owned()),
                ..Pro::default()
            };
            let line = serde_json::to_string(&pro).unwrap_or_default();
            assert!(
                !line.contains("SignedIn") && !line.contains("Yearly") && !line.contains("Plus"),
                "{line}"
            );
            assert_eq!(serde_json::from_str::<Pro>(&line).ok(), Some(pro));
        }
        // A number that names nothing reads as the first choice, and a
        // record with nothing in it as nobody signed in at the usual prices.
        assert_eq!(
            serde_json::from_str::<ProDoing>("99").ok(),
            Some(ProDoing::Nothing)
        );
        assert_eq!(serde_json::from_str::<Pro>("{}").ok(), Some(Pro::default()));
        let offers = Pro::default().offers;
        let offers = offers.iter();
        let offers: Vec<_> = offers
            .map(|offer| (offer.tier, offer.monthly, offer.yearly, offer.seconds))
            .collect();
        assert_eq!(
            offers,
            [
                (ProTier::Plus, 250, 2400, 28_800),
                (ProTier::Pro, 500, 4900, 90_000)
            ]
        );
    }

    #[test]
    fn a_view_from_another_version_still_reads() {
        // A field this version does not know, and most fields missing.
        let line = r#"{"state":{"version":"9.9.9","new_thing":1}}"#;
        let expected = View {
            version: "9.9.9".to_owned(),
            ..View::default()
        };
        assert_eq!(
            Event::from_line(line),
            Some(Event::State(Box::new(expected)))
        );
        assert_eq!(Event::from_line("not json"), None);
    }
}
