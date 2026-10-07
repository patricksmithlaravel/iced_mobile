//! Screenshots as pixels: PNG decoding and encoding, the preview an agent
//! looks at (long edge at most [`PREVIEW_MAX`] px, design §4.4), and blank
//! detection (design §13.1: at least 99.5 % of the pixels within a small
//! distance of one colour).
//!
//! Every capture, on a device or headless, goes through
//! [`examine`]: it reads the screenshot, writes the preview next to it and
//! says whether the image is blank.

use crate::screen::{PREVIEW_MAX, preview_size};
use serde_json::{Value, json};
use std::fs::File;
use std::io::{self, BufReader, BufWriter};
use std::path::Path;

/// The share of pixels near one colour from which a screenshot is blank.
pub const BLANK_FRACTION: f64 = 0.995;

/// How far (per channel, 0–255) a pixel may be from the dominant colour and
/// still count as that colour: anti-aliasing and dithering stay "blank".
pub const BLANK_TOLERANCE: u8 = 12;

/// An 8-bit RGBA image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height` RGBA pixels, row by row.
    pub rgba: Vec<u8>,
}

impl Image {
    /// A single-colour image.
    pub fn filled(width: u32, height: u32, rgba: [u8; 4]) -> Image {
        let count = width as usize * height as usize;
        Image {
            width,
            height,
            rgba: rgba.iter().copied().cycle().take(count * 4).collect(),
        }
    }

    /// The pixel at `x`, `y`.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let at = (y as usize * self.width as usize + x as usize) * 4;
        [
            self.rgba[at],
            self.rgba[at + 1],
            self.rgba[at + 2],
            self.rgba[at + 3],
        ]
    }

    /// Sets the pixel at `x`, `y`.
    pub fn set_pixel(&mut self, x: u32, y: u32, rgba: [u8; 4]) {
        let at = (y as usize * self.width as usize + x as usize) * 4;
        self.rgba[at..at + 4].copy_from_slice(&rgba);
    }
}

fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

/// Decodes a PNG of any colour type and depth into 8-bit RGBA.
pub fn read(path: &Path) -> io::Result<Image> {
    let file = BufReader::new(File::open(path)?);
    let mut decoder = png::Decoder::new_with_limits(
        file,
        png::Limits {
            bytes: 512 * 1024 * 1024,
        },
    );
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(invalid)?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| invalid("the PNG is too large"))?;
    let mut buffer = vec![0; size];
    let info = reader.next_frame(&mut buffer).map_err(invalid)?;
    buffer.truncate(info.buffer_size());

    let pixels = info.width as usize * info.height as usize;
    let rgba = match info.color_type {
        png::ColorType::Rgba => buffer,
        png::ColorType::Rgb => buffer
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|[r, g, b]| [*r, *g, *b, 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => buffer
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|[g, a]| [*g, *g, *g, *a])
            .collect(),
        png::ColorType::Grayscale => buffer.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => {
            return Err(invalid("an indexed PNG was not expanded"));
        }
    };
    if rgba.len() != pixels * 4 {
        return Err(invalid(format!(
            "decoded {} bytes for {}x{} pixels",
            rgba.len(),
            info.width,
            info.height
        )));
    }

    Ok(Image {
        width: info.width,
        height: info.height,
        rgba,
    })
}

/// Encodes an image as an 8-bit RGBA PNG.
pub fn write(path: &Path, image: &Image) -> io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let file = BufWriter::new(File::create(path)?);
    let mut encoder = png::Encoder::new(file, image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().map_err(invalid)?;
    writer.write_image_data(&image.rgba).map_err(invalid)?;
    writer.finish().map_err(invalid)
}

/// The preview: the image scaled down (box filter) so that its long edge
/// is at most [`PREVIEW_MAX`]; a smaller image is returned as it is.
pub fn preview(image: &Image) -> Image {
    let (width, height) = preview_size((image.width, image.height));
    if (width, height) == (image.width, image.height) {
        return image.clone();
    }
    resize(image, width, height)
}

/// Scales an image down with a box filter: each output pixel is the mean
/// of the input pixels it covers.
pub fn resize(image: &Image, width: u32, height: u32) -> Image {
    let width = width.max(1);
    let height = height.max(1);
    let fx = f64::from(image.width) / f64::from(width);
    let fy = f64::from(image.height) / f64::from(height);
    let span = |i: u32, factor: f64, limit: u32| -> (u32, u32) {
        let start = ((f64::from(i) * factor).floor() as u32).min(limit - 1);
        let end = ((f64::from(i + 1) * factor).ceil() as u32).clamp(start + 1, limit);
        (start, end)
    };

    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        let (y0, y1) = span(y, fy, image.height);
        for x in 0..width {
            let (x0, x1) = span(x, fx, image.width);
            let mut sum = [0u64; 4];
            for sy in y0..y1 {
                let row = sy as usize * image.width as usize;
                for sx in x0..x1 {
                    let at = (row + sx as usize) * 4;
                    for (channel, total) in sum.iter_mut().enumerate() {
                        *total += u64::from(image.rgba[at + channel]);
                    }
                }
            }
            let count = u64::from((y1 - y0) * (x1 - x0)).max(1);
            rgba.extend(sum.iter().map(|total| ((total + count / 2) / count) as u8));
        }
    }

    Image {
        width,
        height,
        rgba,
    }
}

/// How uniform an image is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Blankness {
    /// Whether at least [`BLANK_FRACTION`] of the pixels are near one colour.
    pub blank: bool,
    /// The share of pixels near the dominant colour.
    pub fraction: f64,
    /// The dominant colour (RGB, alpha ignored).
    pub color: [u8; 3],
}

impl Blankness {
    /// The dominant colour as `#rrggbb`.
    pub fn hex(&self) -> String {
        format!(
            "#{:02x}{:02x}{:02x}",
            self.color[0], self.color[1], self.color[2]
        )
    }

    /// A one-line description: `99.8% of pixels are #000000`.
    pub fn describe(&self) -> String {
        format!(
            "{:.1}% of pixels are {}",
            (self.fraction * 1000.0).floor() / 10.0,
            self.hex()
        )
    }
}

/// Measures how much of an image is one colour.
pub fn blankness(image: &Image) -> Blankness {
    let pixels = image.width as usize * image.height as usize;
    if pixels == 0 {
        return Blankness {
            blank: true,
            fraction: 1.0,
            color: [0, 0, 0],
        };
    }

    // The most common colour at 5 bits per channel, then the most common
    // exact colour inside it (anti-aliased edges must not shift it).
    let bin = |p: &[u8; 4]| {
        (usize::from(p[0] >> 3) << 10) | (usize::from(p[1] >> 3) << 5) | usize::from(p[2] >> 3)
    };
    let pixels_rgba = image.rgba.as_chunks::<4>().0;
    let mut counts = vec![0u32; 1 << 15];
    for pixel in pixels_rgba {
        counts[bin(pixel)] += 1;
    }
    let top = counts
        .iter()
        .enumerate()
        .max_by_key(|(_, count)| **count)
        .map(|(index, _)| index)
        .unwrap_or(0);
    let mut exact: std::collections::HashMap<[u8; 3], u32> = std::collections::HashMap::new();
    for pixel in pixels_rgba {
        if bin(pixel) == top {
            *exact.entry([pixel[0], pixel[1], pixel[2]]).or_default() += 1;
        }
    }
    let color = exact
        .into_iter()
        .max_by_key(|(color, count)| (*count, *color))
        .map(|(color, _)| color)
        .unwrap_or([0, 0, 0]);

    let near = pixels_rgba
        .iter()
        .filter(|pixel| {
            (0..3).all(|channel| pixel[channel].abs_diff(color[channel]) <= BLANK_TOLERANCE)
        })
        .count();
    let fraction = near as f64 / pixels as f64;

    Blankness {
        blank: fraction >= BLANK_FRACTION,
        fraction,
        color,
    }
}

/// What [`examine`] found out about a screenshot.
#[derive(Clone, Debug, PartialEq)]
pub struct Examined {
    /// The screenshot's size in pixels.
    pub size: (u32, u32),
    /// The preview's size in pixels.
    pub preview_size: (u32, u32),
    /// How uniform it is.
    pub blankness: Blankness,
}

impl Examined {
    /// Fields for the screenshot's `artifact` event: `bytes`, `size`, `blank`.
    pub fn artifact_fields(&self, path: &Path) -> serde_json::Map<String, Value> {
        let mut fields = serde_json::Map::new();
        if let Ok(meta) = std::fs::metadata(path) {
            let _ = fields.insert("bytes".into(), json!(meta.len()));
        }
        let _ = fields.insert("size".into(), json!([self.size.0, self.size.1]));
        let _ = fields.insert("blank".into(), json!(self.blankness.blank));
        fields
    }
}

/// Reads the screenshot at `shot`, writes its preview to `preview`, and
/// measures its blankness.
pub fn examine(shot: &Path, preview_path: &Path) -> io::Result<Examined> {
    let image = read(shot)?;
    let small = preview(&image);
    if (small.width, small.height) == (image.width, image.height) {
        // Small enough already: the preview is a copy.
        let _ = std::fs::copy(shot, preview_path)?;
    } else {
        write(preview_path, &small)?;
    }
    Ok(Examined {
        size: (image.width, image.height),
        preview_size: (small.width, small.height),
        // The preview has the same proportions and is far cheaper to scan.
        blankness: if small.width.max(small.height) <= PREVIEW_MAX {
            blankness(&small)
        } else {
            blankness(&image)
        },
    })
}

/// The preview path for a screenshot: `screen.png` → `screen.preview.png`.
pub fn preview_path(shot: &Path) -> std::path::PathBuf {
    let stem = shot
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "screen".to_string());
    shot.with_file_name(format!("{stem}.preview.png"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        let mut image = Image::filled(5, 3, [10, 20, 30, 255]);
        image.set_pixel(4, 2, [200, 100, 0, 128]);
        write(&path, &image).unwrap();
        assert_eq!(read(&path).unwrap(), image);
    }

    #[test]
    fn rgb_and_gray_pngs_decode_to_rgba() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rgb.png");
        let file = BufWriter::new(File::create(&path).unwrap());
        let mut encoder = png::Encoder::new(file, 2, 1);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[1, 2, 3, 4, 5, 6]).unwrap();
        writer.finish().unwrap();
        assert_eq!(read(&path).unwrap().rgba, vec![1, 2, 3, 255, 4, 5, 6, 255]);

        let path = dir.path().join("gray.png");
        let file = BufWriter::new(File::create(&path).unwrap());
        let mut encoder = png::Encoder::new(file, 2, 1);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[0, 255]).unwrap();
        writer.finish().unwrap();
        assert_eq!(
            read(&path).unwrap().rgba,
            vec![0, 0, 0, 255, 255, 255, 255, 255]
        );
    }

    #[test]
    fn previews_cap_the_long_edge_and_average() {
        // iPhone 17 at @3.
        let image = Image::filled(1206, 2622, [255, 255, 255, 255]);
        let small = preview(&image);
        assert_eq!((small.width, small.height), (471, 1024));
        assert_eq!(small.pixel(100, 100), [255, 255, 255, 255]);

        // Small images are kept.
        let image = Image::filled(800, 600, [1, 2, 3, 4]);
        assert_eq!(preview(&image), image);

        // A 2x2 checkerboard averages to grey.
        let mut image = Image::filled(2, 2, [0, 0, 0, 255]);
        image.set_pixel(0, 0, [255, 255, 255, 255]);
        image.set_pixel(1, 1, [255, 255, 255, 255]);
        let one = resize(&image, 1, 1);
        assert_eq!(one.pixel(0, 0), [128, 128, 128, 255]);
    }

    #[test]
    fn blank_images_are_detected() {
        let image = Image::filled(100, 100, [0, 0, 0, 255]);
        let found = blankness(&image);
        assert!(found.blank);
        assert_eq!(found.hex(), "#000000");
        assert_eq!(found.describe(), "100.0% of pixels are #000000");

        // Near-black noise is still blank.
        let mut noisy = image.clone();
        for x in 0..100 {
            noisy.set_pixel(x, 0, [6, 5, 4, 255]);
        }
        assert!(blankness(&noisy).blank);

        // A few lines of text are not.
        let mut text = Image::filled(100, 100, [255, 255, 255, 255]);
        for y in 10..12 {
            for x in 0..100 {
                text.set_pixel(x, y, [0, 0, 0, 255]);
            }
        }
        let found = blankness(&text);
        assert!(!found.blank, "{found:?}");
        assert_eq!(found.hex(), "#ffffff");
        assert!((found.fraction - 0.98).abs() < 1e-9);
    }

    #[test]
    fn examine_writes_the_preview() {
        let dir = tempfile::tempdir().unwrap();
        let shot = dir.path().join("screen.png");
        write(&shot, &Image::filled(2048, 1536, [9, 9, 9, 255])).unwrap();
        let preview = preview_path(&shot);
        assert_eq!(preview.file_name().unwrap(), "screen.preview.png");
        let examined = examine(&shot, &preview).unwrap();
        assert_eq!(examined.size, (2048, 1536));
        assert_eq!(examined.preview_size, (1024, 768));
        assert!(examined.blankness.blank);
        let back = read(&preview).unwrap();
        assert_eq!((back.width, back.height), (1024, 768));
        let fields = examined.artifact_fields(&shot);
        assert_eq!(fields["blank"], true);
        assert_eq!(fields["size"][0], 2048);
    }
}
