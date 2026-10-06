//! `VinPUT` Pro: signing in, the subscription, and keeping what the windows
//! show about them current.
//!
//! Everything asked of the server waits for an answer, so nothing here asks
//! on the thread of the message loop: each request runs on a thread of its
//! own, which leaves a [`Report`] and posts `PRO_HEARD`. The loop then takes
//! the reports in and changes what it knows ([`Pro`]); the windows are shown
//! that like everything else.
//!
//! What a report changes is decided by [`Pro::take`], which touches nothing
//! but what the host knows and says what is to be done besides as
//! [`Effect`]s: a page to open, the provider to set up again. So the whole
//! of it runs in a test without a server, a browser or a window.
//!
//! Paying happens in the browser. After a checkout was opened the account is
//! asked every few seconds, for a quarter of an hour at most, until the
//! payment has arrived; when it has, Pro becomes the speech model. A server
//! that is busy or cannot be reached for a moment does not end that wait,
//! and whatever does end it leaves the page as it was before. Besides that
//! the account is asked when a window opens and now and then while one is
//! open. While none is, it is asked twice at most: once when Vinput starts
//! with Pro as the chosen model, and once after a dictation that a model on
//! this PC had to serve because the month's hours are used up. Both are to
//! know until when that is so (`vinput-core/src/stt/standby.rs`), which is
//! said once, in a notification, and not with every dictation.
#![allow(unsafe_code)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

use super::tray::About;
use super::{Host, PRO_HEARD};
use crate::view::{ProDoing, ProFacts};
use crate::win::{is_page, open};
use vinput_core::cloud::{self, Account, AccountInfo, Failure, Plan, Prices, Reason, SignIn, Tier};
use vinput_core::session::Controller;
use vinput_core::settings::Settings;
use vinput_core::stt::{ProviderKind, Standby};
use vinput_core::{log, models};
use vinput_view::{self as wire, Page};

/// How often the account is asked while a payment is waited for: every few
/// seconds while the person is likely still at the checkout, and less often
/// after that.
const PAYMENT_PACE: Pace = Pace {
    quick: Duration::from_secs(3),
    quick_for: Duration::from_secs(2 * 60),
    slow: Duration::from_secs(12),
    // Someone who closed the checkout said nothing; after this long the
    // page goes back to how the account stands.
    limit: Duration::from_secs(15 * 60),
};
/// How often the account is asked while a window is open, so that what was
/// done in the browser (a card changed, a subscription cancelled) shows.
const ASK_EVERY: Duration = Duration::from_secs(20);
/// How long the prices the server gave are shown before it is asked again.
const PRICES_FOR: Duration = Duration::from_secs(10 * 60);
/// The same while no plan can be subscribed to yet: the day it can is
/// shown by itself, to whoever looks at the page again.
const SOON_FOR: Duration = Duration::from_secs(60);
/// How often a wait looks at whether it was called off.
const STOP_STEP: Duration = Duration::from_millis(100);

/// What a thread that asked the server leaves for the loop.
#[derive(Debug)]
enum Report {
    /// A sign-in was started: the page to open and the code to show.
    SignInStarted(SignIn),
    /// The sign-in was finished in the browser; this address signed in.
    SignedIn(String),
    /// The sign-in came to nothing.
    SignInFailed(Failure),
    /// How the account stands, or why the server did not say.
    Info(Result<AccountInfo, Failure>),
    /// The checkout to open in the browser.
    Checkout(String),
    /// The page of the subscription to open in the browser.
    Portal(String),
    /// A page for paying could not be had.
    NoPage(Failure),
    /// The wait for a payment is over without one.
    PaymentWaitOver,
    /// The session has been let go of, here and at the server.
    SignedOut,
    /// What the plans cost.
    Prices(Prices),
}

/// What the loop is to do after a report, besides showing what changed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Effect {
    /// Open the page on which a sign-in is finished.
    OpenSignIn(String),
    /// Open the checkout, and wait for the payment if it opened.
    OpenCheckout(String),
    /// Open the page on which the subscription is managed.
    OpenPortal(String),
    /// Set the speech provider up again, if it is Pro: who is signed in
    /// has changed.
    Reconnect,
    /// Ask the server how the account stands; `true` if the page shows
    /// that it is being asked.
    Ask(bool),
    /// The payment that was waited for has arrived: dictate with Pro.
    Paid,
}

/// How a wait for a payment paces itself.
#[derive(Debug, Clone, Copy)]
struct Pace {
    quick: Duration,
    quick_for: Duration,
    slow: Duration,
    limit: Duration,
}

/// Where a thread leaves its reports. Each belongs to the sign-in it was
/// started under, so that what a thread finds out about an account that has
/// since signed out is not taken for the next one's.
#[derive(Clone)]
struct Reporter {
    left: Arc<Mutex<VecDeque<(u64, Report)>>>,
    session: u64,
    /// A window handle is an address that any thread may post to.
    window: usize,
}

impl Reporter {
    fn send(&self, report: Report) {
        self.left
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back((self.session, report));
        // SAFETY: posting to a window that has gone away fails harmlessly.
        unsafe { PostMessageW(self.window as HWND, PRO_HEARD, 0, 0) };
    }
}

/// What a thread for `VinPUT` Pro does.
type Job = Box<dyn FnOnce(&Account, &Reporter) + Send>;

/// What the host knows about `VinPUT` Pro.
pub(super) struct Pro {
    account: Arc<Account>,
    left: Arc<Mutex<VecDeque<(u64, Report)>>>,
    /// Counts the sign-ins and sign-outs; see [`Reporter`].
    session: u64,
    /// The address that is signed in.
    email: Option<String>,
    /// The sign-in under way: its code, once the server has handed it out,
    /// how many minutes that works for, and what calls it off.
    signing_in: Option<(String, u32, Arc<AtomicBool>)>,
    /// How the account stood when the server last said, and when that was.
    info: Option<(AccountInfo, Instant)>,
    doing: ProDoing,
    /// Calls off the wait for a payment.
    payment: Arc<AtomicBool>,
    /// Why the last request failed, in words for the user.
    problem: Option<String>,
    /// When the account was last asked about.
    asked: Option<Instant>,
    /// What the plans cost: the built-in prices until the server has said.
    prices: Prices,
    /// When the server was last asked for the prices.
    prices_asked: Option<Instant>,
    /// The model on this PC that dictation carries on with when the
    /// month's hours are used up, and whether it does.
    standby: Arc<Standby>,
    /// Until when it is not said again that the hours are used up, in
    /// seconds since 1970.
    told_until: u64,
}

/// For how long at least it is not said again that the hours are used up.
const TOLD_FOR: u64 = 24 * 3600;

impl Pro {
    pub(super) fn new(settings: &Settings) -> Self {
        let account = cloud::shared();
        let pro = Self {
            standby: Standby::shared(),
            ..Self::of(account.signed_in_as(), account)
        };
        pro.prefer(settings);
        pro
    }

    /// Notes which model on this PC the settings would carry on with.
    pub(super) fn prefer(&self, settings: &Settings) {
        self.standby
            .prefer(&settings.last_local_model, settings.use_gpu());
    }

    /// Name of the model on this PC that serves in place of `VinPUT` Pro
    /// right now, because the month's hours are used up; `None` while Pro
    /// serves, which is all that is looked at then.
    pub(super) fn carrying_with(&self, settings: &Settings) -> Option<&'static str> {
        if settings.provider != ProviderKind::Cloud || self.standby.until() == 0 {
            return None;
        }
        models::spare(&settings.last_local_model, settings.use_gpu()).map(|model| model.name)
    }

    /// Why `VinPUT` Pro cannot serve a dictation as things stand, as far as
    /// is known without asking: nobody is signed in, the account has no
    /// subscription that is paid for, or the month's hours are used up and
    /// no model on this PC carries on. `None` if it can, and while the
    /// server has not said how the account stands.
    ///
    /// What is known may be old: someone may have subscribed in a browser
    /// since. The page this leads to asks the account when it opens, so
    /// that is found out there, and the next press finds it known.
    ///
    /// This is asked at every key press. With hours left it looks at
    /// nothing but what is in memory; which model carries on is looked up
    /// only while one has to.
    pub(super) fn lacks(&self, settings: &Settings) -> Option<&'static str> {
        self.lacks_unless(|| self.carrying_with(settings).is_some())
    }

    /// [`Pro::lacks`], where `carries` says whether a model on this PC
    /// carries on when the hours are used up.
    fn lacks_unless(&self, carries: impl FnOnce() -> bool) -> Option<&'static str> {
        if self.email.is_none() {
            return Some("Sign in to VinPUT Pro to dictate with it, or use another model.");
        }
        if self.standby.until() != 0 {
            return (!carries()).then_some("This month's VinPUT Pro hours are used up.");
        }
        let (info, _) = self.info.as_ref()?;
        (!info.entitled && !info.complimentary)
            .then_some("VinPUT Pro needs a subscription. Choose a plan, or use another model.")
    }

    /// What the host knows when it starts, with `email` signed in.
    fn of(email: Option<String>, account: Arc<Account>) -> Self {
        Self {
            email,
            account,
            left: Arc::default(),
            session: 0,
            signing_in: None,
            info: None,
            doing: ProDoing::Nothing,
            payment: Arc::default(),
            problem: None,
            asked: None,
            prices: Prices::default(),
            prices_asked: None,
            standby: Arc::default(),
            told_until: 0,
        }
    }

    /// What a picture for the windows is drawn from.
    pub(super) fn facts(&self) -> ProFacts<'_> {
        let sign_in = self.signing_in.as_ref();
        ProFacts {
            email: self.email.as_deref(),
            signing_in: sign_in.map(|(code, minutes, _)| (code.as_str(), *minutes)),
            info: self.info.as_ref().map(|(info, _)| info),
            // Only said while it matters: what is shown is not what the
            // server says now, because it cannot be asked.
            checked: self
                .info
                .as_ref()
                .filter(|_| self.problem.is_some())
                .map(|(_, when)| when.elapsed()),
            doing: self.doing,
            problem: self.problem.as_deref(),
            prices: &self.prices,
            carrying: self.standby.until() != 0,
        }
    }

    /// Runs `job` on a thread of its own, with the place to leave reports.
    /// Says whether the thread was started.
    fn start(&self, window: HWND, job: Job) -> bool {
        let reporter = Reporter {
            left: Arc::clone(&self.left),
            session: self.session,
            window: window as usize,
        };
        let account = Arc::clone(&self.account);
        let started = thread::Builder::new()
            .name("vinput-pro".into())
            .spawn(move || job(&account, &reporter));
        if let Err(err) = &started {
            log::write(&format!("starting a thread for VinPUT Pro failed: {err}"));
        }
        started.is_ok()
    }

    /// Nothing could be started for what was just asked: the page does not
    /// go on waiting for an answer that will never come.
    fn not_started(&mut self) {
        if let Some((_, _, stop)) = self.signing_in.take() {
            stop.store(true, Ordering::Relaxed);
        }
        self.doing = ProDoing::Nothing;
        self.problem = Some("VinPUT could not start that. Try again in a moment.".to_owned());
    }

    /// Forgets everything about the account that was signed in, and has
    /// whatever its threads still find out ignored.
    fn reset(&mut self) {
        self.session += 1;
        if let Some((_, _, stop)) = self.signing_in.take() {
            stop.store(true, Ordering::Relaxed);
        }
        self.payment.store(true, Ordering::Relaxed);
        self.standby.clear();
        self.email = None;
        self.info = None;
        self.doing = ProDoing::Nothing;
        self.asked = None;
    }

    /// Begins a sign-in, if none is under way and nobody is signed in.
    /// Returns what calls it off.
    fn begin_sign_in(&mut self) -> Option<Arc<AtomicBool>> {
        if self.signing_in.is_some() || self.email.is_some() {
            return None;
        }
        self.session += 1;
        self.problem = None;
        let stop = Arc::new(AtomicBool::new(false));
        self.signing_in = Some((String::new(), 0, Arc::clone(&stop)));
        Some(stop)
    }

    /// Stops waiting for a sign-in or for a payment. Says whether the
    /// account is to be asked about, as after a wait for a payment.
    fn cancel(&mut self) -> bool {
        if let Some((_, _, stop)) = self.signing_in.take() {
            stop.store(true, Ordering::Relaxed);
            self.session += 1;
        }
        self.problem = None;
        if self.doing == ProDoing::AwaitingPayment {
            self.payment.store(true, Ordering::Relaxed);
            self.doing = ProDoing::Nothing;
            return true;
        }
        false
    }

    /// Notes that the account is being asked about. `shown` says whether
    /// the page shows that. Says whether to ask at all.
    fn begin_ask(&mut self, shown: bool) -> bool {
        if self.email.is_none() || self.doing == ProDoing::Asking {
            return false;
        }
        if shown && self.doing == ProDoing::Nothing {
            self.doing = ProDoing::Asking;
            self.problem = None;
        }
        self.asked = Some(Instant::now());
        true
    }

    /// Notes that a page for paying is being fetched. Says whether to.
    fn begin_opening(&mut self) -> bool {
        if self.email.is_none() || self.doing != ProDoing::Nothing {
            return false;
        }
        self.doing = ProDoing::Opening;
        self.problem = None;
        true
    }

    /// The checkout was handed to the browser, or could not be. Returns
    /// what calls the wait for the payment off, if there is one to start.
    fn checkout_opened(&mut self, opened: bool) -> Option<Arc<AtomicBool>> {
        if !opened {
            self.doing = ProDoing::Nothing;
            return None;
        }
        self.doing = ProDoing::AwaitingPayment;
        let stop = Arc::new(AtomicBool::new(false));
        self.payment = Arc::clone(&stop);
        Some(stop)
    }

    /// Takes in what a thread started under the sign-in `session` found
    /// out: changes what is known, and returns what is to be done besides.
    fn take(&mut self, session: u64, report: Report) -> Vec<Effect> {
        // The prices are everyone's, whoever is signed in.
        if let Report::Prices(prices) = report {
            self.prices = prices;
            return Vec::new();
        }
        if session != self.session {
            // A sign-in that was called off at the very moment it was
            // finished has left a session behind. Unless someone else is
            // signed in or signing in by now, it is shown for what it is.
            return match report {
                Report::SignedIn(email) if self.email.is_none() && self.signing_in.is_none() => {
                    self.signed_in(email)
                }
                _ => Vec::new(),
            };
        }
        match report {
            Report::SignInStarted(sign_in) => {
                let Some((code, minutes, _)) = &mut self.signing_in else {
                    return Vec::new();
                };
                code.clone_from(&sign_in.confirm_code);
                *minutes = sign_in.minutes();
                vec![Effect::OpenSignIn(sign_in.verification_url)]
            }
            Report::SignedIn(email) => self.signed_in(email),
            Report::SignInFailed(failure) => {
                self.signing_in = None;
                self.problem = Some(words(&failure)).filter(|words| !words.is_empty());
                Vec::new()
            }
            Report::Info(Ok(info)) => self.heard(info),
            Report::Info(Err(Failure::SignedOut)) => {
                self.reset();
                self.problem = Some(words(&Failure::SignedOut));
                vec![Effect::Reconnect]
            }
            Report::Info(Err(failure)) => {
                if self.doing == ProDoing::Asking {
                    self.doing = ProDoing::Nothing;
                }
                // While a payment is waited for, what passes is asked about
                // again by the wait itself; the next look may do better.
                if self.doing != ProDoing::AwaitingPayment || !failure.is_passing() {
                    self.problem = Some(words(&failure));
                }
                Vec::new()
            }
            Report::Checkout(address) => vec![Effect::OpenCheckout(address)],
            Report::Portal(address) => {
                self.doing = ProDoing::Nothing;
                // What is changed there shows at the next look, soon.
                self.asked = None;
                vec![Effect::OpenPortal(address)]
            }
            Report::NoPage(Failure::Refused {
                reason: Reason::NothingToManage,
                ..
            }) => {
                // Nothing was ever bought: the plans are what to show.
                self.doing = ProDoing::Nothing;
                self.problem = Some(
                    "There is no subscription to manage yet. Choose a plan to subscribe."
                        .to_owned(),
                );
                vec![Effect::Ask(false)]
            }
            Report::NoPage(Failure::SignedOut) => {
                self.take(session, Report::Info(Err(Failure::SignedOut)))
            }
            Report::NoPage(failure) => {
                self.doing = ProDoing::Nothing;
                // A server that takes no payments yet has refused nothing
                // that asking again would mend: it is said as news.
                let soon = matches!(
                    failure,
                    Failure::Refused {
                        code: cloud::Code::Unavailable,
                        ..
                    }
                );
                self.problem = Some(if soon {
                    wire::PRO_SOON.to_owned()
                } else {
                    words(&failure)
                });
                Vec::new()
            }
            Report::PaymentWaitOver => {
                if self.doing == ProDoing::AwaitingPayment {
                    self.doing = ProDoing::Nothing;
                }
                Vec::new()
            }
            Report::SignedOut => vec![Effect::Reconnect],
            Report::Prices(_) => Vec::new(),
        }
    }

    /// Someone has signed in.
    fn signed_in(&mut self, email: String) -> Vec<Effect> {
        self.signing_in = None;
        self.email = Some(email);
        self.problem = None;
        // The provider that said "not signed in" is set up again.
        vec![Effect::Reconnect, Effect::Ask(true)]
    }

    /// The server has said how the account stands.
    fn heard(&mut self, info: AccountInfo) -> Vec<Effect> {
        if self.doing == ProDoing::Asking {
            self.doing = ProDoing::Nothing;
        }
        self.problem = None;
        if !info.email.is_empty() {
            self.email = Some(info.email.clone());
        }
        let paid = self.doing == ProDoing::AwaitingPayment && info.entitled;
        // With hours left, dictation is served by VinPUT Pro; with none,
        // by the model on this PC until they are counted anew.
        if info.entitled || info.complimentary {
            self.standby
                .heard(info.used_seconds, info.cap_seconds, info.resets_at);
        } else {
            self.standby.clear();
        }
        self.info = Some((info, Instant::now()));
        if !paid {
            return Vec::new();
        }
        // The payment that was waited for has arrived.
        self.payment.store(true, Ordering::Relaxed);
        self.doing = ProDoing::Nothing;
        vec![Effect::Paid]
    }
}

/// A failure in words for the page.
fn words(failure: &Failure) -> String {
    match failure {
        Failure::SignedOut => {
            "This PC was signed out of VinPUT Pro. Sign in again to use it.".to_owned()
        }
        Failure::Unreachable(why) => {
            format!("VinPUT Pro could not be reached. Check your internet connection. ({why})")
        }
        // The server ends some of its sentences itself.
        Failure::Refused { message, .. } => format!(
            "VinPUT Pro could not do that: {}.",
            message.trim_end_matches('.')
        ),
        Failure::Expired => {
            "The sign-in was not finished in time. Start it again when you are ready.".to_owned()
        }
        Failure::Stopped => String::new(),
    }
}

/// What is said when the hours of the month are used up and dictation has
/// carried on with `model`. `whole` says whether everything that was said
/// reached it.
fn carrying_on(model: &str, whole: bool) -> String {
    let mut text =
        format!("Dictation carries on with {model}, on this PC, until the new month starts.");
    if !whole {
        text.push_str(" What was said just before the switch may be missing.");
    }
    text
}

/// Sleeps for `time` unless `stop` is set first. Says whether it was.
fn stopped_within(stop: &AtomicBool, time: Duration) -> bool {
    let started = Instant::now();
    while started.elapsed() < time {
        if stop.load(Ordering::Relaxed) {
            return true;
        }
        thread::sleep(STOP_STEP.min(time));
    }
    stop.load(Ordering::Relaxed)
}

/// Asks the account at `pace` until the payment made in the browser has
/// arrived (which whoever takes the reports in says by setting `stop`), the
/// wait is called off, or its time is up. A server that is busy, at fault or
/// out of reach for a moment is asked again; anything else ends the wait.
/// However it ends, unless it was called off, the last thing sent says that
/// it is over, so that the page never goes on waiting for nothing.
fn await_payment(
    ask: &mut dyn FnMut() -> Result<AccountInfo, Failure>,
    stop: &AtomicBool,
    send: &mut dyn FnMut(Report),
    pace: Pace,
) {
    let started = Instant::now();
    while started.elapsed() < pace.limit {
        let wait = if started.elapsed() < pace.quick_for {
            pace.quick
        } else {
            pace.slow
        };
        if stopped_within(stop, wait) {
            return;
        }
        let info = ask();
        let over = matches!(&info, Err(failure) if !failure.is_passing());
        send(Report::Info(info));
        if over {
            break;
        }
    }
    send(Report::PaymentWaitOver);
}

/// The page to open for a subscription to `plan` of `tier`: its checkout,
/// or for someone who has a subscription already, also one that is not
/// paid for, the page on which that is managed.
fn page_for(account: &Account, tier: Tier, plan: Plan) -> Report {
    match account.checkout(tier, plan) {
        Ok(url) => Report::Checkout(url),
        Err(Failure::Refused {
            reason: Reason::AlreadySubscribed,
            ..
        }) => manage(account),
        Err(failure) => Report::NoPage(failure),
    }
}

/// The page on which the subscription is managed.
fn manage(account: &Account) -> Report {
    match account.portal() {
        Ok(url) => Report::Portal(url),
        Err(failure) => Report::NoPage(failure),
    }
}

impl Host {
    /// Runs `job` for `VinPUT` Pro on a thread of its own, or says on the
    /// page that it could not be started.
    fn pro_start(&mut self, job: Job) {
        if !self.pro.start(self.hwnd, job) {
            self.pro.not_started();
        }
    }

    /// Starts a sign-in: the browser opens once the server has said where.
    pub(super) fn pro_sign_in(&mut self) {
        let Some(stop) = self.pro.begin_sign_in() else {
            return;
        };
        self.pro_start(Box::new(move |account, reporter| {
            let sign_in = match account.start_sign_in() {
                Ok(sign_in) => sign_in,
                Err(failure) => return reporter.send(Report::SignInFailed(failure)),
            };
            reporter.send(Report::SignInStarted(sign_in.clone()));
            reporter.send(match account.finish_sign_in(&sign_in, &stop) {
                Ok(email) => Report::SignedIn(email),
                Err(failure) => Report::SignInFailed(failure),
            });
        }));
    }

    /// Stops waiting for a sign-in or for a payment. What was started in
    /// the browser is left alone: a payment that still arrives shows the
    /// next time the account is asked.
    pub(super) fn pro_cancel(&mut self) {
        if self.pro.cancel() {
            self.pro_ask(false);
        }
    }

    /// Asks the server how the account stands. `shown` says whether the
    /// page shows that it is being asked, as it does when the user asked.
    pub(super) fn pro_ask(&mut self, shown: bool) {
        if self.pro.begin_ask(shown) {
            self.pro_start(Box::new(|account, reporter| {
                reporter.send(Report::Info(account.info()));
            }));
        }
    }

    /// A window has opened: what was done in the browser since the last
    /// one closed shows from the start.
    pub(super) fn pro_window_opened(&mut self) {
        self.pro.asked = None;
        self.pro_keep_current();
    }

    /// The page of the account is in sight: the prices on it are the
    /// server's, asked for now unless they were a little while ago, and the
    /// account is as current as it gets.
    pub(super) fn pro_shown(&mut self) {
        let kept = if self.pro.prices.open {
            PRICES_FOR
        } else {
            SOON_FOR
        };
        let due = self
            .pro
            .prices_asked
            .is_none_or(|asked| asked.elapsed() >= kept);
        if due {
            self.pro.prices_asked = Some(Instant::now());
            // Should the thread not start, the built-in prices stand.
            self.pro.start(
                self.hwnd,
                Box::new(|account, reporter| {
                    if let Ok(prices) = account.prices() {
                        reporter.send(Report::Prices(prices));
                    }
                }),
            );
        }
        self.pro_keep_current();
    }

    /// Asks about the account if that was not done for a while. For the
    /// timer that runs while a window is open.
    pub(super) fn pro_keep_current(&mut self) {
        let due = self
            .pro
            .asked
            .is_none_or(|asked| asked.elapsed() >= ASK_EVERY);
        if due && self.pro.doing == ProDoing::Nothing {
            // The first time the page says that it is looking; after that
            // what it shows is only brought up to date.
            self.pro_ask(self.pro.info.is_none() && self.pro.problem.is_none());
        }
    }

    /// Opens the checkout for `plan` of `tier` in the browser. Someone who
    /// has a subscription already, also one that is not paid for, gets the
    /// page on which it is managed instead.
    pub(super) fn pro_subscribe(&mut self, tier: wire::ProTier, plan: wire::ProPlan) {
        if !self.pro.begin_opening() {
            return;
        }
        let tier = match tier {
            wire::ProTier::Pro => Tier::Pro,
            wire::ProTier::Plus => Tier::Plus,
        };
        let plan = match plan {
            wire::ProPlan::Monthly => Plan::Monthly,
            wire::ProPlan::Yearly => Plan::Yearly,
        };
        self.pro_start(Box::new(move |account, reporter| {
            reporter.send(page_for(account, tier, plan));
        }));
    }

    /// A session has ended. If the model on this PC served it, because the
    /// hours of the month are used up, that is said, once for a month and
    /// not with every dictation, and the account is asked until when.
    pub(super) fn pro_after_session(&mut self) {
        let Some(whole) = self.pro.standby.take_served() else {
            return;
        };
        let known = self.pro.info.as_ref();
        if !known.is_some_and(|(info, _)| info.over_allowance()) {
            self.pro.asked = None;
            self.pro_ask(false);
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        if now < self.pro.told_until {
            return;
        }
        self.pro.told_until = self.pro.standby.until().max(now + TOLD_FOR);
        let model = self.pro.carrying_with(&self.settings);
        self.tray.inform(
            About::Settings(Page::Pro),
            "VinPUT Pro hours used up",
            &carrying_on(model.unwrap_or("a model on this PC"), whole),
        );
        self.refresh_tip();
        self.show_state();
    }

    /// Opens the page on which the subscription is managed in the browser.
    pub(super) fn pro_manage(&mut self) {
        if self.pro.begin_opening() {
            self.pro_start(Box::new(|account, reporter| {
                reporter.send(manage(account));
            }));
        }
    }

    /// Signs out. The page shows it at once; the speech provider follows
    /// when the session has been let go of.
    pub(super) fn pro_sign_out(&mut self) {
        self.pro.reset();
        self.pro.problem = None;
        self.pro_start(Box::new(|account, reporter| {
            account.sign_out();
            reporter.send(Report::SignedOut);
        }));
    }

    /// A dictation with Pro failed: the account is asked, so that the pages
    /// show why.
    pub(super) fn pro_after_failure(&mut self) {
        self.pro.asked = None;
        self.pro_ask(false);
    }

    /// Opens a page the server handed out in the browser. Says whether it
    /// did.
    fn pro_open(&mut self, address: &str) -> bool {
        let opened = is_page(address, self.pro.account.server()) && open(address);
        if !opened {
            log::write("could not open the browser for a page of VinPUT Pro");
            self.pro.problem = Some("Windows could not open the page in a browser.".to_owned());
        }
        opened
    }

    /// Takes in what the threads have found out.
    pub(super) fn on_pro_heard(&mut self, controller: &mut dyn Controller) {
        loop {
            let next = self
                .pro
                .left
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop_front();
            let Some((session, report)) = next else {
                break;
            };
            for effect in self.pro.take(session, report) {
                self.pro_do(effect, controller);
            }
        }
        // The account may have said that the hours are used up, or back.
        self.refresh_tip();
        self.show_state();
    }

    fn pro_do(&mut self, effect: Effect, controller: &mut dyn Controller) {
        match effect {
            Effect::OpenSignIn(address) | Effect::OpenPortal(address) => {
                self.pro_open(&address);
            }
            Effect::OpenCheckout(address) => {
                let opened = self.pro_open(&address);
                if let Some(stop) = self.pro.checkout_opened(opened) {
                    self.pro_start(Box::new(move |account, reporter| {
                        await_payment(
                            &mut || account.info(),
                            &stop,
                            &mut |report| reporter.send(report),
                            PAYMENT_PACE,
                        );
                    }));
                }
            }
            // Whoever dictates with another model is left alone: setting a
            // local model up again would load it again.
            Effect::Reconnect => {
                if self.settings.provider == ProviderKind::Cloud {
                    controller.configure(&self.settings, true);
                }
            }
            Effect::Ask(shown) => self.pro_ask(shown),
            Effect::Paid => self.pro_paid(controller),
        }
    }

    /// Whoever has just subscribed wants to dictate with what they bought.
    fn pro_paid(&mut self, controller: &mut dyn Controller) {
        let text = match models::by_id(cloud::MODEL_ID) {
            Some(model) => {
                self.choose_model(model, controller);
                "Thank you. VinPUT Pro is active, and dictation uses it from now on."
            }
            None => "Thank you. VinPUT Pro is active.",
        };
        self.notice = Some(wire::Notice {
            kind: wire::NoticeKind::Success,
            text: text.to_owned(),
            page: Page::Pro,
        });
    }
}

#[cfg(test)]
mod tests {
    use vinput_core::cloud::{Code, Server, Subscription};

    use super::*;

    /// What the host knows at its start, with `email` signed in. Nothing in
    /// these tests asks the account for anything.
    fn pro(email: Option<&str>) -> Pro {
        let server = Server::from_env();
        let account = Account::with_access_token(server, "none");
        Pro::of(email.map(str::to_owned), Arc::new(account))
    }

    fn info(entitled: bool) -> AccountInfo {
        AccountInfo {
            email: "ana@example.com".into(),
            entitled,
            subscription: entitled.then(|| Subscription {
                status: "active".into(),
                plan: Some(Plan::Monthly),
                period_end: 1_793_837_814,
                ends: false,
                grace_until: 0,
                ends_at: 0,
                ended_at: 0,
            }),
            used_seconds: 0,
            cap_seconds: 144_000,
            tier: Tier::Pro,
            resets_at: 0,
            complimentary: false,
            intro_eligible: false,
        }
    }

    fn refused(code: Code) -> Failure {
        Failure::refused(code, "not now")
    }

    #[test]
    fn vinput_pro_cannot_serve_signed_out_or_without_a_subscription() {
        let settings = Settings {
            provider: ProviderKind::Cloud,
            model: cloud::MODEL_ID.to_owned(),
            ..Settings::default()
        };
        let lacks =
            |pro: &Pro, word: &str| pro.lacks(&settings).is_some_and(|why| why.contains(word));
        // Nobody is signed in: there is nothing to ask the server.
        assert!(lacks(&pro(None), "Sign in"));
        // Signed in, and the server has not said yet: a session finds out.
        let mut pro = pro(Some("ana@example.com"));
        assert_eq!(pro.lacks(&settings), None);
        // No subscription that is paid for.
        told(&mut pro, vec![Report::Info(Ok(info(false)))]);
        assert!(lacks(&pro, "subscription"));
        // An account the server gives Pro to needs none.
        let given = AccountInfo {
            complimentary: true,
            ..info(false)
        };
        told(&mut pro, vec![Report::Info(Ok(given))]);
        assert_eq!(pro.lacks(&settings), None);
        // Subscribed, in the browser: known the next time the account says.
        told(&mut pro, vec![Report::Info(Ok(info(false)))]);
        told(&mut pro, vec![Report::Info(Ok(info(true)))]);
        assert_eq!(pro.lacks(&settings), None);
        // A server that cannot be reached changes nothing about that.
        let unreachable = Failure::Unreachable("offline".into());
        told(&mut pro, vec![Report::Info(Err(unreachable))]);
        assert_eq!(pro.lacks(&settings), None);
        // Signed out elsewhere.
        told(&mut pro, vec![Report::Info(Err(Failure::SignedOut))]);
        assert!(lacks(&pro, "Sign in"));
    }

    #[test]
    fn used_up_hours_stop_vinput_pro_only_when_no_model_on_this_pc_carries_on() {
        let mut pro = pro(Some("ana@example.com"));
        let asked = std::cell::Cell::new(0);
        let lacks = |pro: &Pro, carries: bool| {
            pro.lacks_unless(|| {
                asked.set(asked.get() + 1);
                carries
            })
        };
        // Hours left, or nothing known yet: whether a model is on this PC
        // is nobody's business, and the disk is not looked at for one.
        assert_eq!(lacks(&pro, false), None);
        let mut account = info(true);
        account.used_seconds = 100;
        account.resets_at = u64::MAX;
        told(&mut pro, vec![Report::Info(Ok(account.clone()))]);
        assert_eq!(lacks(&pro, false), None);
        assert_eq!(asked.get(), 0);
        // None left. A model on this PC carries on: a session is started.
        account.used_seconds = account.cap_seconds;
        told(&mut pro, vec![Report::Info(Ok(account.clone()))]);
        assert_eq!(lacks(&pro, true), None);
        // Without one there is nothing to dictate with.
        assert!(lacks(&pro, false).is_some_and(|why| why.contains("used up")));
        assert_eq!(asked.get(), 2);
        // The same for an account the server gives Pro to.
        let given = AccountInfo {
            entitled: false,
            complimentary: true,
            ..account.clone()
        };
        told(&mut pro, vec![Report::Info(Ok(given))]);
        assert_eq!(lacks(&pro, true), None);
        assert!(lacks(&pro, false).is_some());
        // A server out of reach leaves what was known: the model carries on.
        let unreachable = Failure::Unreachable("offline".into());
        told(&mut pro, vec![Report::Info(Err(unreachable))]);
        assert_eq!(lacks(&pro, true), None);
        // A tier with more hours, or a new month: Pro again.
        account.cap_seconds *= 2;
        told(&mut pro, vec![Report::Info(Ok(account.clone()))]);
        assert_eq!(lacks(&pro, false), None);
        // A subscription that is not paid for has no hours to have run out:
        // it is the subscription that is missing, model or no model.
        account.used_seconds = account.cap_seconds;
        account.entitled = false;
        told(&mut pro, vec![Report::Info(Ok(account))]);
        assert!(lacks(&pro, true).is_some_and(|why| why.contains("subscription")));
        // Signed out, the sign-in is.
        pro.reset();
        assert!(lacks(&pro, true).is_some_and(|why| why.contains("Sign in")));
    }

    /// The reports in the order they arrive, under the sign-in that is
    /// current when each does.
    fn told(pro: &mut Pro, reports: Vec<Report>) -> Vec<Effect> {
        let mut effects = Vec::new();
        for report in reports {
            effects.extend(pro.take(pro.session, report));
        }
        effects
    }

    /// Someone signed in who has opened a checkout and waits for a payment.
    fn waiting() -> Pro {
        let mut pro = pro(Some("ana@example.com"));
        assert!(pro.begin_opening());
        let effects = told(&mut pro, vec![Report::Checkout("https://c".into())]);
        assert_eq!(effects, [Effect::OpenCheckout("https://c".into())]);
        assert!(pro.checkout_opened(true).is_some());
        assert_eq!(pro.doing, ProDoing::AwaitingPayment);
        pro
    }

    #[test]
    fn a_failure_is_put_in_words_for_the_page() {
        assert_eq!(
            words(&Failure::SignedOut),
            "This PC was signed out of VinPUT Pro. Sign in again to use it."
        );
        assert!(
            words(&Failure::Unreachable(
                "the server could not be reached".into()
            ))
            .starts_with("VinPUT Pro could not be reached. Check your internet connection.")
        );
        assert!(words(&Failure::Expired).contains("not finished in time"));
        // Calling a wait off is not a failure to tell anyone about.
        assert_eq!(words(&Failure::Stopped), "");
        let refused = Failure::refused(Code::Unavailable, "billing is not configured");
        assert_eq!(
            words(&refused),
            "VinPUT Pro could not do that: billing is not configured."
        );
        // A sentence the server ended itself is not ended twice.
        let ended = Failure::refused(Code::BadRequest, "No such plan. Pick another.");
        assert_eq!(
            words(&ended),
            "VinPUT Pro could not do that: No such plan. Pick another."
        );
        for failure in [Failure::SignedOut, Failure::Expired, refused] {
            assert!(!words(&failure).contains('!'));
        }
    }

    #[test]
    fn a_wait_ends_when_it_is_called_off() {
        let stop = AtomicBool::new(true);
        let started = Instant::now();
        assert!(stopped_within(&stop, Duration::from_secs(30)));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(!stopped_within(
            &AtomicBool::new(false),
            Duration::from_millis(1)
        ));
    }

    #[test]
    fn reports_carry_the_sign_in_they_belong_to() {
        let left = Arc::new(Mutex::new(VecDeque::new()));
        let reporter = Reporter {
            left: Arc::clone(&left),
            session: 7,
            // No window: the message goes to this thread, which reads none.
            window: 0,
        };
        reporter.send(Report::PaymentWaitOver);
        let left = left.lock().unwrap_or_else(PoisonError::into_inner);
        assert!(matches!(left.front(), Some((7, Report::PaymentWaitOver))));
    }

    /// A pace at which a test does not wait.
    const AT_ONCE: Pace = Pace {
        quick: Duration::ZERO,
        quick_for: Duration::from_secs(60),
        slow: Duration::ZERO,
        limit: Duration::from_secs(60),
    };

    /// Runs a wait for a payment over `answers`, one for each time the
    /// account is asked, and returns what it sent, in short.
    fn wait_over(answers: Vec<Result<AccountInfo, Failure>>, pace: Pace) -> Vec<&'static str> {
        let stop = AtomicBool::new(false);
        let mut answers = answers.into_iter();
        let mut sent = Vec::new();
        await_payment(
            &mut || answers.next().unwrap_or(Err(Failure::SignedOut)),
            &stop,
            &mut |report| {
                sent.push(match report {
                    Report::Info(Ok(info)) if info.entitled => {
                        // Whoever takes the reports in calls the wait off.
                        stop.store(true, Ordering::Relaxed);
                        "paid"
                    }
                    Report::Info(Ok(_)) => "not yet",
                    Report::Info(Err(failure)) if failure.is_passing() => "passing",
                    Report::Info(Err(_)) => "failed",
                    Report::PaymentWaitOver => "over",
                    _ => "other",
                });
            },
            pace,
        );
        sent
    }

    #[test]
    fn the_wait_for_a_payment_survives_a_server_that_is_busy_or_out_of_reach() {
        let sent = wait_over(
            vec![
                Ok(info(false)),
                Err(refused(Code::TooManyRequests)),
                Err(refused(Code::Internal)),
                Err(refused(Code::UpstreamFailed)),
                Err(refused(Code::Unavailable)),
                Err(Failure::Unreachable("offline".into())),
                Ok(info(true)),
            ],
            AT_ONCE,
        );
        assert_eq!(
            sent,
            [
                "not yet", "passing", "passing", "passing", "passing", "passing", "paid"
            ]
        );
    }

    #[test]
    fn however_the_wait_for_a_payment_ends_it_says_that_it_is_over() {
        // The session is gone, or the server says something that will not
        // be different the next time.
        for fatal in [Failure::SignedOut, refused(Code::BadRequest)] {
            let sent = wait_over(vec![Ok(info(false)), Err(fatal)], AT_ONCE);
            assert_eq!(sent, ["not yet", "failed", "over"]);
        }
        // Its time is up.
        let brief = Pace {
            quick: Duration::from_millis(5),
            limit: Duration::from_millis(40),
            ..AT_ONCE
        };
        let sent = wait_over(vec![Ok(info(false)); 100], brief);
        assert!(sent.len() > 2, "{sent:?}");
        assert_eq!(sent.last(), Some(&"over"));
        assert!(sent[..sent.len() - 1].iter().all(|said| *said == "not yet"));
        // Called off, it says nothing more at all.
        let stop = AtomicBool::new(true);
        let mut sent = 0;
        await_payment(&mut || Ok(info(false)), &stop, &mut |_| sent += 1, AT_ONCE);
        assert_eq!(sent, 0);
    }

    #[test]
    fn the_wait_for_a_payment_asks_less_often_after_a_while() {
        let pace = Pace {
            quick: Duration::from_millis(2),
            quick_for: Duration::from_millis(40),
            slow: Duration::from_millis(60),
            limit: Duration::from_millis(220),
        };
        let sent = wait_over(vec![Ok(info(false)); 1000], pace);
        // Some twenty looks at the quick pace would fit the time; with the
        // slow pace after the first stretch it is far fewer.
        assert!((3..40).contains(&sent.len()), "{}", sent.len());
        // And at the pace that ships: about a hundred requests in the
        // quarter of an hour, not three hundred.
        let quick = PAYMENT_PACE.quick_for.as_secs() / PAYMENT_PACE.quick.as_secs();
        let slow = PAYMENT_PACE
            .limit
            .saturating_sub(PAYMENT_PACE.quick_for)
            .as_secs()
            / PAYMENT_PACE.slow.as_secs();
        assert!(quick + slow <= 110, "{}", quick + slow);
    }

    #[test]
    fn a_payment_that_arrives_ends_the_wait_and_asks_for_pro_to_be_used() {
        let mut pro = waiting();
        let stop = Arc::clone(&pro.payment);
        // Not yet, and a server that is busy: the page goes on waiting,
        // and says nothing is wrong.
        let effects = told(
            &mut pro,
            vec![
                Report::Info(Ok(info(false))),
                Report::Info(Err(refused(Code::TooManyRequests))),
                Report::Info(Err(Failure::Unreachable("offline".into()))),
            ],
        );
        assert!(effects.is_empty());
        assert_eq!(pro.doing, ProDoing::AwaitingPayment);
        assert_eq!(pro.problem, None);
        assert!(!stop.load(Ordering::Relaxed));
        // Paid.
        let effects = told(&mut pro, vec![Report::Info(Ok(info(true)))]);
        assert_eq!(effects, [Effect::Paid]);
        assert_eq!(pro.doing, ProDoing::Nothing);
        assert!(stop.load(Ordering::Relaxed));
        assert!(pro.facts().info.is_some_and(|info| info.entitled));
    }

    #[test]
    fn a_wait_for_a_payment_that_ends_badly_leaves_the_page_free_again() {
        // The one refusal that used to leave the page waiting for good.
        let mut pro = waiting();
        let effects = told(
            &mut pro,
            vec![
                Report::Info(Err(refused(Code::BadRequest))),
                Report::PaymentWaitOver,
            ],
        );
        assert!(effects.is_empty());
        assert_eq!(pro.doing, ProDoing::Nothing);
        assert!(pro.problem.is_some());
        // The account can be asked again, and a plan chosen again.
        assert!(pro.begin_ask(true));
        told(&mut pro, vec![Report::Info(Ok(info(false)))]);
        assert!(pro.begin_opening());

        // Time up without a payment: no complaint, just the plans again.
        let mut pro = waiting();
        told(&mut pro, vec![Report::PaymentWaitOver]);
        assert_eq!(
            (pro.doing, pro.problem.as_deref()),
            (ProDoing::Nothing, None)
        );

        // Signed out elsewhere while waiting.
        let mut pro = waiting();
        let stop = Arc::clone(&pro.payment);
        let effects = told(&mut pro, vec![Report::Info(Err(Failure::SignedOut))]);
        assert_eq!(effects, [Effect::Reconnect]);
        assert!(stop.load(Ordering::Relaxed));
        assert_eq!(pro.email, None);
        assert_eq!(
            pro.problem.as_deref(),
            Some("This PC was signed out of VinPUT Pro. Sign in again to use it.")
        );
        // What the wait still says, under the sign-in that is over, counts
        // for nothing.
        assert!(
            pro.take(pro.session - 1, Report::PaymentWaitOver)
                .is_empty()
        );
        assert!(
            pro.take(pro.session - 1, Report::Info(Ok(info(true))))
                .is_empty()
        );
        assert_eq!(pro.email, None);
    }

    #[test]
    fn a_checkout_that_does_not_open_is_not_waited_for() {
        let mut pro = pro(Some("ana@example.com"));
        assert!(pro.begin_opening());
        // The same press twice would open two checkouts.
        assert!(!pro.begin_opening());
        assert!(pro.checkout_opened(false).is_none());
        assert_eq!(pro.doing, ProDoing::Nothing);
        // Cancelled while waiting: the wait is called off, the account asked.
        let mut pro = waiting();
        let stop = Arc::clone(&pro.payment);
        assert!(pro.cancel());
        assert!(stop.load(Ordering::Relaxed));
        assert_eq!(pro.doing, ProDoing::Nothing);
        assert!(!pro.cancel());
    }

    #[test]
    fn someone_who_has_a_subscription_is_sent_to_manage_it_and_the_other_way_round() {
        let mut pro = pro(Some("ana@example.com"));
        assert!(pro.begin_opening());
        let effects = told(&mut pro, vec![Report::Portal("https://p".into())]);
        assert_eq!(effects, [Effect::OpenPortal("https://p".into())]);
        assert_eq!(pro.doing, ProDoing::Nothing);
        // Nothing to manage: said, and the account is asked, which shows
        // the plans.
        assert!(pro.begin_opening());
        let nothing = Failure::Refused {
            code: Code::BadRequest,
            reason: Reason::NothingToManage,
            message: "no customer yet".into(),
        };
        let effects = told(&mut pro, vec![Report::NoPage(nothing)]);
        assert_eq!(effects, [Effect::Ask(false)]);
        assert_eq!(pro.doing, ProDoing::Nothing);
        assert!(
            pro.problem
                .as_deref()
                .is_some_and(|said| said.contains("Choose a plan"))
        );
        // A server that takes no payments yet: the wait is over at once,
        // and it is said as the news it is, for a checkout as for the page
        // of a subscription.
        assert!(pro.begin_opening());
        let shortly = "VinPUT Pro checkout opens shortly. Try again later.";
        let closed = Failure::refused(Code::Unavailable, shortly);
        assert!(told(&mut pro, vec![Report::NoPage(closed)]).is_empty());
        assert_eq!(pro.doing, ProDoing::Nothing);
        assert_eq!(pro.problem.as_deref(), Some(wire::PRO_SOON));
        // Any other refusal is said as it is.
        assert!(pro.begin_opening());
        told(
            &mut pro,
            vec![Report::NoPage(refused(Code::UpstreamFailed))],
        );
        assert_eq!(pro.doing, ProDoing::Nothing);
        assert!(
            pro.problem
                .as_deref()
                .is_some_and(|said| said.contains("not now"))
        );
        // Signed out elsewhere, found out on the way to the portal.
        assert!(pro.begin_opening());
        let effects = told(&mut pro, vec![Report::NoPage(Failure::SignedOut)]);
        assert_eq!(effects, [Effect::Reconnect]);
        assert_eq!(pro.email, None);
    }

    #[test]
    fn a_sign_in_shows_its_code_and_ends_signed_in_or_with_the_reason() {
        let mut pro = pro(None);
        let Some(stop) = pro.begin_sign_in() else {
            unreachable!("nobody is signed in");
        };
        // One at a time.
        assert!(pro.begin_sign_in().is_none());
        assert_eq!(pro.facts().signing_in, Some(("", 0)));
        let effects = told(&mut pro, vec![Report::SignInFailed(Failure::Expired)]);
        assert!(effects.is_empty());
        assert!(pro.signing_in.is_none() && pro.problem.is_some());
        assert!(!stop.load(Ordering::Relaxed));

        // Finished in the browser.
        assert!(pro.begin_sign_in().is_some());
        assert_eq!(pro.problem, None);
        let effects = told(&mut pro, vec![Report::SignedIn("ana@example.com".into())]);
        assert_eq!(effects, [Effect::Reconnect, Effect::Ask(true)]);
        assert_eq!(pro.email.as_deref(), Some("ana@example.com"));
        // Signed in: no second sign-in.
        assert!(pro.begin_sign_in().is_none());
    }

    #[test]
    fn a_sign_in_that_is_called_off_is_over_whatever_its_thread_still_says() {
        let mut pro = pro(None);
        let Some(stop) = pro.begin_sign_in() else {
            unreachable!("nobody is signed in");
        };
        let started_under = pro.session;
        assert!(!pro.cancel());
        assert!(stop.load(Ordering::Relaxed));
        assert!(pro.signing_in.is_none());
        // Its thread reports that it stopped: nothing to say.
        assert!(
            pro.take(started_under, Report::SignInFailed(Failure::Stopped))
                .is_empty()
        );
        assert_eq!(pro.problem, None);

        // Called off at the very moment it was finished: the session was
        // kept, and is shown rather than left behind unseen.
        let effects = pro.take(started_under, Report::SignedIn("ana@example.com".into()));
        assert_eq!(effects, [Effect::Reconnect, Effect::Ask(true)]);
        assert_eq!(pro.email.as_deref(), Some("ana@example.com"));

        // Unless another sign-in is under way by then: that one counts.
        let mut pro = self::pro(None);
        assert!(pro.begin_sign_in().is_some());
        let first = pro.session;
        pro.cancel();
        assert!(pro.begin_sign_in().is_some());
        assert!(
            pro.take(first, Report::SignedIn("old@example.com".into()))
                .is_empty()
        );
        assert_eq!(pro.email, None);
        assert!(pro.signing_in.is_some());
    }

    #[test]
    fn signing_out_during_a_request_drops_what_the_request_finds() {
        let mut pro = pro(Some("ana@example.com"));
        assert!(pro.begin_ask(true));
        assert_eq!(pro.doing, ProDoing::Asking);
        // A second ask while one is shown is not started.
        assert!(!pro.begin_ask(false));
        let asked_under = pro.session;
        pro.reset();
        assert_eq!((pro.email.as_deref(), pro.doing), (None, ProDoing::Nothing));
        // The answer arrives after the sign-out: nobody's account.
        assert!(
            pro.take(asked_under, Report::Info(Ok(info(true))))
                .is_empty()
        );
        assert!(pro.facts().info.is_none() && pro.email.is_none());
        // The sign-out itself is done: the provider follows.
        let effects = told(&mut pro, vec![Report::SignedOut]);
        assert_eq!(effects, [Effect::Reconnect]);
        // Nobody signed in: nothing to ask or to open.
        assert!(!pro.begin_ask(true) && !pro.begin_opening());
    }

    #[test]
    fn a_thread_that_cannot_be_started_does_not_leave_the_page_waiting() {
        let mut signing_in = pro(None);
        let Some(stop) = signing_in.begin_sign_in() else {
            unreachable!("nobody is signed in");
        };
        signing_in.not_started();
        assert!(signing_in.signing_in.is_none() && stop.load(Ordering::Relaxed));
        assert!(signing_in.problem.is_some());
        assert!(signing_in.begin_sign_in().is_some());

        let mut asking = pro(Some("ana@example.com"));
        assert!(asking.begin_ask(true));
        asking.not_started();
        assert_eq!(asking.doing, ProDoing::Nothing);
        assert!(asking.begin_ask(true));

        let mut opening = pro(Some("ana@example.com"));
        assert!(opening.begin_opening());
        opening.not_started();
        assert!(opening.begin_opening());
    }

    #[test]
    fn what_is_known_is_kept_and_dated_while_the_server_cannot_be_asked() {
        let mut pro = pro(Some("ana@example.com"));
        assert!(pro.begin_ask(true));
        told(&mut pro, vec![Report::Info(Ok(info(true)))]);
        // Known and current: not dated.
        assert!(pro.facts().info.is_some() && pro.facts().checked.is_none());
        assert!(pro.begin_ask(false));
        told(
            &mut pro,
            vec![Report::Info(Err(Failure::Unreachable("offline".into())))],
        );
        let facts = pro.facts();
        assert!(
            facts
                .problem
                .is_some_and(|said| said.contains("could not be reached"))
        );
        assert!(facts.info.is_some_and(|info| info.entitled));
        assert!(
            facts
                .checked
                .is_some_and(|ago| ago < Duration::from_secs(60))
        );
    }

    #[test]
    fn the_prices_are_the_built_in_ones_until_the_server_says() {
        let mut pro = pro(None);
        assert_eq!(pro.facts().prices, &Prices::default());
        let mut said = Prices::default();
        said.offers[0].monthly = 350;
        // Whoever is signed in by the time they arrive: prices are everyone's.
        assert!(
            pro.take(pro.session + 5, Report::Prices(said.clone()))
                .is_empty()
        );
        assert_eq!(pro.facts().prices, &said);
    }

    #[test]
    fn used_up_hours_hand_dictation_to_this_pc_and_hours_that_are_back_hand_it_back() {
        let mut pro = pro(Some("ana@example.com"));
        assert!(!pro.facts().carrying);
        let session = pro.session;
        // Hours left: VinPUT Pro serves.
        let mut account = info(true);
        account.used_seconds = 100;
        account.resets_at = u64::MAX;
        assert!(
            pro.take(session, Report::Info(Ok(account.clone())))
                .is_empty()
        );
        assert!(!pro.facts().carrying);
        // None left: the model on this PC does, and that is all that
        // changes. Nothing is to be done besides: no other model is chosen,
        // nothing is set up again, the settings are not touched.
        account.used_seconds = account.cap_seconds;
        assert!(
            pro.take(session, Report::Info(Ok(account.clone())))
                .is_empty()
        );
        assert!(pro.facts().carrying);
        assert_eq!(pro.standby.until(), u64::MAX);
        // A tier with more hours, or a new month: Pro again, by itself.
        account.cap_seconds *= 2;
        assert!(
            pro.take(session, Report::Info(Ok(account.clone())))
                .is_empty()
        );
        assert!(!pro.facts().carrying);
        // An account that cannot use Pro has no hours to have run out, and
        // neither has nobody.
        account.used_seconds = account.cap_seconds;
        account.entitled = false;
        pro.take(session, Report::Info(Ok(account.clone())));
        assert!(!pro.facts().carrying);
        account.complimentary = true;
        pro.take(session, Report::Info(Ok(account)));
        assert!(pro.facts().carrying);
        pro.reset();
        assert!(!pro.facts().carrying);
    }

    #[test]
    fn which_model_carries_on_is_looked_up_only_while_one_does() {
        let pro = pro(Some("ana@example.com"));
        let mut settings = Settings {
            provider: ProviderKind::Cloud,
            model: cloud::MODEL_ID.to_owned(),
            ..Settings::default()
        };
        // Hours left: no name, and no look at the disk for one.
        assert_eq!(pro.carrying_with(&settings), None);
        pro.standby.heard(10, 10, u64::MAX);
        // Another model chosen: its hours are nobody's business.
        settings.provider = ProviderKind::Local;
        assert_eq!(pro.carrying_with(&settings), None);
    }

    #[test]
    fn what_is_said_when_the_hours_run_out_names_the_model_and_what_may_be_missing() {
        assert_eq!(
            carrying_on("Whisper base", true),
            "Dictation carries on with Whisper base, on this PC, until the new month starts."
        );
        let partly = carrying_on("Whisper base", false);
        assert!(partly.ends_with("What was said just before the switch may be missing."));
        // Short enough for a notification.
        assert!(partly.chars().count() < 200);
    }
}
