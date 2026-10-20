//! Running the program of an action.

use std::fmt;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use putlab_core::job::Exit;
use putlab_core::process::hide_console;

use super::{Action, ENV_TEXT, Then, output_text};
use crate::inject::{SendInputSink, TextSink};
use crate::memo::clipboard;

/// The most of what a program writes that is kept. The rest is read and
/// let go of, so that a program that writes a great deal is not held up.
const MAX_OUTPUT: u64 = 1 << 20;
/// The most of what it writes as an error that is kept.
const MAX_ERROR: u64 = 16 << 10;
/// How long what a program wrote is waited for once it has ended. A program
/// it started and left running may hold its output open for good.
const AFTER_EXIT: Duration = Duration::from_secs(2);
/// The longest error line that is passed on.
const MAX_SAID: usize = 200;

/// Why an action did not do its work, for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The action is set up in a way that cannot run.
    Refused(String),
    /// Windows could not start the program.
    NotStarted {
        /// The program.
        program: String,
        /// Why, as Windows says it.
        why: String,
    },
    /// It took longer than it may, and was ended.
    TimedOut(u32),
    /// It ended with an exit code other than 0.
    Exited {
        /// The code, if it ended by itself.
        code: Option<i32>,
        /// The last line it wrote as an error, if any.
        said: String,
    },
    /// It worked, and what it wrote could not be typed or copied.
    NotDelivered(String),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(why) => f.write_str(why),
            Self::NotStarted { program, why } => write!(f, "{program} could not be started: {why}"),
            Self::TimedOut(seconds) => {
                write!(f, "It took longer than {seconds} seconds and was stopped.")
            }
            Self::Exited { code, said } => {
                match code {
                    Some(code) => write!(f, "The program ended with exit code {code}.")?,
                    None => f.write_str("The program was ended.")?,
                }
                if !said.is_empty() {
                    write!(f, " It said: {said}")?;
                }
                Ok(())
            }
            Self::NotDelivered(why) => write!(f, "What it wrote could not be passed on: {why}"),
        }
    }
}

/// Runs the program of `action` with `text`, and returns what it wrote to
/// its standard output. Waits as long as the action allows, which may be
/// for good: call it on a thread of its own.
///
/// # Errors
///
/// Returns why it did not work, in words for the user.
pub fn run(action: &Action, text: &str) -> Result<String, Failure> {
    let arguments = action.arguments_for(text).map_err(Failure::Refused)?;
    let mut command = Command::new(action.program.trim());
    command
        .args(arguments)
        .env(ENV_TEXT, text)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(home) = home() {
        command.current_dir(home);
    }
    hide_console(&mut command);
    let mut child = command.spawn().map_err(|err| Failure::NotStarted {
        program: action.program.trim().to_owned(),
        why: if err.kind() == std::io::ErrorKind::NotFound {
            "Windows cannot find it. Give its whole path, or check its name.".to_owned()
        } else {
            err.to_string()
        },
    })?;
    if let Some(mut stdin) = child.stdin.take() {
        let text = text.as_bytes().to_vec();
        // A program that does not read its input ends without it, and
        // the write then fails, which is no fault of anyone's.
        let _ = thread::Builder::new()
            .name("vinput-action-in".into())
            .spawn(move || stdin.write_all(&text));
    }
    let output = read_on_a_thread(child.stdout.take(), MAX_OUTPUT);
    let error = read_on_a_thread(child.stderr.take(), MAX_ERROR);
    let status = match ended(&child, action.timeout) {
        Ok(()) => child.wait(),
        Err(seconds) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Failure::TimedOut(seconds));
        }
    };
    let output = output.recv_timeout(AFTER_EXIT).unwrap_or_default();
    match status {
        Ok(status) if status.success() => Ok(output_text(&output)),
        Ok(status) => Err(Failure::Exited {
            code: status.code(),
            said: last_line(&error.recv_timeout(AFTER_EXIT).unwrap_or_default()),
        }),
        Err(err) => Err(Failure::Exited {
            code: None,
            said: err.to_string(),
        }),
    }
}

/// Runs the program of `action` with `text`, as [`run`] does, and does
/// with what it writes what the action says: types it where the caret is,
/// or puts it on the clipboard.
///
/// # Errors
///
/// Returns why it did not work, in words for the user.
pub fn carry_out(action: &Action, text: &str) -> Result<(), Failure> {
    let output = run(action, text)?;
    if output.is_empty() {
        return Ok(());
    }
    match action.then {
        Then::Nothing => Ok(()),
        Then::Copy => {
            clipboard::put_text(&output).map_err(|err| Failure::NotDelivered(err.to_string()))
        }
        Then::Type => {
            let mut sink = SendInputSink::new();
            // A typed line break is the Enter key, which sends a message in
            // many a program; one that is pasted is a line break everywhere.
            let typed = if output.contains('\n') {
                sink.paste(&output).and_then(|pasted| {
                    if pasted {
                        Ok(())
                    } else {
                        sink.type_lines(&output)
                    }
                })
            } else {
                sink.type_text(&output)
            };
            typed.map_err(|err| Failure::NotDelivered(err.to_string()))
        }
    }
}

/// Waits for `child` to end, for at most `seconds` (0 is for good). The
/// waiting is done by Windows, on a thread of its own, and nothing is
/// looked at again and again. Fails with the seconds that passed.
fn ended(child: &Child, seconds: u32) -> Result<(), u32> {
    let Some(exit) = Exit::of(child) else {
        return Ok(());
    };
    if seconds == 0 {
        exit.wait();
        return Ok(());
    }
    let (sender, ended) = mpsc::channel();
    let waiting = thread::Builder::new()
        .name("vinput-action-exit".into())
        .spawn(move || {
            exit.wait();
            let _ = sender.send(());
        });
    if waiting.is_err() {
        return Ok(());
    }
    ended
        .recv_timeout(Duration::from_secs(u64::from(seconds)))
        .map_err(|_| seconds)
}

/// Reads all of `pipe` on a thread of its own, keeping at most `most`
/// bytes, and hands them over once the pipe is closed.
fn read_on_a_thread(
    pipe: Option<impl Read + Send + 'static>,
    most: u64,
) -> mpsc::Receiver<Vec<u8>> {
    let (sender, read) = mpsc::channel();
    if let Some(mut pipe) = pipe {
        let _ = thread::Builder::new()
            .name("vinput-action-out".into())
            .spawn(move || {
                let mut kept = Vec::new();
                let _ = (&mut pipe).take(most).read_to_end(&mut kept);
                let _ = std::io::copy(&mut pipe, &mut std::io::sink());
                let _ = sender.send(kept);
            });
    }
    read
}

/// The last line with something on it, shortened.
fn last_line(bytes: &[u8]) -> String {
    let text = output_text(bytes);
    let line = text.lines().rev().find(|line| !line.trim().is_empty());
    line.unwrap_or_default()
        .trim()
        .chars()
        .take(MAX_SAID)
        .collect()
}

/// The user's own folder, where a program of theirs starts out.
fn home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .filter(|home| home.is_dir())
}

#[cfg(test)]
mod tests {
    use super::super::Then;
    use super::*;

    fn action(program: &str, arguments: &str, timeout: u32) -> Action {
        Action {
            name: "Test".to_owned(),
            hotkey: None,
            program: program.to_owned(),
            arguments: arguments.to_owned(),
            then: Then::Type,
            cleanup: crate::rewrite::Mode::Plain,
            timeout,
        }
    }

    const POWERSHELL: &str = "powershell.exe";

    #[test]
    fn the_text_reaches_the_program_in_every_way() {
        let text = "Dober dan, \u{10d}e\u{161}nje & \"quotes\" | $(x)";
        // In the variable.
        let read = action(
            POWERSHELL,
            "-NoProfile -Command \"[Console]::OutputEncoding = [Text.Encoding]::UTF8; \
             [Console]::Out.Write($env:VINPUT_TEXT)\"",
            30,
        );
        assert_eq!(run(&read, text), Ok(text.to_owned()));
        // As an argument of its own. PowerShell joins what follows -Command
        // into the command, so the script is a file.
        let script = std::env::temp_dir().join("vinput-action-test.ps1");
        let written = std::fs::write(
            &script,
            "param($said)\n[Console]::OutputEncoding = [Text.Encoding]::UTF8\n\
             [Console]::Out.Write($said)\n",
        );
        assert!(written.is_ok(), "{written:?}");
        let argument = action(
            POWERSHELL,
            &format!(
                "-NoProfile -ExecutionPolicy Bypass -File \"{}\" {{text}}",
                script.display()
            ),
            30,
        );
        assert_eq!(run(&argument, text), Ok(text.to_owned()));
        let _ = std::fs::remove_file(&script);
        // On standard input.
        let input = action(
            POWERSHELL,
            "-NoProfile -Command \"[Console]::In.ReadToEnd().ToUpper()\"",
            30,
        );
        assert_eq!(run(&input, "shout"), Ok("SHOUT".to_owned()));
    }

    #[test]
    fn a_failure_says_what_went_wrong() {
        let missing = action("no-such-program-anywhere.exe", "", 30);
        assert!(matches!(
            run(&missing, "x"),
            Err(Failure::NotStarted { .. })
        ));
        let failing = action(
            POWERSHELL,
            "-NoProfile -Command \"[Console]::Error.WriteLine('bad input'); exit 3\"",
            30,
        );
        assert_eq!(
            run(&failing, "x"),
            Err(Failure::Exited {
                code: Some(3),
                said: "bad input".to_owned()
            })
        );
        let mixed = action(POWERSHELL, "-Command \"echo {text}\"", 30);
        assert!(matches!(run(&mixed, "x"), Err(Failure::Refused(_))));
    }

    #[test]
    fn a_program_that_takes_too_long_is_ended() {
        let slow = action(POWERSHELL, "-NoProfile -Command \"Start-Sleep 20\"", 1);
        let started = std::time::Instant::now();
        assert_eq!(run(&slow, "x"), Err(Failure::TimedOut(1)));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn the_last_line_of_an_error_is_what_is_said() {
        assert_eq!(last_line(b"first\r\nsecond\r\n\r\n"), "second");
        assert_eq!(last_line(b""), "");
        assert_eq!(last_line("x".repeat(500).as_bytes()).len(), MAX_SAID);
    }
}
