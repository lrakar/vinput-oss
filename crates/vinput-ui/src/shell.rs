//! What is around a page: the navigation pane with the state of the app, the
//! title with its way back, what the page pins under the title, the part
//! that scrolls and the strip of buttons at the bottom. The result is a
//! [`Frame`]: a whole window, laid out.

use vinput_view::{App, Page, RunsOn, Status};

use crate::geometry::{Rect, Size};
use crate::layout::{Builder, Control, Item, Kind, Measure, Memory, Pane, Region};
use crate::node::{Action, ButtonKind, Content, Icon, TextStyle, Tone};
use crate::pages;
use crate::theme::metric;

/// What is around the page.
#[derive(Debug, Clone, Copy)]
pub enum Chrome<'a> {
    /// The Settings window: a navigation pane beside the page.
    Settings {
        /// The page that is open.
        page: Page,
        /// What the app is doing, for the foot of the pane.
        status: &'a Status,
    },
    /// The Settings window while it shows the welcome: no navigation pane,
    /// only who we are and the page, as a column in the middle.
    Focused,
    /// A small window with nothing but its content: the update window, a
    /// question, a message.
    Compact,
}

/// A window, laid out at one size.
pub struct Frame {
    /// The size it was laid out at.
    pub size: Size,
    /// What is painted in the parts that stay put.
    pub fixed: Vec<Item>,
    /// What is painted in the part that scrolls, where it is before any
    /// scrolling.
    pub body: Vec<Item>,
    /// Everything that can be worked, in the order Tab walks it.
    pub controls: Vec<Control>,
    /// The areas inside the page that scroll by themselves; see
    /// [`Frame::panes`].
    panes: Vec<Pane>,
    /// The navigation pane, where Windows shows its wallpaper tint if it can.
    pub pane: Option<Rect>,
    /// What the page lies on.
    pub sheet: Rect,
    /// The strip of buttons at the bottom of a small window.
    pub strip: Option<Rect>,
    /// The part the body shows through.
    pub viewport: Rect,
    /// How tall the body is in all.
    pub content_height: f32,
    /// Whether anything in it moves by itself and needs repainting on a
    /// timer: a progress bar that does not know how far along it is.
    pub moving: bool,
}

impl Frame {
    /// The areas inside the page that scroll by themselves. A program
    /// without pages that have such lists has none, which the compiler is
    /// told here, so that it leaves out what scrolls them; see
    /// [`crate::node::Pickers`].
    #[must_use]
    pub fn panes(&self) -> &[Pane] {
        if cfg!(feature = "pickers") {
            &self.panes
        } else {
            &[]
        }
    }

    /// How far the body can be scrolled.
    #[must_use]
    pub fn scroll_range(&self) -> f32 {
        (self.content_height - self.viewport.h).max(0.0)
    }
}

/// The words for where the model runs.
fn runs_on(place: &RunsOn) -> String {
    match place {
        RunsOn::Nowhere => String::new(),
        RunsOn::Online => "Runs online".to_owned(),
        RunsOn::Processor => "Runs on the processor".to_owned(),
        RunsOn::GraphicsCard(name) => format!("Runs on {name}"),
        RunsOn::Spare(name) => format!("Hours used up, using {name}"),
        RunsOn::StandIn(name) => format!("Out of reach, using {name}"),
    }
}

/// Lays out the navigation pane: who we are, where one can go, and what the
/// app is doing.
fn pane(builder: &mut Builder<'_>, open: Page, status: &Status, height: f32) {
    let margin = metric::NAV_MARGIN;
    let head = Rect::new(0.0, 0.0, metric::PANE, metric::NAV_HEAD);
    // The name alone, in line with the icons of the entries under it.
    let name_x = margin + metric::NAV_ICON_X;
    let line = Size::new(
        metric::PANE - name_x - margin,
        builder.line_height(TextStyle::BodyStrong),
    );
    builder.line(
        Rect::new(name_x, head.y, 0.0, head.h).left_center(line),
        pages::name(open.app()),
        TextStyle::BodyStrong,
        Tone::Primary,
    );

    let home = pages::navigation_home(open);
    let mut y = metric::NAV_HEAD;
    // An entry is as much taller than a button as it usually is.
    let item = builder.control_height() + metric::NAV_ITEM - metric::CONTROL;
    for &(page, label, icon) in pages::navigation(open.app()) {
        builder.control(
            Rect::new(margin, y, metric::PANE - 2.0 * margin, item),
            Kind::Nav {
                page,
                label: label.to_owned(),
                icon,
                selected: page == home,
            },
            true,
            None,
        );
        y += item + metric::NAV_GAP;
    }
    foot(builder, open.app(), status, height);
}

/// What the foot of the pane says of a program: its state in a word, with
/// the color of the dot before it, and where the link under it leads while
/// there is nothing to name there yet.
struct Standing {
    word: &'static str,
    tone: Tone,
    choose: (&'static str, Page),
}

/// How `app` stands, from what its status says. Every program fills the
/// same record: whether it is at work, and the name of what it works with.
fn standing(app: App, status: &Status) -> Standing {
    let chosen = status.model.is_some();
    match app {
        App::Vinput => Standing {
            word: match (chosen, status.listening) {
                (false, _) => "No speech model yet",
                (true, true) => "Listening",
                (true, false) => "Ready",
            },
            tone: match (chosen, status.listening) {
                (false, _) => Tone::Caution,
                (true, true) => Tone::Critical,
                (true, false) => Tone::Success,
            },
            choose: ("Choose a model", Page::Models),
        },
        // The hub is at work for nobody; it says who is signed in.
        #[cfg(feature = "hub")]
        App::Hub => Standing {
            word: if chosen { "Signed in" } else { "Not signed in" },
            tone: if chosen {
                Tone::Success
            } else {
                Tone::Disabled
            },
            choose: ("Sign in", Page::Account),
        },
        #[cfg(feature = "voutput")]
        App::Voutput => Standing {
            word: match (chosen, status.listening) {
                (false, _) => "No voice yet",
                (true, true) => "Reading aloud",
                (true, false) => "Ready",
            },
            tone: match (chosen, status.listening) {
                (false, _) => Tone::Caution,
                (true, true) => Tone::Accent,
                (true, false) => Tone::Success,
            },
            choose: ("Choose a voice", Page::Voices),
        },
    }
}

/// The foot of the pane: listening or ready, the model and where it runs.
fn foot(builder: &mut Builder<'_>, app: App, status: &Status, height: f32) {
    let pad = metric::ROW_PAD;
    let place = runs_on(&status.runs_on);
    let Standing { word, tone, choose } = standing(app, status);
    let strong = builder.line_height(TextStyle::BodyStrong);
    let small = builder.line_height(TextStyle::Caption);
    let lines = if place.is_empty() { 1.0 } else { 2.0 };
    let mut y = height - pad - (strong + metric::GRID + small * lines);
    builder.item(Item::Divider(Rect::new(
        metric::NAV_MARGIN,
        y - pad,
        metric::PANE - 2.0 * metric::NAV_MARGIN,
        metric::LINE,
    )));
    let dot = Size::new(metric::DOT, metric::DOT);
    let dot_x = metric::NAV_LOGO_X + (metric::LOGO - metric::DOT) / 2.0;
    builder.item(Item::Dot {
        rect: Rect::new(dot_x, y, 0.0, strong).left_center(dot),
        tone,
    });
    let text_x = metric::NAV_MARGIN + metric::NAV_TEXT_X;
    let text_w = metric::PANE - text_x - pad;
    builder.line(
        Rect::new(text_x, y, text_w, strong),
        word,
        TextStyle::BodyStrong,
        Tone::Primary,
    );
    y += strong + metric::GRID;
    let Some(model) = &status.model else {
        let (label, page) = choose;
        let size = builder.text_size(label, TextStyle::Caption, text_w);
        builder.control(
            Rect::new(text_x, y, size.w, small),
            Kind::Link {
                label: label.to_owned(),
                action: Action::Go(page),
            },
            true,
            None,
        );
        return;
    };
    builder.line(
        Rect::new(text_x, y, text_w, small),
        model.clone(),
        TextStyle::Caption,
        Tone::Secondary,
    );
    if !place.is_empty() {
        builder.line(
            Rect::new(text_x, y + small, text_w, small),
            place,
            TextStyle::Caption,
            Tone::Secondary,
        );
    }
}

/// Places the title at `x`, `top`, with the way back before it if the page
/// has one. Returns the height the title took.
fn title(
    builder: &mut Builder<'_>,
    text: String,
    back: Option<Action>,
    style: TextStyle,
    at: Rect,
) -> f32 {
    let back_width = metric::CONTROL + metric::GAP;
    let (mut x, mut width) = (at.x, at.w);
    if back.is_some() {
        width -= back_width - metric::GAP;
    }
    let height = builder.text_size(&text, style, width).h;
    if let Some(action) = back {
        // The arrow has no frame, so it starts a little to the left of the
        // page's edge for its glyph to line up with what is under it.
        builder.control(
            Rect::new(x - metric::GAP, at.y, 0.0, height)
                .left_center(Size::new(metric::CONTROL, metric::CONTROL)),
            Kind::Button {
                label: String::new(),
                icon: Some(Icon::Back),
                kind: ButtonKind::Subtle,
                action,
                hint: "Back".to_owned(),
            },
            true,
            None,
        );
        x += back_width - metric::GAP;
    }
    builder.item(Item::Text {
        rect: Rect::new(x, at.y, width, height),
        text,
        style,
        tone: Tone::Primary,
        trim: false,
    });
    height
}

/// Lays `content` out as a window of `size` with `chrome` around it, as it
/// is before the user has opened or typed anything.
#[must_use]
pub fn frame(content: Content, chrome: Chrome<'_>, size: Size, measure: &dyn Measure) -> Frame {
    frame_with(content, chrome, size, measure, &Memory::default())
}

/// Lays `content` out as a window of `size` with `chrome` around it, with
/// the rows open and the lists searched that `memory` says.
#[must_use]
pub fn frame_with(
    content: Content,
    chrome: Chrome<'_>,
    size: Size,
    measure: &dyn Measure,
    memory: &Memory,
) -> Frame {
    let mut builder = Builder::new(measure).remembering(memory);
    let compact = matches!(chrome, Chrome::Compact);
    let sheet_x = match chrome {
        Chrome::Settings { page, status } => {
            pane(&mut builder, page, status, size.h);
            metric::PANE
        }
        Chrome::Focused | Chrome::Compact => 0.0,
    };
    let focused = matches!(chrome, Chrome::Focused);
    let (pad, top, style, gap, bottom) = if compact {
        (
            metric::DIALOG_PAD,
            metric::DIALOG_PAD,
            TextStyle::Subtitle,
            metric::DIALOG_GAP,
            metric::DIALOG_PAD,
        )
    } else {
        (
            metric::PAGE_PAD,
            metric::PAGE_TOP,
            TextStyle::Title,
            metric::TITLE_GAP,
            metric::PAGE_BOTTOM,
        )
    };
    // A page is at most so wide, and sits in the middle of a window that is
    // wider, as the pages of Windows' own Settings do.
    let room = (size.w - sheet_x - 2.0 * pad).max(1.0);
    let width = room.min(if focused {
        metric::WELCOME_MAX
    } else {
        metric::PAGE_MAX
    });
    let x = sheet_x + pad + ((room - width) / 2.0).floor();
    // The welcome has no pane; a little more room over its title stands
    // in for the head of one.
    let top = if focused { metric::WELCOME_TOP } else { top };
    let title_h = title(
        &mut builder,
        content.title,
        content.back,
        style,
        Rect::new(x, top, width, 0.0),
    );

    // The strip of buttons at the bottom.
    let footer_h = if content.footer.is_empty() {
        0.0
    } else {
        metric::CONTROL.max(builder.footer_height(&content.footer, width))
            + 2.0 * metric::FOOTER_PAD
    };
    let strip = Rect::new(sheet_x, size.h - footer_h, size.w - sheet_x, footer_h);

    // What scrolls between them. It starts a little early so the focus
    // rings of the first row are not cut off.
    let slack = metric::GRID;
    let mut body_top = top + title_h + gap;
    // What is pinned is laid out with the parts that stay put, so the body
    // starts, and is cut off, under it.
    if !content.pinned.is_empty() {
        body_top += builder.column(content.pinned, x, body_top, width) + metric::PINNED_GAP;
    }
    let viewport = Rect::new(
        sheet_x,
        body_top - slack,
        size.w - sheet_x,
        (strip.y - body_top + slack).max(0.0),
    );
    builder.region(Region::Body);
    let content_height = slack + builder.column(content.body, x, body_top, width) + bottom;

    builder.region(Region::Fixed);
    if footer_h > 0.0 {
        let line = Rect::new(strip.x, strip.y, strip.w, metric::LINE);
        builder.item(Item::Divider(line));
        builder.footer(content.footer, Rect::new(x, strip.y, width, strip.h));
    }

    let laid = builder.finish();
    let moving = laid
        .body
        .iter()
        .chain(&laid.fixed)
        .any(|item| matches!(item, Item::Progress { value: None, .. }));
    Frame {
        size,
        fixed: laid.fixed,
        body: laid.body,
        controls: laid.controls,
        panes: laid.panes,
        pane: matches!(chrome, Chrome::Settings { .. })
            .then(|| Rect::new(0.0, 0.0, metric::PANE, size.h)),
        sheet: Rect::new(sheet_x, 0.0, size.w - sheet_x, size.h),
        strip: (compact && footer_h > 0.0).then_some(strip),
        viewport,
        content_height,
        moving,
    }
}

/// The height a small window needs to show all of `content` at `width`.
#[must_use]
pub fn compact_height(content: &Content, width: f32, measure: &dyn Measure) -> f32 {
    let builder = Builder::new(measure);
    let pad = metric::DIALOG_PAD;
    let inner = (width - 2.0 * pad).max(1.0);
    let title = builder
        .text_size(&content.title, TextStyle::Subtitle, inner)
        .h;
    let mut body = builder.column_height(&content.body, inner);
    if !content.pinned.is_empty() {
        body += builder.column_height(&content.pinned, inner) + metric::PINNED_GAP;
    }
    let footer = if content.footer.is_empty() {
        0.0
    } else {
        metric::CONTROL.max(builder.footer_height(&content.footer, inner))
            + 2.0 * metric::FOOTER_PAD
    };
    (pad + title + metric::DIALOG_GAP + body + pad + footer)
        .clamp(metric::DIALOG_MIN_H, metric::DIALOG_MAX_H)
}

#[cfg(test)]
mod tests;
