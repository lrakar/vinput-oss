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

use vinput_core::keys::Hotkey;
use vinput_core::search;
use vinput_core::settings::{Settings, parse_languages};
use vinput_view::{Change, Notice, NoticeKind, Page};

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

/// The four hotkeys, by what they are for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Dictate,
    Enter,
    Search,
    Memo,
}

impl Key {
    const ALL: [Self; 4] = [Self::Dictate, Self::Enter, Self::Search, Self::Memo];

    fn purpose(self) -> &'static str {
        match self {
            Self::Dictate => "dictating",
            Self::Enter => "dictating and pressing Enter",
            Self::Search => "dictating a web search",
            Self::Memo => "recording a voice memo",
        }
    }

    fn of(self, settings: &Settings) -> Option<Hotkey> {
        match self {
            Self::Dictate => Some(settings.hotkey),
            Self::Enter => settings.hotkey_enter,
            Self::Search => settings.hotkey_search,
            Self::Memo => settings.hotkey_memo,
        }
    }
}

/// Reads a hotkey as the window wrote it. Empty is no hotkey.
fn hotkey(text: &str) -> Result<Option<Hotkey>, Notice> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let hotkey: Hotkey = text
        .parse()
        .map_err(|_| refused(Page::General, format!("{text} is not a key Vinput knows.")))?;
    let bare = !(hotkey.ctrl || hotkey.alt || hotkey.shift || hotkey.win);
    if bare && NEEDED_FOR_TYPING.contains(&hotkey.vk) {
        return Err(refused(
            Page::General,
            format!("{hotkey} on its own is needed for typing. Add Ctrl, Alt or Shift to it."),
        ));
    }
    Ok(Some(hotkey))
}

/// Sets one of the hotkeys, unless another one already has that key.
fn set_hotkey(settings: &mut Settings, key: Key, text: &str) -> Result<Applied, Notice> {
    let wanted = hotkey(text)?;
    if key == Key::Dictate && wanted.is_none() {
        return Err(refused(
            Page::General,
            "The key for dictating cannot be empty.",
        ));
    }
    if wanted == key.of(settings) {
        return Ok(Applied::Unchanged);
    }
    if let Some(wanted) = wanted
        && let Some(other) = Key::ALL
            .into_iter()
            .find(|other| *other != key && other.of(settings) == Some(wanted))
    {
        return Err(refused(
            Page::General,
            format!(
                "{wanted} is the key for {} already. Pick another key.",
                other.purpose()
            ),
        ));
    }
    match key {
        Key::Dictate => settings.hotkey = wanted.unwrap_or(settings.hotkey),
        Key::Enter => settings.hotkey_enter = wanted,
        Key::Search => settings.hotkey_search = wanted,
        Key::Memo => settings.hotkey_memo = wanted,
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
        Change::CheckUpdates(on) => put(&mut settings.check_updates, on, Follow::UpdateTimer),
        // Without a graphics card there is no switch, and what the user
        // wants for a PC that has one stays as it is.
        Change::GraphicsCard(_) if !ground.graphics_card => Applied::Unchanged,
        Change::GraphicsCard(on) => put(&mut settings.local_gpu, on, Follow::Engine),
        Change::ApiEnabled(on) => put(&mut settings.api_enabled, on, Follow::Api),
        Change::MemoDir(text) => {
            // The default folder is stored as "not set", so it keeps
            // following the Documents folder if that moves.
            let folder = one_line(Page::Advanced, "That folder", &text)?;
            if !folder.is_empty() && !Path::new(folder).is_absolute() {
                return Err(refused(
                    Page::Advanced,
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
                    Page::Advanced,
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
    })
}

/// The hotkeys `after` has that `before` did not have anywhere: the ones
/// Windows has to be asked for.
pub(crate) fn new_hotkeys(before: &Settings, after: &Settings) -> Vec<Hotkey> {
    let had: Vec<Hotkey> = Key::ALL
        .into_iter()
        .filter_map(|key| key.of(before))
        .collect();
    Key::ALL
        .into_iter()
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
        assert_eq!(page, Page::General);
        assert!(why.contains("cannot be empty"), "{why}");
    }

    #[test]
    fn a_hotkey_that_cannot_be_used_is_refused_with_a_reason() {
        let settings = Settings::default();
        let (page, why) = refusal(&settings, &Change::Hotkey("Ctrl+Nonsense".into()));
        assert_eq!(page, Page::General);
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
            (Change::CheckUpdates(false), Follow::UpdateTimer),
            (Change::GraphicsCard(false), Follow::Engine),
            (Change::ApiEnabled(false), Follow::Api),
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
        assert!(!settings.stop_sound);
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
            assert_eq!(page, Page::Advanced, "{bad}");
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
    fn the_memo_folder_needs_its_whole_path() {
        let settings = Settings::default();
        for folder in ["Memos", r"..\Memos", "calc.exe"] {
            let (page, _) = refusal(&settings, &Change::MemoDir(folder.into()));
            assert_eq!(page, Page::Advanced, "{folder}");
        }
    }
}
