//! Drawing: `Direct2D` for shapes, `DirectWrite` for text.
//!
//! A [`Canvas`] draws in device-independent units onto a window or onto a
//! bitmap in memory; the two are the same to whoever paints. [`Fonts`]
//! holds the type ramp and measures text for layout.
#![allow(unsafe_code)]

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use windows::Win32::Foundation::{D2DERR_RECREATE_TARGET, HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_GRADIENT_STOP, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
    D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, D2D1_ELLIPSE, D2D1_EXTEND_MODE_CLAMP,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_GAMMA_2_2, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
    D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_ROUNDED_RECT, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
    D2D1CreateFactory, ID2D1Bitmap, ID2D1DCRenderTarget, ID2D1Factory, ID2D1RenderTarget,
    ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT, DWRITE_LINE_SPACING_METHOD_UNIFORM, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_PARAGRAPH_ALIGNMENT_NEAR, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_METRICS, DWRITE_TEXT_RANGE, DWRITE_TRIMMING,
    DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_EMERGENCY_BREAK,
    DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory, IDWriteFactory, IDWriteFontCollection,
    IDWriteTextFormat, IDWriteTextLayout,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{GetDC, HDC, ReleaseDC};
use windows::Win32::Graphics::Imaging::{IWICBitmap, IWICBitmapSource};
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows::core::{BOOL, HSTRING, w};
use windows_numerics::Vector2;

use crate::error::{Error, Result};
use crate::geometry::{Point, Rect, Size};
use crate::layout::{Corners, Measure};
use crate::node::TextStyle;
use crate::theme::{Color, Family, face, font};

/// The largest size text is laid out in when nothing limits it.
const BOUNDLESS: f32 = 100_000.0;
/// Where the baseline sits in a line, as a part of the line's height.
const BASELINE: f32 = 0.8;
/// Dots per inch of a display at 100 %.
const BASE_DPI: f32 = 96.0;

/// Turns a failure of Windows into an [`Error`] that says what was tried.
pub(crate) fn failed(what: &'static str) -> impl Fn(windows::core::Error) -> Error {
    move |error| Error::System {
        what,
        code: error.code().0,
    }
}

fn color(color: Color) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: color.r,
        g: color.g,
        b: color.b,
        a: color.a,
    }
}

fn rect_f(rect: Rect) -> D2D_RECT_F {
    D2D_RECT_F {
        left: rect.x,
        top: rect.y,
        right: rect.right(),
        bottom: rect.bottom(),
    }
}

/// How a line of text sits in its rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextBox {
    /// Where it sits sideways.
    pub align: Align,
    /// Whether it is in the middle of the height rather than at the top.
    pub middle: bool,
    /// Whether it stays on one line and ends in an ellipsis when it is too
    /// long, rather than wrapping.
    pub trim: bool,
    /// Whether it is underlined.
    pub underline: bool,
}

/// Where text sits sideways in its rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    /// At the left edge.
    #[default]
    Left,
    /// In the middle.
    Center,
    /// At the right edge.
    Right,
}

/// The type ramp as `DirectWrite` formats, and the ruler layout uses.
#[derive(Clone)]
pub struct Fonts {
    factory: IDWriteFactory,
    formats: [IDWriteTextFormat; 6],
    icon_family: HSTRING,
    icons: Rc<RefCell<Vec<(f32, IDWriteTextFormat)>>>,
}

const fn slot(style: TextStyle) -> usize {
    match style {
        TextStyle::Caption => 0,
        TextStyle::Body => 1,
        TextStyle::BodyStrong => 2,
        TextStyle::Subtitle => 3,
        TextStyle::Title => 4,
        TextStyle::Mono => 5,
    }
}

/// The first of `names` that this PC has, or the last of them.
fn first_installed(collection: Option<&IDWriteFontCollection>, names: [&str; 2]) -> HSTRING {
    for name in names {
        let name = HSTRING::from(name);
        let (mut index, mut exists) = (0_u32, BOOL(0));
        // SAFETY: `name` is NUL-terminated and both out pointers are valid
        // for the call.
        let found = collection.is_some_and(|fonts| unsafe {
            fonts
                .FindFamilyName(&name, &raw mut index, &raw mut exists)
                .is_ok()
        });
        if found && exists.as_bool() {
            return name;
        }
    }
    HSTRING::from(names[1])
}

impl Fonts {
    /// The type ramp, each size multiplied by `text_scale`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::System`] if `DirectWrite` cannot be reached.
    pub fn new(text_scale: f32) -> Result<Self> {
        // SAFETY: plain factory creation; the result is checked.
        let factory: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }
            .map_err(failed("starting DirectWrite"))?;
        let mut collection = None;
        // SAFETY: `collection` is a valid out pointer for the call.
        let _ = unsafe { factory.GetSystemFontCollection(&raw mut collection, false) };
        let family = |family: Family| {
            first_installed(
                collection.as_ref(),
                match family {
                    Family::Text => font::TEXT,
                    Family::Display => font::DISPLAY,
                    Family::Mono => font::MONO,
                },
            )
        };
        let make = |style: TextStyle| -> Result<IDWriteTextFormat> {
            let face = face(style);
            let weight = if face.semibold {
                font::SEMIBOLD
            } else {
                font::REGULAR
            };
            let line = face.line * text_scale;
            // SAFETY: the family and locale names are NUL-terminated and
            // outlive the calls; the format is checked before use.
            unsafe {
                let format = factory
                    .CreateTextFormat(
                        &family(face.family),
                        None,
                        DWRITE_FONT_WEIGHT(weight),
                        DWRITE_FONT_STYLE_NORMAL,
                        DWRITE_FONT_STRETCH_NORMAL,
                        face.size * text_scale,
                        w!(""),
                    )
                    .map_err(failed("loading a font"))?;
                format
                    .SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, line, line * BASELINE)
                    .map_err(failed("setting up a font"))?;
                // A word longer than its line (a path, the name of a file)
                // is broken where the line ends rather than left to run out
                // of its place. Windows 7 does not know how, and wraps
                // between words only.
                let _ = format.SetWordWrapping(DWRITE_WORD_WRAPPING_EMERGENCY_BREAK);
                Ok(format)
            }
        };
        let formats = [
            make(TextStyle::Caption)?,
            make(TextStyle::Body)?,
            make(TextStyle::BodyStrong)?,
            make(TextStyle::Subtitle)?,
            make(TextStyle::Title)?,
            make(TextStyle::Mono)?,
        ];
        Ok(Self {
            icon_family: first_installed(collection.as_ref(), font::ICONS),
            factory,
            formats,
            icons: Rc::new(RefCell::new(Vec::new())),
        })
    }

    fn icon_format(&self, size: f32) -> Option<IDWriteTextFormat> {
        let mut icons = self.icons.borrow_mut();
        if let Some((_, format)) = icons.iter().find(|(known, _)| (known - size).abs() < 0.01) {
            return Some(format.clone());
        }
        // SAFETY: the family and locale names are NUL-terminated and outlive
        // the call.
        let format = unsafe {
            self.factory.CreateTextFormat(
                &self.icon_family,
                None,
                DWRITE_FONT_WEIGHT(font::REGULAR),
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!(""),
            )
        }
        .ok()?;
        icons.push((size, format.clone()));
        Some(format)
    }

    fn layout(
        &self,
        text: &str,
        format: &IDWriteTextFormat,
        size: Size,
    ) -> Option<IDWriteTextLayout> {
        let text: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: `text` is a valid slice for the call and the format is a
        // live object.
        unsafe {
            self.factory
                .CreateTextLayout(&text, format, size.w.max(0.0), size.h.max(0.0))
        }
        .ok()
    }

    /// The system's name of the font ordinary text is set in.
    #[must_use]
    pub fn text_family(&self) -> String {
        let mut name = [0_u16; 64];
        // SAFETY: the buffer is valid for its length.
        let _ = unsafe { self.formats[slot(TextStyle::Body)].GetFontFamilyName(&mut name) };
        let end = name.iter().position(|unit| *unit == 0).unwrap_or(0);
        String::from_utf16_lossy(&name[..end])
    }

    /// The size ordinary text is set in, with the user's text scaling.
    #[must_use]
    pub fn text_size(&self) -> f32 {
        // SAFETY: a getter on a live object.
        unsafe { self.formats[slot(TextStyle::Body)].GetFontSize() }
    }
}

impl Measure for Fonts {
    fn text(&self, text: &str, style: TextStyle, max_width: f32) -> Size {
        let face = face(style);
        let fallback = Size::new(0.0, face.line);
        let room = Size::new(max_width.min(BOUNDLESS), BOUNDLESS);
        let Some(layout) = self.layout(text, &self.formats[slot(style)], room) else {
            return fallback;
        };
        let mut metrics = DWRITE_TEXT_METRICS::default();
        // SAFETY: `metrics` is a valid out pointer for the call.
        if unsafe { layout.GetMetrics(&raw mut metrics) }.is_err() {
            return fallback;
        }
        Size::new(
            metrics
                .widthIncludingTrailingWhitespace
                .ceil()
                .min(max_width),
            metrics.height,
        )
    }
}

/// The factories everything is drawn with. Cheap to clone.
#[derive(Clone)]
pub struct Gfx {
    d2d: ID2D1Factory,
    fonts: Fonts,
}

impl Gfx {
    /// Starts `Direct2D` and `DirectWrite`, with text `text_scale` times its
    /// usual size.
    ///
    /// # Errors
    ///
    /// Returns [`Error::System`] if either cannot be started.
    pub fn new(text_scale: f32) -> Result<Self> {
        // SAFETY: plain factory creation; the result is checked.
        let d2d: ID2D1Factory =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) }
                .map_err(failed("starting Direct2D"))?;
        Ok(Self {
            d2d,
            fonts: Fonts::new(text_scale)?,
        })
    }

    /// The type ramp.
    #[must_use]
    pub fn fonts(&self) -> &Fonts {
        &self.fonts
    }

    fn properties(scale: f32, software: bool) -> D2D1_RENDER_TARGET_PROPERTIES {
        D2D1_RENDER_TARGET_PROPERTIES {
            r#type: if software {
                D2D1_RENDER_TARGET_TYPE_SOFTWARE
            } else {
                D2D1_RENDER_TARGET_TYPE_DEFAULT
            },
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: BASE_DPI * scale,
            dpiY: BASE_DPI * scale,
            ..Default::default()
        }
    }

    /// A canvas on the window `hwnd`, shown at `scale` times 100 %.
    ///
    /// It is drawn by the processor and handed to the window through its
    /// device context. A window of settings is still nearly all the time,
    /// and drawn this way it holds no graphics device: the device a window
    /// drawn by the graphics card keeps, with the driver behind it, is tens
    /// of megabytes and dozens of threads for a window that sits there.
    ///
    /// # Errors
    ///
    /// Returns [`Error::System`] if Windows cannot give one.
    pub fn window_canvas(&self, hwnd: HWND, scale: f32) -> Result<Canvas> {
        // SAFETY: the properties are valid for the call.
        let target = unsafe {
            self.d2d
                .CreateDCRenderTarget(&Self::properties(scale, true))
        }
        .map_err(failed("preparing to draw the window"))?;
        let window = Window {
            hwnd,
            target: target.clone(),
            dc: Cell::new(HDC::default()),
        };
        Canvas::new(target.into(), Some(window), self.fonts.clone(), scale)
    }

    /// A canvas on `bitmap`, drawn at `scale` times 100 %.
    ///
    /// # Errors
    ///
    /// Returns [`Error::System`] if Windows cannot give one.
    pub fn bitmap_canvas(&self, bitmap: &IWICBitmap, scale: f32) -> Result<Canvas> {
        // SAFETY: the bitmap is a live object and the properties are valid
        // for the call.
        let target = unsafe {
            self.d2d
                .CreateWicBitmapRenderTarget(bitmap, &Self::properties(scale, true))
        }
        .map_err(failed("preparing to draw the picture"))?;
        Canvas::new(target, None, self.fonts.clone(), scale)
    }
}

/// The window a canvas draws on, and its device context while a drawing is
/// under way.
struct Window {
    hwnd: HWND,
    target: ID2D1DCRenderTarget,
    dc: Cell<HDC>,
}

/// Something to draw on, in device-independent units.
pub struct Canvas {
    target: ID2D1RenderTarget,
    window: Option<Window>,
    brush: ID2D1SolidColorBrush,
    fonts: Fonts,
    logo: Option<ID2D1Bitmap>,
    scale: f32,
}

impl Canvas {
    fn new(
        target: ID2D1RenderTarget,
        window: Option<Window>,
        fonts: Fonts,
        scale: f32,
    ) -> Result<Self> {
        // SAFETY: the target is live; the color is valid for the call.
        let brush = unsafe {
            target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            target.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            target.CreateSolidColorBrush(&color(Color::BLACK), None)
        }
        .map_err(failed("preparing to draw"))?;
        Ok(Self {
            target,
            window,
            brush,
            fonts,
            logo: None,
            scale,
        })
    }

    /// How many pixels one unit is.
    #[must_use]
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// The type ramp this canvas draws text with.
    #[must_use]
    pub fn fonts(&self) -> &Fonts {
        &self.fonts
    }

    /// Gives the canvas the logo to draw, from a bitmap in the format
    /// `Direct2D` takes.
    pub fn set_logo(&mut self, source: &IWICBitmapSource) {
        // SAFETY: the source is a live object.
        self.logo = unsafe { self.target.CreateBitmapFromWicBitmap(source, None) }.ok();
    }

    /// Starts a drawing and wipes the canvas with `background`. A canvas on
    /// a window covers the whole inside of the window as it is right now.
    pub fn begin(&self, background: Color) {
        if let Some(window) = &self.window {
            let mut all = RECT::default();
            // SAFETY: the window is live and of this thread, `all` is valid
            // for both calls, and the device context is given back in `end`.
            // If binding fails the drawing fails, which `end` reports.
            unsafe {
                let _ = GetClientRect(window.hwnd, &raw mut all);
                let dc = GetDC(Some(window.hwnd));
                window.dc.set(dc);
                let _ = window.target.BindDC(dc, &raw const all);
            }
        }
        // SAFETY: the target is live; `end` follows.
        unsafe {
            self.target.BeginDraw();
            self.target.Clear(Some(&color(background)));
        }
    }

    /// Finishes a drawing. Returns `false` if Windows took the surface away
    /// and the canvas has to be made again.
    ///
    /// # Errors
    ///
    /// Returns [`Error::System`] for any other failure.
    pub fn end(&self) -> Result<bool> {
        // SAFETY: `begin` came before.
        let drawn = unsafe { self.target.EndDraw(None, None) };
        if let Some(window) = &self.window {
            let dc = window.dc.replace(HDC::default());
            if !dc.is_invalid() {
                // SAFETY: the device context was taken in `begin` for this
                // window and is not used after this.
                unsafe { ReleaseDC(Some(window.hwnd), dc) };
            }
        }
        match drawn {
            Ok(()) => Ok(true),
            Err(error) if error.code() == D2DERR_RECREATE_TARGET => Ok(false),
            Err(error) => Err(failed("drawing")(error)),
        }
    }

    fn snap(&self, value: f32) -> f32 {
        (value * self.scale).round() / self.scale
    }

    fn snapped(&self, rect: Rect) -> Rect {
        let (x, y) = (self.snap(rect.x), self.snap(rect.y));
        Rect::new(
            x,
            y,
            self.snap(rect.right()) - x,
            self.snap(rect.bottom()) - y,
        )
    }

    fn paint(&self, paint: Color) -> &ID2D1SolidColorBrush {
        // SAFETY: the brush is live and the color valid for the call.
        unsafe { self.brush.SetColor(&color(paint)) };
        &self.brush
    }

    /// Fills `rect`, with corners of `radius`.
    pub fn fill(&self, rect: Rect, radius: f32, paint: Color) {
        if paint.a <= 0.0 || rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let rounded = D2D1_ROUNDED_RECT {
            rect: rect_f(self.snapped(rect)),
            radiusX: radius,
            radiusY: radius,
        };
        // SAFETY: `rounded` is valid for the call; the brush is live.
        unsafe {
            if radius > 0.0 {
                self.target
                    .FillRoundedRectangle(&raw const rounded, self.paint(paint));
            } else {
                self.target
                    .FillRectangle(&raw const rounded.rect, self.paint(paint));
            }
        }
    }

    /// Fills `rect` with only those corners rounded that `corners` names.
    pub fn fill_corners(&self, rect: Rect, radius: f32, paint: Color, corners: Corners) {
        if corners == Corners::ALL {
            self.fill(rect, radius, paint);
            return;
        }
        // The rounded shape reaches past the edges that stay square, and the
        // clip cuts it off there.
        let rect = self.snapped(rect);
        let mut grown = rect;
        if !corners.top {
            grown.y -= radius;
            grown.h += radius;
        }
        if !corners.bottom {
            grown.h += radius;
        }
        self.clip(rect);
        self.fill(grown, radius, paint);
        self.unclip();
    }

    /// Draws a line `width` thick just inside the edge of `rect`.
    pub fn stroke(&self, rect: Rect, radius: f32, paint: Color, width: f32) {
        if paint.a <= 0.0 {
            return;
        }
        let rounded = D2D1_ROUNDED_RECT {
            rect: rect_f(self.snapped(rect).inset(width / 2.0)),
            radiusX: (radius - width / 2.0).max(0.0),
            radiusY: (radius - width / 2.0).max(0.0),
        };
        // SAFETY: `rounded` is valid for the call; the brush is live.
        unsafe {
            self.target
                .DrawRoundedRectangle(&raw const rounded, self.paint(paint), width, None);
        }
    }

    /// Draws a line one unit thick just inside the edge of `rect` whose top
    /// or bottom edge has the color `edge` and the rest the color `paint`:
    /// the slight lift Windows gives its controls.
    pub fn stroke_lifted(&self, rect: Rect, radius: f32, paint: Color, edge: Color, top: bool) {
        let rect = self.snapped(rect);
        let fade = (2.0 / rect.h.max(1.0)).min(0.5);
        let stops = if top {
            [stop(0.0, edge), stop(fade, paint), stop(1.0, paint)]
        } else {
            [stop(0.0, paint), stop(1.0 - fade, paint), stop(1.0, edge)]
        };
        let line = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
            startPoint: Vector2::new(rect.x, rect.y),
            endPoint: Vector2::new(rect.x, rect.bottom()),
        };
        let rounded = D2D1_ROUNDED_RECT {
            rect: rect_f(rect.inset(0.5)),
            radiusX: (radius - 0.5).max(0.0),
            radiusY: (radius - 0.5).max(0.0),
        };
        // SAFETY: the stops, the brush properties and the rectangle are
        // valid for the calls; every object is checked before use.
        unsafe {
            let Ok(collection) = self.target.CreateGradientStopCollection(
                &stops,
                D2D1_GAMMA_2_2,
                D2D1_EXTEND_MODE_CLAMP,
            ) else {
                return;
            };
            let Ok(brush) =
                self.target
                    .CreateLinearGradientBrush(&raw const line, None, &collection)
            else {
                return;
            };
            self.target
                .DrawRoundedRectangle(&raw const rounded, &brush, 1.0, None);
        }
    }

    /// Fills the circle around `center`.
    pub fn disc(&self, center: Point, radius: f32, paint: Color) {
        let ellipse = D2D1_ELLIPSE {
            point: Vector2::new(center.x, center.y),
            radiusX: radius,
            radiusY: radius,
        };
        // SAFETY: `ellipse` is valid for the call; the brush is live.
        unsafe {
            self.target
                .FillEllipse(&raw const ellipse, self.paint(paint));
        }
    }

    /// Keeps what is drawn from now on inside `rect`, until [`Self::unclip`].
    pub fn clip(&self, rect: Rect) {
        // SAFETY: the rectangle is valid for the call.
        unsafe {
            self.target
                .PushAxisAlignedClip(&rect_f(rect), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        }
    }

    /// Ends the last [`Self::clip`].
    pub fn unclip(&self) {
        // SAFETY: a `clip` came before.
        unsafe { self.target.PopAxisAlignedClip() };
    }

    fn draw_layout(&self, layout: &IDWriteTextLayout, at: Rect, paint: Color) {
        // SAFETY: the layout and the brush are live objects.
        unsafe {
            self.target.DrawTextLayout(
                Vector2::new(at.x, at.y),
                layout,
                self.paint(paint),
                D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
            );
        }
    }

    /// Draws `text` in `style` inside `rect`.
    pub fn text(&self, text: &str, rect: Rect, style: TextStyle, paint: Color, how: TextBox) {
        if text.is_empty() {
            return;
        }
        let format = &self.fonts.formats[slot(style)];
        let room = Size::new(rect.w, if how.middle { rect.h } else { BOUNDLESS });
        let Some(layout) = self.fonts.layout(text, format, room) else {
            return;
        };
        let length = u32::try_from(text.encode_utf16().count()).unwrap_or(u32::MAX);
        // SAFETY: the layout is live; the trimming structure and its sign
        // are valid for the calls. Each setting that fails leaves the
        // default, which is harmless.
        unsafe {
            let _ = layout.SetTextAlignment(match how.align {
                Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
                Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
                Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
            });
            let _ = layout.SetParagraphAlignment(if how.middle {
                DWRITE_PARAGRAPH_ALIGNMENT_CENTER
            } else {
                DWRITE_PARAGRAPH_ALIGNMENT_NEAR
            });
            if how.trim {
                let _ = layout.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
                let trimming = DWRITE_TRIMMING {
                    granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                    ..Default::default()
                };
                if let Ok(sign) = self.fonts.factory.CreateEllipsisTrimmingSign(format) {
                    let _ = layout.SetTrimming(&raw const trimming, &sign);
                }
            }
            if how.underline {
                let _ = layout.SetUnderline(
                    true,
                    DWRITE_TEXT_RANGE {
                        startPosition: 0,
                        length,
                    },
                );
            }
        }
        self.draw_layout(&layout, rect, paint);
    }

    /// Draws `glyph` of the icon font, `size` large, in the middle of `rect`.
    pub fn glyph(&self, glyph: char, rect: Rect, size: f32, paint: Color) {
        let Some(format) = self.fonts.icon_format(size) else {
            return;
        };
        let mut text = [0_u8; 4];
        let Some(layout) = self.fonts.layout(
            glyph.encode_utf8(&mut text),
            &format,
            Size::new(rect.w, rect.h),
        ) else {
            return;
        };
        // SAFETY: the layout is live. A setting that fails leaves the glyph
        // a little off, nothing worse.
        unsafe {
            let _ = layout.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
            let _ = layout.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
            let _ = layout.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
        }
        self.draw_layout(&layout, rect, paint);
    }

    /// Draws the logo into `rect`. Returns `false` if there is none.
    #[must_use]
    pub fn logo(&self, rect: Rect) -> bool {
        let Some(logo) = &self.logo else {
            return false;
        };
        // SAFETY: the bitmap is live and the rectangle valid for the call.
        unsafe {
            self.target.DrawBitmap(
                logo,
                Some(&rect_f(self.snapped(rect))),
                1.0,
                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                None,
            );
        }
        true
    }
}

fn stop(position: f32, paint: Color) -> D2D1_GRADIENT_STOP {
    D2D1_GRADIENT_STOP {
        position,
        color: color(paint),
    }
}
