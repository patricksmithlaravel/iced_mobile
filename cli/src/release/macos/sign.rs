//! Signing a macOS release (design §11.4 step 3, §12.4): which identity
//! signs, the codesign command lines, and what codesign, spctl and stapler
//! say afterwards.
//!
//! **The identity.** `[desktop.macos] identity` is `auto`, a SHA-1 or a
//! name. icm lists the code-signing identities with `security
//! find-identity -p codesigning` (reading certificates only, never a
//! private key, so it never prompts), in host.toml `signing_keychain` /
//! `ICM_KEYCHAIN` when set and otherwise in the user's search list:
//!
//! - `auto` takes a *valid* (trusted) `Developer ID Application:` identity:
//!   none is `macos.sign.no_developer_id`, several of different names
//!   `macos.sign.identity_ambiguous` (both the owner's, exit 9);
//! - a SHA-1 or a name takes that identity even when it is not a Developer
//!   ID (an untrusted, self-signed one included): the app is signed with
//!   it, without a secure timestamp, and the release ends with exit 9
//!   `macos.sign.no_developer_id` once it is written, since Apple's notary
//!   service refuses it. That is how a throwaway identity in a test
//!   keychain exercises the signed path.
//!
//! codesign always gets the identity's SHA-1 (never a name, which can
//! match several certificates) and `--keychain` when one is set, so a CI
//! or test keychain never has to join the user's search list. `--sign
//! none` signs ad hoc (`--sign -`): Apple silicon runs only signed code.

use crate::catalogue::CheckId;
use crate::error::IcmError;
use crate::process::Cmd;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long codesign may take before icm assumes it waits for keychain
/// access (`macos.sign.keychain_prompt`).
pub const CODESIGN_TIMEOUT: Duration = Duration::from_secs(120);

/// The prefix of the certificates Apple's notary service accepts.
pub const DEVELOPER_ID: &str = "Developer ID Application:";

/// A code-signing identity `security find-identity` lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    /// The certificate's SHA-1, upper case.
    pub sha1: String,
    /// Its common name.
    pub name: String,
    /// Whether the system trusts it (listed under "Valid identities only").
    pub valid: bool,
    /// Why it is not valid (`CSSMERR_TP_NOT_TRUSTED`, `..._CERT_EXPIRED`).
    pub problem: Option<String>,
}

impl Identity {
    /// A trusted Developer ID Application identity: what notarization needs.
    pub fn is_developer_id(&self) -> bool {
        self.valid && self.name.starts_with(DEVELOPER_ID)
    }

    /// Expired or revoked: codesign may still sign, but nothing accepts it.
    pub fn is_dead(&self) -> bool {
        self.problem
            .as_deref()
            .is_some_and(|p| p.contains("EXPIRED") || p.contains("REVOKED"))
    }
}

/// `security find-identity -p codesigning [<keychain>]`.
pub fn find_identity_cmd(keychain: Option<&Path>) -> Cmd {
    let mut cmd = Cmd::tool("security")
        .args(["find-identity", "-p", "codesigning"])
        .timeout(Duration::from_secs(30));
    if let Some(keychain) = keychain {
        cmd = cmd.arg(keychain);
    }
    cmd
}

/// Parses `security find-identity` output: the "Matching identities"
/// (all), marked valid when they also appear under "Valid identities
/// only". Output with only the second section (`-v`) is all valid.
pub fn parse_identities(text: &str) -> Vec<Identity> {
    let mut all: Vec<Identity> = Vec::new();
    let mut valid: Vec<String> = Vec::new();
    let mut section = "";
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Matching identities") {
            section = "matching";
            continue;
        }
        if trimmed.starts_with("Valid identities only") {
            section = "valid";
            continue;
        }
        let Some((number, rest)) = trimmed.split_once(") ") else {
            continue;
        };
        if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let Some((sha1, rest)) = rest.split_once(' ') else {
            continue;
        };
        if sha1.len() != 40 || !sha1.chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        let rest = rest.trim();
        let Some(rest) = rest.strip_prefix('"') else {
            continue;
        };
        let Some(end) = rest.rfind('"') else {
            continue;
        };
        let name = rest[..end].to_string();
        let problem = rest[end + 1..]
            .trim()
            .trim_start_matches('(')
            .trim_end_matches(')')
            .trim()
            .to_string();
        let sha1 = sha1.to_ascii_uppercase();
        match section {
            "valid" => valid.push(sha1.clone()),
            _ => {
                if !all.iter().any(|identity| identity.sha1 == sha1) {
                    all.push(Identity {
                        sha1: sha1.clone(),
                        name,
                        valid: false,
                        problem: (!problem.is_empty()).then_some(problem),
                    });
                }
                continue;
            }
        }
        if !all.iter().any(|identity| identity.sha1 == sha1) {
            all.push(Identity {
                sha1,
                name,
                valid: true,
                problem: None,
            });
        }
    }
    let sectioned = text.contains("Matching identities");
    for identity in &mut all {
        if !sectioned || valid.contains(&identity.sha1) {
            identity.valid = true;
        }
    }
    all
}

/// Where the identities were looked for, for messages.
pub fn searched(keychain: Option<&Path>) -> String {
    match keychain {
        Some(keychain) => format!("the keychain {}", crate::paths::display(keychain)),
        None => "the user's keychain search list".to_string(),
    }
}

fn no_developer_id(detail: String, keychain: Option<&Path>) -> IcmError {
    IcmError::new(CheckId::MacosSignNoDeveloperId, detail).fix(
        format!(
            "The owner creates a Developer ID Application certificate (Apple Developer > Certificates, Identifiers & Profiles; the Account Holder's role is needed), installs it with its private key in {}, and leaves [desktop.macos] identity = \"auto\" or names it. `icm release macos --sign none` builds an ad-hoc-signed app meanwhile.",
            searched(keychain)
        ),
        &["security find-identity -v -p codesigning"],
    )
}

/// Picks the identity `[desktop.macos] identity` names (see the module
/// docs); the error is the owner's (exit 9).
pub fn choose(
    configured: &str,
    identities: &[Identity],
    keychain: Option<&Path>,
) -> Result<Identity, IcmError> {
    let configured = configured.trim();
    if configured.is_empty() || configured == "auto" {
        let candidates: Vec<&Identity> =
            identities.iter().filter(|i| i.is_developer_id()).collect();
        let Some(first) = candidates.first() else {
            let others: Vec<String> = identities
                .iter()
                .map(|i| {
                    format!(
                        "\"{}\"{}",
                        i.name,
                        i.problem
                            .as_deref()
                            .map(|p| format!(" ({p})"))
                            .unwrap_or_default()
                    )
                })
                .collect();
            return Err(no_developer_id(
                format!(
                    "no valid Developer ID Application identity is in {}{}",
                    searched(keychain),
                    if others.is_empty() {
                        String::new()
                    } else {
                        format!(
                            " (it has {}, which Apple's notary service does not accept)",
                            others.join(", ")
                        )
                    }
                ),
                keychain,
            ));
        };
        let mut names: Vec<&str> = candidates.iter().map(|i| i.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        if names.len() > 1 {
            return Err(IcmError::new(
                CheckId::MacosSignIdentityAmbiguous,
                format!(
                    "{} Developer ID Application identities are in {}: {}",
                    names.len(),
                    searched(keychain),
                    names.join(", ")
                ),
            )
            .fix(
                "The owner sets [desktop.macos] identity to the SHA-1 or the full name of the one to sign with.",
                &["security find-identity -v -p codesigning"],
            ));
        }
        return Ok((*first).clone());
    }

    let is_sha1 = configured.len() == 40 && configured.chars().all(|c| c.is_ascii_hexdigit());
    let matches: Vec<&Identity> = if is_sha1 {
        identities
            .iter()
            .filter(|i| i.sha1.eq_ignore_ascii_case(configured))
            .collect()
    } else {
        let exact: Vec<&Identity> = identities.iter().filter(|i| i.name == configured).collect();
        if exact.is_empty() {
            identities
                .iter()
                .filter(|i| i.name.contains(configured))
                .collect()
        } else {
            exact
        }
    };
    // Prefer a usable one when a renewed certificate shares the name.
    let usable: Vec<&Identity> = matches.iter().copied().filter(|i| !i.is_dead()).collect();
    match (usable.as_slice(), matches.first()) {
        ([one], _) => Ok((*one).clone()),
        ([first, rest @ ..], _) => {
            if rest.iter().all(|i| i.name == first.name) {
                Ok((*usable.iter().find(|i| i.valid).unwrap_or(first)).clone())
            } else {
                Err(IcmError::new(
                    CheckId::MacosSignIdentityAmbiguous,
                    format!(
                        "[desktop.macos] identity \"{configured}\" matches {} identities in {}",
                        usable.len(),
                        searched(keychain)
                    ),
                )
                .fix(
                    "The owner sets [desktop.macos] identity to the certificate's SHA-1.",
                    &["security find-identity -v -p codesigning"],
                ))
            }
        }
        ([], Some(dead)) => Err(no_developer_id(
            format!(
                "[desktop.macos] identity \"{configured}\" is \"{}\", which is {}",
                dead.name,
                dead.problem.as_deref().unwrap_or("not usable")
            ),
            keychain,
        )),
        ([], None) => Err(no_developer_id(
            format!(
                "[desktop.macos] identity \"{configured}\" is not in {}",
                searched(keychain)
            ),
            keychain,
        )),
    }
}

/// How codesign signs: an identity (with or without a secure timestamp),
/// or ad hoc.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signer {
    /// The identity; `None` signs ad hoc.
    pub identity: Option<Identity>,
    /// `--keychain`.
    pub keychain: Option<PathBuf>,
}

impl Signer {
    /// Ad hoc (`--sign none`).
    pub fn ad_hoc() -> Signer {
        Signer {
            identity: None,
            keychain: None,
        }
    }

    /// Whether Apple's notary service can accept what it signs.
    pub fn developer_id(&self) -> bool {
        self.identity
            .as_ref()
            .is_some_and(Identity::is_developer_id)
    }

    /// What `artifacts.json` records under `signing`.
    pub fn json(&self) -> serde_json::Value {
        match &self.identity {
            None => serde_json::json!({"identity": "-", "ad_hoc": true}),
            Some(identity) => serde_json::json!({
                "identity": identity.name,
                "identity_sha1": identity.sha1,
                "developer_id": self.developer_id(),
                "keychain": self.keychain.as_deref().map(|k| k.display().to_string()),
                "timestamp": self.developer_id(),
            }),
        }
    }

    /// `codesign --force [--options runtime] ... --sign <sha1|-> <path>`.
    /// A Developer ID signs with a secure timestamp (notarization requires
    /// it); any other identity without one, so icm never asks Apple's
    /// timestamp service to vouch for a certificate it does not issue.
    pub fn codesign(&self, path: &Path, runtime: bool, entitlements: Option<&Path>) -> Cmd {
        let mut cmd = Cmd::tool("codesign")
            .arg("--force")
            .timeout(CODESIGN_TIMEOUT);
        if runtime {
            cmd = cmd.args(["--options", "runtime"]);
        }
        match &self.identity {
            Some(identity) => {
                cmd = cmd.arg(if self.developer_id() {
                    "--timestamp"
                } else {
                    "--timestamp=none"
                });
                if let Some(entitlements) = entitlements {
                    cmd = cmd.arg("--entitlements").arg(entitlements);
                }
                if let Some(keychain) = &self.keychain {
                    cmd = cmd.arg("--keychain").arg(keychain);
                }
                cmd = cmd.args(["--sign", &identity.sha1]);
            }
            None => {
                if let Some(entitlements) = entitlements {
                    cmd = cmd.arg("--entitlements").arg(entitlements);
                }
                cmd = cmd.args(["--sign", "-"]);
            }
        }
        cmd.arg(path)
    }
}

/// `codesign --verify --strict --deep -vv <path>` (a disk image: no
/// `--deep`).
pub fn verify_cmd(path: &Path) -> Cmd {
    let mut cmd = Cmd::tool("codesign").args(["--verify", "--strict"]);
    if path.extension().is_none_or(|ext| ext != "dmg") {
        cmd = cmd.arg("--deep");
    }
    cmd.arg("-vv").arg(path).timeout(Duration::from_secs(300))
}

/// `codesign -d -vvv <path>` (it writes to stderr; `-vvv` adds the
/// `CDHash`).
pub fn display_cmd(path: &Path) -> Cmd {
    Cmd::tool("codesign")
        .args(["-d", "-vvv"])
        .arg(path)
        .timeout(Duration::from_secs(60))
}

/// What `codesign -d -vvv` says about a signature.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Display {
    /// The CodeDirectory flags (`runtime`, `adhoc`, ...).
    pub flags: Vec<String>,
    /// The certificate chain, leaf first.
    pub authority: Vec<String>,
    /// `TeamIdentifier`, unless "not set".
    pub team: Option<String>,
    /// `Signature=adhoc`.
    pub ad_hoc: bool,
    /// A secure timestamp (`Timestamp=`; `Signed Time=` is the signer's
    /// clock).
    pub timestamp: Option<String>,
    /// `Identifier=`.
    pub identifier: Option<String>,
    /// `CDHash=`: the code directory hash, which names the signature (a
    /// notarization ticket is issued for it, and stapling leaves it).
    pub cdhash: Option<String>,
}

impl Display {
    /// Whether the hardened runtime is on.
    pub fn hardened(&self) -> bool {
        self.flags.iter().any(|flag| flag == "runtime")
    }
}

/// Parses `codesign -d -vvv` output.
pub fn parse_display(text: &str) -> Display {
    let mut display = Display::default();
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("CodeDirectory ")
            && let Some(flags) = rest
                .split_whitespace()
                .find_map(|w| w.strip_prefix("flags="))
            && let (Some(open), Some(close)) = (flags.find('('), flags.rfind(')'))
        {
            display.flags = flags[open + 1..close]
                .split(',')
                .map(|flag| flag.trim().to_string())
                .filter(|flag| !flag.is_empty() && flag != "none")
                .collect();
        } else if let Some(authority) = line.strip_prefix("Authority=") {
            display.authority.push(authority.to_string());
        } else if let Some(team) = line.strip_prefix("TeamIdentifier=") {
            display.team = (team != "not set").then(|| team.to_string());
        } else if line == "Signature=adhoc" {
            display.ad_hoc = true;
        } else if let Some(time) = line.strip_prefix("Timestamp=") {
            display.timestamp = Some(time.to_string());
        } else if let Some(identifier) = line.strip_prefix("Identifier=") {
            display.identifier = Some(identifier.to_string());
        } else if let Some(cdhash) = line.strip_prefix("CDHash=") {
            display.cdhash = Some(cdhash.trim().to_ascii_lowercase());
        }
    }
    display
}

/// `spctl -a -vvv -t exec <app>`, or for a disk image `spctl -a -t open
/// --context context:primary-signature -vv <dmg>`.
pub fn spctl_cmd(path: &Path) -> Cmd {
    let cmd = Cmd::tool("spctl").arg("-a");
    let cmd = if path.extension().is_some_and(|ext| ext == "dmg") {
        cmd.args([
            "-t",
            "open",
            "--context",
            "context:primary-signature",
            "-vv",
        ])
    } else {
        cmd.args(["-vvv", "-t", "exec"])
    };
    cmd.arg(path).timeout(Duration::from_secs(120))
}

/// What Gatekeeper decided, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assessment {
    /// Accepted.
    pub accepted: bool,
    /// `source=` (`Notarized Developer ID`, `Unnotarized Developer ID`).
    pub source: Option<String>,
    /// `origin=` (the signing certificate).
    pub origin: Option<String>,
    /// What it means for the owner.
    pub explanation: String,
}

/// Explains spctl's output (stdout and stderr together).
pub fn assess(text: &str, success: bool) -> Assessment {
    let field = |key: &str| {
        text.lines()
            .find_map(|line| line.trim().strip_prefix(key))
            .map(str::to_string)
    };
    let source = field("source=");
    let origin = field("origin=");
    let accepted = success && text.contains(": accepted");
    let explanation = match (accepted, source.as_deref(), origin.as_deref()) {
        (true, Some(source), _) if source.contains("Notarized") => {
            "Gatekeeper accepts it as notarized: it opens on any Mac without a warning".to_string()
        }
        (true, Some(source), _) => format!("Gatekeeper accepts it ({source})"),
        (true, None, _) => "Gatekeeper accepts it".to_string(),
        (false, Some(source), _) if source.contains("Unnotarized") => {
            "Gatekeeper rejects it until it is notarized and stapled (the owner's steps in UPLOAD.md); that is expected before notarization".to_string()
        }
        (false, _, Some(origin)) => format!(
            "Gatekeeper rejects it: \"{origin}\" is not a Developer ID that Gatekeeper trusts, so other Macs refuse to open it"
        ),
        (false, Some(source), None) => format!("Gatekeeper rejects it ({source})"),
        (false, None, None) if text.contains("no usable signature") => {
            "Gatekeeper rejects it: it has no usable signature".to_string()
        }
        (false, None, None) => "Gatekeeper rejects it: it is signed ad hoc (or not at all), so other Macs refuse to open it without the user overriding Gatekeeper".to_string(),
    };
    Assessment {
        accepted,
        source,
        origin,
        explanation,
    }
}

/// `xcrun stapler validate <path>`: whether a notarization ticket is
/// stapled to it.
pub fn stapler_validate_cmd(path: &Path) -> Cmd {
    Cmd::tool("xcrun")
        .args(["stapler", "validate"])
        .arg(path)
        .timeout(Duration::from_secs(120))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTING: &str = r#"
Policy: Code Signing
  Matching identities
  1) 5A34F2F61B0C9D234A0EC83B5A8E0909516DA0A6 "icm-test Code Signing" (CSSMERR_TP_NOT_TRUSTED)
  2) 1111111111111111111111111111111111111111 "Developer ID Application: Acme Ltd (ABCDE12345)"
  3) 2222222222222222222222222222222222222222 "Apple Distribution: Acme Ltd (ABCDE12345)"
  4) 3333333333333333333333333333333333333333 "Developer ID Application: Old Ltd (ZZZZZ99999)" (CSSMERR_TP_CERT_EXPIRED)
     4 identities found

  Valid identities only
  1) 1111111111111111111111111111111111111111 "Developer ID Application: Acme Ltd (ABCDE12345)"
  2) 2222222222222222222222222222222222222222 "Apple Distribution: Acme Ltd (ABCDE12345)"
     2 valid identities found
"#;

    #[test]
    fn identities_are_parsed_with_their_validity() {
        let identities = parse_identities(LISTING);
        assert_eq!(identities.len(), 4);
        assert_eq!(identities[0].name, "icm-test Code Signing");
        assert!(!identities[0].valid);
        assert_eq!(
            identities[0].problem.as_deref(),
            Some("CSSMERR_TP_NOT_TRUSTED")
        );
        assert!(identities[1].valid && identities[1].is_developer_id());
        assert!(identities[2].valid && !identities[2].is_developer_id());
        assert!(identities[3].is_dead() && !identities[3].is_developer_id());
        // `-v` output: everything listed is valid.
        let only_valid = parse_identities(
            "  1) 1111111111111111111111111111111111111111 \"Developer ID Application: Acme Ltd (ABCDE12345)\"\n     1 valid identities found\n",
        );
        assert!(only_valid[0].valid);
        assert!(parse_identities("     0 valid identities found\n").is_empty());
    }

    #[test]
    fn auto_takes_the_one_developer_id() {
        let identities = parse_identities(LISTING);
        let chosen = choose("auto", &identities, None).unwrap();
        assert_eq!(chosen.sha1, "1111111111111111111111111111111111111111");
        // None: the owner's, naming what the keychain has instead.
        let error =
            choose("auto", &identities[..1], Some(Path::new("/k.keychain-db"))).unwrap_err();
        assert_eq!(error.id, "macos.sign.no_developer_id");
        assert!(
            error.detail.contains("icm-test Code Signing"),
            "{}",
            error.detail
        );
        assert!(error.detail.contains("/k.keychain-db"), "{}", error.detail);
        // Two teams: the owner picks.
        let mut two = identities.clone();
        two.push(Identity {
            sha1: "4".repeat(40),
            name: "Developer ID Application: Other (QQQQQ11111)".into(),
            valid: true,
            problem: None,
        });
        assert_eq!(
            choose("auto", &two, None).unwrap_err().id,
            "macos.sign.identity_ambiguous"
        );
    }

    #[test]
    fn a_named_identity_is_used_even_when_untrusted() {
        let identities = parse_identities(LISTING);
        let by_sha = choose(
            "5a34f2f61b0c9d234a0ec83b5a8e0909516da0a6",
            &identities,
            None,
        )
        .unwrap();
        assert_eq!(by_sha.name, "icm-test Code Signing");
        let by_name = choose("icm-test Code Signing", &identities, None).unwrap();
        assert_eq!(by_name.sha1, by_sha.sha1);
        let by_part = choose("Acme Ltd (ABCDE12345)", &identities, None);
        assert_eq!(
            by_part.unwrap_err().id,
            "macos.sign.identity_ambiguous",
            "a Developer ID and an Apple Distribution identity share it"
        );
        let expired = choose("Developer ID Application: Old Ltd", &identities, None).unwrap_err();
        assert_eq!(expired.id, "macos.sign.no_developer_id");
        assert!(expired.detail.contains("EXPIRED"), "{}", expired.detail);
        let missing = choose(&"9".repeat(40), &identities, None).unwrap_err();
        assert!(
            missing
                .detail
                .contains("is not in the user's keychain search list")
        );
    }

    #[test]
    fn codesign_lines_follow_the_identity() {
        let identities = parse_identities(LISTING);
        let app = Path::new("/d/Notes.app");
        let developer = Signer {
            identity: Some(identities[1].clone()),
            keychain: Some(PathBuf::from("/k.keychain-db")),
        };
        assert_eq!(
            developer
                .codesign(app, true, Some(Path::new("/g/entitlements.plist")))
                .display(),
            "codesign --force --options runtime --timestamp --entitlements /g/entitlements.plist --keychain /k.keychain-db --sign 1111111111111111111111111111111111111111 /d/Notes.app"
        );
        let test = Signer {
            identity: Some(identities[0].clone()),
            keychain: None,
        };
        assert!(!test.developer_id());
        assert!(
            test.codesign(app, true, None)
                .display()
                .contains("--timestamp=none")
        );
        assert_eq!(
            Signer::ad_hoc().codesign(app, true, None).display(),
            "codesign --force --options runtime --sign - /d/Notes.app"
        );
        assert_eq!(Signer::ad_hoc().json()["ad_hoc"], true);
        assert_eq!(developer.json()["developer_id"], true);
        assert!(verify_cmd(app).display().contains("--deep"));
        assert!(
            !verify_cmd(Path::new("/d/N.dmg"))
                .display()
                .contains("--deep")
        );
    }

    #[test]
    fn codesign_display_is_read() {
        let display = parse_display(
            "Executable=/d/Notes.app/Contents/MacOS/notes\nIdentifier=com.acme.notes\nFormat=app bundle with Mach-O thin (arm64)\nCodeDirectory v=20500 size=271 flags=0x10000(runtime) hashes=2+3 location=embedded\nCandidateCDHash sha256=45d1613a435c6cb05794cbb585587a5245989943\nCDHash=45D1613A435C6CB05794CBB585587A5245989943\nAuthority=Developer ID Application: Acme Ltd (ABCDE12345)\nAuthority=Developer ID Certification Authority\nAuthority=Apple Root CA\nTimestamp=Oct 7, 2026 at 5:58:33 AM\nTeamIdentifier=ABCDE12345\n",
        );
        assert!(display.hardened());
        assert_eq!(display.authority.len(), 3);
        assert_eq!(display.team.as_deref(), Some("ABCDE12345"));
        assert!(display.timestamp.is_some());
        assert_eq!(
            display.cdhash.as_deref(),
            Some("45d1613a435c6cb05794cbb585587a5245989943")
        );
        let adhoc = parse_display(
            "CodeDirectory v=20500 size=271 flags=0x10002(adhoc,runtime) hashes=2+3\nSignature=adhoc\nTeamIdentifier=not set\n",
        );
        assert!(adhoc.ad_hoc && adhoc.hardened());
        assert_eq!(adhoc.team, None);
        let plain = parse_display("CodeDirectory v=20500 size=271 flags=0x0(none) hashes=2+3\n");
        assert!(!plain.hardened());
        assert!(plain.flags.is_empty());
    }

    #[test]
    fn gatekeeper_answers_are_explained() {
        let notarized = assess(
            "/d/Notes.app: accepted\nsource=Notarized Developer ID\norigin=Developer ID Application: Acme Ltd (ABCDE12345)\n",
            true,
        );
        assert!(notarized.accepted);
        assert!(notarized.explanation.contains("notarized"));
        let waiting = assess(
            "/d/Notes.app: rejected\nsource=Unnotarized Developer ID\norigin=Developer ID Application: Acme Ltd (ABCDE12345)\n",
            false,
        );
        assert!(!waiting.accepted);
        assert!(waiting.explanation.contains("until it is notarized"));
        let test = assess(
            "/d/Notes.app: rejected\norigin=icm-test Code Signing\n",
            false,
        );
        assert!(
            test.explanation
                .contains("\"icm-test Code Signing\" is not a Developer ID")
        );
        let adhoc = assess("/d/Notes.app: rejected\n", false);
        assert!(adhoc.explanation.contains("ad hoc"));
        assert_eq!(
            spctl_cmd(Path::new("/d/N.dmg")).display(),
            "spctl -a -t open --context context:primary-signature -vv /d/N.dmg"
        );
        assert_eq!(
            spctl_cmd(Path::new("/d/N.app")).display(),
            "spctl -a -vvv -t exec /d/N.app"
        );
    }
}
