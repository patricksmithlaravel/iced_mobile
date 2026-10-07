//! The generated resource tree (design §9.4): the theme, the window's
//! background and bar icons in light and dark mode, the icon's background
//! colour, legacy launcher icons for every density, and adaptive icons
//! whose foreground is the app icon inside the 66 % safe zone.
//!
//! The tree is hash-stamped: unchanged inputs leave it (and aapt2's
//! compiled output) alone.

use super::image::{self, Rgba};
use super::manifest;
use crate::catalogue::CheckId;
use crate::error::{Check, Evidence, IcmError};
use std::path::{Path, PathBuf};

/// Bump when the generated files change, so old stamps are invalidated.
const GENERATOR: &str = "icm-android-res/3";

/// Densities and their scale against mdpi.
pub const DENSITIES: &[(&str, f64)] = &[
    ("mdpi", 1.0),
    ("hdpi", 1.5),
    ("xhdpi", 2.0),
    ("xxhdpi", 3.0),
    ("xxxhdpi", 4.0),
];

/// A legacy launcher icon's size at mdpi, in dp.
pub const LEGACY_DP: f64 = 48.0;
/// An adaptive icon layer's size at mdpi, in dp.
pub const ADAPTIVE_DP: f64 = 108.0;
/// The share of the adaptive layer the icon fills (the safe zone).
pub const SAFE_ZONE: f64 = 0.66;

/// What the resource tree is made from.
#[derive(Clone, Debug)]
pub struct Inputs {
    /// The icon PNG, if `[app] icon` is set.
    pub icon: Option<PathBuf>,
    /// `[app] background`, `#RRGGBB`.
    pub background: String,
}

/// The result of [`generate`].
#[derive(Clone, Debug)]
pub struct Generated {
    /// The resource directory (`aapt2 compile --dir`).
    pub dir: PathBuf,
    /// Whether anything was rewritten.
    pub changed: bool,
    /// Findings about the icon (WARNs).
    pub checks: Vec<Check>,
}

fn icon_error(path: &Path, detail: String) -> IcmError {
    IcmError::new(CheckId::AppIconInvalid, detail).evidence(Evidence::file(path))
}

/// Writes `<gen>/res` unless its stamp says it is current.
pub fn generate(gen_dir: &Path, inputs: &Inputs) -> Result<Generated, IcmError> {
    let dir = gen_dir.join("res");
    let background = image::parse_hex_color(&inputs.background).ok_or_else(|| {
        IcmError::new(
            CheckId::ConfigInvalid,
            format!("`[app] background` is `{}`, not #RRGGBB", inputs.background),
        )
    })?;

    let icon_bytes = match &inputs.icon {
        Some(path) => Some(std::fs::read(path).map_err(|error| {
            icon_error(
                path,
                format!("cannot read the icon {}: {error}", path.display()),
            )
        })?),
        None => None,
    };

    let mut stamp_input = format!("{GENERATOR}\n{}\n", inputs.background).into_bytes();
    if let Some(bytes) = &icon_bytes {
        stamp_input.extend_from_slice(bytes);
    }
    let stamp = crate::hash::sha256_hex(&stamp_input);
    let stamp_path = gen_dir.join("res.stamp");

    let mut checks = Vec::new();
    let icon = match (&inputs.icon, &icon_bytes) {
        (Some(path), Some(bytes)) => {
            let decoded = image::decode(bytes).map_err(|error| {
                icon_error(
                    path,
                    format!("{} is not a readable PNG: {error}", path.display()),
                )
            })?;
            if decoded.width != decoded.height || decoded.width < 1024 {
                checks.push(
                    Check::warn(
                        CheckId::AppIconInvalid,
                        format!(
                            "{} is {}x{}; stores need a square PNG of at least 1024x1024",
                            crate::paths::display(path),
                            decoded.width,
                            decoded.height
                        ),
                    )
                    .evidence(Evidence::file(path)),
                );
            }
            Some(decoded.flatten(background))
        }
        _ => None,
    };

    let current = std::fs::read_to_string(&stamp_path).is_ok_and(|text| text.trim() == stamp);
    if current && dir.is_dir() {
        return Ok(Generated {
            dir,
            changed: false,
            checks,
        });
    }

    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|error| io_error(&dir, &error))?;
    }
    let write = |relative: &str, bytes: &[u8]| -> Result<(), IcmError> {
        let path = dir.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| io_error(parent, &error))?;
        }
        std::fs::write(&path, bytes).map_err(|error| io_error(&path, &error))
    };

    write("values/themes.xml", manifest::themes_xml().as_bytes())?;
    write(
        "values/window.xml",
        manifest::window_xml(background).as_bytes(),
    )?;
    write(
        "values-night/window.xml",
        manifest::window_xml(manifest::night_background(background)).as_bytes(),
    )?;
    write(
        "values/colors.xml",
        manifest::colors_xml(&format!(
            "#{:02X}{:02X}{:02X}",
            background[0], background[1], background[2]
        ))
        .as_bytes(),
    )?;
    write(
        "mipmap-anydpi-v26/ic_launcher.xml",
        manifest::adaptive_icon_xml().as_bytes(),
    )?;
    write(
        "mipmap-anydpi-v26/ic_launcher_round.xml",
        manifest::adaptive_icon_xml().as_bytes(),
    )?;

    let source = icon.unwrap_or_else(|| {
        Rgba::filled(
            1024,
            1024,
            [background[0], background[1], background[2], 255],
        )
    });
    for (density, scale) in DENSITIES {
        let legacy = (LEGACY_DP * scale).round() as u32;
        let square = source.resize(legacy, legacy);
        let mut round = square.clone();
        round.mask_circle();
        write(
            &format!("mipmap-{density}/ic_launcher.png"),
            &encode(&square)?,
        )?;
        write(
            &format!("mipmap-{density}/ic_launcher_round.png"),
            &encode(&round)?,
        )?;

        let layer = (ADAPTIVE_DP * scale).round() as u32;
        let inner = (f64::from(layer) * SAFE_ZONE).round() as u32;
        let mut foreground = Rgba::new(layer, layer);
        let offset = (layer - inner) / 2;
        foreground.draw(&source.resize(inner, inner), offset, offset);
        write(
            &format!("mipmap-{density}/ic_launcher_foreground.png"),
            &encode(&foreground)?,
        )?;
    }

    std::fs::write(&stamp_path, format!("{stamp}\n")).map_err(|e| io_error(&stamp_path, &e))?;
    Ok(Generated {
        dir,
        changed: true,
        checks,
    })
}

fn encode(image: &Rgba) -> Result<Vec<u8>, IcmError> {
    image::encode(image).map_err(|error| {
        IcmError::new(
            CheckId::InternalBug,
            format!("cannot encode an icon: {error}"),
        )
    })
}

fn io_error(path: &Path, error: &std::io::Error) -> IcmError {
    IcmError::new(
        CheckId::InternalBug,
        format!("cannot write {}: {error}", path.display()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_every_density_and_stamps() {
        let gen_dir = tempfile::tempdir().unwrap();
        let icon_path = gen_dir.path().join("icon.png");
        let mut icon = Rgba::filled(1024, 1024, [0, 0, 255, 255]);
        icon.set(0, 0, [0, 0, 0, 0]);
        std::fs::write(&icon_path, image::encode(&icon).unwrap()).unwrap();

        let inputs = Inputs {
            icon: Some(icon_path.clone()),
            background: "#FF0000".into(),
        };
        let generated = generate(gen_dir.path(), &inputs).unwrap();
        assert!(generated.changed);
        assert!(generated.checks.is_empty());
        let dir = &generated.dir;
        for (density, scale) in DENSITIES {
            let legacy =
                std::fs::read(dir.join(format!("mipmap-{density}/ic_launcher.png"))).unwrap();
            let size = (LEGACY_DP * scale).round() as u32;
            assert_eq!(image::png_size(&legacy), Some((size, size)));
            let foreground = image::decode(
                &std::fs::read(dir.join(format!("mipmap-{density}/ic_launcher_foreground.png")))
                    .unwrap(),
            )
            .unwrap();
            let layer = (ADAPTIVE_DP * scale).round() as u32;
            assert_eq!(foreground.width, layer);
            // Transparent padding outside the safe zone, the icon inside.
            assert_eq!(foreground.get(0, 0)[3], 0);
            assert_eq!(foreground.get(layer / 2, layer / 2), [0, 0, 255, 255]);
        }
        let colors = std::fs::read_to_string(dir.join("values/colors.xml")).unwrap();
        assert!(colors.contains("#FF0000"));
        // A light background: the window is dark in dark mode, with white
        // bar icons.
        let day = std::fs::read_to_string(dir.join("values/window.xml")).unwrap();
        assert!(day.contains(">#FF0000<"), "{day}");
        let night = std::fs::read_to_string(dir.join("values-night/window.xml")).unwrap();
        assert!(night.contains(">#2B2D31<"), "{night}");
        assert!(night.contains(">false<"), "{night}");
        assert!(
            dir.join("mipmap-anydpi-v26/ic_launcher_round.xml")
                .is_file()
        );

        // Unchanged inputs: nothing rewritten.
        assert!(!generate(gen_dir.path(), &inputs).unwrap().changed);
        // A new background: regenerated.
        let inputs = Inputs {
            background: "#00FF00".into(),
            ..inputs
        };
        assert!(generate(gen_dir.path(), &inputs).unwrap().changed);
    }

    #[test]
    fn small_icons_warn_and_bad_ones_fail() {
        let gen_dir = tempfile::tempdir().unwrap();
        let small = gen_dir.path().join("small.png");
        std::fs::write(
            &small,
            image::encode(&Rgba::filled(64, 32, [1, 2, 3, 255])).unwrap(),
        )
        .unwrap();
        let generated = generate(
            gen_dir.path(),
            &Inputs {
                icon: Some(small),
                background: "#FFFFFF".into(),
            },
        )
        .unwrap();
        assert_eq!(generated.checks[0].id(), "app.icon.invalid");

        let broken = gen_dir.path().join("broken.png");
        std::fs::write(&broken, b"nope").unwrap();
        let error = generate(
            gen_dir.path(),
            &Inputs {
                icon: Some(broken),
                background: "#FFFFFF".into(),
            },
        )
        .unwrap_err();
        assert_eq!(error.id, "app.icon.invalid");

        let error = generate(
            gen_dir.path(),
            &Inputs {
                icon: None,
                background: "white".into(),
            },
        )
        .unwrap_err();
        assert_eq!(error.id, "config.invalid");
    }
}
