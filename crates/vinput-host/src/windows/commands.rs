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
    // Which commands there are depends on what is built beside Vinput. Built
    // alone, as it ships, the last arm has nothing left to match, and clippy
    // takes it for a wildcard that stands for a single variant; built beside
    // the others it does not, so the lint cannot be expected, only allowed.
    #[allow(
        clippy::match_wildcard_for_single_variants,
        reason = "the last arm is for the commands of the other programs"
    )]
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
            Said::UseTextModel(id) => self.use_text_model(&id, controller),
            Said::InstallModel(id) => self.install_model(&id, controller),
            Said::CancelInstall(id) => {
                if let Some(model) = models::by_id(&id).or_else(|| models::writer(&id)) {
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
            Said::TestAction(index) => self.test_action(index),
            Said::OpenLink(address) => open_link(&address),
            Said::OpenHub => vinput_core::hub::open(),
            Said::ProSignIn => self.pro_sign_in(),
            Said::ProCancel => self.pro_cancel(),
            Said::ProSubscribe(tier, plan) => self.pro_subscribe(tier, plan),
            Said::ProManage => self.pro_manage(),
            Said::ProRefresh => self.pro_ask(true),
            Said::ProShown => self.pro_shown(),
            Said::ProSignOut => self.pro_sign_out(),
            Said::WelcomeDone => {
                if self.settings.first_run {
                    self.settings.first_run = false;
                    self.persist();
                }
            }
            Said::Closed => self.close_window(which),
            // What is asked of another program of the family, when one is
            // built beside this one: no window of Vinput's sends it.
            #[allow(
                unreachable_patterns,
                reason = "built alone, Vinput has no such command"
            )]
            _ => {}
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
        let page = change::page_of(&change);
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
                page,
                format!("{taken} is used by another program. Pick another key."),
            );
            return;
        }
        self.notice = None;
        self.persist();
        controller.configure(&self.settings, false);
        self.ensure_engine();
        match follow {
            // With the switch for the graphics card the models stay as
            // they are, and the engine for the other way of running them
            // was asked for above, if the model in use has such an engine.
            Follow::Nothing | Follow::Engine => {}
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
    /// A model that writes is found as well as one that recognizes speech:
    /// both are downloaded and removed the same way.
    fn model_named(&mut self, id: &str) -> Option<&'static models::Model> {
        let model = models::by_id(id).or_else(|| models::writer(id));
        if model.is_none() {
            self.tell(Page::Models, "That model is no longer on the list.");
        }
        model
    }

    /// Tidies and edits with the model on this PC called `id` from now on,
    /// or with the one that comes with the subscription if `id` is empty.
    fn use_text_model(&mut self, id: &str, controller: &mut dyn Controller) {
        let chosen = if id.is_empty() {
            String::new()
        } else {
            let Some(model) = models::writer(id) else {
                return self.tell(Page::Cleanup, "That model is no longer on the list.");
            };
            if !view::can_use(&self.installer.status(model, self.settings.use_gpu())) {
                let why = format!("{} has to be downloaded before it can be used.", model.name);
                return self.tell(Page::Cleanup, why);
            }
            model.id.to_owned()
        };
        self.notice = None;
        if self.settings.text_model != chosen {
            self.settings.text_model = chosen;
            self.persist();
            // The model is made ready now, where the page says how far it
            // is, and not at the first press of a key.
            controller.configure(&self.settings, false);
        }
        self.slow.make_stale();
    }

    /// Dictates with `model` from now on.
    pub(super) fn choose_model(&mut self, model: &models::Model, controller: &mut dyn Controller) {
        let mut chosen = self.settings.clone();
        // The model on this PC that is left for another is the one
        // VinPUT Pro carries on with when its hours are used up.
        if chosen.provider == vinput_core::stt::ProviderKind::Local
            && model.provider != vinput_core::stt::ProviderKind::Local
        {
            chosen.last_local_model.clone_from(&chosen.model);
        }
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
            if view::writes(model) {
                // It is put to use, and made ready, when it is here.
                self.text_model_on_its_way = Some(model.id);
            } else if !self.settings.has_model() {
                // The first model someone downloads is the one they want.
                self.choose_model(model, controller);
            }
            self.slow.make_stale();
        }
    }

    /// Puts the model that writes whose download was asked for to use once
    /// it is here: whoever downloads one wants to use it. Looked at
    /// whenever a picture is drawn, which is as long as its download is
    /// watched; one that ends unwatched is chosen by hand.
    pub(super) fn use_text_model_that_arrived(&mut self, controller: &mut dyn Controller) {
        let Some(model) = self.text_model_on_its_way.and_then(models::writer) else {
            return;
        };
        match self.installer.status(model, self.settings.use_gpu()) {
            Status::Installing { .. } => {}
            Status::Installed => {
                self.text_model_on_its_way = None;
                self.use_text_model(model.id, controller);
            }
            _ => self.text_model_on_its_way = None,
        }
    }

    fn remove_model(&mut self, id: &str) {
        let Some(model) = self.model_named(id) else {
            return;
        };
        let status = self.installer.status(model, self.settings.use_gpu());
        let in_use = view::is_in_use(&self.settings, model);
        let page = if view::writes(model) {
            Page::Cleanup
        } else {
            Page::Models
        };
        if !view::can_remove(&status, in_use, model.is_custom()) {
            let why = if in_use {
                format!(
                    "{} is in use. Use another model before removing it.",
                    model.name
                )
            } else {
                format!("{} cannot be removed now.", model.name)
            };
            self.tell(page, why);
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
                self.tell(page, why);
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
