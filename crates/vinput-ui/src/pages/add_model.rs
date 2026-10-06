//! The page for adding a model of one's own.
//!
//! One place to say where the model is (a Hugging Face link, a path, or the
//! file picker), and under it whatever the search has to show: that it is at
//! work, why it failed, the files it found with `Add` on each one that can
//! be used, or that a model was added.

use vinput_view::{Command, Finder, FoundFile, Model, Page, View};

use super::common::notice_bar;
use crate::node::{
    Action, ButtonKind, Content, Icon, Key, Node, Severity, button, card, field, header,
    human_size, info_bar, note, plain_setting, row, setting,
};

/// The field the link or the path is typed in.
const SOURCE: Key = "add-model-source";

/// What the page shows for `view`.
#[must_use]
pub fn content(view: &View) -> Content {
    let finder = &view.finder;
    let busy = finder.busy.is_some();
    let mut body = Vec::new();
    body.extend(notice_bar(view, Page::AddModel));
    body.push(source_card(finder));
    body.push(guidance());
    body.extend(finder.busy.as_deref().map(working));
    body.extend(finder.error.as_deref().map(failure));
    body.extend(
        finder
            .added
            .as_deref()
            .map(|id| added(id, &view.models.list)),
    );
    body.extend(found(&finder.files, !busy));
    Content {
        pinned: Vec::new(),
        title: "Add a model".to_owned(),
        body,
        footer: Vec::new(),
        back: Some(back()),
    }
}

/// Going back to the list of models: what the search found is forgotten, so
/// the page is empty the next time it is opened.
fn back() -> Action {
    Action::send(Command::ClearFinder).then(Action::Go(Page::Models))
}

/// Where to say which model to add: a field with Find, and the file picker.
fn source_card(finder: &Finder) -> Node {
    let idle = finder.busy.is_none();
    // Find is the next thing to do until there is something to pick from.
    let find_kind = if idle && finder.files.is_empty() && finder.added.is_none() {
        ButtonKind::Accent
    } else {
        ButtonKind::Standard
    };
    // Enter in the field does what Find does.
    let search = || Action::Submit {
        field: SOURCE,
        make: Command::FindModel,
    };
    let source = field(SOURCE, "", |_| Action::None)
        .placeholder("https://huggingface.co/organization/model")
        .wide()
        .on_enter(search())
        .enabled(idle);
    let find = button("Find", search()).kind(find_kind).enabled(idle);
    let browse = button("Browse for a file", Action::send(Command::PickModelFile)).enabled(idle);
    card([
        setting(Icon::Search, "Hugging Face link or file path")
            .detail("Paste the link of a model page or of a file in it, or type a path")
            .below(row([source.into(), find.into()]))
            .into(),
        setting(Icon::Folder, "A file on this PC")
            .detail("VinPUT keeps a copy of it, so the file can be moved afterwards")
            .control(browse.into())
            .into(),
    ])
}

/// What can be added, in a line.
fn guidance() -> Node {
    note(
        "Works with Whisper models in ggml format (.bin) that are public on Hugging Face or \
         saved on this PC. Models in GGUF, PyTorch or safetensors format need converting first.",
    )
}

/// The search at work, with what it says it is doing.
fn working(busy: &str) -> Node {
    card([plain_setting(busy).below(Node::Progress(None)).into()])
}

/// Why the last step failed.
fn failure(error: &str) -> Node {
    info_bar(Severity::Critical, error)
}

/// That the model with this id was added, and the way back to the list.
fn added(id: &str, list: &[Model]) -> Node {
    let text = match list.iter().find(|model| model.id == id) {
        Some(model) => format!("{} is now one of your speech models.", model.name),
        None => "The model is now one of your speech models.".to_owned(),
    };
    Node::InfoBar {
        severity: Severity::Success,
        text,
        action: Some(("Back to Models".to_owned(), back())),
        dismiss: None,
    }
}

/// The files the search found, each with its own Add.
fn found(files: &[FoundFile], idle: bool) -> Vec<Node> {
    if files.is_empty() {
        return Vec::new();
    }
    let heading = match files.len() {
        1 => "Found 1 file".to_owned(),
        count => format!("Found {count} files"),
    };
    let rows = files
        .iter()
        .enumerate()
        .map(|(index, file)| file_row(index, file, idle));
    vec![header(heading), card(rows)]
}

/// One file: its path, its size and what it is, or why it cannot be used.
fn file_row(index: usize, file: &FoundFile, idle: bool) -> Node {
    let add = if file.usable {
        button("Add", Action::send(Command::AddModel(index))).enabled(idle)
    } else {
        button("Add", Action::None).enabled(false)
    };
    let icon = if file.usable {
        Icon::Model
    } else {
        Icon::Warning
    };
    setting(icon, &file.path)
        .detail(format!("{} \u{b7} {}", human_size(file.bytes), file.detail))
        .control(add.into())
        .enabled(file.usable)
        .into()
}

#[cfg(test)]
mod tests {
    use vinput_view::{Notice, NoticeKind};

    use super::super::common::probe::{Press, Seen, TYPED, all, command, scene, settings};
    use super::*;
    use crate::node::Field;

    fn press<'a>(seen: &'a Seen, label: &str) -> &'a Press {
        let Some(found) = seen.presses.iter().find(|press| press.label == label) else {
            unreachable!("nothing to press called {label}");
        };
        found
    }

    /// The field the link or the path is typed in.
    fn source(content: &Content) -> &Field {
        let found = all(&content.body).into_iter().find_map(|node| match node {
            Node::Field(field) if field.key == SOURCE => Some(field),
            _ => None,
        });
        let Some(field) = found else {
            unreachable!("the page has no field for the source");
        };
        field
    }

    /// Whether `action` forgets what the search found and then turns to the
    /// Models page.
    fn leaves(action: &Action) -> bool {
        let Action::Both(first, second) = action else {
            return false;
        };
        matches!(**first, Action::Send(Command::ClearFinder))
            && matches!(**second, Action::Go(Page::Models))
    }

    #[test]
    fn the_empty_page_asks_for_a_link_or_a_file() {
        let content = content(&scene("settings"));
        assert!(content.back.as_ref().is_some_and(leaves));
        let field = source(&content);
        assert!(field.enabled && field.wide);
        assert!(matches!(
            field.on_enter,
            Action::Submit { field: SOURCE, .. }
        ));
        assert_eq!(
            command(&field.on_enter),
            Some(Command::FindModel(TYPED.to_owned()))
        );
        assert!((field.action)("anything".to_owned()).is_none());
        let seen = Seen::of(&content.body);
        assert_eq!(seen.fields.len(), 1);
        assert_eq!(seen.fields[0].0, SOURCE);
        assert!(seen.fields[0].1.contains("huggingface.co"));
        assert_eq!(seen.labels(), ["Find", "Browse for a file"]);
        assert_eq!(
            seen.commands(),
            [Command::FindModel(TYPED.to_owned()), Command::PickModelFile]
        );
        let find = press(&seen, "Find");
        assert_eq!(find.kind, ButtonKind::Accent);
        assert!(matches!(find.action, Action::Submit { field: SOURCE, .. }));
        assert!(seen.says("ggml format (.bin)"));
        assert!(seen.bars.is_empty() && seen.progress.is_empty());
    }

    #[test]
    fn a_search_at_work_says_so_and_holds_the_buttons() {
        let content = content(&scene("add-model"));
        let seen = Seen::of(&content.body);
        assert!(seen.says("Looking at the repository"));
        assert_eq!(seen.progress, [None]);
        assert!(seen.labels().is_empty());
        assert!(seen.commands().is_empty());
        assert_eq!(seen.presses.len(), 2);
        assert!(!source(&content).enabled, "no typing while it searches");
    }

    #[test]
    fn found_files_can_be_added_only_if_they_are_usable() {
        let content = content(&scene("found"));
        let seen = Seen::of(&content.body);
        assert!(seen.says("Found 3 files"));
        assert_eq!(
            seen.commands(),
            [
                Command::FindModel(TYPED.to_owned()),
                Command::PickModelFile,
                Command::AddModel(0),
                Command::AddModel(1),
            ]
        );
        // With files to pick from, Find is no longer the thing to do next.
        assert_eq!(press(&seen, "Find").kind, ButtonKind::Standard);

        let rows = settings(&content.body);
        let Some(unusable) = rows.iter().find(|line| line.title == "model.safetensors") else {
            unreachable!("the unusable file was expected in the list");
        };
        assert!(unusable.detail.contains("3.0 GB"));
        assert!(unusable.detail.contains("Not a format VinPUT can run"));
        let within = Seen::of_setting(unusable);
        let [add] = within.presses.as_slice() else {
            unreachable!("one button was expected on the row");
        };
        assert!(!add.enabled);
        assert_eq!(command(&add.action), None);
        assert!(!unusable.enabled, "the row is out of reach as a whole");

        let Some(usable) = rows.iter().find(|line| line.title == "ggml-model-q5_0.bin") else {
            unreachable!("the first file was expected in the list");
        };
        assert_eq!(
            usable.detail,
            "547 MB \u{b7} Whisper large-v3, 99 languages"
        );
        assert!(usable.enabled);
    }

    #[test]
    fn nothing_is_added_while_the_search_is_at_work() {
        let mut view = scene("found");
        view.finder.busy = Some("Checking the file".to_owned());
        let seen = Seen::of(&content(&view).body);
        assert!(seen.commands().is_empty());
        assert!(seen.says("Found 3 files"));
    }

    #[test]
    fn a_failed_search_gives_its_reason() {
        let mut view = scene("notice");
        let seen = Seen::of(&content(&view).body);
        assert_eq!(
            seen.bars,
            [(
                Severity::Critical,
                "That link does not lead to a Hugging Face repository".to_owned()
            )]
        );
        assert_eq!(seen.labels(), ["Find", "Browse for a file"]);

        // The notice of that scene is about another page; one about this
        // page comes first and can be dismissed.
        view.notice = Some(Notice {
            kind: NoticeKind::Info,
            text: "This model is on the list already.".to_owned(),
            page: Page::AddModel,
        });
        let seen = Seen::of(&content(&view).body);
        assert_eq!(seen.bars.len(), 2);
        assert_eq!(seen.bars[0].0, Severity::Info);
        assert!(seen.commands().contains(&Command::DismissNotice));
    }

    #[test]
    fn an_added_model_is_named_with_a_way_back() {
        let mut view = scene("settings");
        view.finder.added = Some("ggml-base".to_owned());
        let seen = Seen::of(&content(&view).body);
        assert_eq!(
            seen.bars,
            [(
                Severity::Success,
                "Whisper base is now one of your speech models.".to_owned()
            )]
        );
        let back = press(&seen, "Back to Models");
        assert!(leaves(&back.action));
        assert_eq!(press(&seen, "Find").kind, ButtonKind::Standard);

        view.finder.added = Some("gone".to_owned());
        let seen = Seen::of(&content(&view).body);
        assert!(seen.says("The model is now one of your speech models."));
    }
}
