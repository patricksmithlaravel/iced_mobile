//! `icm wait <run>`: waits for a detached run (default up to 9 minutes,
//! `--timeout` to change), then prints that run's events and result as if
//! it had run in the foreground, and exits with its exit code. While it is
//! still running, the result is `run.still_running` (exit 8) and the call
//! can simply be repeated.

use crate::catalogue::CheckId;
use crate::cli::WaitArgs;
use crate::context::Ctx;
use crate::error::{Evidence, IcmError, Result};
use crate::output::rundir;
use serde_json::json;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// The default wait: under the 10-minute ceiling of an agent's command.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(9 * 60);

/// Runs `icm wait`.
pub fn run(ctx: &mut Ctx, args: &WaitArgs) -> Result<()> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(project) = ctx.try_project() {
        roots.push(project.icm_dir.clone());
    }
    roots.push(crate::paths::cache_dir());

    let Some(dir) = rundir::find_run(&roots, &args.run) else {
        return Err(IcmError::new(
            CheckId::RunNotFound,
            format!(
                "no run `{}` in {}",
                args.run,
                roots
                    .iter()
                    .map(|root| crate::paths::display(&rundir::runs_dir(root)))
                    .collect::<Vec<_>>()
                    .join(" or ")
            ),
        ));
    };

    let limit = ctx.global.timeout.unwrap_or(DEFAULT_WAIT);
    let started = Instant::now();
    let result_path = dir.join("result.json");

    loop {
        if result_path.exists() {
            let events = std::fs::read_to_string(dir.join("events.ndjson")).unwrap_or_default();
            let mut lines: Vec<String> = events
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(str::to_string)
                .collect();
            let ends_with_result = lines
                .last()
                .is_some_and(|line| line.contains("\"type\":\"result\""));
            if !ends_with_result {
                // The result file exists but its event has not been
                // appended yet; use the file.
                if let Ok(text) = std::fs::read_to_string(&result_path)
                    && let Ok(value) = serde_json::from_str::<serde_json::Value>(&text)
                {
                    lines.push(serde_json::to_string(&value).unwrap_or_default());
                }
            }
            let _ = ctx.rep.replay(&lines);
            return Ok(());
        }

        if !rundir::detached_alive(&dir) {
            // It may have written its result between the two checks.
            if result_path.exists() {
                continue;
            }
            return Err(IcmError::new(
                CheckId::RunDetachedLost,
                format!(
                    "run {} ended without writing {}",
                    args.run,
                    crate::paths::display(&result_path)
                ),
            )
            .evidence(Evidence::file(dir.join("detached.stderr"))));
        }

        if let Some(signal) = crate::signals::pending() {
            return Err(crate::output::interrupted(signal));
        }

        if started.elapsed() >= limit {
            ctx.rep.set("status", json!("running"));
            ctx.rep.set("run_dir", json!(crate::paths::display(&dir)));
            ctx.rep.next(
                format!("icm wait {} --timeout 9m --json -q", args.run),
                "it is still running; wait again",
            );
            return Err(IcmError::new(
                CheckId::RunStillRunning,
                format!(
                    "run {} is still running after waiting {}",
                    args.run,
                    crate::time::format_duration(limit)
                ),
            )
            .fix_commands([format!("icm wait {} --timeout 9m --json -q", args.run)]));
        }

        std::thread::sleep(Duration::from_millis(200));
    }
}
