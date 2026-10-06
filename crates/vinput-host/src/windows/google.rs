//! The setup of a Google service of one's own: the steps of its page, what
//! was entered there, and asking Google whether it works.
//!
//! What is entered is a draft until Google has said that it works and the
//! user has finished the setup: only then is a key kept in the credential
//! store, a project written to the settings, and the model of the service
//! put to use. Asking waits for the network, and reading a key file may
//! wait for a disk, so each runs on a thread of its own, which leaves its
//! answer in a queue and posts `GOOGLE_CHECKED`. An answer to a question
//! that was replaced by a newer one, or that nobody waits for because the
//! page was left or the window closed, is dropped: no key is kept from it.
//! Every answer has its own place in the queue, so a late answer to an old
//! question never takes the place of the answer to the newest one.
#![allow(unsafe_code)]

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

use super::picker::Pick;
use super::{GOOGLE_CHECKED, Host};
use crate::view::GoogleFacts;
use vinput_core::session::Controller;
use vinput_core::settings::{DEFAULT_LOCATION, Settings};
use vinput_core::stt::ProviderKind;
use vinput_core::stt::check::{self, ServiceAccount, Verdict};
use vinput_core::{log, models, secrets};
use vinput_view::{GoogleCheck, GoogleProblem, GoogleService, GoogleStep, Setup};

/// The longest key a window may send to be asked about. Keys are some forty
/// characters.
const MAX_KEY: usize = 512;

/// What a thread of the setup found out.
enum Answer {
    /// What Google made of what is entered.
    Verdict(Verdict),
    /// The key file that was picked, read, or why it is not one.
    KeyFile(Result<ServiceAccount, String>),
}

/// Where the threads of the setup leave their answers, each with the
/// number of the question it answers.
type Answers = Arc<Mutex<VecDeque<(u64, Answer)>>>;

/// Where the setup stands.
#[derive(Default)]
pub(super) struct Google {
    step: GoogleStep,
    service: GoogleService,
    check: GoogleCheck,
    /// Counts the questions, so that the answer to one that was replaced or
    /// called off is dropped.
    round: u64,
    /// Where the thread of a question leaves its answer.
    left: Answers,
    /// The Gemini API key that was entered: Google is asked about it, and
    /// finishing the setup keeps it if it works.
    key: Option<String>,
    /// Whether the key that is kept is being replaced.
    replacing: bool,
    /// The Google Cloud project and location, as entered.
    project: String,
    location: String,
    /// The service account key picked in this setup and not kept yet.
    picked: Option<ServiceAccount>,
    /// Whether the sign-in of this PC was chosen over the key that is kept.
    sign_in: bool,
    /// Name of the model the setup has put to use.
    model: String,
}

impl Google {
    /// What a picture for the windows is drawn from. `stored` is the
    /// address of the service account whose key is kept, `sign_in` whether
    /// this PC has a Google sign-in, and `settings` what is in force.
    pub(super) fn facts<'a>(
        &'a self,
        stored: Option<&'a str>,
        sign_in: bool,
        settings: &Settings,
    ) -> GoogleFacts<'a> {
        let account = match &self.picked {
            Some(picked) => Some(picked.email.as_str()),
            None if self.sign_in => None,
            None => stored,
        };
        GoogleFacts {
            step: self.step,
            service: self.service,
            check: &self.check,
            project: &self.project,
            location: &self.location,
            key_length: self.key.as_ref().map_or(0, |key| {
                u8::try_from(key.chars().count()).unwrap_or(u8::MAX)
            }),
            replacing: self.replacing,
            account,
            sign_in,
            // Dictating takes a project, and a key or a sign-in.
            ready: settings.project_id.is_some() && (stored.is_some() || sign_in),
            model: &self.model,
        }
    }

    /// Calls off the question that is under way, if one is, and forgets
    /// the key that was entered.
    fn call_off(&mut self) {
        self.key = None;
        self.ask_anew();
    }

    /// Calls off the question that is under way, if one is: its answer no
    /// longer counts.
    fn ask_anew(&mut self) {
        self.round += 1;
        self.check = GoogleCheck::Idle;
    }

    /// The next answer that is to the question asked last, if it has
    /// arrived. Answers to earlier questions are dropped on the way.
    fn next_answer(&self) -> Option<Answer> {
        let mut left = self.left.lock().unwrap_or_else(PoisonError::into_inner);
        while let Some((round, answer)) = left.pop_front() {
            if round == self.round {
                return Some(answer);
            }
        }
        None
    }
}

/// Runs `ask` on a thread of its own, which leaves its answer under
/// `round` and wakes the loop. Says whether the thread was started.
fn ask_on_a_thread(
    left: Answers,
    round: u64,
    window: usize,
    ask: Box<dyn FnOnce() -> Answer + Send>,
) -> bool {
    let started = thread::Builder::new()
        .name("vinput-google".into())
        .spawn(move || {
            let answer = ask();
            left.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push_back((round, answer));
            // SAFETY: posting to a window that has gone away fails harmlessly.
            unsafe { PostMessageW(window as HWND, GOOGLE_CHECKED, 0, 0) };
        });
    if let Err(err) = &started {
        log::write(&format!("starting a thread to ask Google failed: {err}"));
    }
    started.is_ok()
}

/// The provider a service is dictated with.
fn provider(service: GoogleService) -> ProviderKind {
    match service {
        GoogleService::Gemini => ProviderKind::Gemini,
        GoogleService::Speech => ProviderKind::GoogleCloud,
    }
}

/// The settings that dictate with `service`: the model of it that is
/// chosen already, or else the one the catalog has for it.
fn dictating_with(settings: &Settings, service: GoogleService) -> Settings {
    let mut chosen = settings.clone();
    let provider = provider(service);
    if chosen.provider != provider || !chosen.has_model() {
        let listed = models::CATALOG
            .iter()
            .find(|model| model.provider == provider);
        if let Some(model) = listed {
            chosen.provider = provider;
            model.id.clone_into(&mut chosen.model);
        }
    }
    chosen
}

/// What Google made of it, for the page.
fn outcome(verdict: Verdict) -> GoogleCheck {
    let problem = match verdict {
        Verdict::Works => return GoogleCheck::Works,
        Verdict::WrongKey => GoogleProblem::WrongKey,
        Verdict::NoAccess => GoogleProblem::NoAccess,
        Verdict::Billing => GoogleProblem::Billing,
        Verdict::Quota => GoogleProblem::Quota,
        Verdict::NotFound => GoogleProblem::NotFound,
        Verdict::NoSignIn => GoogleProblem::NoSignIn,
        Verdict::Offline => GoogleProblem::Offline,
        Verdict::Other(said) => return failed(GoogleProblem::Other, said),
    };
    failed(problem, String::new())
}

fn failed(problem: GoogleProblem, said: String) -> GoogleCheck {
    GoogleCheck::Failed { problem, said }
}

impl Host {
    /// Does what the user did in the setup.
    pub(super) fn on_google(&mut self, what: Setup, controller: &mut dyn Controller) {
        match what {
            Setup::Open(service) => self.google_open(service),
            Setup::Step(step) => self.google_step(step),
            Setup::Key(key) => self.google_key(&key),
            Setup::Replace => {
                self.google.call_off();
                self.google.replacing = true;
            }
            Setup::Project(project) => {
                project.trim().clone_into(&mut self.google.project);
                self.google_check();
            }
            Setup::Location(location) => {
                location.trim().clone_into(&mut self.google.location);
                self.google_check();
            }
            Setup::PickKey => self.pick(Pick::GoogleKey),
            Setup::UseSignIn => {
                self.google.picked = None;
                self.google.sign_in = true;
                self.google_check();
            }
            Setup::Check => self.google_check(),
            Setup::Finish => self.google_finish(controller),
            Setup::RemoveKey => {
                secrets::delete(secrets::GEMINI_API_KEY);
                self.google.call_off();
                self.google.replacing = false;
                self.slow.make_stale();
                // Only a dictation with Gemini has lost what it needs.
                if self.settings.provider == ProviderKind::Gemini {
                    controller.configure(&self.settings, true);
                }
            }
            Setup::Close => self.google_close(),
        }
    }

    /// Leaves the setup: nothing of it is kept that was not put to use.
    pub(super) fn google_close(&mut self) {
        self.google = Google {
            round: self.google.round + 1,
            left: Arc::clone(&self.google.left),
            ..Google::default()
        };
    }

    /// Starts the setup of `service` from what is in force. Someone who
    /// has it set up sees that, and whether it works; anyone else is shown
    /// how to get what it needs.
    fn google_open(&mut self, service: GoogleService) {
        self.google_close();
        self.notice = None;
        let settings = &self.settings;
        let set_up = match service {
            GoogleService::Gemini => secrets::get(secrets::GEMINI_API_KEY).is_some(),
            GoogleService::Speech => settings.project_id.is_some(),
        };
        let location = Some(settings.location.as_str()).filter(|location| !location.is_empty());
        self.google.service = service;
        self.google.project = settings.project_id.clone().unwrap_or_default();
        location
            .unwrap_or(DEFAULT_LOCATION)
            .clone_into(&mut self.google.location);
        if set_up {
            self.google_step(GoogleStep::Enter);
        } else {
            self.google.step = GoogleStep::Get;
        }
    }

    /// Turns to a step. The last one is only reached by finishing.
    fn google_step(&mut self, step: GoogleStep) {
        if step == GoogleStep::Done {
            return;
        }
        self.google.call_off();
        self.google.step = step;
        // What is set up already is asked about without being asked to.
        if step == GoogleStep::Enter {
            self.slow.make_stale();
            self.google_check();
        }
    }

    /// A Gemini API key was entered: Google is asked about it.
    fn google_key(&mut self, key: &str) {
        let key = key.trim();
        if key.is_empty() {
            return;
        }
        self.google.service = GoogleService::Gemini;
        if key.len() > MAX_KEY {
            self.google.call_off();
            self.google.check = failed(GoogleProblem::WrongKey, String::new());
            return;
        }
        let asked = key.to_owned();
        self.google.key = Some(key.to_owned());
        self.google_ask(Box::new(move || check::gemini_key(&asked)));
    }

    /// Asks Google about what is entered, or about what is kept while
    /// nothing was entered. Without anything to ask about, nothing is.
    fn google_check(&mut self) {
        match self.google.service {
            GoogleService::Gemini => {
                // A key that was entered is asked about again, the one
                // that is kept otherwise.
                let kept = || secrets::get(secrets::GEMINI_API_KEY);
                let Some(key) = self.google.key.clone().or_else(kept) else {
                    self.google.ask_anew();
                    return;
                };
                self.google_ask(Box::new(move || check::gemini_key(&key)));
            }
            GoogleService::Speech => {
                let google = &self.google;
                if google.project.is_empty() {
                    self.google.call_off();
                    return;
                }
                let (project, location) = (google.project.clone(), google.location.clone());
                let key = match &google.picked {
                    Some(picked) => Some(picked.key.clone()),
                    None if google.sign_in => None,
                    None => secrets::get(secrets::GOOGLE_CLOUD_KEY),
                };
                self.google_ask(Box::new(move || {
                    check::google_cloud(&project, &location, key.as_deref())
                }));
            }
        }
    }

    /// Runs a question on a thread of its own. Its answer counts unless a
    /// newer one was asked, or the setup left, in the meantime.
    fn google_ask(&mut self, ask: Box<dyn FnOnce() -> Verdict + Send>) {
        self.google_wait_for(Box::new(move || Answer::Verdict(ask())));
    }

    /// Has `ask` answered on a thread of its own, and shows that an answer
    /// is waited for.
    fn google_wait_for(&mut self, ask: Box<dyn FnOnce() -> Answer + Send>) {
        self.google.ask_anew();
        self.google.check = GoogleCheck::Checking;
        let (round, left) = (self.google.round, Arc::clone(&self.google.left));
        // A window handle is an address that any thread may post to.
        if !ask_on_a_thread(left, round, self.hwnd as usize, ask) {
            self.google.check = failed(GoogleProblem::Other, String::new());
        }
    }

    /// Takes in what a thread of the setup found out. Nothing is kept or
    /// put to use here: what Google made of what is entered is shown, and
    /// finishing the setup is the user's to do.
    pub(super) fn on_google_checked(&mut self) {
        while let Some(answer) = self.google.next_answer() {
            match answer {
                Answer::Verdict(verdict) => self.google.check = outcome(verdict),
                Answer::KeyFile(read) => self.google_key_read(read),
            }
        }
        self.show_state();
    }

    /// Keeps what was entered and found to work, and dictates with it.
    fn google_finish(&mut self, controller: &mut dyn Controller) {
        if self.google.check != GoogleCheck::Works {
            return;
        }
        let mut settings = self.settings.clone();
        // A Gemini API key that was entered and works is kept now.
        if let Some(key) = self.google.key.take()
            && self.google.service == GoogleService::Gemini
            && let Err(err) = secrets::set(secrets::GEMINI_API_KEY, &key)
        {
            log::write(&err.to_string());
            self.google.key = Some(key);
            self.google.check = failed(GoogleProblem::Other, err.to_string());
            return;
        }
        if self.google.service == GoogleService::Speech {
            let kept = match (&self.google.picked, self.google.sign_in) {
                (Some(picked), _) => secrets::set(secrets::GOOGLE_CLOUD_KEY, &picked.key),
                (None, true) => {
                    secrets::delete(secrets::GOOGLE_CLOUD_KEY);
                    Ok(())
                }
                (None, false) => Ok(()),
            };
            if let Err(err) = kept {
                log::write(&err.to_string());
                self.google.check = failed(GoogleProblem::Other, err.to_string());
                return;
            }
            settings.project_id = Some(self.google.project.clone());
            settings.location.clone_from(&self.google.location);
        }
        self.google_use(&settings, controller);
    }

    /// Puts `settings`, and the model of the service that was set up, in
    /// force, and turns to the last step.
    fn google_use(&mut self, settings: &Settings, controller: &mut dyn Controller) {
        self.settings = dictating_with(settings, self.google.service);
        self.persist();
        // The key or the credentials may be other ones though the settings
        // read as before.
        controller.configure(&self.settings, true);
        self.ensure_engine();
        self.refresh_tip();
        self.slow.make_stale();
        let settings = &self.settings;
        self.google.model = models::name_in_use(settings.provider, &settings.model)
            .unwrap_or_default()
            .to_owned();
        self.google.picked = None;
        self.google.sign_in = false;
        self.google.replacing = false;
        self.google.step = GoogleStep::Done;
    }

    /// The file picker has closed on a service account key file. The file
    /// is read on a thread of its own: it may be on a share that takes its
    /// time, and the loop, with the hotkeys on it, does not wait for one.
    pub(super) fn google_key_picked(&mut self, file: &Path) {
        if self.google.step != GoogleStep::Enter {
            return;
        }
        let file = file.to_owned();
        self.google_wait_for(Box::new(move || {
            let read = check::service_account(&file);
            Answer::KeyFile(read.map_err(|err| err.to_string()))
        }));
    }

    /// The key file that was picked has been read.
    fn google_key_read(&mut self, read: Result<ServiceAccount, String>) {
        match read {
            Ok(account) => {
                // A key file names its project; nobody has to type it.
                if self.google.project.is_empty() {
                    self.google.project = account.project_id.clone().unwrap_or_default();
                }
                self.google.picked = Some(account);
                self.google.sign_in = false;
                self.google_check();
            }
            Err(why) => {
                self.google.ask_anew();
                self.google.check = failed(GoogleProblem::BadFile, why);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_verdict_becomes_what_the_page_says_is_wrong() {
        assert_eq!(outcome(Verdict::Works), GoogleCheck::Works);
        for (verdict, problem) in [
            (Verdict::WrongKey, GoogleProblem::WrongKey),
            (Verdict::NoAccess, GoogleProblem::NoAccess),
            (Verdict::Billing, GoogleProblem::Billing),
            (Verdict::Quota, GoogleProblem::Quota),
            (Verdict::NotFound, GoogleProblem::NotFound),
            (Verdict::NoSignIn, GoogleProblem::NoSignIn),
            (Verdict::Offline, GoogleProblem::Offline),
        ] {
            assert_eq!(outcome(verdict), failed(problem, String::new()));
        }
        assert_eq!(
            outcome(Verdict::Other("HTTP 503".into())),
            failed(GoogleProblem::Other, "HTTP 503".into())
        );
    }

    #[test]
    fn setting_a_service_up_chooses_its_model_unless_one_of_its_is_chosen() {
        let fresh = Settings::default();
        let gemini = dictating_with(&fresh, GoogleService::Gemini);
        assert_eq!(gemini.provider, ProviderKind::Gemini);
        assert!(models::find(gemini.provider, &gemini.model).is_some());
        let speech = dictating_with(&gemini, GoogleService::Speech);
        assert_eq!(speech.provider, ProviderKind::GoogleCloud);
        assert!(models::find(speech.provider, &speech.model).is_some());
        // A model of the service that someone set by hand stays.
        let by_hand = Settings {
            provider: ProviderKind::GoogleCloud,
            model: "chirp_2".into(),
            ..Settings::default()
        };
        assert_eq!(dictating_with(&by_hand, GoogleService::Speech), by_hand);
    }

    #[test]
    fn what_is_entered_is_shown_before_what_is_kept() {
        let settings = Settings::default();
        let mut google = Google::default();
        let account = |google: &Google| {
            let facts = google.facts(Some("kept@x"), false, &settings);
            facts.account.map(str::to_owned)
        };
        assert_eq!(account(&google).as_deref(), Some("kept@x"));
        google.sign_in = true;
        assert_eq!(account(&google), None);
        google.picked = Some(ServiceAccount {
            email: "picked@x".into(),
            project_id: None,
            key: "secret".into(),
        });
        assert_eq!(account(&google).as_deref(), Some("picked@x"));
    }

    #[test]
    fn a_question_that_was_called_off_leaves_nothing_behind() {
        let mut google = Google {
            key: Some("entered".into()),
            check: GoogleCheck::Checking,
            ..Google::default()
        };
        let asked = google.round;
        google.call_off();
        assert!(google.key.is_none() && google.round != asked);
        assert_eq!(google.check, GoogleCheck::Idle);
    }

    #[test]
    fn a_late_answer_to_an_old_question_does_not_take_the_place_of_the_newest() {
        let mut google = Google::default();
        // Two questions, the second replacing the first.
        google.ask_anew();
        let old = google.round;
        google.ask_anew();
        let new = google.round;
        google.check = GoogleCheck::Checking;
        let leave = |round, verdict| {
            let mut left = google.left.lock().unwrap_or_else(PoisonError::into_inner);
            left.push_back((round, Answer::Verdict(verdict)));
        };
        // The newer one is answered first, and the older one after it,
        // before the loop has looked: the order that used to lose the answer.
        leave(new, Verdict::Works);
        leave(old, Verdict::WrongKey);
        assert!(matches!(
            google.next_answer(),
            Some(Answer::Verdict(Verdict::Works))
        ));
        assert!(google.next_answer().is_none());
        // And the other way round.
        leave(old, Verdict::WrongKey);
        leave(new, Verdict::Billing);
        assert!(matches!(
            google.next_answer(),
            Some(Answer::Verdict(Verdict::Billing))
        ));
        assert!(google.next_answer().is_none());
        // An answer nobody waits for any more is dropped.
        leave(new, Verdict::Works);
        google.call_off();
        assert!(google.next_answer().is_none());
    }

    #[test]
    fn answers_arrive_from_their_threads_each_in_its_own_place() {
        let left = Answers::default();
        // No window: the wake-up goes to this thread, which reads none.
        for round in 1..=6 {
            let verdict = if round == 6 {
                Verdict::Works
            } else {
                Verdict::Offline
            };
            assert!(ask_on_a_thread(
                Arc::clone(&left),
                round,
                0,
                Box::new(move || {
                    // The older the question, the later its answer.
                    thread::sleep(std::time::Duration::from_millis(5 * (7 - round)));
                    Answer::Verdict(verdict)
                }),
            ));
        }
        let started = std::time::Instant::now();
        let arrived = || left.lock().unwrap_or_else(PoisonError::into_inner).len();
        while arrived() < 6 && started.elapsed() < std::time::Duration::from_secs(5) {
            thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(arrived(), 6);
        let google = Google {
            round: 6,
            left,
            ..Google::default()
        };
        assert!(matches!(
            google.next_answer(),
            Some(Answer::Verdict(Verdict::Works))
        ));
    }

    #[test]
    fn a_key_that_was_entered_is_counted_for_the_page_and_never_shown() {
        let settings = Settings::default();
        let mut google = Google::default();
        assert_eq!(google.facts(None, false, &settings).key_length, 0);
        google.key = Some("AIza-\u{17e}\u{17e}".to_owned());
        google.replacing = true;
        let facts = google.facts(None, false, &settings);
        assert_eq!((facts.key_length, facts.replacing), (7, true));
        google.key = Some("k".repeat(1000));
        assert_eq!(google.facts(None, false, &settings).key_length, 255);
        // Asking again keeps it; calling the question off forgets it.
        google.ask_anew();
        assert!(google.key.is_some());
        google.call_off();
        assert!(google.key.is_none());
    }

    #[test]
    fn google_cloud_needs_a_project_and_a_way_to_sign_in() {
        let google = Google::default();
        let mut settings = Settings::default();
        let ready = |settings: &Settings, stored: Option<&str>, sign_in| {
            google.facts(stored, sign_in, settings).ready
        };
        assert!(!ready(&settings, Some("kept@x"), true));
        settings.project_id = Some("demo".into());
        assert!(ready(&settings, Some("kept@x"), false));
        assert!(ready(&settings, None, true));
        assert!(!ready(&settings, None, false));
    }
}
