//! Tests of [`super`].

use vinput_view::{Change, Command, Page};

use super::build::{BULLET, LIST_GAP};
use super::ruler::Ruler;
use super::*;
use crate::icons;
use crate::node::{
    Node, Pickers, Search, accent_button, bullets, button, card, chip, choice, expander, field,
    flow, header, info_bar, plain_setting, row, scroll, setting, text, toggle,
};

fn lay(nodes: Vec<Node>, w: f32) -> (Laid, f32) {
    let mut builder = Builder::new(&Ruler);
    builder.region(Region::Body);
    let height = builder.column(nodes, 0.0, 0.0, w);
    (builder.finish(), height)
}

fn switch() -> Node {
    toggle(true, |on| Action::send(Command::Set(Change::Autostart(on))))
}

#[test]
fn a_card_stacks_its_rows_as_tiles_of_at_least_the_standard_height_a_little_apart() {
    let (laid, height) = lay(
        vec![card([
            setting(Icon::Power, "Start with Windows")
                .control(switch())
                .into(),
            setting(Icon::Volume, "Sound").detail("A soft one").into(),
            setting(Icon::Sync, "Updates").into(),
        ])],
        600.0,
    );
    let rows = 3.0 * metric::ROW_MIN + 2.0 * metric::TILE_GAP;
    assert!((height - rows).abs() < 0.01, "{height}");
    // No hairlines: the gap between the tiles parts them.
    assert!(
        !laid
            .body
            .iter()
            .any(|item| matches!(item, Item::Divider(_)))
    );
    let tiles: Vec<Rect> = laid
        .body
        .iter()
        .filter_map(|item| match item {
            Item::Card(rect) => Some(*rect),
            _ => None,
        })
        .collect();
    assert_eq!(tiles.len(), 3);
    let mut top = 0.0;
    for tile in &tiles {
        assert!((tile.y - top).abs() < 0.01 && (tile.h - metric::ROW_MIN).abs() < 0.01);
        top += metric::ROW_MIN + metric::TILE_GAP;
    }
    // The switch sits at the right edge, inside the padding, centered.
    let [control] = laid.controls.as_slice() else {
        unreachable!("one control was expected");
    };
    assert!((control.rect.right() - (600.0 - metric::ROW_PAD)).abs() < 0.01);
    assert!((control.rect.center().y - metric::ROW_MIN / 2.0).abs() < 0.01);
    assert_eq!(control.name(), "Start with Windows");
    assert_eq!(control.region, Region::Body);
}

#[test]
fn a_long_detail_wraps_and_makes_its_row_taller() {
    let detail = "word ".repeat(60);
    let tall = lay(
        vec![setting(Icon::Info, "Title").detail(&detail).into()],
        400.0,
    )
    .1;
    let wide = lay(
        vec![setting(Icon::Info, "Title").detail(&detail).into()],
        900.0,
    )
    .1;
    assert!(tall > wide && wide > metric::ROW_MIN, "{tall} {wide}");
}

#[test]
fn a_wide_control_leaves_the_title_its_least_width() {
    let items = vec!["x".repeat(200)];
    let (laid, _) = lay(
        vec![
            setting(Icon::Microphone, "Audio input")
                .control(choice("microphone", items, 0, |_| Action::None))
                .into(),
        ],
        420.0,
    );
    let title = laid.body.iter().find_map(|item| match item {
        Item::Text { rect, text, .. } if text == "Audio input" => Some(*rect),
        _ => None,
    });
    let Some(title) = title else {
        unreachable!("the title was expected");
    };
    assert!(title.w >= metric::ROW_TEXT_MIN - 0.01, "{}", title.w);
    let list = &laid.controls[0];
    assert!(list.rect.x >= title.right());
    assert!(list.rect.right() <= 420.0 - metric::ROW_PAD + 0.01);
    assert_eq!(list.key, Some("microphone"));
}

#[test]
fn a_spring_pushes_what_follows_to_the_right_edge() {
    let (laid, height) = lay(
        vec![row([
            text("Left"),
            Node::Spring,
            button("One", Action::Close).into(),
            button("Two", Action::Close).into(),
        ])],
        500.0,
    );
    assert!((height - metric::CONTROL).abs() < 0.01);
    let [one, two] = laid.controls.as_slice() else {
        unreachable!("two buttons were expected");
    };
    assert!((two.rect.right() - 500.0).abs() < 0.01);
    assert!((two.rect.x - one.rect.right() - metric::GAP).abs() < 0.01);
}

#[test]
fn a_wide_field_takes_the_room_its_neighbors_leave() {
    let (laid, _) = lay(
        vec![row([
            field("source", "", |_| Action::None).wide().into(),
            button("Find", Action::Close).into(),
        ])],
        500.0,
    );
    let [source, find] = laid.controls.as_slice() else {
        unreachable!("a field and a button were expected");
    };
    assert!((find.rect.right() - 500.0).abs() < 0.01);
    assert!((source.rect.w - (500.0 - metric::GAP - find.rect.w)).abs() < 0.01);
}

#[test]
fn a_row_that_goes_somewhere_is_one_control_under_its_own() {
    let (laid, _) = lay(
        vec![card([
            setting(Icon::Add, "Add a model")
                .action(Action::Go(Page::AddModel))
                .control(button("Now", Action::Close).into())
                .into(),
            setting(Icon::Globe, "Website")
                .action(Action::send(Command::OpenLink("https://x".to_owned())))
                .into(),
            setting(Icon::Globe, "Out of reach")
                .action(Action::Go(Page::About))
                .enabled(false)
                .into(),
        ])],
        600.0,
    );
    let kinds: Vec<u8> = laid.controls.iter().map(|c| c.kind.rank()).collect();
    assert_eq!(kinds, [6, 0, 6]);
    // Each row is a tile of its own, so each has all four corners round.
    assert!(laid.controls[0].corners.top && laid.controls[0].corners.bottom);
    let glyphs: Vec<char> = laid
        .body
        .iter()
        .filter_map(|item| match item {
            Item::Glyph { glyph, size, .. } if (*size - metric::ICON_SMALL).abs() < 0.01 => {
                Some(*glyph)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        glyphs,
        [
            icons::glyph(Icon::ChevronRight),
            icons::glyph(Icon::OpenExternal),
            icons::glyph(Icon::ChevronRight)
        ]
    );
}

#[test]
fn headings_get_more_room_above_than_other_things() {
    let (laid, _) = lay(
        vec![header("One"), text("a"), header("Two"), text("b")],
        300.0,
    );
    let tops: Vec<f32> = laid
        .body
        .iter()
        .filter_map(|item| match item {
            Item::Text { rect, .. } => Some(rect.y),
            _ => None,
        })
        .collect();
    assert!((tops[1] - tops[0] - 20.0 - metric::GAP).abs() < 0.01);
    assert!((tops[2] - tops[1] - 20.0 - metric::GAP - metric::HEADER_ABOVE).abs() < 0.01);
}

#[test]
fn footer_buttons_sit_at_the_right_edge_with_the_accent_one_first() {
    let mut builder = Builder::new(&Ruler);
    let rect = Rect::new(0.0, 100.0, 460.0, 64.0);
    builder.footer(
        vec![
            button("Cancel", Action::Close).into(),
            accent_button("Install", Action::Accept).into(),
        ],
        rect,
    );
    let laid = builder.finish();
    let [first, second] = laid.controls.as_slice() else {
        unreachable!("two buttons were expected");
    };
    assert_eq!((first.name(), second.name()), ("Install", "Cancel"));
    assert!((second.rect.right() - 460.0).abs() < 0.01);
    assert!(first.rect.right() < second.rect.x);
    assert!((first.rect.w - metric::BUTTON_MIN_FOOTER).abs() < 0.01);
    assert!((first.rect.center().y - 132.0).abs() < 0.01);
    assert_eq!(first.region, Region::Fixed);
}

#[test]
fn a_message_bar_keeps_its_buttons_clear_of_its_text() {
    let words = "word ".repeat(40);
    let (laid, height) = lay(
        vec![Node::InfoBar {
            severity: Severity::Critical,
            text: words.clone(),
            action: Some(("Retry".to_owned(), Action::Close)),
            dismiss: Some(Action::Close),
        }],
        480.0,
    );
    assert!(height > metric::BAR_MIN);
    let text = laid.body.iter().find_map(|item| match item {
        Item::Text { rect, .. } => Some(*rect),
        _ => None,
    });
    let Some(text) = text else {
        unreachable!("the text was expected");
    };
    let [retry, dismiss] = laid.controls.as_slice() else {
        unreachable!("two buttons were expected");
    };
    assert!(text.right() <= retry.rect.x && retry.rect.right() <= dismiss.rect.x);
    assert_eq!(dismiss.tip(), Some("Dismiss"));
    let short = lay(vec![info_bar(Severity::Info, "Short")], 480.0).1;
    assert!((short - metric::BAR_MIN).abs() < 0.01);
}

#[test]
fn a_list_puts_each_point_behind_a_bullet() {
    let (laid, height) = lay(vec![bullets(["One", "", "Two"])], 300.0);
    let texts: Vec<&str> = laid
        .body
        .iter()
        .filter_map(|item| match item {
            Item::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, [BULLET, "One", BULLET, "Two"]);
    assert!((height - 40.0 - LIST_GAP).abs() < 0.01);
}

#[test]
fn a_hotkey_field_can_be_cleared_only_when_it_may_and_holds_something() {
    let make = |text: &str, clearable| {
        lay(
            vec![crate::node::hotkey("k", text, clearable, |_| Action::None)],
            300.0,
        )
        .0
    };
    assert!(make("F2", true).controls[0].clear_rect().is_some());
    assert!(make("", true).controls[0].clear_rect().is_none());
    assert!(make("F1", false).controls[0].clear_rect().is_none());
}

/// Lays `nodes` out as [`lay`] does, with `memory` of what the user did.
fn lay_with(nodes: Vec<Node>, w: f32, memory: &Memory) -> (Laid, f32) {
    let mut builder = Builder::new(&Ruler).remembering(memory);
    builder.region(Region::Body);
    let height = builder.column(nodes, 0.0, 0.0, w);
    (builder.finish(), height)
}

fn says(laid: &Laid, words: &str) -> bool {
    laid.body
        .iter()
        .any(|item| matches!(item, Item::Text { text, .. } if text == words))
}

fn opener(open: bool) -> Node {
    expander(
        "opener",
        Icon::Model,
        "Speech model",
        [
            setting(Icon::Model, "Whisper base").into(),
            setting(Icon::Model, "Whisper small")
                .control(button("Use", Action::Close).into())
                .into(),
        ],
    )
    .value("Whisper base")
    .open(open)
    .into()
}

#[test]
fn a_row_that_opens_shows_what_is_chosen_and_hides_the_rest_until_it_is_open() {
    let (closed, height) = lay(vec![opener(false)], 600.0);
    assert!((height - metric::ROW_MIN).abs() < 0.01, "{height}");
    assert!(says(&closed, "Speech model") && says(&closed, "Whisper base"));
    assert!(!says(&closed, "Whisper small"));
    let [head] = closed.controls.as_slice() else {
        unreachable!("the closed row is one control");
    };
    assert!(matches!(head.kind, Kind::Expander { open: false, .. }));
    assert_eq!((head.key, head.name()), (Some("opener"), "Speech model"));
    assert_eq!(head.corners, Corners::ALL);
    // The chevron points down while it is closed and up while it is open.
    let chevron = |laid: &Laid, glyph: char| {
        laid.body
            .iter()
            .any(|item| matches!(item, Item::Glyph { glyph: drawn, .. } if *drawn == glyph))
    };
    assert!(chevron(&closed, icons::CHEVRON_DOWN) && !chevron(&closed, icons::CHEVRON_UP));

    let (open, height) = lay(vec![opener(true)], 600.0);
    let rows = 3.0 * metric::ROW_MIN + 2.0 * metric::LINE;
    assert!((height - rows).abs() < 0.01, "{height}");
    assert!(says(&open, "Whisper small") && chevron(&open, icons::CHEVRON_UP));
    assert_eq!(open.controls.len(), 2);
    // Open, the row no longer reaches the bottom corners of its card.
    assert!(open.controls[0].corners.top && !open.controls[0].corners.bottom);
    assert!(open.controls[1].rect.y > open.controls[0].rect.bottom());
}

#[test]
fn what_the_user_did_to_a_row_that_opens_outweighs_how_it_starts() {
    let mut memory = Memory::default();
    memory.set_open("opener", true);
    let (laid, _) = lay_with(vec![opener(false)], 600.0, &memory);
    assert!(says(&laid, "Whisper small"));
    memory.set_open("opener", false);
    let (laid, height) = lay_with(vec![opener(true)], 600.0, &memory);
    assert!(!says(&laid, "Whisper small"));
    assert!((height - metric::ROW_MIN).abs() < 0.01);
    // A row with nothing under it cannot be opened.
    let empty: Node = expander("empty", Icon::Model, "Nothing", [])
        .open(true)
        .into();
    let (laid, height) = lay(vec![empty], 600.0);
    assert!((height - metric::ROW_MIN).abs() < 0.01);
    assert!(!laid.controls[0].enabled);
}

fn rows(count: u8) -> Vec<Node> {
    (1..=count)
        .map(|number| {
            setting(Icon::Model, format!("Model {number}"))
                .control(button("Use", Action::Close).into())
                .into()
        })
        .collect()
}

#[test]
fn a_long_list_is_cut_off_at_its_height_and_scrolls_by_itself() {
    let most = 2.5 * metric::ROW_MIN;
    let (laid, height) = lay(vec![card([scroll("list", most, rows(6))])], 600.0);
    assert!((height - most).abs() < 0.01, "{height}");
    let [pane] = laid.panes.as_slice() else {
        unreachable!("one area that scrolls was expected");
    };
    let all = 6.0 * metric::ROW_MIN + 5.0 * metric::LINE;
    assert_eq!((pane.key, pane.region), ("list", Region::Body));
    assert!((pane.rect.h - most).abs() < 0.01 && (pane.content - all).abs() < 0.01);
    assert!((pane.range() - (all - most)).abs() < 0.01);
    // Everything in it is laid out where it is before any scrolling, knows
    // the area it is in, and is painted between the two marks.
    assert_eq!(laid.controls.len(), 6);
    assert!(
        laid.controls
            .iter()
            .all(|control| control.pane() == Some(0))
    );
    assert!(laid.controls[5].rect.y > pane.rect.bottom());
    let at = |mark: &Item| laid.body.iter().position(|item| item == mark);
    let (clip, unclip) = (at(&Item::Clip(0, Pickers)), at(&Item::Unclip));
    assert!(clip.is_some() && clip < unclip);
    assert_eq!(unclip, Some(laid.body.len() - 1));
    // Which is how anything painted is told to be in the list or not.
    let inside = clip.map_or(0, |clip| clip + 1);
    assert_eq!(crate::look::pane_of(&laid.body, inside), Some(0));
    assert_eq!(crate::look::pane_of(&laid.body, 0), None);
    assert_eq!(crate::look::pane_of(&laid.body, laid.body.len()), None);
}

#[test]
fn a_list_that_fits_does_not_scroll() {
    let (laid, height) = lay(
        vec![card([scroll("list", 10.0 * metric::ROW_MIN, rows(2))])],
        600.0,
    );
    assert!((height - (2.0 * metric::ROW_MIN + metric::LINE)).abs() < 0.01);
    assert!(laid.panes.is_empty());
    assert!(laid.controls.iter().all(|control| control.pane().is_none()));
    assert!(!laid.body.contains(&Item::Unclip));
}

#[test]
fn a_row_of_a_long_list_is_lower_than_a_row_of_settings() {
    let (_, usual) = lay(vec![plain_setting("Deutsch").into()], 600.0);
    let (_, low) = lay(vec![plain_setting("Deutsch").compact().into()], 600.0);
    assert!((usual - metric::ROW_MIN).abs() < 0.01);
    assert!((low - metric::ROW_COMPACT).abs() < 0.01);
    // With a second line it grows only as far as its text needs.
    let (_, two) = lay(
        vec![plain_setting("Deutsch").detail("German").compact().into()],
        600.0,
    );
    assert!(two > low && two < usual, "{two}");
}

fn finder() -> Node {
    let names = [
        "Deutsch German",
        "English",
        "Sloven\u{161}\u{10d}ina Slovenian",
    ];
    Search {
        key: "find",
        placeholder: "Search languages".to_owned(),
        max: 10.0 * metric::ROW_COMPACT,
        rows: names
            .iter()
            .map(|name| {
                let title = name.split(' ').next().unwrap_or(name);
                ((*name).to_owned(), plain_setting(title).compact().into())
            })
            .collect(),
        none: "No language by that name".to_owned(),
        pickers: Pickers,
    }
    .into()
}

#[test]
fn a_search_shows_the_rows_that_have_what_is_typed() {
    let shown = |typed: &str| {
        let mut memory = Memory::default();
        memory.set_typed("find", typed.to_owned());
        let (laid, height) = lay_with(vec![card([finder()])], 600.0, &memory);
        let names = ["Deutsch", "English", "Sloven\u{161}\u{10d}ina"];
        let found: Vec<_> = names.into_iter().filter(|name| says(&laid, name)).collect();
        (found, says(&laid, "No language by that name"), height)
    };
    let (all, none, tall) = shown("");
    assert_eq!(all, ["Deutsch", "English", "Sloven\u{161}\u{10d}ina"]);
    assert!(!none);
    // By either name, whatever the case, with room around it.
    assert_eq!(shown(" GERMAN ").0, ["Deutsch"]);
    assert_eq!(shown("slo").0, ["Sloven\u{161}\u{10d}ina"]);
    assert_eq!(shown("\u{160}\u{10c}").0, ["Sloven\u{161}\u{10d}ina"]);
    let (some, _, lower) = shown("e");
    assert_eq!(some.len(), 3);
    assert!((tall - lower).abs() < 0.01);
    let (nothing, none, low) = shown("klingon");
    assert!(nothing.is_empty() && none && low < tall);
    // The field is one that the page follows as it is typed in.
    let (laid, _) = lay(vec![card([finder()])], 600.0);
    let [field] = laid.controls.as_slice() else {
        unreachable!("the field was expected, and rows that do nothing");
    };
    assert!(matches!(field.kind, Kind::Field { live: Some(_), .. }));
    assert_eq!(
        (field.key, field.name()),
        (Some("find"), "Search languages")
    );
}

#[test]
fn chips_go_on_in_a_new_line_where_the_width_runs_out() {
    let chips = |count: usize| -> Vec<Node> {
        (0..count)
            .map(|_| chip("Sloven\u{161}\u{10d}ina", Some(Action::Close)))
            .collect()
    };
    let (one, height) = lay(vec![flow(chips(2))], 600.0);
    assert!((height - metric::CHIP).abs() < 0.01, "{height}");
    assert!((one.controls[0].rect.y - one.controls[1].rect.y).abs() < 0.01);
    assert!(one.controls[1].rect.x > one.controls[0].rect.right());
    let (many, height) = lay(vec![flow(chips(12))], 600.0);
    assert!(height > 2.0 * metric::CHIP, "{height}");
    assert!(many.controls.iter().all(|chip| chip.rect.right() <= 600.01));
    // A chip is named by what its cross does; one that stays cannot be used.
    assert_eq!(one.controls[0].name(), "Remove Sloven\u{161}\u{10d}ina");
    let (fixed, _) = lay(vec![flow([chip("English", None)])], 600.0);
    assert!(!fixed.controls[0].enabled);
    assert_eq!(fixed.controls[0].name(), "English");
    assert!(fixed.controls[0].rect.w < one.controls[0].rect.w);
}
