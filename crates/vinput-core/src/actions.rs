//! Actions of the user's own: a key that dictates and hands the text to a
//! program the user names, such as a `PowerShell` script, and does what they
//! chose with what that program writes back.
//!
//! This is how Vinput is extended without growing: it carries no scripting
//! language of its own, and a script is a program like any other. The text
//! reaches the program in three ways, and none of them puts it on a command
//! line a shell reads:
//!
//! - on its standard input, as UTF-8;
//! - in the environment variable [`ENV_TEXT`];
//! - in place of [`PLACEHOLDER`] in the arguments, where that stands as an
//!   argument of its own, and is then handed over as one argument whatever
//!   it holds. Arguments are split once, when the action is set up, and the
//!   text is never split or quoted by a shell of Vinput's.
//!
//! The program runs without a console window, on a thread of its own, and is
//! ended when it takes longer than the action allows ([`run`]). Nothing a
//! dictation does waits for it.
//!
//! In the settings file an action is one line:
//! `action = name | key | then | cleanup | seconds | program | arguments`.
//! Only the arguments, which come last, may hold a `|`.

use std::fmt;
use std::str::FromStr;

use crate::keys::Hotkey;
use crate::rewrite::Mode;

#[cfg(windows)]
mod run;
#[cfg(windows)]
pub use run::{Failure, carry_out, run};

/// The most actions there can be.
pub const MAX_ACTIONS: usize = 8;

/// What stands for the text in the arguments.
pub const PLACEHOLDER: &str = "{text}";

/// The environment variable the text is handed over in.
pub const ENV_TEXT: &str = "VINPUT_TEXT";

/// The times an action may be given to finish, in seconds; 0 is no limit.
pub const TIMEOUTS: [u32; 5] = [10, 30, 60, 300, 0];

/// The time a new action is given.
pub const DEFAULT_TIMEOUT: u32 = 30;

/// The longest name, program or arguments, in bytes.
const MAX_TEXT: usize = 1024;

/// The action a new one starts as: an example that works on every PC, and
/// shows how a `PowerShell` command reads the text.
pub const EXAMPLE_PROGRAM: &str = "powershell.exe";
/// The arguments of that example: it adds what was said to a file of notes
/// in the user's folder.
pub const EXAMPLE_ARGUMENTS: &str = "-NoProfile -Command \"Add-Content -Path $HOME\\Notes.txt \
                                     -Value $env:VINPUT_TEXT -Encoding utf8\"";

/// What is done with what the program writes to its standard output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Then {
    /// Nothing.
    #[default]
    Nothing,
    /// It is typed where the caret is.
    Type,
    /// It is put on the clipboard.
    Copy,
}

impl Then {
    /// Every choice, in the order Settings offers them.
    pub const ALL: [Self; 3] = [Self::Nothing, Self::Type, Self::Copy];

    /// The word the settings file names it by.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Nothing => "nothing",
            Self::Type => "type",
            Self::Copy => "copy",
        }
    }
}

impl fmt::Display for Then {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

impl FromStr for Then {
    type Err = ();

    fn from_str(name: &str) -> Result<Self, ()> {
        Self::ALL
            .into_iter()
            .find(|then| then.id().eq_ignore_ascii_case(name.trim()))
            .ok_or(())
    }
}

/// One action of the user's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    /// What the user calls it.
    pub name: String,
    /// The key that starts it, if one is set.
    pub hotkey: Option<Hotkey>,
    /// The program to run: a name Windows finds on the path, such as
    /// `powershell.exe`, or a whole path.
    pub program: String,
    /// The arguments, as they would be typed after the program, with
    /// [`PLACEHOLDER`] where the text goes.
    pub arguments: String,
    /// What is done with what the program writes.
    pub then: Then,
    /// What a model makes of the dictation before it is handed over.
    pub cleanup: Mode,
    /// How long the program may take, in seconds; 0 is no limit.
    pub timeout: u32,
}

impl Default for Action {
    fn default() -> Self {
        Self {
            name: "Add to my notes".to_owned(),
            hotkey: None,
            program: EXAMPLE_PROGRAM.to_owned(),
            arguments: EXAMPLE_ARGUMENTS.to_owned(),
            then: Then::Nothing,
            cleanup: Mode::Plain,
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

impl Action {
    /// Reads the value of an `action` line of the settings file. `None` for
    /// one that names no program.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let mut parts = value.splitn(7, '|').map(str::trim);
        let mut next = || parts.next().unwrap_or_default();
        let (name, key, then, cleanup, timeout) = (next(), next(), next(), next(), next());
        let (program, arguments) = (next(), next());
        if program.is_empty() {
            return None;
        }
        Some(Self {
            name: if name.is_empty() {
                program.to_owned()
            } else {
                name.to_owned()
            },
            hotkey: key.parse().ok(),
            program: program.to_owned(),
            arguments: arguments.to_owned(),
            then: then.parse().unwrap_or_default(),
            cleanup: cleanup.parse().unwrap_or_default(),
            timeout: timeout
                .parse()
                .ok()
                .filter(|seconds| TIMEOUTS.contains(seconds))
                .unwrap_or(DEFAULT_TIMEOUT),
        })
    }

    /// The value of its line in the settings file, as [`parse`](Self::parse)
    /// reads it.
    #[must_use]
    pub fn line(&self) -> String {
        format!(
            "{} | {} | {} | {} | {} | {} | {}",
            self.name,
            self.hotkey.map(|key| key.to_string()).unwrap_or_default(),
            self.then,
            self.cleanup,
            self.timeout,
            self.program,
            self.arguments,
        )
    }

    /// The arguments the program is started with when `text` was said.
    ///
    /// # Errors
    ///
    /// Says what is wrong with the arguments, in words for the user.
    pub fn arguments_for(&self, text: &str) -> Result<Vec<String>, String> {
        let split = split(&self.arguments);
        if let Some(mixed) = split
            .iter()
            .find(|argument| argument.contains(PLACEHOLDER) && *argument != PLACEHOLDER)
        {
            return Err(format!(
                "Put {PLACEHOLDER} apart from other text, as an argument of its own. \
                 {mixed} is not."
            ));
        }
        Ok(split
            .into_iter()
            .map(|argument| {
                if argument == PLACEHOLDER {
                    text.to_owned()
                } else {
                    argument
                }
            })
            .collect())
    }
}

/// What is wrong with a name, a program or arguments as the user typed
/// them, in words for the user; `None` if they can be kept. `what` names
/// the field.
#[must_use]
pub fn problem(what: Field, text: &str) -> Option<String> {
    if text.len() > MAX_TEXT || text.chars().any(char::is_control) {
        return Some(format!("That {} cannot be used as it is.", what.noun()));
    }
    match what {
        Field::Name | Field::Program if text.contains('|') => {
            Some(format!("The {} cannot have a | in it.", what.noun()))
        }
        Field::Program if text.trim().is_empty() => {
            Some("An action needs a program to run, such as powershell.exe.".to_owned())
        }
        Field::Arguments => Action {
            arguments: text.to_owned(),
            ..Action::default()
        }
        .arguments_for("")
        .err(),
        _ => None,
    }
}

/// The parts of an action that are typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// What the user calls it.
    Name,
    /// The program.
    Program,
    /// Its arguments.
    Arguments,
}

impl Field {
    fn noun(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Program => "program",
            Self::Arguments => "arguments",
        }
    }
}

/// Splits arguments the way a program on Windows splits its command line:
/// at spaces and tabs outside double quotes. A quote that is preceded by a
/// backslash is a quote; backslashes elsewhere are kept as they are, so a
/// path needs nothing doubled.
#[must_use]
pub fn split(arguments: &str) -> Vec<String> {
    let mut split = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut quoted = false;
    let mut backslashes = 0usize;
    for c in arguments.chars() {
        match c {
            '\\' => {
                backslashes += 1;
                started = true;
                continue;
            }
            '"' => {
                current.extend(std::iter::repeat_n('\\', backslashes / 2));
                if backslashes % 2 == 1 {
                    current.push('"');
                } else {
                    quoted = !quoted;
                }
                started = true;
            }
            ' ' | '\t' if !quoted => {
                current.extend(std::iter::repeat_n('\\', backslashes));
                if started {
                    split.push(std::mem::take(&mut current));
                }
                started = false;
            }
            c => {
                current.extend(std::iter::repeat_n('\\', backslashes));
                current.push(c);
                started = true;
            }
        }
        backslashes = 0;
    }
    current.extend(std::iter::repeat_n('\\', backslashes));
    if started {
        split.push(current);
    }
    split
}

/// What a program wrote, as text: UTF-8, or as near as it gets, without the
/// line break most programs end with.
#[must_use]
pub fn output_text(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    text.trim_end_matches(['\r', '\n']).replace("\r\n", "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_read_back_as_it_was_written() {
        let action = Action {
            name: "Translate".to_owned(),
            hotkey: Some("Ctrl+Alt+T".parse().unwrap_or(Hotkey::plain(0x70))),
            program: r"C:\Tools\translate.exe".to_owned(),
            arguments: "--to en {text} | more".to_owned(),
            then: Then::Type,
            cleanup: Mode::Clean,
            timeout: 60,
        };
        let line = action.line();
        assert_eq!(
            line,
            r"Translate | Ctrl+Alt+T | type | clean | 60 | C:\Tools\translate.exe | --to en {text} | more"
        );
        assert_eq!(Action::parse(&line), Some(action));
        let plain = Action::default();
        assert_eq!(Action::parse(&plain.line()), Some(plain));
    }

    #[test]
    fn what_cannot_be_read_takes_its_default() {
        let read = Action::parse("Notes |  | sing | shout | 7 | notepad.exe");
        assert_eq!(
            read,
            Some(Action {
                name: "Notes".to_owned(),
                hotkey: None,
                program: "notepad.exe".to_owned(),
                arguments: String::new(),
                then: Then::Nothing,
                cleanup: Mode::Plain,
                timeout: DEFAULT_TIMEOUT,
            })
        );
        assert_eq!(
            Action::parse(" | F9 | | | | app.exe").map(|a| a.name),
            Some("app.exe".to_owned())
        );
        assert_eq!(Action::parse("Nothing to run | F9"), None);
        assert_eq!(Action::parse(""), None);
    }

    #[test]
    fn arguments_are_split_as_windows_splits_them() {
        assert_eq!(split(""), Vec::<String>::new());
        assert_eq!(split("  a   b\tc "), ["a", "b", "c"]);
        assert_eq!(
            split(r#"-Command "Add-Content -Path $HOME\Notes.txt" {text}"#),
            ["-Command", r"Add-Content -Path $HOME\Notes.txt", "{text}"]
        );
        assert_eq!(split(r"C:\a\b c:\d\"), [r"C:\a\b", r"c:\d\"]);
        assert_eq!(split(r#"say \"hi\""#), ["say", "\"hi\""]);
        assert_eq!(split(r#"a\\"b c" d"#), [r"a\b c", "d"]);
        assert_eq!(split(r#""" x"#), ["", "x"]);
        assert_eq!(split(r#""open to the end"#), ["open to the end"]);
    }

    #[test]
    fn the_text_is_one_argument_whatever_it_holds() {
        let action = Action {
            arguments: r#"--title "From VinPUT" {text} --quiet"#.to_owned(),
            ..Action::default()
        };
        let text = r#"a "quoted" & piped | text; $(rm -rf) %PATH%"#;
        assert_eq!(
            action.arguments_for(text),
            Ok(vec![
                "--title".to_owned(),
                "From VinPUT".to_owned(),
                text.to_owned(),
                "--quiet".to_owned(),
            ])
        );
        // The placeholder may stand more than once, and need not stand at all.
        let twice = Action {
            arguments: "{text} {text}".to_owned(),
            ..Action::default()
        };
        assert_eq!(twice.arguments_for("x"), Ok(vec!["x".to_owned(); 2]));
        assert_eq!(Action::default().arguments_for("x").map(|a| a.len()), Ok(3));
    }

    #[test]
    fn the_text_is_never_put_inside_another_argument() {
        for arguments in [
            "-Command \"Write-Output {text}\"",
            "--message={text}",
            "x{text}",
        ] {
            let action = Action {
                arguments: arguments.to_owned(),
                ..Action::default()
            };
            assert!(action.arguments_for("rm").is_err(), "{arguments}");
            assert!(
                problem(Field::Arguments, arguments).is_some(),
                "{arguments}"
            );
        }
    }

    #[test]
    fn what_is_typed_is_checked() {
        assert_eq!(problem(Field::Name, "Translate"), None);
        assert!(problem(Field::Name, "a | b").is_some());
        assert!(problem(Field::Name, "two\nlines").is_some());
        assert!(problem(Field::Program, "  ").is_some());
        assert!(problem(Field::Program, "a|b.exe").is_some());
        assert_eq!(problem(Field::Program, r"C:\Tools\x.exe"), None);
        assert_eq!(problem(Field::Arguments, "a | b {text}"), None);
        assert!(problem(Field::Arguments, &"a".repeat(MAX_TEXT + 1)).is_some());
        assert_eq!(problem(Field::Arguments, EXAMPLE_ARGUMENTS), None);
    }

    #[test]
    fn output_is_text_without_the_last_line_break() {
        assert_eq!(output_text(b"hello\r\n"), "hello");
        assert_eq!(output_text(b"a\r\nb\n\n"), "a\nb");
        assert_eq!(output_text("\u{feff}\u{10d}a".as_bytes()), "\u{10d}a");
        assert_eq!(output_text(b"bad \xff byte"), "bad \u{fffd} byte");
        assert_eq!(output_text(b""), "");
    }

    #[test]
    fn the_choices_read_back_as_they_are_written() {
        for then in Then::ALL {
            assert_eq!(then.id().parse(), Ok(then));
        }
        assert_eq!("TYPE".parse(), Ok(Then::Type));
        assert_eq!("".parse::<Then>(), Err(()));
    }
}
