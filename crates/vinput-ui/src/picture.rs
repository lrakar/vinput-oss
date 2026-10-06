//! Pictures of windows, drawn off screen.
//!
//! A picture goes through the same layout and the same painting as the
//! window on screen, onto a bitmap in memory instead, so it shows what the
//! window shows without one ever opening. It covers the inside of the
//! window; the title bar, which Windows draws, is left out, and where the
//! live window lets the wallpaper tint show through (Mica) the picture has
//! the plain color Windows falls back to.

use std::path::Path;

use vinput_view::View;

use crate::error::Result;
use crate::geometry::Size;
use crate::gfx::Gfx;
use crate::imaging::Imaging;
use crate::look::Look;
use crate::paint;
use crate::scene::Showing;
use crate::shell;
use crate::theme::{Theme, metric};

/// How a picture is taken.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shot<'a> {
    /// The theme to draw in.
    pub theme: Theme,
    /// How many pixels a unit is: 1 for a display at 100 %, 1.5 at 150 %.
    pub scale: f32,
    /// The size of the inside of the window in units, or `None` for the
    /// size the window opens with.
    pub size: Option<(f32, f32)>,
    /// What the user is in the middle of.
    pub look: Look<'a>,
}

impl Shot<'_> {
    /// A picture in `theme` of the window as it opens, at 100 %.
    #[must_use]
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            scale: 1.0,
            size: None,
            look: Look {
                // A bar that moves is caught a third of the way along.
                phase: 0.55,
                ..Look::default()
            },
        }
    }
}

/// The number of pixels `units` are at `scale`.
fn pixels(units: f32, scale: f32) -> u32 {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to a sane size first"
    )]
    let pixels = (units * scale).round().clamp(1.0, 16_384.0) as u32;
    pixels
}

/// Draws `showing` for `view` and writes it to `path` as a PNG file.
///
/// # Errors
///
/// Returns [`crate::Error::System`] if drawing fails and
/// [`crate::Error::Picture`] if the file cannot be written.
pub fn save(showing: &Showing, view: &View, shot: &Shot<'_>, path: &Path) -> Result<()> {
    let scale = shot.scale.clamp(0.5, 4.0);
    let gfx = Gfx::new(shot.theme.text_scale)?;
    let imaging = Imaging::new()?;
    let content = showing.content(view);
    let (width, height) = shot.size.or_else(|| showing.size()).unwrap_or_else(|| {
        let width = metric::DIALOG_W;
        (width, shell::compact_height(&content, width, gfx.fonts()))
    });
    let frame = shell::frame(
        content,
        showing.chrome(view),
        Size::new(width, height),
        gfx.fonts(),
    );
    let bitmap = imaging.bitmap(pixels(width, scale), pixels(height, scale))?;
    let mut canvas = gfx.bitmap_canvas(&bitmap, scale)?;
    if let Some(icon) = imaging.app_icon(pixels(metric::LOGO, scale)) {
        canvas.set_logo(&icon);
    }
    // A page cannot be scrolled past its end, in a picture no more than on
    // screen.
    let look = Look {
        scroll: shot.look.scroll.clamp(0.0, frame.scroll_range()),
        ..shot.look
    };
    canvas.begin(paint::background(&frame, &shot.theme, &look));
    paint::frame(&canvas, &frame, &shot.theme, &look);
    canvas.end()?;
    drop(canvas);
    imaging.save_png(&bitmap, path)
}
