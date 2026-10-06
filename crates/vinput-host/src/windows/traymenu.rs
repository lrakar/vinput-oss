//! The tray menu: what it offers and what picking each item does.
//!
//! The menu is built afresh each time it is shown, from the settings and
//! from what is on this PC at that moment, so it never goes stale. Each item
//! has a number: the fixed ones below, then a range for the models and a
//! range for the microphones, which the pick is decoded from afterwards.
#![allow(unsafe_code)]

use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

use super::Host;
use super::tray::About;
use crate::autostart;
use crate::menu::{self, Mark, Menu};
use vinput_core::models::Status;
use vinput_core::session::Controller;
use vinput_core::settings::Settings;
use vinput_core::{audio, gpu, log, models};
use vinput_view::Page;

const MENU_SETTINGS: usize = 1;
const MENU_AUTOSTART: usize = 2;
const MENU_QUIT: usize = 3;
const MENU_MEMOS: usize = 4;
const MENU_UPDATE: usize = 5;
const MENU_CHECK_UPDATE: usize = 6;
const MENU_GRAPHICS_CARD: usize = 7;
/// The status line at the top of the tray menu, which cannot be picked.
const MENU_STATUS: usize = 8;
/// The item under the models that opens the Models page.
const MENU_DOWNLOAD_MODELS: usize = 9;
/// Tray menu ids of the models, by their place in the catalog.
const MENU_MODELS: usize = 100;
/// Tray menu ids of the microphones: the default one, then the connected ones.
const MENU_MICROPHONES: usize = 200;

/// The models the tray menu can offer, in the order of the list in Settings:
/// the catalog, then the ones people added, as many as there are ids for.
fn tray_models() -> Vec<&'static models::Model> {
    let mut listed = models::all();
    listed.truncate(MENU_MICROPHONES - MENU_MODELS);
    listed
}

/// The line at the top of the tray menu: what Vinput is doing, or what it
/// needs before it can do anything.
fn status_line(running: bool, settings: &Settings) -> String {
    if running {
        "Listening…".to_owned()
    } else if settings.has_model() {
        format!("Ready · {}", settings.hotkey)
    } else {
        "Choose a speech model in Settings".to_owned()
    }
}

impl Host {
    /// Shows the tray menu at the cursor. Returns the item picked, or 0,
    /// and the microphones it listed, in their order.
    pub(super) fn show_menu(&self) -> (usize, Vec<audio::Microphone>) {
        let microphones = audio::microphones().unwrap_or_else(|err| {
            log::write(&err.to_string());
            Vec::new()
        });
        let Some(menu) = self.tray_menu(&microphones) else {
            return (0, microphones);
        };
        let mut cursor = POINT::default();
        // SAFETY: `cursor` is writable.
        unsafe { GetCursorPos(&raw mut cursor) };
        (menu.show(self.hwnd, cursor.x, cursor.y), microphones)
    }

    /// The tray menu: what Vinput is doing, the choices people change most
    /// often, and the way out.
    fn tray_menu(&self, microphones: &[audio::Microphone]) -> Option<Menu> {
        let settings = &self.settings;
        let mut menu = Menu::new()?;
        let status = status_line(self.trigger.is_running(), settings);
        menu.item(MENU_STATUS, &status, Mark::Disabled).separator();
        menu.item(MENU_SETTINGS, "Settings…", Mark::Default)
            .separator();

        let mut models = Menu::new()?;
        let gpu = settings.use_gpu();
        let in_use = models::find(settings.provider, &settings.model);
        let mut listed = false;
        for (index, model) in tray_models().into_iter().enumerate() {
            let chosen = in_use.is_some_and(|used| used.id == model.id);
            if chosen || self.installer.status(model, gpu) == Status::Installed {
                let mark = if chosen { Mark::Radio } else { Mark::None };
                models.item(MENU_MODELS + index, model.name, mark);
                listed = true;
            }
        }
        if !listed {
            models.item(MENU_STATUS, "No model downloaded yet", Mark::Disabled);
        }
        if gpu::card().is_some() {
            let mark = if gpu { Mark::Check } else { Mark::None };
            models.item(MENU_GRAPHICS_CARD, "Use graphics card", mark);
        }
        models
            .separator()
            .item(MENU_DOWNLOAD_MODELS, "Download models…", Mark::None);
        menu.submenu("Model", models);

        let inputs = menu::microphones(microphones, &settings.microphone, MENU_MICROPHONES)?;
        menu.submenu("Microphone", inputs).separator();
        menu.item(MENU_MEMOS, "Voice memos", Mark::None).separator();
        let autostart = if autostart::is_enabled() {
            Mark::Check
        } else {
            Mark::None
        };
        menu.item(MENU_AUTOSTART, "Start with Windows", autostart);
        match &self.updates.release {
            Some(release) => menu.item(
                MENU_UPDATE,
                &format!("Update to version {}…", release.version),
                Mark::None,
            ),
            None => menu.item(MENU_CHECK_UPDATE, "Check for updates…", Mark::None),
        };
        menu.separator().item(MENU_QUIT, "Quit", Mark::None);
        Some(menu)
    }

    /// Does what the item picked from the tray menu says. Returns whether
    /// that is to quit.
    pub(super) fn on_menu(
        &mut self,
        (item, microphones): (usize, Vec<audio::Microphone>),
        controller: &mut dyn Controller,
    ) -> bool {
        match item {
            MENU_QUIT => return true,
            MENU_SETTINGS => self.open_settings(Page::General),
            MENU_AUTOSTART => self.toggle_autostart(),
            MENU_MEMOS => self.open_memo_folder(),
            MENU_UPDATE => self.open_update_window(),
            MENU_CHECK_UPDATE => self.check_for_updates(true),
            MENU_GRAPHICS_CARD => {
                let mut chosen = self.settings.clone();
                chosen.local_gpu = !self.settings.use_gpu();
                self.apply_choices(chosen, controller);
            }
            MENU_DOWNLOAD_MODELS => self.open_settings(Page::Models),
            item if (MENU_MODELS..MENU_MICROPHONES).contains(&item) => {
                // The list is the one the menu was just built from.
                let Some(model) = tray_models().get(item - MENU_MODELS).copied() else {
                    return false;
                };
                self.choose_model(model, controller);
            }
            item => {
                if let Some(id) = menu::picked_microphone(item, &microphones, MENU_MICROPHONES) {
                    let mut chosen = self.settings.clone();
                    chosen.microphone = id;
                    self.apply_choices(chosen, controller);
                }
            }
        }
        false
    }

    fn toggle_autostart(&mut self) {
        self.settings.autostart = !autostart::is_enabled();
        if let Err(err) = autostart::set(self.settings.autostart) {
            self.tray.warn(
                About::Nothing,
                "Startup setting not applied",
                &err.to_string(),
            );
        }
        self.persist();
        self.show_state();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_line_says_what_vinput_is_doing_or_needs() {
        let mut settings = Settings::default();
        assert_eq!(
            status_line(false, &settings),
            "Choose a speech model in Settings"
        );
        "whisper-base".clone_into(&mut settings.model);
        assert_eq!(status_line(false, &settings), "Ready \u{b7} F1");
        assert_eq!(status_line(true, &settings), "Listening\u{2026}");
    }

    #[test]
    fn tray_menu_ids_do_not_overlap() {
        let fixed = [
            MENU_SETTINGS,
            MENU_AUTOSTART,
            MENU_QUIT,
            MENU_MEMOS,
            MENU_UPDATE,
            MENU_CHECK_UPDATE,
            MENU_GRAPHICS_CARD,
            MENU_STATUS,
            MENU_DOWNLOAD_MODELS,
        ];
        assert!(fixed.iter().all(|id| *id > 0 && *id < MENU_MODELS));
        // However many models people add, none takes the id of a microphone.
        assert!(MENU_MODELS + tray_models().len() <= MENU_MICROPHONES);
        assert!(tray_models().len() >= models::CATALOG.len().min(MENU_MICROPHONES - MENU_MODELS));
    }
}
