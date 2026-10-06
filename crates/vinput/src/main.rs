//! Vinput binary entry point. All logic lives in the library crates.

// Release builds run without a console window; debug builds keep one for diagnostics.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;

use platform::{Launch, Setup};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args[..] {
        [] => platform::start(),
        ["--background"] => platform::run_in_background(Launch::Background),
        // What the setup program starts the new version with after an update
        // (`vinput_host::install::UPDATED_ARGUMENT`).
        ["--updated"] => platform::run_in_background(Launch::Updated),
        ["--install"] => platform::setup(Setup::Asking),
        ["--install", "--quiet"] => platform::setup(Setup::Quiet),
        ["--update"] => platform::setup(Setup::Update),
        ["--uninstall"] => platform::uninstall(false),
        ["--uninstall", "--quiet"] => platform::uninstall(true),
        ["type", text] => platform::type_after_delay(text),
        ["mic"] => platform::check_microphone(),
        ["transcribe", path] => platform::transcribe_file(path),
        ["encode", wav, mp3] => platform::encode_file(wav, mp3),
        ["search", text] => platform::search_address(text),
        ["models"] => platform::list_models(),
        ["install-model", id] => platform::install_model(id),
        ["add-model", link] => platform::add_model(link),
        ["check-update"] => platform::check_update(),
        // Only in a build for development: see `platform::time_pro`.
        #[cfg(debug_assertions)]
        ["pro-timing", wav] => platform::time_pro(wav, "2"),
        #[cfg(debug_assertions)]
        ["pro-timing", wav, rounds] => platform::time_pro(wav, rounds),
        ["ui", ref window @ ..] => ui(window),
        _ => {
            print_usage();
            return ExitCode::FAILURE;
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Every way to start the program, and what each does.
const USAGE: [(&str, &str); 17] = [
    ("", "start Vinput, or show the settings of the running copy"),
    ("--background", "start without showing the settings window"),
    ("--updated", "start and show that an update was installed"),
    ("--install", "install this copy for the current user"),
    ("--uninstall", "remove Vinput"),
    ("type <text>", "type <text> into the focused window"),
    ("mic", "record 3 seconds and report the input level"),
    (
        "transcribe <wav>",
        "recognize a 16 kHz mono WAV and print it",
    ),
    (
        "encode <wav> <mp3>",
        "encode a 16 kHz mono WAV as a voice memo",
    ),
    ("search <text>", "print the web search <text> would open"),
    ("models", "list the speech models and their state"),
    ("install-model <id>", "download and install a local model"),
    (
        "add-model <link>",
        "add a Whisper model from Hugging Face or a file",
    ),
    ("check-update", "look for a newer version"),
    (
        "ui <window>",
        "show settings, models, add-model, google, pro, advanced, about, welcome or update",
    ),
    (
        "ui <window> --mock <scene>",
        "the same over canned data, to look at",
    ),
    (
        "ui <window> --mock <scene> --picture <png>",
        "the same, saved as a picture",
    ),
];

fn print_usage() {
    eprintln!("vinput {}", env!("CARGO_PKG_VERSION"));
    for (index, (arguments, effect)) in USAGE.iter().enumerate() {
        let lead = if index == 0 { "usage:" } else { "" };
        eprintln!("{lead:<6} vinput {arguments:<27} {effect}");
    }
}

#[cfg(windows)]
mod platform {
    use std::path::Path;
    use std::time::{Duration, Instant};

    use vinput_core::audio::{AudioSource, SAMPLE_RATE_HZ, StopHandle, WasapiSource, WavSource};
    use vinput_core::dictation::Dictation;
    use vinput_core::inject::{SendInputSink, TextSink};
    use vinput_core::models::{self, Installer, Status};
    use vinput_core::settings::Settings;
    use vinput_core::stt::SessionOptions;
    use vinput_core::{log, memo, pipeline, search, stt, update};
    use vinput_host::{self as host, install};

    pub(crate) use vinput_host::Launch;
    pub(crate) use vinput_host::install::Setup;

    /// How long `vinput type` waits so the user can focus the target window.
    const TYPE_DELAY: Duration = Duration::from_secs(3);
    /// How much audio `vinput mic` records.
    const MIC_CHECK_SAMPLES: usize = SAMPLE_RATE_HZ as usize * 3;

    /// What a plain launch does: a setup program installs, anything else runs.
    pub(crate) fn start() -> vinput_core::Result<()> {
        if install::is_setup_program() {
            setup(Setup::Asking)
        } else {
            run_in_background(Launch::Settings)
        }
    }

    pub(crate) fn setup(how: Setup) -> vinput_core::Result<()> {
        let result = install::run_setup(how);
        if how == Setup::Quiet {
            noted(result)
        } else {
            report(result)
        }
    }

    pub(crate) fn uninstall(quiet: bool) -> vinput_core::Result<()> {
        let result = install::uninstall(quiet);
        if quiet { noted(result) } else { report(result) }
    }

    /// Release builds have no console, so a failure of what the user
    /// started needs a window.
    fn report(result: vinput_core::Result<()>) -> vinput_core::Result<()> {
        if let Err(err) = &result {
            host::show_error(&err.to_string());
        }
        noted(result)
    }

    /// A failure of what nobody is watching, a quiet setup or a start in
    /// the background, goes to the log and into the exit code, and never
    /// into a window: whoever started it is a program, and waits for it.
    fn noted(result: vinput_core::Result<()>) -> vinput_core::Result<()> {
        if let Err(err) = &result {
            log::write(&format!("vinput stopped: {err}"));
        }
        result
    }

    pub(crate) fn run_in_background(launch: Launch) -> vinput_core::Result<()> {
        // If Vinput is already running, launching it again just shows its settings.
        if host::activate_running_instance(launch.shows_a_window()) {
            return Ok(());
        }
        // A Google Cloud project that older versions took from the
        // environment is in the settings file from here on.
        Settings::adopt_project();
        let settings = Settings::load();
        // Setting up the speech provider starts now so the first press is
        // already fast, but the app does not wait for it: offline or
        // unconfigured, it still starts and says why when a dictation needs
        // the service.
        let mut dictation = Dictation::new(&settings);
        let result = host::run(settings, &mut dictation, launch);
        if launch.shows_a_window() {
            report(result)
        } else {
            noted(result)
        }
    }

    pub(crate) fn type_after_delay(text: &str) -> vinput_core::Result<()> {
        eprintln!(
            "focus the target window; typing in {} seconds",
            TYPE_DELAY.as_secs()
        );
        std::thread::sleep(TYPE_DELAY);
        SendInputSink::new().type_text(text)
    }

    /// Prints each transcript with the time since the start, instead of typing it.
    struct PrintSink(Instant);

    impl TextSink for PrintSink {
        fn type_text(&mut self, text: &str) -> vinput_core::Result<()> {
            eprintln!("[{:>6} ms] {text:?}", self.0.elapsed().as_millis());
            Ok(())
        }
    }

    pub(crate) fn transcribe_file(path: &str) -> vinput_core::Result<()> {
        let started = Instant::now();
        let settings = Settings::load();
        let speech = stt::connect(&settings)?;
        eprintln!(
            "[{:>6} ms] {} {} ready",
            started.elapsed().as_millis(),
            settings.provider,
            settings.model
        );
        let mut audio = WavSource::open(Path::new(path))?;
        let mut session = speech.start_session(SessionOptions {
            languages: &settings.languages,
        })?;
        pipeline::run_session(&mut audio, &mut session, &mut PrintSink(started))?;
        eprintln!("[{:>6} ms] done", started.elapsed().as_millis());
        Ok(())
    }

    /// The environment variable that holds the access token `pro-timing`
    /// dictates with.
    #[cfg(debug_assertions)]
    const ENV_PRO_TOKEN: &str = "VINPUT_PRO_TOKEN";

    /// Dictates a WAV file to `VinPUT` Pro at the pace of speech, a few times
    /// in a row, and prints when what happened: for measuring where the time
    /// of a dictation goes. No microphone, key, window or sound is involved
    /// and nothing is typed anywhere; nothing of the session that is signed
    /// in on this PC is read or changed. The token comes from the
    /// environment, and the server is the one `VINPUT_CLOUD_URL` names, or
    /// the real one.
    #[cfg(debug_assertions)]
    pub(crate) fn time_pro(wav: &str, rounds: &str) -> vinput_core::Result<()> {
        use std::sync::Arc;

        use vinput_core::cloud::{Account, Server};

        let wrong = |what: String| vinput_core::Error::Transcription(what);
        let rounds: usize = rounds
            .parse()
            .ok()
            .filter(|rounds| (1..=20).contains(rounds))
            .ok_or_else(|| wrong(format!("{rounds:?} is not a number of rounds from 1 to 20")))?;
        let token = std::env::var(ENV_PRO_TOKEN)
            .ok()
            .filter(|token| !token.trim().is_empty())
            .ok_or_else(|| {
                wrong(format!(
                    "set {ENV_PRO_TOKEN} to an access token of VinPUT Pro"
                ))
            })?;
        let samples = WavSource::open(Path::new(wav))?.into_samples();
        let seconds = samples.len() / SAMPLE_RATE_HZ as usize;
        let account = Arc::new(Account::with_access_token(Server::from_env(), &token));
        let languages = Settings::load().languages;
        eprintln!("dictating {seconds} s of audio {rounds} times, in {languages:?}");
        let timings = stt::measure(
            account,
            &samples,
            &languages,
            rounds,
            &mut |round, at, text| {
                eprintln!("round {} [{:>6} ms] {text:?}", round + 1, at.as_millis());
            },
        )?;
        for (round, timings) in timings.iter().enumerate() {
            eprintln!("round {}: {timings}", round + 1);
        }
        Ok(())
    }

    pub(crate) fn encode_file(wav: &str, mp3: &str) -> vinput_core::Result<()> {
        let samples = WavSource::open(Path::new(wav))?.into_samples();
        memo::encode_to(&samples, Path::new(mp3))?;
        eprintln!("wrote {mp3}");
        Ok(())
    }

    /// Prints the address a dictated search for `text` opens, without opening it.
    #[expect(clippy::unnecessary_wraps, reason = "every command returns a result")]
    pub(crate) fn search_address(text: &str) -> vinput_core::Result<()> {
        match search::url(&Settings::load().search_url, text) {
            Some(address) => eprintln!("{address}"),
            None => eprintln!("nothing to search for"),
        }
        Ok(())
    }

    fn describe(status: &Status) -> String {
        match status {
            Status::Online => "online".to_owned(),
            Status::Missing { bytes } => format!("not installed ({})", models::human_size(*bytes)),
            Status::Installing { done, total } => format!(
                "installing, {} of {}",
                models::human_size(*done),
                models::human_size(*total)
            ),
            Status::Installed => "installed".to_owned(),
            Status::Failed(reason) => format!("failed: {reason}"),
        }
    }

    #[expect(clippy::unnecessary_wraps, reason = "every command returns a result")]
    pub(crate) fn list_models() -> vinput_core::Result<()> {
        let settings = Settings::load();
        let installer = Installer::new();
        for model in models::all() {
            let chosen = models::find(settings.provider, &settings.model) == Some(model);
            eprintln!(
                "{} {:<28} {:<34} {}",
                if chosen { "*" } else { " " },
                model.id,
                model.name,
                describe(&installer.status(model, settings.use_gpu()))
            );
        }
        Ok(())
    }

    pub(crate) fn install_model(id: &str) -> vinput_core::Result<()> {
        let model = models::by_id(id).ok_or_else(|| {
            vinput_core::Error::Install(format!("{id:?} is not a model; see `vinput models`"))
        })?;
        let gpu = Settings::load().use_gpu();
        let installer = Installer::new();
        installer.install(model, gpu);
        let mut shown = String::new();
        loop {
            let status = installer.status(model, gpu);
            let line = describe(&status);
            if line != shown {
                eprintln!("{line}");
                shown = line;
            }
            match status {
                Status::Installing { .. } => std::thread::sleep(Duration::from_millis(500)),
                Status::Failed(reason) => return Err(vinput_core::Error::Install(reason)),
                _ => return Ok(()),
            }
        }
    }

    /// Adds the Whisper model a Hugging Face link or a path names, and
    /// installs it. A repository with several models lists them instead.
    pub(crate) fn add_model(link: &str) -> vinput_core::Result<()> {
        let choices = models::custom::choices(link)?;
        let [choice] = choices.as_slice() else {
            eprintln!("this repository has several files; add one with its link:");
            for choice in &choices {
                eprintln!(
                    "  {}  ({})",
                    choice.url.replacen("/resolve/", "/blob/", 1),
                    models::human_size(choice.bytes)
                );
            }
            return Ok(());
        };
        let model = models::custom::add(choice)?;
        eprintln!("added {} as {}", model.name, model.id);
        install_model(model.id)
    }

    pub(crate) fn check_update() -> vinput_core::Result<()> {
        match update::check()? {
            Some(release) => eprintln!(
                "version {} is available ({}); you have {}",
                release.version,
                models::human_size(release.bytes),
                update::CURRENT_VERSION
            ),
            None => eprintln!("version {} is the newest", update::CURRENT_VERSION),
        }
        Ok(())
    }

    pub(crate) fn check_microphone() -> vinput_core::Result<()> {
        let mut mic = WasapiSource::open_default(StopHandle::new())?;
        eprintln!("recording 3 seconds from the default microphone; say something");
        let mut buf = [0i16; 1600];
        let (mut total, mut peak) = (0usize, 0u16);
        while total < MIC_CHECK_SAMPLES {
            let read = mic.read(&mut buf)?;
            if read == 0 {
                break;
            }
            total += read;
            peak = buf[..read]
                .iter()
                .map(|s| s.unsigned_abs())
                .fold(peak, u16::max);
        }
        let percent = u32::from(peak) * 100 / u32::from(i16::MAX.unsigned_abs());
        eprintln!("captured {total} samples at {SAMPLE_RATE_HZ} Hz; peak level {percent}%");
        Ok(())
    }
}

#[cfg(not(windows))]
mod platform {
    use vinput_core::{Error, Result};

    fn unsupported() -> Result<()> {
        Err(Error::Host("vinput only supports Windows".into()))
    }

    pub(crate) fn start() -> Result<()> {
        unsupported()
    }

    /// How Vinput would be started, where it runs.
    #[derive(Debug, Clone, Copy)]
    pub(crate) enum Launch {
        Background,
        Updated,
    }

    /// How setup would be run, where it runs.
    #[derive(Debug, Clone, Copy)]
    pub(crate) enum Setup {
        Asking,
        Quiet,
        Update,
    }

    pub(crate) fn run_in_background(_launch: Launch) -> Result<()> {
        unsupported()
    }

    pub(crate) fn setup(_how: Setup) -> Result<()> {
        unsupported()
    }

    pub(crate) fn uninstall(_quiet: bool) -> Result<()> {
        unsupported()
    }

    pub(crate) fn type_after_delay(_text: &str) -> Result<()> {
        unsupported()
    }

    pub(crate) fn check_microphone() -> Result<()> {
        unsupported()
    }

    pub(crate) fn transcribe_file(_path: &str) -> Result<()> {
        unsupported()
    }

    pub(crate) fn encode_file(_wav: &str, _mp3: &str) -> Result<()> {
        unsupported()
    }

    #[cfg(debug_assertions)]
    pub(crate) fn time_pro(_wav: &str, _rounds: &str) -> Result<()> {
        unsupported()
    }

    pub(crate) fn search_address(_text: &str) -> Result<()> {
        unsupported()
    }

    pub(crate) fn list_models() -> Result<()> {
        unsupported()
    }

    pub(crate) fn install_model(_id: &str) -> Result<()> {
        unsupported()
    }

    pub(crate) fn add_model(_link: &str) -> Result<()> {
        unsupported()
    }

    pub(crate) fn check_update() -> Result<()> {
        unsupported()
    }
}

/// Runs a window process: one of the windows, fed by the background process
/// that started it or by canned data.
fn ui(args: &[&str]) -> vinput_core::Result<()> {
    let failed = |error: vinput_ui::Error| vinput_core::Error::Host(error.to_string());
    let options = vinput_ui::Options::parse(args).map_err(failed)?;
    vinput_ui::run(&options).map_err(failed)
}
