//! Windows that only ask or tell: setup, a question, a message.
//!
//! The heading is the title in large type, the detail goes under it, the
//! points are a list in a card, and the buttons stay at the bottom edge: the
//! one that says yes in the accent color, then the one that says no.
//!
//! A window that asks closes with an answer. The button that says yes
//! carries [`Action::Accept`] and the one that says no [`Action::Close`], so
//! nothing but the yes button can be taken for a yes. A window with a single
//! button only tells: there is nothing to answer, and its button closes it.

use vinput_view::Dialog;

use crate::node::{
    Action, Content, Node, TextStyle, Tone, accent_button, bullets, button, card, styled,
};

/// The caption of the one button when the dialog names none.
const OK: &str = "OK";

/// What the window shows for `dialog`.
#[must_use]
pub fn content(dialog: &Dialog) -> Content {
    let mut body = Vec::new();
    body.extend(detail(&dialog.detail));
    body.extend(points(&dialog.lines));
    Content {
        pinned: Vec::new(),
        title: dialog.heading.clone(),
        body,
        footer: buttons(&dialog.primary, &dialog.secondary),
        back: None,
    }
}

/// The line under the heading, if there is one.
fn detail(detail: &str) -> Option<Node> {
    let detail = detail.trim();
    (!detail.is_empty()).then(|| styled(detail, TextStyle::Body, Tone::Secondary))
}

/// The points, as one list in a card. Without a point that says something
/// there is no card.
fn points(lines: &[String]) -> Option<Node> {
    lines
        .iter()
        .any(|line| !line.trim().is_empty())
        .then(|| card([bullets(lines)]))
}

/// The button that says yes, then the one that says no if the dialog has one.
fn buttons(primary: &str, secondary: &str) -> Vec<Node> {
    let primary = match primary.trim() {
        "" => OK,
        caption => caption,
    };
    let secondary = secondary.trim();
    if secondary.is_empty() {
        return vec![accent_button(primary, Action::Close).into()];
    }
    vec![
        accent_button(primary, Action::Accept).into(),
        button(secondary, Action::Close).into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::super::common::probe::Seen;
    use super::*;
    use crate::node::ButtonKind;

    fn setup() -> Dialog {
        Dialog {
            title: "VinPUT setup".to_owned(),
            heading: "Install VinPUT 0.3.0".to_owned(),
            detail: "Type with your voice into any text field.".to_owned(),
            lines: vec![
                "For your Windows account only; no administrator rights".to_owned(),
                String::new(),
                "Keeps your settings, keys and downloaded models".to_owned(),
            ],
            primary: "Install".to_owned(),
            secondary: "Cancel".to_owned(),
        }
    }

    #[test]
    fn a_question_has_an_accent_yes_and_a_no_that_closes() {
        let content = content(&setup());
        assert_eq!(content.title, "Install VinPUT 0.3.0");
        assert!(content.back.is_none());
        assert!(matches!(
            content.body.as_slice(),
            [Node::Text { tone: Tone::Secondary, .. }, Node::Card(inside)]
                if matches!(inside.as_slice(), [Node::List(points)] if points.len() == 2)
        ));
        let body = Seen::of(&content.body);
        assert_eq!(
            body.texts,
            [
                "Type with your voice into any text field.",
                "For your Windows account only; no administrator rights",
                "Keeps your settings, keys and downloaded models",
            ]
        );
        assert!(body.presses.is_empty());

        let footer = Seen::of(&content.footer);
        let [yes, no] = footer.presses.as_slice() else {
            unreachable!("two buttons were expected");
        };
        assert_eq!(
            (yes.label.as_str(), yes.kind),
            ("Install", ButtonKind::Accent)
        );
        assert!(matches!(yes.action, Action::Accept));
        assert_eq!(
            (no.label.as_str(), no.kind),
            ("Cancel", ButtonKind::Standard)
        );
        assert!(matches!(no.action, Action::Close));
    }

    #[test]
    fn a_message_has_one_button_that_closes() {
        let message = Dialog {
            heading: "VinPUT has been removed.".to_owned(),
            ..Dialog::default()
        };
        let content = content(&message);
        assert!(content.body.is_empty());
        let footer = Seen::of(&content.footer);
        let [only] = footer.presses.as_slice() else {
            unreachable!("one button was expected");
        };
        assert_eq!((only.label.as_str(), only.kind), (OK, ButtonKind::Accent));
        assert!(matches!(only.action, Action::Close));
        assert_eq!(footer.labels(), [OK]);
    }

    #[test]
    fn points_that_say_nothing_make_no_card() {
        let message = Dialog {
            heading: "Done".to_owned(),
            lines: vec![String::new(), "  ".to_owned()],
            ..Dialog::default()
        };
        assert!(content(&message).body.is_empty());
    }
}
