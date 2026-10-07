//! App Store screenshots from the simulator (Appendix C item 19): `icm run
//! ios-sim --store` runs the app on a store-size iPhone simulator (the
//! newest `iPhone <n> Pro Max`, 6.9 inches, 1320x2868), and `icm shot
//! ios-sim --store` keeps a capture of an accepted size as an opaque RGB
//! PNG in `target/icm/store/ios/`, ready for App Store Connect's 6.9-inch
//! (or 6.5-inch) iPhone slot.

use super::image;
use super::simctl::{DeviceType, Runtime};
use crate::catalogue::CheckId;
use crate::context::{Ctx, Project};
use crate::error::{Check, Evidence, IcmError, Result};
use serde_json::json;
use std::path::{Path, PathBuf};

/// The iPhone screenshot sizes App Store Connect takes (portrait; landscape
/// is the same sizes turned), with the display class they fill.
pub const SIZES: &[(u32, u32, &str)] = &[
    (1320, 2868, "6.9-inch"),
    (1290, 2796, "6.9-inch"),
    (1260, 2736, "6.9-inch"),
    (1284, 2778, "6.5-inch"),
    (1242, 2688, "6.5-inch"),
];

/// The display class of a screenshot size, if App Store Connect takes it.
pub fn class(width: u32, height: u32) -> Option<&'static str> {
    SIZES
        .iter()
        .find(|(w, h, _)| (*w, *h) == (width, height) || (*h, *w) == (width, height))
        .map(|(_, _, class)| *class)
}

fn pro_max_number(name: &str) -> Option<u32> {
    name.strip_prefix("iPhone ")?
        .strip_suffix(" Pro Max")?
        .trim()
        .parse()
        .ok()
}

/// The store-size device type: the newest `iPhone <n> Pro Max` the runtime
/// supports.
pub fn device_type(runtime: &Runtime) -> std::result::Result<&DeviceType, String> {
    runtime
        .device_types
        .iter()
        .filter_map(|t| pro_max_number(&t.name).map(|n| (n, t)))
        .max_by_key(|(n, _)| *n)
        .map(|(_, t)| t)
        .ok_or_else(|| {
            format!(
                "{} supports no iPhone Pro Max device type for App Store screenshots",
                runtime.name
            )
        })
}

/// Where store screenshots go.
pub fn dir(project: &Project) -> PathBuf {
    project.icm_dir.join("store").join("ios")
}

/// Keeps the capture `png` as a store screenshot, or fails
/// `ios.shot.store_size` when the simulator's screen is not a size App
/// Store Connect takes.
pub fn keep(ctx: &Ctx, project: &Project, png: &Path, name: &str, device: &str) -> Result<PathBuf> {
    let pixels = image::read_png(png).map_err(|e| IcmError::new(CheckId::ToolFailed, e))?;
    let (width, height) = (pixels.width, pixels.height);
    let Some(class) = class(width, height) else {
        return Err(IcmError::new(
            CheckId::IosShotStoreSize,
            format!(
                "{device} screenshots are {width}x{height}; App Store Connect takes 6.9-inch (1320x2868, 1290x2796, 1260x2736) or 6.5-inch (1284x2778, 1242x2688) iPhone screenshots"
            ),
        )
        .evidence(Evidence::file(png))
        .fix(
            "Run the app on a store-size simulator, then take the screenshots.",
            &["icm run ios-sim --store --json -q", "icm shot ios-sim --store --name <screen> --json -q"],
        ));
    };
    // App Store Connect wants no alpha channel.
    let flat = image::flatten(&pixels, [255, 255, 255]);
    let out = dir(project).join(format!("{name}-{width}x{height}.png"));
    image::write_png(&out, &flat, false).map_err(|e| IcmError::new(CheckId::InternalBug, e))?;
    ctx.rep.check(Check::pass(
        CheckId::IosShotStoreSize,
        format!("{width}x{height}: App Store Connect's {class} iPhone screenshot"),
    ));
    ctx.rep.artifact("store_screenshot", &out);
    ctx.rep.set(
        "store",
        json!({"class": class, "size": [width, height], "path": crate::paths::display(&out)}),
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime(names: &[&str]) -> Runtime {
        Runtime {
            identifier: "com.apple.CoreSimulator.SimRuntime.iOS-27-0".into(),
            name: "iOS 27.0".into(),
            version: "27.0".into(),
            platform: "iOS".into(),
            build: "24A434".into(),
            available: true,
            device_types: names
                .iter()
                .map(|name| DeviceType {
                    name: (*name).to_string(),
                    identifier: format!(
                        "com.apple.CoreSimulator.SimDeviceType.{}",
                        name.replace(' ', "-")
                    ),
                    product_family: "iPhone".into(),
                    bundle_path: None,
                })
                .collect(),
        }
    }

    #[test]
    fn the_newest_pro_max_is_the_store_device() {
        let runtime = runtime(&[
            "iPhone 16 Pro Max",
            "iPhone 17",
            "iPhone 17 Pro Max",
            "iPhone 17 Pro",
        ]);
        assert_eq!(device_type(&runtime).unwrap().name, "iPhone 17 Pro Max");
        assert!(device_type(&super::tests::runtime(&["iPhone 17", "iPhone Air"])).is_err());
    }

    #[test]
    fn store_sizes_are_known() {
        assert_eq!(class(1320, 2868), Some("6.9-inch"));
        assert_eq!(class(2868, 1320), Some("6.9-inch"));
        assert_eq!(class(1242, 2688), Some("6.5-inch"));
        assert_eq!(class(1206, 2622), None);
    }
}
