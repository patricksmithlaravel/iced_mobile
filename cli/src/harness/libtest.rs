//! Reads what `cargo test` prints: libtest's human output on stdout (one
//! block per test binary), the harness's `ICM_HARNESS` lines for the flows,
//! and cargo's `Running …` / `Doc-tests …` lines on stderr, which name the
//! blocks in order. cargo's own JSON messages on stdout are skipped.

use serde_json::Value;

/// One test binary's run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Suite {
    /// What cargo called it: `unittests src/lib.rs`, `tests/icm.rs`,
    /// `doc-tests app`.
    pub name: String,
    /// Whether this is the icm harness (it printed `ICM_HARNESS`).
    pub harness: bool,
    /// The harness's protocol, from `ICM_HARNESS {"protocol":N}`.
    pub protocol: Option<u64>,
    /// The `running N tests` count.
    pub planned: usize,
    /// Passed tests.
    pub passed: usize,
    /// Failed tests.
    pub failed: usize,
    /// Ignored tests.
    pub ignored: usize,
    /// Tests the filter left out.
    pub filtered: usize,
    /// The failed tests, with their captured output.
    pub failures: Vec<Failure>,
    /// Whether the binary printed its summary (`test result:` or
    /// `ICM_HARNESS_RESULT`); a binary that crashed did not.
    pub finished: bool,
    /// The harness's result object (`kind: flows`).
    pub result: Option<Value>,
}

/// A failed test.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Failure {
    /// The test's name, e.g. `tests::a_tap_increments_the_count`.
    pub name: String,
    /// Its captured output (the `---- name stdout ----` block).
    pub output: String,
}

fn is_cargo_message(line: &str) -> bool {
    line.starts_with("{\"reason\":")
}

fn running_count(line: &str) -> Option<usize> {
    let rest = line.strip_prefix("running ")?;
    let (count, word) = rest.split_once(' ')?;
    if word == "test" || word == "tests" {
        count.parse().ok()
    } else {
        None
    }
}

/// `test result: ok. 3 passed; 1 failed; 0 ignored; 0 measured; 2 filtered out; …`
fn summary_counts(line: &str) -> Option<[usize; 4]> {
    let rest = line.strip_prefix("test result: ")?;
    let (_, rest) = rest.split_once(". ")?;
    let mut counts = [0usize; 4];
    for part in rest.split(';') {
        let mut words = part.split_whitespace();
        let (Some(number), Some(word)) = (words.next(), words.next()) else {
            continue;
        };
        let Ok(number) = number.parse::<usize>() else {
            continue;
        };
        match word {
            "passed" => counts[0] = number,
            "failed" => counts[1] = number,
            "ignored" => counts[2] = number,
            "filtered" => counts[3] = number,
            _ => {}
        }
    }
    Some(counts)
}

/// The suite names on cargo's stderr, in order.
pub fn suite_names(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("Running ") {
                let name = match rest.rfind(" (") {
                    Some(at) if rest.ends_with(')') => &rest[..at],
                    _ => rest,
                };
                Some(name.to_string())
            } else {
                line.strip_prefix("Doc-tests ")
                    .map(|crate_name| format!("doc-tests {crate_name}"))
            }
        })
        .collect()
}

/// Parses `cargo test`'s stdout into suites, named from its stderr.
pub fn parse(stdout: &str, stderr: &str) -> Vec<Suite> {
    let mut suites: Vec<Suite> = Vec::new();
    let mut capture: Option<Failure> = None;
    let mut in_failures = false;

    let close_capture = |suite: Option<&mut Suite>, capture: &mut Option<Failure>| {
        if let (Some(suite), Some(failure)) = (suite, capture.take()) {
            let mut failure = failure;
            failure.output = failure.output.trim().to_string();
            match suite.failures.iter_mut().find(|f| f.name == failure.name) {
                Some(existing) => existing.output = failure.output,
                None => suite.failures.push(failure),
            }
        }
    };

    for line in stdout.lines() {
        if is_cargo_message(line) {
            continue;
        }

        if let Some(json) = line.strip_prefix("ICM_HARNESS ") {
            close_capture(suites.last_mut(), &mut capture);
            in_failures = false;
            let protocol = serde_json::from_str::<Value>(json)
                .ok()
                .and_then(|value| value.get("protocol").and_then(Value::as_u64));
            suites.push(Suite {
                harness: true,
                protocol,
                ..Suite::default()
            });
            continue;
        }

        let Some(suite) = suites.last_mut() else {
            if let Some(count) = running_count(line) {
                suites.push(Suite {
                    planned: count,
                    ..Suite::default()
                });
            }
            continue;
        };

        if suite.harness && !suite.finished {
            if let Some(json) = line.strip_prefix("ICM_HARNESS_RESULT ") {
                let result = serde_json::from_str::<Value>(json).ok();
                if let Some(result) = &result {
                    let count =
                        |key: &str| result.get(key).and_then(Value::as_u64).unwrap_or(0) as usize;
                    suite.passed = count("passed");
                    suite.failed = count("failed");
                    suite.planned = suite.passed + suite.failed;
                    suite.failures = result
                        .get("flows")
                        .and_then(Value::as_array)
                        .map(|flows| {
                            flows
                                .iter()
                                .filter(|flow| flow.get("passed") == Some(&Value::Bool(false)))
                                .map(|flow| Failure {
                                    name: flow
                                        .get("name")
                                        .and_then(Value::as_str)
                                        .unwrap_or("")
                                        .to_string(),
                                    output: String::new(),
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                }
                suite.result = result;
                suite.finished = true;
            }
            continue;
        }

        if let Some(count) = running_count(line) {
            close_capture(Some(suite), &mut capture);
            in_failures = false;
            suites.push(Suite {
                planned: count,
                ..Suite::default()
            });
            continue;
        }

        if suite.finished {
            continue;
        }

        if let Some(counts) = summary_counts(line) {
            close_capture(Some(suite), &mut capture);
            in_failures = false;
            suite.passed = counts[0];
            suite.failed = counts[1];
            suite.ignored = counts[2];
            suite.filtered = counts[3];
            suite.finished = true;
            continue;
        }

        if let Some(rest) = line.strip_prefix("---- ") {
            close_capture(Some(suite), &mut capture);
            let name = rest
                .strip_suffix(" stdout ----")
                .or_else(|| rest.strip_suffix(" stderr ----"))
                .unwrap_or(rest)
                .to_string();
            capture = Some(Failure {
                name,
                output: String::new(),
            });
            in_failures = true;
            continue;
        }

        if line == "failures:" {
            close_capture(Some(suite), &mut capture);
            continue;
        }

        if let Some(failure) = capture.as_mut() {
            failure.output.push_str(line);
            failure.output.push('\n');
            continue;
        }

        if !in_failures
            && let Some(rest) = line.strip_prefix("test ")
            && let Some((name, status)) = rest.rsplit_once(" ... ")
            && status.starts_with("FAILED")
            && !suite.failures.iter().any(|f| f.name == name)
        {
            suite.failures.push(Failure {
                name: name.to_string(),
                output: String::new(),
            });
        }
    }
    close_capture(suites.last_mut(), &mut capture);

    for (suite, name) in suites.iter_mut().zip(suite_names(stderr)) {
        suite.name = name;
    }
    for (index, suite) in suites.iter_mut().enumerate() {
        if suite.name.is_empty() {
            suite.name = format!("test binary {}", index + 1);
        }
    }
    suites
}

/// The binaries cargo says did not exit successfully, from stderr:
/// `process didn't exit successfully: `…` (signal: 6, SIGABRT: …)`.
pub fn crashed_processes(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .filter(|line| line.contains("process didn't exit successfully"))
        .map(|line| line.trim().trim_start_matches("error: ").to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const STDERR: &str = "   Compiling app v0.1.0 (/p)\n    Finished `test` profile [unoptimized + debuginfo] target(s) in 2.21s\n     Running unittests src/lib.rs (/t/debug/deps/app-0105)\n     Running unittests src/main.rs (/t/debug/deps/app-99fd)\n     Running tests/icm.rs (/t/debug/deps/icm-cf2c)\n   Doc-tests app\n";

    const PASSING: &str = "{\"reason\":\"compiler-artifact\",\"fresh\":true}\n\nrunning 3 tests\ntest tests::a ... ok\ntest tests::b ... ok\ntest tests::c ... ignored\n\ntest result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.18s\n\n\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n\nICM_HARNESS {\"protocol\":1}\n\nrunning 2 flows from /p/tests/flows\nflow flows::add_item ... ok (25 ms)\nflow flows::smoke ... ok (8 ms)\n\ntest result: ok. 2 passed; 0 failed; finished in 0.03s\n\nICM_HARNESS_RESULT {\"protocol\":1,\"kind\":\"flows\",\"ok\":true,\"passed\":2,\"failed\":0,\"flows\":[]}\n\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n\n";

    #[test]
    fn a_passing_run_has_four_named_suites() {
        let suites = parse(PASSING, STDERR);
        assert_eq!(suites.len(), 4, "{suites:#?}");
        assert_eq!(suites[0].name, "unittests src/lib.rs");
        assert_eq!(
            (suites[0].planned, suites[0].passed, suites[0].ignored),
            (3, 2, 1)
        );
        assert!(suites[0].finished && suites[0].failures.is_empty());
        assert_eq!(suites[1].name, "unittests src/main.rs");
        assert_eq!(suites[2].name, "tests/icm.rs");
        assert!(suites[2].harness);
        assert_eq!(suites[2].protocol, Some(1));
        assert_eq!((suites[2].passed, suites[2].failed), (2, 0));
        assert!(suites[2].result.is_some());
        assert_eq!(suites[3].name, "doc-tests app");
        assert!(suites.iter().all(|s| s.finished));
    }

    #[test]
    fn failures_keep_their_output() {
        let stdout = "\nrunning 2 tests\ntest tests::bad ... FAILED\ntest tests::good ... ok\n\nfailures:\n\n---- tests::bad stdout ----\n\nthread 'tests::bad' panicked at src/lib.rs:200:9:\nassertion `left == right` failed\n  left: 1\n right: 2\nnote: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n\n\nfailures:\n    tests::bad\n\ntest result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n\nICM_HARNESS {\"protocol\":1}\nflow flows::smoke ... FAILED (9 ms)\nICM_HARNESS_RESULT {\"protocol\":1,\"kind\":\"flows\",\"ok\":false,\"passed\":1,\"failed\":1,\"flows\":[{\"name\":\"flows::smoke\",\"passed\":false},{\"name\":\"flows::other\",\"passed\":true}]}\n";
        let stderr = "     Running unittests src/lib.rs (/t/a)\nerror: test failed, to rerun pass `--lib`\n     Running tests/icm.rs (/t/icm)\nerror: test failed, to rerun pass `--test icm`\nerror: 2 targets failed:\n";
        let suites = parse(stdout, stderr);
        assert_eq!(suites.len(), 2);
        let unit = &suites[0];
        assert_eq!((unit.passed, unit.failed), (1, 1));
        assert_eq!(unit.failures.len(), 1);
        assert_eq!(unit.failures[0].name, "tests::bad");
        assert!(
            unit.failures[0]
                .output
                .starts_with("thread 'tests::bad' panicked at src/lib.rs:200:9:"),
            "{:?}",
            unit.failures[0].output
        );
        assert!(unit.failures[0].output.contains("right: 2"));
        let flows = &suites[1];
        assert_eq!(flows.name, "tests/icm.rs");
        assert_eq!((flows.passed, flows.failed), (1, 1));
        assert_eq!(flows.failures[0].name, "flows::smoke");
    }

    #[test]
    fn a_crash_leaves_the_suite_unfinished() {
        let stdout = "\nrunning 2 tests\ntest tests::a ... ok\n";
        let stderr = "     Running unittests src/lib.rs (/t/a)\nerror: test failed, to rerun pass `--lib`\n\nCaused by:\n  process didn't exit successfully: `/t/a` (signal: 11, SIGSEGV: invalid memory reference)\n";
        let suites = parse(stdout, stderr);
        assert_eq!(suites.len(), 1);
        assert!(!suites[0].finished);
        assert_eq!(crashed_processes(stderr).len(), 1);
        assert!(crashed_processes(stderr)[0].contains("SIGSEGV"));
    }

    #[test]
    fn summary_lines_parse() {
        assert_eq!(
            summary_counts(
                "test result: FAILED. 1 passed; 2 failed; 3 ignored; 0 measured; 4 filtered out; finished in 0.01s"
            ),
            Some([1, 2, 3, 4])
        );
        assert_eq!(running_count("running 1 test"), Some(1));
        assert_eq!(running_count("running 2 flows from /x"), None);
        assert_eq!(
            suite_names("   Doc-tests my_app\n     Running tests/a b.rs (/t/x)\n"),
            vec!["doc-tests my_app", "tests/a b.rs"]
        );
    }
}
