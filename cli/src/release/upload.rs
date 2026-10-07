//! `UPLOAD.md` and `upload.sh` (design §11): what the owner runs after a
//! release. icm writes them and never runs them.
//!
//! An [`OwnerPlan`] holds the steps: one-time setup, store web UI steps
//! (the listing), the upload commands, and what comes after. Commands are
//! [`Word`]s, so secrets only ever appear as environment-variable
//! references (`"$ASC_KEY_ID"`) or keychain-profile names, and dist paths
//! as `"$D/…"`, `D` being the dist directory `upload.sh` lives in. The
//! argv itself comes from [`super::owner_plans`], the only file allowed to
//! contain upload or notarize commands.
//!
//! `upload.sh` is `set -euo pipefail` bash. It exits 9 when a variable it
//! needs is unset or when the release is not uploadable (unsigned, a gate
//! failed, the owner still has steps), saves each tool's JSON next to the
//! artifacts and runs `icm diagnose` on it even when the tool failed
//! (diagnose's exit then wins, else the tool's), and ends with
//! `icm ledger mark-uploaded`.

use crate::process::shell_quote;
use serde_json::{Value, json};
use std::path::Path;

/// One piece of a shell word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Part {
    /// Literal text.
    Lit(String),
    /// An environment variable's value (`$NAME`); never its value.
    Env(String),
    /// A path inside the dist directory (`$D/<path>`).
    Dist(String),
}

/// A shell word made of parts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Word(pub Vec<Part>);

impl Word {
    /// Literal text.
    pub fn lit(text: impl Into<String>) -> Word {
        Word(vec![Part::Lit(text.into())])
    }

    /// `"$NAME"`.
    pub fn env(name: impl Into<String>) -> Word {
        Word(vec![Part::Env(name.into())])
    }

    /// `"$D/<path>"`.
    pub fn dist(path: impl Into<String>) -> Word {
        Word(vec![Part::Dist(path.into())])
    }

    /// The word as it appears in a bash script.
    pub fn shell(&self) -> String {
        if self.0.iter().all(|part| matches!(part, Part::Lit(_))) {
            let text: String = self
                .0
                .iter()
                .map(|part| match part {
                    Part::Lit(text) => text.as_str(),
                    _ => "",
                })
                .collect();
            return shell_quote(&text);
        }
        let mut out = String::from("\"");
        for part in &self.0 {
            match part {
                Part::Lit(text) => {
                    for c in text.chars() {
                        if matches!(c, '"' | '\\' | '$' | '`') {
                            out.push('\\');
                        }
                        out.push(c);
                    }
                }
                Part::Env(name) => out.push_str(&format!("${{{name}}}")),
                Part::Dist(path) => {
                    out.push_str("$D/");
                    for c in path.chars() {
                        if matches!(c, '"' | '\\' | '$' | '`') {
                            out.push('\\');
                        }
                        out.push(c);
                    }
                }
            }
        }
        out.push('"');
        out
    }

    /// The word for `owner_steps` argv: variables as `$NAME`, dist paths
    /// absolute.
    pub fn plain(&self, dist: &Path) -> String {
        self.0
            .iter()
            .map(|part| match part {
                Part::Lit(text) => text.clone(),
                Part::Env(name) => format!("${name}"),
                Part::Dist(path) => dist.join(path).display().to_string(),
            })
            .collect()
    }

    /// The variables the word reads.
    pub fn vars(&self) -> impl Iterator<Item = &str> {
        self.0.iter().filter_map(|part| match part {
            Part::Env(name) => Some(name.as_str()),
            _ => None,
        })
    }
}

/// A command the owner runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    /// The argv.
    pub argv: Vec<Word>,
    /// A file in the dist directory its stdout is saved to (`| tee`).
    pub tee: Option<String>,
    /// `icm diagnose <tool>` runs on the saved output afterwards.
    pub diagnose: Option<&'static str>,
}

impl Command {
    /// A command from words.
    pub fn new(argv: Vec<Word>) -> Command {
        Command {
            argv,
            tee: None,
            diagnose: None,
        }
    }

    /// Saves stdout to a dist file.
    pub fn tee(mut self, file: &str) -> Command {
        self.tee = Some(file.to_string());
        self
    }

    /// Runs `icm diagnose <tool>` on the saved output.
    pub fn diagnose(mut self, tool: &'static str) -> Command {
        self.diagnose = Some(tool);
        self
    }

    /// The command line in bash.
    pub fn shell(&self) -> String {
        let mut line = self
            .argv
            .iter()
            .map(Word::shell)
            .collect::<Vec<_>>()
            .join(" ");
        if let Some(file) = &self.tee {
            line.push_str(&format!(" | tee {}", Word::dist(file.as_str()).shell()));
        }
        line
    }
}

/// What kind of step it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepKind {
    /// Once per machine or per app (keys, credentials, the store record).
    Once,
    /// In a store's web UI (the listing, a first manual upload).
    Web,
    /// What `upload.sh` runs.
    Upload,
    /// After the upload (`icm ledger mark-uploaded`, a check).
    After,
}

impl StepKind {
    /// The name in `owner_steps`.
    pub fn as_str(self) -> &'static str {
        match self {
            StepKind::Once => "once",
            StepKind::Web => "web",
            StepKind::Upload => "upload",
            StepKind::After => "after",
        }
    }
}

/// One step for the owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerStep {
    /// What it does, in a few words.
    pub title: String,
    /// When it runs.
    pub kind: StepKind,
    /// The command, if it is one.
    pub command: Option<Command>,
    /// More to know.
    pub note: Option<String>,
}

impl OwnerStep {
    /// A command step.
    pub fn run(kind: StepKind, title: &str, command: Command) -> OwnerStep {
        OwnerStep {
            title: title.to_string(),
            kind,
            command: Some(command),
            note: None,
        }
    }

    /// A step without a command (a web UI step, a decision).
    pub fn manual(kind: StepKind, title: &str, note: &str) -> OwnerStep {
        OwnerStep {
            title: title.to_string(),
            kind,
            command: None,
            note: Some(note.to_string()),
        }
    }

    /// Adds a note.
    pub fn note(mut self, note: &str) -> OwnerStep {
        self.note = Some(note.to_string());
        self
    }

    /// The step as an `owner_steps` entry.
    pub fn to_json(&self, dist: &Path) -> Value {
        json!({
            "kind": self.kind.as_str(),
            "title": self.title,
            "argv": self.command.as_ref().map(|c| c.argv.iter().map(|w| w.plain(dist)).collect::<Vec<_>>()),
            "command": self.command.as_ref().map(Command::shell),
            "note": self.note,
        })
    }
}

/// A variable `upload.sh` needs, and what it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NeedEnv {
    /// The name.
    pub name: String,
    /// What it holds.
    pub what: String,
}

/// Everything the owner does after a release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerPlan {
    /// Where it goes, e.g. `App Store Connect`.
    pub destination: String,
    /// The variables the commands read.
    pub env: Vec<NeedEnv>,
    /// The steps, in order.
    pub steps: Vec<OwnerStep>,
    /// When set, `upload.sh` prints this and exits 9 instead of uploading
    /// (a step only the store's web UI can do, such as Google Play's first
    /// upload).
    pub manual_only: Option<String>,
}

impl OwnerPlan {
    /// A plan without steps.
    pub fn new(destination: &str) -> OwnerPlan {
        OwnerPlan {
            destination: destination.to_string(),
            env: Vec::new(),
            steps: Vec::new(),
            manual_only: None,
        }
    }

    /// Adds a variable the commands need (once).
    pub fn need(&mut self, name: &str, what: &str) {
        if !self.env.iter().any(|need| need.name == name) {
            self.env.push(NeedEnv {
                name: name.to_string(),
                what: what.to_string(),
            });
        }
    }

    /// Adds a step.
    pub fn push(&mut self, step: OwnerStep) {
        self.steps.push(step);
    }

    /// The steps of one kind.
    pub fn of(&self, kind: StepKind) -> impl Iterator<Item = &OwnerStep> {
        self.steps.iter().filter(move |step| step.kind == kind)
    }

    /// Every variable a command `upload.sh` runs reads but
    /// [`OwnerPlan::env`] does not declare: a bug in an owner plan. (The
    /// once-only steps are run by hand; their notes name their variables.)
    pub fn undeclared_vars(&self) -> Vec<String> {
        let mut missing: Vec<String> = Vec::new();
        for step in self
            .steps
            .iter()
            .filter(|step| matches!(step.kind, StepKind::Upload | StepKind::After))
        {
            for word in step.command.iter().flat_map(|c| c.argv.iter()) {
                for var in word.vars() {
                    if var != "HOME"
                        && !self.env.iter().any(|need| need.name == var)
                        && !missing.iter().any(|m| m == var)
                    {
                        missing.push(var.to_string());
                    }
                }
            }
        }
        missing
    }

    /// The plan as `owner_steps`.
    pub fn to_json(&self, dist: &Path) -> Vec<Value> {
        self.steps.iter().map(|step| step.to_json(dist)).collect()
    }
}

/// What the documents say about the release.
pub struct Facts<'a> {
    /// `<App> <version> (build <n>)`.
    pub app: String,
    /// The target.
    pub target: &'a str,
    /// The dist directory (absolute).
    pub dist: &'a Path,
    /// `icm --version`.
    pub icm: &'a str,
    /// When it was made.
    pub created: &'a str,
    /// The git revision and whether the tree was dirty.
    pub source: String,
    /// The files: (role, path relative to dist, bytes, sha256).
    pub files: Vec<(String, String, u64, String)>,
    /// `None` when uploadable, else why not.
    pub not_uploadable: Option<String>,
    /// The listing facts (label, value or what to set).
    pub listing: Vec<(String, String)>,
}

/// Renders `UPLOAD.md`.
pub fn upload_md(plan: &OwnerPlan, facts: &Facts<'_>) -> String {
    let mut md = format!(
        "# Upload: {} to {}\n\nGenerated by icm {} on {} from {}.\n\n\
         **icm never uploads, publishes or notarizes.** The owner runs every command below, or \
         `upload.sh` in this directory, which checks the variables first. Secrets appear only as \
         environment variables or keychain profiles; set them in your shell, never in a file.\n\n",
        facts.app, plan.destination, facts.icm, facts.created, facts.source
    );
    match &facts.not_uploadable {
        None => md.push_str("**Uploadable:** yes: signed, and every gate passed.\n\n"),
        Some(why) => md.push_str(&format!(
            "**Not uploadable:** {why}. `upload.sh` refuses to run (exit 9) until a release \
             fixes that.\n\n"
        )),
    }

    md.push_str(&format!(
        "Directory: `{}`\n\n| File | Role | Bytes | SHA-256 |\n|---|---|---|---|\n",
        facts.dist.display()
    ));
    for (role, path, bytes, sha) in &facts.files {
        md.push_str(&format!("| `{path}` | {role} | {bytes} | `{sha}` |\n"));
    }
    md.push('\n');

    let section = |md: &mut String, title: &str, steps: Vec<&OwnerStep>| {
        if steps.is_empty() {
            return;
        }
        md.push_str(&format!("## {title}\n\n"));
        for step in steps {
            md.push_str(&format!("- **{}**", step.title));
            if let Some(note) = &step.note {
                md.push_str(&format!(": {note}"));
            }
            md.push('\n');
            if let Some(command) = &step.command {
                md.push_str(&format!("\n  ```sh\n  {}\n  ```\n\n", command.shell()));
            }
        }
        md.push('\n');
    };

    section(&mut md, "Once", plan.of(StepKind::Once).collect());
    section(
        &mut md,
        "In the store's web UI",
        plan.of(StepKind::Web).collect(),
    );

    if !facts.listing.is_empty() {
        md.push_str("## Listing\n\n");
        for (label, value) in &facts.listing {
            md.push_str(&format!("- {label}: {value}\n"));
        }
        md.push('\n');
    }

    let uploads: Vec<&OwnerStep> = plan
        .of(StepKind::Upload)
        .chain(plan.of(StepKind::After))
        .collect();
    if let Some(message) = &plan.manual_only {
        md.push_str(&format!("## Upload\n\n{message}\n\n"));
    }
    if !uploads.is_empty() {
        if plan.manual_only.is_none() {
            md.push_str("## Upload\n\n");
        } else {
            md.push_str("Later releases run:\n\n");
        }
        if !plan.env.is_empty() {
            md.push_str("Environment:\n\n");
            for need in &plan.env {
                md.push_str(&format!("- `{}`: {}\n", need.name, need.what));
            }
            md.push('\n');
        }
        md.push_str(&format!(
            "```sh\nD={}\n",
            shell_quote(&facts.dist.display().to_string())
        ));
        for step in &uploads {
            md.push_str(&format!("# {}\n", step.title));
            if let Some(command) = &step.command {
                md.push_str(&command.shell());
                md.push('\n');
                if let (Some(tool), Some(file)) = (command.diagnose, &command.tee) {
                    md.push_str(&format!(
                        "icm diagnose {tool} {}\n",
                        Word::dist(file.as_str()).shell()
                    ));
                }
            }
        }
        md.push_str("```\n");
    }
    md
}

/// Renders `upload.sh`.
pub fn upload_sh(plan: &OwnerPlan, facts: &Facts<'_>) -> String {
    let mut sh = format!(
        "#!/usr/bin/env bash\n\
         # upload.sh: {} to {}, generated by icm {} on {}.\n\
         # The owner runs it; icm never uploads, publishes or notarizes. See UPLOAD.md.\n\
         # Exit 9: something only the owner can provide is missing.\n\
         set -euo pipefail\n\
         D=\"$(cd \"$(dirname \"${{BASH_SOURCE[0]}}\")\" && pwd)\"\n\
         ICM=\"${{ICM:-icm}}\"\n\n",
        facts.app, plan.destination, facts.icm, facts.created
    );

    if let Some(why) = &facts.not_uploadable {
        sh.push_str(&format!(
            "echo {} >&2\nexit 9\n",
            shell_quote(&format!("upload.sh: this release is not uploadable: {why}"))
        ));
        return sh;
    }
    if let Some(message) = &plan.manual_only {
        sh.push_str(&format!(
            "echo {} >&2\nexit 9\n",
            shell_quote(&format!("upload.sh: {message}"))
        ));
        return sh;
    }

    if !plan.env.is_empty() {
        sh.push_str("need() {\n  if [ -z \"${!1:-}\" ]; then\n    echo \"upload.sh: set $1 ($2); see UPLOAD.md\" >&2\n    exit 9\n  fi\n}\n");
        for need in &plan.env {
            sh.push_str(&format!("need {} {}\n", need.name, shell_quote(&need.what)));
        }
        sh.push('\n');
    }

    let once: Vec<&OwnerStep> = plan
        .of(StepKind::Once)
        .chain(plan.of(StepKind::Web))
        .collect();
    if !once.is_empty() {
        sh.push_str("# Done once, before the first upload (see UPLOAD.md):\n");
        for step in once {
            sh.push_str(&format!("#   {}\n", step.title));
        }
        sh.push('\n');
    }

    let mut number = 0;
    for step in plan.of(StepKind::Upload).chain(plan.of(StepKind::After)) {
        let Some(command) = &step.command else {
            continue;
        };
        number += 1;
        sh.push_str(&format!("# {number}. {}\n", step.title));
        let mut line = command.shell();
        // icm itself through $ICM, so the owner can point at a build.
        if line.starts_with("icm ") {
            line = format!("\"$ICM\"{}", &line[3..]);
        }
        match (command.diagnose, &command.tee) {
            // The tool's exit is kept and diagnose runs on its output
            // whatever it was, so a failure gets its catalogue id and exit
            // (an authentication error is the owner's, exit 9) instead of
            // ending the script with the tool's own code.
            (Some(tool), Some(file)) => {
                sh.push_str(&format!(
                    "set +e\n{line}\nstatus=${{PIPESTATUS[0]}}\nset -e\n\"$ICM\" diagnose {tool} {}\nif [ \"$status\" -ne 0 ]; then exit \"$status\"; fi\n",
                    Word::dist(file.as_str()).shell()
                ));
            }
            _ => {
                sh.push_str(&line);
                sh.push('\n');
            }
        }
        sh.push('\n');
    }
    sh
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_quote_for_bash() {
        assert_eq!(Word::lit("--wait").shell(), "--wait");
        assert_eq!(Word::lit("a b").shell(), "'a b'");
        assert_eq!(Word::env("ASC_KEY_ID").shell(), "\"${ASC_KEY_ID}\"");
        assert_eq!(Word::dist("Notes App.ipa").shell(), "\"$D/Notes App.ipa\"");
        let mixed = Word(vec![
            Part::Env("HOME".into()),
            Part::Lit("/keys/AuthKey_".into()),
            Part::Env("ASC_KEY_ID".into()),
            Part::Lit(".p8".into()),
        ]);
        assert_eq!(mixed.shell(), "\"${HOME}/keys/AuthKey_${ASC_KEY_ID}.p8\"");
        assert_eq!(
            mixed.plain(Path::new("/d")),
            "$HOME/keys/AuthKey_$ASC_KEY_ID.p8"
        );
        assert_eq!(mixed.vars().collect::<Vec<_>>(), ["HOME", "ASC_KEY_ID"]);
        assert_eq!(Word::dist("a.ipa").plain(Path::new("/d")), "/d/a.ipa");
        assert_eq!(Word::lit("x$y").shell(), "'x$y'");
    }

    fn plan() -> OwnerPlan {
        let mut plan = OwnerPlan::new("Somewhere");
        plan.need("TOKEN", "the upload token");
        plan.push(OwnerStep::manual(
            StepKind::Once,
            "Create the record",
            "in the web UI",
        ));
        plan.push(OwnerStep::run(
            StepKind::Upload,
            "Upload",
            Command::new(vec![
                Word::lit("uploader"),
                Word::dist("App.zip"),
                Word::lit("--token"),
                Word::env("TOKEN"),
            ])
            .tee("upload.json")
            .diagnose("altool"),
        ));
        plan.push(OwnerStep::run(
            StepKind::After,
            "Record it",
            Command::new(vec![
                Word::lit("icm"),
                Word::lit("ledger"),
                Word::lit("mark-uploaded"),
            ]),
        ));
        plan
    }

    fn facts(dist: &Path, not_uploadable: Option<String>) -> Facts<'_> {
        Facts {
            app: "App 1.0.0 (build 3)".into(),
            target: "ios",
            dist,
            icm: "0.14.1-mobile.1",
            created: "2026-10-07T00:00:00Z",
            source: "rev abc".into(),
            files: vec![("upload".into(), "App.zip".into(), 3, "ab".into())],
            not_uploadable,
            listing: vec![("Privacy policy URL".into(), "https://x.dev/p".into())],
        }
    }

    #[test]
    fn upload_sh_checks_variables_and_saves_tool_output() {
        let plan = plan();
        assert!(plan.undeclared_vars().is_empty());
        let sh = upload_sh(&plan, &facts(Path::new("/d"), None));
        assert!(sh.starts_with("#!/usr/bin/env bash\n"), "{sh}");
        assert!(sh.contains("set -euo pipefail"), "{sh}");
        assert!(sh.contains("need TOKEN 'the upload token'"), "{sh}");
        assert!(
            sh.contains("set +e\nuploader \"$D/App.zip\" --token \"${TOKEN}\" | tee \"$D/upload.json\"\nstatus=${PIPESTATUS[0]}\nset -e\n\"$ICM\" diagnose altool \"$D/upload.json\"\nif [ \"$status\" -ne 0 ]; then exit \"$status\"; fi\n"),
            "{sh}"
        );
        assert!(sh.contains("\"$ICM\" ledger mark-uploaded"), "{sh}");
        assert!(sh.contains("#   Create the record"), "{sh}");

        let refused = upload_sh(&plan, &facts(Path::new("/d"), Some("unsigned".into())));
        assert!(
            refused.contains("not uploadable: unsigned' >&2\nexit 9"),
            "{refused}"
        );
        assert!(!refused.contains("uploader"), "{refused}");
    }

    /// Runs `upload.sh` with a fake uploader and a fake icm whose diagnose
    /// exits with `diagnosed`; returns the exit code and the log.
    fn run_upload_sh(uploader_exit: i32, diagnosed: i32) -> (Option<i32>, String) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let log = dir.path().join("log");
        for (name, script) in [
            (
                "uploader",
                format!(
                    "#!/bin/sh\necho '{{\"error\":true}}'\necho uploader >> '{}'\nexit {uploader_exit}\n",
                    log.display()
                ),
            ),
            (
                "icm",
                format!(
                    "#!/bin/sh\necho \"icm $1 $2 $(cat \"$3\" 2>/dev/null)\" >> '{}'\n[ \"$1\" = diagnose ] && exit {diagnosed}\nexit 0\n",
                    log.display()
                ),
            ),
        ] {
            let path = bin.join(name);
            std::fs::write(&path, script).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let dist = dir.path().join("dist");
        std::fs::create_dir_all(&dist).unwrap();
        let sh = dist.join("upload.sh");
        std::fs::write(&sh, upload_sh(&plan(), &facts(&dist, None))).unwrap();
        let output = std::process::Command::new("bash")
            .arg(&sh)
            .env("TOKEN", "t")
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .output()
            .unwrap();
        (
            output.status.code(),
            std::fs::read_to_string(&log).unwrap_or_default(),
        )
    }

    #[test]
    fn upload_sh_diagnoses_a_failed_tool() {
        // The tool failed and diagnose knew why: diagnose's exit.
        let (code, log) = run_upload_sh(1, 9);
        assert_eq!(code, Some(9), "{log}");
        assert!(
            log.contains("icm diagnose altool {\"error\":true}"),
            "{log}"
        );
        assert!(!log.contains("ledger"), "{log}");
        // Diagnose found nothing: the tool's own exit.
        let (code, log) = run_upload_sh(3, 0);
        assert_eq!(code, Some(3), "{log}");
        assert!(log.contains("icm diagnose altool"), "{log}");
        // Success runs on to the ledger.
        let (code, log) = run_upload_sh(0, 0);
        assert_eq!(code, Some(0), "{log}");
        assert!(log.contains("icm ledger mark-uploaded"), "{log}");
    }

    #[test]
    fn upload_md_lists_files_steps_and_listing() {
        let md = upload_md(&plan(), &facts(Path::new("/d"), None));
        assert!(md.contains("**Uploadable:** yes"), "{md}");
        assert!(md.contains("| `App.zip` | upload | 3 | `ab` |"), "{md}");
        assert!(md.contains("## Once"), "{md}");
        assert!(md.contains("- Privacy policy URL: https://x.dev/p"), "{md}");
        assert!(md.contains("- `TOKEN`: the upload token"), "{md}");
        assert!(md.contains("D=/d\n"), "{md}");
        assert!(
            md.contains("icm diagnose altool \"$D/upload.json\""),
            "{md}"
        );
    }

    #[test]
    fn undeclared_variables_are_found() {
        let mut plan = OwnerPlan::new("x");
        plan.push(OwnerStep::run(
            StepKind::Upload,
            "u",
            Command::new(vec![Word::env("SECRET")]),
        ));
        assert_eq!(plan.undeclared_vars(), ["SECRET"]);
    }
}
