//! A recognizer session that may not be there yet.
//!
//! Asking a provider for a session is usually immediate, but not always: a
//! local model that is not loaded takes seconds to load. Whoever feeds the
//! audio must not wait for that, because it is the thread that reads the
//! microphone. So the session is asked for on a thread of its own, and what
//! is said meanwhile is kept here and handed over first, and in order, when
//! the session arrives.

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use super::{Emit, Transcriber, Transcript};
use crate::audio::SAMPLE_RATE_HZ;
use crate::{Error, Result, log};

/// The most audio that is kept for a recognizer that is not there yet: one
/// minute, which is 2 MB. The largest local model loads in a few seconds; a
/// recognizer that takes longer than this has something wrong with it, and
/// the session is not allowed to grow without end while it waits.
pub(crate) const MAX_WAITING_SAMPLES: usize = SAMPLE_RATE_HZ as usize * 60;
/// How much of the kept audio a recognizer is handed at a time, once it is
/// there: 100 ms, the most it is ever handed while listening.
const REPLAY_SAMPLES: usize = SAMPLE_RATE_HZ as usize / 10;

/// A recognizer session of a provider.
pub(crate) type Session = Box<dyn Transcriber + Send>;

/// A session that may not be there yet. Audio fed to it before it arrives is
/// kept, and handed over first and in order when it does.
pub(crate) struct Deferred {
    /// Where the session, or the reason there is none, arrives.
    arriving: Receiver<Result<Session>>,
    session: Option<Session>,
    /// What was heard before the session arrived.
    waiting: Vec<i16>,
    /// The most that `waiting` may hold.
    limit: usize,
    /// Whether audio had to be left out of `waiting`.
    overflowed: bool,
    /// Transcripts the session has produced that have not been passed on:
    /// handing over what was kept can produce several at once.
    ready: VecDeque<Transcript>,
}

impl Deferred {
    /// A session that arrives on `arriving`, with room for `limit` samples
    /// until it does.
    pub(crate) fn awaiting(arriving: Receiver<Result<Session>>, limit: usize) -> Self {
        Self {
            arriving,
            session: None,
            waiting: Vec::new(),
            limit,
            overflowed: false,
            ready: VecDeque::new(),
        }
    }

    /// Has `start` find the session on a thread of its own and returns at
    /// once. Should the thread not start, that is why there is no session.
    pub(crate) fn asking(start: impl FnOnce() -> Result<Session> + Send + 'static) -> Self {
        let (arrived, arriving) = mpsc::channel();
        let failed = arrived.clone();
        let asked = thread::Builder::new()
            .name("vinput-recognizer".into())
            .spawn(move || {
                let _ = arrived.send(start());
            });
        if let Err(err) = asked {
            let why = format!("no thread could be started for the recognizer: {err}");
            let _ = failed.send(Err(Error::Transcription(why)));
        }
        Self::awaiting(arriving, MAX_WAITING_SAMPLES)
    }

    /// The session, if it has arrived; with `wait`, once it has. On arrival
    /// it is handed everything that was kept for it.
    pub(crate) fn session(&mut self, wait: bool) -> Result<Option<&mut Session>> {
        if self.session.is_none() {
            let gone = || Error::Transcription("the speech provider gave no answer".into());
            let arrived = if wait {
                self.arriving.recv().map_err(|_| gone())?
            } else {
                match self.arriving.try_recv() {
                    Ok(arrived) => arrived,
                    Err(TryRecvError::Empty) => return Ok(None),
                    Err(TryRecvError::Disconnected) => return Err(gone()),
                }
            };
            let mut session = arrived?;
            for chunk in std::mem::take(&mut self.waiting).chunks(REPLAY_SAMPLES) {
                self.ready.extend(session.feed(chunk)?);
            }
            self.session = Some(session);
        }
        Ok(self.session.as_mut())
    }

    /// Keeps audio for the session that is still to come, up to the limit.
    fn keep(&mut self, pcm: &[i16]) {
        let room = self.limit.saturating_sub(self.waiting.len());
        self.waiting.extend_from_slice(&pcm[..pcm.len().min(room)]);
        if pcm.len() > room && !self.overflowed {
            self.overflowed = true;
            log::write(
                "the speech provider is taking too long to start; what is said from here on is lost",
            );
        }
    }
}

impl Transcriber for Deferred {
    fn feed(&mut self, pcm: &[i16]) -> Result<Option<Transcript>> {
        match self.session(false)? {
            Some(session) => {
                let transcript = session.feed(pcm)?;
                self.ready.extend(transcript);
            }
            None => self.keep(pcm),
        }
        Ok(self.ready.pop_front())
    }

    /// Waits for the session if it is still not there: the microphone has
    /// stopped by now, and what was said is recognized however late.
    ///
    /// The wait has no limit of its own. It ends when the provider answers,
    /// so it is as bounded as `start_session` of every provider is, and each
    /// of them gives up on a service or an engine that does not come up.
    fn finish(&mut self, emit: &mut Emit<'_>) -> Result<()> {
        self.session(true)?;
        for transcript in std::mem::take(&mut self.ready) {
            emit(transcript)?;
        }
        match self.session.as_mut() {
            Some(session) => session.finish(emit),
            None => Ok(()),
        }
    }

    /// A guess follows the text settled before it, so none is passed on
    /// while settled text is still waiting to be.
    fn guess(&mut self) -> Option<String> {
        let guess = self.session.as_mut()?.guess();
        guess.filter(|_| self.ready.is_empty())
    }
}
