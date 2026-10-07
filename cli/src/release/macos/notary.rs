//! `icm diagnose notarytool <file|->` (design §6, §11.4): reads the JSON
//! the owner's notarytool printed for a submission (saved by `upload.sh`
//! as `notary-app.json` / `notary-dmg.json`) or `notarytool log` printed,
//! and maps it to catalogue ids, so an agent can act on a rejection
//! without trusting notarytool's exit code.
//!
//! - `status: Accepted` is PASS `macos.notarization`;
//! - `Invalid` / `Rejected` is FAIL `macos.notarization` (exit 1), and
//!   each issue of a `notarytool log` maps to the gate that prevents it
//!   (`macos.hardened_runtime`, `macos.sign.verify`,
//!   `macos.sign.no_developer_id`);
//! - authentication and keychain-profile errors are the owner's
//!   (`macos.notary_credentials`, exit 9);
//! - `In Progress` is a WARN: the owner fetches the result later.

use crate::catalogue::CheckId;
use crate::error::{Check, Evidence, IcmError};
use serde_json::Value;

/// One finding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// The catalogue id.
    pub id: CheckId,
    /// PASS, WARN or FAIL.
    pub status: crate::error::Status,
    /// What it says.
    pub detail: String,
}

/// The catalogue id an issue message maps to.
pub fn issue_id(message: &str) -> CheckId {
    let lower = message.to_ascii_lowercase();
    if lower.contains("hardened runtime") {
        CheckId::MacosHardenedRuntime
    } else if lower.contains("developer id certificate")
        || lower.contains("not signed with a valid")
    {
        CheckId::MacosSignNoDeveloperId
    } else if lower.contains("secure timestamp")
        || lower.contains("signature")
        || lower.contains("get-task-allow")
        || lower.contains("not signed")
    {
        CheckId::MacosSignVerify
    } else {
        CheckId::MacosNotarization
    }
}

fn credentials(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "unable to authenticate",
        "http status code: 401",
        "http status code: 403",
        "no keychain password item found",
        "keychain profile",
        "invalid credentials",
        "agreement",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Reads notarytool output (JSON from `submit` or `log`, or its text).
pub fn findings(text: &str) -> Vec<Finding> {
    use crate::error::Status;
    let mut found = Vec::new();
    let json: Option<Value> = serde_json::from_str(text.trim()).ok();
    let status = json
        .as_ref()
        .and_then(|value| value.get("status"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            text.lines()
                .find_map(|line| line.trim().strip_prefix("status:"))
                .map(|status| status.trim().to_string())
        });
    let id = json
        .as_ref()
        .and_then(|value| value.get("id").or_else(|| value.get("jobId")))
        .and_then(Value::as_str)
        .map(|id| format!(" (submission {id})"))
        .unwrap_or_default();

    if let Some(issues) = json
        .as_ref()
        .and_then(|value| value.get("issues"))
        .and_then(Value::as_array)
    {
        for issue in issues {
            let message = issue
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("an issue without a message");
            let path = issue.get("path").and_then(Value::as_str).unwrap_or("");
            let arch = issue
                .get("architecture")
                .and_then(Value::as_str)
                .map(|arch| format!(" [{arch}]"))
                .unwrap_or_default();
            let severity = issue
                .get("severity")
                .and_then(Value::as_str)
                .unwrap_or("error");
            found.push(Finding {
                id: issue_id(message),
                status: if severity == "warning" {
                    Status::Warn
                } else {
                    Status::Fail
                },
                detail: format!("notarization: {path}{arch}: {message}"),
            });
        }
    }

    match status.as_deref() {
        Some("Accepted") => found.insert(
            0,
            Finding {
                id: CheckId::MacosNotarization,
                status: Status::Pass,
                detail: format!("Apple's notary service accepted it{id}; staple the ticket next"),
            },
        ),
        Some(status @ ("Invalid" | "Rejected")) => {
            let summary = json
                .as_ref()
                .and_then(|value| value.get("statusSummary").or_else(|| value.get("message")))
                .and_then(Value::as_str)
                .map(|summary| format!(": {summary}"))
                .unwrap_or_default();
            found.insert(
                0,
                Finding {
                    id: CheckId::MacosNotarization,
                    status: Status::Fail,
                    detail: format!(
                        "Apple's notary service answered {status}{id}{summary}{}",
                        if found.is_empty() {
                            "; `xcrun notarytool log <id> --keychain-profile <profile>` lists the issues (save it and run `icm diagnose notarytool` on it)"
                        } else {
                            ""
                        }
                    ),
                },
            );
        }
        Some(status @ "In Progress") => found.insert(
            0,
            Finding {
                id: CheckId::MacosNotarization,
                status: Status::Warn,
                detail: format!(
                    "the submission{id} is still {status}; wait for it with `xcrun notarytool wait <id>`"
                ),
            },
        ),
        _ if credentials(text) => found.push(Finding {
            id: CheckId::MacosNotaryCredentials,
            status: Status::Fail,
            detail: format!(
                "notarytool could not use the owner's credentials: {}",
                text.lines()
                    .find(|line| credentials(line))
                    .unwrap_or("")
                    .trim()
            ),
        }),
        Some(other) => found.push(Finding {
            id: CheckId::MacosNotarization,
            status: Status::Fail,
            detail: format!("notarytool reported status {other}{id}"),
        }),
        None if found.is_empty() => found.push(Finding {
            id: CheckId::MacosNotarization,
            status: Status::Fail,
            detail: "the input holds no notarytool status or issue: pass the JSON `upload.sh` saved (notary-app.json, notary-dmg.json) or `notarytool log` output".to_string(),
        }),
        None => {}
    }
    found
}

/// The checks for the findings, and the error that ends the command (the
/// owner's first, else the first failure).
pub fn report(text: &str, evidence: Option<&Evidence>) -> (Vec<Check>, Option<IcmError>) {
    use crate::error::Status;
    let mut checks = Vec::new();
    let mut owner = None;
    let mut first = None;
    for finding in findings(text) {
        let mut check = Check::new(finding.id, finding.status, finding.detail);
        if let Some(evidence) = evidence {
            check = check.evidence(evidence.clone());
        }
        if finding.id == CheckId::MacosNotaryCredentials {
            check = check.fix(
                "The owner stores the App Store Connect API key once: `xcrun notarytool store-credentials <profile> --key ... --key-id ... --issuer ...` (UPLOAD.md has the exact line), then reruns upload.sh.",
                &["icm upload-commands macos"],
            );
        }
        if check.status == Status::Fail {
            let error = check.error.clone();
            if finding.id == CheckId::MacosNotaryCredentials {
                owner.get_or_insert(error);
            } else {
                first.get_or_insert(error);
            }
        }
        checks.push(check);
    }
    (checks, owner.or(first))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Status;

    #[test]
    fn an_accepted_submission_passes() {
        let (checks, error) = report(
            r#"{"id":"2efe2717","message":"Processing complete","status":"Accepted"}"#,
            None,
        );
        assert!(error.is_none());
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].status, Status::Pass);
        assert!(checks[0].error.detail.contains("2efe2717"));
    }

    #[test]
    fn a_log_maps_issues_to_gates() {
        let log = r#"{
          "logFormatVersion": 1, "jobId": "abc", "status": "Invalid",
          "statusSummary": "Archive contains critical validation errors",
          "issues": [
            {"severity": "error", "path": "Notes.zip/Notes.app/Contents/MacOS/notes",
             "message": "The executable does not have the hardened runtime enabled.", "architecture": "arm64"},
            {"severity": "error", "path": "Notes.zip/Notes.app/Contents/MacOS/notes",
             "message": "The signature does not include a secure timestamp.", "architecture": "arm64"},
            {"severity": "error", "path": "Notes.zip/Notes.app/Contents/MacOS/notes",
             "message": "The binary is not signed with a valid Developer ID certificate.", "architecture": "arm64"},
            {"severity": "warning", "path": "Notes.zip/Notes.app",
             "message": "Something new.", "architecture": null}
          ]
        }"#;
        let (checks, error) = report(log, Some(&Evidence::file("/d/notary-log.json")));
        let ids: Vec<&str> = checks.iter().map(|c| c.id()).collect();
        assert_eq!(
            ids,
            [
                "macos.notarization",
                "macos.hardened_runtime",
                "macos.sign.verify",
                "macos.sign.no_developer_id",
                "macos.notarization"
            ]
        );
        assert_eq!(checks[4].status, Status::Warn);
        let error = error.unwrap();
        assert_eq!(error.id, "macos.notarization");
        assert!(error.detail.contains("critical validation errors"));
        assert_eq!(checks[1].error.evidence[0].path, "/d/notary-log.json");
    }

    #[test]
    fn a_bare_invalid_submission_points_at_the_log() {
        let (checks, error) = report(
            r#"{"id":"abc","message":"Processing complete","status":"Invalid"}"#,
            None,
        );
        assert_eq!(checks.len(), 1);
        assert!(error.unwrap().detail.contains("notarytool log"));
    }

    #[test]
    fn credential_errors_are_the_owners() {
        let text = "Error: HTTP status code: 401. Unable to authenticate. Invalid credentials.\n";
        let (checks, error) = report(text, None);
        assert_eq!(checks[0].id(), "macos.notary_credentials");
        let error = error.unwrap();
        assert_eq!(error.id, "macos.notary_credentials");
        assert_eq!(error.exit, crate::exit::Exit::NeedsOwner);
        let (_, profile) = report(
            "Error: No Keychain password item found for profile: icm-notary\n",
            None,
        );
        assert_eq!(profile.unwrap().id, "macos.notary_credentials");
    }

    #[test]
    fn text_output_and_waiting_and_nonsense() {
        let (checks, error) = report("Processing complete\n  id: abc\n  status: Accepted\n", None);
        assert!(error.is_none());
        assert_eq!(checks[0].status, Status::Pass);
        let (checks, error) = report(r#"{"id":"abc","status":"In Progress"}"#, None);
        assert!(error.is_none());
        assert_eq!(checks[0].status, Status::Warn);
        let (_, error) = report("hello", None);
        assert_eq!(error.unwrap().id, "macos.notarization");
    }
}
