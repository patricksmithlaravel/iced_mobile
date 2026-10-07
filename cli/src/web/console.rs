//! The web app's log: `console.ndjson`, written by the session host as
//! Chrome reports the page's console, exceptions and browser log entries
//! (and, for `--show`, what the page's forwarder posts), and read back by
//! `icm logs web`.
//!
//! Each line is one record in the shape every platform's `logs.ndjson`
//! uses (design §13.3): `{ts, epoch_ms, platform: "web", source, level,
//! tag, pid: null, msg}`. Sources are `console` (the page's console,
//! including the app's `log` output and `ICM_EVENT` lines), `exception`
//! (uncaught errors), `browser` (Chrome's own log: failed loads,
//! WebGL messages), `forwarder` (a system browser opened with `--show`) and
//! `crash` (the page's renderer died).

use crate::cli::{Level, LogSource};
use crate::time::Utc;
use serde_json::{Value, json};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The tag of `ICM_EVENT` console lines.
pub const ICM_EVENT_TAG: &str = "ICM_EVENT";

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// RFC 3339 with milliseconds, UTC.
pub fn timestamp(epoch_ms: u64) -> String {
    let utc = Utc::from_unix((epoch_ms / 1000) as i64);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        utc.year,
        utc.month,
        utc.day,
        utc.hour,
        utc.minute,
        utc.second,
        epoch_ms % 1000
    )
}

/// A record.
pub fn record(epoch_ms: u64, source: &str, level: &str, tag: &str, msg: &str) -> Value {
    json!({
        "ts": timestamp(epoch_ms),
        "epoch_ms": epoch_ms,
        "platform": "web",
        "source": source,
        "level": level,
        "tag": tag,
        "pid": null,
        "msg": msg,
    })
}

/// The level of a `Runtime.consoleAPICalled` type.
pub fn console_level(kind: &str) -> &'static str {
    match kind {
        "error" | "assert" => "error",
        "warning" | "warn" => "warn",
        "debug" => "debug",
        "trace" => "trace",
        _ => "info",
    }
}

/// The level of a `Log.entryAdded` entry. Chrome's own chatter that says
/// nothing about the app drops to `debug`, so `--level warn` shows the
/// app's problems: SwiftShader's "GPU stall due to ReadPixels" (every
/// screenshot causes one) and WebGPU's "No available adapters" (headless
/// Chrome has none; wgpu then uses WebGL2, `web.renderer_fallback`).
pub fn browser_level(level: &str, text: &str) -> &'static str {
    let noise = (text.contains("GL Driver Message") && text.contains("Performance"))
        || text.starts_with("No available adapters");
    if noise {
        return "debug";
    }
    match level {
        "error" => "error",
        "warning" => "warn",
        "verbose" => "debug",
        _ => "info",
    }
}

/// Joins console arguments (CDP `RemoteObject`s) the way the console
/// shows them: primitives by value, objects by description.
pub fn format_args(args: &[Value]) -> String {
    args.iter()
        .map(|arg| match arg.get("value") {
            Some(Value::String(text)) => text.clone(),
            Some(Value::Null) | None => arg
                .get("description")
                .and_then(Value::as_str)
                .or_else(|| arg.get("unserializableValue").and_then(Value::as_str))
                .or_else(|| arg.get("type").and_then(Value::as_str))
                .unwrap_or("")
                .to_string(),
            Some(other) => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The JSON of an `ICM_EVENT <json>` console line.
pub fn icm_event(msg: &str) -> Option<Value> {
    let json = msg.strip_prefix("ICM_EVENT ")?;
    let value: Value = serde_json::from_str(json.trim()).ok()?;
    value.get("kind")?.as_str()?;
    Some(value)
}

/// The numeric rank of a level (trace 0 .. error 4).
pub fn rank(level: &str) -> u8 {
    match level {
        "trace" => 0,
        "debug" => 1,
        "info" => 2,
        "warn" | "warning" => 3,
        "error" => 4,
        _ => 2,
    }
}

fn level_name(level: Level) -> &'static str {
    match level {
        Level::Trace => "trace",
        Level::Debug => "debug",
        Level::Info => "info",
        Level::Warn => "warn",
        Level::Error => "error",
    }
}

/// What `icm logs web` keeps.
#[derive(Clone, Debug, Default)]
pub struct Filter {
    /// Only records at or after this time (ms since the epoch).
    pub since_ms: Option<u64>,
    /// The lowest level.
    pub level: Option<Level>,
    /// Which sources.
    pub source: Option<LogSource>,
    /// `--grep` ([`crate::grep`]).
    pub grep: Option<String>,
}

impl Filter {
    /// Whether a record passes.
    pub fn keeps(&self, record: &Value) -> bool {
        let field = |key: &str| record.get(key).and_then(Value::as_str).unwrap_or("");
        if let Some(since) = self.since_ms
            && record
                .get("epoch_ms")
                .and_then(Value::as_u64)
                .is_some_and(|ms| ms < since)
        {
            return false;
        }
        if let Some(level) = self.level
            && rank(field("level")) < rank(level_name(level))
        {
            return false;
        }
        if let Some(source) = self.source {
            let class = match field("source") {
                "browser" => LogSource::System,
                "crash" => LogSource::Crash,
                _ => LogSource::App,
            };
            if source != LogSource::All && source != class {
                return false;
            }
        }
        if !crate::grep::keeps(self.grep.as_deref(), &[field("tag"), field("msg")]) {
            return false;
        }
        true
    }
}

/// `--since`: `launch` (everything this session logged) or a duration ago.
pub fn since(spec: &str, now_ms: u64) -> Result<Option<u64>, String> {
    if spec == "launch" || spec.is_empty() {
        return Ok(None);
    }
    let duration: Duration = crate::time::parse_duration(spec)?;
    Ok(Some(now_ms.saturating_sub(duration.as_millis() as u64)))
}

/// Parses NDJSON records, skipping lines that are not objects.
pub fn parse(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(Value::is_object)
        .collect()
}

/// Reads a console file (missing: no records).
pub fn read(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .map(|text| parse(&text))
        .unwrap_or_default()
}

/// One readable line for `app.log`.
pub fn line(record: &Value) -> String {
    let field = |key: &str| record.get(key).and_then(Value::as_str).unwrap_or("");
    let tag = field("tag");
    let tag = if tag.is_empty() {
        String::new()
    } else {
        format!(" [{tag}]")
    };
    format!(
        "{} {:5} {}{}: {}",
        field("ts"),
        field("level").to_ascii_uppercase(),
        field("source"),
        tag,
        field("msg")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_have_the_common_shape() {
        let record = record(1_000_000_000_123, "console", "info", "", "hello");
        assert_eq!(record["ts"], "2001-09-09T01:46:40.123Z");
        assert_eq!(record["platform"], "web");
        assert!(record["pid"].is_null());
        assert_eq!(
            line(&record),
            "2001-09-09T01:46:40.123Z INFO  console: hello"
        );
    }

    #[test]
    fn console_arguments_join_like_the_console() {
        let args = vec![
            json!({"type": "string", "value": "count"}),
            json!({"type": "number", "value": 3}),
            json!({"type": "object", "description": "Object"}),
            json!({"type": "undefined"}),
        ];
        assert_eq!(format_args(&args), "count 3 Object undefined");
    }

    #[test]
    fn icm_events_parse() {
        let event = icm_event(r#"ICM_EVENT {"v":1,"kind":"ready","ms":12}"#).unwrap();
        assert_eq!(event["kind"], "ready");
        assert!(icm_event("ICM_EVENT not json").is_none());
        assert!(icm_event("hello").is_none());
    }

    #[test]
    fn filters_apply_level_source_time_and_text() {
        let records = [
            record(1_000, "console", "debug", "", "noise"),
            record(2_000, "console", "warn", "", "careful"),
            record(3_000, "browser", "error", "network", "404 /x.png"),
            record(4_000, "exception", "error", "exception", "boom"),
        ];
        let keep = |filter: &Filter| -> Vec<&str> {
            records
                .iter()
                .filter(|r| filter.keeps(r))
                .map(|r| r["msg"].as_str().unwrap())
                .collect()
        };
        assert_eq!(keep(&Filter::default()).len(), 4);
        assert_eq!(
            keep(&Filter {
                level: Some(Level::Warn),
                ..Filter::default()
            }),
            ["careful", "404 /x.png", "boom"]
        );
        assert_eq!(
            keep(&Filter {
                source: Some(LogSource::App),
                level: Some(Level::Error),
                ..Filter::default()
            }),
            ["boom"]
        );
        assert_eq!(
            keep(&Filter {
                source: Some(LogSource::System),
                ..Filter::default()
            }),
            ["404 /x.png"]
        );
        assert_eq!(
            keep(&Filter {
                since_ms: Some(2_500),
                grep: Some("boo".into()),
                ..Filter::default()
            }),
            ["boom"]
        );
    }

    #[test]
    fn chrome_noise_is_debug() {
        assert_eq!(browser_level("warning", "No available adapters."), "debug");
        assert_eq!(
            browser_level(
                "warning",
                "[.WebGL-0x1]GL Driver Message (OpenGL, Performance, GL_CLOSE_PATH_NV, High): GPU stall due to ReadPixels"
            ),
            "debug"
        );
        assert_eq!(browser_level("error", "Failed to load resource"), "error");
    }

    #[test]
    fn since_takes_launch_or_a_duration() {
        assert_eq!(since("launch", 10_000).unwrap(), None);
        assert_eq!(since("5s", 10_000).unwrap(), Some(5_000));
        assert!(since("soon", 10_000).is_err());
    }
}
