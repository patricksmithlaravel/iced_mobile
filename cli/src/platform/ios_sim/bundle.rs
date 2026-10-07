//! The simulator `.app` (design §10.3 steps 3-6): the asset catalog
//! (AppIcon flattened onto `[app] background`, the LaunchBackground colour)
//! compiled with actool, the generated Info.plist and PrivacyInfo, the
//! executable and resources, the plist gates, then an ad-hoc signature.
//!
//! Generated inputs go to `target/icm/gen/ios-sim/<profile>/`; actool's
//! output is kept there and reused while its inputs are unchanged (a stamp
//! of their hashes), because actool takes seconds. The bundle itself is
//! rebuilt in `target/icm/build/ios-sim/<profile>/<Name>.app` every time.

use super::image;
use super::plist::{self, APP_ICON, InfoInputs, LAUNCH_COLOR};
use crate::catalogue::CheckId;
use crate::context::{Ctx, Project};
use crate::error::{Check, Evidence, IcmError, Result};
use crate::process::Cmd;
use crate::tools::Xcode;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The smallest icon side the App Store accepts.
pub const ICON_SIZE: u32 = 1024;

/// A finished bundle.
#[derive(Clone, Debug)]
pub struct Bundle {
    /// `<Name>.app`.
    pub app: PathBuf,
    /// The executable inside it.
    pub executable: PathBuf,
    /// The generated Info.plist's contents.
    pub info: Map<String, Value>,
}

/// The bundle directory's name: `[app] name` with path separators replaced.
pub fn bundle_name(app_name: &str) -> String {
    let name: String = app_name
        .chars()
        .map(|c| {
            if matches!(c, '/' | ':' | '\0') {
                '-'
            } else {
                c
            }
        })
        .collect();
    let name = name.trim().trim_start_matches('.');
    format!("{}.app", if name.is_empty() { "App" } else { name })
}

fn io_error(what: &str, path: &Path, error: impl std::fmt::Display) -> IcmError {
    IcmError::new(
        CheckId::InternalBug,
        format!("cannot {what} {}: {error}", crate::paths::display(path)),
    )
}

fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io_error("create", parent, e))?;
    }
    std::fs::write(path, bytes).map_err(|e| io_error("write", path, e))
}

/// Copies a file or directory tree.
pub fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(from, to).map(|_| ())
    }
}

/// The asset catalog's JSON files.
fn contents(value: Value) -> Vec<u8> {
    let mut text = serde_json::to_string_pretty(&value).unwrap_or_default();
    text.push('\n');
    text.into_bytes()
}

/// Checks `[app] icon` and writes the flattened 1024 px PNG; returns its
/// hash, or `None` when the app has no icon.
fn prepare_icon(ctx: &Ctx, project: &Project, catalog: &Path) -> Result<Option<String>> {
    let config = &project.config;
    let Some(icon) = config.config.app.icon.as_deref() else {
        return Ok(None);
    };
    let path = project.dir().join(icon);
    let invalid = |detail: String| {
        IcmError::new(CheckId::AppIconInvalid, detail).evidence(config.evidence("app.icon"))
    };

    let Some((width, height)) = image::png_size(&path) else {
        return Err(invalid(format!(
            "{}: `app.icon` = {icon:?} is missing or not a PNG",
            config.source.location_for("app.icon")
        )));
    };
    if width != height || width < ICON_SIZE {
        return Err(invalid(format!(
            "{}: {icon} is {width}x{height}; it must be a square PNG of at least {ICON_SIZE}x{ICON_SIZE}",
            config.source.location_for("app.icon")
        )));
    }

    let background =
        image::parse_hex_color(&config.config.app.background).unwrap_or([255, 255, 255]);
    let pixels = image::read_png(&path).map_err(invalid)?;
    let had_alpha = pixels.has_transparency();
    let flat = image::flatten(&pixels, background);
    let sized = image::resize(&flat, ICON_SIZE, ICON_SIZE);
    let out = catalog.join("AppIcon.appiconset").join("AppIcon.png");
    image::write_png(&out, &sized, false).map_err(|error| {
        IcmError::new(
            CheckId::InternalBug,
            format!("cannot write the icon: {error}"),
        )
    })?;
    write(
        &catalog.join("AppIcon.appiconset").join("Contents.json"),
        &contents(json!({
            "images": [{"filename": "AppIcon.png", "idiom": "universal", "platform": "ios", "size": "1024x1024"}],
            "info": {"author": "icm", "version": 1}
        })),
    )?;
    if had_alpha {
        ctx.rep.progress(format!(
            "{icon} has transparency; flattened onto {} for the app icon",
            config.config.app.background
        ));
    }
    if width > ICON_SIZE {
        ctx.rep
            .progress(format!("{icon} is {width}x{width}; scaled to {ICON_SIZE}"));
    }
    crate::hash::sha256_file(&out)
        .map(Some)
        .map_err(|error| io_error("hash", &out, error))
}

/// Writes the asset catalog, compiles it with actool (or reuses the last
/// output), and returns actool's output directory and its partial plist.
fn compile_assets(
    ctx: &Ctx,
    project: &Project,
    xcode: &Xcode,
    gen_dir: &Path,
) -> Result<(PathBuf, Option<Map<String, Value>>)> {
    compile_assets_for(ctx, project, xcode, gen_dir, "iphonesimulator")
}

/// [`compile_assets`] for an actool platform: `iphonesimulator`, or
/// `iphoneos` for device and App Store bundles (`crate::ios::bundle`).
pub fn compile_assets_for(
    ctx: &Ctx,
    project: &Project,
    xcode: &Xcode,
    gen_dir: &Path,
    platform: &str,
) -> Result<(PathBuf, Option<Map<String, Value>>)> {
    let config = &project.config.config;
    let catalog = gen_dir.join("Assets.xcassets");
    let _ = std::fs::remove_dir_all(&catalog);
    write(
        &catalog.join("Contents.json"),
        &contents(json!({"info": {"author": "icm", "version": 1}})),
    )?;
    let icon_hash = prepare_icon(ctx, project, &catalog)?;

    let [red, green, blue] = image::parse_hex_color(&config.app.background).unwrap_or([255; 3]);
    let component = |value: u8| format!("{:.3}", f64::from(value) / 255.0);
    write(
        &catalog
            .join(format!("{LAUNCH_COLOR}.colorset"))
            .join("Contents.json"),
        &contents(json!({
            "colors": [{
                "color": {"color-space": "srgb", "components": {
                    "alpha": "1.000", "red": component(red), "green": component(green), "blue": component(blue)
                }},
                "idiom": "universal"
            }],
            "info": {"author": "icm", "version": 1}
        })),
    )?;

    let out = gen_dir.join("actool-out");
    let partial = gen_dir.join("actool.plist");
    let stamp_path = gen_dir.join("actool.stamp");
    let stamp = format!(
        "icon={} background={} min_os={} xcode={} platform={platform}\n",
        icon_hash.as_deref().unwrap_or("none"),
        config.app.background,
        config.ios.min_os,
        xcode.build
    );

    let fresh = std::fs::read_to_string(&stamp_path).is_ok_and(|old| old == stamp)
        && out.join("Assets.car").is_file()
        && partial.is_file();
    if fresh {
        ctx.rep
            .progress("asset catalog unchanged; reusing the last actool output");
    } else {
        let _ = std::fs::remove_dir_all(&out);
        let _ = std::fs::remove_file(&stamp_path);
        std::fs::create_dir_all(&out).map_err(|e| io_error("create", &out, e))?;
        let mut cmd = xcode
            .xcrun()
            .arg("actool")
            .arg(&catalog)
            .arg("--compile")
            .arg(&out)
            .args(["--platform", platform, "--minimum-deployment-target"])
            .arg(&config.ios.min_os);
        if icon_hash.is_some() {
            cmd = cmd.args(["--app-icon", APP_ICON]);
        }
        cmd = cmd
            .args(["--target-device", "iphone", "--output-partial-info-plist"])
            .arg(&partial)
            .args([
                "--errors",
                "--warnings",
                "--output-format",
                "human-readable-text",
            ])
            .timeout(Duration::from_secs(180));
        let outcome = ctx.step("ios.actool", &cmd)?;
        if !outcome.success() || !out.join("Assets.car").is_file() {
            return Err(ctx.step_failure("ios.actool", CheckId::IosActoolFailed, &outcome));
        }
        write(&stamp_path, stamp.as_bytes())?;
    }

    let partial_plist = if icon_hash.is_some() {
        Some(read_plist(ctx, &partial)?)
    } else {
        None
    };
    Ok((out, partial_plist))
}

/// Reads a plist through `plutil -convert json`.
pub fn read_plist(ctx: &Ctx, path: &Path) -> Result<Map<String, Value>> {
    let outcome = ctx.probe(
        &Cmd::tool("plutil")
            .args(["-convert", "json", "-o", "-"])
            .arg(path)
            .timeout(Duration::from_secs(30)),
    )?;
    if !outcome.success() {
        return Err(IcmError::new(
            CheckId::ToolFailed,
            format!(
                "plutil cannot read {}: {}",
                crate::paths::display(path),
                outcome.stderr_tail(3)
            ),
        ));
    }
    serde_json::from_slice::<Value>(&outcome.stdout)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .ok_or_else(|| {
            IcmError::new(
                CheckId::ToolFailed,
                format!(
                    "plutil gave no dictionary for {}",
                    crate::paths::display(path)
                ),
            )
        })
}

/// Expands `[app] resources` globs (relative to the project directory;
/// `*`, `?` and `**`) to files, skipping `target/` and hidden directories.
pub fn resource_files(dir: &Path, patterns: &[String]) -> Vec<PathBuf> {
    if patterns.is_empty() {
        return Vec::new();
    }
    let mut files = Vec::new();
    walk(dir, dir, &mut files);
    files.sort();
    files
        .into_iter()
        .filter(|relative| {
            let text = relative.to_string_lossy();
            patterns.iter().any(|pattern| glob_match(pattern, &text))
        })
        .collect()
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            if name.starts_with('.') || (dir == root && name == "target") {
                continue;
            }
            walk(root, &path, out);
        } else if let Ok(relative) = path.strip_prefix(root) {
            out.push(relative.to_path_buf());
        }
    }
}

/// Matches `/`-separated paths: `*` and `?` stay within a segment, `**`
/// spans segments.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    fn segments(text: &str) -> Vec<&str> {
        text.split('/')
            .filter(|s| !s.is_empty() && *s != ".")
            .collect()
    }
    fn segment_match(pattern: &[char], text: &[char]) -> bool {
        match (pattern.first(), text.first()) {
            (None, None) => true,
            (Some('*'), _) => {
                segment_match(&pattern[1..], text)
                    || (!text.is_empty() && segment_match(pattern, &text[1..]))
            }
            (Some('?'), Some(_)) => segment_match(&pattern[1..], &text[1..]),
            (Some(p), Some(t)) if p == t => segment_match(&pattern[1..], &text[1..]),
            _ => false,
        }
    }
    fn match_from(pattern: &[&str], path: &[&str]) -> bool {
        match pattern.first() {
            None => path.is_empty(),
            Some(&"**") => (0..=path.len()).any(|skip| match_from(&pattern[1..], &path[skip..])),
            Some(segment) => {
                !path.is_empty() && {
                    let p: Vec<char> = segment.chars().collect();
                    let t: Vec<char> = path[0].chars().collect();
                    segment_match(&p, &t) && match_from(&pattern[1..], &path[1..])
                }
            }
        }
    }
    match_from(&segments(pattern), &segments(path))
}

/// Builds and signs the bundle around `exe`.
pub fn assemble(
    ctx: &Ctx,
    project: &Project,
    xcode: &Xcode,
    exe: &Path,
    bin: &str,
    profile: &str,
) -> Result<Bundle> {
    let config = &project.config.config;
    let gen_dir = project.gen_dir("ios-sim", profile);
    std::fs::create_dir_all(&gen_dir).map_err(|e| io_error("create", &gen_dir, e))?;

    let (actool_out, actool_plist) = compile_assets(ctx, project, xcode, &gen_dir)?;

    let package = project.package_for("ios-sim")?;
    let info = plist::info_plist(
        config,
        &InfoInputs {
            executable: bin,
            cargo_version: &package.version,
            platform: "iPhoneSimulator",
            actool: actool_plist.as_ref(),
        },
    );
    let info_path = gen_dir.join("Info.plist");
    write(
        &info_path,
        plist::to_xml(&Value::Object(info.clone())).as_bytes(),
    )?;
    let privacy_path = gen_dir.join("PrivacyInfo.xcprivacy");
    write(
        &privacy_path,
        plist::to_xml(&Value::Object(plist::privacy_info(config))).as_bytes(),
    )?;
    ctx.rep.artifact("info_plist", &info_path);

    // The bundle, rebuilt from scratch.
    let app = project
        .build_dir("ios-sim", profile)
        .join(bundle_name(&config.app.name));
    let _ = std::fs::remove_dir_all(&app);
    std::fs::create_dir_all(&app).map_err(|e| io_error("create", &app, e))?;
    let executable = app.join(bin);
    std::fs::copy(exe, &executable).map_err(|e| io_error("copy", exe, e))?;
    copy_tree(&actool_out, &app).map_err(|e| io_error("copy", &actool_out, e))?;
    std::fs::copy(&info_path, app.join("Info.plist"))
        .map_err(|e| io_error("copy", &info_path, e))?;
    std::fs::copy(&privacy_path, app.join("PrivacyInfo.xcprivacy"))
        .map_err(|e| io_error("copy", &privacy_path, e))?;

    for relative in resource_files(project.dir(), &config.app.resources) {
        copy_tree(&project.dir().join(&relative), &app.join(&relative))
            .map_err(|e| io_error("copy", &relative, e))?;
    }
    let overrides = project.dir().join("platform").join("ios").join("resources");
    if overrides.is_dir() {
        copy_tree(&overrides, &app).map_err(|e| io_error("copy", &overrides, e))?;
    }

    gates(ctx, &app, &executable, &info)?;

    // Ad-hoc signature: the simulator runs it; devices need phase 2.
    let outcome = ctx.step(
        "ios.xattr",
        &Cmd::tool("xattr")
            .arg("-cr")
            .arg(&app)
            .timeout(Duration::from_secs(60)),
    )?;
    if !outcome.success() {
        return Err(ctx.step_failure("ios.xattr", CheckId::ToolFailed, &outcome));
    }
    let outcome = ctx.step(
        "ios.codesign",
        &Cmd::tool("codesign")
            .args(["--force", "--sign", "-", "--timestamp=none"])
            .arg(&app)
            .timeout(Duration::from_secs(120)),
    )?;
    if !outcome.success() {
        return Err(ctx.step_failure("ios.codesign", CheckId::ToolFailed, &outcome));
    }

    Ok(Bundle {
        app,
        executable,
        info,
    })
}

/// `ios.plist.lint`, `ios.plist.required_keys`, `ios.plist.scene_manifest`,
/// `ios.privacy.present` and the executable.
fn gates(ctx: &Ctx, app: &Path, executable: &Path, info: &Map<String, Value>) -> Result<()> {
    let info_path = app.join("Info.plist");
    let outcome = ctx.step(
        "ios.plist.lint",
        &Cmd::tool("plutil")
            .arg("-lint")
            .arg(&info_path)
            .arg(app.join("PrivacyInfo.xcprivacy"))
            .timeout(Duration::from_secs(30)),
    )?;
    if !outcome.success() {
        let text = format!("{}{}", outcome.stdout_text(), outcome.stderr_text());
        return Err(IcmError::new(
            CheckId::IosPlistLint,
            format!("plutil -lint rejected the bundle's plists: {}", text.trim()),
        )
        .evidence(Evidence::file(&info_path)));
    }
    ctx.rep.check(Check::pass(
        CheckId::IosPlistLint,
        "Info.plist and PrivacyInfo.xcprivacy are valid",
    ));

    let missing = plist::missing_required(info);
    if !missing.is_empty() {
        return Err(IcmError::new(
            CheckId::IosPlistRequiredKeys,
            format!("Info.plist lacks {}", missing.join(", ")),
        )
        .evidence(Evidence::file(&info_path)));
    }
    ctx.rep.check(Check::pass(
        CheckId::IosPlistRequiredKeys,
        "the managed keys are present",
    ));

    if !plist::has_scene_manifest(info) {
        return Err(IcmError::new(
            CheckId::IosPlistSceneManifest,
            "Info.plist has no UISceneConfigurations; iOS 27 kills such an app at launch",
        )
        .evidence(Evidence::file(&info_path)));
    }
    ctx.rep.check(Check::pass(
        CheckId::IosPlistSceneManifest,
        "UIApplicationSceneManifest present",
    ));

    if !app.join("PrivacyInfo.xcprivacy").is_file() {
        return Err(IcmError::new(
            CheckId::IosPrivacyPresent,
            "PrivacyInfo.xcprivacy is missing from the bundle root",
        ));
    }
    ctx.rep.check(Check::pass(
        CheckId::IosPrivacyPresent,
        "PrivacyInfo.xcprivacy at the bundle root",
    ));

    if !executable.is_file() {
        return Err(IcmError::new(
            CheckId::InternalBug,
            format!(
                "the executable {} is missing from the bundle",
                crate::paths::display(executable)
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_names_are_safe() {
        assert_eq!(bundle_name("App"), "App.app");
        assert_eq!(bundle_name("My/App: Pro"), "My-App- Pro.app");
        assert_eq!(bundle_name(" "), "App.app");
    }

    #[test]
    fn globs_match_segments() {
        assert!(glob_match("assets/*.ttf", "assets/a.ttf"));
        assert!(!glob_match("assets/*.ttf", "assets/fonts/a.ttf"));
        assert!(glob_match("assets/**/*.ttf", "assets/fonts/a.ttf"));
        assert!(glob_match("assets/**/*.ttf", "assets/a.ttf"));
        assert!(glob_match("data/?.json", "data/a.json"));
        assert!(!glob_match("data/?.json", "data/ab.json"));
        assert!(glob_match("**", "x/y/z"));
        assert!(glob_match("./README.md", "README.md"));
    }

    #[test]
    fn resources_skip_target_and_hidden_dirs() {
        let dir = tempfile::tempdir().unwrap();
        for file in [
            "assets/a.txt",
            "assets/deep/b.txt",
            "target/assets/c.txt",
            ".git/assets/d.txt",
        ] {
            let path = dir.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "x").unwrap();
        }
        let files = resource_files(
            dir.path(),
            &["assets/**".to_string(), "**/c.txt".to_string()],
        );
        assert_eq!(
            files,
            vec![
                PathBuf::from("assets/a.txt"),
                PathBuf::from("assets/deep/b.txt")
            ]
        );
        assert!(resource_files(dir.path(), &[]).is_empty());
    }
}
