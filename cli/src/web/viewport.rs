//! The browser viewport the session emulates (`--viewport`, design §13.1).

use crate::screen::Screen;
use serde_json::{Value, json};

/// The default viewport.
pub const DEFAULT: &str = "desktop";

/// The presets: name, CSS width and height, device pixel ratio, and whether
/// the page is a phone (mobile metrics and touch input).
const PRESETS: &[(&str, u32, u32, f64, bool)] = &[
    ("iphone-17", 402, 874, 3.0, true),
    ("iphone-se", 375, 667, 2.0, true),
    ("pixel-9", 412, 915, 2.625, true),
    ("web-mobile", 390, 844, 3.0, true),
    ("desktop", 1024, 768, 1.0, false),
];

/// An emulated viewport.
#[derive(Clone, Debug, PartialEq)]
pub struct Viewport {
    /// The preset name, or `WxH[@scale]`.
    pub name: String,
    /// CSS pixels.
    pub width: u32,
    /// CSS pixels.
    pub height: u32,
    /// Device pixels per CSS pixel.
    pub scale: f64,
    /// Phone metrics and touch input.
    pub mobile: bool,
}

impl Viewport {
    /// Parses a preset name or `WxH[@scale]` (a desktop-like viewport).
    pub fn parse(spec: &str) -> Result<Viewport, String> {
        let spec = spec.trim();
        if let Some((name, width, height, scale, mobile)) =
            PRESETS.iter().find(|(name, ..)| *name == spec)
        {
            return Ok(Viewport {
                name: (*name).to_string(),
                width: *width,
                height: *height,
                scale: *scale,
                mobile: *mobile,
            });
        }
        match crate::config::parse_viewport(spec) {
            Some((width, height, scale)) if width <= 10_000 && height <= 10_000 => Ok(Viewport {
                name: spec.to_string(),
                width,
                height,
                scale: f64::from(scale),
                mobile: false,
            }),
            _ => Err(format!(
                "`{spec}` is neither a viewport preset ({}) nor WxH[@scale]",
                PRESETS
                    .iter()
                    .map(|(name, ..)| *name)
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    /// The same viewport in portrait or landscape.
    pub fn oriented(&self, landscape: bool) -> Viewport {
        let (short, long) = (self.width.min(self.height), self.width.max(self.height));
        let (width, height) = if landscape {
            (long, short)
        } else {
            (short, long)
        };
        Viewport {
            width,
            height,
            ..self.clone()
        }
    }

    /// Whether it is wider than tall.
    pub fn landscape(&self) -> bool {
        self.width > self.height
    }

    /// The screen this viewport captures to: device pixels and scale.
    pub fn screen(&self) -> Screen {
        Screen::new(
            (
                (f64::from(self.width) * self.scale).round() as u32,
                (f64::from(self.height) * self.scale).round() as u32,
            ),
            self.scale,
        )
    }

    /// The JSON form (session records, results).
    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "width": self.width,
            "height": self.height,
            "scale": self.scale,
            "mobile": self.mobile,
        })
    }

    /// Reads the JSON form.
    pub fn from_json(value: &Value) -> Option<Viewport> {
        Some(Viewport {
            name: value.get("name")?.as_str()?.to_string(),
            width: u32::try_from(value.get("width")?.as_u64()?).ok()?,
            height: u32::try_from(value.get("height")?.as_u64()?).ok()?,
            scale: value.get("scale")?.as_f64()?,
            mobile: value.get("mobile")?.as_bool()?,
        })
    }

    /// `Emulation.setDeviceMetricsOverride` parameters.
    pub fn metrics(&self) -> Value {
        json!({
            "width": self.width,
            "height": self.height,
            "deviceScaleFactor": self.scale,
            "mobile": self.mobile,
            "screenWidth": self.width,
            "screenHeight": self.height,
            "screenOrientation": if self.landscape() {
                json!({"type": "landscapePrimary", "angle": 90})
            } else {
                json!({"type": "portraitPrimary", "angle": 0})
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_and_sizes_parse() {
        let phone = Viewport::parse("iphone-17").unwrap();
        assert_eq!((phone.width, phone.height, phone.scale), (402, 874, 3.0));
        assert!(phone.mobile);
        assert_eq!(phone.screen().px, (1206, 2622));

        let desktop = Viewport::parse(DEFAULT).unwrap();
        assert!(!desktop.mobile);
        assert_eq!(desktop.screen().px, (1024, 768));

        let custom = Viewport::parse("800x600@2").unwrap();
        assert_eq!((custom.width, custom.height, custom.scale), (800, 600, 2.0));
        assert_eq!(custom.name, "800x600@2");

        assert!(Viewport::parse("tablet").unwrap_err().contains("iphone-17"));
        assert!(Viewport::parse("0x10").is_err());
    }

    #[test]
    fn rotation_swaps_the_edges() {
        let phone = Viewport::parse("pixel-9").unwrap();
        let landscape = phone.oriented(true);
        assert_eq!((landscape.width, landscape.height), (915, 412));
        assert!(landscape.landscape());
        assert_eq!(landscape.metrics()["screenOrientation"]["angle"], 90);
        assert_eq!(landscape.oriented(false), phone);
    }

    #[test]
    fn json_round_trips() {
        let viewport = Viewport::parse("web-mobile").unwrap();
        assert_eq!(Viewport::from_json(&viewport.to_json()), Some(viewport));
    }
}
