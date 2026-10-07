//! `codesign` for device bundles (design §10.5 step 5, §11.1 steps 7-8):
//! sign last under a keychain-prompt watchdog, verify, and read back what
//! was signed.

use crate::catalogue::CheckId;
use crate::context::Ctx;
use crate::error::{Evidence, IcmError, Result};
use crate::process::{Cmd, Outcome};
use serde_json::{Map, Value};
use std::path::Path;
use std::time::Duration;

/// How long codesign may take before icm assumes it waits for a keychain
/// dialog nobody will answer (stdin is closed, so it cannot prompt in the
/// terminal).
pub const WATCHDOG: Duration = Duration::from_secs(60);

/// [`WATCHDOG`], or `ICM_CODESIGN_TIMEOUT` seconds (a slow CI host, icm's
/// own tests of a hanging codesign).
pub fn watchdog() -> Duration {
    std::env::var("ICM_CODESIGN_TIMEOUT")
        .ok()
        .and_then(|secs| secs.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map_or(WATCHDOG, Duration::from_secs)
}

/// The ad-hoc identity (`--sign -`): `icm release ios --sign none`.
pub const AD_HOC: &str = "-";

/// `codesign --force --sign <identity> [--entitlements <plist>]
/// --timestamp=none --generate-entitlement-der [--keychain <kc>] <App.app>`.
pub fn sign_cmd(
    app: &Path,
    identity: &str,
    entitlements: Option<&Path>,
    keychain: Option<&Path>,
) -> Cmd {
    let mut cmd = Cmd::tool("codesign").args(["--force", "--sign", identity]);
    if let Some(entitlements) = entitlements {
        cmd = cmd
            .arg("--entitlements")
            .arg(entitlements)
            .arg("--generate-entitlement-der");
    }
    cmd = cmd.arg("--timestamp=none");
    if let Some(keychain) = keychain {
        cmd = cmd.arg("--keychain").arg(keychain);
    }
    cmd.arg(app).timeout(watchdog())
}

/// `xattr -cr <App.app>`: extended attributes would make codesign refuse
/// the bundle ("detritus") or end up as AppleDouble files.
pub fn clear_xattrs(ctx: &Ctx, app: &Path) -> Result<()> {
    let outcome = ctx.step(
        "ios.xattr",
        &Cmd::tool("xattr")
            .arg("-cr")
            .arg(app)
            .timeout(Duration::from_secs(60)),
    )?;
    if !outcome.success() {
        return Err(ctx.step_failure("ios.xattr", CheckId::ToolFailed, &outcome));
    }
    Ok(())
}

/// Signs the bundle. A codesign that outlives the watchdog is waiting for
/// keychain access: `ios.sign.keychain_prompt` (exit 9).
pub fn sign(
    ctx: &Ctx,
    app: &Path,
    identity: &str,
    entitlements: Option<&Path>,
    keychain: Option<&Path>,
) -> Result<()> {
    let cmd = sign_cmd(app, identity, entitlements, keychain);
    let outcome = match ctx.step("ios.codesign", &cmd) {
        Ok(outcome) => outcome,
        Err(error) if error.id == CheckId::StepTimeout.id() && identity != AD_HOC => {
            let mut prompt = IcmError::new(
                CheckId::IosSignKeychainPrompt,
                format!(
                    "codesign did not finish within {}s: it is most likely waiting for permission to use the private key of {identity}",
                    watchdog().as_secs()
                ),
            )
            .fix(
                "The owner runs the release once in a terminal and answers the keychain dialog with \"Always Allow\"; for a CI keychain: security set-key-partition-list -S apple-tool:,apple: -s -k <password> <keychain>.",
                &[],
            );
            prompt.evidence = error.evidence;
            return Err(prompt);
        }
        Err(error) => return Err(error),
    };
    if !outcome.success() {
        let text = outcome.stderr_text();
        let id = if text.contains("no identity found") || text.contains("ambiguous") {
            CheckId::IosSignNoIdentity
        } else {
            CheckId::IosSignVerify
        };
        return Err(ctx.step_failure("ios.codesign", id, &outcome));
    }
    Ok(())
}

/// `codesign --verify --strict --deep -vv <App.app>`.
pub fn verify_cmd(app: &Path) -> Cmd {
    Cmd::tool("codesign")
        .args(["--verify", "--strict", "--deep", "-vv"])
        .arg(app)
        .timeout(Duration::from_secs(120))
}

/// Runs the verification as a step; the outcome is the caller's to judge.
pub fn verify(ctx: &Ctx, step: &str, app: &Path) -> Result<Outcome> {
    ctx.step(step, &verify_cmd(app))
}

/// What `codesign -dvv` says about a signature.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Signature {
    /// `Authority=` lines, leaf first (none for an ad-hoc signature).
    pub authorities: Vec<String>,
    /// `TeamIdentifier=` (`not set` is `None`).
    pub team: Option<String>,
    /// `Signature=adhoc`.
    pub ad_hoc: bool,
    /// `Identifier=`.
    pub identifier: Option<String>,
}

/// Parses `codesign -dvv` output (it goes to stderr).
pub fn parse_signature(text: &str) -> Signature {
    let mut signature = Signature::default();
    for line in text.lines() {
        if let Some(authority) = line.strip_prefix("Authority=") {
            signature.authorities.push(authority.trim().to_string());
        } else if let Some(team) = line.strip_prefix("TeamIdentifier=") {
            let team = team.trim();
            if team != "not set" {
                signature.team = Some(team.to_string());
            }
        } else if line.trim() == "Signature=adhoc" {
            signature.ad_hoc = true;
        } else if let Some(identifier) = line.strip_prefix("Identifier=") {
            signature.identifier = Some(identifier.trim().to_string());
        }
    }
    signature
}

/// Reads a bundle's signature.
pub fn signature(ctx: &Ctx, app: &Path) -> Result<Signature> {
    let outcome = ctx.probe(
        &Cmd::tool("codesign")
            .arg("-dvv")
            .arg(app)
            .timeout(Duration::from_secs(60)),
    )?;
    if !outcome.success() {
        return Err(IcmError::new(
            CheckId::IosSignVerify,
            format!(
                "codesign -dvv {}: {}",
                crate::paths::display(app),
                outcome.stderr_tail(3)
            ),
        )
        .evidence(Evidence::file(app)));
    }
    Ok(parse_signature(&format!(
        "{}{}",
        outcome.stdout_text(),
        outcome.stderr_text()
    )))
}

/// The entitlements a bundle is signed with (`codesign -d --entitlements -
/// --xml`), or `None` when it has none.
pub fn entitlements(ctx: &Ctx, app: &Path) -> Result<Option<Map<String, Value>>> {
    let outcome = ctx.probe(
        &Cmd::tool("codesign")
            .args(["-d", "--entitlements", "-", "--xml"])
            .arg(app)
            .timeout(Duration::from_secs(60)),
    )?;
    if !outcome.success() {
        return Err(IcmError::new(
            CheckId::IosSignVerify,
            format!(
                "codesign cannot read the entitlements of {}: {}",
                crate::paths::display(app),
                outcome.stderr_tail(3)
            ),
        ));
    }
    let text = outcome.stdout_text();
    if !text.contains("<plist") {
        return Ok(None);
    }
    let start = text
        .find("<?xml")
        .or_else(|| text.find("<plist"))
        .unwrap_or(0);
    super::plist_xml::parse(&text[start..])
        .map(|value| value.as_object().cloned())
        .map_err(|error| {
            IcmError::new(
                CheckId::IosSignVerify,
                format!("the signed entitlements are not a plist: {error}"),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sign_command_carries_the_references() {
        let cmd = sign_cmd(
            Path::new("/b/App.app"),
            "0123456789ABCDEF0123456789ABCDEF01234567",
            Some(Path::new("/g/entitlements.plist")),
            Some(Path::new("/k/ci.keychain-db")),
        );
        assert_eq!(
            cmd.display_argv()[1..].join(" "),
            "--force --sign 0123456789ABCDEF0123456789ABCDEF01234567 --entitlements /g/entitlements.plist --generate-entitlement-der --timestamp=none --keychain /k/ci.keychain-db /b/App.app"
        );
        assert_eq!(cmd.timeout, Some(watchdog()));
        let ad_hoc = sign_cmd(Path::new("/b/App.app"), AD_HOC, None, None);
        assert_eq!(
            ad_hoc.display_argv()[1..].join(" "),
            "--force --sign - --timestamp=none /b/App.app"
        );
    }

    #[test]
    fn signatures_are_read() {
        let text = "Executable=/x/App.app/app\nIdentifier=com.acme.notes\nFormat=app bundle with Mach-O thin (arm64)\nAuthority=Apple Distribution: Acme Ltd (ABCDE12345)\nAuthority=Apple Worldwide Developer Relations Certification Authority\nAuthority=Apple Root CA\nTeamIdentifier=ABCDE12345\n";
        let signature = parse_signature(text);
        assert_eq!(signature.authorities.len(), 3);
        assert_eq!(signature.team.as_deref(), Some("ABCDE12345"));
        assert!(!signature.ad_hoc);
        let adhoc = parse_signature("Identifier=app\nSignature=adhoc\nTeamIdentifier=not set\n");
        assert!(adhoc.ad_hoc);
        assert_eq!(adhoc.team, None);
        assert!(adhoc.authorities.is_empty());
    }
}
