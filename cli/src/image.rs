//! Screenshots as images (design §13.1): PNG decode and encode, the preview
//! (long edge at most [`PREVIEW_MAX`](crate::screen::PREVIEW_MAX) pixels, to
//! save an agent's tokens), cropping, and blank detection: at least 99.5 %
//! of the pixels within a small distance of one colour is
//! `run.screen_blank`.

use std::fs::File;
use std::io::{self, BufReader, BufWriter};
use std::path::Path;

/// The share of pixels that makes a screenshot blank.
pub const BLANK_FRACTION: f64 = 0.995;

/// How far (per channel) a pixel may be from the dominant colour and still
/// count as that colour.
pub const BLANK_TOLERANCE: u8 = 8;

/// An 8-bit RGBA image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Row-major RGBA, 4 bytes per pixel.
    pub rgba: Vec<u8>,
}

/// What blank detection found.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Blank {
    /// The most common colour (RGB).
    pub color: [u8; 3],
    /// The share of pixels within [`BLANK_TOLERANCE`] of it.
    pub fraction: f64,
}

impl Blank {
    /// Whether the image is a single colour.
    pub fn is_blank(&self) -> bool {
        self.fraction >= BLANK_FRACTION
    }

    /// The colour as `#RRGGBB`.
    pub fn hex(&self) -> String {
        format!(
            "#{:02X}{:02X}{:02X}",
            self.color[0], self.color[1], self.color[2]
        )
    }

    /// `99.8% of pixels are #000000`.
    pub fn describe(&self) -> String {
        format!("{:.1}% of pixels are {}", self.fraction * 100.0, self.hex())
    }
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

impl Image {
    /// An image of one colour (tests, placeholders).
    pub fn filled(width: u32, height: u32, rgba: [u8; 4]) -> Image {
        let pixels = (width as usize) * (height as usize);
        Image {
            width,
            height,
            rgba: rgba.repeat(pixels),
        }
    }

    /// Reads a PNG of any colour type and depth as 8-bit RGBA.
    pub fn read_png(path: &Path) -> io::Result<Image> {
        let file = File::open(path)?;
        let mut decoder = png::Decoder::new(BufReader::new(file));
        decoder.set_transformations(png::Transformations::normalize_to_color8());
        let mut reader = decoder
            .read_info()
            .map_err(|error| invalid(format!("{}: {error}", path.display())))?;
        let size = reader
            .output_buffer_size()
            .ok_or_else(|| invalid(format!("{}: the image is too large", path.display())))?;
        let mut buffer = vec![0; size];
        let info = reader
            .next_frame(&mut buffer)
            .map_err(|error| invalid(format!("{}: {error}", path.display())))?;
        buffer.truncate(info.buffer_size());

        let pixels = (info.width as usize) * (info.height as usize);
        let rgba = match info.color_type {
            png::ColorType::Rgba => buffer,
            png::ColorType::Rgb => buffer
                .as_chunks::<3>()
                .0
                .iter()
                .flat_map(|[r, g, b]| [*r, *g, *b, 255])
                .collect(),
            png::ColorType::Grayscale => buffer.iter().flat_map(|g| [*g, *g, *g, 255]).collect(),
            png::ColorType::GrayscaleAlpha => buffer
                .as_chunks::<2>()
                .0
                .iter()
                .flat_map(|[g, a]| [*g, *g, *g, *a])
                .collect(),
            png::ColorType::Indexed => {
                return Err(invalid(format!(
                    "{}: an indexed PNG was not expanded",
                    path.display()
                )));
            }
        };
        if rgba.len() != pixels * 4 {
            return Err(invalid(format!(
                "{}: the image data is truncated",
                path.display()
            )));
        }

        Ok(Image {
            width: info.width,
            height: info.height,
            rgba,
        })
    }

    /// Writes the image as an RGBA PNG.
    pub fn write_png(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = File::create(path)?;
        let mut encoder = png::Encoder::new(BufWriter::new(file), self.width, self.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(io::Error::other)?;
        writer
            .write_image_data(&self.rgba)
            .map_err(io::Error::other)?;
        writer.finish().map_err(io::Error::other)
    }

    /// The rectangle `(x, y, width, height)`, clamped to the image.
    pub fn crop(&self, x: u32, y: u32, width: u32, height: u32) -> Image {
        let x = x.min(self.width);
        let y = y.min(self.height);
        let width = width.min(self.width - x);
        let height = height.min(self.height - y);

        let mut rgba = Vec::with_capacity((width as usize) * (height as usize) * 4);
        for row in y..y + height {
            let start = ((row as usize) * (self.width as usize) + x as usize) * 4;
            rgba.extend_from_slice(&self.rgba[start..start + (width as usize) * 4]);
        }
        Image {
            width,
            height,
            rgba,
        }
    }

    /// The image scaled down (never up) to `width` x `height` by averaging
    /// each output pixel's source area.
    pub fn resize(&self, width: u32, height: u32) -> Image {
        if (width, height) == (self.width, self.height) || width == 0 || height == 0 {
            return self.clone();
        }

        let source_width = self.width as usize;
        let span = |out: u32, out_len: u32, in_len: u32| -> (usize, usize) {
            let start = (u64::from(out) * u64::from(in_len) / u64::from(out_len)) as usize;
            let end = (u64::from(out + 1) * u64::from(in_len) / u64::from(out_len)) as usize;
            (start, end.max(start + 1).min(in_len as usize))
        };

        let mut rgba = Vec::with_capacity((width as usize) * (height as usize) * 4);
        for out_y in 0..height {
            let (y0, y1) = span(out_y, height, self.height);
            for out_x in 0..width {
                let (x0, x1) = span(out_x, width, self.width);
                let mut sum = [0u64; 4];
                for y in y0..y1 {
                    let row = y * source_width;
                    for x in x0..x1 {
                        let index = (row + x) * 4;
                        for (channel, total) in sum.iter_mut().enumerate() {
                            *total += u64::from(self.rgba[index + channel]);
                        }
                    }
                }
                let count = ((y1 - y0) * (x1 - x0)) as u64;
                for total in sum {
                    rgba.push(((total + count / 2) / count) as u8);
                }
            }
        }

        Image {
            width,
            height,
            rgba,
        }
    }

    /// The preview: the long edge scaled down to
    /// [`PREVIEW_MAX`](crate::screen::PREVIEW_MAX), never up.
    pub fn preview(&self) -> Image {
        let (width, height) = crate::screen::preview_size((self.width, self.height));
        self.resize(width, height)
    }

    /// The dominant colour and how much of the image it covers. Transparent
    /// pixels count as their colour; screenshots are opaque.
    pub fn blank(&self) -> Blank {
        let pixels = self.rgba.as_chunks::<4>().0;
        let total = pixels.len();
        if total == 0 {
            return Blank {
                color: [0, 0, 0],
                fraction: 1.0,
            };
        }

        // The most common colour at 5 bits per channel, then its exact
        // average, then every pixel near that.
        let bin = |p: &[u8; 4]| {
            ((usize::from(p[0]) >> 3) << 10)
                | ((usize::from(p[1]) >> 3) << 5)
                | (usize::from(p[2]) >> 3)
        };
        let mut counts = vec![0u32; 1 << 15];
        for pixel in pixels {
            counts[bin(pixel)] += 1;
        }
        let dominant = counts
            .iter()
            .enumerate()
            .max_by_key(|(_, count)| **count)
            .map(|(index, _)| index)
            .unwrap_or(0);

        let mut sum = [0u64; 3];
        let mut members = 0u64;
        for pixel in pixels.iter().filter(|p| bin(p) == dominant) {
            for (total, value) in sum.iter_mut().zip(pixel) {
                *total += u64::from(*value);
            }
            members += 1;
        }
        let members = members.max(1);
        let color = [
            ((sum[0] + members / 2) / members) as u8,
            ((sum[1] + members / 2) / members) as u8,
            ((sum[2] + members / 2) / members) as u8,
        ];

        let near = pixels
            .iter()
            .filter(|p| (0..3).all(|c| p[c].abs_diff(color[c]) <= BLANK_TOLERANCE))
            .count();

        Blank {
            color,
            fraction: near as f64 / total as f64,
        }
    }
}

/// What [`write_preview`] found.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Preview {
    /// The screenshot's size in pixels.
    pub px: (u32, u32),
    /// The preview's size in pixels.
    pub preview: (u32, u32),
    /// Blank detection on the screenshot.
    pub blank: Blank,
}

/// Reads `screen`, writes its preview to `preview`, and looks for a blank
/// screen.
pub fn write_preview(screen: &Path, preview: &Path) -> io::Result<Preview> {
    let image = Image::read_png(screen)?;
    let small = image.preview();
    small.write_png(preview)?;
    Ok(Preview {
        px: (image.width, image.height),
        preview: (small.width, small.height),
        blank: image.blank(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checker(width: u32, height: u32) -> Image {
        let mut image = Image::filled(width, height, [255, 255, 255, 255]);
        for y in 0..height {
            for x in 0..width {
                if (x / 4 + y / 4) % 2 == 0 {
                    let i = ((y * width + x) * 4) as usize;
                    image.rgba[i..i + 3].copy_from_slice(&[0, 0, 0]);
                }
            }
        }
        image
    }

    #[test]
    fn pngs_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        let image = checker(37, 21);
        image.write_png(&path).unwrap();
        assert_eq!(Image::read_png(&path).unwrap(), image);
    }

    #[test]
    fn previews_cap_the_long_edge_and_average() {
        let image = Image::filled(2048, 1536, [10, 20, 30, 255]);
        let preview = image.preview();
        assert_eq!((preview.width, preview.height), (1024, 768));
        assert_eq!(&preview.rgba[..4], &[10, 20, 30, 255]);

        // A 2x2 block of black and white averages to grey.
        let mut image = Image::filled(2, 2, [255, 255, 255, 255]);
        image.rgba[..4].copy_from_slice(&[0, 0, 0, 255]);
        image.rgba[12..16].copy_from_slice(&[0, 0, 0, 255]);
        let one = image.resize(1, 1);
        assert_eq!(one.rgba, vec![128, 128, 128, 255]);

        // Small images are kept as they are.
        let small = checker(300, 200);
        assert_eq!(small.preview(), small);
    }

    #[test]
    fn crops_clamp_to_the_image() {
        let image = checker(16, 16);
        let top = image.crop(0, 4, 16, 100);
        assert_eq!((top.width, top.height), (16, 12));
        assert_eq!(&top.rgba[..], &image.rgba[16 * 4 * 4..]);
        let empty = image.crop(20, 20, 5, 5);
        assert_eq!((empty.width, empty.height), (0, 0));
    }

    #[test]
    fn blank_detection() {
        let black = Image::filled(100, 100, [0, 0, 0, 255]);
        let blank = black.blank();
        assert!(blank.is_blank());
        assert_eq!(blank.hex(), "#000000");
        assert_eq!(blank.describe(), "100.0% of pixels are #000000");

        // Nearly uniform: noise within the tolerance is still blank.
        let mut noisy = Image::filled(100, 100, [250, 250, 250, 255]);
        noisy.rgba[0] = 255;
        noisy.rgba[4] = 245;
        assert!(noisy.blank().is_blank());

        // A real UI is not.
        assert!(!checker(64, 64).blank().is_blank());

        // 1 % of the pixels drawing something is enough.
        let mut text = Image::filled(100, 100, [255, 255, 255, 255]);
        text.rgba[..400].fill(0);
        let found = text.blank();
        assert!(!found.is_blank(), "{found:?}");
        assert_eq!(found.hex(), "#FFFFFF");
    }

    #[test]
    fn write_preview_reports_sizes() {
        let dir = tempfile::tempdir().unwrap();
        let screen = dir.path().join("screen.png");
        let preview = dir.path().join("screen.preview.png");
        checker(2000, 1000).write_png(&screen).unwrap();
        let written = write_preview(&screen, &preview).unwrap();
        assert_eq!(written.px, (2000, 1000));
        assert_eq!(written.preview, (1024, 512));
        assert!(!written.blank.is_blank());
        let read = Image::read_png(&preview).unwrap();
        assert_eq!((read.width, read.height), (1024, 512));
    }
}
