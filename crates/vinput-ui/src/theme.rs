//! Every color, size, radius, gap and type size the windows use, in one
//! place. Nothing else in the crate names a color or a number of pixels.
//!
//! The values are those of Windows 11 itself (Fluent 2, as `WinUI` 3 and the
//! Settings app draw it), in a light and a dark set, plus a set built from
//! the system colors for the high-contrast themes.

use crate::node::{Severity, TextStyle, Tone};

/// A color with opacity, each part from 0 to 1, not premultiplied.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    /// Red.
    pub r: f32,
    /// Green.
    pub g: f32,
    /// Blue.
    pub b: f32,
    /// Opacity.
    pub a: f32,
}

impl Color {
    /// Nothing at all.
    pub const CLEAR: Self = Self::rgba(0, 0.0);
    /// Opaque white.
    pub const WHITE: Self = Self::rgb(0x00FF_FFFF);
    /// Opaque black.
    pub const BLACK: Self = Self::rgb(0);

    /// The opaque color `0xRRGGBB`.
    #[must_use]
    pub const fn rgb(hex: u32) -> Self {
        Self::rgba(hex, 1.0)
    }

    /// The color `0xRRGGBB` at opacity `a`.
    #[must_use]
    #[expect(
        clippy::cast_precision_loss,
        reason = "a color part is at most 255, which a float holds exactly"
    )]
    pub const fn rgba(hex: u32, a: f32) -> Self {
        Self {
            r: ((hex >> 16) & 0xFF) as f32 / 255.0,
            g: ((hex >> 8) & 0xFF) as f32 / 255.0,
            b: (hex & 0xFF) as f32 / 255.0,
            a,
        }
    }

    /// White at opacity `a`.
    #[must_use]
    pub const fn white(a: f32) -> Self {
        Self::rgba(0x00FF_FFFF, a)
    }

    /// Black at opacity `a`.
    #[must_use]
    pub const fn black(a: f32) -> Self {
        Self::rgba(0, a)
    }

    /// The same color at `a` times its opacity.
    #[must_use]
    pub fn faded(self, a: f32) -> Self {
        Self {
            a: self.a * a,
            ..self
        }
    }

    /// What is seen when this color lies over the opaque color `under`.
    #[must_use]
    pub fn over(self, under: Self) -> Self {
        let mix = |top: f32, bottom: f32| top * self.a + bottom * (1.0 - self.a);
        Self {
            r: mix(self.r, under.r),
            g: mix(self.g, under.g),
            b: mix(self.b, under.b),
            a: 1.0,
        }
    }

    /// The color as `0x00BBGGRR`, the way the older parts of Windows take
    /// it. Opacity is dropped.
    #[must_use]
    pub fn colorref(self) -> u32 {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to 0..=255 first"
        )]
        let part = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u32;
        part(self.r) | (part(self.g) << 8) | (part(self.b) << 16)
    }

    /// The opaque color Windows writes as `0x00BBGGRR`.
    #[must_use]
    pub const fn from_colorref(value: u32) -> Self {
        Self::rgb(((value & 0xFF) << 16) | (value & 0xFF00) | ((value >> 16) & 0xFF))
    }

    /// How bright it looks, from 0 to 1.
    #[must_use]
    pub fn luminance(self) -> f32 {
        0.2126 * self.r + 0.7152 * self.g + 0.0722 * self.b
    }
}

/// The accent color of Windows and the lighter and darker shades Windows
/// derives from it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Accent {
    /// The lightest shade.
    pub light3: Color,
    /// Lighter: what fills accent controls in the dark theme.
    pub light2: Color,
    /// A little lighter.
    pub light1: Color,
    /// The color the user picked.
    pub base: Color,
    /// A little darker: what fills accent controls in the light theme.
    pub dark1: Color,
    /// Darker.
    pub dark2: Color,
    /// The darkest shade.
    pub dark3: Color,
}

impl Default for Accent {
    /// The blue Windows 11 comes with.
    fn default() -> Self {
        Self {
            light3: Color::rgb(0x0099_EBFF),
            light2: Color::rgb(0x0060_CDFF),
            light1: Color::rgb(0x0000_91F8),
            base: Color::rgb(0x0000_78D4),
            dark1: Color::rgb(0x0000_5FB8),
            dark2: Color::rgb(0x0000_4275),
            dark3: Color::rgb(0x0000_2642),
        }
    }
}

/// The system colors a high-contrast theme is made of.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SystemColors {
    /// Behind everything.
    pub window: Color,
    /// Text on the window.
    pub text: Color,
    /// Text that cannot be used.
    pub gray_text: Color,
    /// The face of a button.
    pub button: Color,
    /// Text on a button.
    pub button_text: Color,
    /// Behind what is selected.
    pub highlight: Color,
    /// Text on what is selected.
    pub highlight_text: Color,
    /// Links.
    pub hot: Color,
}

/// The colors of one theme, by what they are for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    /// Behind everything. Where Windows can show its wallpaper tint (Mica)
    /// this is only what it falls back to.
    pub window: Color,
    /// The upper part of a small window, over its strip of buttons.
    pub sheet: Color,
    /// A card.
    pub card: Color,
    /// The line around a card.
    pub card_stroke: Color,
    /// A button, a list or a field at rest.
    pub control: Color,
    /// The same under the mouse.
    pub control_hover: Color,
    /// The same held down.
    pub control_pressed: Color,
    /// The same when it cannot be used.
    pub control_disabled: Color,
    /// The line around a control.
    pub control_stroke: Color,
    /// The one edge of that line that is a little stronger, which gives a
    /// control its slight lift: the bottom in the light theme, the top in
    /// the dark one.
    pub control_edge: Color,
    /// The bottom edge of a text field.
    pub field_edge: Color,
    /// A text field that is being typed in. Opaque, so the system's edit
    /// control can sit on it.
    pub field_active: Color,
    /// Behind something without a frame while the mouse is over it, and
    /// behind the selected entry of a list.
    pub subtle_hover: Color,
    /// The same held down.
    pub subtle_pressed: Color,
    /// Ordinary text.
    pub text: Color,
    /// Details and notes.
    pub text_secondary: Color,
    /// Text of what cannot be used.
    pub text_disabled: Color,
    /// What fills accent buttons, switches that are on and progress bars.
    pub accent: Color,
    /// The same under the mouse.
    pub accent_hover: Color,
    /// The same held down.
    pub accent_pressed: Color,
    /// The same when it cannot be used.
    pub accent_disabled: Color,
    /// Text and icons on the accent color.
    pub on_accent: Color,
    /// The same when it cannot be used.
    pub on_accent_disabled: Color,
    /// Links and text in the accent color.
    pub accent_text: Color,
    /// Hairlines between rows.
    pub divider: Color,
    /// It worked.
    pub success: Color,
    /// Needs attention.
    pub caution: Color,
    /// It failed.
    pub critical: Color,
    /// Behind a message that something worked.
    pub bar_success: Color,
    /// Behind a message that needs attention.
    pub bar_caution: Color,
    /// Behind a message that something failed.
    pub bar_critical: Color,
    /// Behind a message for information.
    pub bar_info: Color,
    /// A drop-down list or a tooltip.
    pub flyout: Color,
    /// The line around one, where Windows does not draw it.
    pub flyout_stroke: Color,
    /// The outline of a switch that is off, and the track of a progress bar.
    pub strong_stroke: Color,
    /// Inside a switch that is off.
    pub switch_off: Color,
    /// The knob of a switch that is off.
    pub knob: Color,
    /// The outer ring around what has the keyboard focus.
    pub focus_outer: Color,
    /// The inner ring.
    pub focus_inner: Color,
    /// The thumb of the scroll bar.
    pub scroll_thumb: Color,
}

impl Palette {
    /// The dark theme.
    #[must_use]
    pub fn dark(accent: &Accent) -> Self {
        Self {
            window: Color::rgb(0x0020_2020),
            sheet: Color::rgb(0x002B_2B2B),
            card: Color::white(0.05),
            card_stroke: Color::black(0.10),
            control: Color::white(0.06),
            control_hover: Color::white(0.08),
            control_pressed: Color::white(0.03),
            control_disabled: Color::white(0.04),
            control_stroke: Color::white(0.07),
            control_edge: Color::white(0.09),
            field_edge: Color::white(0.54),
            field_active: Color::rgb(0x001F_1F1F),
            subtle_hover: Color::white(0.06),
            subtle_pressed: Color::white(0.04),
            text: Color::WHITE,
            text_secondary: Color::white(0.77),
            text_disabled: Color::white(0.36),
            accent: accent.light2,
            accent_hover: accent.light2.faded(0.9),
            accent_pressed: accent.light2.faded(0.8),
            accent_disabled: Color::white(0.16),
            on_accent: Color::BLACK,
            on_accent_disabled: Color::white(0.53),
            accent_text: accent.light3,
            divider: Color::white(0.08),
            success: Color::rgb(0x006C_CB5F),
            caution: Color::rgb(0x00FC_E100),
            critical: Color::rgb(0x00FF_99A4),
            bar_success: Color::rgb(0x0039_3D1B),
            bar_caution: Color::rgb(0x0043_3519),
            bar_critical: Color::rgb(0x0044_2726),
            bar_info: Color::white(0.03),
            flyout: Color::rgb(0x002C_2C2C),
            flyout_stroke: Color::black(0.20),
            strong_stroke: Color::white(0.54),
            switch_off: Color::black(0.10),
            knob: Color::white(0.77),
            focus_outer: Color::WHITE,
            focus_inner: Color::black(0.70),
            scroll_thumb: Color::white(0.54),
        }
    }

    /// The light theme.
    #[must_use]
    pub fn light(accent: &Accent) -> Self {
        Self {
            window: Color::rgb(0x00F3_F3F3),
            sheet: Color::WHITE,
            card: Color::white(0.70),
            card_stroke: Color::black(0.06),
            control: Color::white(0.70),
            control_hover: Color::rgba(0x00F9_F9F9, 0.50),
            control_pressed: Color::rgba(0x00F9_F9F9, 0.30),
            control_disabled: Color::rgba(0x00F9_F9F9, 0.30),
            control_stroke: Color::black(0.06),
            control_edge: Color::black(0.16),
            field_edge: Color::black(0.45),
            field_active: Color::WHITE,
            subtle_hover: Color::black(0.04),
            subtle_pressed: Color::black(0.02),
            text: Color::black(0.89),
            text_secondary: Color::black(0.62),
            text_disabled: Color::black(0.36),
            accent: accent.dark1,
            accent_hover: accent.dark1.faded(0.9),
            accent_pressed: accent.dark1.faded(0.8),
            accent_disabled: Color::black(0.22),
            on_accent: Color::WHITE,
            on_accent_disabled: Color::WHITE,
            accent_text: accent.dark2,
            divider: Color::black(0.08),
            success: Color::rgb(0x000F_7B0F),
            caution: Color::rgb(0x009D_5D00),
            critical: Color::rgb(0x00C4_2B1C),
            bar_success: Color::rgb(0x00DF_F6DD),
            bar_caution: Color::rgb(0x00FF_F4CE),
            bar_critical: Color::rgb(0x00FD_E7E9),
            bar_info: Color::white(0.50),
            flyout: Color::rgb(0x00F9_F9F9),
            flyout_stroke: Color::black(0.06),
            strong_stroke: Color::black(0.45),
            switch_off: Color::black(0.02),
            knob: Color::black(0.62),
            focus_outer: Color::black(0.89),
            focus_inner: Color::WHITE,
            scroll_thumb: Color::black(0.45),
        }
    }

    /// A high-contrast theme: only the colors the user chose, no shades.
    #[must_use]
    pub fn high_contrast(system: &SystemColors) -> Self {
        Self {
            window: system.window,
            sheet: system.window,
            card: system.window,
            card_stroke: system.text,
            control: system.button,
            control_hover: system.button,
            control_pressed: system.button,
            control_disabled: system.window,
            control_stroke: system.button_text,
            control_edge: system.button_text,
            field_edge: system.button_text,
            field_active: system.window,
            subtle_hover: Color::CLEAR,
            subtle_pressed: Color::CLEAR,
            text: system.text,
            text_secondary: system.text,
            text_disabled: system.gray_text,
            accent: system.highlight,
            accent_hover: system.highlight,
            accent_pressed: system.highlight,
            accent_disabled: system.window,
            on_accent: system.highlight_text,
            on_accent_disabled: system.gray_text,
            accent_text: system.hot,
            divider: system.text,
            success: system.text,
            caution: system.text,
            critical: system.text,
            bar_success: system.window,
            bar_caution: system.window,
            bar_critical: system.window,
            bar_info: system.window,
            flyout: system.window,
            flyout_stroke: system.text,
            strong_stroke: system.text,
            switch_off: system.window,
            knob: system.text,
            focus_outer: system.text,
            focus_inner: system.window,
            scroll_thumb: system.text,
        }
    }

    /// The color text or an icon of this tone has.
    #[must_use]
    pub fn tone(&self, tone: Tone) -> Color {
        match tone {
            Tone::Primary => self.text,
            Tone::Secondary => self.text_secondary,
            Tone::Disabled => self.text_disabled,
            Tone::Accent => self.accent_text,
            Tone::Success => self.success,
            Tone::Caution => self.caution,
            Tone::Critical => self.critical,
        }
    }

    /// The color behind a message bar, and the color of its icon.
    #[must_use]
    pub fn severity(&self, severity: Severity) -> (Color, Color) {
        match severity {
            Severity::Info => (self.bar_info, self.accent),
            Severity::Success => (self.bar_success, self.success),
            Severity::Caution => (self.bar_caution, self.caution),
            Severity::Critical => (self.bar_critical, self.critical),
        }
    }
}

/// How the windows look: the colors, and what the user asked of Windows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    /// Whether this is a dark theme.
    pub dark: bool,
    /// Whether a high-contrast theme is on: no shades, lines around
    /// everything.
    pub contrast: bool,
    /// The colors.
    pub palette: Palette,
    /// How much larger than usual the user wants text, from 1 to 2.25.
    pub text_scale: f32,
}

impl Theme {
    /// The dark theme with the accent color Windows comes with.
    #[must_use]
    pub fn dark() -> Self {
        Self::of(true, &Accent::default())
    }

    /// The light theme with the accent color Windows comes with.
    #[must_use]
    pub fn light() -> Self {
        Self::of(false, &Accent::default())
    }

    /// The dark or the light theme around `accent`.
    #[must_use]
    pub fn of(dark: bool, accent: &Accent) -> Self {
        Self {
            dark,
            contrast: false,
            palette: if dark {
                Palette::dark(accent)
            } else {
                Palette::light(accent)
            },
            text_scale: 1.0,
        }
    }

    /// A high-contrast theme of the kind Windows comes with, white and
    /// bright colors on black, for looking at how a window holds up in one
    /// without switching Windows over.
    #[must_use]
    pub fn contrast_sample() -> Self {
        Self::high_contrast(&SystemColors {
            window: Color::BLACK,
            text: Color::WHITE,
            gray_text: Color::rgb(0x003F_F23F),
            button: Color::BLACK,
            button_text: Color::WHITE,
            highlight: Color::rgb(0x001A_EBFF),
            highlight_text: Color::BLACK,
            hot: Color::rgb(0x00FF_FF00),
        })
    }

    /// A high-contrast theme made of `system`.
    #[must_use]
    pub fn high_contrast(system: &SystemColors) -> Self {
        Self {
            dark: system.window.luminance() < 0.5,
            contrast: true,
            palette: Palette::high_contrast(system),
            text_scale: 1.0,
        }
    }
}

/// One step of the type ramp.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Face {
    /// The size of the type.
    pub size: f32,
    /// The height of a line of it.
    pub line: f32,
    /// Whether it is semibold rather than regular.
    pub semibold: bool,
    /// The font it is set in.
    pub family: Family,
}

/// The fonts text is set in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// The font of Windows, in its cut for reading sizes.
    Text,
    /// The same in its cut for large sizes.
    Display,
    /// A font where every character is as wide as the next.
    Mono,
}

/// The type a [`TextStyle`] is set in: the Windows 11 type ramp.
#[must_use]
pub const fn face(style: TextStyle) -> Face {
    let (size, line, semibold, family) = match style {
        TextStyle::Caption => (12.0, 16.0, false, Family::Text),
        TextStyle::Body => (14.0, 20.0, false, Family::Text),
        TextStyle::BodyStrong => (14.0, 20.0, true, Family::Text),
        TextStyle::Subtitle => (20.0, 28.0, true, Family::Display),
        TextStyle::Title => (28.0, 36.0, true, Family::Display),
        TextStyle::Mono => (13.0, 20.0, false, Family::Mono),
    };
    Face {
        size,
        line,
        semibold,
        family,
    }
}

/// The fonts, in the order they are tried: what Windows 11 has, then what
/// Windows 10 has.
pub mod font {
    /// Text up to the size of a body line.
    pub const TEXT: [&str; 2] = ["Segoe UI Variable Text", "Segoe UI"];
    /// Titles.
    pub const DISPLAY: [&str; 2] = ["Segoe UI Variable Display", "Segoe UI"];
    /// Text where every character is as wide as the next.
    pub const MONO: [&str; 2] = ["Cascadia Mono", "Consolas"];
    /// Icons.
    pub const ICONS: [&str; 2] = ["Segoe Fluent Icons", "Segoe MDL2 Assets"];
    /// The weight of regular type.
    pub const REGULAR: i32 = 400;
    /// The weight of semibold type.
    pub const SEMIBOLD: i32 = 600;
}

/// Sizes, gaps and radii, in device-independent units. Everything sits on a
/// grid of four.
pub mod metric {
    /// The grid.
    pub const GRID: f32 = 4.0;
    /// The gap between things side by side or one under another.
    pub const GAP: f32 = 8.0;
    /// The extra room over a heading that is not the first thing on a page.
    pub const HEADER_ABOVE: f32 = 16.0;
    /// The room under a heading, on top of the usual gap.
    pub const HEADER_BELOW: f32 = 0.0;
    /// How round controls and cards are.
    pub const RADIUS: f32 = 4.0;
    /// How round pop-ups and the content sheet are.
    pub const RADIUS_FLYOUT: f32 = 8.0;
    /// The width of hairlines and outlines.
    pub const LINE: f32 = 1.0;

    /// The height of a button, a list or a field.
    pub const CONTROL: f32 = 32.0;
    /// The room between the edge of a button and its caption.
    pub const BUTTON_PAD: f32 = 12.0;
    /// The narrowest a button with a caption gets.
    pub const BUTTON_MIN: f32 = 72.0;
    /// The narrowest a button in the strip at the bottom of a window gets.
    pub const BUTTON_MIN_FOOTER: f32 = 96.0;
    /// The size of an icon inside a control or on its own.
    pub const ICON: f32 = 16.0;
    /// The size of the icon of a settings row.
    pub const ICON_ROW: f32 = 20.0;
    /// The size of the small glyphs: the chevron of a list.
    pub const ICON_SMALL: f32 = 12.0;
    /// The width of a field or a list that was not asked to be wide.
    pub const FIELD: f32 = 240.0;
    /// The narrowest a field or a list gets.
    pub const FIELD_MIN: f32 = 120.0;
    /// The widest a list gets by itself.
    pub const CHOICE_MAX: f32 = 320.0;
    /// The width of a hotkey field.
    pub const HOTKEY: f32 = 180.0;
    /// The room between the edge of a field and its text.
    pub const FIELD_PAD: f32 = 11.0;
    /// The thickness of the accent line under a field that has the focus.
    pub const FIELD_FOCUS: f32 = 2.0;
    /// How far the system's edit control reaches into the padding of the
    /// field it lies over, so that a letter which starts a little before its
    /// place is not cut off. Less than [`FIELD_PAD`].
    pub const EDIT_BLEED: f32 = 6.0;

    /// The width of a switch.
    pub const SWITCH_W: f32 = 40.0;
    /// The height of a switch.
    pub const SWITCH_H: f32 = 20.0;
    /// The width of the knob of a switch at rest.
    pub const KNOB: f32 = 12.0;
    /// The width of the knob under the mouse.
    pub const KNOB_HOVER: f32 = 14.0;
    /// The room between a switch and the word beside it.
    pub const SWITCH_GAP: f32 = 12.0;

    /// The least height of a settings row.
    pub const ROW_MIN: f32 = 68.0;
    /// The room around what is in a row or a card.
    pub const ROW_PAD: f32 = 16.0;
    /// The least room over and under the text of a row.
    pub const ROW_PAD_Y: f32 = 12.0;
    /// The narrowest the text of a row gets before its control gives way.
    pub const ROW_TEXT_MIN: f32 = 140.0;
    /// The least width text shrinks to where room is short.
    pub const TEXT_MIN: f32 = 48.0;
    /// The width of the accent pill that marks what is selected.
    pub const PILL_W: f32 = 3.0;
    /// The height of that pill.
    pub const PILL_H: f32 = 16.0;

    /// The height of a progress bar's box.
    pub const PROGRESS_H: f32 = 4.0;
    /// The thickness of the bar itself.
    pub const PROGRESS_BAR: f32 = 3.0;
    /// The narrowest a progress bar gets in a row.
    pub const PROGRESS_MIN: f32 = 80.0;
    /// How much of its track a progress bar that moves by itself covers.
    pub const PROGRESS_SWEEP: f32 = 0.4;
    /// The height of a badge.
    pub const BADGE_H: f32 = 20.0;
    /// The room between the edge of a badge and its text.
    pub const BADGE_PAD: f32 = 8.0;
    /// How strongly a badge is tinted with the color of its text.
    pub const BADGE_TINT: f32 = 0.14;
    /// The least height of a message bar.
    pub const BAR_MIN: f32 = 48.0;
    /// The room over and under the text of a message bar.
    pub const BAR_PAD_Y: f32 = 14.0;
    /// The room between the icon of a message bar and its text.
    pub const BAR_ICON_GAP: f32 = 12.0;

    /// The width of the navigation pane.
    pub const PANE: f32 = 260.0;
    /// The height of an entry in the navigation pane.
    pub const NAV_ITEM: f32 = 36.0;
    /// The room between the entries, and between them and the pane's edge.
    pub const NAV_GAP: f32 = 4.0;
    /// The room left and right of the entries.
    pub const NAV_MARGIN: f32 = 8.0;
    /// Where the icon of an entry starts, from the entry's left edge.
    pub const NAV_ICON_X: f32 = 14.0;
    /// Where the name of an entry starts, from the entry's left edge.
    pub const NAV_TEXT_X: f32 = 44.0;
    /// The height of the strip with the logo and the name of the app.
    pub const NAV_HEAD: f32 = 56.0;
    /// Where the logo starts, from the left edge of the pane.
    pub const NAV_LOGO_X: f32 = 18.0;
    /// The size of the logo.
    pub const LOGO: f32 = 24.0;
    /// The size of the dot that says whether Vinput is listening.
    pub const DOT: f32 = 8.0;

    /// The room left and right of a page.
    pub const PAGE_PAD: f32 = 32.0;
    /// The room over the title of a page.
    pub const PAGE_TOP: f32 = 20.0;
    /// The room between the title and what scrolls under it.
    pub const TITLE_GAP: f32 = 16.0;
    /// The room between what is pinned under the title and what scrolls
    /// under that.
    pub const PINNED_GAP: f32 = 24.0;
    /// The room under the last thing on a page.
    pub const PAGE_BOTTOM: f32 = 24.0;
    /// The widest a page gets.
    pub const PAGE_MAX: f32 = 1000.0;
    /// The room around a small window's content.
    pub const DIALOG_PAD: f32 = 24.0;
    /// The room between the heading of a small window and what is under it.
    pub const DIALOG_GAP: f32 = 12.0;
    /// The room around the buttons at the bottom of a window.
    pub const FOOTER_PAD: f32 = 16.0;

    /// The width of the scroll bar's thumb while the mouse is elsewhere on
    /// the page.
    pub const SCROLL_THIN: f32 = 2.0;
    /// Its width while the mouse is over it.
    pub const SCROLL_WIDE: f32 = 6.0;
    /// The width of the strip at the right edge that belongs to it.
    pub const SCROLL_LANE: f32 = 12.0;
    /// The shortest the thumb gets.
    pub const SCROLL_MIN: f32 = 32.0;
    /// How far one notch of the mouse wheel scrolls.
    pub const WHEEL: f32 = 96.0;

    /// The thickness of each of the two rings around what has the focus.
    pub const FOCUS: f32 = 2.0;
    /// How far outside a control its focus rings start.
    pub const FOCUS_GAP: f32 = 1.0;

    /// The height of an entry in a drop-down list.
    pub const MENU_ITEM: f32 = 36.0;
    /// The room around the entries of a drop-down list.
    pub const MENU_PAD: f32 = 4.0;
    /// The most entries a drop-down list shows before it scrolls.
    pub const MENU_ROWS: usize = 12;
    /// The room around the text of a tooltip, sideways.
    pub const TIP_PAD_X: f32 = 9.0;
    /// The room around the text of a tooltip, up and down.
    pub const TIP_PAD_Y: f32 = 6.0;
    /// The widest a tooltip gets.
    pub const TIP_MAX: f32 = 320.0;

    /// The size of the Settings window when it first opens.
    pub const SETTINGS: (f32, f32) = (940.0, 660.0);
    /// The smallest the Settings window gets.
    pub const SETTINGS_MIN: (f32, f32) = (720.0, 440.0);
    /// The size of the update window.
    pub const UPDATE: (f32, f32) = (480.0, 432.0);
    /// The width of a window that asks or tells.
    pub const DIALOG_W: f32 = 460.0;
    /// The least height of one.
    pub const DIALOG_MIN_H: f32 = 180.0;
    /// The most height of one before its content scrolls.
    pub const DIALOG_MAX_H: f32 = 560.0;
}

/// How much of its opacity something keeps in a passing state.
pub mod shade {
    /// A link while it is held down.
    pub const LINK_PRESSED: f32 = 0.8;
    /// The caption of an accent button while it is held down.
    pub const ON_ACCENT_PRESSED: f32 = 0.7;
}

/// How long things take, in milliseconds.
pub mod timing {
    /// Between two frames of a progress bar that moves by itself.
    pub const FRAME: u32 = 16;
    /// One sweep of such a bar.
    pub const SWEEP: u32 = 1800;
    /// How long the mouse rests on a button before its tooltip shows.
    pub const TIP_DELAY: u32 = 600;
    /// How long after a letter typed into a drop-down the next one still
    /// belongs to the same word.
    pub const TYPE_AHEAD: u32 = 1000;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Contrast ratio of two opaque colors, after WCAG.
    fn contrast(a: Color, b: Color) -> f32 {
        let linear = |c: f32| {
            if c <= 0.039_28 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        let light =
            |c: Color| 0.2126 * linear(c.r) + 0.7152 * linear(c.g) + 0.0722 * linear(c.b) + 0.05;
        let (a, b) = (light(a), light(b));
        a.max(b) / a.min(b)
    }

    #[test]
    fn text_can_be_read_on_what_it_sits_on() {
        for theme in [Theme::dark(), Theme::light()] {
            let colors = theme.palette;
            let card = colors.card.over(colors.window);
            for (text, least) in [
                (colors.text, 7.0),
                (colors.text_secondary, 4.5),
                (colors.accent_text, 4.5),
                (colors.success, 4.5),
                (colors.critical, 4.5),
            ] {
                let seen = text.over(card);
                assert!(
                    contrast(seen, card) >= least,
                    "dark={} {text:?} on {card:?}",
                    theme.dark
                );
            }
            let accent = colors.accent.over(card);
            assert!(contrast(colors.on_accent.over(accent), accent) >= 4.5);
            for (bar, _) in [
                colors.severity(Severity::Success),
                colors.severity(Severity::Caution),
                colors.severity(Severity::Critical),
                colors.severity(Severity::Info),
            ] {
                let bar = bar.over(colors.window);
                assert!(contrast(colors.text.over(bar), bar) >= 7.0);
            }
        }
    }

    #[test]
    fn the_themes_are_what_they_say() {
        assert!(Theme::dark().palette.window.luminance() < 0.2);
        assert!(Theme::light().palette.window.luminance() > 0.8);
        assert!((Theme::dark().palette.field_active.a - 1.0).abs() < f32::EPSILON);
        assert!((Theme::light().palette.field_active.a - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn colors_convert_both_ways() {
        let orange = Color::rgb(0x00FF_8000);
        assert_eq!(orange.colorref(), 0x0000_80FF);
        assert_eq!(Color::from_colorref(0x0000_80FF), orange);
        let half = Color::white(0.5).over(Color::BLACK);
        assert!((half.r - 0.5).abs() < 0.01 && (half.a - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn sizes_sit_on_the_grid() {
        for size in [
            metric::GAP,
            metric::CONTROL,
            metric::ROW_MIN,
            metric::ROW_PAD,
            metric::NAV_ITEM,
            metric::PAGE_PAD,
            metric::SWITCH_W,
            metric::SWITCH_H,
        ] {
            assert!((size % metric::GRID).abs() < f32::EPSILON, "{size}");
        }
    }

    #[test]
    fn high_contrast_uses_only_the_system_colors() {
        let system = SystemColors {
            window: Color::BLACK,
            text: Color::WHITE,
            gray_text: Color::rgb(0x003F_F23F),
            button: Color::BLACK,
            button_text: Color::WHITE,
            highlight: Color::rgb(0x001A_EBFF),
            highlight_text: Color::BLACK,
            hot: Color::rgb(0x00FF_FF00),
        };
        let theme = Theme::high_contrast(&system);
        assert!(theme.dark && theme.contrast);
        assert_eq!(theme.palette.text, Color::WHITE);
        assert_eq!(theme.palette.accent, system.highlight);
    }
}
