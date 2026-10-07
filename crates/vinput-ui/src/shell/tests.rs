//! Tests of [`super`].

use super::*;
use crate::layout::ruler::Ruler;
use crate::node::{accent_button, button, card, setting, text};

fn settled() -> Status {
    Status {
        listening: false,
        model: Some("Whisper base".to_owned()),
        runs_on: RunsOn::GraphicsCard("NVIDIA GeForce RTX 4070".to_owned()),
    }
}

fn page(rows: usize) -> Content {
    Content {
        pinned: Vec::new(),
        title: "Models".to_owned(),
        body: vec![card(
            (0..rows).map(|_| setting(Icon::Model, "A model").into()),
        )],
        footer: Vec::new(),
        back: Some(Action::Go(Page::Models)),
    }
}

#[test]
fn the_settings_window_has_a_pane_a_title_and_a_body_that_scrolls() {
    let status = settled();
    let chrome = Chrome::Settings {
        page: Page::AddModel,
        status: &status,
    };
    let frame = frame(page(20), chrome, Size::new(940.0, 660.0), &Ruler);
    assert_eq!(frame.pane.map(|pane| pane.w), Some(metric::PANE));
    assert!((frame.sheet.x - metric::PANE).abs() < 0.01);
    assert!(frame.scroll_range() > 0.0);
    assert!((frame.viewport.bottom() - 660.0).abs() < 0.01);
    // Four places to go, then the way back; a page reached from Models
    // shows Models as the open one.
    let selected: Vec<bool> = frame
        .controls
        .iter()
        .filter_map(|control| match &control.kind {
            Kind::Nav { selected, .. } => Some(*selected),
            _ => None,
        })
        .collect();
    assert_eq!(selected, [false, true, false, false, false]);
    assert_eq!(frame.controls[5].name(), "Back");
    assert!(frame.controls[5].rect.right() <= metric::PANE + metric::PAGE_PAD + metric::CONTROL);
    let says = |part: &str| {
        frame
            .fixed
            .iter()
            .any(|item| matches!(item, Item::Text { text, .. } if text.contains(part)))
    };
    assert!(says("Ready") && says("Whisper base") && says("RTX 4070"));
    assert!(!frame.moving);
}

#[test]
fn a_short_page_does_not_scroll() {
    let status = Status::default();
    let chrome = Chrome::Settings {
        page: Page::General,
        status: &status,
    };
    let frame = frame(page(2), chrome, Size::new(940.0, 660.0), &Ruler);
    assert!(frame.scroll_range() <= 0.0);
    // Without a model the pane says so and offers the way to one.
    assert!(
        frame
            .controls
            .iter()
            .any(|control| control.name() == "Choose a model")
    );
}

#[test]
fn what_is_pinned_stays_under_the_title_while_the_body_scrolls_under_it() {
    let status = settled();
    let chrome = Chrome::Settings {
        page: Page::Models,
        status: &status,
    };
    let size = Size::new(940.0, 660.0);
    let pinned = |rows: usize| Content {
        pinned: vec![card([setting(Icon::Star, "Pinned")
            .control(button("Open", Action::Go(Page::Pro)).into())
            .into()])],
        back: None,
        ..page(rows)
    };
    let plain = frame(
        Content {
            back: None,
            ..page(20)
        },
        chrome,
        size,
        &Ruler,
    );
    let frame = frame(pinned(20), chrome, size, &Ruler);
    let card_in = |items: &[Item]| {
        let found = items.iter().find_map(|item| match item {
            Item::Card(rect) => Some(*rect),
            _ => None,
        });
        found.unwrap_or_default()
    };
    // The card is among what stays put, where the body would have begun,
    // on the same grid as the cards of the body.
    let (card, first) = (card_in(&frame.fixed), card_in(&frame.body));
    assert!((card.y - card_in(&plain.body).y).abs() < 0.01);
    assert!((card.x - first.x).abs() < 0.01 && (card.w - first.w).abs() < 0.01);
    assert!((card.h - metric::ROW_MIN).abs() < 0.01);
    // The body begins under it and is cut off under it, so nothing that
    // scrolls is ever drawn over it or takes a click meant for it.
    assert!((first.y - (card.bottom() + metric::PINNED_GAP)).abs() < 0.01);
    assert!(frame.viewport.y >= card.bottom() + metric::GAP);
    let room = card.h + metric::PINNED_GAP;
    assert!((plain.viewport.h - frame.viewport.h - room).abs() < 0.01);
    assert!((frame.content_height - plain.content_height).abs() < 0.01);
    assert!((frame.scroll_range() - plain.scroll_range() - room).abs() < 0.01);
    // Its button is worked like the title's: it does not move, and Tab
    // reaches it after the pane and before anything in the body.
    let open = frame
        .controls
        .iter()
        .position(|control| control.name() == "Open");
    let Some(open) = open else {
        unreachable!("the pinned button was expected");
    };
    assert_eq!(frame.controls[open].region, Region::Fixed);
    assert!(card.contains(frame.controls[open].rect.center()));
    assert_eq!(open, pages::NAVIGATION.len());
    assert!(
        frame.controls[open + 1..]
            .iter()
            .all(|control| control.region == Region::Body)
    );
    // A small window counts it into its height.
    let small = |content: &Content| compact_height(content, metric::DIALOG_W, &Ruler);
    let without = Content {
        pinned: Vec::new(),
        ..pinned(3)
    };
    assert!((small(&pinned(3)) - small(&without) - room).abs() < 0.01);
}

#[test]
fn a_small_window_is_as_tall_as_what_it_says() {
    let content = || Content {
        pinned: Vec::new(),
        title: "Install VinPUT 0.3.0".to_owned(),
        body: vec![text("Type with your voice into any text field.")],
        footer: vec![
            accent_button("Install", Action::Accept).into(),
            button("Cancel", Action::Close).into(),
        ],
        back: None,
    };
    let height = compact_height(&content(), metric::DIALOG_W, &Ruler);
    assert!((metric::DIALOG_MIN_H..metric::DIALOG_MAX_H).contains(&height));
    let size = Size::new(metric::DIALOG_W, height);
    let frame = frame(content(), Chrome::Compact, size, &Ruler);
    assert!(frame.pane.is_none() && frame.strip.is_some());
    assert!(frame.scroll_range() <= 0.01, "{}", frame.scroll_range());
    let [install, cancel] = frame.controls.as_slice() else {
        unreachable!("two buttons were expected");
    };
    assert!(install.rect.x < cancel.rect.x);
    assert!((cancel.rect.right() - (metric::DIALOG_W - metric::DIALOG_PAD)).abs() < 0.01);
}

/// A ruler for text twice as large as usual.
struct Large;

impl Measure for Large {
    fn text(&self, text: &str, style: TextStyle, max_width: f32) -> Size {
        let size = Ruler.text(text, style, max_width / 2.0);
        Size::new(size.w * 2.0, size.h * 2.0)
    }
}

#[test]
fn a_page_sits_in_the_middle_of_a_window_wider_than_pages_get() {
    let status = settled();
    let chrome = Chrome::Settings {
        page: Page::Models,
        status: &status,
    };
    let card_of = |frame: &Frame| {
        let found = frame.body.iter().find_map(|item| match item {
            Item::Card(rect) => Some(*rect),
            _ => None,
        });
        found.unwrap_or_default()
    };
    let usual = card_of(&frame(page(2), chrome, Size::new(940.0, 660.0), &Ruler));
    assert!((usual.x - (metric::PANE + metric::PAGE_PAD)).abs() < 0.01);
    let wide = frame(page(2), chrome, Size::new(2000.0, 900.0), &Ruler);
    let card = card_of(&wide);
    assert!((card.w - metric::PAGE_MAX).abs() < 0.01);
    let (left, right) = (card.x - metric::PANE, 2000.0 - card.right());
    assert!((left - right).abs() <= 1.0, "{left} and {right}");
}

#[test]
fn larger_text_gets_taller_controls_and_pane_entries() {
    let status = settled();
    let chrome = Chrome::Settings {
        page: Page::Models,
        status: &status,
    };
    let content = || Content {
        footer: vec![button("Close", Action::Close).into()],
        ..page(1)
    };
    let heights = |frame: &Frame| {
        let of = |test: fn(&Kind) -> bool| {
            let found = frame.controls.iter().find(|control| test(&control.kind));
            found.map_or(0.0, |control| control.rect.h)
        };
        (
            of(|kind| matches!(kind, Kind::Nav { .. })),
            of(|kind| matches!(kind, Kind::Button { label, .. } if !label.is_empty())),
        )
    };
    let usual = heights(&frame(content(), chrome, Size::new(940.0, 660.0), &Ruler));
    assert!((usual.0 - metric::NAV_ITEM).abs() < 0.01);
    assert!((usual.1 - metric::CONTROL).abs() < 0.01);
    let large = heights(&frame(content(), chrome, Size::new(940.0, 660.0), &Large));
    let line = crate::theme::face(TextStyle::Body).line;
    assert!((large.0 - (metric::NAV_ITEM + line)).abs() < 0.01);
    assert!((large.1 - (metric::CONTROL + line)).abs() < 0.01);
}

#[test]
fn the_welcome_has_the_window_to_itself() {
    let usual = frame(page(2), Chrome::Focused, Size::new(940.0, 660.0), &Ruler);
    let wide = frame(page(2), Chrome::Focused, Size::new(2000.0, 900.0), &Ruler);
    for laid in [&usual, &wide] {
        // No navigation pane and nothing to turn to another page with.
        assert!(laid.pane.is_none() && laid.strip.is_none());
        let mut controls = laid.controls.iter();
        assert!(!controls.any(|control| matches!(control.kind, Kind::Nav { .. })));
        // And no logo: the title is all that stands over the page.
        assert!(!laid.fixed.iter().any(|item| matches!(item, Item::Logo(_))));
        // The page is a column in the middle, never wider than reads well.
        let mut cards = 0;
        for item in &laid.body {
            if let Item::Card(card) = item {
                cards += 1;
                assert!(card.w <= metric::WELCOME_MAX);
                let middle = card.x + card.w / 2.0;
                assert!((middle - laid.size.w / 2.0).abs() <= 1.0);
            }
        }
        assert_eq!(cards, 1);
    }
}
