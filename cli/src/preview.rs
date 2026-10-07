//! Screenshots after capture (design §13.1, Appendix C item 25): decoding,
//! the token-saving `screen.preview.png` (long edge at most
//! [`PREVIEW_MAX`](crate::screen::PREVIEW_MAX) pixels), and blank detection
//! (`run.screen_blank`: at least 99.5 % of the pixels within a small
//! distance of one colour).
//!
//! Every platform's capture ends in [`finish`], which writes the preview,
//! reports the `screenshot` and `preview` artifacts, the result's `screen`
//! object, and the blank check.

use crate::catalogue::CheckId;
use crate::error::{Check, Evidence};
use crate::output::Reporter;
use crate::screen::{Screen, preview_size};
use serde_json::{Map, json};
use std::io::Cursor;
use std::path::Path;

/// The share of pixels of one colour from which a screenshot is blank.
pub const BLANK_THRESHOLD: f64 = 0.995;

/// An RGBA image, 8 bits per channel, rows top to bottom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

impl Image {
    /// A single-colour image.
    pub fn filled(width: u32, height: u32, rgba: [u8; 4]) -> Image {
        Image {
            width,
            height,
            rgba: rgba.repeat((width as usize) * (height as usize)),
        }
    }

    fn pixel(&self, x: u32, y: u32) -> &[u8] {
        let index = ((y as usize) * (self.width as usize) + x as usize) * 4;
        &self.rgba[index..index + 4]
    }
}

/// Decodes a PNG into RGBA8, whatever its colour type and depth.
pub fn decode(bytes: &[u8]) -> Result<Image, String> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder
        .read_info()
        .map_err(|error| format!("not a PNG: {error}"))?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| "the PNG is too large".to_string())?;
    let mut buffer = vec![0; size];
    let info = reader
        .next_frame(&mut buffer)
        .map_err(|error| format!("cannot decode the PNG: {error}"))?;
    buffer.truncate(info.buffer_size());

    let pixels = (info.width as usize) * (info.height as usize);
    let rgba = match info.color_type {
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
        png::ColorType::Indexed => {
            return Err("an indexed PNG was not expanded".to_string());
        }
    };
    if rgba.len() != pixels * 4 {
        return Err(format!(
            "decoded {} bytes for {}x{} pixels",
            rgba.len(),
            info.width,
            info.height
        ));
    }
    Ok(Image {
        width: info.width,
        height: info.height,
        rgba,
    })
}

/// Encodes an RGBA8 image as a PNG.
pub fn encode(image: &Image) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, image.width, image.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder
            .write_header()
            .map_err(|error| format!("cannot encode the PNG: {error}"))?;
        writer
            .write_image_data(&image.rgba)
            .map_err(|error| format!("cannot encode the PNG: {error}"))?;
    }
    Ok(out)
}

/// Scales an image down to `width` x `height` by averaging the source
/// pixels each output pixel covers (never up).
pub fn downscale(image: &Image, width: u32, height: u32) -> Image {
    let width = width.clamp(1, image.width.max(1));
    let height = height.clamp(1, image.height.max(1));
    if width == image.width && height == image.height {
        return image.clone();
    }

    let span = |index: u32, out: u32, source: u32| -> (u32, u32) {
        let start = (u64::from(index) * u64::from(source) / u64::from(out)) as u32;
        let end = (u64::from(index + 1) * u64::from(source) / u64::from(out)) as u32;
        (start, end.max(start + 1).min(source))
    };

    let mut rgba = Vec::with_capacity((width as usize) * (height as usize) * 4);
    for oy in 0..height {
        let (y0, y1) = span(oy, height, image.height);
        for ox in 0..width {
            let (x0, x1) = span(ox, width, image.width);
            let mut sum = [0u64; 4];
            let mut count = 0u64;
            for y in y0..y1 {
                for x in x0..x1 {
                    for (total, value) in sum.iter_mut().zip(image.pixel(x, y)) {
                        *total += u64::from(*value);
                    }
                    count += 1;
                }
            }
            let count = count.max(1);
            rgba.extend(sum.iter().map(|total| ((total + count / 2) / count) as u8));
        }
    }

    Image {
        width,
        height,
        rgba,
    }
}

/// How much of an image is one colour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Blankness {
    /// The share of pixels close to the dominant colour (0 to 1).
    pub fraction: f64,
    /// The dominant colour.
    pub color: [u8; 3],
}

impl Blankness {
    /// Whether the image counts as blank.
    pub fn is_blank(&self) -> bool {
        self.fraction >= BLANK_THRESHOLD
    }

    /// `#RRGGBB`.
    pub fn hex(&self) -> String {
        format!(
            "#{:02X}{:02X}{:02X}",
            self.color[0], self.color[1], self.color[2]
        )
    }
}

/// Measures how much of an image is its dominant colour. Colours are
/// bucketed at 5 bits per channel; a pixel counts when every channel is
/// within one bucket of the dominant one (about 8 levels of 255).
pub fn blankness(image: &Image) -> Blankness {
    let bucket = |p: &[u8]| -> usize {
        (usize::from(p[0] >> 3) << 10) | (usize::from(p[1] >> 3) << 5) | usize::from(p[2] >> 3)
    };

    let mut counts = vec![0u32; 1 << 15];
    let pixels = image.rgba.as_chunks::<4>().0;
    for pixel in pixels {
        counts[bucket(pixel)] += 1;
    }
    let (mode, _) = counts
        .iter()
        .enumerate()
        .max_by_key(|(_, count)| **count)
        .unwrap_or((0, &0));
    let channels = [(mode >> 10) & 31, (mode >> 5) & 31, mode & 31];

    let mut close = 0u64;
    let mut sums = [0u64; 3];
    let mut exact = 0u64;
    for pixel in pixels {
        let near = (0..3).all(|c| (usize::from(pixel[c] >> 3)).abs_diff(channels[c]) <= 1);
        if near {
            close += 1;
        }
        if bucket(pixel) == mode {
            exact += 1;
            for c in 0..3 {
                sums[c] += u64::from(pixel[c]);
            }
        }
    }

    let total = (image.rgba.len() / 4) as u64;
    let mean = |sum: u64| sum.checked_div(exact).unwrap_or(0) as u8;
    let color = [mean(sums[0]), mean(sums[1]), mean(sums[2])];
    Blankness {
        fraction: if total == 0 {
            1.0
        } else {
            close as f64 / total as f64
        },
        color,
    }
}

/// A processed screenshot.
#[derive(Clone, Debug)]
pub struct Shot {
    /// The screen's sizes (device pixels, points, preview).
    pub screen: Screen,
    /// How blank it is.
    pub blankness: Blankness,
    /// The size of `screen.png` in bytes.
    pub bytes: u64,
}

/// Reads `png`, writes the preview next to it (or at `preview`), and
/// measures blankness. `scale` is device pixels per point (CSS pixel on
/// the web).
pub fn process(png: &Path, preview: &Path, scale: f64) -> Result<Shot, String> {
    let bytes =
        std::fs::read(png).map_err(|error| format!("cannot read {}: {error}", png.display()))?;
    let image = decode(&bytes)?;
    let screen = Screen::new((image.width, image.height), scale);

    let (width, height) = preview_size((image.width, image.height));
    let preview_bytes = if (width, height) == (image.width, image.height) {
        bytes.clone()
    } else {
        encode(&downscale(&image, width, height))?
    };
    if let Some(parent) = preview.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(preview, preview_bytes)
        .map_err(|error| format!("cannot write {}: {error}", preview.display()))?;

    Ok(Shot {
        screen,
        blankness: blankness(&image),
        bytes: bytes.len() as u64,
    })
}

/// The preview path for a screenshot: `screen.png` → `screen.preview.png`.
pub fn preview_path(png: &Path) -> std::path::PathBuf {
    let stem = png
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "screen".to_string());
    png.with_file_name(format!("{stem}.preview.png"))
}

/// Writes the preview of `png` and reports it: the `screenshot` and
/// `preview` artifacts, the result's `screen`, and `run.screen_blank` (a
/// WARN, or a FAIL with `expect_content`). `fix` is what the blank check
/// suggests running.
pub fn finish(
    rep: &Reporter,
    png: &Path,
    scale: f64,
    expect_content: bool,
    fix: &[&str],
) -> Result<Shot, String> {
    let preview = preview_path(png);
    let shot = process(png, &preview, scale)?;

    let mut extra = Map::new();
    let _ = extra.insert("bytes".into(), json!(shot.bytes));
    let _ = extra.insert("blank".into(), json!(shot.blankness.is_blank()));
    rep.artifact_with("screenshot", png, extra);
    rep.artifact("preview", &preview);
    rep.set("screen", shot.screen.to_json());

    if shot.blankness.is_blank() {
        let detail = format!(
            "{:.1}% of pixels are {}",
            shot.blankness.fraction * 100.0,
            shot.blankness.hex()
        );
        let check = if expect_content {
            Check::fail(CheckId::RunScreenBlank, detail)
        } else {
            Check::warn(CheckId::RunScreenBlank, detail)
        };
        rep.check(check.evidence(Evidence::file(png)).fix(
            "Compare with `icm shot --headless`; check fonts and theme; read the logs.",
            fix,
        ));
    }
    Ok(shot)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checker(width: u32, height: u32) -> Image {
        let mut rgba = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let on = (x + y) % 2 == 0;
                rgba.extend(if on {
                    [255, 255, 255, 255]
                } else {
                    [0, 0, 0, 255]
                });
            }
        }
        Image {
            width,
            height,
            rgba,
        }
    }

    #[test]
    fn png_round_trips() {
        let image = checker(7, 5);
        let decoded = decode(&encode(&image).unwrap()).unwrap();
        assert_eq!(decoded, image);
        assert!(decode(b"not a png").is_err());
    }

    #[test]
    fn downscaling_averages() {
        let image = checker(4, 4);
        let small = downscale(&image, 2, 2);
        assert_eq!((small.width, small.height), (2, 2));
        // Each output pixel covers two white and two black pixels.
        for pixel in small.rgba.as_chunks::<4>().0 {
            assert!((126..=129).contains(&pixel[0]), "{pixel:?}");
            assert_eq!(pixel[3], 255);
        }
        // Never up.
        assert_eq!(downscale(&image, 8, 8), image);
    }

    #[test]
    fn blank_screens_are_detected() {
        let black = Image::filled(100, 100, [0, 0, 0, 255]);
        let measured = blankness(&black);
        assert!(measured.is_blank());
        assert_eq!(measured.hex(), "#000000");

        // A little noise is still blank; a real UI is not.
        let mut noisy = Image::filled(100, 100, [255, 255, 255, 255]);
        noisy.rgba[0..4].copy_from_slice(&[250, 250, 250, 255]);
        assert!(blankness(&noisy).is_blank());

        let mut ui = Image::filled(100, 100, [255, 255, 255, 255]);
        for pixel in ui.rgba.as_chunks_mut::<4>().0.iter_mut().take(2_000) {
            pixel.copy_from_slice(&[30, 30, 200, 255]);
        }
        let measured = blankness(&ui);
        assert!(!measured.is_blank(), "{measured:?}");
        assert!((measured.fraction - 0.8).abs() < 0.001);
    }

    #[test]
    fn previews_cap_the_long_edge() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("screen.png");
        std::fs::write(
            &png,
            encode(&Image::filled(2048, 1000, [10, 20, 30, 255])).unwrap(),
        )
        .unwrap();
        let preview = preview_path(&png);
        assert!(preview.ends_with("screen.preview.png"));
        let shot = process(&png, &preview, 2.0).unwrap();
        assert_eq!(shot.screen.px, (2048, 1000));
        assert_eq!(shot.screen.preview, (1024, 500));
        let small = decode(&std::fs::read(&preview).unwrap()).unwrap();
        assert_eq!((small.width, small.height), (1024, 500));
        assert!(shot.blankness.is_blank());
    }
}
