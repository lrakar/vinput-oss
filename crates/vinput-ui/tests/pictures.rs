//! Takes pictures of the test card and of real pages in every theme and
//! checks that something was drawn: the whole path from nodes to a PNG file,
//! without a window.
// The canned scenes and the pictures are part of a build for development.
#![cfg(all(windows, debug_assertions))]

use std::path::PathBuf;

use vinput_ui::mock;
use vinput_ui::picture::{self, Shot};
use vinput_ui::scene::{Asking, Showing};
use vinput_ui::theme::Theme;
use vinput_view::{Dialog, Page, View};

/// The first bytes of every PNG file.
const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

fn file(name: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("vinput-ui-pictures-{}", std::process::id()));
    assert!(std::fs::create_dir_all(&folder).is_ok());
    folder.join(name)
}

/// Takes the picture and returns the bytes of its file.
fn take(name: &str, showing: &Showing, view: &View, shot: &Shot<'_>) -> Vec<u8> {
    let path = file(name);
    let saved = picture::save(showing, view, shot, &path);
    assert!(saved.is_ok(), "{name}: {saved:?}");
    let bytes = std::fs::read(&path).unwrap_or_default();
    let _ = std::fs::remove_file(&path);
    assert!(bytes.starts_with(&PNG), "{name} is not a PNG file");
    bytes
}

#[test]
fn the_gallery_draws_in_every_theme() {
    let view = mock::scene("settings").unwrap_or_default();
    let mut pictures = Vec::new();
    for (name, theme) in [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("contrast", Theme::contrast_sample()),
    ] {
        let mut shot = Shot::new(theme);
        shot.size = Some((940.0, 2600.0));
        shot.look.focus = Some(6);
        shot.look.ring = true;
        shot.look.hover = Some(7);
        let bytes = take(
            &format!("gallery-{name}.png"),
            &Showing::Gallery,
            &view,
            &shot,
        );
        // A page full of text and controls does not compress to nothing.
        assert!(bytes.len() > 60_000, "{name}: {} bytes", bytes.len());
        pictures.push(bytes);
    }
    assert_ne!(pictures[0], pictures[1]);
    assert_ne!(pictures[0], pictures[2]);
}

#[test]
fn every_page_draws_at_other_scales_and_sizes() {
    let view = mock::scene("downloading").unwrap_or_default();
    for page in [
        Page::General,
        Page::Models,
        Page::AddModel,
        Page::Google,
        Page::Pro,
        Page::Advanced,
        Page::About,
    ] {
        let mut shot = Shot::new(Theme::light());
        shot.scale = 1.5;
        shot.size = Some((760.0, 480.0));
        shot.look.scroll = 80.0;
        let bytes = take(
            &format!("{page:?}.png"),
            &Showing::Settings(page),
            &view,
            &shot,
        );
        assert!(bytes.len() > 15_000, "{page:?}: {} bytes", bytes.len());
    }
}

#[test]
fn the_pro_page_draws_in_every_state_of_the_account() {
    let mut pictures = Vec::new();
    for name in mock::SCENES.iter().filter(|name| name.starts_with("pro-")) {
        let view = mock::scene(name).unwrap_or_default();
        let shot = Shot::new(Theme::dark());
        let showing = Showing::Settings(Page::Pro);
        let bytes = take(&format!("{name}.png"), &showing, &view, &shot);
        assert!(bytes.len() > 15_000, "{name}: {} bytes", bytes.len());
        pictures.push(bytes);
    }
    // One scene for every state, and no two of them look the same.
    assert_eq!(pictures.len(), 24);
    for (index, picture) in pictures.iter().enumerate() {
        assert!(!pictures[..index].contains(picture), "picture {index}");
    }
}

#[test]
fn the_models_page_draws_the_card_of_pro_in_every_state_and_theme() {
    // The states in which the card over the lists says something else.
    let scenes = [
        "settings",
        "pro-subscribed",
        "pro-subscribed-elsewhere",
        "pro-plus",
        "pro-over-allowance",
        "pro-used-up-no-model",
        "pro-low-no-model",
        "pro-payment-problem",
        "pro-expired",
        "pro-offline",
    ];
    let mut pictures = Vec::new();
    for name in scenes {
        let view = mock::scene(name).unwrap_or_default();
        for (theme_name, theme) in [
            ("dark", Theme::dark()),
            ("light", Theme::light()),
            ("contrast", Theme::contrast_sample()),
        ] {
            let shot = Shot::new(theme);
            let showing = Showing::Settings(Page::Models);
            let file = format!("models-{name}-{theme_name}.png");
            let bytes = take(&file, &showing, &view, &shot);
            assert!(bytes.len() > 15_000, "{file}: {} bytes", bytes.len());
            pictures.push(bytes);
        }
    }
    for (index, picture) in pictures.iter().enumerate() {
        assert!(!pictures[..index].contains(picture), "picture {index}");
    }
    // The card stays where it is while the lists move under it, in a small
    // window with large text as in any other.
    let view = mock::scene("many-models").unwrap_or_default();
    let mut theme = Theme::light();
    theme.text_scale = 1.5;
    let mut shot = Shot::new(theme);
    shot.size = Some((720.0, 440.0));
    let showing = Showing::Settings(Page::Models);
    let top = take("models-top.png", &showing, &view, &shot);
    shot.look.scroll = 400.0;
    let scrolled = take("models-scrolled.png", &showing, &view, &shot);
    assert!(top.len() > 15_000 && scrolled.len() > 15_000);
    assert_ne!(top, scrolled);
}

#[test]
fn the_google_setup_draws_at_every_step_and_for_every_answer() {
    let mut pictures = Vec::new();
    for name in mock::SCENES
        .iter()
        .filter(|name| name.starts_with("google-"))
    {
        let view = mock::scene(name).unwrap_or_default();
        let shot = Shot::new(Theme::dark());
        let showing = Showing::Settings(Page::Google);
        let bytes = take(&format!("{name}.png"), &showing, &view, &shot);
        assert!(bytes.len() > 15_000, "{name}: {} bytes", bytes.len());
        pictures.push(bytes);
    }
    // One scene for every step and answer, and no two of them look the same.
    assert_eq!(pictures.len(), 28);
    for (index, picture) in pictures.iter().enumerate() {
        assert!(!pictures[..index].contains(picture), "picture {index}");
    }
}

#[test]
fn the_small_windows_draw_at_the_size_of_what_they_say() {
    let update = mock::scene("update").unwrap_or_default();
    let shot = Shot::new(Theme::dark());
    assert!(take("update.png", &Showing::Update, &update, &shot).len() > 5_000);
    let dialog = Dialog {
        title: "VinPUT setup".to_owned(),
        heading: "Install VinPUT".to_owned(),
        detail: "Type with your voice into any text field.".to_owned(),
        lines: vec!["For your Windows account only".to_owned()],
        primary: "Install".to_owned(),
        secondary: "Cancel".to_owned(),
    };
    let asking = Showing::Asking(Asking::new(dialog));
    let small = take("dialog.png", &asking, &View::default(), &shot);
    assert!(small.len() > 3_000);
}
