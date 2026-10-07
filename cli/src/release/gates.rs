//! The gate framework (design §12): how `icm release` and `icm verify`
//! report what they check, and what `--sign none` changes.
//!
//! - A check whose catalogue entry fails with exit 9 (`NeedsOwner`) is
//!   **owner-dependent**: signing assets, profiles, keystores, the owner's
//!   decisions, placeholders. Under `--sign none` it is a WARN. Under
//!   `--sign auto` it is a FAIL that ends the command with exit 9, either
//!   before the build ([`Gates::needs_owner`], at the next
//!   [`Gates::checkpoint`]) or once the artifacts are written
//!   ([`Gates::needs_owner_later`], and any owner-dependent FAIL a gate
//!   reports through [`Gates::check`]).
//! - Every other FAIL keeps its severity: non-blocking (exit 1 unless
//!   something blocks), and the artifact is not uploadable.
//! - The tally of every check goes into `artifacts.json` (`checks`), so
//!   `icm verify` can tell what the release that produced it saw.

use crate::catalogue::{CheckId, Level};
use crate::cli::SignMode;
use crate::context::Ctx;
use crate::error::{Check, IcmError, Result, Status};
use crate::exit::Exit;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Whether a check id is owner-dependent: its catalogue entry fails with
/// exit 9 (`NeedsOwner`).
pub fn owner_dependent(id: &str) -> bool {
    CheckId::from_id(id).is_some_and(|check| {
        let entry = check.entry();
        entry.exit == Exit::NeedsOwner && entry.level == Level::Fail
    })
}

/// The checks a release or verify reported (`artifacts.json` `checks`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tally {
    /// PASS checks.
    pub pass: u32,
    /// WARN checks.
    pub warn: u32,
    /// FAIL checks (owner items under `--sign auto` included).
    pub fail: u32,
    /// The ids that warned.
    #[serde(default)]
    pub ids_warn: Vec<String>,
    /// The ids that failed.
    #[serde(default)]
    pub ids_fail: Vec<String>,
}

/// What a release or verify has checked so far.
#[derive(Clone, Debug)]
pub struct Gates {
    /// `--sign auto` or `--sign none` (for verify: the release's).
    pub mode: SignMode,
    /// What was reported.
    pub tally: Tally,
    blockers: Vec<IcmError>,
    later: Vec<IcmError>,
}

impl Gates {
    /// No checks yet.
    pub fn new(mode: SignMode) -> Gates {
        Gates {
            mode,
            tally: Tally::default(),
            blockers: Vec::new(),
            later: Vec::new(),
        }
    }

    fn count(&mut self, ctx: &Ctx, check: &Check) {
        let id = check.id().to_string();
        let strict = ctx.rep.mode().strict;
        match check.status {
            Status::Pass => self.tally.pass += 1,
            Status::Warn if !strict => {
                self.tally.warn += 1;
                if !self.tally.ids_warn.contains(&id) {
                    self.tally.ids_warn.push(id);
                }
            }
            Status::Fail | Status::Warn => {
                self.tally.fail += 1;
                if !self.tally.ids_fail.contains(&id) {
                    self.tally.ids_fail.push(id);
                }
            }
            Status::Skip | Status::Info => {}
        }
    }

    /// Under `--sign none`, the WARN an owner-dependent failure becomes.
    fn unsigned_warn(mut check: Check) -> Check {
        check.status = Status::Warn;
        check.error.detail = format!(
            "{} (a WARN under --sign none: the artifacts are unsigned and not uploadable)",
            check.error.detail
        );
        check
    }

    /// Reports a gate's result (see the module docs).
    pub fn check(&mut self, ctx: &Ctx, check: Check) {
        if check.status == Status::Fail && owner_dependent(check.id()) {
            match self.mode {
                SignMode::None => {
                    let warn = Self::unsigned_warn(check);
                    self.count(ctx, &warn);
                    ctx.rep.check(warn);
                }
                SignMode::Auto => {
                    self.count(ctx, &check);
                    let error = check.error.clone().exit(Exit::NeedsOwner);
                    ctx.rep.check(check);
                    self.later.push(error);
                }
            }
            return;
        }
        self.count(ctx, &check);
        ctx.rep.check(check);
    }

    /// An owner-dependent precondition: a WARN under `--sign none`; under
    /// `--sign auto` a FAIL that blocks at the next [`Gates::checkpoint`].
    pub fn needs_owner(&mut self, ctx: &Ctx, error: IcmError) {
        self.owner(ctx, error, true);
    }

    /// Like [`Gates::needs_owner`], but the release still writes its
    /// (unsigned) artifacts first; the command ends with exit 9 at
    /// [`Gates::finish`].
    pub fn needs_owner_later(&mut self, ctx: &Ctx, error: IcmError) {
        self.owner(ctx, error, false);
    }

    fn owner(&mut self, ctx: &Ctx, error: IcmError, now: bool) {
        let check = Check::from_error(error, Status::Fail);
        match self.mode {
            SignMode::None => {
                let warn = Self::unsigned_warn(check);
                self.count(ctx, &warn);
                ctx.rep.check(warn);
            }
            SignMode::Auto => {
                self.count(ctx, &check);
                let error = check.error.clone().exit(Exit::NeedsOwner);
                ctx.rep.check(check);
                if now {
                    self.blockers.push(error);
                } else {
                    self.later.push(error);
                }
            }
        }
    }

    /// The first owner item that must block now (exit 9), if any.
    pub fn checkpoint(&self) -> Result<()> {
        match self.blockers.first() {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    /// The first owner item of all (exit 9), if any: the end of a release
    /// whose artifacts are written but need the owner.
    pub fn finish(&self) -> Result<()> {
        match self.blockers.iter().chain(&self.later).next() {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    /// Every owner item so far.
    pub fn owner_items(&self) -> impl Iterator<Item = &IcmError> {
        self.blockers.iter().chain(&self.later)
    }

    /// Whether anything failed or needs the owner.
    pub fn clean(&self) -> bool {
        self.tally.fail == 0 && self.blockers.is_empty() && self.later.is_empty()
    }

    /// The owner items as the result's `owner_steps` entries.
    pub fn owner_steps(&self) -> Vec<Value> {
        self.owner_items()
            .map(|error| {
                json!({
                    "kind": "fix",
                    "id": error.id,
                    "title": error.title,
                    "detail": error.detail,
                    "commands": error.fix.commands,
                    "summary": error.fix.summary,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::{Mode, Reporter, RunInfo};

    fn ctx(strict: bool) -> Ctx {
        let rep = Reporter::with_writers(
            Mode {
                json: true,
                strict,
                ..Mode::default()
            },
            RunInfo {
                run: "20261007T000000Z-release-ios-0000".into(),
                command: "release".into(),
                target: Some("ios".into()),
                argv: vec![],
                save: false,
            },
            Box::new(std::io::sink()),
            Box::new(std::io::sink()),
        );
        Ctx::new(crate::cli::GlobalArgs::default(), rep, vec![])
    }

    #[test]
    fn owner_dependence_follows_the_catalogue() {
        // A WARN-level placeholder: the release asks the owner explicitly.
        assert!(!owner_dependent("app.id.placeholder"));
        assert!(owner_dependent("config.owner_decision"));
        assert!(owner_dependent("ios.sign.no_profile"));
        assert!(owner_dependent("android.keystore.password_env_unset"));
        assert!(owner_dependent("windows.sign.not_configured"));
        // WARN-level owner ids and agent ids are not.
        assert!(!owner_dependent("ios.export_compliance.documentation"));
        assert!(!owner_dependent("ios.ipa.layout"));
        assert!(!owner_dependent("version.build_not_increased"));
    }

    #[test]
    fn unsigned_releases_turn_owner_items_into_warnings() {
        let ctx = ctx(false);
        let mut gates = Gates::new(SignMode::None);
        gates.needs_owner(
            &ctx,
            IcmError::new(CheckId::ConfigOwnerDecision, "[ios] team_id is unset"),
        );
        gates.check(
            &ctx,
            Check::fail(CheckId::IosSignNoProfile, "no App Store profile"),
        );
        gates.check(&ctx, Check::fail(CheckId::IosIpaLayout, "a ._ entry"));
        gates.check(&ctx, Check::pass(CheckId::IosPlistSceneManifest, "present"));
        assert!(gates.checkpoint().is_ok());
        assert!(gates.finish().is_ok());
        assert_eq!(gates.tally.warn, 2);
        assert_eq!(gates.tally.fail, 1);
        assert_eq!(gates.tally.pass, 1);
        assert_eq!(
            gates.tally.ids_warn,
            ["config.owner_decision", "ios.sign.no_profile"]
        );
        assert_eq!(gates.tally.ids_fail, ["ios.ipa.layout"]);
        assert!(!gates.clean());
    }

    #[test]
    fn signed_releases_block_on_owner_items() {
        let ctx = ctx(false);
        let mut gates = Gates::new(SignMode::Auto);
        gates.needs_owner_later(
            &ctx,
            IcmError::new(
                CheckId::AndroidKeystorePasswordEnvUnset,
                "ICM_STORE_PASS is not set",
            ),
        );
        assert!(
            gates.checkpoint().is_ok(),
            "deferred items do not block early"
        );
        gates.needs_owner(
            &ctx,
            IcmError::new(CheckId::ConfigOwnerDecision, "[ios] team_id is unset"),
        );
        let now = gates.checkpoint().unwrap_err();
        assert_eq!(now.id, "config.owner_decision");
        assert_eq!(now.exit, Exit::NeedsOwner);
        // At the end, the earlier blocker still comes first.
        assert_eq!(gates.finish().unwrap_err().id, "config.owner_decision");
        let steps = gates.owner_steps();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[1]["id"], "android.keystore.password_env_unset");
        assert_eq!(gates.tally.fail, 2);
    }

    #[test]
    fn strict_counts_warnings_as_failures() {
        let ctx = ctx(true);
        let mut gates = Gates::new(SignMode::Auto);
        gates.check(&ctx, Check::warn(CheckId::EnvPolicyStale, "old"));
        assert_eq!(gates.tally.fail, 1);
        assert_eq!(gates.tally.ids_fail, ["env.policy_stale"]);
    }
}
