//! The desktop app's logs (design §13.3): its stdout and stderr files,
//! parsed into normalized records `{ts, platform, source, level, tag, pid,
//! msg}`.
//!
//! On stderr, `iced::mobile::init_logger` writes one line per record,
//! `[2026-10-06T12:34:56.789Z INFO  my_app] message`; those become `source:
//! "app"` with their time, level and target. `ICM_EVENT` lines are `app`
//! records tagged `ICM_EVENT`. A panic (`thread 'main' panicked at
//! src/lib.rs:41:9:` and the lines after it) is one `error` record. Any
//! other stderr line is a `stderr` record, an `error` or `warn` when it
//! says so; stdout lines are `stdout` records. Lines without a time take
//! the previous record's (the launch time before the first one).

use crate::cli::Level;
use crate::time::Utc;
use serde_json::{Value, json};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// One log record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// UTC time, `2026-10-06T12:34:56.789Z`.
    pub ts: String,
    /// `app`, `stdout` or `stderr`.
    pub source: &'static str,
    /// The level.
    pub level: Level,
    /// The logger target, `ICM_EVENT`, `panic`, or none.
    pub tag: Option<String>,
    /// The message (several lines for a panic).
    pub msg: String,
}

/// A level's lower-case name.
pub fn level_name(level: Level) -> &'static str {
    match level {
        Level::Trace => "trace",
        Level::Debug => "debug",
        Level::Info => "info",
        Level::Warn => "warn",
        Level::Error => "error",
    }
}

impl Record {
    /// The record as JSON (`logs.ndjson`, `log` events, `records`).
    pub fn to_json(&self, pid: Option<i32>) -> Value {
        json!({
            "ts": self.ts,
            "platform": "desktop",
            "source": self.source,
            "level": level_name(self.level),
            "tag": self.tag,
            "pid": pid,
            "msg": self.msg,
        })
    }

    /// One readable line (continuation lines indented), for `app.log`.
    pub fn to_line(&self) -> String {
        let mut line = format!(
            "{} {:<5} {}",
            self.ts,
            level_name(self.level).to_ascii_uppercase(),
            self.source
        );
        if let Some(tag) = &self.tag {
            line.push(' ');
            line.push_str(tag);
        }
        line.push_str(": ");
        let mut lines = self.msg.lines();
        line.push_str(lines.next().unwrap_or(""));
        for more in lines {
            line.push_str("\n    ");
            line.push_str(more);
        }
        line
    }
}

/// `2026-10-06T12:34:56.789Z`.
pub fn timestamp(time: SystemTime) -> String {
    let millis = time
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_millis())
        .unwrap_or(0);
    let utc = Utc::from_system(time);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        utc.year, utc.month, utc.day, utc.hour, utc.minute, utc.second
    )
}

/// Parses lines into records, carrying state across chunks (`--follow`).
#[derive(Clone, Debug)]
pub struct Parser {
    source: &'static str,
    last_ts: String,
    after_panic: bool,
}

impl Parser {
    /// A parser for stderr (`stderr: true`) or stdout, whose untimed lines
    /// start at `launched`.
    pub fn new(stderr: bool, launched: &str) -> Parser {
        Parser {
            source: if stderr { "stderr" } else { "stdout" },
            last_ts: launched.to_string(),
            after_panic: false,
        }
    }

    /// Parses complete lines. A line that continues the previous record (a
    /// panic's message and backtrace, indented lines) is appended to it; one
    /// that continues a record from an earlier chunk becomes its own record
    /// at the same level.
    pub fn parse(&mut self, text: &str) -> Vec<Record> {
        let mut records: Vec<Record> = Vec::new();
        for line in text.lines() {
            let line = line.trim_end_matches('\r');
            if line.is_empty() {
                continue;
            }

            if self.source == "stderr" && self.continues(line) {
                match records.last_mut() {
                    Some(last) => {
                        last.msg.push('\n');
                        last.msg.push_str(line);
                    }
                    None => records.push(Record {
                        ts: self.last_ts.clone(),
                        source: "stderr",
                        level: Level::Error,
                        tag: Some("panic".to_string()),
                        msg: line.to_string(),
                    }),
                }
                continue;
            }
            self.after_panic = false;

            let record = if self.source == "stdout" {
                Record {
                    ts: self.last_ts.clone(),
                    source: "stdout",
                    level: Level::Info,
                    tag: None,
                    msg: line.to_string(),
                }
            } else {
                self.stderr_line(line)
            };
            self.last_ts = record.ts.clone();
            records.push(record);
        }
        records
    }

    /// Whether a stderr line continues the record before it.
    fn continues(&mut self, line: &str) -> bool {
        if self.after_panic {
            // The message, `note: run with RUST_BACKTRACE=1 ...`, and the
            // backtrace up to the next ordinary record.
            let ordinary = logger_line(line).is_some()
                || line.starts_with("ICM_EVENT ")
                || line.contains(" panicked at ");
            return !ordinary;
        }
        line.starts_with([' ', '\t'])
    }

    fn stderr_line(&mut self, line: &str) -> Record {
        if let Some((ts, level, target, msg)) = logger_line(line) {
            return Record {
                ts: ts.to_string(),
                source: "app",
                level,
                tag: Some(target.to_string()),
                msg: msg.to_string(),
            };
        }

        if let Some(event) = line.strip_prefix("ICM_EVENT ") {
            let kind = serde_json::from_str::<Value>(event)
                .ok()
                .and_then(|value| {
                    value
                        .get("kind")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_default();
            return Record {
                ts: self.last_ts.clone(),
                source: "app",
                level: match kind.as_str() {
                    "panic" => Level::Error,
                    "warning" => Level::Warn,
                    _ => Level::Info,
                },
                tag: Some("ICM_EVENT".to_string()),
                msg: event.to_string(),
            };
        }

        if line.contains(" panicked at ") {
            self.after_panic = true;
            return Record {
                ts: self.last_ts.clone(),
                source: "stderr",
                level: Level::Error,
                tag: Some("panic".to_string()),
                msg: line.to_string(),
            };
        }

        let lower = line.to_ascii_lowercase();
        let level = if lower.starts_with("error")
            || lower.contains(" error:")
            || lower.starts_with("fatal")
        {
            Level::Error
        } else if lower.starts_with("warning") || lower.starts_with("warn") {
            Level::Warn
        } else {
            Level::Info
        };
        Record {
            ts: self.last_ts.clone(),
            source: "stderr",
            level,
            tag: None,
            msg: line.to_string(),
        }
    }
}

/// `[2026-10-06T12:34:56.789Z INFO  my_app] message` → (ts, level, target,
/// message).
pub fn logger_line(line: &str) -> Option<(&str, Level, &str, &str)> {
    let rest = line.strip_prefix('[')?;
    let (head, msg) = rest.split_once(']')?;
    let mut parts = head.split_whitespace();
    let ts = parts.next()?;
    let level = match parts.next()? {
        "TRACE" => Level::Trace,
        "DEBUG" => Level::Debug,
        "INFO" => Level::Info,
        "WARN" => Level::Warn,
        "ERROR" => Level::Error,
        _ => return None,
    };
    let target = parts.next().unwrap_or("");
    let looks_like_time = ts.len() >= 20
        && ts.as_bytes()[..4].iter().all(u8::is_ascii_digit)
        && ts.contains('T')
        && ts.ends_with('Z');
    looks_like_time.then(|| (ts, level, target, msg.strip_prefix(' ').unwrap_or(msg)))
}

/// Both files' records, stderr's first, in time order (stable).
pub fn merge(stderr: Vec<Record>, stdout: Vec<Record>) -> Vec<Record> {
    let mut all = stderr;
    all.extend(stdout);
    all.sort_by(|a, b| a.ts.cmp(&b.ts));
    all
}

/// What `icm logs` keeps.
#[derive(Clone, Debug, Default)]
pub struct Filter {
    /// The lowest level.
    pub level: Option<Level>,
    /// Only records at or after this time.
    pub since: Option<String>,
    /// Alternatives (`a|b`), any of which the message or tag must contain.
    pub grep: Vec<String>,
}

impl Filter {
    /// A filter from `--level`, `--since` (`launch` or a duration ago) and
    /// `--grep` (substrings separated by `|`).
    pub fn new(level: Option<Level>, since: &str, grep: Option<&str>) -> Result<Filter, String> {
        let since = match since.trim() {
            "" | "launch" => None,
            duration => {
                let duration = crate::time::parse_duration(duration)?;
                let from = SystemTime::now()
                    .checked_sub(duration)
                    .unwrap_or(UNIX_EPOCH + Duration::ZERO);
                Some(timestamp(from))
            }
        };
        let grep = grep
            .map(|pattern| {
                pattern
                    .split('|')
                    .filter(|part| !part.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Ok(Filter { level, since, grep })
    }

    /// Whether a record passes.
    pub fn keeps(&self, record: &Record) -> bool {
        self.level.is_none_or(|level| record.level >= level)
            && self.since.as_ref().is_none_or(|since| record.ts >= *since)
            && (self.grep.is_empty()
                || self.grep.iter().any(|part| {
                    record.msg.contains(part.as_str())
                        || record
                            .tag
                            .as_deref()
                            .is_some_and(|tag| tag.contains(part.as_str()))
                }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STDERR: &str = "\
ICM_EVENT {\"v\":1,\"kind\":\"start\",\"protocol\":1}
[2026-10-06T12:34:56.789Z INFO  app] added \"Milk\"
[2026-10-06T12:34:57.001Z WARN  iced_wgpu] surface lost
ICM_EVENT {\"v\":1,\"kind\":\"panic\",\"message\":\"boom\"}

thread 'main' panicked at src/lib.rs:41:9:
index out of bounds: the len is 3 but the index is 7
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
[2026-10-06T12:34:58.000Z ERROR app] after
error: something broke
plain line
";

    #[test]
    fn stderr_lines_become_records() {
        let mut parser = Parser::new(true, "2026-10-06T12:34:50.000Z");
        let records = parser.parse(STDERR);
        let summary: Vec<(&str, &str, Option<&str>)> = records
            .iter()
            .map(|r| (r.source, level_name(r.level), r.tag.as_deref()))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("app", "info", Some("ICM_EVENT")),
                ("app", "info", Some("app")),
                ("app", "warn", Some("iced_wgpu")),
                ("app", "error", Some("ICM_EVENT")),
                ("stderr", "error", Some("panic")),
                ("app", "error", Some("app")),
                ("stderr", "error", None),
                ("stderr", "info", None),
            ]
        );
        assert_eq!(records[0].ts, "2026-10-06T12:34:50.000Z");
        assert_eq!(records[1].msg, "added \"Milk\"");
        assert_eq!(records[4].ts, "2026-10-06T12:34:57.001Z");
        assert_eq!(records[4].msg.lines().count(), 3);
        assert!(records[4].msg.contains("index out of bounds"));
        assert_eq!(records[7].ts, "2026-10-06T12:34:58.000Z");

        let line = records[4].to_line();
        assert!(line.starts_with("2026-10-06T12:34:57.001Z ERROR stderr panic: thread 'main'"));
        assert!(line.contains("\n    index out of bounds"));

        let json = records[1].to_json(Some(7));
        assert_eq!(json["platform"], "desktop");
        assert_eq!(json["level"], "info");
        assert_eq!(json["pid"], 7);
    }

    #[test]
    fn continuations_across_chunks_keep_their_level() {
        let mut parser = Parser::new(true, "2026-10-06T00:00:00.000Z");
        let first = parser.parse("thread 'main' panicked at src/lib.rs:1:1:\n");
        assert_eq!(first.len(), 1);
        let second = parser.parse("boom\n[2026-10-06T00:00:01.000Z INFO  app] next\n");
        assert_eq!(second.len(), 2);
        assert_eq!(second[0].level, Level::Error);
        assert_eq!(second[0].msg, "boom");
        assert_eq!(second[1].source, "app");
    }

    #[test]
    fn stdout_records_and_merging() {
        let mut out = Parser::new(false, "2026-10-06T12:34:55.000Z");
        let stdout = out.parse("hello\n");
        assert_eq!(stdout[0].source, "stdout");
        let mut err = Parser::new(true, "2026-10-06T12:34:50.000Z");
        let stderr = err.parse("[2026-10-06T12:34:56.789Z INFO  app] later\n");
        let merged = merge(stderr, stdout);
        assert_eq!(merged[0].msg, "hello");
        assert_eq!(merged[1].msg, "later");
    }

    #[test]
    fn filters() {
        let mut parser = Parser::new(true, "2026-10-06T12:34:50.000Z");
        let records = parser.parse(STDERR);
        let warn = Filter::new(Some(Level::Warn), "launch", None).unwrap();
        assert_eq!(records.iter().filter(|r| warn.keeps(r)).count(), 5);
        let grep = Filter::new(None, "launch", Some("Milk|surface")).unwrap();
        assert_eq!(records.iter().filter(|r| grep.keeps(r)).count(), 2);
        let tag = Filter::new(None, "launch", Some("ICM_EVENT")).unwrap();
        assert_eq!(records.iter().filter(|r| tag.keeps(r)).count(), 2);
        // Records from 2026 are older than a minute ago only if the clock
        // says so; a far-future filter keeps nothing.
        let recent = Filter {
            since: Some("2999-01-01T00:00:00.000Z".into()),
            ..Filter::default()
        };
        assert!(records.iter().all(|r| !recent.keeps(r)));
        assert!(Filter::new(None, "soon", None).is_err());
    }

    #[test]
    fn logger_lines_need_a_time_and_a_level() {
        assert_eq!(
            logger_line("[2026-10-06T12:34:56.789Z DEBUG my_app::ui] x"),
            Some(("2026-10-06T12:34:56.789Z", Level::Debug, "my_app::ui", "x"))
        );
        assert_eq!(logger_line("[INFO] x"), None);
        assert_eq!(logger_line("[2026-10-06T12:34:56.789Z LOUD app] x"), None);
        assert_eq!(
            timestamp(UNIX_EPOCH + Duration::from_millis(1_500)),
            "1970-01-01T00:00:01.500Z"
        );
    }
}
