//! Installing, updating and removing Vinput.
//!
//! Vinput is one executable, so its setup program is that same executable
//! under the name `VinputSetup`: started that way, it copies itself to the
//! programs folder of the signed-in user, registers itself with Windows and
//! starts the installed copy. No administrator rights are involved. An update
//! is the same thing done by a newer setup program, without questions.
//!
//! Asked to be quiet (`--install --quiet`, `--uninstall --quiet`), setup is
//! what a package manager runs and waits for: it shows no window whatever
//! happens, says how it went with its exit code and its log alone, and is
//! gone the moment it is done. So it starts nothing that would keep whoever
//! waits for it waiting: a new install is left for the user to start, from
//! the Start menu entry setup makes, and only a Vinput that was running, and
//! had to be stopped to be replaced, is started again, cut loose from the
//! setup program ([`start`]).
//!
//! Settings, keys and downloaded models live outside the programs folder and
//! are never touched by an install or an update.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate as host;
use vinput_core::models::hide_console;
use vinput_core::settings::{data_dir, local_dir};
use vinput_core::{Error, Result, cloud, log, memo, secrets, update};
use vinput_ui::dialog;
use vinput_view::Dialog;

/// What the file name of a setup program starts with.
pub const SETUP_PREFIX: &str = "vinputsetup";
const PROGRAM_FILE: &str = "vinput.exe";
/// How long removing everything waits for the server of `VinPUT` Pro to hear
/// that this PC's session is over.
const SIGN_OUT_WAIT: Duration = Duration::from_secs(3);
/// Name of the entry under "Installed apps" in Windows.
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Vinput";
/// Where Windows keeps what starts at sign-in.
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Programs that may be installed beside Vinput and go when it goes: the file
/// name of each and the name of its entry under [`RUN_KEY`].
const COMPANIONS: [(&str, &str); 1] = [("vinput-bridges.exe", "VinputBridges")];
const SITE: &str = "https://vinput-dasvcr.web.app";

/// Whether the running executable is a setup program, going by its name.
#[must_use]
pub fn is_setup_program() -> bool {
    std::env::current_exe().is_ok_and(|exe| is_setup_name(&exe))
}

fn is_setup_name(exe: &Path) -> bool {
    exe.file_stem().is_some_and(|name| {
        name.to_string_lossy()
            .to_ascii_lowercase()
            .starts_with(SETUP_PREFIX)
    })
}

/// Where Vinput is installed: `%LOCALAPPDATA%\Programs\Vinput\vinput.exe`.
#[must_use]
pub fn installed_exe() -> Option<PathBuf> {
    Some(
        PathBuf::from(std::env::var_os("LOCALAPPDATA")?)
            .join("Programs")
            .join("Vinput")
            .join(PROGRAM_FILE),
    )
}

/// The version that is installed, if one is.
#[must_use]
pub fn installed_version() -> Option<String> {
    installed_exe()
        .filter(|exe| exe.is_file())
        .and_then(|_| registry::read(UNINSTALL_KEY, "DisplayVersion"))
}

/// Removes the program an update put aside because it was still running
/// then (`vinput.old`, see [`install_to`]). The copy that has just started in
/// its place calls this; by then the old one has gone. A separate copy
/// leaves it alone: it belongs to the installed one.
pub fn tidy() {
    if vinput_core::instance::is_separate() {
        return;
    }
    if let Some(aside) = installed_exe().map(|exe| exe.with_extension("old")) {
        let _ = std::fs::remove_file(aside);
    }
}

/// A separate copy, run beside the installed Vinput for a try, never
/// installs, updates or removes anything: all of that belongs to the
/// installed one.
fn refuse_separate() -> Result<()> {
    if vinput_core::instance::is_separate() {
        return Err(Error::Install(format!(
            "a copy started with {} set does not install or remove Vinput",
            vinput_core::instance::ENV
        )));
    }
    Ok(())
}

/// How the setup program was asked to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setup {
    /// By the user: it shows a window and waits for them to press Install.
    Asking,
    /// By a tool that installs without questions (`--install --quiet`): it
    /// goes ahead, shows nothing, not a failure either, and starts Vinput,
    /// in the tray, only if it had to stop it.
    Quiet,
    /// By the running Vinput, for an update the user chose (`--update`): it
    /// goes ahead and starts the new version so that it shows itself.
    Update,
}

impl Setup {
    /// How the installed copy is started afterwards, when nothing was asked.
    fn started_with(self) -> &'static str {
        match self {
            Self::Update => UPDATED_ARGUMENT,
            Self::Asking | Self::Quiet => "--background",
        }
    }
}

/// The argument the setup program starts the new version with after an
/// update. `vinput/src/main.rs` reads it.
pub const UPDATED_ARGUMENT: &str = "--updated";

/// Runs the setup program as `how` says.
///
/// # Errors
///
/// Returns [`Error::Install`] if the program cannot be put in place.
pub fn run_setup(how: Setup) -> Result<()> {
    refuse_separate()?;
    let target = installed_exe().ok_or_else(|| Error::Install("LOCALAPPDATA is not set".into()))?;
    if how != Setup::Asking {
        let was_running = host::is_running();
        install_to(&target)?;
        if how == Setup::Quiet {
            // Whoever asked may never start Vinput, so what it registers
            // when it first runs is registered now.
            host::register_installed(&target);
            if !was_running {
                return Ok(());
            }
        }
        return start(&target, &[how.started_with()]);
    }
    let version = update::CURRENT_VERSION;
    let previous = installed_version();
    let (heading, detail, button) = match &previous {
        Some(previous) if previous == version => (
            format!("Reinstall VinPUT {version}"),
            "This version is already installed.".to_owned(),
            "Reinstall",
        ),
        Some(previous) => (
            format!("Update VinPUT to {version}"),
            format!("Version {previous} is installed."),
            "Update",
        ),
        None => (
            format!("Install VinPUT {version}"),
            "Type with your voice into any text field.".to_owned(),
            "Install",
        ),
    };
    let folder = target.parent().unwrap_or(&target).display().to_string();
    let lines = [
        format!("Installs to {folder}"),
        "For your Windows account only; no administrator rights".to_owned(),
        "Adds VinPUT to the Start menu and starts it with Windows".to_owned(),
        "Keeps your settings, keys and downloaded models".to_owned(),
    ];
    let dialog = Dialog {
        title: "VinPUT setup".to_owned(),
        heading,
        detail,
        lines: lines.into(),
        primary: button.to_owned(),
        secondary: "Cancel".to_owned(),
    };
    let installed = dialog::confirm_and_run(&dialog, "Installing", || install_to(&target))
        .map_err(|error| window(&error))?;
    if installed {
        // Without arguments the installed copy opens its Settings, which is
        // the first thing a new user needs.
        start(&target, &[])?;
    }
    Ok(())
}

/// A window that could not be shown, as the error setup reports.
fn window(error: &vinput_ui::Error) -> Error {
    Error::Host(error.to_string())
}

/// Puts the program `source` under the name `target`, so that at every
/// moment there is a whole program under that name or, on a first install,
/// none: never half of one.
///
/// The new program is copied to `<name>.new` beside the target and written
/// through to the disk before anything is touched. Then the installed one is
/// renamed to `<name>.old`, which Windows allows even for a program that was
/// running a moment ago and is still locked, and the new one is renamed into
/// its place. If that last step fails, the old one is renamed back. The old
/// file is deleted here if it can be, and by [`tidy`] when the new version
/// starts if it cannot.
///
/// # Errors
///
/// Returns what Windows said of the step that failed. The installed program
/// is then what it was.
fn replace_program(source: &Path, target: &Path) -> std::io::Result<()> {
    replace_program_with(source, target, &|from, to| std::fs::rename(from, to))
}

/// [`replace_program`], renaming with `rename`: a test hands in one that
/// fails where it wants to see what happens then.
fn replace_program_with(
    source: &Path,
    target: &Path,
    rename: &dyn Fn(&Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let fresh = target.with_extension("new");
    let aside = target.with_extension("old");
    if let Some(folder) = target.parent() {
        std::fs::create_dir_all(folder)?;
    }
    let copied = std::fs::copy(source, &fresh).and_then(|_| {
        std::fs::OpenOptions::new()
            .write(true)
            .open(&fresh)?
            .sync_all()
    });
    if let Err(err) = copied {
        let _ = std::fs::remove_file(&fresh);
        return Err(err);
    }
    let installed = target.exists();
    if installed {
        let _ = std::fs::remove_file(&aside);
        if let Err(err) = rename(target, &aside) {
            let _ = std::fs::remove_file(&fresh);
            return Err(err);
        }
    }
    if let Err(err) = rename(&fresh, target) {
        if installed {
            let _ = rename(&aside, target);
        }
        let _ = std::fs::remove_file(&fresh);
        return Err(err);
    }
    let _ = std::fs::remove_file(&aside);
    Ok(())
}

/// Puts the running executable in place as the installed program and
/// registers it with Windows.
fn install_to(target: &Path) -> Result<()> {
    let failed = |action: &str, err: &dyn std::fmt::Display| {
        Error::Install(format!("{action} failed: {err}"))
    };
    let source =
        std::env::current_exe().map_err(|err| failed("finding the setup program", &err))?;
    let was_running = host::is_running();
    if !host::stop_running_instance() {
        return Err(Error::Install(
            "Vinput is still running; exit it from its tray icon and try again".into(),
        ));
    }
    if source != target
        && let Err(err) = replace_program(&source, target)
    {
        // The program that was installed is still the one installed. If it
        // was running, it runs again: nobody is left without Vinput because
        // an update did not go through.
        if was_running && target.is_file() {
            let _ = start(target, &["--background"]);
        }
        return Err(failed("putting the program in place", &err));
    }
    register(target);
    Ok(())
}

/// Lists Vinput under "Installed apps", with its uninstaller.
fn register(target: &Path) {
    let exe = target.display().to_string();
    let folder = target.parent().unwrap_or(target).display().to_string();
    let size_kb = std::fs::metadata(target).map_or(0, |meta| meta.len() / 1024);
    for (name, value) in [
        ("DisplayName", "VinPUT"),
        ("DisplayVersion", update::CURRENT_VERSION),
        ("Publisher", "PUTLAB"),
        ("DisplayIcon", &format!("{exe},0")),
        ("InstallLocation", folder.as_str()),
        ("InstallDate", memo::today().as_str()),
        ("UninstallString", &format!("\"{exe}\" --uninstall")),
        (
            "QuietUninstallString",
            &format!("\"{exe}\" --uninstall --quiet"),
        ),
        ("URLInfoAbout", SITE),
        ("HelpLink", &format!("{SITE}/docs")),
    ] {
        registry::write(UNINSTALL_KEY, name, value);
    }
    for (name, value) in [
        ("NoModify", 1),
        ("NoRepair", 1),
        ("EstimatedSize", u32::try_from(size_kb).unwrap_or(u32::MAX)),
    ] {
        registry::write_number(UNINSTALL_KEY, name, value);
    }
}

/// Starts the installed program and lets go of it.
fn start(program: &Path, arguments: &[&str]) -> Result<()> {
    let mut command = Command::new(program);
    command.args(arguments);
    spawn_detached(&mut command)
        .map_err(|err| Error::Install(format!("starting {} failed: {err}", program.display())))
}

/// Starts `command` as a process of its own, with nothing of this one's: no
/// console, no standard handles, not in its process group and, where Windows
/// allows it, not in its job. Whoever waits for the setup program, or for
/// everything it started, is then not kept waiting by a Vinput that runs for
/// as long as the user is signed in.
fn spawn_detached(command: &mut Command) -> std::io::Result<()> {
    use std::process::Stdio;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        /// `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`.
        const APART: u32 = 0x0000_0008 | 0x0000_0200;
        /// `CREATE_BREAKAWAY_FROM_JOB`, which a job may refuse.
        const OUT_OF_THE_JOB: u32 = 0x0100_0000;
        if command
            .creation_flags(APART | OUT_OF_THE_JOB)
            .spawn()
            .is_ok()
        {
            return Ok(());
        }
        command.creation_flags(APART);
    }
    command.spawn().map(drop)
}

/// Removes Vinput. Asked the usual way, the user chooses whether settings,
/// keys and downloaded models go as well; with `quiet` nothing is asked or
/// shown, and they are kept.
///
/// # Errors
///
/// Returns [`Error::Install`] if the running copy does not exit.
pub fn uninstall(quiet: bool) -> Result<()> {
    refuse_separate()?;
    let everything = if quiet {
        false
    } else {
        let question = Dialog {
            title: "VinPUT".to_owned(),
            heading: "Remove VinPUT from this PC?".to_owned(),
            detail: "The program is removed either way. Choose what happens to the rest."
                .to_owned(),
            lines: vec![
                "Keep: your settings, stored keys and downloaded speech models stay for next time"
                    .to_owned(),
                "Remove everything: those are deleted too".to_owned(),
            ],
            primary: "Keep settings and models".to_owned(),
            secondary: "Remove everything".to_owned(),
        };
        // Closing the window backs out of removing anything.
        let Some(keep) = dialog::ask(&question).map_err(|error| window(&error))? else {
            return Ok(());
        };
        !keep
    };
    let folder = installed_exe().and_then(|exe| exe.parent().map(Path::to_path_buf));
    // Programs installed beside Vinput are stopped and forgotten with it.
    for (file, run_entry) in COMPANIONS {
        if let Some(program) = folder.as_ref().map(|folder| folder.join(file))
            && program.is_file()
        {
            let mut stop = Command::new(&program);
            stop.arg("--stop");
            hide_console(&mut stop);
            let _ = stop.status();
        }
        registry::delete_value(RUN_KEY, run_entry);
    }
    if !host::stop_running_instance() {
        return Err(Error::Install(
            "Vinput is still running; exit it from its tray icon and try again".into(),
        ));
    }
    host::unregister();
    registry::delete(UNINSTALL_KEY);
    if everything {
        // The server is told to forget this PC's session with VinPUT Pro
        // before the token that names it is deleted. Removal does not
        // depend on it: after a few seconds it goes on regardless, and a
        // session nobody ended runs out by itself.
        cloud::end_session_before_removal(SIGN_OUT_WAIT);
        for secret in [
            secrets::GEMINI_API_KEY,
            secrets::GOOGLE_CLOUD_KEY,
            secrets::PRO_SESSION,
        ] {
            secrets::delete(secret);
        }
        for dir in [data_dir(), local_dir()].into_iter().flatten() {
            if let Err(err) = std::fs::remove_dir_all(&dir)
                && err.kind() != std::io::ErrorKind::NotFound
            {
                log::write(&format!("removing {}: {err}", dir.display()));
            }
        }
    }
    // A running program cannot delete itself, so a command that outlives this
    // process removes the program folder a moment later. It only ever runs on
    // the folder Vinput is installed to.
    let running_from = std::env::current_exe().ok();
    if let Some(folder) = folder
        && running_from.is_some_and(|exe| exe.parent() == Some(folder.as_path()))
    {
        let mut remove = Command::new("cmd");
        remove.args([
            "/c",
            "ping",
            "-n",
            "3",
            "127.0.0.1",
            ">nul",
            "&",
            "rmdir",
            "/s",
            "/q",
        ]);
        remove.arg(&folder);
        remove.current_dir(std::env::temp_dir());
        // It outlives this process, which nobody should have to wait past.
        if let Err(err) = spawn_detached(&mut remove) {
            log::write(&format!("removing {}: {err}", folder.display()));
        }
    }
    if !quiet {
        dialog::tell("VinPUT has been removed.").map_err(|error| window(&error))?;
    }
    Ok(())
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod registry {
    //! The few registry operations setup needs, all under the current user.

    use std::ptr::null_mut;

    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_DWORD, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegDeleteTreeW,
        RegGetValueW, RegSetKeyValueW,
    };

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub(super) fn write(key: &str, name: &str, value: &str) {
        let (key, name, value) = (wide(key), wide(name), wide(value));
        let bytes = u32::try_from(value.len() * size_of::<u16>()).unwrap_or(0);
        // SAFETY: all strings are NUL-terminated and `bytes` is the exact size
        // of `value` including its terminator.
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                REG_SZ,
                value.as_ptr().cast(),
                bytes,
            );
        }
    }

    pub(super) fn write_number(key: &str, name: &str, value: u32) {
        let (key, name) = (wide(key), wide(name));
        // SAFETY: both strings are NUL-terminated and the data is the four
        // bytes of `value`.
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                REG_DWORD,
                (&raw const value).cast(),
                4,
            );
        }
    }

    pub(super) fn read(key: &str, name: &str) -> Option<String> {
        let (key, name) = (wide(key), wide(name));
        let mut buffer = [0u16; 260];
        let mut bytes = u32::try_from(size_of_val(&buffer)).unwrap_or(0);
        // SAFETY: both strings are NUL-terminated and `bytes` is the size of
        // the buffer the value is written to.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                null_mut(),
                buffer.as_mut_ptr().cast(),
                &raw mut bytes,
            )
        };
        if status != ERROR_SUCCESS {
            return None;
        }
        let length = buffer.iter().position(|unit| *unit == 0)?;
        Some(String::from_utf16_lossy(&buffer[..length]))
    }

    pub(super) fn delete_value(key: &str, name: &str) {
        let (key, name) = (wide(key), wide(name));
        // SAFETY: both strings are NUL-terminated. A value that is not there
        // is not an error worth reporting.
        unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) };
    }

    pub(super) fn delete(key: &str) {
        let key = wide(key);
        // SAFETY: the string is NUL-terminated. Deleting the tree of a key
        // empties it; the empty key is then removed like a value-less one.
        unsafe {
            RegDeleteTreeW(HKEY_CURRENT_USER, key.as_ptr());
            windows_sys::Win32::System::Registry::RegDeleteKeyW(HKEY_CURRENT_USER, key.as_ptr());
        }
    }
}

#[cfg(not(windows))]
mod registry {
    pub(super) fn write(_key: &str, _name: &str, _value: &str) {}
    pub(super) fn write_number(_key: &str, _name: &str, _value: u32) {}
    pub(super) fn read(_key: &str, _name: &str) -> Option<String> {
        None
    }
    pub(super) fn delete_value(_key: &str, _name: &str) {}
    pub(super) fn delete(_key: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_update_starts_the_new_version_so_that_it_shows_itself() {
        assert_eq!(Setup::Update.started_with(), "--updated");
        // A tool that installs without questions gets no window.
        assert_eq!(Setup::Quiet.started_with(), "--background");
    }

    #[cfg(windows)]
    #[test]
    fn a_program_started_by_setup_is_cut_loose_from_it() -> std::io::Result<()> {
        // A child that holds on to what it was given would keep this pipe
        // open for as long as it runs; one cut loose was given nothing.
        let marker = std::env::temp_dir().join(format!("vinput-detached-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let mut command = Command::new("cmd");
        command.args(["/c", "ping -n 2 127.0.0.1 >nul & echo done >"]);
        command.arg(&marker);
        let started = std::time::Instant::now();
        spawn_detached(&mut command)?;
        // Not waited for: back at once, and the program goes on by itself.
        assert!(started.elapsed() < Duration::from_millis(900));
        assert!(!marker.exists());
        for _ in 0..100 {
            if marker.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(marker.exists());
        std::fs::remove_file(marker)
    }

    /// A folder of its own for one test, with a program of `old` in it if
    /// one is given, and a setup program of `new` beside it.
    fn programs(test: &str, old: Option<&str>) -> std::io::Result<(PathBuf, PathBuf)> {
        let folder =
            std::env::temp_dir().join(format!("vinput-replace-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(folder.join("Programs"))?;
        let (source, target) = (folder.join("setup.exe"), folder.join("Programs/vinput.exe"));
        std::fs::write(&source, "new")?;
        if let Some(old) = old {
            std::fs::write(&target, old)?;
        }
        Ok((source, target))
    }

    /// What is in the folder of the program, by name.
    fn left_in(target: &Path) -> Vec<String> {
        let mut names: Vec<String> = target
            .parent()
            .and_then(|folder| std::fs::read_dir(folder).ok())
            .into_iter()
            .flatten()
            .filter_map(|entry| Some(entry.ok()?.file_name().to_string_lossy().into_owned()))
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_program_is_put_in_place_and_nothing_else_is_left_beside_it() -> std::io::Result<()> {
        // A first install, into a folder that is not there yet.
        let (source, target) = programs("first", None)?;
        std::fs::remove_dir_all(target.parent().unwrap_or(&target))?;
        replace_program(&source, &target)?;
        assert_eq!(std::fs::read_to_string(&target)?, "new");
        assert_eq!(left_in(&target), ["vinput.exe"]);

        // An update, with what an earlier one left behind in the way.
        let (source, target) = programs("update", Some("old"))?;
        std::fs::write(target.with_extension("old"), "older")?;
        std::fs::write(target.with_extension("new"), "half a download")?;
        replace_program(&source, &target)?;
        assert_eq!(std::fs::read_to_string(&target)?, "new");
        assert_eq!(left_in(&target), ["vinput.exe"]);
        Ok(())
    }

    #[test]
    fn a_setup_program_that_cannot_be_copied_leaves_the_installed_one_whole() -> std::io::Result<()>
    {
        let (source, target) = programs("unreadable", Some("old"))?;
        std::fs::remove_file(&source)?;
        assert!(replace_program(&source, &target).is_err());
        assert_eq!(std::fs::read_to_string(&target)?, "old");
        assert_eq!(left_in(&target), ["vinput.exe"]);
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn an_installed_program_that_cannot_be_moved_stays_and_the_new_one_is_taken_away()
    -> std::io::Result<()> {
        use std::os::windows::fs::OpenOptionsExt;
        let (source, target) = programs("held", Some("old"))?;
        // Held open by someone who shares it with nobody: it can be neither
        // renamed nor written over.
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&target)?;
        assert!(replace_program(&source, &target).is_err());
        drop(held);
        assert_eq!(std::fs::read_to_string(&target)?, "old");
        assert_eq!(left_in(&target), ["vinput.exe"]);
        Ok(())
    }

    #[test]
    fn when_the_new_program_cannot_take_the_name_the_old_one_has_it_back() -> std::io::Result<()> {
        let (source, target) = programs("back", Some("old"))?;
        // The installed program is moved aside, and then the new one cannot
        // be given its name.
        let renamed = std::cell::Cell::new(0);
        let failing = |from: &Path, to: &Path| {
            renamed.set(renamed.get() + 1);
            if renamed.get() == 2 {
                return Err(std::io::Error::other("the name could not be taken"));
            }
            std::fs::rename(from, to)
        };
        assert!(replace_program_with(&source, &target, &failing).is_err());
        // Moved aside, refused, moved back.
        assert_eq!(renamed.get(), 3);
        assert_eq!(std::fs::read_to_string(&target)?, "old");
        assert_eq!(left_in(&target), ["vinput.exe"]);
        Ok(())
    }

    #[test]
    fn a_setup_program_is_known_by_its_name() {
        for setup in [
            r"C:\Users\me\Downloads\VinputSetup.exe",
            r"C:\Users\me\Downloads\VinputSetup-0.3.0 (1).exe",
            r"C:\x\vinputsetup.EXE",
        ] {
            assert!(is_setup_name(Path::new(setup)), "{setup}");
        }
        for program in [r"C:\Programs\Vinput\vinput.exe", r"C:\x\setup.exe"] {
            assert!(!is_setup_name(Path::new(program)), "{program}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn registry_values_are_written_read_and_deleted() {
        let key = format!(r"Software\VinputTest\{}", std::process::id());
        assert_eq!(registry::read(&key, "Name"), None);
        registry::write(&key, "Name", "Vinput čšž");
        registry::write_number(&key, "Number", 7);
        assert_eq!(registry::read(&key, "Name").as_deref(), Some("Vinput čšž"));
        registry::delete_value(&key, "Name");
        assert_eq!(registry::read(&key, "Name"), None);
        registry::delete(&key);
        assert_eq!(registry::read(&key, "Name"), None);
        registry::delete(r"Software\VinputTest");
    }
}
