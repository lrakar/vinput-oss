//! Pixel rendering for the listening indicator and the fallback tray icon.
//!
//! Everything here is plain arithmetic on a pixel buffer, independent of Windows,
//! so the artwork can be tested and previewed without a screen. Pixels are
//! premultiplied `0xAARRGGBB`, which is the layout Windows expects for 32-bit
//! bitmaps with alpha.
//!
//! Shapes are rounded rectangles described by their signed distance function,
//! so the coverage of a pixel is computed rather than sampled: edges are as
//! smooth as they can be at any scale, and a pixel costs one evaluation.
//!
//! The indicator is drawn in two steps. [`Artwork::new`] paints what never
//! changes, the capsule with its shadow and hairline, once per scale.
//! [`Artwork::paint`] then copies that and adds the bars, which is all a frame
//! costs: it allocates nothing and touches a few hundred pixels.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap,
    reason = "pixel coordinates are small and channel values are clamped to 0..=255"
)]

/// An opaque color as (red, green, blue).
pub type Rgb = (u8, u8, u8);

const WHITE: Rgb = (255, 255, 255);
const BLACK: Rgb = (0, 0, 0);
const ICON_IDLE: Rgb = (91, 91, 214);
const ICON_LISTENING: Rgb = (229, 72, 77);

/// The capsule of the indicator: opaque, so that text under it never shows
/// through, and dark in both of Windows' modes, like the system's own overlays.
const CAPSULE: Rgb = (32, 32, 34);
/// The hairline just inside the capsule's edge, which sets it off from a dark
/// background the way the shadow sets it off from a light one.
const HAIRLINE_ALPHA: f32 = 0.16;
const SHADOW_ALPHA: f32 = 0.22;
/// How bright a bar at rest is; a bar at full height is fully white.
const BAR_REST_ALPHA: f32 = 0.55;

/// The accent the indicator uses where Windows does not say what the user's is:
/// the light blue Windows 11 draws on dark surfaces by default.
pub const DEFAULT_ACCENT: Rgb = (76, 194, 255);

/// Number of bars in the indicator.
pub const INDICATOR_BARS: usize = 7;

/// An axis-aligned rectangle with rounded corners. With a radius of half its
/// shorter side it is a capsule, and a circle if it is square as well.
#[derive(Debug, Clone, Copy)]
struct RoundedRect {
    center: (f32, f32),
    half_size: (f32, f32),
    radius: f32,
}

impl RoundedRect {
    /// A rectangle from its left, top, width and height.
    fn new(left: f32, top: f32, width: f32, height: f32, radius: f32) -> Self {
        Self {
            center: (left + width / 2.0, top + height / 2.0),
            half_size: (width / 2.0, height / 2.0),
            radius,
        }
    }

    /// Distance from the edge to a point, in pixels: negative inside.
    fn distance(&self, x: f32, y: f32) -> f32 {
        let radius = self.radius.min(self.half_size.0).min(self.half_size.1);
        let dx = (x - self.center.0).abs() - (self.half_size.0 - radius);
        let dy = (y - self.center.1).abs() - (self.half_size.1 - radius);
        let outside = dx.max(0.0).hypot(dy.max(0.0));
        outside + dx.max(dy).min(0.0) - radius
    }

    /// The whole pixels this shape, grown by `margin`, can touch, clipped to a
    /// buffer of `width` by `height`: left, top, right and bottom, the last two
    /// exclusive.
    fn bounds(&self, margin: f32, width: u32, height: u32) -> (u32, u32, u32, u32) {
        let clip = |value: f32, limit: u32| value.clamp(0.0, limit as f32) as u32;
        (
            clip((self.center.0 - self.half_size.0 - margin).floor(), width),
            clip((self.center.1 - self.half_size.1 - margin).floor(), height),
            clip((self.center.0 + self.half_size.0 + margin).ceil(), width),
            clip((self.center.1 + self.half_size.1 + margin).ceil(), height),
        )
    }
}

/// How much of a pixel lies inside an edge that is `distance` away from the
/// pixel's center.
fn coverage(distance: f32) -> f32 {
    (0.5 - distance).clamp(0.0, 1.0)
}

/// Paints `color` over a premultiplied pixel with the given opacity.
fn blend(under: u32, color: Rgb, alpha: f32) -> u32 {
    let keep = 1.0 - alpha;
    let mix = |shift: u32, value: u8| {
        let below = ((under >> shift) & 0xFF) as f32;
        (f32::from(value) * alpha + below * keep)
            .round()
            .clamp(0.0, 255.0) as u32
    };
    let below = (under >> 24) as f32;
    let a = (255.0 * alpha + below * keep).round().clamp(0.0, 255.0) as u32;
    a << 24 | mix(16, color.0) << 16 | mix(8, color.1) << 8 | mix(0, color.2)
}

/// A pixel buffer being painted on, `width` pixels to a row.
struct Canvas<'a> {
    pixels: &'a mut [u32],
    width: u32,
    height: u32,
}

impl Canvas<'_> {
    /// Paints `color` wherever `opacity`, which is given the distance from the
    /// edge of `shape`, says so. Only pixels within `reach` of the shape are
    /// looked at.
    fn paint(&mut self, shape: &RoundedRect, reach: f32, color: Rgb, opacity: impl Fn(f32) -> f32) {
        let (left, top, right, bottom) = shape.bounds(reach, self.width, self.height);
        for y in top..bottom {
            let row = (y * self.width) as usize;
            for x in left..right {
                let alpha = opacity(shape.distance(x as f32 + 0.5, y as f32 + 0.5));
                if alpha > 0.0
                    && let Some(pixel) = self.pixels.get_mut(row + x as usize)
                {
                    *pixel = blend(*pixel, color, alpha);
                }
            }
        }
    }

    /// Fills `shape` with `color` at the given opacity.
    fn fill(&mut self, shape: &RoundedRect, color: Rgb, alpha: f32) {
        self.paint(shape, 1.0, color, |distance| alpha * coverage(distance));
    }
}

/// Draws the square tray icon used when the executable carries no icons of
/// its own: three white sound bars on a rounded tile, indigo when idle and
/// red while listening.
#[must_use]
pub fn icon(size: u32, listening: bool) -> Vec<u32> {
    let mut pixels = vec![0; (size * size) as usize];
    let mut canvas = Canvas {
        pixels: &mut pixels,
        width: size,
        height: size,
    };
    let s = size as f32;
    let tile = RoundedRect::new(s * 0.03, s * 0.03, s * 0.94, s * 0.94, s * 0.24);
    let color = if listening { ICON_LISTENING } else { ICON_IDLE };
    canvas.fill(&tile, color, 1.0);
    // Bars are a whole number of pixels wide and start on a pixel, so that
    // they stay sharp on the smallest icons. Their sizes are whole numbers as
    // odd or even as the icon is, so that the middle one can be centered.
    let odd = s % 2.0;
    let whole = |share: f32| (((s * share - odd) / 2.0).round() * 2.0 + odd).max(1.0);
    let width = whole(0.14);
    let pitch = (s * 0.22).round().max(width + 1.0);
    let middle = (s - width) / 2.0;
    for (step, height) in [(-1.0, 0.28), (0.0, 0.56), (1.0, 0.28)] {
        let height = whole(height);
        let bar = RoundedRect::new(
            middle + step * pitch,
            (s - height) / 2.0,
            width,
            height,
            width / 2.0,
        );
        canvas.fill(&bar, WHITE, 1.0);
    }
    pixels
}

/// Converts a premultiplied pixel to straight alpha, which icons use.
#[must_use]
pub fn unpremultiply(pixel: u32) -> u32 {
    let alpha = pixel >> 24;
    if alpha == 0 {
        return 0;
    }
    let channel = |shift: u32| (((pixel >> shift) & 0xFF) * 255 / alpha).min(255) << shift;
    alpha << 24 | channel(16) | channel(8) | channel(0)
}

/// One bar of the indicator.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Bar {
    /// From 0.0, a dot, to 1.0, the tallest the capsule holds.
    pub height: f32,
    /// From 0.0, white, to 1.0, the accent color at full strength.
    pub glow: f32,
}

/// The bars as they are while listening: white, and brighter the taller.
#[must_use]
pub fn listening(levels: &[f32; INDICATOR_BARS]) -> [Bar; INDICATOR_BARS] {
    levels.map(|height| Bar { height, glow: 0.0 })
}

/// The bars as they are while the last words are still being typed: what is
/// left of `levels` settling into a row of dots, along which a soft glow in
/// the accent color travels. `phase` says where the glow is, and one sweep
/// takes it from 0.0 to 1.0; it repeats from there.
#[must_use]
pub fn finishing(levels: &[f32; INDICATOR_BARS], phase: f32) -> [Bar; INDICATOR_BARS] {
    /// How far the glow reaches, in bars.
    const REACH: f32 = 1.6;
    /// How much a dot swells as the glow passes.
    const SWELL: f32 = 0.22;
    let bars = INDICATOR_BARS as f32;
    // The sweep starts and ends beyond the row, so the glow enters and leaves.
    let at = phase.rem_euclid(1.0) * (bars + 2.0 * REACH) - REACH - 0.5;
    std::array::from_fn(|index| {
        let away = (index as f32 - at) / REACH;
        let glow = (-away * away * 2.0).exp();
        Bar {
            height: levels[index].max(glow * SWELL),
            glow,
        }
    })
}

/// The sizes of everything in the indicator at one scale, in whole pixels so
/// that edges fall on pixel boundaries wherever they can.
#[derive(Debug, Clone, Copy)]
struct Geometry {
    /// Size of the bitmap, which holds the capsule and its shadow.
    size: (u32, u32),
    /// The capsule within the bitmap: left, top, width, height.
    capsule: (i32, i32, i32, i32),
    bar_width: i32,
    /// Distance from one bar to the next.
    pitch: i32,
    /// How much taller than a dot the tallest bar is.
    bar_growth: i32,
    /// How far the shadow spreads.
    blur: i32,
    /// How far below the capsule its shadow lies.
    drop: i32,
    /// Thickness of the hairline.
    line: f32,
}

impl Geometry {
    fn new(scale: f32) -> Self {
        let scale = scale.clamp(0.5, 16.0);
        let px = |value: f32| (value * scale).round().max(1.0) as i32;
        let bar_width = px(3.0);
        let pitch = bar_width + px(2.0);
        // Twice a whole number, so that every height in between keeps the
        // bars centered on the same row as the dots.
        let bar_growth = 2 * px(4.5);
        let (pad_x, pad_y) = (px(9.0), px(4.5));
        let width = (INDICATOR_BARS as i32 - 1) * pitch + bar_width + 2 * pad_x;
        let height = bar_width + bar_growth + 2 * pad_y;
        let (blur, drop) = (px(6.0), px(2.0));
        Self {
            size: ((width + 2 * blur) as u32, (height + 2 * blur + drop) as u32),
            capsule: (blur, blur, width, height),
            bar_width,
            pitch,
            bar_growth,
            blur,
            drop,
            line: scale.max(1.0),
        }
    }

    fn capsule(&self) -> RoundedRect {
        let (left, top, width, height) = self.capsule;
        RoundedRect::new(
            left as f32,
            top as f32,
            width as f32,
            height as f32,
            height as f32 / 2.0,
        )
    }

    /// The bar at `index`, counted from the left, at a height from 0.0 to 1.0.
    fn bar(&self, index: usize, height: f32) -> RoundedRect {
        let (left, top, width, capsule_height) = self.capsule;
        let span = (INDICATOR_BARS as i32 - 1) * self.pitch + self.bar_width;
        let x = left + (width - span) / 2 + index as i32 * self.pitch;
        let tall = self.bar_width as f32 + self.bar_growth as f32 * height.clamp(0.0, 1.0);
        RoundedRect::new(
            x as f32,
            top as f32 + (capsule_height as f32 - tall) / 2.0,
            self.bar_width as f32,
            tall,
            self.bar_width as f32 / 2.0,
        )
    }
}

/// The indicator at one scale, ready to be painted frame after frame.
#[derive(Debug)]
pub struct Artwork {
    geometry: Geometry,
    accent: Rgb,
    /// The capsule with its shadow and hairline, which every frame starts from.
    base: Vec<u32>,
}

impl Artwork {
    /// Prepares the indicator at `scale` (1.0 is 96 DPI), with `accent` as the
    /// color of the glow.
    #[must_use]
    pub fn new(scale: f32, accent: Rgb) -> Self {
        let geometry = Geometry::new(scale);
        let (width, height) = geometry.size;
        let mut base = vec![0; (width * height) as usize];
        let mut canvas = Canvas {
            pixels: &mut base,
            width,
            height,
        };
        let capsule = geometry.capsule();
        let blur = geometry.blur as f32;
        let shadow = RoundedRect {
            center: (capsule.center.0, capsule.center.1 + geometry.drop as f32),
            ..capsule
        };
        canvas.paint(&shadow, blur, BLACK, |distance| {
            let fade = 1.0 - (distance / blur).clamp(0.0, 1.0);
            SHADOW_ALPHA * fade * fade
        });
        canvas.fill(&capsule, CAPSULE, 1.0);
        let line = geometry.line;
        canvas.paint(&capsule, 1.0, WHITE, |distance| {
            HAIRLINE_ALPHA * (coverage(distance) - coverage(distance + line))
        });
        Self {
            geometry,
            accent,
            base,
        }
    }

    /// Size of the bitmap in pixels: the capsule and the shadow around it.
    #[must_use]
    pub fn size(&self) -> (u32, u32) {
        self.geometry.size
    }

    /// Where the capsule itself is within the bitmap: left, top, width and
    /// height. This is what is lined up with the caret; the shadow hangs over.
    #[must_use]
    pub fn capsule(&self) -> (i32, i32, i32, i32) {
        self.geometry.capsule
    }

    /// Paints a frame into `pixels`, which holds as many pixels as
    /// [`Artwork::size`] says. A buffer of any other length is left alone.
    pub fn paint(&self, bars: &[Bar; INDICATOR_BARS], pixels: &mut [u32]) {
        if pixels.len() != self.base.len() {
            return;
        }
        pixels.copy_from_slice(&self.base);
        let (width, height) = self.geometry.size;
        let mut canvas = Canvas {
            pixels,
            width,
            height,
        };
        for (index, bar) in bars.iter().enumerate() {
            let height = bar.height.clamp(0.0, 1.0);
            let glow = bar.glow.clamp(0.0, 1.0);
            let rest = BAR_REST_ALPHA + (1.0 - BAR_REST_ALPHA) * height;
            let towards = |accent: u8| (255.0 + (f32::from(accent) - 255.0) * glow).round() as u8;
            let color = (
                towards(self.accent.0),
                towards(self.accent.1),
                towards(self.accent.2),
            );
            let shape = self.geometry.bar(index, height);
            canvas.fill(&shape, color, rest + (1.0 - rest) * glow);
        }
    }
}

/// Size in pixels of the indicator's bitmap at a given scale (1.0 is 96 DPI).
#[must_use]
pub fn indicator_size(scale: f32) -> (u32, u32) {
    Geometry::new(scale).size
}

/// How many bars the loudness spreads over on each side of the middle one.
const REACH: usize = INDICATOR_BARS / 2;
/// How many redraws the loudness takes to travel from one bar to the next.
const TRAVEL: f32 = 1.5;
/// How many past levels are kept: enough for the outermost bar.
const HISTORY: usize = 6;
const _: () = assert!(HISTORY as f32 >= REACH as f32 * TRAVEL + 1.0);
/// How much of the way to a louder level the bars rise in one redraw, and how
/// much of the way to a quieter one they fall: quick up, gentle down.
const ATTACK: f32 = 0.6;
const RELEASE: f32 = 0.2;

/// Keeps the recent loudness and turns it into bar heights.
///
/// The newest level sits in the middle bar and older ones spread outwards on
/// both sides, lower the further out, so speech reads as a pulse travelling
/// from the center. Levels are eased in time and between bars, so the bars
/// glide rather than jump.
#[derive(Debug, Default)]
pub struct Levels {
    /// Eased levels, newest first.
    history: [f32; HISTORY],
}

impl Levels {
    /// Records the latest raw level (0.0 to 1.0 of full scale).
    pub fn push(&mut self, raw: f32) {
        // Speech peaks are a small fraction of full scale; a square root spreads
        // them over the visible range.
        let shaped = (raw.clamp(0.0, 1.0) * 1.6).sqrt().min(1.0);
        let last = self.history[0];
        let rate = if shaped > last { ATTACK } else { RELEASE };
        self.history.rotate_right(1);
        self.history[0] = last + (shaped - last) * rate;
    }

    /// Bar heights from left to right.
    #[must_use]
    pub fn bars(&self) -> [f32; INDICATOR_BARS] {
        std::array::from_fn(|index| {
            let away = index.abs_diff(REACH) as f32;
            // A bar between two redraws old shows a mix of the two levels.
            let age = away * TRAVEL;
            let (older, newer) = (age.ceil() as usize, age.floor() as usize);
            let level = self.history[newer]
                + (self.history[older.min(HISTORY - 1)] - self.history[newer]) * age.fract();
            // Outer bars are lower, which gives the row the outline of a voice.
            level * (1.0 - 0.62 * away / REACH as f32)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha(pixel: u32) -> u32 {
        pixel >> 24
    }

    /// One frame of the indicator, drawn from scratch.
    fn indicator(scale: f32, bars: &[Bar; INDICATOR_BARS]) -> Vec<u32> {
        let artwork = Artwork::new(scale, DEFAULT_ACCENT);
        let mut pixels = vec![0; artwork.base.len()];
        artwork.paint(bars, &mut pixels);
        pixels
    }

    /// A frame of white bars at the given heights.
    fn frame(scale: f32, levels: [f32; INDICATOR_BARS]) -> Vec<u32> {
        indicator(scale, &listening(&levels))
    }

    #[test]
    fn icon_is_opaque_in_the_middle_and_clear_in_the_corners() {
        for listening in [false, true] {
            let pixels = icon(32, listening);
            assert_eq!(pixels.len(), 32 * 32);
            assert_eq!(alpha(pixels[0]), 0);
            assert_eq!(pixels[16 * 32 + 16], 0xFFFF_FFFF, "center bar is white");
        }
    }

    #[test]
    fn icon_bars_are_sharp_and_symmetric_at_every_tray_size() {
        for size in [16, 20, 24, 32, 40, 48] {
            let pixels = icon(size, false);
            let row = (size / 2 * size) as usize;
            let row = &pixels[row..row + size as usize];
            for pixel in &row[2..row.len() - 2] {
                assert!(
                    [0xFFFF_FFFF, 0xFF5B_5BD6].contains(pixel),
                    "no blurred bar edges at {size}: {pixel:08X}"
                );
            }
            assert!(row.iter().eq(row.iter().rev()), "symmetric at {size}");
        }
    }

    #[test]
    fn unpremultiply_restores_straight_color() {
        assert_eq!(unpremultiply(0), 0);
        assert_eq!(unpremultiply(0xFF12_3456), 0xFF12_3456);
        // Half-transparent white, premultiplied, becomes half-transparent full white.
        assert_eq!(unpremultiply(0x8080_8080), 0x80FF_FFFF);
    }

    #[test]
    fn icon_color_shows_the_state() {
        // A tile pixel beside the bars, left of center.
        let at = 16 * 32 + 3;
        assert_eq!(icon(32, false)[at], 0xFF5B_5BD6);
        assert_eq!(icon(32, true)[at], 0xFFE5_484D);
    }

    #[test]
    fn premultiplied_channels_never_exceed_alpha() {
        let bars = finishing(&[0.0, 0.2, 0.6, 1.0, 0.6, 0.2, 0.0], 0.4);
        for scale in [1.0, 1.25, 1.5, 2.0, 3.75] {
            for pixel in indicator(scale, &bars) {
                let a = alpha(pixel);
                for shift in [16, 8, 0] {
                    assert!((pixel >> shift) & 0xFF <= a);
                }
            }
        }
    }

    #[test]
    fn the_bitmap_is_as_large_as_it_says_at_every_scale() {
        for scale in [0.0, 1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 5.0, 1000.0] {
            let (width, height) = indicator_size(scale);
            let artwork = Artwork::new(scale, DEFAULT_ACCENT);
            assert_eq!(artwork.size(), (width, height));
            assert_eq!(frame(scale, [0.5; 7]).len(), (width * height) as usize);
            let (left, top, wide, tall) = artwork.capsule();
            assert!(left > 0 && top > 0, "room for the shadow");
            assert!((left + wide).cast_unsigned() < width && (top + tall).cast_unsigned() < height);
        }
    }

    #[test]
    fn the_shadow_fades_to_nothing_at_the_edge_of_the_bitmap() {
        for scale in [1.0, 1.5, 2.0] {
            let (width, height) = indicator_size(scale);
            let pixels = frame(scale, [1.0; 7]);
            let at = |x: u32, y: u32| alpha(pixels[(y * width + x) as usize]);
            for x in 0..width {
                assert!(at(x, 0) <= 2 && at(x, height - 1) <= 8);
            }
            for y in 0..height {
                assert!(at(0, y) <= 4 && at(width - 1, y) <= 4);
            }
        }
    }

    #[test]
    fn the_indicator_is_symmetric_from_left_to_right() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let (width, _) = indicator_size(scale);
            let pixels = frame(scale, [0.1, 0.4, 0.8, 1.0, 0.8, 0.4, 0.1]);
            for row in pixels.chunks(width as usize) {
                assert!(row.iter().eq(row.iter().rev()), "at scale {scale}");
            }
        }
    }

    #[test]
    fn bars_have_sharp_sides_at_whole_scales() {
        // Along the middle row there is only bar and capsule, nothing between.
        for scale in [1.0, 2.0, 3.0] {
            let (width, _) = indicator_size(scale);
            let artwork = Artwork::new(scale, DEFAULT_ACCENT);
            let (left, top, wide, tall) = artwork.capsule();
            let pixels = frame(scale, [1.0; 7]);
            let row = (top + tall / 2).cast_unsigned() * width;
            let mut colors: Vec<u32> = ((left + tall / 2)..(left + wide - tall / 2))
                .map(|x| pixels[(row + x.cast_unsigned()) as usize])
                .collect();
            colors.sort_unstable();
            colors.dedup();
            assert_eq!(colors.len(), 2, "at scale {scale}: {colors:08X?}");
            assert_eq!(colors[1], 0xFFFF_FFFF);
        }
    }

    #[test]
    fn louder_audio_draws_taller_bars() {
        let (width, height) = indicator_size(1.0);
        let white_in_middle_column = |levels| {
            let pixels = frame(1.0, levels);
            (0..height)
                .filter(|y| pixels[(y * width + width / 2) as usize] == 0xFFFF_FFFF)
                .count()
        };
        assert!(white_in_middle_column([1.0; 7]) > white_in_middle_column([0.0; 7]) + 6);
    }

    #[test]
    fn painting_again_leaves_nothing_of_the_frame_before() {
        let artwork = Artwork::new(1.5, DEFAULT_ACCENT);
        let (width, height) = artwork.size();
        let mut pixels = vec![0; (width * height) as usize];
        artwork.paint(&listening(&[1.0; 7]), &mut pixels);
        artwork.paint(&listening(&[0.0; 7]), &mut pixels);
        assert_eq!(pixels, frame(1.5, [0.0; 7]));
        // A buffer of the wrong size is not written to.
        let mut short = vec![7; 5];
        artwork.paint(&listening(&[1.0; 7]), &mut short);
        assert_eq!(short, [7; 5]);
    }

    #[test]
    fn the_glow_takes_the_accent_color() {
        let artwork = Artwork::new(2.0, (10, 200, 30));
        let (width, height) = artwork.size();
        let mut pixels = vec![0; (width * height) as usize];
        let mut bars = [Bar::default(); INDICATOR_BARS];
        bars[3] = Bar {
            height: 1.0,
            glow: 1.0,
        };
        artwork.paint(&bars, &mut pixels);
        assert_eq!(
            pixels[(height / 2 * width + width / 2) as usize],
            0xFF0A_C81E
        );
    }

    #[test]
    fn the_finishing_glow_sweeps_from_left_to_right_and_leaves() {
        let still = [0.0; INDICATOR_BARS];
        let brightest = |phase: f32| {
            let bars = finishing(&still, phase);
            (0..INDICATOR_BARS)
                .max_by(|a, b| bars[*a].glow.total_cmp(&bars[*b].glow))
                .unwrap_or(0)
        };
        assert!(brightest(0.25) < brightest(0.5) && brightest(0.5) < brightest(0.75));
        for bar in finishing(&still, 0.0) {
            assert!(bar.glow < 0.2, "the sweep starts outside the row");
        }
        // What is left of the voice is kept until it has settled.
        assert!((finishing(&[0.9; 7], 0.5)[0].height - 0.9).abs() < f32::EPSILON);
    }

    #[test]
    fn newest_level_is_in_the_middle_and_spreads_outwards() {
        let mut levels = Levels::default();
        assert!(levels.bars().iter().all(|bar| *bar < 0.01));
        levels.push(1.0);
        let bars = levels.bars();
        assert!(bars[3] > 0.5, "the middle bar rises at once");
        assert!(bars[0] < f32::EPSILON && bars[6] < f32::EPSILON);
        for _ in 0..8 {
            levels.push(1.0);
        }
        let bars = levels.bars();
        assert!(bars[3] > 0.99);
        assert!(
            bars[3] > bars[2] && bars[2] > bars[1] && bars[1] > bars[0] && bars[0] > 0.3,
            "lower towards the ends: {bars:?}"
        );
        assert!(bars.iter().eq(bars.iter().rev()), "the same on both sides");
    }

    #[test]
    fn levels_fall_gently_and_come_to_rest() {
        let mut levels = Levels::default();
        for _ in 0..8 {
            levels.push(1.0);
        }
        levels.push(0.0);
        let middle = levels.bars()[3];
        assert!(
            middle > 0.7 && middle < 0.9,
            "eases down instead of dropping"
        );
        for _ in 0..40 {
            levels.push(0.0);
        }
        assert!(levels.bars().iter().all(|bar| *bar < 0.01));
    }
}
