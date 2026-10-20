//! What Voutput shows: the keys that read a selection aloud, and the two
//! choices behind them, the voice that speaks and the model that sums up.
//!
//! The two are picked apart from each other, and each can be on this PC, at
//! a service the user has a key for, or at a server of their own. A choice
//! on this PC says how it would run here ([`crate::Fit`]), as the models of
//! Vinput do.

use serde::{Deserialize, Serialize};

use crate::{Fit, ModelState};

/// Everything the pages of Voutput show.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Reader {
    /// The settings as they are in force.
    pub settings: ReaderSettings,
    /// The voices, in the order to list them.
    pub voices: Vec<Pick>,
    /// The models that sum a text up, in the order to list them.
    pub summaries: Vec<Pick>,
    /// The speakers the voice in use can speak as, the usual one first;
    /// empty for a voice that has one.
    pub speakers: Vec<String>,
    /// Whether something is being read aloud right now.
    pub speaking: bool,
    /// Whether a Google API key is in the Windows credential store. The key
    /// itself never leaves the background process.
    pub google_key_stored: bool,
    /// Name of the graphics card that can run local models, if the PC has
    /// one.
    pub graphics_card: Option<String>,
    /// Bytes the downloaded voices, models and their engines take on disk.
    pub disk_bytes: u64,
}

/// The settings of Voutput. Hotkeys are text such as `F6`; an empty one is
/// not set.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
#[expect(clippy::struct_excessive_bools, reason = "independent on/off switches")]
pub struct ReaderSettings {
    /// Key that reads the selected text aloud.
    pub hotkey_read: String,
    /// Key that sums the selected text up and reads the summary.
    pub hotkey_summary: String,
    /// How fast the voice speaks, in hundredths of its usual pace.
    pub speed: u32,
    /// The speaker the voice in use speaks as: one of
    /// [`Reader::speakers`], or empty for a voice that has one.
    pub speaker: String,
    /// How long a summary is.
    pub summary_length: Length,
    /// How a summary is written: what the model that sums up is told,
    /// the length aside. The words Voutput comes with until the user
    /// writes their own.
    pub summary_instructions: String,
    /// Whether [`Self::summary_instructions`] are the user's own.
    pub summary_instructions_own: bool,
    /// Whether Voutput starts with Windows.
    pub autostart: bool,
    /// Whether Voutput looks for newer versions by itself.
    pub check_updates: bool,
    /// Whether local models run on the graphics card.
    pub graphics_card: bool,
    /// Whether the model that sums up is unloaded when no summary was asked
    /// for in a while. Without it the model stays loaded.
    pub free_memory_when_idle: bool,
    /// The address of a server of the user's own that sums up, in the form
    /// most local servers speak: `http://127.0.0.1:1234/v1`.
    pub endpoint_url: String,
    /// The name of the model to ask that server for.
    pub endpoint_model: String,
}

/// How long a summary is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Length {
    /// Two or three sentences.
    #[default]
    Short,
    /// About half a minute of speech.
    Medium,
}

/// Where a voice or a model runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Place {
    /// On this PC, after a download.
    #[default]
    ThisPc,
    /// On this PC, as a part of Windows: nothing to download.
    Windows,
    /// At Google, with the user's own key.
    OwnKey,
    /// At a server of the user's own.
    Endpoint,
    /// At the service that comes with a subscription.
    Cloud,
}

/// How far a voice or a model is on the way to being ready to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Busy {
    /// It is not being made ready.
    #[default]
    No,
    /// It is, and there is no saying how far.
    Moving,
    /// It is: so many hundredths are done.
    Part(u8),
}

/// One voice, or one model that sums up.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Pick {
    /// What [`ReaderAsk`] names it by.
    pub id: String,
    /// Its name.
    pub name: String,
    /// One line about what it is good at.
    pub summary: String,
    /// What the list says beside its size: its languages, or what it needs.
    pub detail: String,
    /// Where it runs.
    pub place: Place,
    /// Bytes its download takes; zero if it has none.
    pub bytes: u64,
    /// Whether it is the one in use.
    pub in_use: bool,
    /// Whether it is announced and cannot be chosen yet.
    pub soon: bool,
    /// How it would run on this PC, for one that runs here on a PC that
    /// could be looked over.
    pub fit: Option<Fit>,
    /// What to expect of it on this PC, in a few words: `About 10 s a page`.
    /// Empty where there is nothing to say.
    pub pace: String,
    /// Where it runs and what it is doing right now, for the one in use
    /// that has something to say: `Ready on the graphics card`. Empty
    /// otherwise.
    pub status: String,
    /// Whether it is being made ready right now, and how far that is.
    pub busy: Busy,
    /// Where it stands.
    pub state: ModelState,
}

/// One setting changed by the user. Each takes effect at once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReaderChange {
    /// See [`ReaderSettings::hotkey_read`].
    HotkeyRead(String),
    /// See [`ReaderSettings::hotkey_summary`].
    HotkeySummary(String),
    /// See [`ReaderSettings::speed`]. A pace that is not on offer is left
    /// unmade.
    Speed(u32),
    /// See [`ReaderSettings::speaker`]. A speaker the voice in use does not
    /// have is left unmade.
    Speaker(String),
    /// See [`ReaderSettings::summary_length`].
    SummaryLength(Length),
    /// See [`ReaderSettings::summary_instructions`]. Empty puts the words
    /// Voutput comes with back.
    SummaryInstructions(String),
    /// See [`ReaderSettings::autostart`].
    Autostart(bool),
    /// See [`ReaderSettings::check_updates`].
    CheckUpdates(bool),
    /// See [`ReaderSettings::graphics_card`].
    GraphicsCard(bool),
    /// See [`ReaderSettings::free_memory_when_idle`].
    FreeMemoryWhenIdle(bool),
    /// See [`ReaderSettings::endpoint_url`].
    EndpointUrl(String),
    /// See [`ReaderSettings::endpoint_model`].
    EndpointModel(String),
}

/// What the user asks of Voutput in a window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReaderAsk {
    /// A setting was changed.
    Set(ReaderChange),
    /// Speak with this voice from now on.
    UseVoice(String),
    /// Sum up with this model from now on.
    UseSummary(String),
    /// Download this voice or model.
    Install(String),
    /// Stop downloading it.
    CancelInstall(String),
    /// Delete it from the PC.
    Remove(String),
    /// Keep this Google API key in the Windows credential store.
    SaveGoogleKey(String),
    /// Forget the Google API key.
    ForgetGoogleKey,
    /// Open the folder the voices and models are kept in.
    OpenFolder,
    /// Say a sentence with the voice in use, to hear it.
    Sample,
    /// Stop what is being read aloud.
    Stop,
}
