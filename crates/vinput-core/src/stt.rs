//! Speech-to-text stage.
//!
//! A [`SpeechProvider`] is a speech service that has been set up once and stays
//! ready; each dictation asks it for a [`Transcriber`], which recognizes one
//! stream of audio. The rest of the crate only sees these two traits, so adding a
//! service means adding a module here and a [`ProviderKind`] for it.

pub mod check;
mod cloud;
mod gemini;
mod google;
mod live;
mod local;

use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

pub use cloud::{CloudSpeech, Timings, measure};
pub use gemini::{GeminiConfig, GeminiSpeech};
pub use google::{GoogleConfig, GoogleSession, GoogleSpeech};
pub use local::LocalSpeech;

use crate::settings::Settings;
use crate::{Error, Result, log};

/// A piece of recognized speech.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcript {
    /// The recognized text.
    pub text: String,
    /// Whether the recognizer has finished with this utterance. Interim results
    /// may still be revised and are not typed.
    pub is_final: bool,
}

/// A streaming speech recognizer.
pub trait Transcriber {
    /// Feeds a chunk of PCM audio and returns a transcript if one is ready.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transcription`](crate::Error::Transcription) if the service fails.
    fn feed(&mut self, pcm: &[i16]) -> Result<Option<Transcript>>;

    /// Signals the end of audio and hands every transcript that is still to
    /// come to `emit`, each one as soon as it arrives, so the last words are
    /// typed without waiting for the service to wind down.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transcription`](crate::Error::Transcription) if the
    /// service fails, or the first error `emit` returns.
    fn finish(&mut self, emit: &mut Emit<'_>) -> Result<()>;
}

/// Where [`Transcriber::finish`] hands each transcript.
pub type Emit<'a> = dyn FnMut(Transcript) -> Result<()> + 'a;

impl<T: Transcriber + ?Sized> Transcriber for Box<T> {
    fn feed(&mut self, pcm: &[i16]) -> Result<Option<Transcript>> {
        (**self).feed(pcm)
    }

    fn finish(&mut self, emit: &mut Emit<'_>) -> Result<()> {
        (**self).finish(emit)
    }
}

/// What one dictation session asks the provider to recognize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionOptions<'a> {
    /// BCP-47 language codes the speech may be in, such as `en-US`.
    pub languages: &'a [String],
}

/// A speech service that is connected and ready to recognize.
///
/// A provider is created once and shared by every session, so whatever is slow
/// to set up (connections, credentials, a loaded model) belongs in it, and
/// [`start_session`](Self::start_session) should return without waiting.
///
/// It cannot always: a provider that is still being set up when a session
/// starts makes that session wait ([`Lazy`]). A dictation therefore never
/// asks for its session on the thread that reads the microphone
/// (`dictation.rs`); what is said during the wait is kept and recognized.
pub trait SpeechProvider: Send + Sync {
    /// Which kind of provider this is.
    fn kind(&self) -> ProviderKind;

    /// Starts recognizing a new stream of audio.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Transcription`](crate::Error::Transcription) if a session cannot be started.
    fn start_session(&self, options: SessionOptions<'_>) -> Result<Box<dyn Transcriber + Send>>;
}

/// The speech services Vinput can use, by the name written in the settings file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ProviderKind {
    /// Google Cloud Speech-to-Text v2. It is what a settings file that names
    /// no provider has always meant, and nothing more: no model is chosen
    /// with it, so nothing is set up until the user chooses one.
    #[default]
    GoogleCloud,
    /// Google's Gemini API, live transcription.
    Gemini,
    /// A model that runs on this computer.
    Local,
    /// `VinPUT` Pro: Vinput's own server, by subscription.
    Cloud,
}

impl ProviderKind {
    /// Every provider, in the order a settings list should show them.
    pub const ALL: &[Self] = &[Self::Cloud, Self::GoogleCloud, Self::Gemini, Self::Local];

    /// The name used in the settings file.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::GoogleCloud => "google-cloud",
            Self::Gemini => "gemini",
            Self::Local => "local",
            Self::Cloud => crate::cloud::MODEL_ID,
        }
    }
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

impl FromStr for ProviderKind {
    type Err = Error;

    fn from_str(name: &str) -> Result<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|kind| kind.id().eq_ignore_ascii_case(name.trim()))
            .ok_or_else(|| Error::Transcription(format!("unknown speech provider {name:?}")))
    }
}

/// Sets up the provider the settings choose.
///
/// # Errors
///
/// Returns [`Error::Transcription`](crate::Error::Transcription) if the provider is not configured or cannot
/// be reached.
pub fn connect(settings: &Settings) -> Result<Arc<dyn SpeechProvider>> {
    if !settings.has_model() {
        return Err(Error::Transcription(
            "no speech model is chosen; pick one under Settings, Models".into(),
        ));
    }
    match settings.provider {
        ProviderKind::GoogleCloud => {
            Ok(Arc::new(GoogleSpeech::connect(settings.google_config()?)?))
        }
        ProviderKind::Gemini => Ok(Arc::new(GeminiSpeech::connect(
            GeminiConfig::from_settings(settings)?,
        )?)),
        ProviderKind::Local => Ok(Arc::new(LocalSpeech::connect(
            &settings.model,
            settings.use_gpu(),
        )?)),
        ProviderKind::Cloud => Ok(Arc::new(CloudSpeech::connect(crate::cloud::shared())?)),
    }
}

/// How a [`Lazy`] provider reaches its service.
type Connect = Box<dyn Fn() -> Result<Arc<dyn SpeechProvider>> + Send + Sync>;

/// A provider that connects on first use and tries again after a failure.
///
/// This lets the app start while offline or before it is configured: the reason
/// then shows up on the dictation that needed the service, and the next one
/// retries, instead of the app refusing to start.
pub struct Lazy {
    kind: ProviderKind,
    connect: Connect,
    connected: Mutex<Option<Arc<dyn SpeechProvider>>>,
}

impl fmt::Debug for Lazy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Lazy")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl Lazy {
    /// Wraps `connect`, which is not called until the provider is needed.
    pub fn new(
        kind: ProviderKind,
        connect: impl Fn() -> Result<Arc<dyn SpeechProvider>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            connect: Box::new(connect),
            connected: Mutex::default(),
        }
    }

    /// Connects now unless that has already happened, so the first session is fast.
    ///
    /// # Errors
    ///
    /// Returns the error of the failed connection; a later call tries again.
    pub fn warm_up(&self) -> Result<()> {
        self.provider().map(drop)
    }

    fn provider(&self) -> Result<Arc<dyn SpeechProvider>> {
        // Held while connecting, so a session that starts during the warm-up
        // waits for that connection instead of opening a second one: a local
        // model is not loaded twice. The wait can be seconds, which is why
        // the caller must not be the thread that reads the microphone.
        let mut connected = self
            .connected
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(provider) = connected.as_ref() {
            return Ok(Arc::clone(provider));
        }
        let provider = (self.connect)()?;
        *connected = Some(Arc::clone(&provider));
        Ok(provider)
    }
}

impl SpeechProvider for Lazy {
    fn kind(&self) -> ProviderKind {
        self.kind
    }

    fn start_session(&self, options: SessionOptions<'_>) -> Result<Box<dyn Transcriber + Send>> {
        self.provider()?.start_session(options)
    }
}

/// Starts setting up the provider the settings choose and returns at once.
///
/// The connection is made on another thread. If it fails, the reason is logged
/// and the first dictation tries again and reports its own error.
#[must_use]
pub fn connect_in_background(settings: &Settings) -> Arc<dyn SpeechProvider> {
    let chosen = settings.clone();
    let lazy = Arc::new(Lazy::new(settings.provider, move || connect(&chosen)));
    let warming = Arc::clone(&lazy);
    // If the thread cannot be started, the first dictation connects instead.
    let _ = thread::Builder::new()
        .name("vinput-connect".into())
        .spawn(move || {
            if let Err(err) = warming.warm_up() {
                log::write(&format!("speech provider not ready: {err}"));
            }
        });
    lazy
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn provider_names_round_trip() {
        for kind in ProviderKind::ALL {
            assert_eq!(kind.id().parse::<ProviderKind>().ok(), Some(*kind));
        }
        assert_eq!(
            " Google-Cloud ".parse::<ProviderKind>().ok(),
            Some(ProviderKind::GoogleCloud)
        );
        assert!("banana".parse::<ProviderKind>().is_err());
    }

    struct Silent;

    impl Transcriber for Silent {
        fn feed(&mut self, _pcm: &[i16]) -> Result<Option<Transcript>> {
            Ok(None)
        }

        fn finish(&mut self, _emit: &mut Emit<'_>) -> Result<()> {
            Ok(())
        }
    }

    struct Fake;

    impl SpeechProvider for Fake {
        fn kind(&self) -> ProviderKind {
            ProviderKind::GoogleCloud
        }

        fn start_session(&self, _: SessionOptions<'_>) -> Result<Box<dyn Transcriber + Send>> {
            Ok(Box::new(Silent))
        }
    }

    #[test]
    fn a_lazy_provider_retries_until_connected_and_then_connects_no_more() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&attempts);
        let lazy = Lazy::new(ProviderKind::GoogleCloud, move || {
            if counter.fetch_add(1, Ordering::Relaxed) == 0 {
                Err(Error::Transcription("offline".into()))
            } else {
                Ok(Arc::new(Fake) as Arc<dyn SpeechProvider>)
            }
        });
        let options = SessionOptions { languages: &[] };

        assert!(lazy.start_session(options).is_err());
        assert!(lazy.start_session(options).is_ok());
        assert!(lazy.warm_up().is_ok());
        assert!(lazy.start_session(options).is_ok());
        assert_eq!(attempts.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn a_session_started_during_the_warm_up_waits_for_that_one_connection() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let (loaded, loading) = std::sync::mpsc::channel::<()>();
        let loading = Mutex::new(loading);
        let counter = Arc::clone(&attempts);
        let lazy = Arc::new(Lazy::new(ProviderKind::Local, move || {
            counter.fetch_add(1, Ordering::SeqCst);
            // A model that takes its time to load.
            let _ = loading
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .recv();
            Ok(Arc::new(Fake) as Arc<dyn SpeechProvider>)
        }));
        let warming = Arc::clone(&lazy);
        let warm_up = thread::spawn(move || warming.warm_up().is_ok());
        while attempts.load(Ordering::SeqCst) == 0 {
            thread::yield_now();
        }
        let pressed = Arc::clone(&lazy);
        let session = thread::spawn(move || {
            pressed
                .start_session(SessionOptions { languages: &[] })
                .is_ok()
        });
        thread::sleep(std::time::Duration::from_millis(30));
        assert!(!session.is_finished(), "the session waits for the model");

        drop(loaded);
        assert_eq!(warm_up.join().ok(), Some(true));
        assert_eq!(session.join().ok(), Some(true));
        assert_eq!(attempts.load(Ordering::SeqCst), 1, "loaded once");
    }
}
