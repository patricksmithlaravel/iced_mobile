//! The app's logs on the simulator, normalized (design §13.3, Appendix C
//! item 26).
//!
//! Sources, re-read live by `icm logs ios-sim`:
//! - `stdout`, `stderr`: the files `simctl launch --stdout/--stderr` keeps
//!   writing; `ICM_EVENT` lines and panics are in stderr.
//! - `oslog`: the unified-log collector `icm run` starts at launch
//!   (`log stream --level debug --style ndjson`), because `log show` later
//!   returns nothing for the Info and Debug types, which hold the app's
//!   `info!`/`debug!` records (the oslog crate maps Trace, Debug, Info,
//!   Warn, Error to Debug, Info, Default, Error, Fault).
//! - `system`: a live `log show` query for the app's errors from other
//!   subsystems and other processes' messages about the app.
//! - `crash`: crash reports (`~/Library/Logs/DiagnosticReports/<exe>-*.ips`)
//!   newer than the launch.
//!
//! Every record has the shape `{ts, platform, source, level, tag, pid,
//! msg}`; `ts` is null for stdout and stderr lines, which carry no time.

use crate::cli::Level;
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The subsystem iced's mobile logger writes to on iOS.
pub const ICED_SUBSYSTEM: &str = "iced";

/// One normalized log record.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Record {
    /// RFC 3339 UTC with milliseconds, when the source has a time.
    pub ts: Option<String>,
    /// `ios-sim`.
    pub platform: &'static str,
    /// `stdout`, `stderr`, `oslog`, `system` or `crash`.
    pub source: String,
    /// `trace`, `debug`, `info`, `warn` or `error`.
    pub level: &'static str,
    /// The oslog category, `ICM_EVENT`, the reporting process, ...
    pub tag: Option<String>,
    /// The process id, when known.
    pub pid: Option<i64>,
    /// The message.
    pub msg: String,
    /// Milliseconds since the Unix epoch, for ordering and `--since`.
    #[serde(skip)]
    pub unix_ms: Option<i64>,
}

impl Record {
    fn new(source: &str, level: &'static str, msg: impl Into<String>) -> Record {
        Record {
            ts: None,
            platform: "ios-sim",
            source: source.to_string(),
            level,
            tag: None,
            pid: None,
            msg: msg.into(),
            unix_ms: None,
        }
    }

    fn at(mut self, unix_ms: Option<i64>) -> Record {
        self.unix_ms = unix_ms;
        self.ts = unix_ms.map(rfc3339_ms);
        self
    }

    /// The level as the CLI's enum.
    pub fn level_enum(&self) -> Level {
        level_enum(self.level)
    }

    /// The readable one-line form used in `app.log`.
    pub fn line(&self) -> String {
        let ts = self.ts.as_deref().unwrap_or("-");
        let tag = self
            .tag
            .as_deref()
            .map(|tag| format!(" [{tag}]"))
            .unwrap_or_default();
        format!(
            "{ts} {:<5} {}{tag}: {}",
            self.level.to_ascii_uppercase(),
            self.source,
            self.msg
        )
    }
}

/// `info` gives `Level::Info`.
pub fn level_enum(level: &str) -> Level {
    match level {
        "trace" => Level::Trace,
        "debug" => Level::Debug,
        "warn" => Level::Warn,
        "error" => Level::Error,
        _ => Level::Info,
    }
}

/// `2026-10-07T00:32:06.123Z`.
pub fn rfc3339_ms(unix_ms: i64) -> String {
    let utc = crate::time::Utc::from_unix(unix_ms.div_euclid(1_000));
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        utc.year,
        utc.month,
        utc.day,
        utc.hour,
        utc.minute,
        utc.second,
        unix_ms.rem_euclid(1_000)
    )
}

/// Days since 1970-01-01 of a civil date (Howard Hinnant's
/// days_from_civil).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let month = i64::from(month);
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Parses the unified log's `2026-10-06 19:32:07.096609-0500` into
/// milliseconds since the epoch.
pub fn parse_log_time(text: &str) -> Option<i64> {
    let text = text.trim();
    let (date, rest) = text.split_once([' ', 'T'])?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;

    let offset_at = rest.rfind(['+', '-']).filter(|at| *at >= 8);
    let (time, offset) = match offset_at {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest.trim_end_matches('Z'), "+0000"),
    };
    let time = time.trim();
    let (clock, fraction) = time.split_once('.').unwrap_or((time, "0"));
    let mut clock_parts = clock.split(':');
    let hour: i64 = clock_parts.next()?.parse().ok()?;
    let minute: i64 = clock_parts.next()?.parse().ok()?;
    let second: i64 = clock_parts.next()?.parse().ok()?;
    let millis: i64 = format!("{:0<3}", &fraction[..fraction.len().min(3)])
        .parse()
        .ok()?;

    let sign = if offset.starts_with('-') { -1 } else { 1 };
    let digits: String = offset.chars().filter(char::is_ascii_digit).collect();
    let (oh, om) = match digits.len() {
        4 => (
            digits[..2].parse::<i64>().ok()?,
            digits[2..].parse::<i64>().ok()?,
        ),
        2 => (digits.parse::<i64>().ok()?, 0),
        _ => (0, 0),
    };
    let offset_secs = sign * (oh * 3_600 + om * 60);

    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3_600 + minute * 60 + second - offset_secs;
    Some(secs * 1_000 + millis)
}

/// The `--start` value `log show` takes for a Unix time: `2026-10-07
/// 00:32:06+0000`.
pub fn log_show_start(unix_ms: i64) -> String {
    let utc = crate::time::Utc::from_unix(unix_ms.div_euclid(1_000));
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}+0000",
        utc.year, utc.month, utc.day, utc.hour, utc.minute, utc.second
    )
}

/// An `ICM_EVENT <json>` line's object.
pub fn parse_event(line: &str) -> Option<Value> {
    let json = line.trim_start().strip_prefix("ICM_EVENT ")?;
    let value: Value = serde_json::from_str(json.trim()).ok()?;
    value.get("kind")?.as_str()?;
    Some(value)
}

/// A panic found in stderr: `(location, message)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Panic {
    /// `src/lib.rs:41:9`, when known.
    pub location: Option<String>,
    /// The panic message.
    pub message: String,
    /// The 1-based stderr line it starts on.
    pub line: u32,
}

/// The first panic in stderr text: an `ICM_EVENT` `panic` line, else Rust's
/// `thread '<name>' panicked at <file:line:col>:` followed by the message.
pub fn find_panic(stderr: &str) -> Option<Panic> {
    let lines: Vec<&str> = stderr.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        if let Some(event) = parse_event(line)
            && event.get("kind").and_then(Value::as_str) == Some("panic")
        {
            return Some(Panic {
                location: event
                    .get("location")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                message: event
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                line: index as u32 + 1,
            });
        }
    }
    for (index, line) in lines.iter().enumerate() {
        let Some(at) = line.find("panicked at ") else {
            continue;
        };
        let rest = line[at + "panicked at ".len()..].trim();
        // Rust 1.73+: `panicked at src/lib.rs:41:9:` then the message;
        // older: `panicked at 'message', src/lib.rs:41:9`.
        let (location, message) = if let Some(location) = rest.strip_suffix(':') {
            (
                Some(location.to_string()),
                lines.get(index + 1).map(|m| m.trim().to_string()),
            )
        } else if let Some((message, location)) = rest.rsplit_once("', ") {
            (
                Some(location.to_string()),
                Some(message.trim_start_matches('\'').to_string()),
            )
        } else {
            (None, Some(rest.to_string()))
        };
        return Some(Panic {
            location,
            message: message.unwrap_or_default(),
            line: index as u32 + 1,
        });
    }
    None
}

/// Records for an app stdout or stderr file's text.
pub fn stdio_records(source: &str, text: &str) -> Vec<Record> {
    let mut records = Vec::new();
    let mut in_panic = false;
    for line in text.lines() {
        if line.trim().is_empty() {
            in_panic = false;
            continue;
        }
        if let Some(event) = parse_event(line) {
            in_panic = false;
            let kind = event.get("kind").and_then(Value::as_str).unwrap_or("");
            let level = match kind {
                "panic" => "error",
                "warning" => "warn",
                _ => "info",
            };
            let mut record = Record::new(source, level, line.trim_start_matches("ICM_EVENT "));
            record.tag = Some("ICM_EVENT".to_string());
            record.pid = event.get("pid").and_then(Value::as_i64);
            records.push(record);
            continue;
        }
        if line.contains("panicked at ") {
            in_panic = true;
        }
        let level = if in_panic || line.starts_with("fatal runtime error") {
            "error"
        } else {
            "info"
        };
        records.push(Record::new(source, level, line));
    }
    records
}

/// The level of a unified-log record. For iced's subsystem the oslog crate's
/// mapping is undone (Default is `info!`, Error is `warn!`, Fault is
/// `error!`); other subsystems map type for type.
pub fn oslog_level(message_type: &str, subsystem: &str) -> &'static str {
    let iced = subsystem == ICED_SUBSYSTEM;
    match message_type {
        "Debug" if iced => "trace",
        "Debug" => "debug",
        "Info" if iced => "debug",
        "Info" => "info",
        "Error" if iced => "warn",
        "Error" | "Fault" => "error",
        _ => "info",
    }
}

/// A record for one `--style ndjson` line of `log stream`/`log show`; `None`
/// for headers and non-log events.
pub fn oslog_record(source: &str, line: &str) -> Option<Record> {
    let value: Value = serde_json::from_str(line.trim()).ok()?;
    if value.get("eventType").and_then(Value::as_str) != Some("logEvent") {
        return None;
    }
    let str_field = |key: &str| value.get(key).and_then(Value::as_str).unwrap_or("");
    let subsystem = str_field("subsystem");
    let category = str_field("category");
    let level = oslog_level(str_field("messageType"), subsystem);

    let mut record = Record::new(source, level, continued(str_field("eventMessage"))).at(value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_log_time));
    record.pid = value.get("processID").and_then(Value::as_i64);
    let process = Path::new(str_field("processImagePath"))
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    record.tag = Some(match (subsystem, category) {
        (ICED_SUBSYSTEM, "") => ICED_SUBSYSTEM.to_string(),
        (ICED_SUBSYSTEM, category) => category.to_string(),
        ("", "") => process,
        (subsystem, "") => format!("{process} {subsystem}"),
        (subsystem, category) => format!("{process} {subsystem}:{category}"),
    });
    Some(record)
}

/// A multi-line message with its continuation lines indented two spaces,
/// the human line protocol's continuation form (`LOG` lines stay
/// grep-able).
pub fn continued(message: &str) -> String {
    message.trim_end().replace('\n', "\n  ")
}

/// Records for an ndjson file of unified-log lines.
pub fn oslog_records(source: &str, text: &str) -> Vec<Record> {
    text.lines()
        .filter_map(|line| oslog_record(source, line))
        .collect()
}

/// A crash report's summary record (`.ips`: a JSON header line, then a JSON
/// body).
pub fn crash_record(path: &Path) -> Option<Record> {
    let text = std::fs::read_to_string(path).ok()?;
    let (header, body) = text.split_once('\n').unwrap_or((&text, ""));
    let header: Value = serde_json::from_str(header).unwrap_or(Value::Null);
    let body: Value = serde_json::from_str(body).unwrap_or(Value::Null);

    let mut parts = Vec::new();
    if let Some(exception) = body.get("exception") {
        let kind = exception.get("type").and_then(Value::as_str).unwrap_or("");
        let signal = exception
            .get("signal")
            .and_then(Value::as_str)
            .unwrap_or("");
        parts.push(format!("{kind} {signal}").trim().to_string());
    }
    if let Some(indicator) = body
        .get("termination")
        .and_then(|t| t.get("indicator"))
        .and_then(Value::as_str)
    {
        parts.push(indicator.to_string());
    }
    if let Some(asi) = body.get("asi").and_then(Value::as_object) {
        for messages in asi.values().filter_map(Value::as_array) {
            for message in messages.iter().filter_map(Value::as_str) {
                parts.push(message.trim().to_string());
            }
        }
    }
    let summary = if parts.is_empty() {
        "crashed".to_string()
    } else {
        parts.join("; ")
    };

    let mut record = Record::new(
        "crash",
        "error",
        format!("{summary} ({})", crate::paths::display(path)),
    )
    // The capture time is the crash; the header's timestamp is when the
    // report was written, up to ~10 s later.
    .at(body
        .get("captureTime")
        .or_else(|| header.get("timestamp"))
        .and_then(Value::as_str)
        .and_then(parse_log_time));
    record.pid = body.get("pid").and_then(Value::as_i64);
    record.tag = header
        .get("app_name")
        .or_else(|| header.get("name"))
        .and_then(Value::as_str)
        .map(str::to_string);
    Some(record)
}

/// Whether a crash report is this app's on this simulator: its `pid` is
/// the app's, its process path is inside the simulator's directory, or it
/// is an iOS-simulator report (platform 7) for the bundle id. Every
/// template app's executable is called `app`, so the name alone would also
/// match desktop apps' reports.
pub fn crash_belongs(path: &Path, pid: Option<i64>, udid: &str, app_id: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let (header, body) = text.split_once('\n').unwrap_or((&text, ""));
    let header: Value = serde_json::from_str(header).unwrap_or(Value::Null);
    let body: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    if pid.is_some() && body.get("pid").and_then(Value::as_i64) == pid {
        return true;
    }
    if !udid.is_empty()
        && body
            .get("procPath")
            .and_then(Value::as_str)
            .is_some_and(|proc_path| proc_path.contains(udid))
    {
        return true;
    }
    header.get("platform").and_then(Value::as_i64) == Some(7)
        && header.get("bundleID").and_then(Value::as_str) == Some(app_id)
        && pid.is_none()
}

/// Crash reports for an executable written at or after `since_unix_ms`.
pub fn crash_reports(dir: &Path, exe: &str, since_unix_ms: i64) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let prefix = format!("{exe}-");
    let mut reports: Vec<(std::time::SystemTime, PathBuf)> = read
        .flatten()
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.starts_with(&prefix) && (name.ends_with(".ips") || name.ends_with(".crash"))
        })
        .filter_map(|entry| {
            let modified = entry.metadata().ok()?.modified().ok()?;
            let ms = modified
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_millis() as i64;
            (ms + 1_000 >= since_unix_ms).then(|| (modified, entry.path()))
        })
        .collect();
    reports.sort();
    reports.into_iter().map(|(_, path)| path).collect()
}

/// `--source`'s groups.
pub fn source_group(source: &str) -> &'static str {
    match source {
        "stdout" | "stderr" | "oslog" => "app",
        "crash" => "crash",
        _ => "system",
    }
}

/// What `icm logs` keeps.
#[derive(Clone, Debug, Default)]
pub struct Filter {
    /// The lowest level.
    pub level: Option<Level>,
    /// `app`, `system`, `crash`, or `None` for all.
    pub source: Option<&'static str>,
    /// Alternatives separated by `|`, each a case-insensitive substring.
    pub grep: Option<String>,
    /// Records with a time before this are dropped.
    pub since_unix_ms: Option<i64>,
    /// Keep only the last N.
    pub tail: Option<usize>,
}

impl Filter {
    /// Whether a record passes (before `tail`).
    pub fn keeps(&self, record: &Record) -> bool {
        if let Some(level) = self.level
            && record.level_enum() < level
        {
            return false;
        }
        if let Some(group) = self.source
            && source_group(&record.source) != group
        {
            return false;
        }
        if let (Some(since), Some(ms)) = (self.since_unix_ms, record.unix_ms)
            && ms < since
        {
            return false;
        }
        if let Some(grep) = &self.grep {
            let haystack =
                format!("{} {}", record.tag.as_deref().unwrap_or(""), record.msg).to_lowercase();
            if !grep
                .split('|')
                .map(|alternative| alternative.trim().to_lowercase())
                .filter(|alternative| !alternative.is_empty())
                .any(|alternative| haystack.contains(&alternative))
            {
                return false;
            }
        }
        true
    }

    /// Filters, then keeps the last `tail`.
    pub fn apply(&self, records: Vec<Record>) -> Vec<Record> {
        let mut kept: Vec<Record> = records.into_iter().filter(|r| self.keeps(r)).collect();
        if let Some(tail) = self.tail
            && kept.len() > tail
        {
            kept.drain(..kept.len() - tail);
        }
        kept
    }
}

/// Merges sources into one list: the untimed stdio records first, in file
/// order, then the timed ones by time.
pub fn merge(stdio: Vec<Record>, mut timed: Vec<Record>) -> Vec<Record> {
    timed.sort_by_key(|record| record.unix_ms.unwrap_or(i64::MIN));
    let mut all = stdio;
    all.extend(timed);
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unified_log_times_parse() {
        // 2026-10-07T00:32:07.096Z
        let ms = parse_log_time("2026-10-06 19:32:07.096609-0500").unwrap();
        assert_eq!(rfc3339_ms(ms), "2026-10-07T00:32:07.096Z");
        assert_eq!(parse_log_time("2026-10-07 00:32:06+0000"), Some(ms - 1_096));
        assert_eq!(log_show_start(ms), "2026-10-07 00:32:07+0000");
        assert_eq!(
            parse_log_time("2000-02-29 00:00:00.5Z"),
            Some(951_782_400_500)
        );
        assert_eq!(parse_log_time("yesterday"), None);
    }

    #[test]
    fn events_and_panics_are_found_in_stderr() {
        let stderr = "ICM_EVENT {\"v\":1,\"kind\":\"start\",\"protocol\":1,\"pid\":42}\n\
                      hello\n\
                      \n\
                      thread 'main' panicked at src/lib.rs:41:9:\n\
                      index out of bounds: the len is 0 but the index is 3\n\
                      note: run with `RUST_BACKTRACE=1`\n";
        let panic = find_panic(stderr).unwrap();
        assert_eq!(panic.location.as_deref(), Some("src/lib.rs:41:9"));
        assert_eq!(
            panic.message,
            "index out of bounds: the len is 0 but the index is 3"
        );
        assert_eq!(panic.line, 4);

        let with_event = format!(
            "{stderr}ICM_EVENT {{\"v\":1,\"kind\":\"panic\",\"message\":\"boom\",\"location\":\"src/x.rs:1:2\",\"thread\":\"main\"}}\n"
        );
        let panic = find_panic(&with_event).unwrap();
        assert_eq!(panic.message, "boom");
        assert_eq!(panic.location.as_deref(), Some("src/x.rs:1:2"));
        assert_eq!(find_panic("all good\n"), None);

        let records = stdio_records("stderr", stderr);
        assert_eq!(records[0].tag.as_deref(), Some("ICM_EVENT"));
        assert_eq!(records[0].pid, Some(42));
        assert_eq!(records[1].level, "info");
        assert_eq!(records[2].level, "error");
        assert_eq!(records[3].level, "error");
        assert!(records.iter().all(|r| r.ts.is_none()));
    }

    #[test]
    fn oslog_lines_become_records() {
        let line = r#"{"messageType":"Error","eventType":"logEvent","subsystem":"iced","category":"app","processImagePath":"\/x\/App.app\/app","timestamp":"2026-10-06 19:32:07.096609-0500","eventMessage":"low memory","processID":29731}"#;
        let record = oslog_record("oslog", line).unwrap();
        assert_eq!(record.level, "warn");
        assert_eq!(record.tag.as_deref(), Some("app"));
        assert_eq!(record.pid, Some(29731));
        assert_eq!(record.ts.as_deref(), Some("2026-10-07T00:32:07.096Z"));
        assert!(record.line().contains("WARN  oslog [app]: low memory"));

        let system = r#"{"messageType":"Default","eventType":"logEvent","subsystem":"com.apple.UIKit","category":"","processImagePath":"\/x\/SpringBoard","timestamp":"2026-10-06 19:32:08.000000-0500","eventMessage":"launching com.example.app","processID":1}"#;
        let record = oslog_record("system", system).unwrap();
        assert_eq!(record.level, "info");
        assert_eq!(record.tag.as_deref(), Some("SpringBoard com.apple.UIKit"));

        assert!(oslog_record("oslog", "Filtering the log data using \"x\"").is_none());
        assert_eq!(oslog_level("Default", "iced"), "info");
        assert_eq!(oslog_level("Info", "iced"), "debug");
        assert_eq!(oslog_level("Debug", "iced"), "trace");
        assert_eq!(oslog_level("Fault", "iced"), "error");
    }

    #[test]
    fn filters_apply_level_source_grep_since_and_tail() {
        let mut records = stdio_records(
            "stderr",
            "one\ntwo\nthread 'main' panicked at a.rs:1:1:\nboom\n",
        );
        records.push(
            oslog_record(
                "oslog",
                r#"{"messageType":"Default","eventType":"logEvent","subsystem":"iced","category":"app","timestamp":"2026-10-07 00:00:00+0000","eventMessage":"submitted","processID":1}"#,
            )
            .unwrap(),
        );

        let warn = Filter {
            level: Some(Level::Warn),
            ..Filter::default()
        };
        assert_eq!(warn.apply(records.clone()).len(), 2);

        let grep = Filter {
            grep: Some("SUBMIT|two".into()),
            ..Filter::default()
        };
        assert_eq!(grep.apply(records.clone()).len(), 2);

        let crash_only = Filter {
            source: Some("crash"),
            ..Filter::default()
        };
        assert!(crash_only.apply(records.clone()).is_empty());

        let since = Filter {
            since_unix_ms: Some(parse_log_time("2026-10-08 00:00:00+0000").unwrap()),
            ..Filter::default()
        };
        // Untimed records always pass `--since`.
        assert_eq!(since.apply(records.clone()).len(), 4);

        let tail = Filter {
            tail: Some(2),
            ..Filter::default()
        };
        let kept = tail.apply(records);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[1].msg, "submitted");
    }

    #[test]
    fn crash_reports_are_summarized() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app-2026-10-06-193200.ips");
        std::fs::write(
            &path,
            "{\"app_name\":\"app\",\"timestamp\":\"2026-10-06 19:32:00.00 -0500\"}\n{\"pid\":7,\"exception\":{\"type\":\"EXC_CRASH\",\"signal\":\"SIGABRT\"},\"termination\":{\"indicator\":\"Abort trap: 6\"},\"asi\":{\"libsystem_c.dylib\":[\"abort() called\"]}}",
        )
        .unwrap();
        std::fs::write(dir.path().join("other-2026.ips"), "{}").unwrap();
        let found = crash_reports(dir.path(), "app", 0);
        assert_eq!(found, vec![path.clone()]);
        assert!(crash_belongs(&path, Some(7), "UDID", "com.x"));
        assert!(!crash_belongs(&path, Some(8), "UDID", "com.x"));
        std::fs::write(
            &path,
            "{\"platform\":7,\"bundleID\":\"com.x\"}\n{\"pid\":9,\"procPath\":\"/Users/USER/Library/Developer/CoreSimulator/Devices/UDID/data/x/App.app/app\"}",
        )
        .unwrap();
        assert!(crash_belongs(&path, Some(8), "UDID", "com.x"));
        assert!(crash_belongs(&path, None, "", "com.x"));
        assert!(!crash_belongs(&path, Some(8), "OTHER", "com.x"));
        std::fs::write(
            &path,
            "{\"app_name\":\"app\",\"timestamp\":\"2026-10-06 19:32:00.00 -0500\"}\n{\"pid\":7,\"exception\":{\"type\":\"EXC_CRASH\",\"signal\":\"SIGABRT\"},\"termination\":{\"indicator\":\"Abort trap: 6\"},\"asi\":{\"libsystem_c.dylib\":[\"abort() called\"]}}",
        )
        .unwrap();
        let record = crash_record(&path).unwrap();
        assert_eq!(record.level, "error");
        assert_eq!(record.pid, Some(7));
        assert!(
            record
                .msg
                .starts_with("EXC_CRASH SIGABRT; Abort trap: 6; abort() called")
        );
        assert!(crash_reports(dir.path(), "app", i64::MAX / 2).is_empty());
    }
}
