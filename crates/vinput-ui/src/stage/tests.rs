//! Tests of [`super`].

use vinput_view::{Change, Dialog, GoogleStep, Settings, Setup};

use super::*;
use crate::hotkey::{self, Modifiers};
use crate::layout::Memory;
use crate::layout::ruler::Ruler;
use crate::mock;
use crate::scene::Asking;

const NONE: Modifiers = Modifiers {
    ctrl: false,
    alt: false,
    shift: false,
    win: false,
};

fn stage(page: Page) -> Stage {
    Stage::new(
        Showing::Settings(page),
        mock::scene("settings").unwrap_or_default(),
        Size::new(940.0, 660.0),
        Box::new(Ruler),
    )
}

/// A stage on `page` with the row that opens called `key` open.
fn opened(page: Page, key: &str) -> Stage {
    let mut stage = stage(page);
    let mut memory = Memory::default();
    memory.set_open(key, true);
    stage.remember(memory);
    stage
}

/// What the field of the Shortcuts page that these tests type in holds.
const SEARCH: &str = "https://www.google.com/search?q=%s";

fn find(stage: &Stage, test: impl Fn(&Control) -> bool) -> usize {
    let found = stage.frame().controls.iter().position(test);
    assert!(found.is_some(), "no such control");
    found.unwrap_or(0)
}

fn click(stage: &mut Stage, index: usize) -> Vec<Effect> {
    // Bring it into sight first, as a user would by scrolling.
    stage.reveal(index);
    let center = stage.rect_of(index).unwrap_or_default().center();
    stage.mouse_move(center);
    let mut effects = stage.mouse_down(center);
    effects.extend(stage.mouse_up(center));
    effects
}

fn sends(effects: &[Effect]) -> Vec<Command> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Send(command) => Some(command.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn clicking_a_switch_flips_it_at_once_and_tells_the_background() {
    let mut stage = stage(Page::General);
    let switch = find(&stage, |c| c.label == "Start with Windows");
    let effects = click(&mut stage, switch);
    assert_eq!(sends(&effects), [Command::Set(Change::Autostart(false))]);
    assert!(matches!(
        stage.frame().controls[switch].kind,
        Kind::Toggle { on: false, .. }
    ));
    assert!(stage.take_dirty());
    // A press that ends elsewhere does nothing.
    let center = stage.rect_of(switch).unwrap_or_default().center();
    stage.mouse_down(center);
    assert!(stage.mouse_up(Point::new(5.0, 300.0)).is_empty());
}

#[test]
fn the_pane_turns_pages_and_keeps_the_keyboard() {
    let mut stage = stage(Page::General);
    let models = find(&stage, |c| {
        matches!(
            c.kind,
            Kind::Nav {
                page: Page::Models,
                ..
            }
        )
    });
    let effects = click(&mut stage, models);
    assert!(effects.is_empty());
    assert_eq!(stage.showing(), &Showing::Settings(Page::Models));
    assert!(matches!(
        stage.frame().controls[models].kind,
        Kind::Nav { selected: true, .. }
    ));
    // Arrows move within the pane, past Words and Cleanup, and Enter opens.
    let (used, _) = stage.key_down(vk::DOWN, NONE);
    assert!(used);
    let (used, _) = stage.key_down(vk::DOWN, NONE);
    assert!(used);
    let (used, _) = stage.key_down(vk::DOWN, NONE);
    assert!(used);
    let (_, effects) = stage.key_down(vk::ENTER, NONE);
    // The page of the account says that it has come into sight.
    assert_eq!(sends(&effects), [Command::ProShown]);
    assert_eq!(stage.showing(), &Showing::Settings(Page::Pro));
    assert!(stage.look().ring);
}

#[test]
fn leaving_the_add_page_by_the_pane_clears_the_finder() {
    let mut stage = stage(Page::AddModel);
    let about = find(&stage, |c| {
        matches!(
            c.kind,
            Kind::Nav {
                page: Page::About,
                ..
            }
        )
    });
    assert_eq!(sends(&click(&mut stage, about)), [Command::ClearFinder]);
    let mut stage = self::stage(Page::AddModel);
    let effects = stage.show(Page::General);
    assert_eq!(sends(&effects), [Command::ClearFinder]);
    assert_eq!(effects.last(), Some(&Effect::Front));
}

#[test]
fn leaving_the_google_setup_by_the_pane_calls_it_off() {
    let close = Command::Google(Setup::Close);
    let mut stage = stage(Page::Google);
    let about = find(&stage, |c| {
        matches!(
            c.kind,
            Kind::Nav {
                page: Page::About,
                ..
            }
        )
    });
    assert_eq!(
        sends(&click(&mut stage, about)),
        std::slice::from_ref(&close)
    );
    // The arrow beside the title says so itself, once.
    let mut stage = self::stage(Page::Google);
    assert_eq!(sends(&stage.back()), [close]);
    assert_eq!(stage.showing(), &Showing::Settings(Page::Models));
}

#[test]
fn the_page_of_the_account_says_when_it_comes_into_sight() {
    // Turned to from another page.
    let mut stage = stage(Page::General);
    assert_eq!(stage.first_words(), [Command::Ready]);
    let pro = find(&stage, |c| {
        matches!(
            c.kind,
            Kind::Nav {
                page: Page::Pro,
                ..
            }
        )
    });
    assert_eq!(sends(&click(&mut stage, pro)), [Command::ProShown]);
    // Already on it: nothing more to say.
    let pro = find(&stage, |c| {
        matches!(
            c.kind,
            Kind::Nav {
                page: Page::Pro,
                ..
            }
        )
    });
    assert!(sends(&click(&mut stage, pro)).is_empty());
    // Asked for by the background process, or opened on from the start.
    let mut stage = self::stage(Page::General);
    assert_eq!(sends(&stage.show(Page::Pro)), [Command::ProShown]);
    let stage = self::stage(Page::Pro);
    assert_eq!(stage.first_words(), [Command::Ready, Command::ProShown]);
}

#[test]
fn the_arrow_of_the_setup_goes_one_step_back_and_out_from_the_first() {
    let on = |scene: &str| {
        Stage::new(
            Showing::Settings(Page::Google),
            mock::scene(scene).unwrap_or_default(),
            Size::new(940.0, 660.0),
            Box::new(Ruler),
        )
    };
    let step = |step| Command::Google(Setup::Step(step));
    // From entering to getting, from getting to the choice of service:
    // the page stays, the background process turns the step.
    let mut stage = on("google-gemini-wrong-key");
    assert_eq!(sends(&stage.back()), [step(GoogleStep::Get)]);
    assert_eq!(stage.showing(), &Showing::Settings(Page::Google));
    let mut stage = on("google-speech-get");
    assert_eq!(sends(&stage.back()), [step(GoogleStep::Service)]);
    assert_eq!(stage.showing(), &Showing::Settings(Page::Google));
    // From the first step and from the last, out to the models.
    for scene in ["google-service", "google-gemini-done"] {
        let mut stage = on(scene);
        assert_eq!(sends(&stage.back()), [Command::Google(Setup::Close)]);
        assert_eq!(stage.showing(), &Showing::Settings(Page::Models));
    }
}

/// A stage on the step of the setup where a Gemini API key is pasted.
fn pasting() -> Stage {
    Stage::new(
        Showing::Settings(Page::Google),
        mock::scene("google-gemini-enter").unwrap_or_default(),
        Size::new(940.0, 660.0),
        Box::new(Ruler),
    )
}

#[test]
fn the_paste_button_types_in_its_field_and_what_is_pasted_is_handed_in_at_once() {
    let mut stage = pasting();
    let paste = find(&stage, |c| c.name() == "Paste");
    let effects = click(&mut stage, paste);
    // The field is being typed in, and the window is asked to paste there.
    assert!(matches!(
        effects.as_slice(),
        [Effect::Edit(Editing { secret: true, .. }), Effect::Paste]
    ));
    let field = find(&stage, |c| matches!(c.kind, Kind::Field { .. }));
    assert_eq!(stage.focused(), Some(field));
    // The edit control says what it reads now, then that it was pasted.
    stage.typed(" AIzaExample ".to_owned());
    let key = Command::Google(Setup::Key("AIzaExample".to_owned()));
    let effects = stage.edit_pasted();
    assert_eq!(sends(&effects), [key]);
    // What was pasted is left selected, for another paste to replace.
    assert!(matches!(effects.last(), Some(Effect::Retype(text)) if text == " AIzaExample "));
    // Nothing new to hand in when the field is left.
    assert!(sends(&stage.edit_lost()).is_empty());
}

#[test]
fn a_paste_into_any_other_field_waits_for_enter() {
    let mut stage = stage(Page::AddModel);
    let field = find(&stage, |c| matches!(c.kind, Kind::Field { .. }));
    click(&mut stage, field);
    stage.typed("https://huggingface.co/a/b".to_owned());
    assert!(stage.edit_pasted().is_empty());
}

#[test]
fn tab_walks_every_control_once_and_stops_in_the_pane_once() {
    let mut stage = stage(Page::Shortcuts);
    let stops = stage
        .frame()
        .controls
        .iter()
        .filter(|control| Stage::tab_stop(control))
        .count();
    let mut seen = Vec::new();
    for _ in 0..stops {
        stage.key_down(vk::TAB, NONE);
        seen.push(stage.focused());
    }
    let mut unique = seen.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), stops);
    assert!(seen.windows(2).all(|pair| pair[0] < pair[1]));
    // Once around, and back the other way.
    stage.key_down(vk::TAB, NONE);
    assert_eq!(stage.focused(), seen[0]);
    let shift = Modifiers {
        shift: true,
        ..NONE
    };
    stage.key_down(vk::TAB, shift);
    assert_eq!(stage.focused(), seen[stops - 1]);
    // The last control is below the fold: Tab scrolled it into sight.
    assert!(stage.look().scroll > 0.0);
}

#[test]
fn a_field_hands_in_what_was_typed_only_if_it_changed() {
    let mut stage = opened(Page::Shortcuts, "search-with");
    let field = find(&stage, |c| c.key == Some("search-url"));
    let effects = click(&mut stage, field);
    assert!(matches!(
        effects.as_slice(),
        [Effect::Edit(Editing { key: "search-url", text, select_all: false, .. })] if text == SEARCH
    ));
    assert!(stage.edit_rect().is_some_and(|(_, seen)| seen));
    // Enter without a change says nothing.
    assert!(stage.edit_enter().is_empty());
    assert!(
        stage
            .typed("https://example.com/?q=%s".to_owned())
            .is_empty()
    );
    assert_eq!(
        sends(&stage.edit_enter()),
        [Command::Set(Change::SearchUrl(
            "https://example.com/?q=%s".to_owned()
        ))]
    );
    // Nor does leaving after that; the edit control goes away.
    assert_eq!(stage.edit_lost(), [Effect::EndEdit]);
    assert!(stage.edit_rect().is_none());
}

#[test]
fn escape_puts_a_field_back_and_a_rebuild_keeps_what_is_typed() {
    let mut stage = opened(Page::Shortcuts, "search-with");
    let field = find(&stage, |c| c.key == Some("search-url"));
    click(&mut stage, field);
    stage.typed("fr".to_owned());
    // A new view arrives in the middle of typing.
    let view = mock::scene("listening").unwrap_or_default();
    assert_eq!(view.settings.search_url, SEARCH);
    assert!(stage.set_view(view).is_empty());
    assert!(matches!(
        &stage.frame().controls[field].kind,
        Kind::Field { text, .. } if text == "fr"
    ));
    assert_eq!(stage.look().editing, Some(field));
    assert_eq!(stage.edit_escape(), [Effect::Retype(SEARCH.to_owned())]);
    assert_eq!(stage.edit_lost(), [Effect::EndEdit]);
}

#[test]
fn what_was_handed_in_gives_way_to_what_the_view_says() {
    let mut stage = opened(Page::Shortcuts, "search-with");
    let field = find(&stage, |c| c.key == Some("search-url"));
    click(&mut stage, field);
    stage.typed("xx".to_owned());
    let lost = stage.edit_lost();
    assert_eq!(sends(&lost).len(), 1);
    // The background refused it: the view still says the old text.
    stage.set_view(mock::scene("settings").unwrap_or_default());
    assert!(matches!(
        &stage.frame().controls[field].kind,
        Kind::Field { text, .. } if text == SEARCH
    ));
}

#[test]
fn a_button_beside_a_field_reads_what_is_typed_in_it() {
    let mut stage = stage(Page::AddModel);
    let source = find(&stage, |c| matches!(c.kind, Kind::Field { .. }));
    click(&mut stage, source);
    stage.typed("https://huggingface.co/x/y".to_owned());
    let find_button = find(&stage, |c| c.name() == "Find");
    let effects = click(&mut stage, find_button);
    assert_eq!(
        sends(&effects),
        [Command::FindModel("https://huggingface.co/x/y".to_owned())]
    );
    assert!(effects.contains(&Effect::EndEdit));
    // The text stays in the field though no view knows it.
    stage.set_view(mock::scene("found").unwrap_or_default());
    assert!(matches!(
        &stage.frame().controls[source].kind,
        Kind::Field { text, .. } if text.ends_with("x/y")
    ));
}

#[test]
fn adding_an_entry_reads_both_fields_and_empties_them_for_the_next() {
    let mut stage = stage(Page::Words);
    let field = |stage: &Stage, key: &str| find(stage, |c| c.key == Some(key));
    let reads = |stage: &Stage, key: &str| match &stage.frame().controls[field(stage, key)].kind {
        Kind::Field { text, .. } => text.clone(),
        _ => unreachable!("a field was expected"),
    };
    let word = field(&stage, "word");
    click(&mut stage, word);
    stage.typed("VinPUT".to_owned());
    let sounds_like = field(&stage, "word-sounds-like");
    click(&mut stage, sounds_like);
    stage.typed("vin put".to_owned());
    // Enter in the second field adds the word, as the button would.
    let effects = stage.edit_enter();
    let added = Change::AddWord("VinPUT".to_owned(), "vin put".to_owned());
    assert_eq!(sends(&effects), [Command::Set(added)]);
    assert_eq!(reads(&stage, "word"), "");
    assert_eq!(reads(&stage, "word-sounds-like"), "");
    // The first field is typed in again, and starts out empty.
    assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::Edit(Editing { key: "word", text, .. }) if text.is_empty()
    )));

    // The button reads the fields of its own list, though nothing of
    // theirs is being typed in.
    stage.typed("my email".to_owned());
    let text = field(&stage, "snippet-text");
    click(&mut stage, text);
    stage.typed("someone@example.com".to_owned());
    let trigger = field(&stage, "snippet-trigger");
    click(&mut stage, trigger);
    stage.typed("my email".to_owned());
    let add = stage
        .frame()
        .controls
        .iter()
        .rposition(|c| c.name() == "Add");
    let effects = click(&mut stage, add.unwrap_or_default());
    let added = Change::AddSnippet("my email".to_owned(), "someone@example.com".to_owned());
    assert_eq!(sends(&effects), [Command::Set(added)]);
    assert_eq!(reads(&stage, "snippet-trigger"), "");
    assert_eq!(reads(&stage, "snippet-text"), "");
    // What was typed for a word and not added is still there.
    assert_eq!(reads(&stage, "word"), "my email");
}

#[test]
fn a_hotkey_field_takes_a_combination_and_lets_the_background_know() {
    let mut stage = stage(Page::Shortcuts);
    let field = find(&stage, |c| c.key == Some("hotkey-memo"));
    assert_eq!(
        click(&mut stage, field),
        [Effect::Send(Command::CapturingHotkey(true))]
    );
    let ctrl = Modifiers { ctrl: true, ..NONE };
    let (used, effects) = stage.key_down(0x11, ctrl);
    assert!(used && effects.is_empty());
    assert_eq!(stage.look().capture, Some((field, "Ctrl+")));
    let (_, effects) = stage.key_down(0x4B, ctrl);
    assert_eq!(
        effects,
        [
            Effect::Send(Command::CapturingHotkey(false)),
            Effect::Send(Command::Set(Change::HotkeyMemo("Ctrl+K".to_owned()))),
        ]
    );
    assert!(matches!(
        &stage.frame().controls[field].kind,
        Kind::Hotkey { text, .. } if text == "Ctrl+K"
    ));
    // Esc gives up; Backspace clears a field that may be cleared.
    stage.key_down(vk::ENTER, NONE);
    assert!(stage.capturing());
    let (_, effects) = stage.key_down(vk::ESCAPE, NONE);
    assert_eq!(effects, [Effect::Send(Command::CapturingHotkey(false))]);
    stage.key_down(vk::SPACE, NONE);
    let (_, effects) = stage.key_down(hotkey::VK_BACK, NONE);
    assert_eq!(
        sends(&effects).last(),
        Some(&Command::Set(Change::HotkeyMemo(String::new())))
    );
}

#[test]
fn the_main_hotkey_cannot_be_cleared() {
    let mut stage = stage(Page::Shortcuts);
    let field = find(&stage, |c| c.key == Some("hotkey"));
    click(&mut stage, field);
    let (_, effects) = stage.key_down(hotkey::VK_DELETE, NONE);
    assert_eq!(effects, [Effect::Send(Command::CapturingHotkey(false))]);
    assert!(matches!(
        &stage.frame().controls[field].kind,
        Kind::Hotkey { text, .. } if text == "F1"
    ));
    assert!(stage.frame().controls[field].clear_rect().is_none());
}

#[test]
fn a_list_opens_picks_and_closes() {
    let mut stage = stage(Page::General);
    let list = find(&stage, |c| c.key == Some("microphone"));
    let effects = click(&mut stage, list);
    let [Effect::OpenChoice(choosing)] = effects.as_slice() else {
        unreachable!("the drop-down was expected to open");
    };
    assert_eq!(choosing.items.len(), 3);
    assert_eq!(choosing.selected, 0);
    assert!(stage.choosing() && stage.look().open == Some(list));
    let picked = stage.picked(2);
    assert_eq!(sends(&picked).len(), 1);
    assert!(!stage.choosing());
    assert!(matches!(
        stage.frame().controls[list].kind,
        Kind::Choice { selected: 2, .. }
    ));
    // A click anywhere closes an open drop-down and does nothing else.
    click(&mut stage, list);
    stage.take_dirty();
    let effects = stage.mouse_down(Point::new(600.0, 30.0));
    assert_eq!(effects, [Effect::CloseChoice]);
}

#[test]
fn what_is_pinned_stays_in_reach_while_the_page_scrolls_under_it() {
    let mut stage = opened(Page::Models, "speech-model");
    let plans = find(&stage, |c| c.name() == "See plans");
    let place = stage.rect_of(plans).unwrap_or_default();
    assert!(stage.frame().scroll_range() > 0.0);
    stage.key_down(vk::END, NONE);
    assert!(stage.look().scroll > 0.0);
    // It has not moved, and nothing that scrolled past takes its click.
    assert_eq!(stage.rect_of(plans), Some(place));
    assert_eq!(stage.control_at(place.center()), Some(plans));
    let card = Point::new(place.x - metric::CONTROL, place.center().y);
    assert_eq!(stage.control_at(card), None);
    // Bringing it into sight scrolls nothing, as it is in sight already.
    let scroll = stage.look().scroll;
    stage.reveal(plans);
    assert!((stage.look().scroll - scroll).abs() < f32::EPSILON);
    click(&mut stage, plans);
    assert_eq!(stage.showing(), &Showing::Settings(Page::Pro));
}

#[test]
fn the_wheel_and_the_keys_scroll_within_bounds() {
    let mut stage = stage(Page::Shortcuts);
    let range = stage.frame().scroll_range();
    assert!(range > 0.0);
    stage.wheel(1.0);
    assert!(stage.look().scroll.abs() < f32::EPSILON);
    stage.wheel(-1.0);
    assert!((stage.look().scroll - metric::WHEEL.min(range)).abs() < 0.01);
    stage.key_down(vk::END, NONE);
    assert!((stage.look().scroll - range).abs() < 0.01);
    stage.key_down(vk::HOME, NONE);
    assert!(stage.look().scroll.abs() < f32::EPSILON);
    // The scroll bar shows while the mouse is over the page, wide at
    // the right edge, where it can be dragged.
    stage.mouse_move(Point::new(600.0, 300.0));
    assert_eq!(stage.look().bar, ScrollBar::Thin);
    let edge = Point::new(936.0, 100.0);
    stage.mouse_move(edge);
    assert_eq!(stage.look().bar, ScrollBar::Wide);
    stage.mouse_down(edge);
    stage.mouse_move(Point::new(936.0, 640.0));
    assert!((stage.look().scroll - range).abs() < 0.01);
    stage.mouse_up(Point::new(936.0, 640.0));
    stage.mouse_leave();
    assert_eq!(stage.look().bar, ScrollBar::Hidden);
}

#[test]
fn focus_survives_a_rebuild_by_key_or_by_place() {
    let mut stage = stage(Page::General);
    let list = find(&stage, |c| c.key == Some("microphone"));
    stage.set_focus(Some(list), true);
    let mut view = mock::scene("notice").unwrap_or_default();
    view.settings = Settings {
        autostart: false,
        ..view.settings
    };
    if let Some(notice) = &mut view.notice {
        notice.page = Page::General;
    }
    // The notice adds a bar with a button above everything: the list
    // moves down the order but keeps the focus.
    stage.set_view(view);
    let now = stage.focused();
    assert!(now.is_some_and(|index| index > list));
    assert_eq!(
        now.and_then(|index| stage.frame().controls[index].key),
        Some("microphone")
    );
}

#[test]
fn a_window_that_asks_answers_with_its_buttons_and_keys() {
    let dialog = Dialog {
        heading: "Install?".to_owned(),
        primary: "Install".to_owned(),
        secondary: "Cancel".to_owned(),
        ..Dialog::default()
    };
    let mut stage = Stage::new(
        Showing::Asking(Asking::new(dialog)),
        View::default(),
        Size::new(460.0, 200.0),
        Box::new(Ruler),
    );
    // Enter with nothing focused presses the accent button.
    assert_eq!(
        stage.key_down(vk::ENTER, NONE),
        (true, vec![Effect::Accept])
    );
    assert_eq!(
        stage.key_down(vk::ESCAPE, NONE),
        (true, vec![Effect::Dismiss])
    );
    let cancel = find(&stage, |c| c.name() == "Cancel");
    assert_eq!(click(&mut stage, cancel), [Effect::Close]);
    // While the work runs nothing can be pressed.
    stage.set_phase(Phase::Working("Installing".to_owned()));
    assert!(stage.moving());
    assert_eq!(stage.key_down(vk::ENTER, NONE), (false, Vec::new()));
    stage.set_phase(Phase::Failed("No.".to_owned()));
    let again = find(&stage, |c| c.name() == "Try again");
    assert_eq!(click(&mut stage, again), [Effect::Accept]);
}

#[test]
fn escape_does_not_close_settings_and_back_goes_back() {
    let mut stage = stage(Page::AddModel);
    assert_eq!(stage.key_down(vk::ESCAPE, NONE), (false, Vec::new()));
    let alt = Modifiers { alt: true, ..NONE };
    let (used, _) = stage.key_down(vk::LEFT, alt);
    assert!(used);
    assert_eq!(stage.showing(), &Showing::Settings(Page::Models));
}

#[test]
fn a_tooltip_belongs_to_a_button_without_a_caption() {
    let mut stage = opened(Page::Models, "speech-model");
    let remove = find(&stage, |c| c.tip().is_some());
    stage.reveal(remove);
    let center = stage.rect_of(remove).unwrap_or_default().center();
    stage.mouse_move(center);
    assert!(
        stage
            .tip()
            .is_some_and(|(tip, _)| tip.starts_with("Remove"))
    );
    assert_eq!(stage.cursor(), Cursor::Arrow);
    stage.mouse_down(center);
    assert!(stage.tip().is_none());
}

#[test]
fn a_hotkey_field_refuses_keys_that_type_and_lets_tab_leave() {
    let mut stage = stage(Page::Shortcuts);
    let field = find(&stage, |c| c.key == Some("hotkey-memo"));
    click(&mut stage, field);
    // A letter by itself, or with Shift, would never type again: the field
    // waits on, and so it does for what Windows keeps for itself.
    let shift = Modifiers {
        shift: true,
        ..NONE
    };
    let alt = Modifiers { alt: true, ..NONE };
    for (key, modifiers) in [
        (0x41, NONE),
        (0x41, shift),
        (vk::SPACE, NONE),
        (vk::TAB, alt),
    ] {
        let (used, effects) = stage.key_down(key, modifiers);
        assert!(used && effects.is_empty(), "key {key:#x}");
        assert!(stage.capturing());
    }
    assert!(matches!(
        &stage.frame().controls[field].kind,
        Kind::Hotkey { text, .. } if text == "Ctrl+Shift+M"
    ));
    // A function key stands alone.
    let (_, effects) = stage.key_down(0x78, NONE);
    assert_eq!(
        sends(&effects).last(),
        Some(&Command::Set(Change::HotkeyMemo("F9".to_owned())))
    );
    // Tab leaves the field, to the next control, and says so once.
    stage.key_down(vk::ENTER, NONE);
    assert!(stage.capturing());
    let (used, effects) = stage.key_down(vk::TAB, NONE);
    assert!(used && !stage.capturing());
    assert_eq!(sends(&effects), [Command::CapturingHotkey(false)]);
    assert!(stage.focused().is_some_and(|index| index != field));
}

#[test]
fn the_background_hears_once_that_a_hotkey_field_waits_and_once_that_none_does() {
    let mut stage = stage(Page::Shortcuts);
    let first = find(&stage, |c| c.key == Some("hotkey"));
    let second = find(&stage, |c| c.key == Some("hotkey-enter"));
    let mut said = sends(&click(&mut stage, first));
    // Assistive tools press a control without the mouse going there.
    said.extend(sends(&stage.press(second)));
    said.extend(sends(&stage.press(second)));
    assert!(stage.capturing());
    // The window goes away, or loses the keyboard.
    said.extend(sends(&stage.blur()));
    said.extend(sends(&stage.blur()));
    let mut waiting = false;
    for command in said {
        let Command::CapturingHotkey(now) = command else {
            continue;
        };
        assert_ne!(now, waiting, "said twice in a row");
        waiting = now;
    }
    assert!(!waiting && !stage.capturing());
}

#[test]
fn the_shape_changes_with_the_page_and_not_with_what_it_says() {
    let mut stage = stage(Page::Models);
    let shape = stage.shape();
    let mut view = mock::scene("settings").unwrap_or_default();
    view.models.disk_bytes += 1 << 30;
    stage.set_view(view.clone());
    assert_eq!(stage.shape(), shape, "only a number changed");
    view.models.list.pop();
    stage.set_view(view);
    assert_ne!(stage.shape(), shape, "a row went");
    let shape = stage.shape();
    stage.show(Page::About);
    assert_ne!(stage.shape(), shape, "another page");
}

#[test]
fn a_field_that_can_no_longer_be_typed_in_loses_its_edit_control() {
    let mut stage = stage(Page::AddModel);
    let field = find(&stage, |c| matches!(c.kind, Kind::Field { .. }));
    let effects = click(&mut stage, field);
    assert!(matches!(effects.as_slice(), [Effect::Edit(_)]));
    assert_eq!(stage.look().editing, Some(field));
    let mut view = mock::scene("add-model").unwrap_or_default();
    view.finder.busy = Some("Looking".to_owned());
    let effects = stage.set_view(view);
    let enabled =
        stage.frame().controls[find(&stage, |c| matches!(c.kind, Kind::Field { .. }))].enabled;
    assert_eq!(effects.contains(&Effect::EndEdit), !enabled);
    assert_eq!(stage.look().editing.is_some(), enabled);
}

#[test]
fn a_list_opens_with_the_keys_windows_gives_it() {
    let alt = Modifiers { alt: true, ..NONE };
    for (key, modifiers) in [(vk::F4, NONE), (vk::DOWN, alt)] {
        let mut stage = stage(Page::General);
        let list = find(&stage, |c| c.key == Some("microphone"));
        stage.focus_on(list);
        let (used, effects) = stage.key_down(key, modifiers);
        assert!(used && matches!(effects.as_slice(), [Effect::OpenChoice(_)]));
    }
    // Alt+F4 is still the window's.
    let mut stage = stage(Page::General);
    let list = find(&stage, |c| c.key == Some("microphone"));
    stage.focus_on(list);
    assert!(!stage.key_down(vk::F4, alt).0);
}

#[test]
fn a_row_that_opens_is_opened_and_closed_by_a_click_and_by_the_keyboard() {
    let mut stage = stage(Page::Models);
    let count = stage.frame().controls.len();
    let head = find(&stage, |c| c.key == Some("speech-model"));
    assert!(matches!(
        stage.frame().controls[head].kind,
        Kind::Expander { open: false, .. }
    ));
    let shape = stage.shape();
    assert!(click(&mut stage, head).is_empty());
    let head = find(&stage, |c| c.key == Some("speech-model"));
    assert!(matches!(
        stage.frame().controls[head].kind,
        Kind::Expander { open: true, .. }
    ));
    assert!(stage.frame().controls.len() > count);
    assert_ne!(stage.shape(), shape, "rows came");
    // The keyboard stays on the row, and Enter closes it again.
    assert_eq!(stage.focused(), Some(head));
    let (used, _) = stage.key_down(vk::ENTER, NONE);
    assert!(used);
    assert_eq!(stage.frame().controls.len(), count);
    // It stays as the user left it when a new view arrives.
    click(&mut stage, head);
    stage.set_view(mock::scene("listening").unwrap_or_default());
    assert!(stage.frame().controls.len() > count);
    // And is as the page says again on another visit.
    stage.show(Page::About);
    stage.show(Page::Models);
    assert_eq!(stage.frame().controls.len(), count);
}

#[test]
fn a_list_scrolls_by_itself_under_the_wheel_and_then_the_page_goes_on() {
    let mut stage = opened(Page::Models, "speech-model");
    let [pane] = stage.frame().panes() else {
        unreachable!("the list of models was expected to scroll");
    };
    let (area, range) = (pane.rect, pane.range());
    assert!(range > 0.0);
    let first = find(&stage, |c| c.pane() == Some(0));
    let before = stage.rect_of(first).unwrap_or_default();
    // Over the list, the wheel moves the list and not the page.
    stage.mouse_move(area.center());
    stage.wheel(-1.0);
    assert!((stage.look().pane(0) - metric::WHEEL.min(range)).abs() < 0.01);
    assert!(stage.look().scroll.abs() < f32::EPSILON);
    let after = stage.rect_of(first).unwrap_or_default();
    assert!((before.y - after.y - metric::WHEEL.min(range)).abs() < 0.01);
    // At its end the page takes over.
    for _ in 0..40 {
        stage.wheel(-1.0);
    }
    assert!((stage.look().pane(0) - range).abs() < 0.01);
    assert!(stage.look().scroll > 0.0);
    // Elsewhere on the page the wheel never touches the list.
    stage.key_down(vk::HOME, NONE);
    stage.mouse_move(Point::new(area.center().x, 30.0));
    stage.wheel(1.0);
    assert!((stage.look().pane(0) - range).abs() < 0.01);
}

#[test]
fn what_is_scrolled_out_of_a_list_takes_no_click_and_tab_brings_it_back() {
    let mut stage = opened(Page::Models, "speech-model");
    let area = stage.frame().panes()[0].rect;
    let inside: Vec<usize> = (0..stage.frame().controls.len())
        .filter(|index| stage.frame().controls[*index].pane() == Some(0))
        .collect();
    let (first, last) = (inside[0], inside[inside.len() - 1]);
    // The last row is below the edge of the list: nothing of it is hit,
    // though the page under the list has things there.
    let below = stage.rect_of(last).unwrap_or_default();
    assert!(below.y > area.bottom());
    assert_ne!(stage.control_at(below.center()), Some(last));
    // Giving it the keyboard scrolls the list, and only the list, to it.
    stage.focus_on(last);
    let shown = stage.rect_of(last).unwrap_or_default();
    assert!(shown.y >= area.y && shown.bottom() <= area.bottom() + 0.01);
    assert!(stage.look().pane(0) > 0.0);
    assert_eq!(stage.control_at(shown.center()), Some(last));
    // Now the first row is above the list, and is not hit there.
    let above = stage.rect_of(first).unwrap_or_default();
    assert!(above.bottom() <= area.y + 0.01);
    assert_ne!(stage.control_at(above.center()), Some(first));
}

#[test]
fn the_scroll_bar_of_a_list_is_dragged_and_clicked_like_that_of_the_page() {
    let mut stage = opened(Page::Models, "speech-model");
    let pane = &stage.frame().panes()[0];
    let (area, range) = (pane.rect, pane.range());
    let lane = Point::new(area.right() - 4.0, area.y + 12.0);
    stage.mouse_move(area.center());
    assert_eq!(stage.look().pane_bar, None);
    stage.mouse_move(lane);
    assert_eq!(stage.look().pane_bar, Some(0));
    // Dragging the thumb to the bottom shows the end of the list.
    stage.mouse_down(lane);
    stage.mouse_move(Point::new(lane.x, area.bottom() + 200.0));
    assert!((stage.look().pane(0) - range).abs() < 0.01);
    assert_eq!(stage.look().pane_bar, Some(0), "held while it is dragged");
    stage.mouse_up(Point::new(lane.x, area.bottom() + 200.0));
    // A click above the thumb goes back by the height of the list.
    stage.mouse_move(lane);
    stage.mouse_down(lane);
    stage.mouse_up(lane);
    assert!((stage.look().pane(0) - (range - area.h).max(0.0)).abs() < 0.01);
    assert!(stage.look().scroll.abs() < f32::EPSILON);
}

#[test]
fn typing_in_a_search_narrows_its_list_at_every_key() {
    let mut stage = opened(Page::General, "languages");
    let rows = |stage: &Stage| {
        let controls = stage.frame().controls.iter();
        controls
            .filter(|c| matches!(c.kind, Kind::Row { .. }))
            .count()
    };
    let all = rows(&stage);
    assert!(all > 60);
    // The list is scrolled, to see that a search starts it from its top.
    let area = stage.frame().panes()[0].rect;
    stage.reveal(find(&stage, |c| c.key == Some("language-search")));
    let field = find(&stage, |c| c.key == Some("language-search"));
    let effects = click(&mut stage, field);
    assert!(matches!(effects.as_slice(), [Effect::Edit(editing)] if editing.text.is_empty()));
    let _ = area;
    assert!(stage.typed("slov".to_owned()).is_empty());
    assert_eq!(rows(&stage), 2, "Slovak and Slovenian");
    assert!(stage.look().pane(0).abs() < f32::EPSILON);
    // The field keeps the keyboard and what was typed through the rebuild.
    let field = find(&stage, |c| c.key == Some("language-search"));
    assert_eq!(stage.look().editing, Some(field));
    assert!(matches!(
        &stage.frame().controls[field].kind,
        Kind::Field { text, .. } if text == "slov"
    ));
    // Enter and leaving send nothing: the search is nobody's setting.
    assert!(stage.edit_enter().is_empty());
    assert_eq!(stage.edit_lost(), [Effect::EndEdit]);
    assert_eq!(rows(&stage), 2, "the search stays while the page does");
    // Clicking a row adds its language.
    let slovak = find(&stage, |c| {
        matches!(c.kind, Kind::Row { .. }) && c.label == "Sloven\u{10d}ina"
    });
    assert_eq!(
        sends(&click(&mut stage, slovak)),
        [Command::Set(Change::Languages(
            "en-US, sl-SI, sk-SK".to_owned()
        ))]
    );
}

#[test]
fn the_cross_of_a_chip_takes_its_language_away() {
    let mut stage = opened(Page::General, "languages");
    let chip = find(
        &stage,
        |c| matches!(&c.kind, Kind::Chip { label, .. } if label.starts_with("English")),
    );
    assert_eq!(
        stage.frame().controls[chip].name(),
        "Remove English (United States)"
    );
    assert_eq!(
        sends(&click(&mut stage, chip)),
        [Command::Set(Change::Languages("sl-SI".to_owned()))]
    );
    // The only language left has no cross, and Tab passes it by.
    let mut view = mock::scene("settings").unwrap_or_default();
    "sl-SI".clone_into(&mut view.settings.languages);
    stage.set_view(view);
    let last = find(&stage, |c| matches!(c.kind, Kind::Chip { .. }));
    assert!(!stage.frame().controls[last].enabled);
    assert!(click(&mut stage, last).is_empty());
}
