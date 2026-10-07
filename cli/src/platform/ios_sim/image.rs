//! PNG pixels for icons and screenshots (design §13.1): decode, flatten,
//! resample, encode, the preview (long edge at most 1024 px) and blank
//! detection. Nothing here is iOS-specific; every platform's screenshot
//! goes through the same steps.

use crate::screen::{PREVIEW_MAX, preview_size};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read};
use std::path::Path;

/// A screenshot whose dominant colour covers at least this share of the
/// pixels is blank (`run.screen_blank`).
pub const BLANK_FRACTION: f64 = 0.995;

/// How far (per channel) a pixel may be from the dominant colour and still
/// count as that colour.
const BLANK_TOLERANCE: u8 = 8;

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
    /// A uniform image.
    pub fn filled(width: u32, height: u32, color: [u8; 4]) -> Rgba {
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
        for _ in 0..(width as usize * height as usize) {
            pixels.extend_from_slice(&color);
        }
        Rgba {
            width,
            height,
            pixels,
        }
    }

    fn pixel(&self, x: u32, y: u32) -> &[u8] {
        let index = (y as usize * self.width as usize + x as usize) * 4;
        &self.pixels[index..index + 4]
    }

    /// Sets one pixel (tests and fixtures).
    pub fn set(&mut self, x: u32, y: u32, color: [u8; 4]) {
        let index = (y as usize * self.width as usize + x as usize) * 4;
        self.pixels[index..index + 4].copy_from_slice(&color);
    }

    /// Whether any pixel is not fully opaque.
    pub fn has_transparency(&self) -> bool {
        self.pixels
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[3] != 255)
    }
}

/// The width and height in a PNG's IHDR chunk, without decoding it.
pub fn png_size(path: &Path) -> Option<(u32, u32)> {
    let mut header = [0u8; 24];
    File::open(path).ok()?.read_exact(&mut header).ok()?;
    if &header[..8] != b"\x89PNG\r\n\x1a\n" || &header[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(header[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(header[20..24].try_into().ok()?);
    Some((width, height))
}

/// Decodes a PNG of any colour type and depth into RGBA8.
pub fn read_png(path: &Path) -> Result<Rgba, String> {
    let file =
        File::open(path).map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    let mut decoder = png::Decoder::new(BufReader::new(file));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder
        .read_info()
        .map_err(|error| format!("{} is not a valid PNG: {error}", path.display()))?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| format!("{} is too large to decode", path.display()))?;
    let mut buffer = vec![0u8; size];
    let info = reader
        .next_frame(&mut buffer)
        .map_err(|error| format!("cannot decode {}: {error}", path.display()))?;
    buffer.truncate(info.buffer_size());

    let count = info.width as usize * info.height as usize;
    let mut pixels = Vec::with_capacity(count * 4);
    match info.color_type {
        png::ColorType::Rgba => pixels = buffer,
        png::ColorType::Rgb => {
            for rgb in buffer.as_chunks::<3>().0.iter() {
                pixels.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for ga in buffer.as_chunks::<2>().0.iter() {
                pixels.extend_from_slice(&[ga[0], ga[0], ga[0], ga[1]]);
            }
        }
        png::ColorType::Grayscale => {
            for g in buffer {
                pixels.extend_from_slice(&[g, g, g, 255]);
            }
        }
        png::ColorType::Indexed => {
            return Err(format!("{}: indexed PNG was not expanded", path.display()));
        }
    }

    Ok(Rgba {
        width: info.width,
        height: info.height,
        pixels,
    })
}

/// Encodes an image as PNG: RGB when `alpha` is false (the alpha channel is
/// dropped, so flatten first), RGBA otherwise.
pub fn write_png(path: &Path, image: &Rgba, alpha: bool) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    let file =
        File::create(path).map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), image.width, image.height);
    encoder.set_color(if alpha {
        png::ColorType::Rgba
    } else {
        png::ColorType::Rgb
    });
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder
        .write_header()
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;

    let data: Vec<u8> = if alpha {
        image.pixels.clone()
    } else {
        image
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
            .collect()
    };
    writer
        .write_image_data(&data)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    writer
        .finish()
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

/// Composites an image over an opaque background colour.
pub fn flatten(image: &Rgba, background: [u8; 3]) -> Rgba {
    let mut pixels = Vec::with_capacity(image.pixels.len());
    for pixel in image.pixels.as_chunks::<4>().0.iter() {
        let alpha = u32::from(pixel[3]);
        for channel in 0..3 {
            let value = (u32::from(pixel[channel]) * alpha
                + u32::from(background[channel]) * (255 - alpha)
                + 127)
                / 255;
            pixels.push(value as u8);
        }
        pixels.push(255);
    }
    Rgba {
        width: image.width,
        height: image.height,
        pixels,
    }
}

/// Resamples to `width` x `height` by averaging the source area under each
/// output pixel (a box filter: good for the downscales icm needs).
pub fn resize(image: &Rgba, width: u32, height: u32) -> Rgba {
    if image.width == width && image.height == height {
        return image.clone();
    }
    let sx = f64::from(image.width) / f64::from(width);
    let sy = f64::from(image.height) / f64::from(height);
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);

    for oy in 0..height {
        let y0 = (f64::from(oy) * sy).floor() as u32;
        let y1 = ((f64::from(oy + 1) * sy).ceil() as u32).clamp(y0 + 1, image.height);
        for ox in 0..width {
            let x0 = (f64::from(ox) * sx).floor() as u32;
            let x1 = ((f64::from(ox + 1) * sx).ceil() as u32).clamp(x0 + 1, image.width);
            let mut sum = [0u64; 4];
            for y in y0..y1 {
                for x in x0..x1 {
                    let pixel = image.pixel(x.min(image.width - 1), y.min(image.height - 1));
                    for (total, value) in sum.iter_mut().zip(pixel) {
                        *total += u64::from(*value);
                    }
                }
            }
            let count = u64::from((y1 - y0) * (x1 - x0)).max(1);
            for total in sum {
                pixels.push(((total + count / 2) / count) as u8);
            }
        }
    }

    Rgba {
        width,
        height,
        pixels,
    }
}

/// How uniform an image is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Blank {
    /// The share of pixels close to the dominant colour, 0 to 1.
    pub fraction: f64,
    /// The dominant colour.
    pub color: [u8; 3],
}

impl Blank {
    /// Whether the image counts as blank.
    pub fn is_blank(&self) -> bool {
        self.fraction >= BLANK_FRACTION
    }

    /// `99.8% of pixels are #000000`.
    pub fn describe(&self) -> String {
        format!(
            "{:.1}% of pixels are #{:02X}{:02X}{:02X}",
            self.fraction * 100.0,
            self.color[0],
            self.color[1],
            self.color[2]
        )
    }
}

/// Finds the dominant colour (on a 5-bit-per-channel histogram) and the
/// share of pixels within a small distance of it.
pub fn blank_stats(image: &Rgba) -> Blank {
    let total = image.pixels.len() / 4;
    if total == 0 {
        return Blank {
            fraction: 1.0,
            color: [0, 0, 0],
        };
    }

    let mut histogram = vec![0u32; 1 << 15];
    for pixel in image.pixels.as_chunks::<4>().0.iter() {
        let key = (usize::from(pixel[0] >> 3) << 10)
            | (usize::from(pixel[1] >> 3) << 5)
            | usize::from(pixel[2] >> 3);
        histogram[key] += 1;
    }
    let (key, _) = histogram
        .iter()
        .enumerate()
        .max_by_key(|(_, count)| **count)
        .unwrap_or((0, &0));

    // The exact colour: the average of the pixels in the winning bucket.
    let bucket = |pixel: &[u8]| {
        (usize::from(pixel[0] >> 3) << 10)
            | (usize::from(pixel[1] >> 3) << 5)
            | usize::from(pixel[2] >> 3)
    };
    let mut sum = [0u64; 3];
    let mut count = 0u64;
    for pixel in image.pixels.as_chunks::<4>().0.iter() {
        if bucket(pixel) == key {
            for channel in 0..3 {
                sum[channel] += u64::from(pixel[channel]);
            }
            count += 1;
        }
    }
    let count = count.max(1);
    let color = [
        (sum[0] / count) as u8,
        (sum[1] / count) as u8,
        (sum[2] / count) as u8,
    ];

    let close = image
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| {
            (0..3).all(|channel| pixel[channel].abs_diff(color[channel]) <= BLANK_TOLERANCE)
        })
        .count();

    Blank {
        fraction: close as f64 / total as f64,
        color,
    }
}

/// What [`preview`] found.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Preview {
    /// The screenshot's size in pixels.
    pub px: (u32, u32),
    /// The preview's size.
    pub preview: (u32, u32),
    /// How uniform the screenshot is.
    pub blank: Blank,
}

/// Writes `dst`, the screenshot `src` with its long edge scaled down to at
/// most [`PREVIEW_MAX`] px, and measures how blank the screenshot is.
pub fn preview(src: &Path, dst: &Path) -> Result<Preview, String> {
    let image = read_png(src)?;
    let blank = blank_stats(&image);
    let (width, height) = preview_size((image.width, image.height));
    debug_assert!(width.max(height) <= PREVIEW_MAX.max(image.width.max(image.height)));
    let small = resize(&image, width, height);
    write_png(dst, &small, small.has_transparency())?;
    Ok(Preview {
        px: (image.width, image.height),
        preview: (width, height),
        blank,
    })
}

/// Parses `#RRGGBB` (or `RRGGBB`).
pub fn parse_hex_color(text: &str) -> Option<[u8; 3]> {
    let hex = text.trim().trim_start_matches('#');
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |range: std::ops::Range<usize>| u8::from_str_radix(&hex[range], 16).ok();
    Some([channel(0..2)?, channel(2..4)?, channel(4..6)?])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pngs_round_trip_with_and_without_alpha() {
        let dir = tempfile::tempdir().unwrap();
        let mut image = Rgba::filled(7, 5, [10, 20, 30, 255]);
        image.set(3, 2, [200, 100, 50, 128]);

        let path = dir.path().join("a.png");
        write_png(&path, &image, true).unwrap();
        assert_eq!(png_size(&path), Some((7, 5)));
        assert_eq!(read_png(&path).unwrap(), image);

        let flat = flatten(&image, [255, 255, 255]);
        assert!(!flat.has_transparency());
        assert_eq!(flat.pixel(3, 2), &[227, 177, 152, 255]);
        let rgb = dir.path().join("b.png");
        write_png(&rgb, &flat, false).unwrap();
        assert_eq!(read_png(&rgb).unwrap(), flat);

        std::fs::write(dir.path().join("c.png"), b"not a png").unwrap();
        assert!(read_png(&dir.path().join("c.png")).is_err());
        assert_eq!(png_size(&dir.path().join("c.png")), None);
    }

    #[test]
    fn resizing_averages_areas() {
        let mut image = Rgba::filled(4, 2, [0, 0, 0, 255]);
        image.set(0, 0, [255, 255, 255, 255]);
        image.set(1, 0, [255, 255, 255, 255]);
        let small = resize(&image, 2, 1);
        assert_eq!((small.width, small.height), (2, 1));
        assert_eq!(small.pixel(0, 0), &[128, 128, 128, 255]);
        assert_eq!(small.pixel(1, 0), &[0, 0, 0, 255]);

        // Odd ratios cover every source pixel.
        let odd = resize(&Rgba::filled(1206, 2622, [9, 9, 9, 255]), 471, 1024);
        assert_eq!(odd.pixels.len(), 471 * 1024 * 4);
        assert!(
            odd.pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [9, 9, 9, 255])
        );
    }

    #[test]
    fn blank_detection() {
        let black = Rgba::filled(100, 100, [0, 0, 0, 255]);
        let stats = blank_stats(&black);
        assert!(stats.is_blank());
        assert_eq!(stats.describe(), "100.0% of pixels are #000000");

        // Near-identical noise still counts as one colour.
        let mut noisy = Rgba::filled(100, 100, [250, 250, 250, 255]);
        noisy.set(1, 1, [255, 255, 255, 255]);
        assert!(blank_stats(&noisy).is_blank());

        // 2% content is not blank.
        let mut content = Rgba::filled(100, 100, [255, 255, 255, 255]);
        for x in 0..100 {
            content.set(x, 10, [0, 0, 0, 255]);
            content.set(x, 11, [80, 90, 240, 255]);
        }
        let stats = blank_stats(&content);
        assert!(!stats.is_blank(), "{stats:?}");
        assert_eq!(stats.color, [255, 255, 255]);
    }

    #[test]
    fn previews_cap_the_long_edge() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("screen.png");
        let dst = dir.path().join("screen.preview.png");
        write_png(&src, &Rgba::filled(1206, 2622, [255, 255, 255, 255]), false).unwrap();
        let preview = preview(&src, &dst).unwrap();
        assert_eq!(preview.px, (1206, 2622));
        assert_eq!(preview.preview, (471, 1024));
        assert_eq!(png_size(&dst), Some((471, 1024)));
        assert!(preview.blank.is_blank());
    }

    #[test]
    fn colors_parse() {
        assert_eq!(parse_hex_color("#FFFFFF"), Some([255, 255, 255]));
        assert_eq!(parse_hex_color("1a2B3c"), Some([0x1a, 0x2b, 0x3c]));
        assert_eq!(parse_hex_color("#FFF"), None);
        assert_eq!(parse_hex_color("#GGGGGG"), None);
    }
}
