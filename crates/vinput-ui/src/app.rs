//! What `vinput ui` does: works out which window is meant and what it shows,
//! then opens it or takes its picture.

use crate::error::Result;
use crate::theme::Theme;
use crate::{Appearance, Options, system};

#[cfg(any(debug_assertions, test))]
use canned::resolve;

/// What the options ask to be shown, and the view it starts from, in the
/// build that ships: the real windows and nothing else.
#[cfg(not(any(debug_assertions, test)))]
fn resolve(options: &Options) -> Result<(crate::scene::Showing, vinput_view::View)> {
    use crate::scene::Showing;
    match &options.source {
        crate::Source::Host | crate::Source::Program(_) => {
            Ok((Showing::of(options.surface), vinput_view::View::default()))
        }
        crate::Source::Mock(name) => Err(crate::Error::UnknownScene(name.clone())),
    }
}

/// The theme the options ask for.
pub(crate) fn theme(appearance: Appearance) -> Theme {
    match appearance {
        Appearance::System => system::theme(),
        Appearance::Light => system::theme_forced(false),
        Appearance::Dark => system::theme_forced(true),
        Appearance::Contrast => Theme::contrast_sample(),
    }
}

/// See [`crate::run`].
pub(crate) fn run(options: &Options) -> Result<()> {
    let (showing, view) = resolve(options)?;
    #[cfg(any(debug_assertions, test))]
    if let Some(path) = &options.picture {
        return canned::take_picture(&showing, &view, options, path);
    }
    crate::window::run(showing, view, options)
}

/// What only a build for development and the tests have: the canned scenes
/// and the pictures taken of them.
#[cfg(any(debug_assertions, test))]
mod canned {
    use std::path::Path;

    use vinput_view::{Dialog, View};

    use super::theme;
    use crate::error::{Error, Result};
    use crate::gallery;
    use crate::look::Look;
    use crate::picture::{self, Shot};
    use crate::scene::{Asking, Phase, Showing};
    use crate::{Options, Source, mock};

    fn lines(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|line| (*line).to_owned()).collect()
    }

    /// The window setup shows, as a canned scene.
    pub(crate) fn setup_dialog() -> Dialog {
        Dialog {
            title: "VinPUT setup".to_owned(),
            heading: "Install VinPUT 0.3.0".to_owned(),
            detail: "Type with your voice into any text field.".to_owned(),
            lines: lines(&[
                r"Installs to C:\Users\you\AppData\Local\Programs\Vinput",
                "For your Windows account only; no administrator rights",
                "Adds VinPUT to the Start menu and starts it with Windows",
                "Keeps your settings, keys and downloaded models",
            ]),
            primary: "Install".to_owned(),
            secondary: "Cancel".to_owned(),
        }
    }

    /// The question removal asks, as a canned scene.
    pub(crate) fn question_dialog() -> Dialog {
        Dialog {
            title: "VinPUT".to_owned(),
            heading: "Remove VinPUT from this PC?".to_owned(),
            detail: "Your settings, stored keys and downloaded speech models can stay for next \
                 time, or go as well."
                .to_owned(),
            lines: Vec::new(),
            primary: "Remove everything".to_owned(),
            secondary: "Keep my data".to_owned(),
        }
    }

    /// The canned window that asks or tells which is called `name`.
    fn asking(name: &str) -> Option<Asking> {
        let mut asking = Asking::new(match name {
            "dialog-setup" | "dialog-working" | "dialog-failed" => setup_dialog(),
            "dialog-question" => question_dialog(),
            "dialog-message" => Dialog {
                title: "VinPUT".to_owned(),
                heading: "VinPUT has been removed.".to_owned(),
                ..Dialog::default()
            },
            _ => return None,
        });
        match name {
            "dialog-working" => asking.phase = Phase::Working("Installing".to_owned()),
            "dialog-failed" => {
                asking.phase = Phase::Failed(
                    "VinPUT is still running; exit it from its tray icon and try again".to_owned(),
                );
            }
            _ => {}
        }
        Some(asking)
    }

    /// What the options ask to be shown, and the view it starts from.
    pub(super) fn resolve(options: &Options) -> Result<(Showing, View)> {
        let surface = Showing::of(options.surface);
        match &options.source {
            Source::Host | Source::Program(_) => Ok((surface, View::default())),
            Source::Mock(name) if name == gallery::SCENE => Ok((
                Showing::Gallery,
                mock::scene("settings").unwrap_or_default(),
            )),
            Source::Mock(name) => {
                if let Some(asking) = asking(name) {
                    return Ok((Showing::Asking(asking), View::default()));
                }
                let view = mock::scene(name).ok_or_else(|| Error::UnknownScene(name.clone()))?;
                Ok((surface, view))
            }
        }
    }

    /// Saves a picture of what is shown instead of showing it.
    pub(super) fn take_picture(
        showing: &Showing,
        view: &View,
        options: &Options,
        path: &Path,
    ) -> Result<()> {
        let mut shot = Shot::new(theme(options.appearance));
        if let Some(scale) = options.text_scale {
            shot.theme.text_scale = scale;
        }
        shot.scale = options.scale;
        shot.size = options
            .size
            .map(|(width, height)| (f32::from(width), f32::from(height)));
        shot.look = Look {
            focus: options.focus,
            ring: options.focus.is_some(),
            hover: options.hover,
            scroll: f32::from(options.scroll),
            ..shot.look
        };
        shot.memory = Some(&options.memory);
        shot.lists = f32::from(options.list_scroll);
        picture::save(showing, view, &shot, path)
    }
}

#[cfg(test)]
mod tests {
    use vinput_view::{Page, Surface};

    use super::*;
    use crate::scene::Showing;
    use crate::{Error, Source, gallery, mock};

    #[test]
    fn every_scene_resolves_to_something_to_show() {
        let surface = Surface::Settings(Page::General);
        for name in mock::SCENES.iter().chain(&crate::scene::DIALOG_SCENES) {
            let options = Options::new(surface, Source::Mock((*name).to_owned()));
            assert!(resolve(&options).is_ok(), "{name}");
        }
        let gallery = Options::new(surface, Source::Mock(gallery::SCENE.to_owned()));
        assert!(matches!(resolve(&gallery), Ok((Showing::Gallery, _))));
        let unknown = Options::new(surface, Source::Mock("nothing".to_owned()));
        assert!(matches!(resolve(&unknown), Err(Error::UnknownScene(_))));
        assert!(matches!(
            resolve(&Options::new(Surface::Update, Source::Host)),
            Ok((Showing::Update, _))
        ));
    }
}
