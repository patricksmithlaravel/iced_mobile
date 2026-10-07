//! The desktop icons (design §9.6): `[app] icon` (a square PNG of at least
//! 1024 px; the template's placeholder when it is unset) resampled to each
//! size the platforms want, with its alpha kept. Desktop icons may be
//! transparent; only iOS flattens them.
//!
//! - macOS: an `.iconset` directory for `iconutil -c icns` ([`iconset`]);
//! - Windows: an ICO of PNG frames from 16 to 256 px ([`ico`]);
//! - Linux: hicolor PNGs ([`HICOLOR`]).

use crate::catalogue::CheckId;
use crate::context::Project;
use crate::error::{IcmError, Result};
use crate::raster::Image;
use std::io::Cursor;
use std::path::Path;

/// The hicolor sizes a Linux package installs.
pub const HICOLOR: &[u32] = &[16, 32, 48, 64, 128, 256, 512];

/// The frames of the Windows ICO.
pub const ICO_SIZES: &[u32] = &[16, 24, 32, 48, 64, 128, 256];

/// The app's icon.
#[derive(Clone, Debug)]
pub struct Icon {
    /// The full-size image.
    pub image: Image,
    /// Where it came from (`assets/icon.png`, or the placeholder).
    pub source: String,
}

fn invalid(detail: String) -> IcmError {
    IcmError::new(CheckId::AppIconInvalid, detail)
}

/// Decodes a PNG of any colour type into 8-bit RGBA.
pub fn decode(bytes: &[u8]) -> std::result::Result<Image, String> {
    let mut decoder = png::Decoder::new_with_limits(
        Cursor::new(bytes),
        png::Limits {
            bytes: 512 * 1024 * 1024,
        },
    );
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| "the PNG is too large".to_string())?;
    let mut buffer = vec![0; size];
    let info = reader.next_frame(&mut buffer).map_err(|e| e.to_string())?;
    buffer.truncate(info.buffer_size());
    let rgba: Vec<u8> = match info.color_type {
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
        png::ColorType::Indexed => return Err("an indexed PNG was not expanded".to_string()),
    };
    if rgba.len() != info.width as usize * info.height as usize * 4 {
        return Err(format!(
            "decoded {} bytes for {}x{}",
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

/// Encodes an RGBA PNG.
pub fn encode(image: &Image) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, image.width, image.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Balanced);
        let mut writer = encoder.write_header().expect("a valid PNG header");
        writer
            .write_image_data(&image.rgba)
            .expect("the image matches its size");
        writer.finish().expect("the PNG is complete");
    }
    bytes
}

/// The project's icon, or the template's placeholder when `[app] icon` is
/// unset (the release core has already reported that).
pub fn load(project: &Project) -> Result<Icon> {
    let (bytes, source) = match project.app().icon.as_deref() {
        Some(icon) => {
            let path = project.dir().join(icon);
            let bytes = std::fs::read(&path).map_err(|error| {
                invalid(format!(
                    "cannot read {}: {error}",
                    crate::paths::display(&path)
                ))
            })?;
            (bytes, crate::paths::display(&path))
        }
        None => (
            crate::template::file("assets/icon.png")
                .ok_or_else(|| {
                    IcmError::new(
                        CheckId::InternalBug,
                        "the template's placeholder icon is not embedded",
                    )
                })?
                .to_vec(),
            "the template's placeholder icon".to_string(),
        ),
    };
    let image = decode(&bytes).map_err(|error| invalid(format!("{source}: {error}")))?;
    if image.width != image.height {
        return Err(invalid(format!(
            "{source} is {}x{}; it must be square",
            image.width, image.height
        )));
    }
    Ok(Icon { image, source })
}

/// Scales a square image to `size` px with a box filter over
/// premultiplied colour, so transparent pixels do not darken the edges.
pub fn resize(image: &Image, size: u32) -> Image {
    if image.width == size && image.height == size {
        return image.clone();
    }
    let size = size.max(1);
    let fx = f64::from(image.width) / f64::from(size);
    let fy = f64::from(image.height) / f64::from(size);
    let span = |i: u32, factor: f64, limit: u32| -> (u32, u32) {
        let start = ((f64::from(i) * factor).floor() as u32).min(limit - 1);
        let end = ((f64::from(i + 1) * factor).ceil() as u32).clamp(start + 1, limit);
        (start, end)
    };
    let mut rgba = Vec::with_capacity(size as usize * size as usize * 4);
    for y in 0..size {
        let (y0, y1) = span(y, fy, image.height);
        for x in 0..size {
            let (x0, x1) = span(x, fx, image.width);
            let mut sum = [0u64; 4];
            for sy in y0..y1 {
                let row = sy as usize * image.width as usize;
                for sx in x0..x1 {
                    let at = (row + sx as usize) * 4;
                    let alpha = u64::from(image.rgba[at + 3]);
                    for (total, value) in sum.iter_mut().zip(&image.rgba[at..at + 3]) {
                        *total += u64::from(*value) * alpha;
                    }
                    sum[3] += alpha;
                }
            }
            let count = u64::from((y1 - y0) * (x1 - x0)).max(1);
            let alpha = sum[3];
            for total in &sum[..3] {
                rgba.push(
                    (total + alpha / 2)
                        .checked_div(alpha)
                        .map_or(0, |value| value.min(255) as u8),
                );
            }
            rgba.push(((alpha + count / 2) / count).min(255) as u8);
        }
    }
    Image {
        width: size,
        height: size,
        rgba,
    }
}

/// The PNG of the icon at `size` px.
pub fn png(icon: &Icon, size: u32) -> Vec<u8> {
    encode(&resize(&icon.image, size))
}

/// The files of a macOS `.iconset` (`iconutil -c icns` turns it into
/// `AppIcon.icns`): `(name, size in px)`.
pub const ICONSET: &[(&str, u32)] = &[
    ("icon_16x16.png", 16),
    ("icon_16x16@2x.png", 32),
    ("icon_32x32.png", 32),
    ("icon_32x32@2x.png", 64),
    ("icon_128x128.png", 128),
    ("icon_128x128@2x.png", 256),
    ("icon_256x256.png", 256),
    ("icon_256x256@2x.png", 512),
    ("icon_512x512.png", 512),
    ("icon_512x512@2x.png", 1024),
];

/// Writes the iconset into `dir` (emptied first).
pub fn iconset(icon: &Icon, dir: &Path) -> Result<()> {
    super::fresh_dir(dir)?;
    for (name, size) in ICONSET {
        super::write_file(&dir.join(name), &png(icon, *size), 0o644)?;
    }
    Ok(())
}

/// A Windows ICO whose frames are PNGs at [`ICO_SIZES`] (Windows Vista and
/// later read PNG frames at every size).
pub fn ico(icon: &Icon) -> Vec<u8> {
    let frames: Vec<(u32, Vec<u8>)> = ICO_SIZES
        .iter()
        .map(|&size| (size, png(icon, size)))
        .collect();
    let count = frames.len() as u16;
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    out.extend_from_slice(&count.to_le_bytes());
    let mut offset = 6 + 16 * u32::from(count);
    for (size, data) in &frames {
        let edge = if *size >= 256 { 0u8 } else { *size as u8 };
        out.push(edge); // width (0 means 256)
        out.push(edge); // height
        out.push(0); // palette colours
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += data.len() as u32;
    }
    for (_, data) in frames {
        out.extend_from_slice(&data);
    }
    out
}

/// The frames of an ICO: `(width, height, PNG bytes)` (tests and verify).
pub fn ico_frames(bytes: &[u8]) -> Option<Vec<(u32, u32, Vec<u8>)>> {
    let get16 = |at: usize| Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?));
    let get32 = |at: usize| Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
    if get16(0)? != 0 || get16(2)? != 1 {
        return None;
    }
    let count = get16(4)? as usize;
    let mut frames = Vec::with_capacity(count);
    for index in 0..count {
        let entry = 6 + index * 16;
        let edge = |byte: u8| if byte == 0 { 256 } else { u32::from(byte) };
        let width = edge(*bytes.get(entry)?);
        let height = edge(*bytes.get(entry + 1)?);
        let len = get32(entry + 8)? as usize;
        let at = get32(entry + 12)? as usize;
        frames.push((width, height, bytes.get(at..at.checked_add(len)?)?.to_vec()));
    }
    Some(frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn icon() -> Icon {
        // A transparent square with an opaque red centre.
        let mut image = Image::filled(1024, 1024, [0, 0, 0, 0]);
        for y in 256..768 {
            for x in 256..768 {
                image.set_pixel(x, y, [255, 0, 0, 255]);
            }
        }
        Icon {
            image,
            source: "test".into(),
        }
    }

    #[test]
    fn resizing_keeps_alpha_without_dark_fringes() {
        let small = resize(&icon().image, 16);
        assert_eq!((small.width, small.height), (16, 16));
        assert_eq!(small.pixel(0, 0)[3], 0, "the corner stays transparent");
        assert_eq!(small.pixel(8, 8), [255, 0, 0, 255]);
        // An edge pixel half covered: still pure red, half transparent.
        let edge = resize(&icon().image, 3);
        let [r, g, b, a] = edge.pixel(1, 1);
        assert_eq!((r, g, b), (255, 0, 0));
        assert!(a > 0);
        let png = png(&icon(), 64);
        let decoded = decode(&png).unwrap();
        assert_eq!((decoded.width, decoded.height), (64, 64));
    }

    #[test]
    fn the_iconset_has_every_size() {
        let dir = tempfile::tempdir().unwrap();
        let set = dir.path().join("AppIcon.iconset");
        iconset(&icon(), &set).unwrap();
        for (name, size) in ICONSET {
            let bytes = std::fs::read(set.join(name)).unwrap();
            let image = decode(&bytes).unwrap();
            assert_eq!(image.width, *size, "{name}");
        }
    }

    #[test]
    fn the_ico_holds_png_frames() {
        let bytes = ico(&icon());
        let frames = ico_frames(&bytes).unwrap();
        let sizes: Vec<u32> = frames.iter().map(|(w, _, _)| *w).collect();
        assert_eq!(sizes, ICO_SIZES);
        for (width, height, png) in frames {
            assert_eq!(width, height);
            assert!(png.starts_with(b"\x89PNG"));
            assert_eq!(decode(&png).unwrap().width, width);
        }
        assert!(ico_frames(b"nope").is_none());
    }
}
