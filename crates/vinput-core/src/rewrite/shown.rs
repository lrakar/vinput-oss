//! Putting what a model writes on the screen, while it is still writing.
//!
//! The model's words arrive a few at a time. Up to the first line break
//! they are typed as they arrive, so the wait is for the first of them.
//! From the first line break on they are held and pasted in one piece when
//! the model is done: a line break that is typed is the Enter key, which
//! sends the message in a chat and runs the line in a terminal, and one
//! that is pasted is a line break everywhere. Most of what is tidied is one
//! paragraph and never gets that far.
//!
//! White space around the whole is dropped, since the text goes where a
//! dictation would go, and that has none either.
//!
//! What a selection is replaced with is not shown this way but put in
//! place whole ([`put_over_selection`]): a model that fails halfway must
//! not leave half of the user's own text behind.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{Job, Rewriter};
use crate::inject::TextSink;
use crate::inject::live::erasable;
use crate::session::Told;
use crate::words::{Rules, Shaper};
use crate::{Error, Result};

/// What a model writes, on its way to the screen.
pub(crate) struct Shown<'a, S: TextSink> {
    sink: &'a mut S,
    /// Whether anything but white space has arrived.
    started: bool,
    /// White space that has arrived and is passed on once something
    /// follows it; at the end it is dropped.
    space: String,
    /// What was typed.
    typed: String,
    /// What has arrived from the first line break on.
    held: Option<String>,
}

impl<'a, S: TextSink> Shown<'a, S> {
    pub(crate) fn new(sink: &'a mut S) -> Self {
        Self {
            sink,
            started: false,
            space: String::new(),
            typed: String::new(),
            held: None,
        }
    }

    /// Whether nothing was written at all.
    pub(crate) fn is_empty(&self) -> bool {
        !self.started
    }

    /// Takes the next words of the model.
    pub(crate) fn push(&mut self, piece: &str) -> Result<()> {
        let mut now = String::new();
        for c in piece.chars() {
            if c == '\r' || (!self.started && c.is_whitespace()) {
                continue;
            }
            if c.is_whitespace() {
                self.space.push(c);
                continue;
            }
            self.started = true;
            let space = std::mem::take(&mut self.space);
            match &mut self.held {
                Some(held) => {
                    held.push_str(&space);
                    held.push(c);
                }
                None if space.contains('\n') => {
                    let mut held = space;
                    held.push(c);
                    self.held = Some(held);
                }
                None => {
                    now.push_str(&space);
                    now.push(c);
                }
            }
        }
        if !now.is_empty() {
            self.sink.type_text(&now)?;
            self.typed.push_str(&now);
        }
        Ok(())
    }

    /// The model is done: what was held is put in place.
    pub(crate) fn finish(&mut self) -> Result<()> {
        match self.held.take() {
            Some(held) => put(self.sink, &held),
            None => Ok(()),
        }
    }

    /// Takes back what was typed and types `instead` in its place. Says
    /// whether it did: only where every character typed is one that one
    /// Backspace deletes, and the caret is still where the typing left it.
    /// Nothing that was held is put in place.
    pub(crate) fn take_back_for(&mut self, instead: &str) -> Result<bool> {
        let able =
            self.sink.rewrites() && self.typed.chars().all(erasable) && !self.sink.disturbed();
        if !able {
            return Ok(false);
        }
        self.held = None;
        let erase = self.typed.chars().count();
        self.typed.clear();
        self.sink.rewrite(erase, instead)?;
        Ok(true)
    }
}

/// Puts `text` at the caret in one piece: pasted, or where it cannot be
/// pasted, typed with line breaks that send nothing.
fn put(sink: &mut impl TextSink, text: &str) -> Result<()> {
    if sink.paste(text)? {
        Ok(())
    } else {
        sink.type_lines(text)
    }
}

/// Puts `text` in place of what is selected. Pasted first, even where it
/// is one line: pasting into something that takes no text does nothing,
/// while typing there presses keys the program may take for commands.
pub(crate) fn put_over_selection(sink: &mut impl TextSink, text: &str) -> Result<()> {
    put(sink, text.trim())
}

/// What the user is told when a dictation was typed as it was said.
const AS_SAID: &str = "Typed as you said it";

/// Tidies a dictation with `rewriter` and types the result into `sink` as
/// it is written. `job` holds what was said, and `said` the same as a
/// plain dictation would have typed it, which is what is typed if the
/// model cannot be had or fails: nothing that was said is lost to it.
/// Returns what there is to tell the user, if anything.
///
/// With `snippets`, a snippet the model wrote out is typed as what it
/// stands for, as it is in what is typed instead.
///
/// # Errors
///
/// Returns [`Error::Injection`] if the text cannot be typed. A model that
/// fails is no error of the dictation.
pub(crate) fn type_tidied(
    rewriter: &dyn Rewriter,
    job: &Job<'_>,
    said: &str,
    snippets: Option<&Arc<Rules>>,
    sink: &mut impl TextSink,
    stop: &AtomicBool,
) -> Result<Option<Told>> {
    let said = &match snippets {
        Some(snippets) => {
            let mut shaper = Shaper::new(Arc::clone(snippets));
            shaper.push(said) + &shaper.finish()
        }
        None => said.to_owned(),
    };
    let mut shaper = snippets.map(|snippets| Shaper::new(Arc::clone(snippets)));
    if job.too_long() {
        sink.type_text(said)?;
        return Ok(Some(Told {
            title: AS_SAID,
            message: "That was too long to tidy in one go.".to_owned(),
            setup: false,
        }));
    }
    let mut shown = Shown::new(sink);
    let mut typing = Ok(());
    let written = rewriter.rewrite(job, stop, &mut |piece| {
        if typing.is_ok() {
            typing = match &mut shaper {
                Some(shaper) => shown.push(&shaper.push(piece)),
                None => shown.push(piece),
            };
        }
    });
    typing?;
    // Called off: nothing more is typed, not even what was said.
    if stop.load(Ordering::Acquire) {
        return Ok(None);
    }
    // What a snippet may still have been the start of is the model's text
    // as it stands.
    if let Some(shaper) = &mut shaper {
        shown.push(&shaper.finish())?;
    }
    let (why, setup) = match written {
        Ok(()) if !shown.is_empty() => {
            shown.finish()?;
            return Ok(None);
        }
        Ok(()) => ("The model wrote nothing.".to_owned(), false),
        Err(Error::Setup(why)) => (why, true),
        Err(err) => (err.to_string(), false),
    };
    if shown.is_empty() {
        shown.sink.type_text(said)?;
    } else if !shown.take_back_for(said)? {
        // Part of the tidied text is on the screen and cannot be taken
        // back. The rest of what the model wrote is put after it, and the
        // user is told that the end is missing.
        shown.finish()?;
        return Ok(Some(Told {
            title: "Cleanup stopped partway",
            message: format!("The end of what you said is missing. {why}"),
            setup,
        }));
    }
    Ok(Some(Told {
        title: AS_SAID,
        message: why,
        setup,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rewrite::Kind;
    use crate::rewrite::testing::{Script, Scripted};

    /// What a made-up window was sent.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Did {
        Typed(String),
        Pasted(String),
        TypedLines(String),
        Erased(usize, String),
    }

    /// A window that notes what it is sent.
    #[derive(Default)]
    struct Window {
        did: Vec<Did>,
        /// Whether text can be pasted here.
        pastes: bool,
        /// Whether what was typed can be taken back.
        takes_back: bool,
        /// Whether the caret has been moved.
        moved: bool,
    }

    impl Window {
        fn pasting() -> Self {
            Self {
                pastes: true,
                takes_back: true,
                ..Self::default()
            }
        }

        /// Everything typed, in order, as one text.
        fn typed(&self) -> String {
            let typed = self.did.iter().filter_map(|did| match did {
                Did::Typed(text) => Some(text.as_str()),
                _ => None,
            });
            typed.collect()
        }
    }

    impl TextSink for Window {
        fn type_text(&mut self, text: &str) -> Result<()> {
            self.did.push(Did::Typed(text.to_owned()));
            Ok(())
        }

        fn rewrites(&self) -> bool {
            self.takes_back
        }

        fn rewrite(&mut self, erase: usize, text: &str) -> Result<()> {
            self.did.push(Did::Erased(erase, text.to_owned()));
            Ok(())
        }

        fn disturbed(&mut self) -> bool {
            self.moved
        }

        fn paste(&mut self, text: &str) -> Result<bool> {
            if self.pastes {
                self.did.push(Did::Pasted(text.to_owned()));
            }
            Ok(self.pastes)
        }

        fn type_lines(&mut self, text: &str) -> Result<()> {
            self.did.push(Did::TypedLines(text.to_owned()));
            Ok(())
        }
    }

    fn show(window: &mut Window, pieces: &[&str]) -> Result<()> {
        let mut shown = Shown::new(window);
        for piece in pieces {
            shown.push(piece)?;
        }
        shown.finish()
    }

    fn typed(text: &str) -> Did {
        Did::Typed(text.to_owned())
    }

    #[test]
    fn one_paragraph_is_typed_as_it_arrives_without_the_space_around_it() -> Result<()> {
        let mut window = Window::pasting();
        show(&mut window, &["\n  So we ", "meet", " at six. ", "\n"])?;
        // A space waits for the word after it: one at the very end is
        // never typed.
        assert_eq!(
            window.did,
            [typed("So we"), typed(" meet"), typed(" at six.")]
        );
        Ok(())
    }

    #[test]
    fn from_the_first_line_break_on_the_text_is_pasted_in_one_piece() -> Result<()> {
        let mut window = Window::pasting();
        let pieces = [
            "Hi Ana,",
            "\n\nthe report ",
            "is done.\r\n",
            "\nBest,\nLovro\n",
        ];
        show(&mut window, &pieces)?;
        assert_eq!(
            window.did,
            [
                typed("Hi Ana,"),
                Did::Pasted("\n\nthe report is done.\n\nBest,\nLovro".to_owned())
            ]
        );
        // Where nothing can be pasted, the lines are typed with line
        // breaks that send nothing.
        let mut window = Window::default();
        show(&mut window, &["One.\n", "Two."])?;
        assert_eq!(
            window.did,
            [typed("One."), Did::TypedLines("\nTwo.".to_owned())]
        );
        Ok(())
    }

    #[test]
    fn a_line_break_that_nothing_follows_is_never_put_anywhere() -> Result<()> {
        let mut window = Window::pasting();
        show(&mut window, &["All of it.", "\n", "\n"])?;
        assert_eq!(window.did, [typed("All of it.")]);
        let mut nothing = Window::pasting();
        show(&mut nothing, &[" ", "\n"])?;
        assert!(nothing.did.is_empty());
        Ok(())
    }

    fn tidy(script: Script, window: &mut Window, stop: bool) -> Result<(Option<Told>, Scripted)> {
        let model = Scripted::new(script);
        let job = Job::tidy(Kind::Clean, "um so we meet at six", &[]);
        let stop = AtomicBool::new(stop);
        let told = type_tidied(&model, &job, "um so we meet at six ", None, window, &stop)?;
        Ok((told, model))
    }

    #[test]
    fn a_dictation_is_typed_tidied_and_nothing_is_said_about_it() -> Result<()> {
        let mut window = Window::pasting();
        let script = Script::Writes(vec!["So we meet ", "at six."]);
        let (told, model) = tidy(script, &mut window, false)?;
        assert_eq!(told, None);
        assert_eq!(window.typed(), "So we meet at six.");
        assert_eq!(
            model.asked(),
            [(
                Kind::Clean,
                "um so we meet at six".to_owned(),
                String::new()
            )]
        );
        Ok(())
    }

    #[test]
    fn a_dictation_that_cannot_be_tidied_is_typed_as_it_was_said() -> Result<()> {
        // The model cannot be had, fails before its first word, or writes
        // nothing: what was said is typed, spacing and all, and the user
        // is told why.
        for (script, setup) in [
            (Script::NeedsSetup("Choose a model."), true),
            (Script::Breaks(vec![], "No connection."), false),
            (Script::Writes(vec![" ", "\n"]), false),
        ] {
            let mut window = Window::pasting();
            let (told, _) = tidy(script, &mut window, false)?;
            assert_eq!(window.did, [typed("um so we meet at six ")]);
            let told = told.ok_or_else(|| Error::Host("nothing told".into()))?;
            assert_eq!(told.title, "Typed as you said it");
            assert_eq!(told.setup, setup);
            assert!(!told.message.is_empty());
        }
        Ok(())
    }

    #[test]
    fn a_model_that_fails_midway_has_its_words_taken_back_for_what_was_said() -> Result<()> {
        let mut window = Window::pasting();
        let script = Script::Breaks(vec!["So we ", "meet"], "The server stopped.");
        let (told, _) = tidy(script, &mut window, false)?;
        assert_eq!(
            window.did,
            [
                typed("So we"),
                typed(" meet"),
                Did::Erased(10, "um so we meet at six ".to_owned())
            ]
        );
        assert_eq!(told.map(|told| told.title), Some("Typed as you said it"));
        Ok(())
    }

    #[test]
    fn what_cannot_be_taken_back_stays_and_the_user_is_told_the_end_is_missing() -> Result<()> {
        // The caret was moved meanwhile, or the program takes nothing
        // back, or what was typed is more than Backspace surely deletes.
        let moved = Window {
            moved: true,
            ..Window::pasting()
        };
        let stubborn = Window {
            pastes: true,
            ..Window::default()
        };
        for (mut window, pieces) in [
            (moved, vec!["So we ", "meet"]),
            (stubborn, vec!["So we ", "meet"]),
            (Window::pasting(), vec!["So \u{1f600} ", "we"]),
        ] {
            let script = Script::Breaks(pieces, "The server stopped.");
            let (told, _) = tidy(script, &mut window, false)?;
            assert!(!window.did.iter().any(|did| matches!(did, Did::Erased(..))));
            let told = told.ok_or_else(|| Error::Host("nothing told".into()))?;
            assert_eq!(told.title, "Cleanup stopped partway");
            assert!(told.message.contains("The server stopped."));
        }
        Ok(())
    }

    #[test]
    fn a_dictation_too_long_to_tidy_is_typed_as_said_without_asking_the_model() -> Result<()> {
        let said = "word ".repeat(crate::rewrite::MAX_TEXT_CHARS / 5 + 1);
        let model = Scripted::writing(&["never"]);
        let job = Job::tidy(Kind::Clean, &said, &[]);
        let mut window = Window::pasting();
        let stop = AtomicBool::new(false);
        let told = type_tidied(&model, &job, &said, None, &mut window, &stop)?;
        assert_eq!(window.did, [typed(&said)]);
        assert!(told.is_some() && model.asked().is_empty());
        Ok(())
    }

    #[test]
    fn a_dictation_that_is_called_off_types_nothing_more() -> Result<()> {
        let mut window = Window::pasting();
        let (told, _) = tidy(Script::Writes(vec!["So we"]), &mut window, true)?;
        assert_eq!(told, None);
        assert!(window.did.is_empty());
        Ok(())
    }

    #[test]
    fn a_selection_is_replaced_by_pasting_even_where_the_text_is_one_line() -> Result<()> {
        let mut window = Window::pasting();
        put_over_selection(&mut window, "  Shorter.\n")?;
        assert_eq!(window.did, [Did::Pasted("Shorter.".to_owned())]);
        let mut window = Window::default();
        put_over_selection(&mut window, "One.\nTwo.")?;
        assert_eq!(window.did, [Did::TypedLines("One.\nTwo.".to_owned())]);
        Ok(())
    }
}
