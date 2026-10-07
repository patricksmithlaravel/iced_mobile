//! PNG work for screenshots and icons: decode to RGBA, resample, flatten,
//! encode, previews (long edge at most [`PREVIEW_MAX`]) and blank detection
//! (design §13.1).
//!
//! Nothing here is Android-specific; it lives with the Android pipeline
//! until another platform needs it.

use crate::screen::{PREVIEW_MAX, preview_size};
use std::io::Cursor;
use std::path::Path;

/// An 8-bit RGBA image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgba {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes, row by row.
    pub pixels: Vec<u8>,
}

impl Rgba {
    /// A blank (transparent) image.
    pub fn new(width: u32, height: u32) -> Rgba {
        Rgba {
            width,
            height,
            pixels: vec![0; (width as usize) * (height as usize) * 4],
        }
    }

    /// An image filled with one colour.
    pub fn filled(width: u32, height: u32, color: [u8; 4]) -> Rgba {
        let mut image = Rgba::new(width, height);
        for pixel in image.pixels.as_chunks_mut::<4>().0 {
            pixel.copy_from_slice(&color);
        }
        image
    }

    fn index(&self, x: u32, y: u32) -> usize {
        ((y as usize) * (self.width as usize) + x as usize) * 4
    }

    /// The pixel at (x, y).
    pub fn get(&self, x: u32, y: u32) -> [u8; 4] {
        let i = self.index(x, y);
        [
            self.pixels[i],
            self.pixels[i + 1],
            self.pixels[i + 2],
            self.pixels[i + 3],
        ]
    }

    /// Sets the pixel at (x, y).
    pub fn set(&mut self, x: u32, y: u32, color: [u8; 4]) {
        let i = self.index(x, y);
        self.pixels[i..i + 4].copy_from_slice(&color);
    }

    /// Composites the image over an opaque colour.
    pub fn flatten(&self, background: [u8; 3]) -> Rgba {
        let mut out = self.clone();
        for pixel in out.pixels.as_chunks_mut::<4>().0 {
            let alpha = u32::from(pixel[3]);
            for channel in 0..3 {
                let over = u32::from(pixel[channel]) * alpha
                    + u32::from(background[channel]) * (255 - alpha);
                pixel[channel] = ((over + 127) / 255) as u8;
            }
            pixel[3] = 255;
        }
        out
    }

    /// Whether every pixel is opaque.
    pub fn is_opaque(&self) -> bool {
        self.pixels
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel[3] == 255)
    }

    /// Resamples to `width` x `height` by area averaging (a box filter over
    /// each output pixel's footprint): good for shrinking, adequate for
    /// the rare enlargement.
    pub fn resize(&self, width: u32, height: u32) -> Rgba {
        let width = width.max(1);
        let height = height.max(1);
        if width == self.width && height == self.height {
            return self.clone();
        }
        let mut out = Rgba::new(width, height);
        let scale_x = f64::from(self.width) / f64::from(width);
        let scale_y = f64::from(self.height) / f64::from(height);

        for y in 0..height {
            let y0 = f64::from(y) * scale_y;
            let y1 = (f64::from(y + 1) * scale_y).max(y0 + 1e-9);
            for x in 0..width {
                let x0 = f64::from(x) * scale_x;
                let x1 = (f64::from(x + 1) * scale_x).max(x0 + 1e-9);

                // Premultiplied sums, so transparent pixels do not darken
                // their neighbours.
                let mut sum = [0f64; 4];
                let mut total = 0f64;
                let mut sy = y0.floor() as u32;
                while f64::from(sy) < y1 && sy < self.height {
                    let wy = (f64::from(sy + 1).min(y1) - f64::from(sy).max(y0)).max(0.0);
                    let mut sx = x0.floor() as u32;
                    while f64::from(sx) < x1 && sx < self.width {
                        let wx = (f64::from(sx + 1).min(x1) - f64::from(sx).max(x0)).max(0.0);
                        let weight = wx * wy;
                        let pixel = self.get(sx, sy);
                        let alpha = f64::from(pixel[3]) / 255.0;
                        for channel in 0..3 {
                            sum[channel] += f64::from(pixel[channel]) * alpha * weight;
                        }
                        sum[3] += alpha * weight;
                        total += weight;
                        sx += 1;
                    }
                    sy += 1;
                }

                if total > 0.0 && sum[3] > 0.0 {
                    let alpha = sum[3] / total;
                    let color = [
                        (sum[0] / sum[3]).round().clamp(0.0, 255.0) as u8,
                        (sum[1] / sum[3]).round().clamp(0.0, 255.0) as u8,
                        (sum[2] / sum[3]).round().clamp(0.0, 255.0) as u8,
                        (alpha * 255.0).round().clamp(0.0, 255.0) as u8,
                    ];
                    out.set(x, y, color);
                }
            }
        }
        out
    }

    /// Draws `other` with its top-left corner at (x, y) (source over).
    pub fn draw(&mut self, other: &Rgba, x: u32, y: u32) {
        for oy in 0..other.height {
            for ox in 0..other.width {
                let (tx, ty) = (x + ox, y + oy);
                if tx >= self.width || ty >= self.height {
                    continue;
                }
                let src = other.get(ox, oy);
                let dst = self.get(tx, ty);
                let sa = u32::from(src[3]);
                let da = u32::from(dst[3]);
                let out_a = sa + da * (255 - sa) / 255;
                if out_a == 0 {
                    self.set(tx, ty, [0, 0, 0, 0]);
                    continue;
                }
                let mut color = [0u8; 4];
                for channel in 0..3 {
                    let value = (u32::from(src[channel]) * sa
                        + u32::from(dst[channel]) * da * (255 - sa) / 255)
                        / out_a;
                    color[channel] = value.min(255) as u8;
                }
                color[3] = out_a.min(255) as u8;
                self.set(tx, ty, color);
            }
        }
    }

    /// Clears everything outside the centred circle (legacy round icons).
    pub fn mask_circle(&mut self) {
        let cx = f64::from(self.width) / 2.0;
        let cy = f64::from(self.height) / 2.0;
        let radius = cx.min(cy);
        for y in 0..self.height {
            for x in 0..self.width {
                let dx = f64::from(x) + 0.5 - cx;
                let dy = f64::from(y) + 0.5 - cy;
                let distance = (dx * dx + dy * dy).sqrt();
                // One pixel of anti-aliasing at the edge.
                let coverage = (radius - distance + 0.5).clamp(0.0, 1.0);
                if coverage < 1.0 {
                    let mut pixel = self.get(x, y);
                    pixel[3] = (f64::from(pixel[3]) * coverage).round() as u8;
                    self.set(x, y, pixel);
                }
            }
        }
    }
}

/// Reads a PNG's size from its IHDR chunk without decoding it.
pub fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((width, height))
}

/// Decodes a PNG of any colour type and bit depth to 8-bit RGBA.
pub fn decode(bytes: &[u8]) -> Result<Rgba, String> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| "the image is too large".to_string())?;
    let mut buffer = vec![0; size];
    let info = reader.next_frame(&mut buffer).map_err(|e| e.to_string())?;
    buffer.truncate(info.buffer_size());

    let (width, height) = (info.width, info.height);
    let pixels: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => buffer,
        png::ColorType::Rgb => buffer
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::Grayscale => buffer.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::GrayscaleAlpha => buffer
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Indexed => return Err("unexpanded palette image".to_string()),
    };
    if pixels.len() != (width as usize) * (height as usize) * 4 {
        return Err(format!(
            "decoded {} bytes for {width}x{height}",
            pixels.len()
        ));
    }
    Ok(Rgba {
        width,
        height,
        pixels,
    })
}

/// Encodes RGBA as a PNG; opaque images are written as RGB.
pub fn encode(image: &Rgba) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, image.width, image.height);
        let opaque = image.is_opaque();
        encoder.set_color(if opaque {
            png::ColorType::Rgb
        } else {
            png::ColorType::Rgba
        });
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Balanced);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        if opaque {
            let rgb: Vec<u8> = image
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2]])
                .collect();
            writer.write_image_data(&rgb).map_err(|e| e.to_string())?;
        } else {
            writer
                .write_image_data(&image.pixels)
                .map_err(|e| e.to_string())?;
        }
        writer.finish().map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// What a screenshot looks like.
#[derive(Clone, Debug, PartialEq)]
pub struct Stats {
    /// Its size.
    pub px: (u32, u32),
    /// The preview's size.
    pub preview: (u32, u32),
    /// The share of pixels close to the dominant colour (0 to 1).
    pub dominant_share: f64,
    /// The dominant colour, `#RRGGBB`.
    pub dominant: String,
    /// Whether it is a single colour (at least 99.5 % of the pixels).
    pub blank: bool,
}

/// The share of pixels that makes a screenshot "blank".
pub const BLANK_SHARE: f64 = 0.995;

/// Finds the dominant colour and how much of the image is within a small
/// distance of it.
pub fn dominant(image: &Rgba) -> (f64, [u8; 3]) {
    let total = (image.width as usize) * (image.height as usize);
    if total == 0 {
        return (1.0, [0, 0, 0]);
    }
    // A coarse histogram (4 bits per channel) finds the dominant bucket;
    // its mean colour is the reference.
    let mut counts = vec![0u32; 4096];
    let mut sums = vec![[0u64; 3]; 4096];
    for pixel in image.pixels.as_chunks::<4>().0 {
        let bucket = (usize::from(pixel[0] >> 4) << 8)
            | (usize::from(pixel[1] >> 4) << 4)
            | usize::from(pixel[2] >> 4);
        counts[bucket] += 1;
        for channel in 0..3 {
            sums[bucket][channel] += u64::from(pixel[channel]);
        }
    }
    let (best, count) = counts
        .iter()
        .enumerate()
        .max_by_key(|(_, count)| **count)
        .map(|(index, count)| (index, u64::from(*count)))
        .unwrap_or((0, 1));
    let reference = [
        (sums[best][0] / count.max(1)) as u8,
        (sums[best][1] / count.max(1)) as u8,
        (sums[best][2] / count.max(1)) as u8,
    ];
    let close = image
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| (0..3).all(|c| pixel[c].abs_diff(reference[c]) <= 12))
        .count();
    (close as f64 / total as f64, reference)
}

/// Writes `<dir>/<stem>.preview.png` next to a screenshot and returns its
/// statistics. The preview's long edge is at most [`PREVIEW_MAX`].
pub fn write_preview(png_bytes: &[u8], preview_path: &Path) -> Result<Stats, String> {
    let image = decode(png_bytes)?;
    let (width, height) = preview_size((image.width, image.height));
    let preview = if (width, height) == (image.width, image.height) {
        image.flatten([0, 0, 0])
    } else {
        image.resize(width, height).flatten([0, 0, 0])
    };
    debug_assert!(width.max(height) <= PREVIEW_MAX.max(image.width.max(image.height)));
    let encoded = encode(&preview)?;
    std::fs::write(preview_path, encoded).map_err(|e| e.to_string())?;

    let (share, color) = dominant(&preview);
    Ok(Stats {
        px: (image.width, image.height),
        preview: (width, height),
        dominant_share: share,
        dominant: format!("#{:02X}{:02X}{:02X}", color[0], color[1], color[2]),
        blank: share >= BLANK_SHARE,
    })
}

/// Parses `#RRGGBB` (or `RRGGBB`).
pub fn parse_hex_color(text: &str) -> Option<[u8; 3]> {
    let hex = text.trim().trim_start_matches('#');
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some([channel(0)?, channel(2)?, channel(4)?])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checker(width: u32, height: u32) -> Rgba {
        let mut image = Rgba::new(width, height);
        for y in 0..height {
            for x in 0..width {
                let on = (x / 4 + y / 4) % 2 == 0;
                let value = if on { 255 } else { 0 };
                image.set(x, y, [value, value, value, 255]);
            }
        }
        image
    }

    #[test]
    fn round_trips_through_png() {
        let image = checker(16, 8);
        let bytes = encode(&image).unwrap();
        assert_eq!(png_size(&bytes), Some((16, 8)));
        assert_eq!(decode(&bytes).unwrap(), image);

        let mut alpha = Rgba::filled(3, 2, [10, 20, 30, 128]);
        alpha.set(0, 0, [1, 2, 3, 0]);
        assert_eq!(decode(&encode(&alpha).unwrap()).unwrap(), alpha);
    }

    #[test]
    fn shrinking_averages_areas() {
        let image = checker(8, 8);
        let small = image.resize(1, 1);
        let pixel = small.get(0, 0);
        assert!((120..=135).contains(&pixel[0]), "{pixel:?}");
        assert_eq!(pixel[3], 255);

        let solid = Rgba::filled(1080, 2400, [255, 0, 0, 255]).resize(461, 1024);
        assert_eq!((solid.width, solid.height), (461, 1024));
        assert!(
            solid
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [255, 0, 0, 255])
        );
    }

    #[test]
    fn transparent_pixels_do_not_darken() {
        let mut image = Rgba::new(2, 1);
        image.set(0, 0, [200, 100, 50, 255]);
        let small = image.resize(1, 1);
        assert_eq!(&small.get(0, 0)[..3], &[200, 100, 50]);
        assert!((126..=129).contains(&small.get(0, 0)[3]));
    }

    #[test]
    fn flattening_and_masks() {
        let image = Rgba::filled(4, 4, [0, 0, 0, 0]);
        assert!(
            image
                .flatten([255, 255, 255])
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [255, 255, 255, 255])
        );
        let mut round = Rgba::filled(48, 48, [9, 9, 9, 255]);
        round.mask_circle();
        assert_eq!(round.get(0, 0)[3], 0);
        assert_eq!(round.get(24, 24)[3], 255);

        let mut canvas = Rgba::new(10, 10);
        canvas.draw(&Rgba::filled(4, 4, [1, 2, 3, 255]), 3, 3);
        assert_eq!(canvas.get(4, 4), [1, 2, 3, 255]);
        assert_eq!(canvas.get(0, 0), [0, 0, 0, 0]);
    }

    #[test]
    fn previews_and_blank_detection() {
        let dir = tempfile::tempdir().unwrap();
        let blank = encode(&Rgba::filled(1080, 2400, [0, 0, 0, 255])).unwrap();
        let stats = write_preview(&blank, &dir.path().join("a.preview.png")).unwrap();
        assert!(stats.blank);
        assert_eq!(stats.px, (1080, 2400));
        assert_eq!(stats.preview, (461, 1024));
        assert_eq!(stats.dominant, "#000000");
        let written = std::fs::read(dir.path().join("a.preview.png")).unwrap();
        assert_eq!(png_size(&written), Some((461, 1024)));

        let mut busy = Rgba::filled(400, 400, [255, 255, 255, 255]);
        busy.draw(&checker(100, 100), 150, 150);
        let stats =
            write_preview(&encode(&busy).unwrap(), &dir.path().join("b.preview.png")).unwrap();
        assert!(!stats.blank, "{stats:?}");
        assert_eq!(stats.preview, (400, 400));
    }

    #[test]
    fn hex_colours() {
        assert_eq!(parse_hex_color("#FFFFFF"), Some([255, 255, 255]));
        assert_eq!(parse_hex_color("1a2B3c"), Some([0x1a, 0x2b, 0x3c]));
        assert_eq!(parse_hex_color("#FFF"), None);
        assert_eq!(parse_hex_color("white"), None);
    }
}
