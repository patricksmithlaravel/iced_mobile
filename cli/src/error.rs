//! Errors and checks: what goes into `errors[]`, `warnings[]` and `check`
//! events (design §4.3, §4.4).

use crate::catalogue::{By, CheckId, Level};
use crate::exit::Exit;
use serde::Serialize;
use serde_json::{Value, json};
use std::borrow::Cow;
use std::fmt;

/// A file (and optionally a line) that backs a finding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Evidence {
    /// The path, relative to the current directory when it is inside it.
    pub path: String,
    /// The 1-based line, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// The relevant text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
}

impl Evidence {
    /// Evidence pointing at a whole file.
    pub fn file(path: impl AsRef<std::path::Path>) -> Self {
        Evidence {
            path: crate::paths::display(path.as_ref()),
            line: None,
            excerpt: None,
        }
    }

    /// Evidence pointing at a line of a file.
    pub fn line(path: impl AsRef<std::path::Path>, line: u32, excerpt: impl Into<String>) -> Self {
        let excerpt = excerpt.into();
        Evidence {
            path: crate::paths::display(path.as_ref()),
            line: Some(line),
            excerpt: (!excerpt.is_empty()).then_some(excerpt),
        }
    }

    /// Adds an excerpt.
    pub fn with_excerpt(mut self, excerpt: impl Into<String>) -> Self {
        self.excerpt = Some(excerpt.into());
        self
    }
}

impl fmt::Display for Evidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.path)?;
        if let Some(line) = self.line {
            write!(f, ":{line}")?;
        }
        Ok(())
    }
}

/// How to fix a finding, and who must.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Fix {
    /// One sentence.
    pub summary: String,
    /// Commands to run, in order.
    pub commands: Vec<String>,
    /// Who acts.
    pub by: By,
}

/// A compiler diagnostic, as reported by cargo (design §4.3 `diagnostic`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    /// `error`, `warning`, ...
    pub level: String,
    /// The lint or error code, e.g. `E0308`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// The one-line message.
    pub message: String,
    /// rustc's rendered text.
    pub rendered: String,
    /// The primary span's file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// The primary span's line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// The primary span's column.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub col: Option<u32>,
    /// The cargo targets that reported it.
    pub targets: Vec<String>,
}

/// A failure that stops a command, or one entry of `errors[]`.
#[derive(Clone, Debug)]
pub struct IcmError {
    /// The catalogue id (or `hook.<name>`).
    pub id: Cow<'static, str>,
    /// The exit code it causes.
    pub exit: Exit,
    /// What went wrong, in one line.
    pub title: String,
    /// The specifics.
    pub detail: String,
    /// Files backing it.
    pub evidence: Vec<Evidence>,
    /// Matched failure signatures.
    pub likely_causes: Vec<String>,
    /// How to fix it.
    pub fix: Fix,
    /// Rendered compiler diagnostics (Appendix C item 11).
    pub diagnostics: Vec<Diagnostic>,
}

/// icm's result type.
pub type Result<T, E = IcmError> = std::result::Result<T, E>;

impl IcmError {
    /// An error with the catalogue's exit code, title and fix.
    pub fn new(id: CheckId, detail: impl Into<String>) -> Self {
        let entry = id.entry();
        let exit = if entry.exit == Exit::Ok {
            Exit::CheckFailed
        } else {
            entry.exit
        };
        IcmError {
            id: Cow::Borrowed(entry.id),
            exit,
            title: entry.title.to_string(),
            detail: detail.into(),
            evidence: Vec::new(),
            likely_causes: Vec::new(),
            fix: Fix {
                summary: entry.fix.to_string(),
                commands: Vec::new(),
                by: entry.by,
            },
            diagnostics: Vec::new(),
        }
    }

    /// A failure reported by a project hook (`hook.<name>`, exit 1).
    pub fn hook(name: &str, detail: impl Into<String>) -> Self {
        IcmError {
            id: Cow::Owned(format!("hook.{name}")),
            exit: Exit::CheckFailed,
            title: format!("The project hook `{name}` failed"),
            detail: detail.into(),
            evidence: Vec::new(),
            likely_causes: Vec::new(),
            fix: Fix {
                summary: "Read the hook's output and fix what it names.".to_string(),
                commands: Vec::new(),
                by: By::Agent,
            },
            diagnostics: Vec::new(),
        }
    }

    /// The catalogue id, if this is not a hook.
    pub fn check_id(&self) -> Option<CheckId> {
        CheckId::from_id(&self.id)
    }

    /// Adds evidence.
    pub fn evidence(mut self, evidence: Evidence) -> Self {
        self.evidence.push(evidence);
        self
    }

    /// Replaces the fix summary and adds commands.
    pub fn fix(mut self, summary: impl Into<String>, commands: &[&str]) -> Self {
        self.fix.summary = summary.into();
        self.fix.commands = commands.iter().map(ToString::to_string).collect();
        self
    }

    /// Adds fix commands, keeping the summary.
    pub fn fix_commands<I, S>(mut self, commands: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.fix
            .commands
            .extend(commands.into_iter().map(Into::into));
        self
    }

    /// Changes who must act.
    pub fn by(mut self, by: By) -> Self {
        self.fix.by = by;
        self
    }

    /// Overrides the exit code.
    pub fn exit(mut self, exit: Exit) -> Self {
        self.exit = exit;
        self
    }

    /// Adds a likely cause.
    pub fn cause(mut self, cause: impl Into<String>) -> Self {
        self.likely_causes.push(cause.into());
        self
    }

    /// The JSON form used in `errors[]` and `warnings[]`.
    pub fn to_json(&self) -> Value {
        let mut value = json!({
            "id": self.id,
            "exit": self.exit.code(),
            "title": self.title,
            "detail": self.detail,
            "evidence": self.evidence,
            "likely_causes": self.likely_causes,
            "fix": self.fix,
            "docs": format!("icm explain {}", self.id),
        });
        if !self.diagnostics.is_empty() {
            value["diagnostics"] = json!(self.diagnostics);
        }
        value
    }
}

impl fmt::Display for IcmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.detail.is_empty() {
            write!(f, "{}: {}", self.id, self.title)
        } else {
            write!(f, "{}: {}", self.id, self.detail)
        }
    }
}

impl std::error::Error for IcmError {}

/// The status of one check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// It holds.
    Pass,
    /// It does not hold.
    Fail,
    /// It does not hold, but nothing stops.
    Warn,
    /// It was not run.
    Skip,
    /// For information.
    Info,
}

impl Status {
    /// The upper-case keyword of the human `CHECK` line.
    pub fn keyword(self) -> &'static str {
        match self {
            Status::Pass => "PASS",
            Status::Fail => "FAIL",
            Status::Warn => "WARN",
            Status::Skip => "SKIP",
            Status::Info => "INFO",
        }
    }
}

/// One `check` event.
#[derive(Clone, Debug)]
pub struct Check {
    /// What it found, with the id, evidence and fix.
    pub error: IcmError,
    /// Its status.
    pub status: Status,
}

impl Check {
    /// A check with an explicit status.
    pub fn new(id: CheckId, status: Status, detail: impl Into<String>) -> Self {
        Check {
            error: IcmError::new(id, detail),
            status,
        }
    }

    /// A passing check.
    pub fn pass(id: CheckId, detail: impl Into<String>) -> Self {
        Check::new(id, Status::Pass, detail)
    }

    /// A failing check (non-blocking unless returned as an error).
    pub fn fail(id: CheckId, detail: impl Into<String>) -> Self {
        Check::new(id, Status::Fail, detail)
    }

    /// A warning.
    pub fn warn(id: CheckId, detail: impl Into<String>) -> Self {
        Check::new(id, Status::Warn, detail)
    }

    /// A skipped check.
    pub fn skip(id: CheckId, detail: impl Into<String>) -> Self {
        Check::new(id, Status::Skip, detail)
    }

    /// Information.
    pub fn info(id: CheckId, detail: impl Into<String>) -> Self {
        Check::new(id, Status::Info, detail)
    }

    /// A check at the id's default level: FAIL, WARN or INFO.
    pub fn at_default_level(id: CheckId, detail: impl Into<String>) -> Self {
        let status = match id.entry().level {
            Level::Fail => Status::Fail,
            Level::Warn => Status::Warn,
            Level::Info => Status::Info,
            Level::Pass => Status::Pass,
        };
        Check::new(id, status, detail)
    }

    /// A check from an error.
    pub fn from_error(error: IcmError, status: Status) -> Self {
        Check { error, status }
    }

    /// Adds evidence.
    pub fn evidence(mut self, evidence: Evidence) -> Self {
        self.error = self.error.evidence(evidence);
        self
    }

    /// Replaces the fix.
    pub fn fix(mut self, summary: impl Into<String>, commands: &[&str]) -> Self {
        self.error = self.error.fix(summary, commands);
        self
    }

    /// The id.
    pub fn id(&self) -> &str {
        &self.error.id
    }

    /// Whether it failed.
    pub fn failed(&self) -> bool {
        self.status == Status::Fail
    }

    /// Turns a failed check into the error that stops a command.
    pub fn into_error(self) -> IcmError {
        self.error
    }

    /// The `check` event body (without the common `v`/`type`/`run`/`t`).
    pub fn to_event(&self) -> Value {
        let mut value = json!({
            "type": "check",
            "id": self.error.id,
            "status": self.status,
            "detail": self.error.detail,
            "evidence": self.error.evidence,
        });
        if matches!(self.status, Status::Fail | Status::Warn | Status::Info) {
            value["fix"] = json!(self.error.fix);
        }
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_take_the_catalogue_defaults() {
        let error = IcmError::new(CheckId::ConfigUnknownKey, "unknown key `x`");
        assert_eq!(error.exit, Exit::Config);
        assert_eq!(error.fix.by, By::Agent);
        let json = error.to_json();
        assert_eq!(json["id"], "config.unknown_key");
        assert_eq!(json["exit"], 3);
        assert_eq!(json["fix"]["by"], "agent");
        assert_eq!(json["docs"], "icm explain config.unknown_key");
    }

    #[test]
    fn pass_only_ids_still_fail_with_a_non_zero_exit() {
        assert_eq!(IcmError::new(CheckId::RunReady, "").exit, Exit::CheckFailed);
    }

    #[test]
    fn hooks_are_dynamic_ids() {
        let error = IcmError::hook("wallet_smoke", "exit 3");
        assert_eq!(error.id, "hook.wallet_smoke");
        assert_eq!(error.exit, Exit::CheckFailed);
        assert!(error.check_id().is_none());
    }

    #[test]
    fn check_events_carry_fixes_only_when_not_passing() {
        let pass = Check::pass(CheckId::DepsSingleIced, "one iced").to_event();
        assert!(pass.get("fix").is_none());
        let warn = Check::warn(CheckId::DepsCliFrameworkSkew, "skew").to_event();
        assert_eq!(warn["status"], "warn");
        assert_eq!(warn["fix"]["by"], "agent");
    }
}
