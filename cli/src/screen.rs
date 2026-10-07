//! One coordinate space for seeing and acting (Appendix C item 25).
//!
//! An agent reads positions off `screen.preview.png`, whose long edge is at
//! most 1024 px; adb wants device pixels, the simulator points and CDP CSS
//! pixels. `icm input` therefore takes preview pixels by default (`--space
//! preview|px|pt`) and converts here, and every screenshot result reports
//! `screen{px, pt, preview, scale}`.

use clap::ValueEnum;
use serde_json::{Value, json};

/// The longest edge of a preview image.
pub const PREVIEW_MAX: u32 = 1024;

/// The coordinate space of `icm input` coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum Space {
    /// Pixels of `screen.preview.png` (long edge at most 1024).
    #[default]
    Preview,
    /// Device pixels (`screen.png`).
    Px,
    /// Points (iOS), CSS pixels (web), dp (Android).
    Pt,
}

/// A screen's sizes in every space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Screen {
    /// Device pixels.
    pub px: (u32, u32),
    /// Device pixels per point.
    pub scale: f64,
    /// The preview's pixels.
    pub preview: (u32, u32),
}

/// The preview size for an image: the long edge scaled down to
/// [`PREVIEW_MAX`], never up.
pub fn preview_size(px: (u32, u32)) -> (u32, u32) {
    let (width, height) = px;
    let long = width.max(height);
    if long <= PREVIEW_MAX || long == 0 {
        return px;
    }
    let factor = f64::from(PREVIEW_MAX) / f64::from(long);
    (
        ((f64::from(width) * factor).round() as u32).max(1),
        ((f64::from(height) * factor).round() as u32).max(1),
    )
}

impl Screen {
    /// A screen of `px` device pixels at `scale` pixels per point.
    pub fn new(px: (u32, u32), scale: f64) -> Screen {
        let scale = if scale > 0.0 { scale } else { 1.0 };
        Screen {
            px,
            scale,
            preview: preview_size(px),
        }
    }

    /// The size in points.
    pub fn pt(&self) -> (f64, f64) {
        (
            f64::from(self.px.0) / self.scale,
            f64::from(self.px.1) / self.scale,
        )
    }

    /// Device pixels per preview pixel.
    pub fn preview_factor(&self) -> f64 {
        if self.preview.0 == 0 {
            1.0
        } else {
            f64::from(self.px.0) / f64::from(self.preview.0)
        }
    }

    /// A point in `space` as device pixels.
    pub fn to_px(&self, x: f64, y: f64, space: Space) -> (f64, f64) {
        match space {
            Space::Px => (x, y),
            Space::Pt => (x * self.scale, y * self.scale),
            Space::Preview => {
                let factor = self.preview_factor();
                (x * factor, y * factor)
            }
        }
    }

    /// A point in `space` as points.
    pub fn to_pt(&self, x: f64, y: f64, space: Space) -> (f64, f64) {
        let (px, py) = self.to_px(x, y, space);
        (px / self.scale, py / self.scale)
    }

    /// Whether a point in `space` lies on the screen.
    pub fn contains(&self, x: f64, y: f64, space: Space) -> bool {
        let (px, py) = self.to_px(x, y, space);
        px >= 0.0 && py >= 0.0 && px < f64::from(self.px.0) && py < f64::from(self.px.1)
    }

    /// The result's `screen` object.
    pub fn to_json(&self) -> Value {
        let (pt_w, pt_h) = self.pt();
        json!({
            "px": [self.px.0, self.px.1],
            "pt": [pt_w, pt_h],
            "preview": [self.preview.0, self.preview.1],
            "scale": self.scale,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_cap_the_long_edge() {
        assert_eq!(preview_size((1206, 2622)), (471, 1024));
        assert_eq!(preview_size((1080, 2400)), (461, 1024));
        assert_eq!(preview_size((800, 600)), (800, 600));
        assert_eq!(preview_size((2048, 1536)), (1024, 768));
    }

    #[test]
    fn coordinates_convert_between_spaces() {
        // iPhone 17: 402x874 pt at @3.
        let screen = Screen::new((1206, 2622), 3.0);
        assert_eq!(screen.pt(), (402.0, 874.0));
        let (x, y) = screen.to_px(100.0, 200.0, Space::Pt);
        assert_eq!((x, y), (300.0, 600.0));
        let (x, y) = screen.to_pt(471.0 / 2.0, 512.0, Space::Preview);
        assert!(
            (x - 201.0).abs() < 0.5 && (y - 437.0).abs() < 1.0,
            "{x} {y}"
        );
        assert!(screen.contains(470.0, 1023.0, Space::Preview));
        assert!(!screen.contains(500.0, 10.0, Space::Preview));

        let json = screen.to_json();
        assert_eq!(json["px"][0], 1206);
        assert_eq!(json["preview"][1], 1024);
        assert_eq!(json["scale"], 3.0);
    }
}
