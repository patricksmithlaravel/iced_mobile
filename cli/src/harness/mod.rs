//! Drives the app's headless harness, `tests/icm.rs` (design §13.2, harness
//! protocol 1), which is one line: `iced_test::agent::main(app::application(),
//! env!("CARGO_MANIFEST_DIR"))`.
//!
//! icm builds the `icm` test target once (`cargo test -p <pkg> --test icm
//! --no-run`, so compiler errors become diagnostics), then runs the built
//! executable from the package directory with `ICED_TEST_BACKEND=tiny-skia`
//! and one command:
//!
//! | Command | icm |
//! |---|---|
//! | `icm-shot --viewport V [--scale F] --theme T [--preset P] --wait-ms N --out PNG` | `icm shot --headless` |
//! | `icm-tree --viewport V [--preset P] --wait-ms N --out JSON` | `icm ui --headless tree|find` |
//! | `icm-ice FILE --report JSON --timeout-ms N` | `icm ui --headless ice`, flows outside `tests/flows` |
//! | (no command: every `tests/flows/*.ice`) | `icm test` (through `cargo test`) |
//!
//! The harness prints `ICM_HARNESS {"protocol":1}` first and
//! `ICM_HARNESS_RESULT <json>` last, and exits 0 (passed), 1 (a flow failed)
//! or 2 (usage, or a file it cannot read or write). Anything else is the
//! app's code failing inside the harness: a panic is `run.app_panicked`, a
//! signal `run.app_died` (exit 10), with the failure signatures (§13.4) as
//! likely causes.

pub mod libtest;

use crate::cargo::{Invocation, Select};
use crate::catalogue::CheckId;
use crate::context::{Ctx, Project};
use crate::error::{Check, Evidence, IcmError, Result};
use crate::process::{Cmd, End, Outcome};
use crate::signatures::{self, Facts};
use crate::tools::Env;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The harness protocol this icm speaks.
pub const PROTOCOL: u64 = 1;

/// The name of the harness's test target (`[[test]] name = "icm"`).
pub const TARGET: &str = "icm";

/// The backend the harness draws with unless `ICED_TEST_BACKEND` says
/// otherwise: deterministic, CPU only.
pub const DEFAULT_BACKEND: &str = "tiny-skia";

/// How many visible texts a failed flow step lists.
const MAX_TEXTS: usize = 20;

/// How long one `icm-shot` or `icm-tree` may take.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(180);

/// The viewport presets of design §13.1, as the harness knows them: name,
/// logical size and scale.
pub const PRESETS: &[(&str, (u32, u32), f64)] = &[
    ("iphone-17", (402, 874), 3.0),
    ("iphone-se", (375, 667), 2.0),
    ("pixel-9", (412, 915), 2.625),
    ("web-mobile", (390, 844), 3.0),
    ("desktop", (1024, 768), 1.0),
];

/// A viewport to render: a preset or `WxH[@scale]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Viewport {
    /// As given, e.g. `iphone-17` or `800x600@2`: names files.
    pub label: String,
    /// The harness's `--viewport`: the preset name or `WxH`.
    pub arg: String,
    /// The logical size.
    pub size: (u32, u32),
    /// Device pixels per logical pixel.
    pub scale: f64,
    /// Whether `@scale` was given (passed as `--scale`).
    pub explicit_scale: bool,
}

impl Viewport {
    /// Parses a preset name or `WxH[@scale]`.
    pub fn parse(text: &str) -> std::result::Result<Viewport, String> {
        let text = text.trim();
        if let Some((name, size, scale)) = PRESETS.iter().find(|(name, _, _)| *name == text) {
            return Ok(Viewport {
                label: (*name).to_string(),
                arg: (*name).to_string(),
                size: *size,
                scale: *scale,
                explicit_scale: false,
            });
        }
        match crate::config::parse_viewport(text) {
            Some((width, height, scale)) if scale.is_finite() => Ok(Viewport {
                label: text.to_string(),
                arg: format!("{width}x{height}"),
                size: (width, height),
                scale: f64::from(scale),
                explicit_scale: text.contains('@'),
            }),
            _ => Err(format!(
                "`{text}` is neither a viewport preset ({}) nor WxH[@scale], e.g. 800x600@2",
                PRESETS
                    .iter()
                    .map(|(name, _, _)| *name)
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    /// The harness arguments that select it.
    pub fn args(&self) -> Vec<String> {
        let mut args = vec!["--viewport".to_string(), self.arg.clone()];
        if self.explicit_scale {
            args.extend(["--scale".to_string(), format_scale(self.scale)]);
        }
        args
    }
}

fn format_scale(scale: f64) -> String {
    let text = format!("{scale:.4}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// The backend to draw with: `ICED_TEST_BACKEND` when set, else tiny-skia.
pub fn backend(env: &Env) -> String {
    env.var("ICED_TEST_BACKEND")
        .map(str::trim)
        .filter(|backend| !backend.is_empty())
        .unwrap_or(DEFAULT_BACKEND)
        .to_string()
}

/// Whether the package declares the harness's test target.
pub fn has_target(package: &crate::cargo::Package) -> bool {
    package
        .targets
        .iter()
        .any(|target| target.name == TARGET && target.kind.iter().any(|kind| kind == "test"))
}

/// The `harness.missing` error for a package without the target.
pub fn missing(package: &crate::cargo::Package) -> IcmError {
    IcmError::new(
        CheckId::HarnessMissing,
        format!(
            "package `{}` has no `{TARGET}` test target, so icm cannot render or inspect it headlessly",
            package.name
        ),
    )
    .evidence(Evidence::file(&package.manifest_path))
    .fix(
        "Add tests/icm.rs with `fn main() -> std::process::ExitCode { iced_test::agent::main(<crate>::application(), env!(\"CARGO_MANIFEST_DIR\")) }`, a `[[test]] name = \"icm\"`, `path = \"tests/icm.rs\"`, `harness = false` entry, and iced_test (same source as iced) in [dev-dependencies], as `icm new` writes them.",
        &["icm explain harness.missing"],
    )
}

/// A built harness, ready to run commands.
#[derive(Clone, Debug)]
pub struct Harness {
    /// The test executable.
    pub exe: PathBuf,
    /// The package's directory (the harness's working directory).
    pub package_dir: PathBuf,
    /// The workspace root (panic locations are relative to it).
    pub workspace_root: PathBuf,
    /// The backend it draws with.
    pub backend: String,
}

/// The `cargo test` invocation for the package.
pub fn invocation(project: &Project, select: Select, flags: &[&str]) -> Invocation {
    let package = &project.package;
    let mut invocation = Invocation::new("test", &package.manifest_path, &package.name);
    invocation.select = select;
    invocation.flags = flags.iter().map(ToString::to_string).collect();
    invocation
}

/// Builds the harness (`cargo test -p <pkg> --test icm --no-run`); fails
/// `harness.missing` when the package has no `icm` test target.
pub fn build(ctx: &mut Ctx) -> Result<Harness> {
    let project = ctx.project()?.clone();
    if !has_target(&project.package) {
        return Err(missing(&project.package));
    }
    let backend = backend(&ctx.env);
    let invocation = invocation(&project, Select::Test(TARGET.to_string()), &["--no-run"]);
    ctx.rep.progress(format!(
        "building the app's harness: cargo test -p {} --test {TARGET} --no-run",
        project.package.name
    ));
    let built = ctx.cargo("cargo.test.build", &invocation, &[])?;

    let exe = built
        .artifacts
        .iter()
        .filter(|artifact| {
            artifact.target_name == TARGET && artifact.target_kind.iter().any(|kind| kind == "test")
        })
        .find_map(|artifact| artifact.executable.clone())
        .ok_or_else(|| {
            IcmError::new(
                CheckId::ToolFailed,
                format!(
                    "cargo built package `{}` but reported no executable for its `{TARGET}` test target",
                    project.package.name
                ),
            )
        })?;

    Ok(Harness {
        exe,
        package_dir: project.package.dir().to_path_buf(),
        workspace_root: project.metadata.workspace_root.clone(),
        backend,
    })
}

/// What a harness command answered.
#[derive(Clone, Debug)]
pub struct Reply {
    /// The `ICM_HARNESS_RESULT` object.
    pub result: Value,
    /// Its `ok`.
    pub ok: bool,
    /// The process outcome.
    pub outcome: Outcome,
}

impl Harness {
    /// The command line for a harness command.
    pub fn cmd(&self, args: &[String], timeout: Duration) -> Cmd {
        Cmd::new(&self.exe)
            .args(args)
            .cwd(&self.package_dir)
            .env("ICED_TEST_BACKEND", &self.backend)
            .env("CARGO_MANIFEST_DIR", &self.package_dir)
            .env("RUST_BACKTRACE", "0")
            .timeout(timeout)
    }

    /// Runs a harness command as a reported step and reads its answer.
    pub fn call(&self, ctx: &Ctx, step: &str, args: &[String], timeout: Duration) -> Result<Reply> {
        let outcome = ctx.step(step, &self.cmd(args, timeout))?;
        self.interpret(step, outcome)
    }

    /// Judges a harness command's outcome (protocol, exit code, crash).
    pub fn interpret(&self, step: &str, outcome: Outcome) -> Result<Reply> {
        let stdout = outcome.stdout_text();
        let protocol = protocol_line(&stdout);
        let result = result_line(&stdout);

        if let Some(found) = protocol
            && found != PROTOCOL
        {
            return Err(mismatch(found, &outcome));
        }

        match (outcome.end, protocol, result) {
            (End::Exited(code @ (0 | 1)), Some(_), Some(result)) => {
                let ok = code == 0 && result.get("ok").and_then(Value::as_bool).unwrap_or(false);
                Ok(Reply {
                    result,
                    ok,
                    outcome,
                })
            }
            (End::Exited(2), Some(_), Some(result)) => Err(self.refused(&result, &outcome)),
            (End::Exited(_), None, _) => Err(self.not_a_harness(&outcome)),
            _ => Err(self.crashed(step, &outcome)),
        }
    }

    /// The error for a test target that does not speak the protocol: it
    /// printed no `ICM_HARNESS` line, which the harness prints before
    /// running any of the app's code.
    fn not_a_harness(&self, outcome: &Outcome) -> IcmError {
        let stdout = outcome.stdout_text();
        let stderr = outcome.stderr_text();
        let libtest = stdout.contains("test result:")
            || stdout.contains("running ")
            || stderr.contains("Unrecognized option");
        let detail = if libtest {
            format!(
                "the `{TARGET}` test target runs libtest's harness, not iced_test::agent::main: its [[test]] entry lacks `harness = false` (or Cargo discovered tests/icm.rs on its own)"
            )
        } else {
            let tail = outcome.stderr_tail(4);
            format!(
                "the `{TARGET}` test target printed no ICM_HARNESS line ({}){}",
                outcome.describe(),
                if tail.is_empty() {
                    String::new()
                } else {
                    format!(":\n{tail}")
                }
            )
        };
        let mut error = IcmError::new(CheckId::HarnessMissing, detail).fix(
            "Declare `[[test]] name = \"icm\"`, `path = \"tests/icm.rs\"`, `harness = false` in Cargo.toml, and make tests/icm.rs call iced_test::agent::main, as `icm new` writes them.",
            &["icm explain harness.missing"],
        );
        if let Some(log) = &outcome.log {
            error = error.evidence(Evidence::file(log));
        }
        error
    }

    /// The error for a harness that exited 2.
    fn refused(&self, result: &Value, outcome: &Outcome) -> IcmError {
        let message = result
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("the harness refused the command")
            .to_string();
        let mut error = if message.starts_with("unknown command")
            || message.starts_with("unknown option")
        {
            IcmError::new(
                CheckId::HarnessProtocolMismatch,
                format!("the app's harness is older than this icm: {message}"),
            )
            .fix(
                "Update the app's iced_mobile framework pin, or use the icm built from the same tag.",
                &[],
            )
        } else if message.starts_with("cannot write") || message.starts_with("cannot read") {
            IcmError::new(CheckId::ToolFailed, format!("the harness: {message}"))
        } else if message.contains("preset") {
            IcmError::new(
                CheckId::UsageBadArgs,
                format!("the app's harness: {message}"),
            )
            .fix(
                "Pass one of the presets the message lists, or drop --preset.",
                &[],
            )
        } else {
            IcmError::new(
                CheckId::UsageBadArgs,
                format!("the app's harness: {message}"),
            )
        };
        if let Some(log) = &outcome.log {
            error = error.evidence(Evidence::file(log));
        }
        error
    }

    /// The error for a harness that crashed: the app's code panicked or
    /// died inside it.
    pub fn crashed(&self, step: &str, outcome: &Outcome) -> IcmError {
        let stderr = outcome.stderr_text();
        let stdout = outcome.stdout_text();
        let text = format!("{stderr}\n{stdout}");
        let facts = Facts {
            platform: Some("headless"),
            ..Facts::default()
        };

        let mut error = match signatures::first_panic(&text) {
            Some(panic) => {
                let mut error = IcmError::new(
                    CheckId::RunAppPanicked,
                    format!(
                        "the app {} while the harness ran it ({step})",
                        panic.describe()
                    ),
                );
                if let Some((file, line)) = panic.file_line()
                    && let Some(path) = self.source_file(&file)
                {
                    error = error.evidence(Evidence::line(path, line, panic.message.clone()));
                }
                if let Some(log) = &outcome.log {
                    error = error.evidence(Evidence::file(log).with_excerpt(panic.excerpt.clone()));
                }
                error.fix(
                    "Fix the panic at the location in the detail, then rerun.",
                    &[],
                )
            }
            None => {
                let tail = outcome.stderr_tail(6);
                let mut detail = format!(
                    "the harness ended unexpectedly ({}) during {step}",
                    outcome.describe()
                );
                if !tail.is_empty() {
                    detail.push_str(&format!(":\n{tail}"));
                }
                let mut error = IcmError::new(CheckId::RunAppDied, detail);
                if let Some(log) = &outcome.log {
                    error = error.evidence(Evidence::file(log));
                }
                error
            }
        };
        error = signatures::annotate(error, &text, &facts);
        error
    }

    /// A source path from a panic location, if the file exists: relative to
    /// the workspace root (how rustc prints it), the package, or absolute.
    pub fn source_file(&self, file: &str) -> Option<PathBuf> {
        let path = Path::new(file);
        if path.is_absolute() {
            return path.is_file().then(|| path.to_path_buf());
        }
        [&self.workspace_root, &self.package_dir]
            .into_iter()
            .map(|base| base.join(path))
            .find(|candidate| candidate.is_file())
    }
}

/// The protocol of the first `ICM_HARNESS` line.
pub fn protocol_line(stdout: &str) -> Option<u64> {
    stdout.lines().find_map(|line| {
        let json = line.strip_prefix("ICM_HARNESS ")?;
        serde_json::from_str::<Value>(json)
            .ok()?
            .get("protocol")?
            .as_u64()
    })
}

/// The object of the last `ICM_HARNESS_RESULT` line.
pub fn result_line(stdout: &str) -> Option<Value> {
    stdout
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix("ICM_HARNESS_RESULT "))
        .and_then(|json| serde_json::from_str(json).ok())
}

/// The `harness.protocol_mismatch` error.
pub fn mismatch(found: u64, outcome: &Outcome) -> IcmError {
    let mut error = IcmError::new(
        CheckId::HarnessProtocolMismatch,
        format!("the app's harness speaks protocol {found}; this icm speaks protocol {PROTOCOL}"),
    )
    .fix(
        "Install the icm built from the same iced_mobile tag as the app's framework pin.",
        &["icm --version"],
    );
    if let Some(log) = &outcome.log {
        error = error.evidence(Evidence::file(log));
    }
    error
}

/// Reports a flow's result (a harness `ice` object, or one entry of a
/// `flows` result): a passing check, a failing step as `test.failed` with
/// the flow's file and line as evidence, or a flow that could not run.
/// Returns the blocking error for a flow that does not parse.
pub fn report_flow(ctx: &Ctx, flow: &Value) -> Option<IcmError> {
    let name = flow.get("name").and_then(Value::as_str).unwrap_or("flow");
    let file = flow.get("file").and_then(Value::as_str).unwrap_or("");
    let ms = flow.get("ms").and_then(Value::as_u64).unwrap_or(0);
    let steps = flow
        .get("steps")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    if let Some(message) = flow.get("error").and_then(Value::as_str) {
        if message.starts_with("invalid flow") {
            let error = IcmError::new(CheckId::TestIceParse, format!("{name}: {message}"))
                .evidence(Evidence::file(file));
            ctx.rep
                .check(Check::from_error(error.clone(), crate::error::Status::Fail));
            return Some(error);
        }
        ctx.rep.check(
            Check::fail(CheckId::TestFailed, format!("{name}: {message}"))
                .evidence(Evidence::file(file)),
        );
        return None;
    }

    match steps
        .iter()
        .find(|step| step.get("status").and_then(Value::as_str) == Some("failed"))
    {
        Some(step) => {
            let line = step.get("line").and_then(Value::as_u64).unwrap_or(0) as u32;
            let instruction = step
                .get("instruction")
                .and_then(Value::as_str)
                .unwrap_or("");
            let reason = step
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("it failed");
            let all_texts: Vec<String> = step
                .get("texts")
                .and_then(Value::as_array)
                .map(|texts| {
                    texts
                        .iter()
                        .filter_map(Value::as_str)
                        .map(|text| format!("{text:?}"))
                        .collect()
                })
                .unwrap_or_default();
            let mut texts: Vec<String> = all_texts.iter().take(MAX_TEXTS).cloned().collect();
            if all_texts.len() > MAX_TEXTS {
                texts.push(format!("… {} more", all_texts.len() - MAX_TEXTS));
            }
            let mut detail = format!("{name} line {line}: `{instruction}` failed: {reason}");
            if !texts.is_empty() {
                detail.push_str(&format!("\nvisible texts: {}", texts.join(", ")));
            }
            let rerun = format!(
                "icm ui --headless ice {} --json -q",
                crate::process::shell_quote(&crate::paths::display(Path::new(file)))
            );
            ctx.rep.check(
                Check::fail(CheckId::TestFailed, detail)
                    .evidence(Evidence::line(file, line.max(1), instruction))
                    .fix(
                        "Fix the app or the flow: `icm ui --headless tree` lists every widget with its text and bounds, and the second command reruns this flow.",
                        &["icm ui --headless tree --json -q", &rerun],
                    ),
            );
        }
        None => {
            let passed = steps
                .iter()
                .filter(|step| step.get("status").and_then(Value::as_str) == Some("passed"))
                .count();
            ctx.rep.check(Check::pass(
                CheckId::TestPassed,
                format!("{name}: {passed} step(s) passed in {ms} ms"),
            ));
        }
    }
    None
}

/// A flow entry without its long fields, for results.
pub fn flow_summary(flow: &Value) -> Value {
    let failed = flow
        .get("steps")
        .and_then(Value::as_array)
        .and_then(|steps| {
            steps
                .iter()
                .find(|step| step.get("status").and_then(Value::as_str) == Some("failed"))
        })
        .cloned();
    json!({
        "name": flow.get("name"),
        "file": flow.get("file").and_then(Value::as_str).map(|f| crate::paths::display(Path::new(f))),
        "passed": flow.get("passed"),
        "ms": flow.get("ms"),
        "error": flow.get("error"),
        "failed_step": failed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn outcome(end: End, stdout: &str, stderr: &str) -> Outcome {
        Outcome {
            end,
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
            duration: Duration::from_millis(5),
            log: None,
            stdout_path: None,
        }
    }

    fn harness(root: &Path) -> Harness {
        Harness {
            exe: root.join("icm-test"),
            package_dir: root.join("app"),
            workspace_root: root.to_path_buf(),
            backend: DEFAULT_BACKEND.into(),
        }
    }

    #[test]
    fn viewports_parse() {
        let phone = Viewport::parse("pixel-9").unwrap();
        assert_eq!(phone.size, (412, 915));
        assert_eq!(phone.scale, 2.625);
        assert_eq!(phone.args(), ["--viewport", "pixel-9"]);

        let custom = Viewport::parse("800x600@2").unwrap();
        assert_eq!(custom.label, "800x600@2");
        assert_eq!(custom.args(), ["--viewport", "800x600", "--scale", "2"]);

        let plain = Viewport::parse("1280x720").unwrap();
        assert_eq!(plain.scale, 1.0);
        assert_eq!(plain.args(), ["--viewport", "1280x720"]);

        assert!(Viewport::parse("big").unwrap_err().contains("iphone-17"));
        assert!(Viewport::parse("0x10").is_err());
        assert_eq!(format_scale(2.625), "2.625");
    }

    #[test]
    fn presets_match_the_config_list() {
        let names: Vec<&str> = PRESETS.iter().map(|(name, _, _)| *name).collect();
        assert_eq!(names, crate::config::VIEWPORT_PRESETS);
    }

    #[test]
    fn invocations_build_the_harness() {
        let mut invocation = Invocation::new("test", Path::new("/p/Cargo.toml"), "app");
        invocation.select = Select::Test(TARGET.into());
        invocation.flags = vec!["--no-run".into()];
        assert_eq!(
            invocation.args().join(" "),
            "test --manifest-path /p/Cargo.toml -p app --test icm --no-run --message-format=json"
        );
    }

    #[test]
    fn answers_are_judged() {
        let dir = tempfile::tempdir().unwrap();
        let h = harness(dir.path());
        let ok = "ICM_HARNESS {\"protocol\":1}\nICM_HARNESS_RESULT {\"protocol\":1,\"kind\":\"shot\",\"ok\":true,\"size\":[1206,2622]}\n";
        let reply = h
            .interpret("harness.shot", outcome(End::Exited(0), ok, ""))
            .unwrap();
        assert!(reply.ok);
        assert_eq!(reply.result["size"][0], 1206);

        let failed = "ICM_HARNESS {\"protocol\":1}\nICM_HARNESS_RESULT {\"protocol\":1,\"kind\":\"ice\",\"ok\":false}\n";
        let reply = h
            .interpret("harness.ice", outcome(End::Exited(1), failed, ""))
            .unwrap();
        assert!(!reply.ok);

        let refused = "ICM_HARNESS {\"protocol\":1}\nICM_HARNESS_RESULT {\"protocol\":1,\"ok\":false,\"error\":\"the preset \\\"x\\\" does not exist (available presets: [])\"}\n";
        let error = h
            .interpret("harness.shot", outcome(End::Exited(2), refused, ""))
            .unwrap_err();
        assert_eq!(error.id, "usage.bad_args");

        let old = "ICM_HARNESS {\"protocol\":1}\nICM_HARNESS_RESULT {\"protocol\":1,\"ok\":false,\"error\":\"unknown command \\\"icm-tree\\\"\"}\n";
        let error = h
            .interpret("harness.tree", outcome(End::Exited(2), old, ""))
            .unwrap_err();
        assert_eq!(error.id, "harness.protocol_mismatch");

        let newer =
            "ICM_HARNESS {\"protocol\":2}\nICM_HARNESS_RESULT {\"protocol\":2,\"ok\":true}\n";
        let error = h
            .interpret("harness.shot", outcome(End::Exited(0), newer, ""))
            .unwrap_err();
        assert_eq!(error.id, "harness.protocol_mismatch");
        assert_eq!(error.exit, crate::exit::Exit::Environment);

        let libtest = "\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n";
        let error = h
            .interpret("harness.shot", outcome(End::Exited(0), libtest, ""))
            .unwrap_err();
        assert_eq!(error.id, "harness.missing");
    }

    #[test]
    fn a_panicking_view_is_the_apps_panic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("app/src")).unwrap();
        std::fs::write(dir.path().join("app/src/lib.rs"), "fn view() {}\n").unwrap();
        let h = harness(dir.path());
        let stderr = "thread 'main' panicked at app/src/lib.rs:1:1:\nthe view broke\nnote: run with `RUST_BACKTRACE=1`\n";
        let error = h
            .interpret(
                "harness.shot",
                outcome(End::Exited(101), "ICM_HARNESS {\"protocol\":1}\n", stderr),
            )
            .unwrap_err();
        assert_eq!(error.id, "run.app_panicked");
        assert_eq!(error.exit, crate::exit::Exit::AppDied);
        assert!(error.detail.contains("the view broke"), "{}", error.detail);
        assert_eq!(error.evidence[0].line, Some(1));
        assert!(error.evidence[0].path.ends_with("app/src/lib.rs"));
        assert!(!error.likely_causes.is_empty());

        let error = h
            .interpret(
                "harness.shot",
                outcome(
                    End::Signaled(11),
                    "ICM_HARNESS {\"protocol\":1}\n",
                    "Failed to find an appropriate adapter\n",
                ),
            )
            .unwrap_err();
        assert_eq!(error.id, "run.app_died");
        assert!(
            error
                .likely_causes
                .iter()
                .any(|cause| cause.contains("ICED_TEST_BACKEND")),
            "{:?}",
            error.likely_causes
        );
    }

    #[test]
    fn result_lines_are_the_last() {
        let stdout = "ICM_HARNESS {\"protocol\":1}\nICM_HARNESS_RESULT {\"ok\":false}\nnoise\nICM_HARNESS_RESULT {\"ok\":true}\n";
        assert_eq!(protocol_line(stdout), Some(1));
        assert_eq!(result_line(stdout).unwrap()["ok"], true);
        assert_eq!(protocol_line("nothing"), None);
    }
}
