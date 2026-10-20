//! The microphone, kept ready for a while after it was used.
//!
//! Opening a microphone is slow: Windows has to find the device, connect a
//! stream to it and set that stream up, which takes a few hundredths of a
//! second, and whatever is said meanwhile is lost. Someone who has just
//! dictated is likely to dictate again, so when a session ends its stream is
//! not closed but stopped and kept for two minutes; a key press within that
//! time only starts it.
//!
//! A stopped stream captures nothing and Windows does not show the microphone
//! as in use for it. It is still a connection to the device, though, and one
//! that is held for good might keep a program that wants the microphone for
//! itself from getting it, or keep the device from saving power. So nothing
//! is opened ahead of the first session, and when the time is up the stream
//! is closed and nothing at all is held: the next press opens the microphone
//! from nothing, as every press once did.
//!
//! While a stream is kept it can become the wrong one: the device is
//! unplugged, another one becomes the default, the PC sleeps. Windows tells
//! the keeper, which then lets go of it. Nothing is kept unless Windows has
//! agreed to tell, and a stream that was started is compared with the device
//! a session should use now, so a change that went unreported costs at most
//! one session on the old microphone. Should a kept stream not start, the
//! microphone is opened afresh, so a session never fails for having been
//! kept ready.
//!
//! The thread that keeps the stream waits for its time to be up, and when
//! nothing is kept it waits for nothing but a message, which costs nothing.
//!
//! What touches Windows is behind [`CaptureBackend`] and [`CaptureStream`],
//! so all of the above is tested with a fake device.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::{AudioSource, SAMPLE_RATE_HZ, StopHandle};
use crate::{Result, log};

/// How long a stream is kept after the session that used it: two minutes,
/// in which the next dictation is likely. After that it is closed.
const ARMED_FOR: Duration = Duration::from_secs(120);

/// A stream from one microphone that is set up and can be started and stopped
/// any number of times.
///
/// It is used by one thread at a time but not always the same one: a session
/// opens, starts, reads and stops it, and the keeper holds it in between.
pub trait CaptureStream: Send {
    /// What Windows knows the device behind the stream by.
    fn device(&self) -> &str;

    /// Samples per second it delivers: [`SAMPLE_RATE_HZ`] unless it was
    /// opened for a recording.
    fn rate(&self) -> u32 {
        SAMPLE_RATE_HZ
    }

    /// Whether the stream may be kept between sessions. A stream that costs
    /// the user something merely by existing may not.
    fn may_stay_armed(&self) -> bool;

    /// Whether the stream is still connected to its device.
    fn alive(&self) -> bool;

    /// Starts capturing, with nothing left over from an earlier start.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Audio`](crate::Error::Audio) if the device does not
    /// start, as when it has gone since the stream was opened.
    fn start(&mut self) -> Result<()>;

    /// Waits for audio and writes what the device has into `buf`. Returns
    /// `Ok(0)` only once `stop` has been triggered.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Audio`](crate::Error::Audio) if the device fails.
    fn read(&mut self, buf: &mut [i16], stop: &StopHandle) -> Result<usize>;

    /// Stops capturing, if it has not stopped yet, and writes into `buf`
    /// what was captured before that and not read. It never waits; `Ok(0)`
    /// means that nothing is left.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Audio`](crate::Error::Audio) if the device fails.
    fn drain(&mut self, buf: &mut [i16]) -> Result<usize>;

    /// Stops capturing and throws away what was captured but not read.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Audio`](crate::Error::Audio) if the stream cannot be
    /// stopped and emptied; it is then not used again.
    fn stop(&mut self) -> Result<()>;
}

/// Where streams come from: Windows, or a fake in tests.
pub trait CaptureBackend: Send + Sync + 'static {
    /// The streams it opens.
    type Stream: CaptureStream + 'static;
    /// Keeps the backend reporting changes for as long as it lives. It is
    /// made and dropped on the keeper's thread.
    type Watch;

    /// Opens a stopped stream on the microphone the settings name by
    /// `microphone`; empty, or one that is not connected, means the default
    /// one.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Audio`](crate::Error::Audio) if there is no
    /// microphone or it cannot be opened.
    fn open(&self, microphone: &str) -> Result<Self::Stream>;

    /// As [`open`](Self::open), for a recording someone will listen to: at
    /// the rate of the microphone rather than the one speech is recognized
    /// at, where the backend can tell what that is.
    ///
    /// # Errors
    ///
    /// As [`open`](Self::open).
    fn open_for_recording(&self, microphone: &str) -> Result<Self::Stream> {
        self.open(microphone)
    }

    /// The device a stream for `microphone` may be kept on right now, or
    /// `None` if there is none, or none that may be kept open.
    fn standby(&self, microphone: &str) -> Option<String>;

    /// Starts reporting to `changed` whatever can make a kept stream the
    /// wrong one.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Audio`](crate::Error::Audio) if changes will not be
    /// reported. Nothing is kept then, and the keeper asks again later.
    fn watch(&self, changed: Changed) -> Result<Self::Watch>;
}

/// How a [`CaptureBackend`] says that the stream being kept may have become
/// the wrong one. Safe to call from any thread, and it never waits.
#[derive(Clone)]
pub struct Changed(Arc<dyn Fn(bool) + Send + Sync>);

impl Changed {
    /// A device came, went, or became the default one. The stream is let go
    /// of if it is on the wrong device or has lost its device.
    pub fn devices(&self) {
        (self.0)(false);
    }

    /// The PC woke from sleep. The stream is let go of whatever it looks
    /// like, because a device can come back different without saying so.
    pub fn resumed(&self) {
        (self.0)(true);
    }
}

impl std::fmt::Debug for Changed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Changed").finish_non_exhaustive()
    }
}

/// What wakes the keeper.
enum Wake {
    /// Something changed, or a stream came back: look again.
    Check,
    /// A kept stream on this device was started for a session.
    Started(String),
    /// Answer once everything asked before this has been done.
    #[cfg(test)]
    Settled(Sender<()>),
    /// The owner is gone.
    Quit,
}

/// A stopped stream that is being kept.
struct Kept<S> {
    stream: S,
    /// The count of changes it was last found right under.
    epoch: u64,
    /// When it is closed if nobody has used it.
    until: Instant,
}

struct State<S> {
    /// The microphone setting sessions open.
    microphone: String,
    /// Counts the changes. A stream is only as good as the count it was
    /// started or checked under: after any change it is not started again
    /// until the keeper has looked at it.
    epoch: u64,
    /// Set when the next look must let go of the stream without asking it.
    distrust: bool,
    armed: Option<Kept<S>>,
    /// Whether the backend reports changes. Nothing is kept while not.
    watching: bool,
    /// Set once the keeper has ended; nothing is kept from then on.
    closed: bool,
}

/// What the owner, the keeper and the sessions share.
struct Shared<S> {
    state: Mutex<State<S>>,
    wake: Sender<Wake>,
    /// How long a stream is kept.
    keep_for: Duration,
}

impl<S: CaptureStream> Shared<S> {
    fn state(&self) -> MutexGuard<'_, State<S>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Has the keeper look again.
    fn check(&self) {
        let _ = self.wake.send(Wake::Check);
    }

    /// Something changed: what is kept now is not to be started as it is.
    fn invalidate(&self, distrust: bool) {
        {
            let mut state = self.state();
            state.epoch += 1;
            state.distrust |= distrust;
        }
        self.check();
    }

    /// Takes a stopped stream back from the session that used it, to keep
    /// it for a while if it is still the right one.
    fn give_back(&self, epoch: u64, stream: S) {
        let mut state = self.state();
        // A stream that is still there from before a change gives way.
        let room = state
            .armed
            .as_ref()
            .is_none_or(|kept| kept.epoch != state.epoch);
        let keep = !state.closed
            && state.watching
            && state.epoch == epoch
            && room
            && stream.may_stay_armed();
        let let_go = if keep {
            state
                .armed
                .replace(Kept {
                    stream,
                    epoch,
                    until: Instant::now() + self.keep_for,
                })
                .map(|kept| kept.stream)
        } else {
            Some(stream)
        };
        // A stream is let go of outside the lock: that is a call to Windows.
        drop(state);
        drop(let_go);
        // The keeper starts counting the time, or tries again to have
        // changes reported.
        self.check();
    }
}

/// A microphone that stays ready for a while after each use. See the
/// [module](self).
pub struct Armed<B: CaptureBackend> {
    backend: Arc<B>,
    shared: Arc<Shared<B::Stream>>,
    keeper: Option<JoinHandle<()>>,
}

impl<B: CaptureBackend> std::fmt::Debug for Armed<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Armed").finish_non_exhaustive()
    }
}

impl<B: CaptureBackend> Armed<B> {
    /// The microphone the settings name by `microphone`. Nothing is opened
    /// until a session asks for it.
    pub fn with(backend: B, microphone: &str) -> Self {
        Self::keeping_for(backend, microphone, ARMED_FOR)
    }

    /// As [`with`](Self::with), keeping a stream for `keep_for` after use.
    fn keeping_for(backend: B, microphone: &str, keep_for: Duration) -> Self {
        let backend = Arc::new(backend);
        let (wake, woken) = mpsc::channel();
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                microphone: microphone.to_owned(),
                epoch: 0,
                distrust: false,
                armed: None,
                watching: false,
                closed: false,
            }),
            wake,
            keep_for,
        });
        let keeper = {
            let backend = Arc::clone(&backend);
            let shared = Arc::clone(&shared);
            thread::Builder::new()
                .name("vinput-microphone".into())
                .spawn(move || keep_armed(backend.as_ref(), &shared, &woken))
        };
        let keeper = keeper
            .map_err(|err| {
                // Without a keeper nothing is kept, and every session opens
                // the microphone itself.
                log::write(&format!("the microphone is not kept ready: {err}"));
                shared.state().closed = true;
            })
            .ok();
        Self {
            backend,
            shared,
            keeper,
        }
    }

    /// Changes which microphone sessions open. A stream that is kept for the
    /// one before is let go of.
    pub fn set_microphone(&self, microphone: &str) {
        {
            let mut state = self.shared.state();
            if state.microphone == microphone {
                return;
            }
            microphone.clone_into(&mut state.microphone);
            state.epoch += 1;
        }
        self.shared.check();
    }

    /// Starts capturing. Triggering `stop` from any thread stops the
    /// microphone; the source then hands on what it had already captured
    /// and ends.
    ///
    /// This starts the stream that was kept, if one is. If there is none, or
    /// it does not start, the microphone is opened afresh, so a stream that
    /// went stale costs time and nothing else.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Audio`](crate::Error::Audio) if there is no
    /// microphone, access to it is denied, or the device cannot be started.
    pub fn open(&self, stop: StopHandle) -> Result<ArmedSource<B::Stream>> {
        let (microphone, epoch, armed) = {
            let mut state = self.shared.state();
            let armed = match state.armed.take() {
                Some(kept) if kept.epoch == state.epoch && Instant::now() < kept.until => {
                    Some(kept.stream)
                }
                // Something changed, or its time is up, and the keeper has
                // not got to it yet.
                unchecked => {
                    state.armed = unchecked;
                    None
                }
            };
            (state.microphone.clone(), state.epoch, armed)
        };
        let home = Some((Arc::clone(&self.shared), epoch));
        if let Some(mut stream) = armed {
            match stream.start() {
                Ok(()) => {
                    // Whether it is still the microphone a session should
                    // use takes Windows a moment to say, so the keeper asks
                    // while this session is already listening.
                    let _ = self
                        .shared
                        .wake
                        .send(Wake::Started(stream.device().to_owned()));
                    return Ok(ArmedSource::new(stream, stop, home));
                }
                Err(err) => log::write(&format!(
                    "the microphone that was kept ready did not start; opening it again: {err}"
                )),
            }
        }
        let mut stream = self.backend.open(&microphone)?;
        stream.start()?;
        Ok(ArmedSource::new(stream, stop, home))
    }

    /// Starts capturing for a recording someone will listen to, such as a
    /// voice memo: on a stream opened for it at the rate of the microphone
    /// ([`CaptureBackend::open_for_recording`]), which is closed when the
    /// recording ends. A stream kept for dictation is left as it is, since
    /// it runs at the rate of speech recognition; a recording does not need
    /// to start in a hurry.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Audio`](crate::Error::Audio) if there is no
    /// microphone, access to it is denied, or the device cannot be started.
    pub fn record(&self, stop: StopHandle) -> Result<ArmedSource<B::Stream>> {
        let microphone = self.shared.state().microphone.clone();
        let mut stream = self.backend.open_for_recording(&microphone)?;
        stream.start()?;
        Ok(ArmedSource::unarmed(stream, stop))
    }

    /// Waits until the keeper has done everything asked of it so far.
    #[cfg(test)]
    pub(crate) fn settle(&self) {
        let (done, wait) = mpsc::channel();
        if self.shared.wake.send(Wake::Settled(done)).is_ok() && self.keeper.is_some() {
            let _ = wait.recv();
        }
    }
}

impl<B: CaptureBackend> Drop for Armed<B> {
    fn drop(&mut self) {
        let _ = self.shared.wake.send(Wake::Quit);
        if let Some(keeper) = self.keeper.take() {
            let _ = keeper.join();
        }
    }
}

/// The keeper's thread. While a stream is kept it waits until that stream's
/// time is up; while none is, it waits without end for a message, so that an
/// idle Vinput is not woken by it at all.
fn keep_armed<B: CaptureBackend>(
    backend: &B,
    shared: &Arc<Shared<B::Stream>>,
    woken: &Receiver<Wake>,
) {
    let changed = {
        let shared = Arc::clone(shared);
        Changed(Arc::new(move |distrust| shared.invalidate(distrust)))
    };
    let mut watch = None;
    let mut last_error = None;
    #[cfg(test)]
    let mut settled: Vec<Sender<()>> = Vec::new();
    'awake: loop {
        if watch.is_none() {
            // Asked again on every wake until it works: at sign-in Vinput
            // can be up before the audio service is.
            match backend.watch(changed.clone()) {
                Ok(watching) => {
                    watch = Some(watching);
                    last_error = None;
                    shared.state().watching = true;
                }
                Err(err) => {
                    // Said once, not on every wake while it lasts.
                    let err = err.to_string();
                    if last_error.as_ref() != Some(&err) {
                        log::write(&format!("the microphone is not kept ready: {err}"));
                        last_error = Some(err);
                    }
                }
            }
        }
        let until = look(backend, shared);
        #[cfg(test)]
        for done in settled.drain(..) {
            let _ = done.send(());
        }
        let mut wake = match until {
            None => woken.recv().unwrap_or(Wake::Quit),
            Some(until) => {
                match woken.recv_timeout(until.saturating_duration_since(Instant::now())) {
                    Ok(wake) => wake,
                    Err(RecvTimeoutError::Timeout) => Wake::Check,
                    Err(RecvTimeoutError::Disconnected) => Wake::Quit,
                }
            }
        };
        // Windows reports one change as a burst; one look answers all of it.
        loop {
            match wake {
                Wake::Check => {}
                Wake::Started(device) => verify(backend, shared, &device),
                #[cfg(test)]
                Wake::Settled(done) => settled.push(done),
                Wake::Quit => break 'awake,
            }
            match woken.try_recv() {
                Ok(next) => wake = next,
                Err(_) => break,
            }
        }
    }
    drop(watch);
    let left = {
        let mut state = shared.state();
        state.closed = true;
        state.armed.take()
    };
    drop(left);
}

/// Looks at the stream that is kept and lets go of it unless its time is not
/// up, it is on the device a session would use now, and it still reaches
/// that device. Returns when its time is up, if it is still kept.
fn look<B: CaptureBackend>(backend: &B, shared: &Shared<B::Stream>) -> Option<Instant> {
    let (microphone, epoch, distrust, kept) = {
        let mut state = shared.state();
        (
            state.microphone.clone(),
            state.epoch,
            std::mem::take(&mut state.distrust),
            state.armed.take(),
        )
    };
    let mut kept = kept?;
    let right = !distrust
        && Instant::now() < kept.until
        && backend.standby(&microphone).as_deref() == Some(kept.stream.device())
        && kept.stream.alive();
    if !right {
        return None;
    }
    let until = kept.until;
    let mut state = shared.state();
    if state.epoch == epoch && state.armed.is_none() && !state.closed {
        kept.epoch = epoch;
        state.armed = Some(kept);
        Some(until)
    } else {
        // Either a session has handed its stream back meanwhile, or
        // something changed again and the next look is already asked for.
        let other = state.armed.as_ref().map(|kept| kept.until);
        drop(state);
        drop(kept);
        other
    }
}

/// A kept stream on `device` was started for a session. If that is not the
/// device a session should use now, a change went unreported: the stream is
/// not kept after this session, and the next one opens the right device.
fn verify<B: CaptureBackend>(backend: &B, shared: &Shared<B::Stream>, device: &str) {
    let microphone = shared.state().microphone.clone();
    if backend.standby(&microphone).as_deref() != Some(device) {
        shared.state().epoch += 1;
    }
}

/// A microphone that is capturing, as an [`AudioSource`].
///
/// When it is stopped the microphone stops at once; the source hands on what
/// had been captured by then and ends. The stream then goes back to the
/// [`Armed`] microphone it came from, if any, to be kept for a while.
pub struct ArmedSource<S: CaptureStream> {
    /// `None` once the source has ended.
    stream: Option<S>,
    stop: StopHandle,
    /// Where the stream goes back to, and the count it was started under.
    home: Option<(Arc<Shared<S>>, u64)>,
}

impl<S: CaptureStream> std::fmt::Debug for ArmedSource<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArmedSource")
            .field("capturing", &self.stream.is_some())
            .finish_non_exhaustive()
    }
}

impl<S: CaptureStream> ArmedSource<S> {
    fn new(stream: S, stop: StopHandle, home: Option<(Arc<Shared<S>>, u64)>) -> Self {
        Self {
            stream: Some(stream),
            stop,
            home,
        }
    }

    /// A source over a started stream that nothing keeps afterwards.
    pub(crate) fn unarmed(stream: S, stop: StopHandle) -> Self {
        Self::new(stream, stop, None)
    }

    /// Samples per second it delivers.
    #[must_use]
    pub fn rate(&self) -> u32 {
        self.stream
            .as_ref()
            .map_or(SAMPLE_RATE_HZ, CaptureStream::rate)
    }

    /// The device being captured from, while capture runs.
    #[cfg(test)]
    pub(crate) fn device(&self) -> Option<&str> {
        self.stream.as_ref().map(CaptureStream::device)
    }

    /// Stops and empties the stream and hands it back.
    fn release(&mut self) {
        let Some(mut stream) = self.stream.take() else {
            return;
        };
        let stopped = stream.stop();
        let Some((shared, epoch)) = &self.home else {
            return;
        };
        match stopped {
            Ok(()) => shared.give_back(*epoch, stream),
            Err(err) => log::write(&format!("the microphone is not kept ready: {err}")),
        }
    }
}

impl<S: CaptureStream> AudioSource for ArmedSource<S> {
    fn read(&mut self, buf: &mut [i16]) -> Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let Some(stream) = self.stream.as_mut() else {
            return Ok(0);
        };
        let read = if self.stop.is_stopped() {
            Ok(0)
        } else {
            stream.read(buf, &self.stop)
        };
        // Stopped: the microphone captures no more, but a reader that was
        // behind (typing takes time) has not had everything yet, and what is
        // left is the end of what was said.
        let read = match read {
            Ok(0) => stream.drain(buf),
            read => read,
        };
        match read {
            // The stream is let go of the moment capture ends, not when the
            // session does: its last words can take a while yet.
            Ok(0) => {
                self.release();
                Ok(0)
            }
            Ok(read) => Ok(read),
            Err(err) => {
                // A stream that failed is not kept.
                self.stream = None;
                Err(err)
            }
        }
    }
}

impl<S: CaptureStream> Drop for ArmedSource<S> {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;
    use crate::Error;

    /// What the fake device is doing, seen from the test.
    #[derive(Default)]
    struct World {
        /// The default microphone; empty when there is none.
        default: Mutex<String>,
        /// Microphones that can be chosen by name.
        connected: Mutex<Vec<String>>,
        /// Devices no stream may be kept on.
        never_kept: Mutex<Vec<String>>,
        /// Everything done to any stream, in order.
        log: Mutex<Vec<String>>,
        opened: AtomicUsize,
        closed: AtomicUsize,
        /// Streams that are capturing now.
        running: AtomicUsize,
        /// Makes streams opened so far refuse to start.
        stale_before: AtomicUsize,
        /// Makes streams opened so far report that their device is gone.
        dead_before: AtomicUsize,
        fail_open: AtomicBool,
        fail_read: AtomicBool,
        fail_stop: AtomicBool,
        /// Makes Windows refuse to report changes.
        fail_watch: AtomicBool,
        changed: Mutex<Option<Changed>>,
    }

    impl World {
        fn new(default: &str) -> Arc<Self> {
            let world = Arc::new(Self::default());
            *lock(&world.default) = default.to_owned();
            world
        }

        fn note(&self, what: String) {
            lock(&self.log).push(what);
        }

        fn log(&self) -> Vec<String> {
            lock(&self.log).clone()
        }

        fn opened(&self) -> usize {
            self.opened.load(Ordering::SeqCst)
        }

        fn closed(&self) -> usize {
            self.closed.load(Ordering::SeqCst)
        }

        /// The device a stream for `microphone` is opened on.
        fn device(&self, microphone: &str) -> Option<String> {
            if lock(&self.connected).iter().any(|name| name == microphone) {
                return Some(microphone.to_owned());
            }
            Some(lock(&self.default).clone()).filter(|name| !name.is_empty())
        }

        /// Windows reports that a device came, went or became the default.
        fn report_devices(&self) {
            if let Some(changed) = lock(&self.changed).clone() {
                changed.devices();
            }
        }

        /// Windows reports that the PC woke up.
        fn report_resume(&self) {
            if let Some(changed) = lock(&self.changed).clone() {
                changed.resumed();
            }
        }
    }

    fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(PoisonError::into_inner)
    }

    struct FakeBackend(Arc<World>);

    struct FakeStream {
        world: Arc<World>,
        number: usize,
        device: String,
        running: bool,
        /// Audio captured and not read yet: sample `n` is the `n`th captured
        /// since the stream was opened.
        buffered: VecDeque<i16>,
        captured: i16,
        rate: u32,
    }

    impl FakeStream {
        fn halt(&mut self) {
            if self.running {
                self.world.running.fetch_sub(1, Ordering::SeqCst);
                self.running = false;
            }
        }

        fn take(&mut self, buf: &mut [i16]) -> usize {
            let count = buf.len().min(self.buffered.len());
            for (slot, sample) in buf.iter_mut().zip(self.buffered.drain(..count)) {
                *slot = sample;
            }
            count
        }
    }

    impl CaptureBackend for FakeBackend {
        type Stream = FakeStream;
        type Watch = ();

        fn open(&self, microphone: &str) -> Result<FakeStream> {
            if self.0.fail_open.load(Ordering::SeqCst) {
                return Err(Error::Audio("access is denied".into()));
            }
            let device = self
                .0
                .device(microphone)
                .ok_or_else(|| Error::Audio("there is no microphone".into()))?;
            let number = self.0.opened.fetch_add(1, Ordering::SeqCst);
            self.0.note(format!("open {device}"));
            Ok(FakeStream {
                world: Arc::clone(&self.0),
                number,
                device,
                running: false,
                buffered: VecDeque::new(),
                captured: 0,
                rate: SAMPLE_RATE_HZ,
            })
        }

        fn open_for_recording(&self, microphone: &str) -> Result<FakeStream> {
            let mut stream = self.open(microphone)?;
            self.0.note(format!("at 48 kHz {}", stream.device));
            stream.rate = 48_000;
            Ok(stream)
        }

        fn standby(&self, microphone: &str) -> Option<String> {
            self.0
                .device(microphone)
                .filter(|device| !lock(&self.0.never_kept).contains(device))
        }

        fn watch(&self, changed: Changed) -> Result<()> {
            if self.0.fail_watch.load(Ordering::SeqCst) {
                return Err(Error::Audio("the audio service is not running".into()));
            }
            *lock(&self.0.changed) = Some(changed);
            Ok(())
        }
    }

    impl CaptureStream for FakeStream {
        fn device(&self) -> &str {
            &self.device
        }

        fn rate(&self) -> u32 {
            self.rate
        }

        fn may_stay_armed(&self) -> bool {
            !lock(&self.world.never_kept).contains(&self.device)
        }

        fn alive(&self) -> bool {
            self.number >= self.world.dead_before.load(Ordering::SeqCst)
        }

        fn start(&mut self) -> Result<()> {
            if self.number < self.world.stale_before.load(Ordering::SeqCst) {
                return Err(Error::Audio("the device was removed".into()));
            }
            self.world.note(format!("start {}", self.device));
            self.world.running.fetch_add(1, Ordering::SeqCst);
            self.running = true;
            Ok(())
        }

        fn read(&mut self, buf: &mut [i16], stop: &StopHandle) -> Result<usize> {
            if stop.is_stopped() {
                return Ok(0);
            }
            if self.world.fail_read.load(Ordering::SeqCst) {
                return Err(Error::Audio(
                    "the microphone stopped delivering audio".into(),
                ));
            }
            // The device captures four samples at a time, wanted or not.
            for _ in 0..4 {
                self.buffered.push_back(self.captured);
                self.captured = self.captured.wrapping_add(1);
            }
            Ok(self.take(buf))
        }

        fn drain(&mut self, buf: &mut [i16]) -> Result<usize> {
            self.halt();
            Ok(self.take(buf))
        }

        fn stop(&mut self) -> Result<()> {
            self.halt();
            if self.world.fail_stop.load(Ordering::SeqCst) {
                return Err(Error::Audio("the stream did not stop".into()));
            }
            self.world.note(format!("stop {}", self.device));
            self.buffered.clear();
            Ok(())
        }
    }

    impl Drop for FakeStream {
        fn drop(&mut self) {
            self.halt();
            self.world.closed.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn armed(world: &Arc<World>, microphone: &str) -> Armed<FakeBackend> {
        let armed = Armed::with(FakeBackend(Arc::clone(world)), microphone);
        armed.settle();
        armed
    }

    /// Reads `reads` times, two samples a time, and returns what came.
    fn listen(source: &mut ArmedSource<FakeStream>, reads: usize) -> Result<Vec<i16>> {
        let mut heard = Vec::new();
        let mut buf = [0i16; 2];
        for _ in 0..reads {
            let read = source.read(&mut buf)?;
            heard.extend_from_slice(&buf[..read]);
        }
        Ok(heard)
    }

    /// Runs a whole session: a little is heard, then it ends. Returns the
    /// device it was on.
    fn dictate(armed: &Armed<FakeBackend>) -> Result<String> {
        let mut source = armed.open(StopHandle::new())?;
        let device = source.device().unwrap_or_default().to_owned();
        listen(&mut source, 1)?;
        drop(source);
        armed.settle();
        Ok(device)
    }

    #[test]
    fn nothing_is_opened_ahead_of_the_first_session_and_its_stream_serves_the_next() -> Result<()> {
        let world = World::new("desk");
        let armed = armed(&world, "");
        assert_eq!(world.opened(), 0);

        let stop = StopHandle::new();
        let mut source = armed.open(stop.clone())?;
        assert_eq!(listen(&mut source, 1)?, [0, 1]);
        stop.stop();
        while !listen(&mut source, 1)?.is_empty() {}
        assert_eq!(world.running.load(Ordering::SeqCst), 0);
        assert_eq!(world.log(), ["open desk", "start desk", "stop desk"]);
        assert_eq!(world.closed(), 0, "kept, and not capturing");

        // The same stream serves the next session.
        drop(source);
        armed.settle();
        assert_eq!(dictate(&armed)?, "desk");
        assert_eq!(world.opened(), 1);
        assert_eq!(
            world.log(),
            [
                "open desk",
                "start desk",
                "stop desk",
                "start desk",
                "stop desk"
            ]
        );
        Ok(())
    }

    #[test]
    fn a_recording_has_a_stream_of_its_own_and_leaves_the_kept_one_alone() -> Result<()> {
        let world = World::new("desk");
        let armed = armed(&world, "");
        assert_eq!(dictate(&armed)?, "desk");
        assert_eq!(world.opened(), 1);

        let stop = StopHandle::new();
        let mut recording = armed.record(stop.clone())?;
        assert_eq!(recording.rate(), 48_000);
        assert_eq!(listen(&mut recording, 1)?, [0, 1]);
        stop.stop();
        while !listen(&mut recording, 1)?.is_empty() {}
        drop(recording);
        armed.settle();
        // Opened for the recording and closed after it; the stream kept for
        // dictation is the one the next dictation starts.
        assert_eq!(world.opened(), 2);
        assert_eq!(world.closed(), 1);
        assert_eq!(dictate(&armed)?, "desk");
        assert_eq!(world.opened(), 2);
        assert_eq!(
            world.log()[3..],
            [
                "open desk",
                "at 48 kHz desk",
                "start desk",
                "stop desk",
                "start desk",
                "stop desk"
            ]
        );
        let source = armed.open(StopHandle::new())?;
        assert_eq!(source.rate(), SAMPLE_RATE_HZ);
        Ok(())
    }

    #[test]
    fn a_stream_nobody_uses_is_closed_when_its_time_is_up() -> Result<()> {
        let world = World::new("desk");
        let armed = Armed::keeping_for(
            FakeBackend(Arc::clone(&world)),
            "",
            Duration::from_millis(40),
        );
        dictate(&armed)?;
        let waited = Instant::now();
        while world.closed() == 0 && waited.elapsed() < Duration::from_secs(10) {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(world.closed(), 1, "nothing is held any more");

        // The next session opens the microphone from nothing.
        assert_eq!(dictate(&armed)?, "desk");
        assert_eq!(world.opened(), 2);
        Ok(())
    }

    #[test]
    fn a_stream_past_its_time_is_not_started_even_if_still_there() -> Result<()> {
        let world = World::new("desk");
        let armed = Armed::keeping_for(FakeBackend(Arc::clone(&world)), "", Duration::ZERO);
        dictate(&armed)?;
        dictate(&armed)?;
        assert_eq!((world.opened(), world.closed()), (2, 2));
        Ok(())
    }

    #[test]
    fn what_was_captured_before_the_stop_is_still_handed_on() -> Result<()> {
        let world = World::new("desk");
        let armed = armed(&world, "");
        let stop = StopHandle::new();
        let mut source = armed.open(stop.clone())?;
        // Four samples are captured per read and two taken: the reader is
        // behind by two, then four.
        assert_eq!(listen(&mut source, 2)?, [0, 1, 2, 3]);
        stop.stop();
        assert_eq!(listen(&mut source, 1)?, [4, 5]);
        assert_eq!(
            world.running.load(Ordering::SeqCst),
            0,
            "capture stopped at once"
        );
        assert_eq!(listen(&mut source, 1)?, [6, 7]);
        assert!(listen(&mut source, 2)?.is_empty());
        Ok(())
    }

    #[test]
    fn nothing_of_one_session_is_heard_by_the_next() -> Result<()> {
        let world = World::new("desk");
        let armed = armed(&world, "");
        let mut first = armed.open(StopHandle::new())?;
        // Audio is left in the stream when the session is dropped.
        assert_eq!(listen(&mut first, 3)?, [0, 1, 2, 3, 4, 5]);
        drop(first);
        armed.settle();

        let mut second = armed.open(StopHandle::new())?;
        assert_eq!(world.opened(), 1);
        // Twelve had been captured; the six unread ones are gone.
        assert_eq!(listen(&mut second, 1)?, [12, 13]);
        Ok(())
    }

    #[test]
    fn stopping_lets_go_of_the_microphone_before_the_session_ends() -> Result<()> {
        let world = World::new("desk");
        let armed = armed(&world, "");
        let stop = StopHandle::new();
        let mut finishing = armed.open(stop.clone())?;
        stop.stop();
        assert!(listen(&mut finishing, 1)?.is_empty());
        armed.settle();

        // The first session is still alive, typing its last words; the next
        // one gets the stream that was kept all the same.
        let mut next = armed.open(StopHandle::new())?;
        assert_eq!(world.opened(), 1);
        listen(&mut next, 1)?;
        assert!(listen(&mut finishing, 1)?.is_empty());
        drop(finishing);
        assert_eq!(world.running.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[test]
    fn a_stream_that_went_stale_never_makes_a_session_fail() -> Result<()> {
        let world = World::new("desk");
        let armed = armed(&world, "");
        dictate(&armed)?;
        // The device went and came back, and nobody said so.
        world.stale_before.store(1, Ordering::SeqCst);

        let mut source = armed.open(StopHandle::new())?;
        assert_eq!(listen(&mut source, 1)?, [0, 1]);
        assert_eq!(world.opened(), 2);
        assert_eq!(world.closed(), 1, "the stale stream is let go of");

        // The stream opened afresh is the one kept from now on.
        drop(source);
        armed.settle();
        dictate(&armed)?;
        assert_eq!(world.opened(), 2);
        Ok(())
    }

    #[test]
    fn an_error_is_reported_only_when_opening_afresh_fails_too() -> Result<()> {
        let world = World::new("desk");
        let armed = armed(&world, "");
        dictate(&armed)?;
        world.stale_before.store(1, Ordering::SeqCst);
        world.fail_open.store(true, Ordering::SeqCst);
        assert!(matches!(
            armed.open(StopHandle::new()),
            Err(Error::Audio(_))
        ));

        // The next session works again once the microphone does.
        world.fail_open.store(false, Ordering::SeqCst);
        assert!(armed.open(StopHandle::new()).is_ok());
        Ok(())
    }

    #[test]
    fn choosing_another_microphone_lets_go_of_the_one_that_was_kept() -> Result<()> {
        let world = World::new("desk");
        lock(&world.connected).push("headset".into());
        let armed = armed(&world, "");
        dictate(&armed)?;
        armed.set_microphone("headset");
        // Whether or not the keeper has got to it yet, a session never
        // records from the microphone that was chosen before.
        assert_eq!(dictate(&armed)?, "headset");
        assert_eq!((world.opened(), world.closed()), (2, 1));
        assert_eq!(dictate(&armed)?, "headset");
        assert_eq!(world.opened(), 2);

        // Setting the same one again changes nothing.
        armed.set_microphone("headset");
        armed.settle();
        assert_eq!((world.opened(), world.closed()), (2, 1));
        Ok(())
    }

    #[test]
    fn a_new_default_microphone_is_used_when_windows_reports_it() -> Result<()> {
        let world = World::new("desk");
        let armed = armed(&world, "");
        dictate(&armed)?;

        // A change that leaves the microphone as it was costs nothing.
        world.report_devices();
        armed.settle();
        assert_eq!(dictate(&armed)?, "desk");
        assert_eq!((world.opened(), world.closed()), (1, 0));

        *lock(&world.default) = "webcam".into();
        world.report_devices();
        assert_eq!(dictate(&armed)?, "webcam");
        assert_eq!((world.opened(), world.closed()), (2, 1));
        Ok(())
    }

    #[test]
    fn a_change_windows_never_reported_costs_one_session_at_most() -> Result<()> {
        let world = World::new("desk");
        let armed = armed(&world, "");
        dictate(&armed)?;
        armed.settle();

        // Another microphone becomes the default and nothing is reported.
        *lock(&world.default) = "webcam".into();
        // The stream that was kept may still serve the session that follows,
        dictate(&armed)?;
        // but is found out while it does, and not kept again.
        assert_eq!(dictate(&armed)?, "webcam");
        assert_eq!(dictate(&armed)?, "webcam");
        assert_eq!((world.opened(), world.closed()), (2, 1));
        Ok(())
    }

    #[test]
    fn nothing_is_kept_while_windows_does_not_report_changes() -> Result<()> {
        let world = World::new("desk");
        world.fail_watch.store(true, Ordering::SeqCst);
        let armed = armed(&world, "");
        dictate(&armed)?;
        dictate(&armed)?;
        assert_eq!(
            (world.opened(), world.closed()),
            (2, 2),
            "opened for each session, as it used to be"
        );

        // The audio service is up at last. The keeper asks again when the
        // next session ends, and what ends after that is kept.
        world.fail_watch.store(false, Ordering::SeqCst);
        dictate(&armed)?;
        dictate(&armed)?;
        assert_eq!((world.opened(), world.closed()), (4, 3));
        dictate(&armed)?;
        assert_eq!(world.opened(), 4);
        Ok(())
    }

    #[test]
    fn a_chosen_microphone_is_followed_as_it_goes_and_comes_back() -> Result<()> {
        let world = World::new("desk");
        lock(&world.connected).push("headset".into());
        let armed = armed(&world, "headset");
        assert_eq!(dictate(&armed)?, "headset");

        lock(&world.connected).clear();
        world.dead_before.store(1, Ordering::SeqCst);
        world.report_devices();
        assert_eq!(dictate(&armed)?, "desk");

        lock(&world.connected).push("headset".into());
        world.report_devices();
        assert_eq!(dictate(&armed)?, "headset");
        assert_eq!((world.opened(), world.closed()), (3, 2));
        Ok(())
    }

    #[test]
    fn a_stream_that_lost_its_device_is_let_go_of() -> Result<()> {
        let world = World::new("desk");
        let armed = armed(&world, "");
        dictate(&armed)?;
        // Same device, but the stream no longer reaches it.
        world.dead_before.store(1, Ordering::SeqCst);
        world.report_devices();
        armed.settle();
        assert_eq!((world.opened(), world.closed()), (1, 1));
        Ok(())
    }

    #[test]
    fn waking_from_sleep_lets_go_of_the_stream_whatever_it_looks_like() -> Result<()> {
        let world = World::new("desk");
        let armed = armed(&world, "");
        dictate(&armed)?;
        world.report_resume();
        armed.settle();
        assert_eq!((world.opened(), world.closed()), (1, 1));
        Ok(())
    }

    #[test]
    fn a_change_during_a_session_is_not_undone_by_its_end() -> Result<()> {
        let world = World::new("desk");
        let armed = armed(&world, "");
        let source = armed.open(StopHandle::new())?;
        *lock(&world.default) = "webcam".into();
        world.report_devices();
        armed.settle();
        // The session ends on the old microphone, which is not kept.
        drop(source);
        armed.settle();
        assert_eq!(world.closed(), 1);
        assert_eq!(dictate(&armed)?, "webcam");
        Ok(())
    }

    #[test]
    fn a_microphone_that_may_not_be_kept_is_opened_for_each_session() -> Result<()> {
        let world = World::new("earbuds");
        lock(&world.never_kept).push("earbuds".into());
        let armed = armed(&world, "");
        for session in 1..=2 {
            dictate(&armed)?;
            assert_eq!((world.opened(), world.closed()), (session, session));
        }
        Ok(())
    }

    #[test]
    fn a_stream_that_fails_is_not_kept() -> Result<()> {
        let world = World::new("desk");
        let armed = armed(&world, "");
        let mut source = armed.open(StopHandle::new())?;
        world.fail_read.store(true, Ordering::SeqCst);
        assert!(matches!(listen(&mut source, 1), Err(Error::Audio(_))));
        assert_eq!(world.closed(), 1);
        assert!(
            listen(&mut source, 1)?.is_empty(),
            "and the source has ended"
        );

        world.fail_read.store(false, Ordering::SeqCst);
        assert_eq!(dictate(&armed)?, "desk");
        assert_eq!(world.opened(), 2);
        Ok(())
    }

    #[test]
    fn a_stream_that_does_not_stop_cleanly_is_not_kept() -> Result<()> {
        let world = World::new("desk");
        let armed = armed(&world, "");
        let source = armed.open(StopHandle::new())?;
        world.fail_stop.store(true, Ordering::SeqCst);
        drop(source);
        assert_eq!(world.closed(), 1);
        assert_eq!(world.running.load(Ordering::SeqCst), 0);
        Ok(())
    }

    #[test]
    fn two_sessions_at_once_leave_one_stream_behind() -> Result<()> {
        let world = World::new("desk");
        let armed = armed(&world, "");
        let first = armed.open(StopHandle::new())?;
        let second = armed.open(StopHandle::new())?;
        assert_eq!(world.opened(), 2);
        drop(first);
        drop(second);
        armed.settle();
        assert_eq!(world.closed(), 1);
        Ok(())
    }

    #[test]
    fn nothing_is_kept_once_the_owner_is_gone() -> Result<()> {
        let world = World::new("desk");
        let armed = armed(&world, "");
        dictate(&armed)?;
        let source = armed.open(StopHandle::new())?;
        drop(armed);
        drop(source);
        assert_eq!((world.opened(), world.closed()), (1, 1));
        Ok(())
    }

    #[test]
    fn a_stop_from_another_thread_ends_the_session() -> Result<()> {
        let world = World::new("desk");
        let armed = armed(&world, "");
        let stop = StopHandle::new();
        let mut source = armed.open(stop.clone())?;
        let stopper = thread::spawn(move || stop.stop());
        let mut buf = [0i16; 2];
        while source.read(&mut buf)? > 0 {}
        assert!(stopper.join().is_ok());
        assert_eq!(world.running.load(Ordering::SeqCst), 0);
        Ok(())
    }

    #[test]
    fn a_source_nothing_keeps_closes_its_stream() -> Result<()> {
        let world = World::new("desk");
        let mut stream = FakeBackend(Arc::clone(&world)).open("")?;
        stream.start()?;
        let mut source = ArmedSource::unarmed(stream, StopHandle::new());
        assert_eq!(listen(&mut source, 1)?, [0, 1]);
        drop(source);
        assert_eq!(world.closed(), 1);
        Ok(())
    }
}
