//! Tests of [`super`].

use vinput_view::{Change, Command, Page};

use super::build::{BULLET, LIST_GAP};
use super::ruler::Ruler;
use super::*;
use crate::icons;
use crate::node::{
    Node, accent_button, bullets, button, card, choice, field, header, info_bar, row, setting,
    text, toggle,
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
fn a_card_stacks_rows_of_at_least_the_standard_height_with_hairlines_between() {
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
    let rows = 3.0 * metric::ROW_MIN + 2.0 * metric::LINE;
    assert!((height - rows).abs() < 0.01, "{height}");
    let dividers = laid
        .body
        .iter()
        .filter(|item| matches!(item, Item::Divider(_)))
        .count();
    assert_eq!(dividers, 2);
    assert!(matches!(laid.body.first(), Some(Item::Card(rect)) if (rect.h - rows).abs() < 0.01));
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
    assert!(laid.controls[0].corners.top && !laid.controls[0].corners.bottom);
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
