//! `icm test --on android --lifecycle` (`icm test android --lifecycle`;
//! design §13.5, phase 3): the built-in lifecycle suite. It launches the app
//! as `icm run android` does, then puts it through what Android does to
//! apps, one step at a time, each a `test.lifecycle` check:
//!
//! | Step | Action | The app must |
//! |---|---|---|
//! | `dark-mode`, `light-mode` | `cmd uimode night yes`, `no` | keep its process and activity, and draw |
//! | `landscape`, `portrait` | `user_rotation 1`, `0` (auto-rotate off) | the same |
//! | `font-scale` | `font_scale 1.3` | the same |
//! | `font-weight` | `font_weight_adjustment 300` (API 31+) | the same |
//! | `home` | HOME | stay alive behind the launcher |
//! | `home-relaunch` | `am start` | come back in the same process without starting over, and draw |
//! | `back` | BACK | with `[android] back = "system"`: end with its activity (or go behind); with `"key"`: stay |
//! | `back-relaunch` | `am start` | start over in its process (`ICM_EVENT start`, `ready`) and draw |
//! | `kill` | HOME, then `am kill` (Android freeing memory) | be gone |
//! | `kill-relaunch` | `am start` | start in a new process and draw |
//!
//! A step fails on a relaunch or destruction of the activity where none is
//! due (`wm_relaunch_*`, `wm_destroy_activity`, `am_*` before API 29), an
//! `ANR in` the app, a panic or crash, a changed process where it must
//! stay, a new `ICM_EVENT start` where the app must carry on, no `ready`
//! where it must start over, or a blank screenshot (the frame was lost;
//! FLAG_SECURE windows are exempt). `landscape` and `portrait` are SKIP
//! for an app that `[app] orientations` locks to one axis when its frame
//! stays on that axis: Android does not turn it, so they test nothing. The
//! device's settings are restored at the end, whatever happened. The project's `[checks] android` scripts
//! run after every step that leaves the app in front, with
//! `ICM_LIFECYCLE_STEP` naming the step.

use super::adb::Adb;
use super::logcat;
use super::manifest::{ACTIVITY, Axis};
use super::pipeline::{self, Launched, Presence};
use crate::catalogue::CheckId;
use crate::cli::{Platform, RunArgs, TestArgs};
use crate::context::Ctx;
use crate::error::{Check, Evidence, IcmError, Result, Status};
use crate::plan::{Plan, Step};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How long a step lets Android settle after its action.
const SETTLE: Duration = Duration::from_millis(2500);

/// How long an app that starts over may take to draw again.
const REDRAW: Duration = Duration::from_secs(30);

/// What a step expects of the app once Android is done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expect {
    /// In front, in the same process and activity, carrying on (no new
    /// `ICM_EVENT start`).
    Kept,
    /// Alive, behind another activity (Home).
    Behind,
    /// Back with `[android] back = "system"`: the app ends with its
    /// activity, which Android destroys (or moves behind the launcher).
    Ended,
    /// After `am kill`: no process.
    Killed,
    /// In front again after starting over: a new `start` and `ready`, in
    /// a new process when `new_process`.
    StartedOver { new_process: bool },
}

/// One step of the suite.
#[derive(Clone, Debug)]
struct Planned {
    name: &'static str,
    what: &'static str,
    line: String,
    expect: Expect,
}

/// The device settings the suite changes, as they were.
#[derive(Clone, Debug, Default)]
struct Settings {
    night: Option<String>,
    accelerometer_rotation: Option<String>,
    user_rotation: Option<String>,
    font_scale: Option<String>,
    font_weight: Option<String>,
}

impl Settings {
    fn read(adb: &Adb, api: u32) -> Settings {
        let get = |line: &str| adb.shell_text(line, Duration::from_secs(15));
        Settings {
            // "Night mode: no" (or yes, auto, custom...).
            night: get("cmd uimode night").and_then(|text| {
                text.rsplit(':')
                    .next()
                    .map(|mode| mode.trim().to_string())
                    .filter(|mode| ["yes", "no", "auto"].contains(&mode.as_str()))
            }),
            accelerometer_rotation: get("settings get system accelerometer_rotation"),
            user_rotation: get("settings get system user_rotation"),
            font_scale: get("settings get system font_scale"),
            font_weight: (api >= 31)
                .then(|| get("settings get secure font_weight_adjustment"))
                .flatten(),
        }
    }

    /// The shell line that puts them back (`null`: the setting was unset).
    fn restore_line(&self) -> String {
        let mut lines = Vec::new();
        if let Some(night) = &self.night {
            lines.push(format!("cmd uimode night {night}"));
        }
        let put = |namespace: &str, key: &str, value: &Option<String>| match value.as_deref() {
            None => None,
            Some("null") | Some("") => Some(format!("settings delete {namespace} {key}")),
            Some(value) => Some(format!(
                "settings put {namespace} {key} {}",
                super::adb::quote(value)
            )),
        };
        lines.extend(put("system", "user_rotation", &self.user_rotation));
        lines.extend(put(
            "system",
            "accelerometer_rotation",
            &self.accelerometer_rotation,
        ));
        lines.extend(put("system", "font_scale", &self.font_scale));
        lines.extend(put("secure", "font_weight_adjustment", &self.font_weight));
        // `settings delete` prints "Deleted 0 rows" when there was nothing.
        lines.join(" ; ")
    }
}

/// The steps, for an app whose Back is `back` on a device at `api`.
fn steps(app_id: &str, back: &str, api: u32) -> Vec<Planned> {
    let component = format!("{app_id}/{ACTIVITY}");
    let relaunch = format!("am start -W -n {}", super::adb::quote(&component));
    let mut steps = vec![
        Planned {
            name: "dark-mode",
            what: "dark mode on (uiMode night)",
            line: "cmd uimode night yes".into(),
            expect: Expect::Kept,
        },
        Planned {
            name: "light-mode",
            what: "dark mode off",
            line: "cmd uimode night no".into(),
            expect: Expect::Kept,
        },
        Planned {
            name: "landscape",
            what: "rotation to landscape (auto-rotate off, user_rotation 1)",
            line:
                "settings put system accelerometer_rotation 0 ; settings put system user_rotation 1"
                    .into(),
            expect: Expect::Kept,
        },
        Planned {
            name: "portrait",
            what: "rotation back to portrait (user_rotation 0)",
            line: "settings put system user_rotation 0".into(),
            expect: Expect::Kept,
        },
        Planned {
            name: "font-scale",
            what: "font scale 1.3",
            line: "settings put system font_scale 1.3".into(),
            expect: Expect::Kept,
        },
    ];
    if api >= 31 {
        steps.push(Planned {
            name: "font-weight",
            what: "bold text (font_weight_adjustment 300)",
            line: "settings put secure font_weight_adjustment 300".into(),
            expect: Expect::Kept,
        });
    }
    steps.extend([
        Planned {
            name: "home",
            what: "Home",
            line: "input keyevent KEYCODE_HOME".into(),
            expect: Expect::Behind,
        },
        Planned {
            name: "home-relaunch",
            what: "relaunch from the launcher after Home",
            line: relaunch.clone(),
            expect: Expect::Kept,
        },
        Planned {
            name: "back",
            what: "Back at the app's root",
            line: "input keyevent KEYCODE_BACK".into(),
            expect: if back == "key" {
                Expect::Kept
            } else {
                Expect::Ended
            },
        },
        Planned {
            name: "back-relaunch",
            what: "relaunch after Back",
            line: relaunch.clone(),
            expect: if back == "key" {
                Expect::Kept
            } else {
                Expect::StartedOver { new_process: false }
            },
        },
        Planned {
            name: "kill",
            what: "Home, then Android kills the process to free memory (am kill)",
            line: format!(
                "input keyevent KEYCODE_HOME ; sleep 2 ; am kill {}",
                super::adb::quote(app_id)
            ),
            expect: Expect::Killed,
        },
        Planned {
            name: "kill-relaunch",
            what: "relaunch after the kill",
            line: relaunch,
            expect: Expect::StartedOver { new_process: true },
        },
    ]);
    steps
}

/// The run arguments the suite launches with (`icm run android`'s
/// defaults).
fn run_args() -> RunArgs {
    RunArgs {
        platform: Platform::Android,
        release: false,
        no_build: false,
        device: None,
        sim: None,
        avd: None,
        fresh: false,
        runtime: None,
        show: false,
        env: Vec::new(),
        wait_ready: Duration::from_secs(30),
        settle: Duration::from_millis(1500),
        no_shot: false,
        expect_content: false,
        reinstall: false,
        wipe_data: false,
        viewport: None,
        port: 8787,
        attach: false,
        from_aab: false,
        store: false,
    }
}

/// `icm test --on android --lifecycle`.
pub fn run(ctx: &mut Ctx, _args: &TestArgs) -> Result<()> {
    if ctx.dry_run() {
        return plan(ctx);
    }
    let launched = pipeline::launch_app(ctx, &run_args())?;
    let ctx: &Ctx = ctx;
    let mut suite = Suite::new(ctx, launched);
    let outcome = suite.run();
    suite.finish(outcome)
}

/// `--dry-run`: what the suite would do (it touches no device).
fn plan(ctx: &mut Ctx) -> Result<()> {
    let project = ctx.project()?.clone();
    let config = &project.config.config;
    let mut plan = Plan::new();
    plan.push(Step::internal(
        "android.launch",
        "as `icm run android`: choose (or boot) the device, build and install the dev APK, launch, wait for ICM_EVENT ready, screenshot",
    ));
    plan.push(Step::internal(
        "lifecycle.settings",
        "read uiMode night, accelerometer_rotation, user_rotation, font_scale and font_weight_adjustment, to restore them at the end",
    ));
    for step in steps(
        &config.app.id,
        &config.android.back,
        config.android.target_sdk,
    ) {
        plan.push(
            Step::internal(
                &format!("lifecycle.{}", step.name),
                &format!(
                    "{}: adb shell {}; then the app must {}",
                    step.what,
                    step.line,
                    expectation(step.expect)
                ),
            )
            .gate(CheckId::TestLifecycle),
        );
    }
    plan.push(Step::internal(
        "lifecycle.restore",
        "put the device's settings back as they were",
    ));
    plan.report(ctx);
    ctx.rep.summary(format!(
        "the plan of the Android lifecycle suite for {} (nothing was run)",
        config.app.id
    ));
    Ok(())
}

fn expectation(expect: Expect) -> &'static str {
    match expect {
        Expect::Kept => "keep its process and activity, carry on (no new ICM_EVENT start) and draw",
        Expect::Behind => "stay alive behind the launcher, its activity not destroyed",
        Expect::Ended => {
            "end with its activity (ICM_EVENT exit destroyed) or go behind, without a crash"
        }
        Expect::Killed => "be gone",
        Expect::StartedOver { new_process: false } => {
            "start over in its process (ICM_EVENT start, ready) and draw"
        }
        Expect::StartedOver { new_process: true } => {
            "start in a new process (ICM_EVENT start, ready) and draw"
        }
    }
}

/// The suite in progress.
struct Suite<'a> {
    ctx: &'a Ctx,
    launched: Launched,
    /// The app's process now.
    pid: Option<u32>,
    /// The app sent `ICM_EVENT start` at launch: its events are evidence.
    speaks: bool,
    /// The device's API level.
    api: u32,
    settings: Settings,
    /// The axis of the app's last screenshot.
    axis: Option<Axis>,
    results: Vec<Value>,
    hooks: Vec<Value>,
}

/// What one step found.
struct Found {
    status: Status,
    detail: String,
    evidence: Vec<Evidence>,
    causes: Vec<String>,
    screenshot: Option<PathBuf>,
    /// The axis of the screenshot.
    axis: Option<Axis>,
    /// Where the app is now.
    presence: Presence,
}

impl<'a> Suite<'a> {
    fn new(ctx: &'a Ctx, launched: Launched) -> Suite<'a> {
        let api = launched
            .adb
            .getprop("ro.build.version.sdk")
            .and_then(|text| text.trim().parse().ok())
            .unwrap_or(0);
        let processes = pipeline::processes(&launched.adb, &launched.app_id, &launched.pids);
        let speaks = pipeline::events_since(&launched.adb, &launched.mark)
            .iter()
            .filter(|record| processes.owns(record))
            .filter_map(logcat::event)
            .any(|event| logcat::kind(&event) == "start");
        let settings = Settings::read(&launched.adb, api);
        Suite {
            ctx,
            pid: launched.session.pid,
            speaks,
            api,
            settings,
            axis: None,
            results: Vec::new(),
            hooks: Vec::new(),
            launched,
        }
    }

    fn adb(&self) -> &Adb {
        &self.launched.adb
    }

    fn app_id(&self) -> &str {
        &self.launched.app_id
    }

    fn run(&mut self) -> Result<()> {
        let config = &self.launched.project.config.config;
        let back = config.android.back.clone();
        let app_id = self.app_id().to_string();
        let launch = format!(
            "launched on {} (pid {}){}",
            self.launched.device_name,
            self.pid
                .map_or("unknown".to_string(), |pid| pid.to_string()),
            if self.speaks {
                ""
            } else {
                "; the app sends no ICM_EVENT, so starts and frames are judged by probes and screenshots"
            }
        );
        self.record(
            "launch",
            Status::Pass,
            &launch,
            None,
            Vec::new(),
            Vec::new(),
        );

        let mut plan = steps(&app_id, &back, self.api);
        let mut index = 0;
        while index < plan.len() {
            let step = plan[index].clone();
            let found = self.step(&step)?;
            // What the next relaunch expects depends on what Back and the
            // kill did.
            if step.name == "back"
                && back != "key"
                && let Some(next) = plan.get_mut(index + 1)
                && matches!(found.presence, Presence::Behind { .. })
            {
                // Android moved the task behind instead of destroying the
                // activity: the relaunch brings it back as it was.
                next.expect = Expect::Kept;
            }
            if step.name == "kill"
                && found.status == Status::Skip
                && let Some(next) = plan.get_mut(index + 1)
            {
                next.expect = Expect::Kept;
            }
            index += 1;
        }
        Ok(())
    }

    /// Runs one step and reports it.
    fn step(&mut self, step: &Planned) -> Result<Found> {
        let ctx = self.ctx;
        let adb = self.adb().clone();
        let app_id = self.app_id().to_string();
        let mark = adb.epoch().ok_or_else(|| {
            IcmError::new(
                CheckId::AndroidDeviceNone,
                format!("{} does not answer `date`", adb.serial),
            )
        })?;
        let before = self.pid;
        ctx.rep.progress(format!("lifecycle: {}", step.what));
        let started = Instant::now();
        let outcome = ctx.step(
            &format!("lifecycle.{}", step.name),
            &adb.shell(&step.line).timeout(Duration::from_secs(60)),
        )?;
        let text = format!("{}{}", outcome.stdout_text(), outcome.stderr_text());
        if !outcome.success() || text.contains("Exception") || text.contains("Error:") {
            let mut evidence = Vec::new();
            if let Some(log) = &outcome.log {
                evidence.push(Evidence::file(log));
            }
            let detail = format!("{}: `{}` failed: {}", step.what, step.line, text.trim());
            self.record(step.name, Status::Fail, &detail, None, evidence, Vec::new());
            return Ok(Found {
                status: Status::Fail,
                detail,
                evidence: Vec::new(),
                causes: Vec::new(),
                screenshot: None,
                axis: None,
                presence: pipeline::presence(&adb, &app_id),
            });
        }

        // Let Android finish: a fixed settle, or until the app's process is
        // gone or the app has drawn again.
        let mut ready_seen = None;
        match step.expect {
            Expect::Killed => {
                let until = Instant::now() + Duration::from_secs(8);
                while !adb.pids(&app_id).is_empty() && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
            Expect::StartedOver { .. } if self.speaks => {
                ready_seen = Some(pipeline::wait_ready(
                    ctx,
                    &adb,
                    &app_id,
                    &mark,
                    Instant::now(),
                    REDRAW,
                ));
                std::thread::sleep(Duration::from_millis(1500));
            }
            _ => std::thread::sleep(SETTLE),
        }

        let mut found = self.judge(step, &mark, before, ready_seen)?;
        // A rotation that cannot turn an app locked to one axis tests
        // nothing: the frame stays on that axis.
        let orientations = &self.launched.project.config.config.app.orientations;
        if matches!(step.name, "landscape" | "portrait")
            && found.status == Status::Pass
            && let Some(locked) = super::manifest::locked_axis(orientations)
            && found.axis.is_some()
            && found.axis == self.axis
        {
            found.status = Status::Skip;
            found.detail = format!(
                "no rotation was tested: {}; {}",
                pipeline::orientation_locked(
                    &app_id,
                    orientations,
                    locked,
                    "the device turned, but Android kept the app",
                )
                .error
                .detail,
                found.detail
            );
        }
        self.axis = found.axis.or(self.axis);
        found.detail = format!(
            "{}: {} ({:.1} s)",
            step.what,
            found.detail,
            started.elapsed().as_secs_f64()
        );
        if found.status == Status::Fail {
            // The logs since the step, for the evidence and likely causes.
            let pids: BTreeSet<u32> = before.into_iter().chain(adb.pids(&app_id)).collect();
            let dir = self.launched.dir.join(format!("lifecycle-{}", step.name));
            let _ = std::fs::create_dir_all(&dir);
            let logs = pipeline::collect_logs(ctx, &adb, &dir, &app_id, &mark, &pids);
            let mut error = IcmError::new(CheckId::TestLifecycle, found.detail.clone());
            pipeline::attach_evidence(&mut error, &logs, &self.launched.project);
            for evidence in error.evidence {
                if !found.evidence.contains(&evidence) {
                    found.evidence.push(evidence);
                }
            }
            for cause in error.likely_causes {
                if !found.causes.contains(&cause) {
                    found.causes.push(cause);
                }
            }
        }
        // The last process seen (a relaunch after the kill compares with
        // it), recorded with the step.
        if let Presence::Front { pid } | Presence::Behind { pid } | Presence::NoActivity { pid } =
            found.presence
        {
            self.pid = Some(pid);
        }
        self.record(
            step.name,
            found.status,
            &found.detail,
            found.screenshot.clone(),
            found.evidence.clone(),
            found.causes.clone(),
        );
        if matches!(found.presence, Presence::Front { .. }) {
            self.hooks(step.name)?;
        }
        if found.status == Status::Fail
            && !matches!(step.expect, Expect::Behind | Expect::Ended | Expect::Killed)
            && !matches!(found.presence, Presence::Front { .. })
        {
            self.recover()?;
        }
        Ok(found)
    }

    /// Judges a step from the device's state, its event log, logcat and a
    /// screenshot.
    fn judge(
        &mut self,
        step: &Planned,
        mark: &str,
        before: Option<u32>,
        ready_seen: Option<Result<pipeline::Ready>>,
    ) -> Result<Found> {
        let ctx = self.ctx;
        let adb = self.adb().clone();
        let app_id = self.app_id().to_string();
        let dir = self.launched.dir.clone();
        let mut problems: Vec<String> = Vec::new();
        let mut notes: Vec<String> = Vec::new();
        let mut evidence: Vec<Evidence> = Vec::new();
        let mut causes: Vec<String> = Vec::new();

        let presence = pipeline::presence(&adb, &app_id);
        let now = match presence {
            Presence::Gone => None,
            Presence::NoActivity { pid } | Presence::Behind { pid } | Presence::Front { pid } => {
                Some(pid)
            }
        };

        // The activity: relaunched or destroyed since the step's mark.
        let events_path = dir.join(format!("events-{}.txt", step.name));
        let events = pipeline::events_buffer(&adb, mark, &[]).unwrap_or_default();
        let _ = crate::process::write_redacted(&events_path, &events);
        let records = logcat::parse(&events);
        let relaunched = logcat::relaunches(&records, &app_id);
        let destroyed = logcat::destroys(&records, &app_id);
        let line_of = |record: &logcat::Record| {
            let line = events
                .lines()
                .position(|line| line.contains(&record.ts) && line.contains(&record.tag))
                .map_or(1, |index| index as u32 + 1);
            Evidence::line(&events_path, line, record.line())
        };
        if let Some(first) = relaunched.first() {
            let names = first
                .mask
                .map(super::manifest::config_names)
                .unwrap_or_default();
            problems.push(format!(
                "Android relaunched the activity ({})",
                if names.is_empty() {
                    "no change named".to_string()
                } else {
                    names.join("|")
                }
            ));
            evidence.push(line_of(&first.record));
            causes.push("the change is missing from android:configChanges (`icm verify android` checks the list), or an older APK is installed".to_string());
        }
        let destroy_due = matches!(step.expect, Expect::Ended | Expect::Killed);
        if let Some((record, reason)) = destroyed.first()
            && !destroy_due
            && relaunched.is_empty()
        {
            problems.push(format!("Android destroyed the activity ({reason})"));
            evidence.push(line_of(record));
        }

        // The app's own events (not another iced_mobile app's) and its
        // logs: panics, crashes, ANRs.
        let icm_records = pipeline::events_since(&adb, mark);
        let processes = pipeline::processes(&adb, &app_id, &before.into_iter().collect());
        let icm_events: Vec<(logcat::Record, Value)> = icm_records
            .into_iter()
            .filter(|record| processes.owns(record))
            .filter_map(|record| logcat::event(&record).map(|event| (record, event)))
            .collect();
        let starts = icm_events
            .iter()
            .filter(|(_, event)| logcat::kind(event) == "start")
            .count();
        if let Some((_, event)) = icm_events
            .iter()
            .find(|(_, event)| logcat::kind(event) == "panic")
        {
            problems.push(format!(
                "the app panicked at {}: {}",
                event.get("location").and_then(Value::as_str).unwrap_or("?"),
                event.get("message").and_then(Value::as_str).unwrap_or("")
            ));
        }
        if let Some(text) = pipeline::query(&adb, mark, &["main", "system", "crash"]) {
            let anr = format!("ANR in {app_id}");
            if let Some(line) = text.lines().find(|line| line.contains(&anr)) {
                problems.push(format!("Android reported an ANR: {}", line.trim()));
            }
            let crashed = text.lines().find(|line| {
                (line.contains("FATAL EXCEPTION") || line.contains("Fatal signal"))
                    && before.is_some_and(|pid| line.contains(&format!(" {pid} ")))
            });
            if let Some(line) = crashed {
                problems.push(format!("the app crashed: {}", line.trim()));
            }
        }

        // What the step expects.
        let mut skipped = false;
        let mut front = false;
        match step.expect {
            Expect::Kept => {
                match presence {
                    Presence::Front { .. } => front = true,
                    other => problems.push(format!("the app is not in front afterwards ({})", describe(other))),
                }
                if before.is_some() && now != before {
                    problems.push(format!(
                        "its process changed ({} → {})",
                        show(before),
                        show(now)
                    ));
                }
                if self.speaks && starts > 0 {
                    problems.push("the app started over (a new ICM_EVENT start): it lost what it kept in memory".to_string());
                }
            }
            Expect::Behind => match presence {
                Presence::Behind { pid } if Some(pid) == before || before.is_none() => {
                    notes.push(format!("pid {pid} waits behind the launcher"));
                }
                other => problems.push(format!(
                    "the app should wait behind the launcher in pid {}, but {}",
                    show(before),
                    describe(other)
                )),
            },
            Expect::Ended => match presence {
                Presence::NoActivity { pid } => notes.push(format!(
                    "the app ended with its activity, which Android destroyed; pid {pid} lives on, cached"
                )),
                Presence::Gone => notes.push("the app ended with its activity and its process".to_string()),
                Presence::Behind { pid } => notes.push(format!(
                    "Android moved the app behind the launcher (pid {pid}) instead of destroying its activity"
                )),
                Presence::Front { .. } => problems.push(
                    "the app is still in front: Back at its root should close it ([android] back = \"system\")".to_string(),
                ),
            },
            Expect::Killed => match presence {
                Presence::Gone => notes.push(format!("pid {} is gone", show(before))),
                other => {
                    skipped = true;
                    notes.push(format!(
                        "Android kept the process ({}): `am kill` only kills a process it may reclaim",
                        describe(other)
                    ));
                }
            },
            Expect::StartedOver { new_process } => {
                match presence {
                    Presence::Front { .. } => front = true,
                    other => problems.push(format!("the app is not in front afterwards ({})", describe(other))),
                }
                if new_process && now.is_some() && now == before {
                    problems.push(format!("the process is still {} after the kill", show(now)));
                } else if now.is_some() && now != before {
                    notes.push(format!("a new process (pid {} → {})", show(before), show(now)));
                } else if now.is_some() {
                    notes.push(format!("in pid {}", show(now)));
                }
                match ready_seen {
                    Some(Ok(ready)) => notes.push(format!(
                        "it started over and drew after {} ms (source: {})",
                        ready.ms.unwrap_or(0),
                        ready.source
                    )),
                    Some(Err(error)) => problems.push(format!("it did not draw again: {}", error.detail)),
                    None => {}
                }
            }
        }

        // The frame.
        let mut screenshot = None;
        let mut axis = None;
        if front {
            let stem = format!("screen-{}", step.name);
            match pipeline::grab(ctx, &adb, &dir, &stem) {
                Ok(grabbed) => {
                    let (w, h) = grabbed.stats.px;
                    let shown = if w > h {
                        Axis::Landscape
                    } else {
                        Axis::Portrait
                    };
                    axis = Some(shown);
                    notes.push(format!("screenshot {w}x{h} ({})", shown.name()));
                    if grabbed.stats.blank {
                        if pipeline::window_is_secure(&adb, &app_id) {
                            notes.push(
                                "the window has FLAG_SECURE, so its frame cannot be checked"
                                    .to_string(),
                            );
                        } else {
                            problems.push(format!(
                                "the screenshot is blank ({:.1}% {}): the app lost its frame",
                                grabbed.stats.dominant_share * 100.0,
                                grabbed.stats.dominant
                            ));
                            evidence.push(Evidence::file(&grabbed.png));
                        }
                    }
                    screenshot = Some(grabbed.png);
                }
                Err(error) => problems.push(format!("no screenshot: {}", error.detail)),
            }
        }

        let status = if !problems.is_empty() {
            Status::Fail
        } else if skipped {
            Status::Skip
        } else {
            Status::Pass
        };
        let mut detail = if problems.is_empty() {
            notes.join("; ")
        } else {
            problems.join("; ")
        };
        if detail.is_empty() {
            detail = format!("pid {} carried on", show(now));
        } else if problems.is_empty() && step.expect == Expect::Kept {
            detail = format!("pid {} carried on, {detail}", show(now));
        }
        if status == Status::Fail {
            evidence.push(Evidence::file(&events_path));
        }
        Ok(Found {
            status,
            detail,
            evidence,
            causes,
            screenshot,
            axis,
            presence,
        })
    }

    /// Brings the app back to front after a failed step, so the next steps
    /// test something; stops the suite when it cannot.
    fn recover(&mut self) -> Result<()> {
        let adb = self.adb().clone();
        let app_id = self.app_id().to_string();
        let component = format!("{app_id}/{ACTIVITY}");
        let _ = self.ctx.step(
            "lifecycle.recover",
            &adb.shell(&format!("am start -W -n {}", super::adb::quote(&component)))
                .timeout(Duration::from_secs(60)),
        )?;
        let deadline = Instant::now() + REDRAW;
        while Instant::now() < deadline {
            if let Presence::Front { pid } = pipeline::presence(&adb, &app_id) {
                self.pid = Some(pid);
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        Err(IcmError::new(
            CheckId::TestLifecycle,
            format!(
                "{app_id} could not be brought back to front on {} after a failed step; the suite stopped",
                adb.serial
            ),
        )
        .fix(
            "Read the failed step's evidence, then run the app again.",
            &["icm run android --json -q"],
        ))
    }

    /// The project's `[checks] android` scripts, after a step that left the
    /// app in front.
    fn hooks(&mut self, step: &str) -> Result<()> {
        let tools = &self.launched.tools;
        let adb = self.adb();
        let mut env = tools.child_env().to_vec();
        env.push(("ICM_LIFECYCLE_STEP".to_string(), step.to_string()));
        let reports = crate::hooks::run_for(
            self.ctx,
            &self.launched.project,
            &crate::hooks::HookContext {
                platform: "android".to_string(),
                pid: self.pid,
                device: Some(adb.serial.clone()),
                adb: Some(format!(
                    "{} -s {}",
                    crate::process::shell_quote(
                        &tools.sdk.adb(&self.ctx.env).display().to_string()
                    ),
                    adb.serial
                )),
                bin: Some(self.launched.apk.clone()),
                logs: Some(self.launched.dir.join("logs.ndjson")),
                log_mark: Some(self.launched.mark.clone()),
                env,
                ..crate::hooks::HookContext::default()
            },
        )?;
        for report in reports {
            let mut value = report.to_json();
            value["step"] = json!(step);
            self.hooks.push(value);
        }
        Ok(())
    }

    /// Reports a step: a `test.lifecycle` check and an entry of the
    /// result's `lifecycle.steps`.
    fn record(
        &mut self,
        name: &str,
        status: Status,
        detail: &str,
        screenshot: Option<PathBuf>,
        evidence: Vec<Evidence>,
        causes: Vec<String>,
    ) {
        let mut check = Check::new(CheckId::TestLifecycle, status, format!("{name}: {detail}"));
        for item in evidence {
            check = check.evidence(item);
        }
        if let Some(path) = &screenshot {
            check = check.evidence(Evidence::file(path));
        }
        check.error.likely_causes = causes;
        if status == Status::Pass || status == Status::Skip {
            check = check.fix("Nothing to fix.", &[]);
        }
        self.ctx.rep.check(check);
        self.results.push(json!({
            "step": name,
            "status": status,
            "detail": detail,
            "pid": self.pid,
            "screenshot": screenshot.as_deref().map(crate::paths::display),
        }));
    }

    /// Restores the settings, records the session and the result.
    fn finish(mut self, outcome: Result<()>) -> Result<()> {
        let ctx = self.ctx;
        let adb = self.adb().clone();
        let restore = self.settings.restore_line();
        if !restore.is_empty() {
            let restored = ctx.step(
                "lifecycle.restore",
                &adb.shell(&restore).timeout(Duration::from_secs(60)),
            );
            if !restored.as_ref().is_ok_and(|outcome| outcome.success()) {
                ctx.rep.check(Check::warn(
                    CheckId::TestLifecycle,
                    format!(
                        "could not restore {}'s settings; run `adb -s {} shell \"{restore}\"`",
                        adb.serial, adb.serial
                    ),
                ));
            }
        }

        // The app may run in a new process now: the session says which.
        let app_id = self.app_id().to_string();
        let mut session = self.launched.session.clone();
        session.pid = adb.pids(&app_id).first().copied().or(self.pid);
        let _ = super::session::write(&self.launched.project, &session);

        let failed: Vec<&str> = self
            .results
            .iter()
            .filter(|step| step["status"] == "fail")
            .filter_map(|step| step["step"].as_str())
            .collect();
        let skipped = self
            .results
            .iter()
            .filter(|step| step["status"] == "skip")
            .count();
        ctx.rep.set(
            "lifecycle",
            json!({
                "device": self.launched.device_name,
                "serial": adb.serial,
                "api": self.api,
                "back": self.launched.project.config.config.android.back,
                "icm_events": self.speaks,
                "steps": self.results,
                "passed": self.results.iter().filter(|step| step["status"] == "pass").count(),
                "failed": failed.len(),
                "skipped": skipped,
            }),
        );
        if !self.hooks.is_empty() {
            ctx.rep
                .set("hooks", Value::Array(std::mem::take(&mut self.hooks)));
        }
        let total = self.results.len();
        ctx.rep.summary(if failed.is_empty() {
            format!(
                "lifecycle suite on {}: {} of {total} steps passed{}",
                self.launched.device_name,
                total - skipped,
                if skipped > 0 {
                    format!(", {skipped} skipped")
                } else {
                    String::new()
                }
            )
        } else {
            format!(
                "lifecycle suite on {}: {} of {total} steps failed ({})",
                self.launched.device_name,
                failed.len(),
                failed.join(", ")
            )
        });
        ctx.rep.next(
            "icm logs android --level warn --json",
            "read the app's warnings and errors",
        );
        ctx.rep.next("icm stop android --json -q", "stop the app");
        outcome
    }
}

fn show(pid: Option<u32>) -> String {
    pid.map_or_else(|| "none".to_string(), |pid| pid.to_string())
}

fn describe(presence: Presence) -> String {
    match presence {
        Presence::Gone => "its process is gone".to_string(),
        Presence::NoActivity { pid } => {
            format!("pid {pid} lives on without an activity (Android destroyed it)")
        }
        Presence::Behind { pid } => format!("pid {pid} is behind another activity"),
        Presence::Front { pid } => format!("pid {pid} is in front"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_steps_follow_back_and_the_api_level() {
        let names = |steps: &[Planned]| steps.iter().map(|s| s.name).collect::<Vec<_>>();
        let system = steps("com.acme.notes", "system", 36);
        assert_eq!(
            names(&system),
            [
                "dark-mode",
                "light-mode",
                "landscape",
                "portrait",
                "font-scale",
                "font-weight",
                "home",
                "home-relaunch",
                "back",
                "back-relaunch",
                "kill",
                "kill-relaunch"
            ]
        );
        let back = system.iter().find(|s| s.name == "back").unwrap();
        assert_eq!(back.expect, Expect::Ended);
        let relaunch = system.iter().find(|s| s.name == "back-relaunch").unwrap();
        assert_eq!(relaunch.expect, Expect::StartedOver { new_process: false });
        assert!(
            relaunch
                .line
                .contains("am start -W -n com.acme.notes/android.app.NativeActivity")
        );
        assert!(
            !relaunch.line.contains("-S"),
            "a relaunch must not force-stop"
        );
        let kill = system.iter().find(|s| s.name == "kill").unwrap();
        assert!(
            kill.line.ends_with("am kill com.acme.notes"),
            "{}",
            kill.line
        );

        let key = steps("com.acme.notes", "key", 30);
        assert!(!names(&key).contains(&"font-weight"));
        assert_eq!(
            key.iter().find(|s| s.name == "back").unwrap().expect,
            Expect::Kept
        );
    }

    #[test]
    fn settings_are_restored_as_they_were() {
        let settings = Settings {
            night: Some("no".into()),
            accelerometer_rotation: Some("1".into()),
            user_rotation: Some("0".into()),
            font_scale: Some("1.0".into()),
            font_weight: Some("null".into()),
        };
        assert_eq!(
            settings.restore_line(),
            "cmd uimode night no ; settings put system user_rotation 0 ; settings put system accelerometer_rotation 1 ; settings put system font_scale 1.0 ; settings delete secure font_weight_adjustment"
        );
        assert_eq!(Settings::default().restore_line(), "");
    }
}
