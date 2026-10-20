//! User settings, stored as a small `key = value` text file.
//!
//! The file survives upgrades in both directions: keys this version does not
//! know are kept as they are and written back, and keys the file does not have
//! take their default. Secrets such as API keys are never stored here; they
//! live in the Windows credential store (see [`crate::secrets`]).

use std::path::{Path, PathBuf};

pub use putlab_core::folders::{data_dir, local_dir};

use crate::actions::{self, Action};
use crate::keys::Hotkey;
use crate::rewrite::Mode;
use crate::stt::{GoogleConfig, ProviderKind};
use crate::words::Words;
use crate::{Error, Result, search};

/// The Google Cloud Speech-to-Text location the setup in Settings starts
/// from, and the one used while the settings name none: the European Union.
pub const DEFAULT_LOCATION: &str = "eu";

/// The environment variable that, before 0.3.3, named the Google Cloud
/// project while the settings file named none. Nothing reads it any more
/// but [`Settings::adopt_project`], which writes what it holds into the
/// settings file once, so that a setup that rested on it keeps working.
pub const ENV_PROJECT: &str = "VINPUT_GCP_PROJECT";

const VK_F1: u16 = 0x70;
const VK_F2: u16 = 0x71;

/// The times a dictation may be given before it stops for want of a sound,
/// in minutes: what `stop_after_silence` in the file may say.
pub const STOP_AFTER_MINUTES: [u32; 5] = [1, 5, 15, 30, 60];
/// The one in use until another is chosen. Long enough to read or think for
/// a while in the middle of a dictation and find it as it was left.
pub const DEFAULT_STOP_AFTER_MINUTES: u32 = 30;

/// Everything the user can configure.
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(clippy::struct_excessive_bools, reason = "independent on/off switches")]
pub struct Settings {
    /// Key that dictates into the focused window.
    pub hotkey: Hotkey,
    /// Key that dictates and then presses Enter, if set.
    pub hotkey_enter: Option<Hotkey>,
    /// Key that dictates a web search, if set.
    pub hotkey_search: Option<Hotkey>,
    /// Key that records a voice memo, if set.
    pub hotkey_memo: Option<Hotkey>,
    /// Key that edits the selected text the way it is told to, if set.
    pub hotkey_command: Option<Hotkey>,
    /// What is done with a dictation of [`hotkey`](Self::hotkey) before it
    /// is typed.
    pub cleanup: Mode,
    /// The same for a dictation of [`hotkey_enter`](Self::hotkey_enter).
    pub cleanup_enter: Mode,
    /// The model that tidies and edits: empty for the one that comes with
    /// `VinPUT` Plus and Pro, or the id of one on this PC
    /// ([`crate::models::writers`]).
    pub text_model: String,
    /// BCP-47 language codes to recognize.
    pub languages: Vec<String>,
    /// Whether Vinput starts when the user signs in to Windows.
    pub autostart: bool,
    /// Whether a short sound plays when Vinput starts listening.
    pub ready_sound: bool,
    /// Whether a short sound plays when Vinput stops listening.
    pub stop_sound: bool,
    /// Whether words are typed as they are said, before the recognizer has
    /// settled them, and corrected in place when it changes its mind. Only
    /// `VinPUT` Pro makes the guesses this types.
    pub live_typing: bool,
    /// After how many minutes without a sound a dictation stops by itself:
    /// one of [`STOP_AFTER_MINUTES`].
    pub stop_after_minutes: u32,
    /// The microphone to listen with, as Windows identifies it. Empty means
    /// the one Windows uses by default, whichever that is at the time.
    pub microphone: String,
    /// Speech service that does the recognition.
    pub provider: ProviderKind,
    /// Recognition model, as the provider names it. Empty until the user has
    /// chosen one: Vinput comes with no model picked for them.
    pub model: String,
    /// Google Cloud project to bill, once the user has named theirs.
    pub project_id: Option<String>,
    /// Google Cloud Speech-to-Text location. Empty until the user has chosen
    /// one; see [`DEFAULT_LOCATION`].
    pub location: String,
    /// Whether local models run on the graphics card when the PC has one.
    pub local_gpu: bool,
    /// Whether other programs may connect to the local API.
    pub api_enabled: bool,
    /// Whether programs connected to the local API are sent the text of
    /// dictations that are typed, as it is typed. Off until the user
    /// switches it on: what is dictated is nobody else's business.
    pub api_transcript: bool,
    /// Folder for voice memos. Empty means the default, see [`crate::memo`].
    pub memo_dir: String,
    /// Whether a saved voice memo is put on the clipboard, ready to paste.
    pub memo_to_clipboard: bool,
    /// Whether Vinput looks for a newer version of itself.
    pub check_updates: bool,
    /// The web search a dictated search opens, with `%s` for what was said;
    /// see [`crate::search`].
    pub search_url: String,
    /// The model on this PC that was in use when an online one was chosen
    /// in its place; empty if none was. It is what `VinPUT` Pro carries on
    /// with when the month's hours are used up, if it is still here.
    pub last_local_model: String,
    /// Whether the welcome is still to be shown: set when Vinput starts
    /// without a settings file, which only someone who has just installed
    /// it does, and cleared once they have chosen a model or said later.
    /// A file that does not say has it off, so an update never shows it.
    pub first_run: bool,
    /// The dictionary, the snippets, and whether spoken formatting is
    /// listened for; see [`crate::words`].
    pub words: Words,
    /// The actions of the user's own, in the order they were added: keys
    /// that hand what was said to a program; see [`crate::actions`].
    pub actions: Vec<Action>,
    /// Lines with keys this version does not know, kept so that saving does
    /// not throw away what another version or program wrote.
    pub extra: Vec<(String, String)>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkey: Hotkey::plain(VK_F1),
            hotkey_enter: Some(Hotkey::plain(VK_F2)),
            hotkey_search: None,
            hotkey_memo: None,
            hotkey_command: None,
            cleanup: Mode::Plain,
            cleanup_enter: Mode::Plain,
            text_model: String::new(),
            languages: vec!["en-US".to_owned()],
            autostart: true,
            ready_sound: true,
            stop_sound: true,
            live_typing: true,
            stop_after_minutes: DEFAULT_STOP_AFTER_MINUTES,
            microphone: String::new(),
            provider: ProviderKind::default(),
            model: String::new(),
            project_id: None,
            location: String::new(),
            local_gpu: true,
            api_enabled: true,
            api_transcript: false,
            memo_dir: String::new(),
            memo_to_clipboard: true,
            check_updates: true,
            search_url: search::DEFAULT_TEMPLATE.to_owned(),
            last_local_model: String::new(),
            first_run: false,
            words: Words::default(),
            actions: Vec::new(),
            extra: Vec::new(),
        }
    }
}

/// The part of the settings that decides which speech provider is set up and
/// how. When it changes, the provider is set up again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeechConfig {
    /// Speech service that does the recognition.
    pub provider: ProviderKind,
    /// Recognition model, as the provider names it.
    pub model: String,
    /// Google Cloud project to bill.
    pub project_id: Option<String>,
    /// Google Cloud Speech-to-Text location.
    pub location: String,
    /// Whether local models run on an NVIDIA graphics card.
    pub local_gpu: bool,
}

impl Settings {
    /// Parses the file format. Unparsable values are ignored, so a damaged file
    /// degrades to defaults instead of stopping the app.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut settings = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            if key.starts_with('#') {
                continue;
            }
            let flag = || value.eq_ignore_ascii_case("true");
            match key {
                "hotkey" => {
                    if let Ok(hotkey) = value.parse() {
                        settings.hotkey = hotkey;
                    }
                }
                "hotkey_enter" => set_optional_hotkey(&mut settings.hotkey_enter, value),
                "hotkey_search" => set_optional_hotkey(&mut settings.hotkey_search, value),
                "hotkey_memo" => set_optional_hotkey(&mut settings.hotkey_memo, value),
                "hotkey_command" => set_optional_hotkey(&mut settings.hotkey_command, value),
                // A word this version does not know leaves the dictation
                // as it is said, which is what every version can do.
                "cleanup" => settings.cleanup = value.parse().unwrap_or_default(),
                "cleanup_enter" => settings.cleanup_enter = value.parse().unwrap_or_default(),
                "text_model" => value.clone_into(&mut settings.text_model),
                "languages" => {
                    let languages = parse_languages(value);
                    if !languages.is_empty() {
                        settings.languages = languages;
                    }
                }
                "autostart" => settings.autostart = flag(),
                "ready_sound" => settings.ready_sound = flag(),
                "stop_sound" => settings.stop_sound = flag(),
                "live_typing" => settings.live_typing = flag(),
                "stop_after_silence" => {
                    if let Some(minutes) = value
                        .parse()
                        .ok()
                        .filter(|minutes| STOP_AFTER_MINUTES.contains(minutes))
                    {
                        settings.stop_after_minutes = minutes;
                    }
                }
                "microphone" => value.clone_into(&mut settings.microphone),
                "provider" => {
                    if let Ok(provider) = value.parse() {
                        settings.provider = provider;
                    }
                }
                "model" => {
                    if !value.is_empty() {
                        value.clone_into(&mut settings.model);
                    }
                }
                "project_id" => {
                    settings.project_id = Some(value.to_owned()).filter(|v| !v.is_empty());
                }
                "location" => {
                    if !value.is_empty() {
                        value.clone_into(&mut settings.location);
                    }
                }
                "graphics_card" => settings.local_gpu = flag(),
                // What versions before 0.2.4 wrote. It meant the 670 MB build
                // for NVIDIA cards, off unless asked for; it is not carried on.
                "local_gpu" => {}
                "api_enabled" => settings.api_enabled = flag(),
                "api_transcript" => settings.api_transcript = flag(),
                "memo_dir" => value.clone_into(&mut settings.memo_dir),
                "memo_to_clipboard" => settings.memo_to_clipboard = flag(),
                "check_updates" => settings.check_updates = flag(),
                "search_url" => {
                    if search::is_valid(value) {
                        value.clone_into(&mut settings.search_url);
                    }
                }
                "last_local_model" => value.clone_into(&mut settings.last_local_model),
                "first_run" => settings.first_run = flag(),
                "action" if settings.actions.len() < actions::MAX_ACTIONS => {
                    settings.actions.extend(Action::parse(value));
                }
                _ if settings.words.read(key, value) => {}
                _ => settings.extra.push((key.to_owned(), value.to_owned())),
            }
        }
        settings
    }

    /// Renders the settings in the format [`Settings::parse`] reads.
    #[must_use]
    pub fn serialize(&self) -> String {
        let mut text = format!(
            "# Vinput settings. Edit here or from the tray icon's Settings window.\n\
             hotkey = {}\n\
             hotkey_enter = {}\n\
             hotkey_memo = {}\n\
             languages = {}\n\
             autostart = {}\n\
             ready_sound = {}\n\
             provider = {}\n\
             model = {}\n\
             project_id = {}\n\
             location = {}\n\
             graphics_card = {}\n\
             api_enabled = {}\n\
             memo_dir = {}\n\
             memo_to_clipboard = {}\n\
             check_updates = {}\n\
             hotkey_search = {}\n\
             search_url = {}\n\
             microphone = {}\n\
             last_local_model = {}\n\
             stop_sound = {}\n\
             stop_after_silence = {}\n\
             live_typing = {}\n\
             hotkey_command = {}\n\
             cleanup = {}\n\
             cleanup_enter = {}\n\
             text_model = {}\n\
             api_transcript = {}\n",
            self.hotkey,
            optional_hotkey(self.hotkey_enter),
            optional_hotkey(self.hotkey_memo),
            self.languages.join(", "),
            self.autostart,
            self.ready_sound,
            self.provider,
            self.model,
            self.project_id.as_deref().unwrap_or_default(),
            self.location,
            self.local_gpu,
            self.api_enabled,
            self.memo_dir,
            self.memo_to_clipboard,
            self.check_updates,
            optional_hotkey(self.hotkey_search),
            self.search_url,
            self.microphone,
            self.last_local_model,
            self.stop_sound,
            self.stop_after_minutes,
            self.live_typing,
            optional_hotkey(self.hotkey_command),
            self.cleanup,
            self.cleanup_enter,
            self.text_model,
            self.api_transcript,
        );
        self.words.write(&mut text);
        for action in &self.actions {
            text.push_str("action = ");
            text.push_str(&action.line());
            text.push('\n');
        }
        // Written only while it is on: the file of someone who was never
        // owed a welcome, or has had it, says nothing of it.
        if self.first_run {
            text.push_str(
                "first_run = true
",
            );
        }
        for (key, value) in &self.extra {
            text.push_str(key);
            text.push_str(" = ");
            text.push_str(value);
            text.push('\n');
        }
        text
    }

    /// Where the settings file lives: `%APPDATA%\Vinput\settings.ini`.
    #[must_use]
    pub fn path() -> Option<PathBuf> {
        Some(data_dir()?.join("settings.ini"))
    }

    /// Loads the settings file, or defaults if it cannot be read. Without a
    /// file this is the first start after a new install, and the settings
    /// say so ([`Settings::first_run`]); such a start is set to the
    /// languages Windows is set to.
    #[must_use]
    pub fn load() -> Self {
        // A test must not depend on the PC it runs on.
        let windows: &[String] = if crate::instance::name() == Some(crate::instance::TESTS) {
            &[]
        } else {
            crate::languages::preferred()
        };
        Self::path().map_or_else(Self::default, |path| Self::load_from(&path, windows))
    }

    /// The settings in the file at `path`, as [`load`](Self::load) reads
    /// them on a PC whose Windows is set to the languages `windows`.
    fn load_from(path: &Path, windows: &[String]) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text),
            // Only a file that is not there says "new here". One that is
            // there and cannot be read belongs to someone who has settings.
            Err(err) => {
                let mut settings = Self {
                    first_run: err.kind() == std::io::ErrorKind::NotFound,
                    ..Self::default()
                };
                let spoken = spoken_here(windows);
                if settings.first_run && !spoken.is_empty() {
                    settings.languages = spoken;
                }
                settings
            }
        }
    }

    /// Whether the welcome is to be shown: this is a new install, and no
    /// model was chosen yet.
    #[must_use]
    pub fn owes_welcome(&self) -> bool {
        self.first_run && !self.has_model()
    }

    /// Writes the settings file, creating its folder if needed. The file is
    /// written beside the old one and renamed over it, so a crash or a full
    /// disk never leaves half a file behind.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Host`] if the file cannot be written.
    pub fn save(&self) -> Result<()> {
        let path = Self::path().ok_or_else(|| Error::Host("APPDATA is not set".into()))?;
        self.save_to(&path)
    }

    /// Writes the settings to `path`, as [`save`](Self::save) does.
    fn save_to(&self, path: &Path) -> Result<()> {
        let write = || {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let temporary = path.with_extension("ini.tmp");
            std::fs::write(&temporary, self.serialize())?;
            std::fs::rename(&temporary, path)
        };
        write().map_err(|err| Error::Host(format!("saving {}: {err}", path.display())))
    }

    /// Carries a Google Cloud project that was only ever named by
    /// [`ENV_PROJECT`] over into the settings file, where it is read from
    /// now. Done when Vinput starts, without a word: it happens once, since
    /// from then on the file names a project, and never touches a project
    /// the file names already.
    pub fn adopt_project() {
        if let Some(path) = Self::path() {
            Self::adopt_project_at(&path, std::env::var(ENV_PROJECT).ok().as_deref());
        }
    }

    /// The rule of [`adopt_project`](Self::adopt_project), for the settings
    /// file at `path` and what the variable holds. Says whether the file
    /// was written.
    fn adopt_project_at(path: &Path, named: Option<&str>) -> bool {
        let Some(project) = named
            .map(str::trim)
            .filter(|project| crate::stt::check::is_project(project))
        else {
            return false;
        };
        let mut settings = std::fs::read_to_string(path)
            .map_or_else(|_| Self::default(), |text| Self::parse(&text));
        if settings.project_id.is_some() {
            return false;
        }
        settings.project_id = Some(project.to_owned());
        settings
            .save_to(path)
            .map_err(|err| crate::log::write(&err.to_string()))
            .is_ok()
    }

    /// Whether the user has chosen a speech model yet.
    #[must_use]
    pub fn has_model(&self) -> bool {
        !self.model.is_empty()
    }

    /// The part of these settings that decides how speech is recognized.
    #[must_use]
    pub fn speech_config(&self) -> SpeechConfig {
        SpeechConfig {
            provider: self.provider,
            model: self.model.clone(),
            project_id: self.project_id.clone(),
            location: self.location.clone(),
            local_gpu: self.use_gpu(),
        }
    }

    /// Whether local models are to run on a graphics card: the user wants
    /// it, which is the default, and the PC has one.
    #[must_use]
    pub fn use_gpu(&self) -> bool {
        self.local_gpu && crate::gpu::card().is_some()
    }

    /// The Google Cloud configuration these settings describe.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Setup`] if no project is configured.
    pub fn google_config(&self) -> Result<GoogleConfig> {
        let project_id = self.project_id.clone().ok_or_else(|| {
            Error::Setup(
                "no Google Cloud project is set; set Google Cloud Speech up under Settings, Models"
                    .into(),
            )
        })?;
        let location = if self.location.is_empty() {
            DEFAULT_LOCATION
        } else {
            &self.location
        };
        Ok(GoogleConfig {
            project_id,
            location: location.to_owned(),
            model: self.model.clone(),
            language_codes: self.languages.clone(),
        })
    }
}

/// Reads an optional hotkey: empty switches it off, nonsense leaves it alone.
fn set_optional_hotkey(slot: &mut Option<Hotkey>, value: &str) {
    if value.is_empty() {
        *slot = None;
    } else if let Ok(hotkey) = value.parse() {
        *slot = Some(hotkey);
    }
}

fn optional_hotkey(hotkey: Option<Hotkey>) -> String {
    hotkey.map(|hotkey| hotkey.to_string()).unwrap_or_default()
}

/// The most languages a new install is set to by itself.
const LANGUAGES_AT_FIRST: usize = 3;

/// The languages a new install dictates in, drawn from those Windows is set
/// to: the ones that name a language and a country, such as `sl-SI`, one
/// for each language, the most wanted first. Whoever speaks others names
/// them in Settings.
fn spoken_here(windows: &[String]) -> Vec<String> {
    let letters = |part: &str, least: usize, most: usize| {
        (least..=most).contains(&part.len()) && part.chars().all(|c| c.is_ascii_alphabetic())
    };
    let mut spoken: Vec<String> = Vec::new();
    for code in windows {
        let Some((language, country)) = code.trim().split_once('-') else {
            continue;
        };
        if !letters(language, 2, 3) || !letters(country, 2, 2) {
            continue;
        }
        let language = language.to_ascii_lowercase();
        let known = |code: &String| code.split('-').next() == Some(language.as_str());
        if !spoken.iter().any(known) {
            spoken.push(format!("{language}-{}", country.to_ascii_uppercase()));
        }
        if spoken.len() == LANGUAGES_AT_FIRST {
            break;
        }
    }
    spoken
}

/// Splits a comma-separated list of language codes, dropping blanks.
#[must_use]
pub fn parse_languages(list: &str) -> Vec<String> {
    list.split(',')
        .map(str::trim)
        .filter(|code| !code.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_survive_a_round_trip() {
        let settings = Settings::default();
        assert_eq!(Settings::parse(&settings.serialize()), settings);
    }

    #[test]
    fn a_dictation_is_stopped_after_one_of_the_times_on_offer_and_half_an_hour_otherwise() {
        assert_eq!(Settings::default().stop_after_minutes, 30);
        assert_eq!(Settings::parse("").stop_after_minutes, 30);
        for minutes in STOP_AFTER_MINUTES {
            let settings = Settings::parse(&format!("stop_after_silence = {minutes}"));
            assert_eq!(settings.stop_after_minutes, minutes);
        }
        for odd in ["0", "7", "-5", "soon", ""] {
            let settings = Settings::parse(&format!("stop_after_silence = {odd}"));
            assert_eq!(settings.stop_after_minutes, 30, "{odd:?}");
        }
    }

    #[test]
    fn custom_values_survive_a_round_trip() {
        let mut words = Words {
            commands: false,
            ..Words::default()
        };
        assert_eq!(words.add_word("VinPUT", "vin put"), Ok(true));
        assert_eq!(words.add_snippet("my email", "a = b | c"), Ok(true));
        let settings = Settings {
            hotkey: Hotkey {
                ctrl: true,
                ..Hotkey::plain(0x20)
            },
            hotkey_enter: None,
            hotkey_search: Some(Hotkey::plain(0x79)),
            hotkey_memo: Some(Hotkey::plain(0x78)),
            hotkey_command: Some(Hotkey::plain(0x77)),
            cleanup: Mode::Clean,
            cleanup_enter: Mode::Prompt,
            text_model: "gemma-4-e2b".into(),
            languages: vec!["sl-SI".into(), "en-US".into()],
            autostart: false,
            ready_sound: false,
            stop_sound: false,
            live_typing: false,
            stop_after_minutes: 5,
            microphone: r"{0.0.1.00000000}.{6c1f3d2e-0d3a-4c58-9a51-1f0f2b8e7a10}".into(),
            provider: ProviderKind::Local,
            model: "whisper-base".into(),
            project_id: Some("demo".into()),
            location: "us".into(),
            local_gpu: true,
            api_enabled: false,
            api_transcript: true,
            memo_dir: r"D:\Memos".into(),
            memo_to_clipboard: false,
            check_updates: false,
            search_url: "https://duckduckgo.com/?q=%s".into(),
            last_local_model: "whisper-base".into(),
            first_run: true,
            words,
            actions: vec![
                Action::default(),
                Action {
                    name: "Translate".into(),
                    hotkey: Some(Hotkey::plain(0x76)),
                    program: r"C:\Tools\translate.exe".into(),
                    arguments: "--to en {text}".into(),
                    then: actions::Then::Type,
                    cleanup: Mode::Clean,
                    timeout: 0,
                },
            ],
            extra: vec![("future_key".into(), "kept".into())],
        };
        assert_eq!(Settings::parse(&settings.serialize()), settings);
    }

    #[test]
    fn no_more_actions_are_read_than_there_can_be() {
        let line = format!("action = {}\n", Action::default().line());
        let settings = Settings::parse(&line.repeat(actions::MAX_ACTIONS + 3));
        assert_eq!(settings.actions.len(), actions::MAX_ACTIONS);
        // One that names no program is left out.
        assert!(Settings::parse("action = Broken | F9\n").actions.is_empty());
    }

    #[test]
    fn bad_lines_fall_back_to_defaults() {
        let settings = Settings::parse(
            "hotkey = Banana\nnonsense\nlanguages = ,\nmodel =\nprovider = banana\n\
             search_url = calc.exe\nhotkey_search = Banana\n",
        );
        assert_eq!(settings, Settings::default());
    }

    #[test]
    fn a_file_from_an_older_version_keeps_its_values_and_its_unknown_keys() {
        let old = "hotkey = F5\nhotkey_enter = \nhotkey_extra = F3\nextra_link = \n\
                   languages = en-US, sl-SI\nautostart = false\nready_sound = true\n\
                   provider = google-cloud\nproject_id = demo\nlocation = eu\nmodel = chirp_3\n";
        let settings = Settings::parse(old);
        assert_eq!(settings.hotkey, Hotkey::plain(0x74));
        assert_eq!(settings.hotkey_enter, None);
        assert_eq!(settings.languages, ["en-US", "sl-SI"]);
        assert!(!settings.autostart);
        // Settings added since then take their defaults.
        assert!(settings.api_enabled && settings.memo_to_clipboard);
        assert!(settings.live_typing);
        assert_eq!(settings.words, Words::default());
        assert_eq!(settings.hotkey_search, None);
        assert_eq!(settings.hotkey_command, None);
        assert_eq!(
            (settings.cleanup, settings.cleanup_enter),
            (Mode::Plain, Mode::Plain)
        );
        assert!(settings.text_model.is_empty());
        assert_eq!(settings.search_url, search::DEFAULT_TEMPLATE);
        assert!(settings.microphone.is_empty(), "the default microphone");
        // What this version does not know is written back untouched.
        let saved = settings.serialize();
        assert!(saved.contains("hotkey_extra = F3\n") && saved.contains("extra_link = \n"));
        assert_eq!(Settings::parse(&saved), settings);
    }

    #[test]
    fn a_dictation_is_typed_as_said_unless_a_way_to_tidy_it_is_named() {
        let settings = Settings::parse("cleanup = email\ncleanup_enter = PROMPT\n");
        assert_eq!(settings.cleanup, Mode::Email);
        assert_eq!(settings.cleanup_enter, Mode::Prompt);
        // A way a later version may add is not one this version has.
        let later = Settings::parse("cleanup = poem\ncleanup_enter = \n");
        assert_eq!(
            (later.cleanup, later.cleanup_enter),
            (Mode::Plain, Mode::Plain)
        );
        let saved = Settings::default().serialize();
        assert!(saved.contains("cleanup = plain\n") && saved.contains("text_model = \n"));
    }

    #[test]
    fn no_model_is_chosen_until_the_user_chooses_one() {
        assert!(!Settings::default().has_model());
        assert!(!Settings::parse("model = \n").has_model());
        let chosen = Settings::parse("provider = local\nmodel = whisper-base\n");
        assert!(chosen.has_model());
        assert!(Settings::parse(&chosen.serialize()).has_model());
    }

    #[test]
    fn the_graphics_card_is_used_unless_the_user_says_no() {
        assert!(Settings::default().local_gpu);
        // The switch of earlier versions meant something else and is dropped.
        let old = Settings::parse("local_gpu = false\n");
        assert!(old.local_gpu && old.extra.is_empty());
        assert!(!Settings::parse("graphics_card = false\n").local_gpu);
        assert!(old.serialize().contains("graphics_card = true\n"));
    }

    #[test]
    fn nothing_of_google_cloud_is_set_until_the_user_sets_it() {
        let fresh = Settings::default();
        assert_eq!(fresh.project_id, None);
        assert_eq!(fresh.location, "");
        assert!(fresh.google_config().is_err());
        // A project named in a file that says nothing of a location.
        let old = Settings::parse("provider = google-cloud\nproject_id = demo\nmodel = chirp_3\n");
        let config = old.google_config();
        assert!(config.is_ok(), "{config:?}");
        assert!(config.is_ok_and(|config| config.project_id == "demo" && config.location == "eu"));
        // What someone chose stays as it is.
        let chosen = Settings::parse("project_id = demo\nlocation = us\n");
        assert!(
            chosen
                .google_config()
                .is_ok_and(|config| config.location == "us")
        );
    }

    #[test]
    fn a_project_that_was_only_in_the_environment_moves_into_the_file_once() {
        let folder = std::env::temp_dir().join(format!("vinput-settings-{}", std::process::id()));
        let path = folder.join("settings.ini");
        let read = || std::fs::read_to_string(&path).map(|text| Settings::parse(&text));
        let _ = std::fs::remove_dir_all(&folder);
        assert!(std::fs::create_dir_all(&folder).is_ok());
        let before = "hotkey = F9\nprovider = google-cloud\nmodel = chirp_3\nlater_key = kept\n";
        assert!(std::fs::write(&path, before).is_ok());

        // Without the variable, or with nonsense in it, nothing is written.
        for named in [None, Some(""), Some("  "), Some("not a/project")] {
            assert!(!Settings::adopt_project_at(&path, named), "{named:?}");
        }
        assert_eq!(std::fs::read_to_string(&path).ok().as_deref(), Some(before));

        // With it, the project is in the file from now on, and everything
        // else in the file is as it was.
        assert!(Settings::adopt_project_at(&path, Some(" my-project ")));
        let Ok(adopted) = read() else {
            unreachable!("the settings file is there");
        };
        assert_eq!(adopted.project_id.as_deref(), Some("my-project"));
        assert_eq!(adopted.hotkey.to_string(), "F9");
        assert_eq!(adopted.model, "chirp_3");
        assert_eq!(adopted.extra, [("later_key".to_owned(), "kept".to_owned())]);
        assert!(adopted.google_config().is_ok());

        // Once: a project the file names is never replaced.
        assert!(!Settings::adopt_project_at(&path, Some("another-project")));
        assert_eq!(
            read()
                .ok()
                .and_then(|settings| settings.project_id)
                .as_deref(),
            Some("my-project")
        );

        // Someone who never had a settings file gets one that names it.
        let fresh = folder.join("fresh").join("settings.ini");
        assert!(Settings::adopt_project_at(&fresh, Some("my-project")));
        let fresh = std::fs::read_to_string(&fresh).map(|text| Settings::parse(&text));
        assert_eq!(
            fresh
                .ok()
                .and_then(|settings| settings.project_id)
                .as_deref(),
            Some("my-project")
        );
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn only_a_start_without_a_settings_file_owes_a_welcome() {
        let folder = std::env::temp_dir().join(format!("vinput-welcome-{}", std::process::id()));
        let path = folder.join("settings.ini");
        let _ = std::fs::remove_dir_all(&folder);

        // A new install: no file, so the welcome is owed, and the file
        // that is then written keeps saying so until it is over.
        let fresh = Settings::load_from(&path, &[]);
        assert!(fresh.first_run && fresh.owes_welcome());
        assert_eq!(fresh.languages, ["en-US"]);
        assert!(fresh.save_to(&path).is_ok());
        assert!(Settings::load_from(&path, &[]).owes_welcome());

        // Once a model is chosen nothing is owed, whatever the file says.
        let mut chosen = fresh.clone();
        "whisper-base".clone_into(&mut chosen.model);
        assert!(!chosen.owes_welcome());

        // Someone who has settings is never new: not with a file of an
        // older version, with or without a model, nor with an empty one.
        for old in [
            "hotkey = F5
provider = google-cloud
model = chirp_3
",
            "hotkey = F5
model = 
",
            "",
        ] {
            assert!(std::fs::write(&path, old).is_ok());
            let settings = Settings::load_from(&path, &[]);
            assert!(!settings.first_run && !settings.owes_welcome(), "{old:?}");
            // And saving such a file does not start to say anything of it.
            assert!(!settings.serialize().contains("first_run"));
        }

        // After the welcome the line is gone from the file.
        let mut done = fresh;
        done.first_run = false;
        assert!(done.save_to(&path).is_ok());
        assert!(!Settings::load_from(&path, &[]).first_run);
        assert!(std::fs::read_to_string(&path).is_ok_and(|text| !text.contains("first_run")));
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn a_new_install_is_set_to_the_languages_of_windows() {
        let windows = |codes: &[&str]| -> Vec<String> {
            codes.iter().map(|code| (*code).to_owned()).collect()
        };
        let folder = std::env::temp_dir().join(format!("vinput-spoken-{}", std::process::id()));
        let path = folder.join("settings.ini");
        let _ = std::fs::remove_dir_all(&folder);
        let fresh = Settings::load_from(&path, &windows(&["sl-SI", "en-US"]));
        assert!(fresh.first_run);
        assert_eq!(fresh.languages, ["sl-SI", "en-US"]);
        // Someone who has settings keeps theirs, whatever Windows says.
        assert!(Settings::default().save_to(&path).is_ok());
        let kept = Settings::load_from(&path, &windows(&["sl-SI"]));
        assert_eq!(kept.languages, ["en-US"]);
        let _ = std::fs::remove_dir_all(&folder);

        // One for each language, only what names a language and a country,
        // and never more than three.
        assert_eq!(
            spoken_here(&windows(&[
                "EN-us",
                "en-GB",
                "sl",
                "sr-Latn-RS",
                "de-DE",
                "x-y"
            ])),
            ["en-US", "de-DE"]
        );
        assert_eq!(
            spoken_here(&windows(&["sl-SI", "en-US", "de-DE", "hr-HR", "it-IT"])),
            ["sl-SI", "en-US", "de-DE"]
        );
        assert!(spoken_here(&[]).is_empty());
    }

    #[test]
    fn languages_are_split_and_trimmed() {
        assert_eq!(parse_languages("en-US, sl-SI ,,"), ["en-US", "sl-SI"]);
        assert!(parse_languages(" ").is_empty());
    }
}
