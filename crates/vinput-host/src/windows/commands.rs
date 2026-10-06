//! What the user did in a window, carried out.
//!
//! Each [`vinput_view::Command`] ends up here. A setting is checked and
//! applied by [`crate::change`] and then put in force; a model is chosen,
//! downloaded or removed; a folder or a web page is opened. What cannot be
//! done changes nothing and is answered with a notice for the window to
//! show. Every string a window sends is treated as something a user typed:
//! checked before it is used.
#![allow(unsafe_code)]

use windows_sys::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use super::children::Which;
use super::picker::Pick;
use super::tray::About;
use super::{Host, UPDATE_CHECK_MS, UPDATE_TIMER_ID, apiserve, hotkeys};
use crate::change::{self, Applied, Follow, Ground};
use crate::view;
use crate::win::{is_link, open, open_folder};
use vinput_core::models::{self, Status};
use vinput_core::session::Controller;
use vinput_core::settings::local_dir;
use vinput_core::{Error, gpu, log, memo};
use vinput_view::{self as wire, Page};

impl Host {
    /// Does what the user did in a window.
    pub(super) fn on_window_command(
        &mut self,
        which: Which,
        command: wire::Command,
        controller: &mut dyn Controller,
    ) {
        use wire::Command as Said;
        match command {
            Said::Ready => {
                self.windows.ready(which);
                self.slow.make_stale();
                // What was done in the browser since the window was last
                // open shows from the start.
                self.pro_window_opened();
            }
            Said::Set(change) => self.set(change, controller),
            Said::CapturingHotkey(true) => self.start_capturing(controller),
            Said::CapturingHotkey(false) => self.stop_capturing(),
            Said::UseModel(id) => self.use_model(&id, controller),
            Said::InstallModel(id) => self.install_model(&id, controller),
            Said::CancelInstall(id) => {
                if let Some(model) = models::by_id(&id) {
                    self.installer.stop(model);
                }
            }
            Said::RemoveModel(id) => self.remove_model(&id),
            Said::FindModel(text) => self.finder.find(&text),
            Said::PickModelFile => self.pick(Pick::ModelFile),
            Said::AddModel(index) => self.finder.add(index),
            Said::ClearFinder => self.finder.clear(),
            Said::Google(what) => self.on_google(what, controller),
            Said::BrowseMemoDir => self.pick(Pick::MemoDir),
            Said::OpenMemoDir => self.open_memo_folder(),
            Said::OpenModelsDir => self.open_models_folder(),
            Said::CheckForUpdates => self.check_for_updates(true),
            Said::ShowUpdate => self.open_update_window(),
            Said::InstallUpdate => self.start_update(),
            Said::CancelUpdate => self.cancel_update(),
            Said::DismissNotice => self.notice = None,
            Said::OpenLink(address) => open_link(&address),
            Said::ProSignIn => self.pro_sign_in(),
            Said::ProCancel => self.pro_cancel(),
            Said::ProSubscribe(plan) => self.pro_subscribe(plan),
            Said::ProManage => self.pro_manage(),
            Said::ProRefresh => self.pro_ask(true),
            Said::ProShown => self.pro_shown(),
            Said::ProSignOut => self.pro_sign_out(),
            Said::Closed => self.close_window(which),
        }
    }

    /// Leaves something for the windows to tell the user, on `page`.
    pub(super) fn tell(&mut self, page: Page, text: impl Into<String>) {
        self.notice = Some(wire::Notice {
            kind: wire::NoticeKind::Error,
            text: text.into(),
            page,
        });
    }

    /// Makes one change to the settings and puts it in force at once. A
    /// change that cannot be made changes nothing and is answered with a
    /// notice.
    pub(super) fn set(&mut self, change: wire::Change, controller: &mut dyn Controller) {
        let before = self.settings.clone();
        let default_memo_dir = memo::folder("");
        let ground = Ground {
            graphics_card: gpu::card().is_some(),
            default_memo_dir: &default_memo_dir,
        };
        let follow = match change::apply(&mut self.settings, change, ground) {
            Err(notice) => {
                self.notice = Some(notice);
                return;
            }
            Ok(Applied::Unchanged) => {
                // Whatever was wrong with the last try no longer is.
                self.notice = None;
                return;
            }
            Ok(Applied::Changed(follow)) => follow,
        };
        if follow == Follow::Hotkeys
            && let Some(taken) = change::new_hotkeys(&before, &self.settings)
                .into_iter()
                .find(|hotkey| !hotkeys::is_free(*hotkey))
        {
            self.settings = before;
            self.tell(
                Page::General,
                format!("{taken} is used by another program. Pick another key."),
            );
            return;
        }
        self.notice = None;
        self.persist();
        controller.configure(&self.settings, false);
        self.ensure_engine();
        match follow {
            Follow::Nothing => {}
            // While a hotkey field has the keys they stay let go of; they
            // are registered as they are then when it gives them back.
            Follow::Hotkeys => {
                if !self.capturing {
                    self.hotkeys.unregister();
                    self.register_hotkeys();
                }
            }
            Follow::Autostart => {
                if let Err(err) = self.set_autostart() {
                    log::write(&err.to_string());
                    self.tell(
                        Page::General,
                        format!("Windows did not take the startup setting: {err}"),
                    );
                }
            }
            Follow::Api => self.apply_api(),
            Follow::UpdateTimer => {
                // SAFETY: `hwnd` is our live window; the timer id is ours.
                unsafe {
                    if self.settings.check_updates {
                        SetTimer(self.hwnd, UPDATE_TIMER_ID, UPDATE_CHECK_MS, None);
                    } else {
                        KillTimer(self.hwnd, UPDATE_TIMER_ID);
                    }
                }
            }
            // The models stay as they are; only the engine for the other
            // way of running them is fetched, if it is not here yet.
            Follow::Engine => {
                if models::any_local_model() {
                    self.installer.ensure_engine(self.settings.use_gpu());
                }
            }
        }
        self.refresh_tip();
    }

    /// Starts or stops the local API, as the settings say.
    fn apply_api(&mut self) {
        if !self.settings.api_enabled {
            self.api = None;
        } else if self.api.is_none() {
            self.api = apiserve::start_api(self.hwnd);
            if self.api.is_none() {
                self.tell(Page::Advanced, "The local API could not be started.");
            }
        }
    }

    /// The model a window named, or a notice that it is not there any more.
    fn model_named(&mut self, id: &str) -> Option<&'static models::Model> {
        let model = models::by_id(id);
        if model.is_none() {
            self.tell(Page::Models, "That model is no longer on the list.");
        }
        model
    }

    /// Dictates with `model` from now on.
    pub(super) fn choose_model(&mut self, model: &models::Model, controller: &mut dyn Controller) {
        let mut chosen = self.settings.clone();
        chosen.provider = model.provider;
        model.id.clone_into(&mut chosen.model);
        self.apply_choices(chosen, controller);
    }

    fn use_model(&mut self, id: &str, controller: &mut dyn Controller) {
        let Some(model) = self.model_named(id) else {
            return;
        };
        if view::can_use(&self.installer.status(model, self.settings.use_gpu())) {
            self.notice = None;
            self.choose_model(model, controller);
        } else {
            self.tell(
                Page::Models,
                format!("{} has to be downloaded before it can be used.", model.name),
            );
        }
    }

    fn install_model(&mut self, id: &str, controller: &mut dyn Controller) {
        let Some(model) = self.model_named(id) else {
            return;
        };
        let gpu = self.settings.use_gpu();
        if matches!(
            self.installer.status(model, gpu),
            Status::Missing { .. } | Status::Failed(_)
        ) {
            self.installer.install(model, gpu);
            // The first model someone downloads is the one they want.
            if !self.settings.has_model() {
                self.choose_model(model, controller);
            }
            self.slow.make_stale();
        }
    }

    fn remove_model(&mut self, id: &str) {
        let Some(model) = self.model_named(id) else {
            return;
        };
        let status = self.installer.status(model, self.settings.use_gpu());
        let in_use = view::is_in_use(&self.settings, model);
        if !view::can_remove(&status, in_use, model.is_custom()) {
            let why = if in_use {
                format!(
                    "{} is in use. Use another model before removing it.",
                    model.name
                )
            } else {
                format!("{} cannot be removed now.", model.name)
            };
            self.tell(Page::Models, why);
            return;
        }
        match self.installer.remove(model) {
            Ok(()) => self.notice = None,
            Err(err) => {
                log::write(&err.to_string());
                let why = match err {
                    Error::Install(why) => why,
                    other => other.to_string(),
                };
                self.tell(Page::Models, why);
            }
        }
        self.slow.make_stale();
    }

    /// Opens the folder the models are kept in, creating it if nothing was
    /// downloaded yet.
    fn open_models_folder(&mut self) {
        let Some(folder) = local_dir().map(|dir| dir.join("models")) else {
            return;
        };
        if !open_folder(&folder) {
            self.tell(Page::Models, format!("Could not open {}", folder.display()));
        }
    }

    /// Opens the folder voice memos are saved in, creating it if this is the
    /// first look.
    pub(super) fn open_memo_folder(&mut self) {
        let folder = memo::folder(&self.settings.memo_dir);
        if !open_folder(&folder) {
            self.tray.warn(
                About::Nothing,
                "Folder not opened",
                &format!("Could not open {}", folder.display()),
            );
        }
    }

    /// Installs a model the search has just added, if it has.
    pub(super) fn on_finder_done(&mut self, controller: &mut dyn Controller) {
        if let Some(model) = self.finder.take_added() {
            self.installer.install(model, self.settings.use_gpu());
            // The first model someone adds is the one they want.
            if !self.settings.has_model() {
                self.choose_model(model, controller);
            }
            self.slow.make_stale();
        }
        self.show_state();
    }
}

/// Opens a web page a window asked for in the default browser.
fn open_link(address: &str) {
    if !is_link(address) {
        log::write("a window asked for something that is not a web page to be opened");
    } else if !open(address) {
        log::write("could not open the browser for a link");
    }
}
