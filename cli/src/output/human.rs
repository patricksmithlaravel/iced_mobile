//! The human line protocol (design §4.2), rendered from NDJSON events so
//! that a live run and a replayed one (`icm wait`) print the same lines.
//!
//! Every stdout line starts with a fixed keyword (`STEP`, `CHECK`,
//! `ARTIFACT`, `READY`, `PLAN`, `LOG`, `NEXT`, `RESULT`); continuation lines
//! are indented two spaces, so `grep '^CHECK FAIL'` always works.

use crate::time::format_duration;
use serde_json::Value;
use std::time::Duration;

/// Where a rendered line goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    /// Protocol lines.
    Stdout,
    /// Progress and tool output.
    Stderr,
}

/// Renders one event as human lines. With `quiet`, only `CHECK FAIL`,
/// `CHECK WARN` and `RESULT` lines remain.
pub fn render(event: &Value, quiet: bool, verbose: bool) -> Vec<(Stream, String)> {
    let mut out = Vec::new();
    let kind = event.get("type").and_then(Value::as_str).unwrap_or("");

    match kind {
        "step" => {
            let phase = str_field(event, "phase");
            if phase == "begin" {
                if verbose
                    && !quiet
                    && let Some(argv) = event.get("argv").and_then(Value::as_array)
                {
                    let line: Vec<&str> = argv.iter().filter_map(Value::as_str).collect();
                    out.push((Stream::Stderr, format!("+ {}", line.join(" "))));
                }
            } else if !quiet {
                out.push((Stream::Stdout, step_line(event)));
            }
        }
        "check" => {
            let status = str_field(event, "status");
            if !quiet || matches!(status, "fail" | "warn") {
                out.extend(check_lines(event).into_iter().map(|l| (Stream::Stdout, l)));
            }
        }
        "artifact" if !quiet => {
            out.push((
                Stream::Stdout,
                format!(
                    "ARTIFACT {} {}",
                    str_field(event, "kind"),
                    str_field(event, "path")
                ),
            ));
        }
        "ready" if !quiet => {
            let what = event
                .get("url")
                .and_then(Value::as_str)
                .or_else(|| event.get("session").and_then(Value::as_str))
                .unwrap_or("");
            let mut line = format!("READY {what}");
            if let Some(ms) = event.get("ms_since_launch").and_then(Value::as_u64) {
                line.push_str(&format!(
                    " after {}",
                    format_duration(Duration::from_millis(ms))
                ));
            }
            if let Some(source) = event.get("source").and_then(Value::as_str) {
                line.push_str(&format!(" (source: {source})"));
            }
            out.push((Stream::Stdout, line.trim_end().replace("READY  ", "READY ")));
        }
        // A dependency's warnings (the framework checkout's, say) are in
        // events.ndjson; the app cannot act on them.
        "diagnostic"
            if event.get("dependency").and_then(Value::as_bool) == Some(true)
                && str_field(event, "level") == "warning" => {}
        "diagnostic" if !quiet => {
            let rendered = str_field(event, "rendered");
            let text = if rendered.is_empty() {
                str_field(event, "message")
            } else {
                rendered
            };
            for line in text.trim_end().lines() {
                out.push((Stream::Stderr, line.to_string()));
            }
        }
        "plan" if !quiet => {
            if let Some(steps) = event.get("steps").and_then(Value::as_array) {
                for (index, step) in steps.iter().enumerate() {
                    out.push((
                        Stream::Stdout,
                        format!(
                            "PLAN {:02} {}: {}",
                            index + 1,
                            str_field(step, "name"),
                            str_field(step, "display")
                        ),
                    ));
                    if let Some(env) = step.get("env").and_then(Value::as_object) {
                        for (key, value) in env {
                            out.push((
                                Stream::Stdout,
                                format!("  env: {key}={}", value.as_str().unwrap_or("")),
                            ));
                        }
                    }
                    if let Some(cwd) = step.get("cwd").and_then(Value::as_str) {
                        out.push((Stream::Stdout, format!("  cwd: {cwd}")));
                    }
                }
            }
        }
        "log" if !quiet => {
            // A multi-line message (a panic and its backtrace) continues on
            // indented lines.
            let mut lines = str_field(event, "msg").lines();
            out.push((
                Stream::Stdout,
                format!(
                    "LOG {} {}: {}",
                    str_field(event, "level"),
                    str_field(event, "source"),
                    lines.next().unwrap_or("")
                ),
            ));
            out.extend(lines.map(|line| (Stream::Stdout, format!("  {line}"))));
        }
        "result" => {
            if !quiet && let Some(next) = event.get("next").and_then(Value::as_array) {
                for item in next {
                    let cmd = str_field(item, "cmd");
                    let why = str_field(item, "why");
                    if why.is_empty() {
                        out.push((Stream::Stdout, format!("NEXT {cmd}")));
                    } else {
                        out.push((Stream::Stdout, format!("NEXT {cmd}   # {why}")));
                    }
                }
            }
            out.push((Stream::Stdout, result_line(event)));
            let ok = event.get("ok").and_then(Value::as_bool).unwrap_or(false);
            let summary = str_field(event, "summary");
            if !ok && !summary.is_empty() {
                out.push((
                    Stream::Stdout,
                    format!("  summary: {}", first_line(summary)),
                ));
            }
        }
        _ => {}
    }

    out
}

fn str_field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

/// `STEP cargo.build ok 41.2s  (log: target/icm/runs/<id>/steps/01-cargo.build.log)`
fn step_line(event: &Value) -> String {
    let name = str_field(event, "name");
    let word = match event.get("ok").and_then(Value::as_bool) {
        Some(true) => "ok",
        _ => match str_field(event, "end") {
            "" => "FAIL",
            "timed_out" => "TIMEOUT",
            "interrupted" => "INTERRUPTED",
            _ => "FAIL",
        },
    };
    let ms = event.get("ms").and_then(Value::as_u64).unwrap_or(0);
    let mut line = format!(
        "STEP {name} {word} {}",
        format_duration(Duration::from_millis(ms))
    );
    let log = str_field(event, "log");
    if !log.is_empty() {
        line.push_str(&format!("  (log: {log})"));
    }
    line
}

/// `CHECK FAIL <id>: <detail>` plus `evidence:`, `likely:` and `fix:`
/// continuations.
fn check_lines(event: &Value) -> Vec<String> {
    let status = str_field(event, "status").to_ascii_uppercase();
    let id = str_field(event, "id");
    let detail = str_field(event, "detail");

    let mut lines = Vec::new();
    let mut detail_lines = detail.lines();
    match detail_lines.next() {
        Some(first) => lines.push(format!("CHECK {status} {id}: {first}")),
        None => lines.push(format!("CHECK {status} {id}")),
    }
    for more in detail_lines.take(20) {
        lines.push(format!("  {more}"));
    }

    if let Some(evidence) = event.get("evidence").and_then(Value::as_array) {
        for item in evidence.iter().take(10) {
            let mut text = str_field(item, "path").to_string();
            if let Some(line) = item.get("line").and_then(Value::as_u64) {
                text.push_str(&format!(":{line}"));
            }
            if let Some(excerpt) = item.get("excerpt").and_then(Value::as_str) {
                let excerpt = first_line(excerpt.trim());
                if !excerpt.is_empty() {
                    text.push_str(&format!("  {excerpt}"));
                }
            }
            lines.push(format!("  evidence: {text}"));
        }
    }

    if let Some(causes) = event.get("likely_causes").and_then(Value::as_array) {
        for cause in causes.iter().filter_map(Value::as_str).take(5) {
            let mut cause_lines = cause.lines();
            if let Some(first) = cause_lines.next() {
                lines.push(format!("  likely: {first}"));
            }
            for more in cause_lines.take(5) {
                lines.push(format!("    {more}"));
            }
        }
    }

    if let Some(fix) = event.get("fix") {
        let summary = str_field(fix, "summary");
        let by = str_field(fix, "by");
        let commands: Vec<&str> = fix
            .get("commands")
            .and_then(Value::as_array)
            .map(|c| c.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let label = if by == "owner" { "fix (owner)" } else { "fix" };
        let mut text = summary.to_string();
        if !commands.is_empty() {
            if !text.is_empty() {
                text.push_str(" ; ");
            }
            text.push_str(&commands.join(" ; "));
        }
        if !text.is_empty() {
            lines.push(format!("  {label}: {text}"));
        }
    }

    lines
}

/// `RESULT ok run ios-sim exit=0 run=<id> (23.1 s)`
fn result_line(event: &Value) -> String {
    let ok = event.get("ok").and_then(Value::as_bool).unwrap_or(false);
    let command = str_field(event, "command");
    let target = str_field(event, "target");
    let exit = event.get("exit").and_then(Value::as_u64).unwrap_or(70);
    let run = str_field(event, "run");
    let ms = event.get("ms").and_then(Value::as_u64).unwrap_or(0);
    let status = match event.get("status").and_then(Value::as_str) {
        Some("running") => "running",
        _ if ok => "ok",
        _ => "fail",
    };

    let mut line = format!("RESULT {status} {command}");
    if !target.is_empty() {
        line.push(' ');
        line.push_str(target);
    }
    line.push_str(&format!(
        " exit={exit} run={run} ({:.1} s)",
        ms as f64 / 1_000.0
    ));
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn stdout(lines: Vec<(Stream, String)>) -> Vec<String> {
        lines
            .into_iter()
            .filter(|(stream, _)| *stream == Stream::Stdout)
            .map(|(_, line)| line)
            .collect()
    }

    #[test]
    fn checks_render_with_continuations() {
        let event = json!({
            "type": "check", "id": "run.screen_blank", "status": "warn",
            "detail": "99.8% of pixels are #000000",
            "evidence": [{"path": "target/icm/latest/ios-sim/screen.png"}],
            "fix": {"summary": "", "commands": ["icm logs ios-sim --level warn", "icm explain run.screen_blank"], "by": "agent"}
        });
        assert_eq!(
            stdout(render(&event, false, false)),
            vec![
                "CHECK WARN run.screen_blank: 99.8% of pixels are #000000",
                "  evidence: target/icm/latest/ios-sim/screen.png",
                "  fix: icm logs ios-sim --level warn ; icm explain run.screen_blank",
            ]
        );
    }

    #[test]
    fn likely_causes_render_as_continuations() {
        let event = json!({
            "type": "check", "id": "run.app_panicked", "status": "fail",
            "detail": "panicked at src/lib.rs:107:9: too many items: 20",
            "likely_causes": ["a bug at src/lib.rs:107", "two lines\nof cause"],
            "fix": {"summary": "Fix the panic.", "commands": [], "by": "agent"}
        });
        assert_eq!(
            stdout(render(&event, true, false)),
            vec![
                "CHECK FAIL run.app_panicked: panicked at src/lib.rs:107:9: too many items: 20",
                "  likely: a bug at src/lib.rs:107",
                "  likely: two lines",
                "    of cause",
                "  fix: Fix the panic.",
            ]
        );
    }

    #[test]
    fn owner_fixes_are_labelled() {
        let event = json!({
            "type": "check", "id": "env.xcode_missing", "status": "fail", "detail": "",
            "evidence": [], "fix": {"summary": "Install Xcode", "commands": [], "by": "owner"}
        });
        let lines = stdout(render(&event, true, false));
        assert_eq!(lines[0], "CHECK FAIL env.xcode_missing");
        assert_eq!(lines[1], "  fix (owner): Install Xcode");
    }

    #[test]
    fn quiet_keeps_only_failures_warnings_and_result() {
        let pass =
            json!({"type": "check", "id": "deps.single_iced", "status": "pass", "detail": "ok"});
        let step =
            json!({"type": "step", "phase": "end", "name": "cargo.build", "ok": true, "ms": 100});
        let artifact = json!({"type": "artifact", "kind": "preview", "path": "x.png"});
        for event in [pass, step, artifact] {
            assert!(stdout(render(&event, true, false)).is_empty());
        }
        let result = json!({"type": "result", "ok": true, "exit": 0, "command": "run", "target": "web",
            "run": "id", "ms": 23100, "next": [{"cmd": "icm stop web", "why": "stop"}]});
        assert_eq!(
            stdout(render(&result, true, false)),
            vec!["RESULT ok run web exit=0 run=id (23.1 s)"]
        );
        assert_eq!(
            stdout(render(&result, false, false)),
            vec![
                "NEXT icm stop web   # stop",
                "RESULT ok run web exit=0 run=id (23.1 s)"
            ]
        );
    }

    #[test]
    fn steps_render_with_their_log() {
        let event = json!({"type": "step", "phase": "end", "name": "cargo.build", "ok": true,
            "ms": 41_200, "log": "target/icm/runs/x/steps/01-cargo.build.log"});
        assert_eq!(
            stdout(render(&event, false, false)),
            vec!["STEP cargo.build ok 41.2s  (log: target/icm/runs/x/steps/01-cargo.build.log)"]
        );
        let failed = json!({"type": "step", "phase": "end", "name": "x", "ok": false, "end": "timed_out", "ms": 5});
        assert!(stdout(render(&failed, false, false))[0].starts_with("STEP x TIMEOUT"));
    }
}
