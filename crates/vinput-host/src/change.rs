//! What one change made in Settings does to the settings.
//!
//! Settings take effect one at a time, as they are made. [`apply`] checks a
//! change and writes it into the settings; what it returns says what the
//! background process has to do for the change to be in force, beyond
//! keeping the file and telling the engine. A change that cannot be made
//! leaves the settings as they are and comes back as the [`Notice`] to show.
//!
//! Nothing here touches Windows, so every rule can be tested.

use std::path::Path;

use vinput_core::actions::{self, Action, Field};
use vinput_core::keys::Hotkey;
use vinput_core::rewrite::Mode;
use vinput_core::search;
use vinput_core::settings::{STOP_AFTER_MINUTES, Settings, parse_languages};
use vinput_core::words::{
    MAX_SNIPPET, MAX_SNIPPETS, MAX_SOUNDS_LIKE, MAX_WORD, MAX_WORDS, Refused,
};
use vinput_view::{Change, Notice, NoticeKind, Page, Then, Tidy};

const VK_BACK: u16 = 0x08;
const VK_TAB: u16 = 0x09;
const VK_RETURN: u16 = 0x0D;
const VK_ESCAPE: u16 = 0x1B;
const VK_SPACE: u16 = 0x20;
const VK_DELETE: u16 = 0x2E;
/// Keys that cannot be a hotkey on their own: with one of them taken away
/// from every program, typing would be impossible.
const NEEDED_FOR_TYPING: [u16; 6] = [VK_BACK, VK_TAB, VK_RETURN, VK_ESCAPE, VK_SPACE, VK_DELETE];

/// What a change depends on besides the settings.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Ground<'a> {
    /// Whether this PC has a graphics card that can run local models.
    pub(crate) graphics_card: bool,
    /// Where voice memos go when no folder is set.
    pub(crate) default_memo_dir: &'a Path,
}

/// What the background process does after a change, besides keeping the
/// settings file and telling the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Follow {
    /// Nothing more.
    Nothing,
    /// Register the hotkeys again.
    Hotkeys,
    /// Bring the entry that starts Vinput with Windows in line.
    Autostart,
    /// Start or stop the local API.
    Api,
    /// Start or stop looking for newer versions.
    UpdateTimer,
    /// Fetch the engine for the other way of running local models.
    Engine,
}

/// What became of a change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Applied {
    /// The settings already said so.
    Unchanged,
    /// The settings were changed.
    Changed(Follow),
}

fn refused(page: Page, text: impl Into<String>) -> Notice {
    Notice {
        kind: NoticeKind::Error,
        text: text.into(),
        page,
    }
}

/// The longest text a setting may hold. Nothing a setting is for comes near.
const MAX_TEXT: usize = 1024;

/// What a window sent for a setting that is kept as text, trimmed. It is
/// refused if it would not fit on one line of the settings file, where a
/// line break would start a setting of its own.
fn one_line<'a>(page: Page, what: &str, text: &'a str) -> Result<&'a str, Notice> {
    let text = text.trim();
    if text.len() > MAX_TEXT || text.chars().any(char::is_control) {
        return Err(refused(page, format!("{what} cannot be used as it is.")));
    }
    Ok(text)
}

/// Whether `code` looks like a language code such as `en-US`.
fn is_language(code: &str) -> bool {
    code.len() <= 35 && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// What the window calls a way to tidy a dictation, as the settings keep
/// it.
pub(crate) fn mode_of(tidy: Tidy) -> Mode {
    match tidy {
        Tidy::Plain => Mode::Plain,
        Tidy::Clean => Mode::Clean,
        Tidy::Prompt => Mode::Prompt,
        Tidy::Email => Mode::Email,
    }
}

/// A way to tidy a dictation, as the window is told of it.
pub(crate) fn tidy_of(mode: Mode) -> Tidy {
    match mode {
        Mode::Plain => Tidy::Plain,
        Mode::Clean => Tidy::Clean,
        Mode::Prompt => Tidy::Prompt,
        Mode::Email => Tidy::Email,
    }
}

/// The page a change is made on, which is where what is wrong with it is
/// said.
pub(crate) fn page_of(change: &Change) -> Page {
    match change {
        Change::Cleanup(_) | Change::CleanupEnter(_) => Page::Cleanup,
        Change::Hotkey(_)
        | Change::HotkeyEnter(_)
        | Change::HotkeySearch(_)
        | Change::HotkeyMemo(_)
        | Change::HotkeyCommand(_)
        | Change::MemoDir(_)
        | Change::MemoToClipboard(_)
        | Change::SearchUrl(_)
        | Change::AddAction
        | Change::RemoveAction(_)
        | Change::ActionName(..)
        | Change::ActionHotkey(..)
        | Change::ActionProgram(..)
        | Change::ActionArguments(..)
        | Change::ActionThen(..)
        | Change::ActionCleanup(..)
        | Change::ActionTimeout(..) => Page::Shortcuts,
        _ => Page::General,
    }
}

/// The hotkeys, by what they are for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Dictate,
    Enter,
    Search,
    Memo,
    Command,
    /// The key of the action of the user's own at this place.
    Action(usize),
}

impl Key {
    /// Every hotkey there is with `settings`.
    fn all(settings: &Settings) -> impl Iterator<Item = Self> {
        [
            Self::Dictate,
            Self::Enter,
            Self::Search,
            Self::Memo,
            Self::Command,
        ]
        .into_iter()
        .chain((0..settings.actions.len()).map(Self::Action))
    }

    fn purpose(self, settings: &Settings) -> String {
        match self {
            Self::Dictate => "dictating".to_owned(),
            Self::Enter => "dictating and pressing Enter".to_owned(),
            Self::Search => "dictating a web search".to_owned(),
            Self::Memo => "recording a voice memo".to_owned(),
            Self::Command => "editing the selected text".to_owned(),
            Self::Action(index) => settings.actions.get(index).map_or_else(
                || "an action".to_owned(),
                |action| format!("the action {}", action.name),
            ),
        }
    }

    fn of(self, settings: &Settings) -> Option<Hotkey> {
        match self {
            Self::Dictate => Some(settings.hotkey),
            Self::Enter => settings.hotkey_enter,
            Self::Search => settings.hotkey_search,
            Self::Memo => settings.hotkey_memo,
            Self::Command => settings.hotkey_command,
            Self::Action(index) => settings.actions.get(index).and_then(|action| action.hotkey),
        }
    }
}

/// Reads a hotkey as the window wrote it on `page`. Empty is no hotkey.
fn hotkey(page: Page, text: &str) -> Result<Option<Hotkey>, Notice> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let hotkey: Hotkey = text
        .parse()
        .map_err(|_| refused(page, format!("{text} is not a key Vinput knows.")))?;
    let bare = !(hotkey.ctrl || hotkey.alt || hotkey.shift || hotkey.win);
    if bare && NEEDED_FOR_TYPING.contains(&hotkey.vk) {
        return Err(refused(
            page,
            format!("{hotkey} on its own is needed for typing. Add Ctrl, Alt or Shift to it."),
        ));
    }
    Ok(Some(hotkey))
}

/// Sets one of the hotkeys, unless another one already has that key.
fn set_hotkey(settings: &mut Settings, key: Key, text: &str) -> Result<Applied, Notice> {
    let wanted = hotkey(Page::Shortcuts, text)?;
    if key == Key::Dictate && wanted.is_none() {
        return Err(refused(
            Page::Shortcuts,
            "The key for dictating cannot be empty.",
        ));
    }
    if wanted == key.of(settings) {
        return Ok(Applied::Unchanged);
    }
    if let Some(wanted) = wanted
        && let Some(other) =
            Key::all(settings).find(|other| *other != key && other.of(settings) == Some(wanted))
    {
        return Err(refused(
            Page::Shortcuts,
            format!(
                "{wanted} is the key for {} already. Pick another key.",
                other.purpose(settings)
            ),
        ));
    }
    match key {
        Key::Dictate => settings.hotkey = wanted.unwrap_or(settings.hotkey),
        Key::Enter => settings.hotkey_enter = wanted,
        Key::Search => settings.hotkey_search = wanted,
        Key::Memo => settings.hotkey_memo = wanted,
        Key::Command => settings.hotkey_command = wanted,
        Key::Action(index) => match settings.actions.get_mut(index) {
            Some(action) => action.hotkey = wanted,
            None => return Ok(Applied::Unchanged),
        },
    }
    Ok(Applied::Changed(Follow::Hotkeys))
}

/// Writes `value` into `slot` and says whether that changed anything.
fn put<T: PartialEq>(slot: &mut T, value: T, follow: Follow) -> Applied {
    if *slot == value {
        Applied::Unchanged
    } else {
        *slot = value;
        Applied::Changed(follow)
    }
}

/// Makes one change to `settings`.
///
/// # Errors
///
/// Returns what to tell the user if the change cannot be made, in which case
/// `settings` is as it was.
pub(crate) fn apply(
    settings: &mut Settings,
    change: Change,
    ground: Ground<'_>,
) -> Result<Applied, Notice> {
    Ok(match change {
        Change::Hotkey(text) => set_hotkey(settings, Key::Dictate, &text)?,
        Change::HotkeyEnter(text) => set_hotkey(settings, Key::Enter, &text)?,
        Change::HotkeySearch(text) => set_hotkey(settings, Key::Search, &text)?,
        Change::HotkeyMemo(text) => set_hotkey(settings, Key::Memo, &text)?,
        Change::HotkeyCommand(text) => set_hotkey(settings, Key::Command, &text)?,
        Change::Cleanup(tidy) => put(&mut settings.cleanup, mode_of(tidy), Follow::Nothing),
        Change::CleanupEnter(tidy) => {
            put(&mut settings.cleanup_enter, mode_of(tidy), Follow::Nothing)
        }
        Change::Microphone(id) => {
            let id = one_line(Page::General, "That microphone", &id)?;
            put(&mut settings.microphone, id.to_owned(), Follow::Nothing)
        }
        Change::Languages(text) => {
            let languages = parse_languages(&text);
            if languages.is_empty() {
                return Err(refused(
                    Page::General,
                    "Vinput needs at least one language, such as en-US.",
                ));
            }
            if let Some(odd) = languages.iter().find(|code| !is_language(code)) {
                let odd: String = odd.chars().filter(|c| !c.is_control()).take(40).collect();
                return Err(refused(
                    Page::General,
                    format!("{odd} is not a language code. One looks like en-US."),
                ));
            }
            put(&mut settings.languages, languages, Follow::Nothing)
        }
        Change::Autostart(on) => put(&mut settings.autostart, on, Follow::Autostart),
        Change::ReadySound(on) => put(&mut settings.ready_sound, on, Follow::Nothing),
        Change::StopSound(on) => put(&mut settings.stop_sound, on, Follow::Nothing),
        Change::LiveTyping(on) => put(&mut settings.live_typing, on, Follow::Nothing),
        Change::StopAfterSilence(minutes) if !STOP_AFTER_MINUTES.contains(&minutes) => {
            Applied::Unchanged
        }
        Change::StopAfterSilence(minutes) => {
            put(&mut settings.stop_after_minutes, minutes, Follow::Nothing)
        }
        Change::CheckUpdates(on) => put(&mut settings.check_updates, on, Follow::UpdateTimer),
        // Without a graphics card there is no switch, and what the user
        // wants for a PC that has one stays as it is.
        Change::GraphicsCard(_) if !ground.graphics_card => Applied::Unchanged,
        Change::GraphicsCard(on) => put(&mut settings.local_gpu, on, Follow::Engine),
        Change::ApiEnabled(on) => put(&mut settings.api_enabled, on, Follow::Api),
        Change::ApiTranscript(on) => put(&mut settings.api_transcript, on, Follow::Nothing),
        Change::MemoDir(text) => {
            // The default folder is stored as "not set", so it keeps
            // following the Documents folder if that moves.
            let folder = one_line(Page::Shortcuts, "That folder", &text)?;
            if !folder.is_empty() && !Path::new(folder).is_absolute() {
                return Err(refused(
                    Page::Shortcuts,
                    "The folder for voice memos needs its whole path, such as D:\\Memos.",
                ));
            }
            let folder = Some(folder)
                .filter(|folder| Path::new(folder) != ground.default_memo_dir)
                .unwrap_or_default();
            put(&mut settings.memo_dir, folder.to_owned(), Follow::Nothing)
        }
        Change::MemoToClipboard(on) => put(&mut settings.memo_to_clipboard, on, Follow::Nothing),
        Change::SearchUrl(text) => {
            let address = text.trim();
            let address = if address.is_empty() {
                search::DEFAULT_TEMPLATE
            } else if search::is_valid(address) {
                address
            } else {
                return Err(refused(
                    Page::Shortcuts,
                    "That is not a web address a search can open. It starts with https:// and \
                     has %s where the words go.",
                ));
            };
            put(
                &mut settings.search_url,
                address.to_owned(),
                Follow::Nothing,
            )
        }
        Change::SpokenFormatting(on) => put(&mut settings.words.commands, on, Follow::Nothing),
        Change::AddWord(word, sounds_like) => add_word(settings, &word, &sounds_like)?,
        Change::RemoveWord(word) => changed(settings.words.remove_word(&word)),
        Change::AddSnippet(trigger, text) => add_snippet(settings, &trigger, &text)?,
        Change::RemoveSnippet(trigger) => changed(settings.words.remove_snippet(&trigger)),
        change @ (Change::AddAction
        | Change::RemoveAction(_)
        | Change::ActionHotkey(..)
        | Change::ActionName(..)
        | Change::ActionProgram(..)
        | Change::ActionArguments(..)
        | Change::ActionThen(..)
        | Change::ActionCleanup(..)
        | Change::ActionTimeout(..)) => change_action(settings, change)?,
    })
}

/// Makes one change to the actions of the user's own.
fn change_action(settings: &mut Settings, change: Change) -> Result<Applied, Notice> {
    Ok(match change {
        Change::AddAction => add_action(settings)?,
        Change::RemoveAction(index) if index < settings.actions.len() => {
            settings.actions.remove(index);
            Applied::Changed(Follow::Hotkeys)
        }
        Change::ActionHotkey(index, text) => set_hotkey(settings, Key::Action(index), &text)?,
        Change::ActionName(index, text) => {
            let name = action_text(Field::Name, &text)?;
            if name.is_empty() {
                return Err(refused(Page::Shortcuts, "An action needs a name."));
            }
            edit(settings, index, |action| {
                put(&mut action.name, name.to_owned(), Follow::Nothing)
            })
        }
        Change::ActionProgram(index, text) => {
            let program = action_text(Field::Program, &text)?;
            edit(settings, index, |action| {
                put(&mut action.program, program.to_owned(), Follow::Nothing)
            })
        }
        Change::ActionArguments(index, text) => {
            let arguments = action_text(Field::Arguments, &text)?;
            edit(settings, index, |action| {
                put(&mut action.arguments, arguments.to_owned(), Follow::Nothing)
            })
        }
        Change::ActionThen(index, then) => edit(settings, index, |action| {
            put(&mut action.then, then_of(then), Follow::Nothing)
        }),
        Change::ActionCleanup(index, tidy) => edit(settings, index, |action| {
            put(&mut action.cleanup, mode_of(tidy), Follow::Nothing)
        }),
        Change::ActionTimeout(_, seconds) if !actions::TIMEOUTS.contains(&seconds) => {
            Applied::Unchanged
        }
        Change::ActionTimeout(index, seconds) => edit(settings, index, |action| {
            put(&mut action.timeout, seconds, Follow::Nothing)
        }),
        // Another change, or the place of an action that is not there.
        _ => Applied::Unchanged,
    })
}

/// Adds an action of the user's own, set up as the example, or says why
/// there is no room for one.
fn add_action(settings: &mut Settings) -> Result<Applied, Notice> {
    if settings.actions.len() >= actions::MAX_ACTIONS {
        return Err(refused(
            Page::Shortcuts,
            format!(
                "There can be {} actions. Remove one to add another.",
                actions::MAX_ACTIONS
            ),
        ));
    }
    let mut action = Action::default();
    let taken = |name: &str| settings.actions.iter().any(|other| other.name == name);
    if taken(&action.name) {
        let first = action.name.clone();
        if let Some(name) = (2..=actions::MAX_ACTIONS + 1)
            .map(|number| format!("{first} {number}"))
            .find(|name| !taken(name))
        {
            action.name = name;
        }
    }
    settings.actions.push(action);
    Ok(Applied::Changed(Follow::Nothing))
}

/// What a window sent for a part of an action that is typed, trimmed, or
/// why it cannot be kept.
fn action_text(field: Field, text: &str) -> Result<&str, Notice> {
    let text = text.trim();
    match actions::problem(field, text) {
        Some(why) => Err(refused(Page::Shortcuts, why)),
        None => Ok(text),
    }
}

/// Changes the action at `index` with `change`; nothing if there is none.
fn edit(
    settings: &mut Settings,
    index: usize,
    change: impl FnOnce(&mut Action) -> Applied,
) -> Applied {
    settings
        .actions
        .get_mut(index)
        .map_or(Applied::Unchanged, change)
}

/// What the window calls what is done with the output of an action, as
/// the settings keep it.
pub(crate) fn then_of(then: Then) -> actions::Then {
    match then {
        Then::Nothing => actions::Then::Nothing,
        Then::Type => actions::Then::Type,
        Then::Copy => actions::Then::Copy,
    }
}

/// The same, as the window is told of it.
pub(crate) fn then_to(then: actions::Then) -> Then {
    match then {
        actions::Then::Nothing => Then::Nothing,
        actions::Then::Type => Then::Type,
        actions::Then::Copy => Then::Copy,
    }
}

/// Adds a word to the dictionary, or says why it cannot be kept.
fn add_word(settings: &mut Settings, word: &str, sounds_like: &str) -> Result<Applied, Notice> {
    let added = settings.words.add_word(word, sounds_like);
    Ok(changed(added.map_err(|why| {
        refused(
            Page::Words,
            match why {
                Refused::Empty | Refused::Unspoken => "Type the word first.".to_owned(),
                Refused::TooLong => format!(
                    "That is too long. A word can have {MAX_WORD} characters, and what \
                     it sounds like {MAX_SOUNDS_LIKE}."
                ),
                Refused::Full => format!(
                    "The dictionary is full. It holds {MAX_WORDS} words. Remove one \
                     to add another."
                ),
            },
        )
    })?))
}

/// Adds a snippet, or says why it cannot be kept.
fn add_snippet(settings: &mut Settings, trigger: &str, text: &str) -> Result<Applied, Notice> {
    let added = settings.words.add_snippet(trigger, text);
    Ok(changed(added.map_err(|why| {
        refused(
            Page::Words,
            match why {
                Refused::Empty => "A snippet needs both what you say and what is typed.".to_owned(),
                Refused::Unspoken => "What you say needs at least one letter or number.".to_owned(),
                Refused::TooLong => format!(
                    "That is too long. What you say can have {MAX_WORD} characters, \
                     and what is typed {MAX_SNIPPET}."
                ),
                Refused::Full => format!(
                    "There is no room for another snippet. There can be \
                     {MAX_SNIPPETS}. Remove one to add another."
                ),
            },
        )
    })?))
}

/// What a change to the dictionary or the snippets came to.
fn changed(changed: bool) -> Applied {
    if changed {
        Applied::Changed(Follow::Nothing)
    } else {
        Applied::Unchanged
    }
}

/// The hotkeys `after` has that `before` did not have anywhere: the ones
/// Windows has to be asked for.
pub(crate) fn new_hotkeys(before: &Settings, after: &Settings) -> Vec<Hotkey> {
    let had: Vec<Hotkey> = Key::all(before).filter_map(|key| key.of(before)).collect();
    Key::all(after)
        .filter_map(|key| key.of(after))
        .filter(|hotkey| !had.contains(hotkey))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT_MEMOS: &str = r"C:\Users\you\Documents\Vinput memos";

    fn ground(graphics_card: bool) -> Ground<'static> {
        Ground {
            graphics_card,
            default_memo_dir: Path::new(DEFAULT_MEMOS),
        }
    }

    /// Makes a change that is expected to be made.
    fn made(settings: &mut Settings, change: Change) -> Applied {
        apply(settings, change, ground(true)).unwrap_or(Applied::Unchanged)
    }

    /// Makes a change that is expected to be refused, and checks that it
    /// left the settings alone. Returns the page and the words of the notice.
    fn refusal(settings: &Settings, change: &Change) -> (Page, String) {
        let mut changed = settings.clone();
        let outcome = apply(&mut changed, change.clone(), ground(true));
        assert!(outcome.is_err(), "{change:?} was not refused");
        assert_eq!(&changed, settings);
        let notice = outcome.err().unwrap_or_default();
        assert_eq!(notice.kind, NoticeKind::Error);
        (notice.page, notice.text)
    }

    #[test]
    fn a_hotkey_is_set_from_the_text_the_window_sends() {
        let mut settings = Settings::default();
        assert_eq!(
            made(&mut settings, Change::Hotkey("Ctrl+Shift+Space".into())),
            Applied::Changed(Follow::Hotkeys)
        );
        assert_eq!(settings.hotkey.to_string(), "Ctrl+Shift+Space");
        assert_eq!(
            made(&mut settings, Change::HotkeyMemo(" f9 ".into())),
            Applied::Changed(Follow::Hotkeys)
        );
        assert_eq!(settings.hotkey_memo, Some(Hotkey::plain(0x78)));
        assert_eq!(
            made(&mut settings, Change::HotkeySearch("Alt+S".into())),
            Applied::Changed(Follow::Hotkeys)
        );
        assert_eq!(
            settings.hotkey_search.map(|key| key.to_string()),
            Some("Alt+S".to_owned())
        );
        // Saying what is already so changes nothing.
        assert_eq!(
            made(&mut settings, Change::HotkeyMemo("F9".into())),
            Applied::Unchanged
        );
    }

    #[test]
    fn the_key_that_edits_is_a_hotkey_like_the_others_and_is_spoken_of_with_them() {
        let mut settings = Settings::default();
        assert_eq!(
            made(&mut settings, Change::HotkeyCommand("F8".into())),
            Applied::Changed(Follow::Hotkeys)
        );
        assert_eq!(settings.hotkey_command, Some(Hotkey::plain(0x77)));
        // It cannot be a key another purpose has, and no other purpose can
        // take its key.
        let (page, why) = refusal(&settings, &Change::HotkeyCommand("F1".into()));
        assert_eq!(page, Page::Shortcuts);
        assert!(why.contains("dictating"), "{why}");
        let (page, why) = refusal(&settings, &Change::HotkeyMemo("F8".into()));
        assert_eq!(page, Page::Shortcuts);
        assert!(why.contains("editing the selected text"), "{why}");
        let (page, _) = refusal(&settings, &Change::HotkeyCommand("Nonsense".into()));
        assert_eq!(page, Page::Shortcuts);
        assert_eq!(
            new_hotkeys(&Settings::default(), &settings),
            [Hotkey::plain(0x77)]
        );
        assert_eq!(
            made(&mut settings, Change::HotkeyCommand(String::new())),
            Applied::Changed(Follow::Hotkeys)
        );
        assert_eq!(settings.hotkey_command, None);
    }

    #[test]
    fn each_key_that_types_is_given_its_own_way_to_tidy() {
        let mut settings = Settings::default();
        assert_eq!(
            made(&mut settings, Change::Cleanup(Tidy::Clean)),
            Applied::Changed(Follow::Nothing)
        );
        assert_eq!(
            made(&mut settings, Change::CleanupEnter(Tidy::Prompt)),
            Applied::Changed(Follow::Nothing)
        );
        assert_eq!(
            (settings.cleanup, settings.cleanup_enter),
            (Mode::Clean, Mode::Prompt)
        );
        assert_eq!(
            made(&mut settings, Change::Cleanup(Tidy::Clean)),
            Applied::Unchanged
        );
        for mode in Mode::ALL {
            assert_eq!(mode_of(tidy_of(mode)), mode);
        }
        assert_eq!(page_of(&Change::Cleanup(Tidy::Email)), Page::Cleanup);
        assert_eq!(page_of(&Change::Autostart(true)), Page::General);
        assert_eq!(page_of(&Change::SearchUrl(String::new())), Page::Shortcuts);
        assert_eq!(
            page_of(&Change::HotkeyCommand(String::new())),
            Page::Shortcuts
        );
        assert_eq!(
            page_of(&Change::ActionName(0, String::new())),
            Page::Shortcuts
        );
    }

    #[test]
    fn every_hotkey_but_the_first_is_switched_off_by_emptying_it() {
        let mut settings = Settings::default();
        assert!(settings.hotkey_enter.is_some());
        assert_eq!(
            made(&mut settings, Change::HotkeyEnter(String::new())),
            Applied::Changed(Follow::Hotkeys)
        );
        assert_eq!(settings.hotkey_enter, None);
        assert_eq!(
            made(&mut settings, Change::HotkeySearch("  ".into())),
            Applied::Unchanged
        );
        let (page, why) = refusal(&settings, &Change::Hotkey(String::new()));
        assert_eq!(page, Page::Shortcuts);
        assert!(why.contains("cannot be empty"), "{why}");
    }

    #[test]
    fn a_hotkey_that_cannot_be_used_is_refused_with_a_reason() {
        let settings = Settings::default();
        let (page, why) = refusal(&settings, &Change::Hotkey("Ctrl+Nonsense".into()));
        assert_eq!(page, Page::Shortcuts);
        assert!(why.contains("Ctrl+Nonsense"), "{why}");
        // F2 dictates and presses Enter in the settings a new user has.
        let (_, why) = refusal(&settings, &Change::Hotkey("F2".into()));
        assert!(
            why.contains("F2") && why.contains("pressing Enter"),
            "{why}"
        );
        let (_, why) = refusal(&settings, &Change::HotkeyMemo("F1".into()));
        assert!(why.contains("the key for dictating already"), "{why}");
        for bare in ["Space", "Enter", "Tab", "Esc", "Backspace", "Delete"] {
            let (_, why) = refusal(&settings, &Change::HotkeySearch(bare.into()));
            assert!(why.contains("needed for typing"), "{bare}: {why}");
        }
        // With a modifier the same keys are fine.
        let mut settings = settings;
        assert_eq!(
            made(&mut settings, Change::HotkeySearch("Ctrl+Space".into())),
            Applied::Changed(Follow::Hotkeys)
        );
    }

    #[test]
    fn two_hotkeys_can_trade_places_one_change_at_a_time() {
        let mut settings = Settings::default();
        // F1 dictates and F2 dictates and presses Enter; the other way
        // round takes a step over a third key.
        refusal(&settings, &Change::Hotkey("F2".into()));
        made(&mut settings, Change::HotkeyEnter("F3".into()));
        made(&mut settings, Change::Hotkey("F2".into()));
        made(&mut settings, Change::HotkeyEnter("F1".into()));
        assert_eq!(settings.hotkey, Hotkey::plain(0x71));
        assert_eq!(settings.hotkey_enter, Some(Hotkey::plain(0x70)));
    }

    #[test]
    fn only_keys_that_are_new_have_to_be_asked_for() {
        let before = Settings::default();
        let mut after = before.clone();
        made(&mut after, Change::HotkeyMemo("Ctrl+M".into()));
        assert_eq!(
            new_hotkeys(&before, &after)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["Ctrl+M"]
        );
        let mut emptied = after.clone();
        made(&mut emptied, Change::HotkeyMemo(String::new()));
        assert!(new_hotkeys(&after, &emptied).is_empty());
    }

    #[test]
    fn switches_say_what_has_to_follow_them() {
        let mut settings = Settings::default();
        let cases = [
            (Change::Autostart(false), Follow::Autostart),
            (Change::ReadySound(false), Follow::Nothing),
            (Change::StopSound(false), Follow::Nothing),
            (Change::LiveTyping(false), Follow::Nothing),
            (Change::StopAfterSilence(5), Follow::Nothing),
            (Change::CheckUpdates(false), Follow::UpdateTimer),
            (Change::GraphicsCard(false), Follow::Engine),
            (Change::ApiEnabled(false), Follow::Api),
            (Change::ApiTranscript(true), Follow::Nothing),
            (Change::MemoToClipboard(false), Follow::Nothing),
        ];
        for (change, follow) in cases {
            assert_eq!(
                made(&mut settings, change.clone()),
                Applied::Changed(follow),
                "{change:?}"
            );
            // The second time round the settings already say so.
            assert_eq!(made(&mut settings, change), Applied::Unchanged);
        }
        assert!(!settings.autostart && !settings.ready_sound && !settings.check_updates);
        assert!(!settings.stop_sound && !settings.live_typing);
        assert_eq!(settings.stop_after_minutes, 5);
        // A time that is not on offer changes nothing.
        assert_eq!(
            made(&mut settings, Change::StopAfterSilence(7)),
            Applied::Unchanged
        );
        assert_eq!(settings.stop_after_minutes, 5);
        assert!(!settings.local_gpu && !settings.api_enabled && !settings.memo_to_clipboard);
    }

    #[test]
    fn the_graphics_card_switch_does_nothing_on_a_pc_without_one() {
        let mut settings = Settings::default();
        assert_eq!(
            apply(&mut settings, Change::GraphicsCard(false), ground(false)),
            Ok(Applied::Unchanged)
        );
        assert!(settings.local_gpu);
    }

    #[test]
    fn the_microphone_is_kept_as_windows_names_it() {
        let mut settings = Settings::default();
        assert_eq!(
            made(&mut settings, Change::Microphone("{0.0.1}.{a}".into())),
            Applied::Changed(Follow::Nothing)
        );
        assert_eq!(settings.microphone, "{0.0.1}.{a}");
        made(&mut settings, Change::Microphone(String::new()));
        assert_eq!(settings.microphone, "");
    }

    #[test]
    fn languages_are_split_and_at_least_one_is_needed() {
        let mut settings = Settings::default();
        assert_eq!(
            made(&mut settings, Change::Languages("en-US,  sl-SI,".into())),
            Applied::Changed(Follow::Nothing)
        );
        assert_eq!(settings.languages, ["en-US", "sl-SI"]);
        let (page, _) = refusal(&settings, &Change::Languages(" , ".into()));
        assert_eq!(page, Page::General);
    }

    #[test]
    fn the_default_memo_folder_is_kept_as_not_set() {
        let mut settings = Settings::default();
        assert_eq!(
            made(&mut settings, Change::MemoDir(r"D:\Memos".into())),
            Applied::Changed(Follow::Nothing)
        );
        assert_eq!(settings.memo_dir, r"D:\Memos");
        made(&mut settings, Change::MemoDir(DEFAULT_MEMOS.into()));
        assert_eq!(settings.memo_dir, "");
        made(&mut settings, Change::MemoDir(r"D:\Memos".into()));
        made(&mut settings, Change::MemoDir(String::new()));
        assert_eq!(settings.memo_dir, "");
    }

    #[test]
    fn a_search_address_has_to_be_a_web_address_and_empty_is_the_default() {
        let mut settings = Settings::default();
        assert_eq!(
            made(
                &mut settings,
                Change::SearchUrl("https://duckduckgo.com/?q=%s".into())
            ),
            Applied::Changed(Follow::Nothing)
        );
        assert_eq!(settings.search_url, "https://duckduckgo.com/?q=%s");
        for bad in ["calc.exe", "file:///C:/x", "https://", "https://a b"] {
            let (page, _) = refusal(&settings, &Change::SearchUrl(bad.into()));
            assert_eq!(page, Page::Shortcuts, "{bad}");
        }
        made(&mut settings, Change::SearchUrl("  ".into()));
        assert_eq!(settings.search_url, search::DEFAULT_TEMPLATE);
    }

    #[test]
    fn text_that_would_break_the_settings_file_is_refused() {
        let settings = Settings::default();
        for change in [
            Change::Microphone("{usb}\nsearch_url = https://example.com/?%s".into()),
            Change::MemoDir("D:\\Memos\nhotkey = F9".into()),
            Change::Languages("en-US\nmodel = x".into()),
            Change::Languages("en-US, <script>".into()),
            Change::Microphone("x".repeat(MAX_TEXT + 1)),
        ] {
            let (_, why) = refusal(&settings, &change);
            assert!(!why.contains('\n'), "{why:?}");
        }
        // What is kept still reads back as it was written.
        let mut changed = settings;
        made(&mut changed, Change::Microphone(" {usb} ".into()));
        assert_eq!(Settings::parse(&changed.serialize()), changed);
    }

    #[test]
    fn words_and_snippets_are_added_changed_and_removed() {
        let mut settings = Settings::default();
        let changed = Applied::Changed(Follow::Nothing);
        let word = |word: &str, sounds: &str| Change::AddWord(word.into(), sounds.into());
        let snippet = |said: &str, text: &str| Change::AddSnippet(said.into(), text.into());
        assert_eq!(made(&mut settings, word(" VinPUT ", "vin put")), changed);
        assert_eq!(
            made(&mut settings, word("VinPUT", "vin put")),
            Applied::Unchanged
        );
        assert_eq!(made(&mut settings, word("vinput", "")), changed);
        assert_eq!(settings.words.dictionary.len(), 1);
        assert_eq!(made(&mut settings, snippet("my email", "a@b.c")), changed);
        assert_eq!(made(&mut settings, snippet("My email", "d@e.f")), changed);
        assert_eq!(settings.words.snippets.len(), 1);
        assert_eq!(settings.words.snippets[0].text, "d@e.f");
        assert_eq!(
            made(&mut settings, Change::SpokenFormatting(false)),
            changed
        );
        // All of it is in the file, which reads back as it was written.
        assert_eq!(Settings::parse(&settings.serialize()), settings);

        assert_eq!(
            made(&mut settings, Change::RemoveWord("vinput".into())),
            changed
        );
        assert_eq!(
            made(&mut settings, Change::RemoveWord("vinput".into())),
            Applied::Unchanged
        );
        assert_eq!(
            made(&mut settings, Change::RemoveSnippet("My email".into())),
            changed
        );
        assert!(settings.words.dictionary.is_empty() && settings.words.snippets.is_empty());
    }

    #[test]
    fn a_word_or_a_snippet_that_cannot_be_kept_is_refused_on_its_page() {
        let settings = Settings::default();
        let long = "x".repeat(MAX_SNIPPET + 1);
        for change in [
            Change::AddWord("  ".into(), "sound".into()),
            Change::AddWord(long.clone(), String::new()),
            Change::AddSnippet(String::new(), "text".into()),
            Change::AddSnippet("phrase".into(), " ".into()),
            Change::AddSnippet("?!".into(), "text".into()),
            Change::AddSnippet("phrase".into(), long),
        ] {
            let (page, why) = refusal(&settings, &change);
            assert_eq!(page, Page::Words, "{change:?}");
            assert!(why.ends_with('.'), "{why:?}");
        }
        // A line break in a word cannot start a line of its own in the file.
        let mut changed = settings;
        made(
            &mut changed,
            Change::AddWord("Vin\nhotkey = F9".into(), "a | b\n".into()),
        );
        assert_eq!(changed.words.dictionary[0].word, "Vin hotkey = F9");
        assert_eq!(Settings::parse(&changed.serialize()), changed);
    }

    #[test]
    fn the_memo_folder_needs_its_whole_path() {
        let settings = Settings::default();
        for folder in ["Memos", r"..\Memos", "calc.exe"] {
            let (page, _) = refusal(&settings, &Change::MemoDir(folder.into()));
            assert_eq!(page, Page::Shortcuts, "{folder}");
        }
    }

    #[test]
    fn an_action_is_added_as_the_example_up_to_as_many_as_there_can_be() {
        let mut settings = Settings::default();
        assert_eq!(
            made(&mut settings, Change::AddAction),
            Applied::Changed(Follow::Nothing)
        );
        assert_eq!(settings.actions, [Action::default()]);
        made(&mut settings, Change::AddAction);
        let names: Vec<_> = settings.actions.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Add to my notes", "Add to my notes 2"]);
        for _ in 2..actions::MAX_ACTIONS {
            made(&mut settings, Change::AddAction);
        }
        assert_eq!(settings.actions.len(), actions::MAX_ACTIONS);
        let (page, why) = refusal(&settings, &Change::AddAction);
        assert_eq!(page, Page::Shortcuts);
        assert!(why.contains("Remove one"), "{why}");
    }

    #[test]
    fn each_part_of_an_action_is_changed_and_checked() {
        let mut settings = Settings::default();
        made(&mut settings, Change::AddAction);
        made(&mut settings, Change::ActionName(0, " Translate ".into()));
        made(
            &mut settings,
            Change::ActionProgram(0, r"C:\Tools\tr.exe".into()),
        );
        made(
            &mut settings,
            Change::ActionArguments(0, "--to en {text}".into()),
        );
        made(&mut settings, Change::ActionThen(0, Then::Copy));
        made(&mut settings, Change::ActionCleanup(0, Tidy::Clean));
        made(&mut settings, Change::ActionTimeout(0, 60));
        assert_eq!(
            settings.actions,
            [Action {
                name: "Translate".into(),
                hotkey: None,
                program: r"C:\Tools\tr.exe".into(),
                arguments: "--to en {text}".into(),
                then: actions::Then::Copy,
                cleanup: Mode::Clean,
                timeout: 60,
            }]
        );
        // A time that is not on offer, or an action that is not there,
        // changes nothing.
        assert_eq!(
            made(&mut settings, Change::ActionTimeout(0, 7)),
            Applied::Unchanged
        );
        assert_eq!(
            made(&mut settings, Change::ActionName(3, "x".into())),
            Applied::Unchanged
        );
        assert_eq!(
            made(&mut settings, Change::RemoveAction(3)),
            Applied::Unchanged
        );
        for bad in [
            Change::ActionName(0, "  ".into()),
            Change::ActionName(0, "a | b".into()),
            Change::ActionProgram(0, String::new()),
            Change::ActionArguments(0, "-Command \"echo {text}\"".into()),
            Change::ActionArguments(0, "two\nlines".into()),
        ] {
            let (page, _) = refusal(&settings, &bad);
            assert_eq!(page, Page::Shortcuts, "{bad:?}");
        }
        assert_eq!(
            made(&mut settings, Change::RemoveAction(0)),
            Applied::Changed(Follow::Hotkeys)
        );
        assert!(settings.actions.is_empty());
    }

    #[test]
    fn the_key_of_an_action_is_one_no_other_has() {
        let mut settings = Settings::default();
        made(&mut settings, Change::AddAction);
        made(&mut settings, Change::AddAction);
        assert_eq!(
            made(&mut settings, Change::ActionHotkey(0, "Ctrl+Alt+N".into())),
            Applied::Changed(Follow::Hotkeys)
        );
        assert_eq!(
            settings.actions[0].hotkey.map(|key| key.to_string()),
            Some("Ctrl+Alt+N".to_owned())
        );
        // Not one a built-in key has, nor one of another action.
        let (_, why) = refusal(&settings, &Change::ActionHotkey(1, "F1".into()));
        assert!(why.contains("for dictating"), "{why}");
        let (_, why) = refusal(&settings, &Change::ActionHotkey(1, "Ctrl+Alt+N".into()));
        assert!(why.contains("the action Add to my notes"), "{why}");
        let (_, why) = refusal(&settings, &Change::HotkeySearch("Ctrl+Alt+N".into()));
        assert!(why.contains("the action Add to my notes"), "{why}");
        // Nor one typing needs.
        refusal(&settings, &Change::ActionHotkey(1, "Space".into()));
        // A key that is new is asked of Windows.
        let before = settings.clone();
        made(&mut settings, Change::ActionHotkey(1, "F9".into()));
        let new: Vec<_> = new_hotkeys(&before, &settings)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(new, ["F9"]);
        assert_eq!(
            made(&mut settings, Change::ActionHotkey(1, String::new())),
            Applied::Changed(Follow::Hotkeys)
        );
        assert_eq!(settings.actions[1].hotkey, None);
    }
}
