//! Renders the listening indicator and the fallback tray icons to a picture
//! for a visual check: on a dark and on a light background, at 100%, 150% and
//! 200%, quiet, speaking, loud and finishing.
//!
//! ```text
//! cargo run -p vinput-host --example preview -- preview.png [zoom]
//! ```
//!
//! The picture is a PNG, or a BMP if the name ends in `.bmp`. `zoom` makes
//! each pixel a square of that many, for looking at edges; it is 1 if left out.
#![allow(
    clippy::print_stderr,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]

use vinput_host::draw::{self, Artwork, Bar, INDICATOR_BARS};

const MARGIN: usize = 14;
const SCALES: [f32; 3] = [1.0, 1.5, 2.0];
const ICON_SIZES: [u32; 4] = [16, 20, 24, 32];
/// The two backgrounds: a dark window and a light one.
const BACKGROUNDS: [[u8; 3]; 2] = [[32, 32, 32], [243, 243, 243]];

struct Canvas {
    width: usize,
    height: usize,
    zoom: usize,
    pixels: Vec<[u8; 3]>,
}

impl Canvas {
    /// A canvas of `width` by `height` unzoomed pixels, its left half dark
    /// and its right half light.
    fn new(width: usize, height: usize, zoom: usize) -> Self {
        let (width, height) = (width * zoom, height * zoom);
        let pixels = (0..width * height)
            .map(|i| BACKGROUNDS[usize::from(i % width >= width / 2)])
            .collect();
        Self {
            width,
            height,
            zoom,
            pixels,
        }
    }

    /// Composites premultiplied ARGB `sprite` at (`left`, `top`).
    fn blit(&mut self, sprite: &[u32], sprite_width: usize, left: usize, top: usize) {
        for (index, pixel) in sprite.iter().enumerate() {
            let alpha = (pixel >> 24) as f32 / 255.0;
            let source = [(pixel >> 16) & 0xFF, (pixel >> 8) & 0xFF, pixel & 0xFF];
            for dy in 0..self.zoom {
                for dx in 0..self.zoom {
                    let x = (left + index % sprite_width) * self.zoom + dx;
                    let y = (top + index / sprite_width) * self.zoom + dy;
                    let target = &mut self.pixels[y * self.width + x];
                    for channel in 0..3 {
                        let under = f32::from(target[channel]) * (1.0 - alpha);
                        target[channel] = (source[channel] as f32 + under).min(255.0) as u8;
                    }
                }
            }
        }
    }

    fn bmp(&self) -> Vec<u8> {
        let row = (self.width * 3).div_ceil(4) * 4;
        let size = 54 + row * self.height;
        let mut out = Vec::with_capacity(size);
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&(size as u32).to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&54u32.to_le_bytes());
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&(self.width as i32).to_le_bytes());
        out.extend_from_slice(&(-(self.height as i32)).to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&24u16.to_le_bytes());
        out.extend_from_slice(&[0; 24]);
        for line in self.pixels.chunks(self.width) {
            for [r, g, b] in line {
                out.extend_from_slice(&[*b, *g, *r]);
            }
            out.resize(out.len() + row - self.width * 3, 0);
        }
        out
    }

    fn png(&self) -> Vec<u8> {
        // Each row starts with a zero: no filter.
        let mut raw = Vec::with_capacity((self.width * 3 + 1) * self.height);
        for line in self.pixels.chunks(self.width) {
            raw.push(0);
            raw.extend(line.iter().flatten());
        }
        // A zlib stream of stored blocks: nothing is compressed.
        let mut data = vec![0x78, 0x01];
        let mut blocks = raw.chunks(0xFFFF).peekable();
        while let Some(block) = blocks.next() {
            data.push(u8::from(blocks.peek().is_none()));
            data.extend_from_slice(&(block.len() as u16).to_le_bytes());
            data.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
            data.extend_from_slice(block);
        }
        let (a, b) = raw.iter().fold((1u32, 0u32), |(a, b), byte| {
            let a = (a + u32::from(*byte)) % 65521;
            (a, (b + a) % 65521)
        });
        data.extend_from_slice(&(b << 16 | a).to_be_bytes());

        let mut header = Vec::new();
        header.extend_from_slice(&(self.width as u32).to_be_bytes());
        header.extend_from_slice(&(self.height as u32).to_be_bytes());
        // Eight bits a channel, RGB, and the only compression, filter and
        // interlace methods there are.
        header.extend_from_slice(&[8, 2, 0, 0, 0]);
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        for (kind, body) in [(b"IHDR", &header), (b"IDAT", &data), (b"IEND", &Vec::new())] {
            out.extend_from_slice(&(body.len() as u32).to_be_bytes());
            let start = out.len();
            out.extend_from_slice(kind);
            out.extend_from_slice(body);
            let crc = crc32(&out[start..]);
            out.extend_from_slice(&crc.to_be_bytes());
        }
        out
    }
}

fn crc32(bytes: &[u8]) -> u32 {
    !bytes.iter().fold(!0u32, |crc, byte| {
        (0..8).fold(crc ^ u32::from(*byte), |crc, _| {
            (crc >> 1) ^ (0xEDB8_8320 & 0u32.wrapping_sub(crc & 1))
        })
    })
}

/// The frames shown side by side: quiet, speaking, loud, and finishing with
/// the glow in the middle of the row.
fn frames() -> [[Bar; INDICATOR_BARS]; 4] {
    let mut speaking = draw::Levels::default();
    for level in [0.02, 0.05, 0.3, 0.25, 0.04, 0.2] {
        speaking.push(level);
    }
    let mut loud = draw::Levels::default();
    for _ in 0..12 {
        loud.push(1.0);
    }
    let still = [0.0; INDICATOR_BARS];
    [
        draw::listening(&still),
        draw::listening(&speaking.bars()),
        draw::listening(&loud.bars()),
        draw::finishing(&still, 0.5),
    ]
}

fn main() -> std::io::Result<()> {
    let mut arguments = std::env::args().skip(1);
    let path = arguments.next().unwrap_or_else(|| "preview.png".to_owned());
    let zoom = arguments
        .next()
        .and_then(|zoom| zoom.parse().ok())
        .unwrap_or(1);

    let frames = frames();
    let (widest, _) = draw::indicator_size(SCALES[SCALES.len() - 1]);
    let half = MARGIN + frames.len() * (widest as usize + MARGIN);
    let icons = ICON_SIZES[ICON_SIZES.len() - 1] as usize + MARGIN;
    let height = SCALES
        .iter()
        .map(|scale| draw::indicator_size(*scale).1 as usize + MARGIN)
        .sum::<usize>()
        + icons
        + MARGIN;
    let mut canvas = Canvas::new(half * 2, height, zoom);

    for side in 0..2 {
        let mut top = MARGIN;
        let mut left = side * half + MARGIN;
        for listening in [false, true] {
            for size in ICON_SIZES {
                canvas.blit(&draw::icon(size, listening), size as usize, left, top);
                left += size as usize + MARGIN;
            }
        }
        top += icons;
        for scale in SCALES {
            let artwork = Artwork::new(scale, draw::DEFAULT_ACCENT);
            let (width, height) = artwork.size();
            let mut pixels = vec![0; (width * height) as usize];
            for (column, bars) in frames.iter().enumerate() {
                let left = side * half + MARGIN + column * (widest as usize + MARGIN);
                artwork.paint(bars, &mut pixels);
                canvas.blit(&pixels, width as usize, left, top);
            }
            top += height as usize + MARGIN;
        }
    }

    let lower = path.to_ascii_lowercase();
    let picture = if std::path::Path::new(&lower)
        .extension()
        .is_some_and(|extension| extension == "bmp")
    {
        canvas.bmp()
    } else {
        canvas.png()
    };
    std::fs::write(&path, picture)?;
    eprintln!("wrote {path}");
    Ok(())
}
