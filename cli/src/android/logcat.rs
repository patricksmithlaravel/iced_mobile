//! logcat (design §10.4 steps 10–14, §13.3): parsing
//! `logcat -v threadtime,epoch` lines, picking the app's records, and the
//! `ICM_EVENT` protocol lines the framework writes under the tag
//! `ICM_EVENT`.
//!
//! The log is never cleared (`logcat -c` would destroy other apps'
//! evidence); every query starts at the launch mark (`-T <epoch>`).
//!
//! A record is the app's when its process was the app's at the time
//! ([`Processes`]). Its tag says nothing: `debug.icm.events` is a system
//! property, so every iced_mobile app on the device writes `ICM_EVENT`
//! lines, and any app may log under `iced` or `RustStdoutStderr`.

use crate::cli::Level;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// System tags kept when they concern the app.
pub const SYSTEM_TAGS: &[&str] = &["AndroidRuntime", "DEBUG", "ActivityManager", "libc"];

/// One logcat record.
#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    /// Seconds since the epoch, as logcat printed them (`1728245000.123`).
    pub ts: String,
    /// The process.
    pub pid: u32,
    /// The thread.
    pub tid: u32,
    /// The priority letter (`V D I W E F A`).
    pub priority: char,
    /// The tag.
    pub tag: String,
    /// The message.
    pub msg: String,
}

impl Record {
    /// The level name (`trace` … `error`).
    pub fn level(&self) -> &'static str {
        match self.priority {
            'V' => "trace",
            'D' => "debug",
            'I' => "info",
            'W' => "warn",
            _ => "error",
        }
    }

    /// The level as the CLI's enum.
    pub fn level_enum(&self) -> Level {
        match self.priority {
            'V' => Level::Trace,
            'D' => Level::Debug,
            'I' => Level::Info,
            'W' => Level::Warn,
            _ => Level::Error,
        }
    }

    /// The timestamp as seconds.
    pub fn seconds(&self) -> f64 {
        self.ts.parse().unwrap_or(0.0)
    }

    /// A `logs.ndjson` record (design §13.3).
    pub fn to_json(&self, source: &str) -> Value {
        json!({
            "ts": crate::time::Utc::from_unix(self.seconds() as i64).rfc3339(),
            "epoch": self.ts,
            "platform": "android",
            "source": source,
            "level": self.level(),
            "tag": self.tag,
            "pid": self.pid,
            "msg": self.msg,
        })
    }

    /// The readable form for `app.log`.
    pub fn line(&self) -> String {
        format!(
            "{} {:>5} {} {}: {}",
            self.ts, self.pid, self.priority, self.tag, self.msg
        )
    }
}

/// Parses one `threadtime,epoch` line:
/// `1728245000.123  1234  1250 I ICM_EVENT: {...}`.
pub fn parse_line(line: &str) -> Option<Record> {
    let line = line.trim_end_matches(['\r', '\n']);
    let trimmed = line.trim_start();
    if trimmed.starts_with("---------") {
        return None;
    }
    let mut rest = trimmed;
    let mut fields = Vec::with_capacity(4);
    for _ in 0..4 {
        let end = rest.find(char::is_whitespace)?;
        fields.push(&rest[..end]);
        rest = rest[end..].trim_start();
    }
    let ts = fields[0];
    if !ts.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    let pid = fields[1].parse().ok()?;
    let tid = fields[2].parse().ok()?;
    let priority = fields[3].chars().next().filter(|_| fields[3].len() == 1)?;
    let (tag, msg) = match rest.find(": ") {
        Some(at) => (rest[..at].trim(), &rest[at + 2..]),
        None => (rest.trim_end_matches(':').trim(), ""),
    };
    Some(Record {
        ts: ts.to_string(),
        pid,
        tid,
        priority,
        tag: tag.to_string(),
        msg: msg.to_string(),
    })
}

/// Parses logcat output, skipping lines that are not records.
pub fn parse(text: &str) -> Vec<Record> {
    text.lines().filter_map(parse_line).collect()
}

/// The events-buffer tags Android writes when it starts an app's process
/// (`am_proc_start: [user,pid,uid,process,type,component]`) and when it
/// learns that one died (`am_proc_died: [user,pid,process,...]`).
pub const PROCESS_TAGS: &[&str] = &["am_proc_start", "am_proc_died"];

/// How long before Android logs `am_proc_start` a new process may already
/// write its first lines: the child runs while the system server is still
/// recording the start.
const START_SLACK: f64 = 1.0;

/// One start or death of a process, from the events buffer.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Change {
    /// Epoch seconds.
    at: f64,
    /// A start; else a death.
    start: bool,
    /// The process was the app's (its name is the app id, or `<id>:<name>`).
    app: bool,
}

/// Which process was the app's when: the starts and deaths Android logged
/// for each pid, and the pids known to be the app's now (`pidof`) or from
/// an earlier look. A pid outlives its process and is handed out again, so
/// a record counts as the app's only while its pid was the app's.
#[derive(Clone, Debug, Default)]
pub struct Processes {
    changes: BTreeMap<u32, Vec<Change>>,
    known: BTreeSet<u32>,
    /// The app's pids now, from `pidof`.
    pub now: Vec<u32>,
}

impl Processes {
    /// Only pids known to be the app's, with no starts or deaths to place
    /// them in time.
    pub fn of(known: impl IntoIterator<Item = u32>) -> Processes {
        Processes {
            known: known.into_iter().collect(),
            ..Processes::default()
        }
    }

    /// The app's processes from `pidof`'s pids now and the
    /// [`PROCESS_TAGS`] records of the events buffer.
    pub fn read(app_id: &str, now: Vec<u32>, events: &[Record]) -> Processes {
        let mut changes: BTreeMap<u32, Vec<Change>> = BTreeMap::new();
        for record in events {
            let start = match record.tag.as_str() {
                "am_proc_start" => true,
                "am_proc_died" => false,
                _ => continue,
            };
            let fields: Vec<&str> = record
                .msg
                .trim()
                .trim_start_matches('[')
                .trim_end_matches(']')
                .split(',')
                .map(str::trim)
                .collect();
            let name = if start { fields.get(3) } else { fields.get(2) };
            let (Some(pid), Some(name)) = (fields.get(1).and_then(|pid| pid.parse().ok()), name)
            else {
                continue;
            };
            let app = *name == app_id
                || name
                    .strip_prefix(app_id)
                    .is_some_and(|rest| rest.starts_with(':'));
            let at = record.seconds() - if start && app { START_SLACK } else { 0.0 };
            changes
                .entry(pid)
                .or_default()
                .push(Change { at, start, app });
        }
        for list in changes.values_mut() {
            list.sort_by(|a, b| a.at.total_cmp(&b.at));
        }
        Processes {
            changes,
            known: now.iter().copied().collect(),
            now,
        }
    }

    /// Adds pids known to be the app's (seen earlier in this run, or the
    /// session's).
    pub fn knowing(mut self, pids: impl IntoIterator<Item = u32>) -> Processes {
        self.known.extend(pids);
        self
    }

    /// Whether `pid` was the app's at `at` (epoch seconds).
    pub fn owned(&self, pid: u32, at: f64) -> bool {
        let Some(changes) = self.changes.get(&pid) else {
            return self.known.contains(&pid);
        };
        let before = changes.iter().rev().find(|change| change.at <= at);
        let after = changes.iter().find(|change| change.at > at);
        match (before, after) {
            // The app's process, started before `at`.
            (Some(change), _) if change.start && change.app => true,
            // After another process's start or a death, the pid is not the
            // app's, unless `pidof` names the app now and the buffer missed
            // its start.
            (Some(_), None) => self.now.contains(&pid),
            (Some(_), Some(_)) => false,
            // A process the buffer only saw die had the pid until then.
            (None, Some(change)) => !change.start && change.app,
            (None, None) => self.known.contains(&pid),
        }
    }

    /// Whether a record was written by the app's process.
    pub fn owns(&self, record: &Record) -> bool {
        self.owned(record.pid, record.seconds())
    }

    /// The pids the app had at some time, as far as these records say.
    pub fn pids(&self) -> BTreeSet<u32> {
        let mut pids = self.known.clone();
        for (pid, changes) in &self.changes {
            if changes.iter().any(|change| change.app) {
                let _ = pids.insert(*pid);
            }
        }
        pids
    }
}

/// Where a record belongs, or `None` when it is not the app's business.
/// Stateless: see [`select`] for native crash dumps.
pub fn classify(record: &Record, app_id: &str, processes: &Processes) -> Option<&'static str> {
    if processes.owns(record) {
        return Some("app");
    }
    if record.tag == "DEBUG" && record.msg.contains(app_id) {
        return Some("crash");
    }
    if SYSTEM_TAGS.contains(&record.tag.as_str()) && record.msg.contains(app_id) {
        return Some("system");
    }
    None
}

/// The app's records with their source. A native crash dump names the
/// app once (`>>> com.example.app <<<`); the rest of that dump (same
/// crash_dump pid, tag `DEBUG`) is kept with it.
pub fn select<'a>(
    records: &'a [Record],
    app_id: &str,
    processes: &Processes,
) -> Vec<(&'static str, &'a Record)> {
    let mut dumpers: BTreeSet<u32> = BTreeSet::new();
    let mut out = Vec::new();
    for record in records {
        let source = match classify(record, app_id, processes) {
            Some("crash") => {
                let _ = dumpers.insert(record.pid);
                Some("crash")
            }
            None if record.tag == "DEBUG" && dumpers.contains(&record.pid) => Some("crash"),
            other => other,
        };
        if let Some(source) = source {
            out.push((source, record));
        }
    }
    out
}

/// An `ICM_EVENT` record's JSON.
pub fn event(record: &Record) -> Option<Value> {
    if record.tag != "ICM_EVENT" {
        return None;
    }
    let value: Value = serde_json::from_str(record.msg.trim()).ok()?;
    (value.get("v").and_then(Value::as_u64) == Some(1)).then_some(value)
}

/// The `kind` of an event.
pub fn kind(event: &Value) -> &str {
    event.get("kind").and_then(Value::as_str).unwrap_or("")
}

/// A panic message from the records the app's processes wrote: an
/// `ICM_EVENT` panic, or a `panicked at` line from the logger or
/// redirected stderr.
pub fn panic_of(records: &[Record], processes: &Processes) -> Option<(String, Option<String>)> {
    for record in records.iter().filter(|record| processes.owns(record)) {
        if let Some(event) = event(record)
            && kind(&event) == "panic"
        {
            let message = event
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let location = event
                .get("location")
                .and_then(Value::as_str)
                .map(str::to_string);
            return Some((message, location));
        }
    }
    records
        .iter()
        .filter(|r| processes.owns(r))
        .find(|r| r.msg.contains("panicked at"))
        .map(|r| {
            let location = r.msg.split("panicked at ").nth(1).map(|rest| {
                rest.split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_end_matches([':', ','])
                    .to_string()
            });
            (r.msg.clone(), location)
        })
}

/// The events-buffer tags Android writes when it relaunches an activity
/// (destroys it and creates it again): `wm_*` from API 29, `am_*` before.
pub const RELAUNCH_TAGS: &[&str] = &[
    "wm_relaunch_resume_activity",
    "wm_relaunch_activity",
    "am_relaunch_resume_activity",
    "am_relaunch_activity",
];

/// One relaunch of the app's activity, from the events buffer.
#[derive(Clone, Debug, PartialEq)]
pub struct Relaunch {
    /// The record (`wm_relaunch_resume_activity: [0,175822296,8,<component>,80000000]`).
    pub record: Record,
    /// The activity (`com.example.app/android.app.NativeActivity`).
    pub component: String,
    /// The configuration changes that caused it (`ActivityInfo.CONFIG_*`
    /// bits); the `am_*` events of older releases carry none.
    pub mask: Option<u32>,
}

/// The relaunches of `app_id`'s activities in events-buffer records.
pub fn relaunches(records: &[Record], app_id: &str) -> Vec<Relaunch> {
    let prefix = format!("{app_id}/");
    records
        .iter()
        .filter(|record| RELAUNCH_TAGS.contains(&record.tag.as_str()))
        .filter_map(|record| {
            let fields: Vec<&str> = record
                .msg
                .trim()
                .trim_start_matches('[')
                .trim_end_matches(']')
                .split(',')
                .map(str::trim)
                .collect();
            let at = fields.iter().position(|f| f.starts_with(&prefix))?;
            Some(Relaunch {
                record: record.clone(),
                component: fields[at].to_string(),
                mask: fields
                    .get(at + 1)
                    .and_then(|mask| u32::from_str_radix(mask, 16).ok()),
            })
        })
        .collect()
}

/// The events-buffer tags Android writes when it destroys an activity
/// (`wm_*` from API 29, `am_*` before).
pub const DESTROY_TAGS: &[&str] = &["wm_destroy_activity", "am_destroy_activity"];

/// The records that destroy one of `app_id`'s activities
/// (`wm_destroy_activity: [0,123,8,<component>,finish-imm]`), with the
/// reason Android gave (the last field).
pub fn destroys(records: &[Record], app_id: &str) -> Vec<(Record, String)> {
    let prefix = format!("{app_id}/");
    records
        .iter()
        .filter(|record| DESTROY_TAGS.contains(&record.tag.as_str()))
        .filter_map(|record| {
            let fields: Vec<&str> = record
                .msg
                .trim()
                .trim_start_matches('[')
                .trim_end_matches(']')
                .split(',')
                .map(str::trim)
                .collect();
            fields.iter().any(|f| f.starts_with(&prefix)).then(|| {
                (
                    record.clone(),
                    fields.last().map(ToString::to_string).unwrap_or_default(),
                )
            })
        })
        .collect()
}

/// Known failure signatures in the app's logs (design §13.4), as
/// `likely_causes`.
pub fn likely_causes(records: &[Record], lib: &str) -> Vec<String> {
    let mut causes = Vec::new();
    let mut add = |cause: String| {
        if !causes.contains(&cause) {
            causes.push(cause);
        }
    };
    for record in records {
        let msg = &record.msg;
        if msg.contains("dlopen failed") && msg.contains("not found") {
            add(format!(
                "Android could not load lib{lib}.so: `android.app.lib_name` must name the library (android.manifest.lib_name)"
            ));
        }
        if msg.contains("Unable to find native library")
            || msg.contains("android_main") && msg.contains("undefined")
        {
            add(
                "the library has no android_main: keep `iced::android_main!(run);` in src/lib.rs"
                    .to_string(),
            );
        }
        if msg.contains("an event loop is already running in this process") {
            add("a second activity started while another one still ran the app (a launch right after Back, or a start into another task); launch it again".to_string());
        } else if msg.contains("Call set_android_app")
            || msg.contains("RecreationAttempt")
            || msg.contains("ran twice")
        {
            add("duplicate iced copies (deps.single_iced), or Activity recreation on an iced_mobile from before the Android lifecycle fix (update the pin); never call iced::exit".to_string());
        }
        if msg.starts_with("ANR in") {
            add("the main thread is blocked (ANR)".to_string());
        }
        if msg.contains("Failed to find an appropriate adapter")
            || msg.contains("surface") && msg.contains("Error") && record.priority == 'E'
        {
            add("GPU: retry with `--env ICED_BACKEND=tiny-skia` (sets the sysprop debug.iced.backend)".to_string());
        }
        if msg.contains("No Unix display server backend") {
            add("the framework floor is not met: update the iced_mobile pin".to_string());
        }
        if msg.contains("Fatal signal") {
            add(format!("a native crash: {}", msg.trim()));
        }
    }
    causes
}

/// The `--level` filter.
pub fn at_least(record: &Record, level: Option<Level>) -> bool {
    level.is_none_or(|level| record.level_enum() >= level)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "--------- beginning of main
1728245000.101  4321  4321 I ICM_EVENT: {\"v\":1,\"kind\":\"start\",\"protocol\":1,\"pid\":4321,\"platform\":\"android\"}
1728245000.812  4321  4350 I ICM_EVENT: {\"v\":1,\"kind\":\"ready\",\"ms\":711,\"window\":{\"size\":[411,914],\"physical\":[1080,2400],\"scale\":2.625},\"backend\":\"wgpu\"}
1728245000.900  1111  1111 I ActivityManager: Displayed com.example.app/android.app.NativeActivity: +812ms
1728245000.950  2222  2222 I chatty: unrelated
1728245001.000  4321  4400 W iced: font fallback
1728245001.100  4321  4400 E RustStdoutStderr: thread 'main' panicked at src/lib.rs:41:9:
";

    #[test]
    fn parses_threadtime_epoch_lines() {
        let records = parse(SAMPLE);
        assert_eq!(records.len(), 6);
        let first = &records[0];
        assert_eq!(first.ts, "1728245000.101");
        assert_eq!(first.pid, 4321);
        assert_eq!(first.tid, 4321);
        assert_eq!(first.priority, 'I');
        assert_eq!(first.tag, "ICM_EVENT");
        assert_eq!(kind(&event(first).unwrap()), "start");
        assert_eq!(records[4].level(), "warn");
        assert!(parse_line("garbage").is_none());
        assert!(parse_line("").is_none());
        let json = first.to_json("app");
        assert_eq!(json["platform"], "android");
        assert_eq!(json["level"], "info");
        assert!(json["ts"].as_str().unwrap().starts_with("2024-10-06T"));
    }

    #[test]
    fn classifies_the_apps_records() {
        let records = parse(SAMPLE);
        let processes = Processes::of([4321]);
        let sources: Vec<Option<&str>> = records
            .iter()
            .map(|r| classify(r, "com.example.app", &processes))
            .collect();
        assert_eq!(
            sources,
            vec![
                Some("app"),
                Some("app"),
                Some("system"),
                None,
                Some("app"),
                Some("app")
            ]
        );
    }

    /// Another iced_mobile app on the device writes the same tags (the
    /// `ICM_EVENT` opt-in is a system property): its lines are not the
    /// app's, and neither is anything when no pid of the app is known.
    #[test]
    fn another_apps_tags_are_not_the_apps() {
        let other = "1728245000.300  7777  7777 I ICM_EVENT: {\"v\":1,\"kind\":\"ready\",\"ms\":90}
1728245000.310  7777  7790 I iced: another app's logger
1728245000.320  7777  7791 E RustStdoutStderr: thread 'main' panicked at other/src/lib.rs:1:1:
1728245000.330  7777  7777 I ICM_EVENT: {\"v\":1,\"kind\":\"panic\",\"message\":\"not ours\",\"location\":\"other/src/lib.rs:1:1\"}
";
        let records = parse(&format!("{SAMPLE}{other}"));
        let processes = Processes::of([4321]);
        let selected = select(&records, "com.example.app", &processes);
        assert!(selected.iter().all(|(_, record)| record.pid != 7777));
        let (message, _) = panic_of(&records, &processes).unwrap();
        assert!(message.contains("src/lib.rs:41:9"), "{message}");

        let theirs = parse(other);
        assert!(panic_of(&theirs, &processes).is_none());
        let nobody = Processes::default();
        assert!(
            select(&records, "com.example.app", &nobody)
                .iter()
                .all(|(source, _)| *source != "app")
        );
    }

    /// `am_proc_start` and `am_proc_died` place each pid in time: a pid the
    /// app had before counts only while the app had it.
    #[test]
    fn processes_place_pids_in_time() {
        let events = parse(
            "100.000   600   610 I am_proc_start: [0,4321,10123,com.example.app,next-top-activity,{com.example.app/android.app.NativeActivity}]
200.000   600   610 I am_proc_died: [0,4321,com.example.app,900,19]
300.000   600   610 I am_proc_start: [0,4321,10200,com.other.app,activity,{com.other.app/.Main}]
400.000   600   610 I am_proc_start: [0,5000,10123,com.example.app:remote,service,{com.example.app/.Sync}]
450.000   600   610 I am_proc_start: [0,6000,10124,com.example.application,activity,{com.example.application/.Main}]
500.000   600   610 I am_proc_died: [0,5555,com.example.app,900,19]
600.000   600   610 I am_proc_died: [0,6666,com.other.app]
",
        );
        let processes = Processes::read("com.example.app", vec![7000], &events).knowing([8000]);
        let owned = |pid, at| processes.owned(pid, at);
        // The app's process, from its start (and the second before it,
        // while Android logs the start) to its death.
        assert!(owned(4321, 150.0));
        assert!(owned(4321, 99.5));
        assert!(!owned(4321, 98.0));
        assert!(!owned(4321, 250.0));
        // Then another app's process with the same pid.
        assert!(!owned(4321, 350.0));
        // The app's other processes, but not an app whose id starts alike.
        assert!(owned(5000, 401.0));
        assert!(!owned(6000, 451.0));
        // A process the buffer only saw die had the pid until then.
        assert!(owned(5555, 450.0));
        assert!(!owned(5555, 550.0));
        assert!(!owned(6666, 550.0));
        // Pids the buffer says nothing about: `pidof`'s and those seen
        // before are the app's, others not.
        assert!(owned(7000, 650.0));
        assert!(owned(8000, 650.0));
        assert!(!owned(9000, 650.0));
        assert_eq!(processes.now, vec![7000]);
        assert_eq!(
            processes.pids(),
            [4321, 5000, 5555, 7000, 8000].into_iter().collect()
        );
    }

    #[test]
    fn finds_panics_and_causes() {
        let records = parse(SAMPLE);
        let processes = Processes::of([4321]);
        let (message, location) = panic_of(&records, &processes).unwrap();
        assert!(message.contains("panicked at src/lib.rs:41:9"));
        assert_eq!(location.as_deref(), Some("src/lib.rs:41:9"));

        let event = "1.0 4321 1 I ICM_EVENT: {\"v\":1,\"kind\":\"panic\",\"message\":\"boom\",\"location\":\"src/lib.rs:3:5\",\"thread\":\"main\"}";
        let (message, location) = panic_of(&parse(event), &processes).unwrap();
        assert_eq!(message, "boom");
        assert_eq!(location.as_deref(), Some("src/lib.rs:3:5"));

        let dlopen = parse(
            "1.0 9 9 E AndroidRuntime: java.lang.IllegalArgumentException: Unable to load native library: dlopen failed: library \"libapp.so\" not found",
        );
        assert!(likely_causes(&dlopen, "app")[0].contains("lib_name"));
    }

    #[test]
    fn finds_relaunches_of_the_app() {
        // `logcat -b events -v threadtime,epoch` on a fresh android-36
        // emulator while SystemUI applied its theme overlays.
        let events = parse(
            "1791342497.682   660   683 I wm_relaunch_resume_activity: [0,175822296,8,com.example.demo/android.app.NativeActivity,80000000]
1791342497.688   660  1033 I wm_relaunch_activity: [0,161146439,6,com.google.android.apps.nexuslauncher/.NexusLauncherActivity,80000000]
1791342497.766  2634  2634 I wm_on_stop_called: [175822296,android.app.NativeActivity,handleRelaunchActivity,0]
1791342400.000   500   510 I am_relaunch_activity: [0,123,9,com.example.demo/android.app.NativeActivity]
",
        );
        let found = relaunches(&events, "com.example.demo");
        assert_eq!(found.len(), 2);
        assert_eq!(
            found[0].component,
            "com.example.demo/android.app.NativeActivity"
        );
        assert_eq!(found[0].mask, Some(0x8000_0000));
        assert_eq!(found[0].record.tag, "wm_relaunch_resume_activity");
        assert_eq!(found[1].mask, None);
        assert!(relaunches(&events, "com.example.demo2").is_empty());
        assert!(relaunches(&events, "com.example").is_empty());
    }

    #[test]
    fn levels_filter() {
        let records = parse(SAMPLE);
        assert!(at_least(&records[4], Some(Level::Warn)));
        assert!(!at_least(&records[0], Some(Level::Warn)));
        assert!(at_least(&records[0], None));
    }
}
