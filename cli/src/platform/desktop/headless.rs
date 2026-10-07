//! The screenshot fallback (design §10.1 step 4): when macOS does not let
//! icm record the screen, or no window can be captured (Wayland, a missing
//! X11 tool), the view is rendered by the app's own harness (`tests/icm.rs`,
//! §13.2) at the window's size and scale, on the CPU with tiny-skia:
//!
//! `ICED_TEST_BACKEND=tiny-skia cargo test -p <pkg> --test icm -- icm-shot
//! --viewport WxH --scale S --theme light|dark --out <run>/screen.png`
//!
//! It shows the app's initial state, not the running window's.

use crate::cargo::{self, Invocation, Select};
use crate::catalogue::CheckId;
use crate::context::{Ctx, Project};
use crate::error::{Evidence, IcmError, Result};
use serde_json::Value;
use std::path::Path;

/// The name of the harness test target.
pub const HARNESS: &str = "icm";

/// Whether the package declares the harness test target.
pub fn has_harness(package: &cargo::Package) -> bool {
    package
        .targets
        .iter()
        .any(|target| target.name == HARNESS && target.kind.iter().any(|kind| kind == "test"))
}

/// The `cargo test` invocation that renders one screenshot.
pub fn invocation(
    package: &cargo::Package,
    size: (f64, f64),
    scale: f64,
    dark: bool,
    out: &Path,
) -> Invocation {
    let mut invocation = Invocation::new("test", &package.manifest_path, &package.name);
    invocation.select = Select::Test(HARNESS.to_string());
    invocation.config = cargo::profile_config("dev");
    invocation.trailing = vec![
        "icm-shot".to_string(),
        "--viewport".to_string(),
        format!("{}x{}", trim(size.0), trim(size.1)),
        "--scale".to_string(),
        trim(scale),
        "--theme".to_string(),
        if dark { "dark" } else { "light" }.to_string(),
        "--out".to_string(),
        out.display().to_string(),
    ];
    invocation
}

/// `1024` for 1024.0, `402.5` for 402.5.
fn trim(value: f64) -> String {
    let text = format!("{value:.3}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Renders the view to `out` (an absolute path) at `size` logical pixels
/// and `scale`.
pub fn render(
    ctx: &Ctx,
    project: &Project,
    size: (f64, f64),
    scale: f64,
    dark: bool,
    out: &Path,
) -> Result<()> {
    let package = project.package_for("desktop")?;
    if !has_harness(package) {
        return Err(IcmError::new(
            CheckId::HarnessMissing,
            format!(
                "package `{}` has no `[[test]] name = \"icm\"` harness, so icm cannot render the view headlessly",
                package.name
            ),
        )
        .evidence(Evidence::file(&package.manifest_path)));
    }

    let _ = std::fs::remove_file(out);
    let invocation = invocation(package, size, scale, dark, out);
    let output = ctx.cargo(
        "harness.shot",
        &invocation,
        &[("ICED_TEST_BACKEND".to_string(), "tiny-skia".to_string())],
    )?;

    let stdout = output.outcome.stdout_text();
    let result = harness_result(&stdout);
    let ok = result
        .as_ref()
        .and_then(|value| value.get("ok"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !ok || !out.is_file() {
        let detail = result
            .as_ref()
            .and_then(|value| value.get("error"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| "the harness wrote no screenshot".to_string());
        let mut error = IcmError::new(CheckId::ToolFailed, format!("icm-shot failed: {detail}"));
        if let Some(log) = &output.outcome.log {
            error = error.evidence(Evidence::file(log));
        }
        return Err(error);
    }
    Ok(())
}

/// The object of the last `ICM_HARNESS_RESULT <json>` line.
pub fn harness_result(stdout: &str) -> Option<Value> {
    stdout
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix("ICM_HARNESS_RESULT "))
        .and_then(|json| serde_json::from_str(json).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn the_invocation_renders_at_the_window_size() {
        let package = cargo::Package {
            name: "app".into(),
            version: "0.1.0".into(),
            id: "path+file:///p#app@0.1.0".into(),
            manifest_path: PathBuf::from("/p/Cargo.toml"),
            targets: vec![cargo::Target {
                name: "icm".into(),
                kind: vec!["test".into()],
                crate_types: vec!["bin".into()],
                src_path: PathBuf::from("/p/tests/icm.rs"),
                required_features: Vec::new(),
            }],
            dependencies: Vec::new(),
            features: Default::default(),
            source: None,
        };
        assert!(has_harness(&package));
        let args = invocation(
            &package,
            (1024.0, 768.0),
            2.0,
            true,
            Path::new("/r/screen.png"),
        )
        .args()
        .join(" ");
        assert!(
            args.starts_with("test --config profile.dev.package.\"*\".opt-level=2 --manifest-path /p/Cargo.toml -p app --test icm"),
            "{args}"
        );
        assert!(
            args.ends_with(
                "-- icm-shot --viewport 1024x768 --scale 2 --theme dark --out /r/screen.png"
            ),
            "{args}"
        );
        assert_eq!(trim(402.5), "402.5");
        assert_eq!(trim(2.625), "2.625");
    }

    #[test]
    fn harness_results_are_read_from_the_last_line() {
        let stdout = "ICM_HARNESS {\"protocol\":1}\nICM_HARNESS_RESULT {\"protocol\":1,\"kind\":\"shot\",\"ok\":true}\n";
        assert_eq!(harness_result(stdout).unwrap()["ok"], true);
        assert!(harness_result("nothing\n").is_none());
    }
}
