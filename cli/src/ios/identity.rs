//! Code-signing identities (design §10.5 step 2, §11.1 preconditions):
//! `security find-identity -p codesigning [<keychain>]`, read only. icm
//! never imports, unlocks or trusts anything; the keychain is host.toml
//! `signing_keychain` / `ICM_KEYCHAIN` when set (a CI or test keychain that
//! never joins the user's search list), else the search list.
//!
//! A reference (`[ios.signing] <kind>.identity`) is `auto`, a certificate
//! SHA-1 or a common name. `auto` takes the valid identities of the role
//! (Apple Distribution for releases, Apple Development for device runs); a
//! named identity is taken even when the system does not trust it, and the
//! problem `security` reports is the owner's to fix.

use crate::catalogue::CheckId;
use crate::context::Ctx;
use crate::error::{IcmError, Result};
use crate::process::Cmd;
use std::path::Path;
use std::time::Duration;

/// One identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    /// The certificate's SHA-1, upper case.
    pub sha1: String,
    /// Its common name (`Apple Distribution: Acme Ltd (ABCDE12345)`).
    pub name: String,
    /// What `security` says is wrong with it (`CSSMERR_TP_CERT_EXPIRED`).
    pub problem: Option<String>,
}

/// What an identity signs for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// App Store builds.
    Distribution,
    /// Device runs.
    Development,
}

impl Role {
    /// The name prefixes of the role's certificates.
    pub fn prefixes(self) -> &'static [&'static str] {
        match self {
            Role::Distribution => &["Apple Distribution:", "iPhone Distribution:"],
            Role::Development => &["Apple Development:", "iPhone Developer:"],
        }
    }

    /// `Apple Distribution` or `Apple Development`.
    pub fn label(self) -> &'static str {
        match self {
            Role::Distribution => "Apple Distribution",
            Role::Development => "Apple Development",
        }
    }
}

impl Identity {
    /// Whether the system trusts it and it has not expired.
    pub fn valid(&self) -> bool {
        self.problem.is_none()
    }

    /// Whether its name is one of the role's.
    pub fn has_role(&self, role: Role) -> bool {
        role.prefixes().iter().any(|p| self.name.starts_with(p))
    }

    /// The team id in the name's trailing parentheses, if any.
    pub fn team(&self) -> Option<&str> {
        let open = self.name.rfind('(')?;
        let team = self.name[open + 1..].strip_suffix(')')?;
        (team.len() == 10 && team.chars().all(|c| c.is_ascii_alphanumeric())).then_some(team)
    }

    /// `"name" (SHA-1)`.
    pub fn label(&self) -> String {
        format!("\"{}\" ({})", self.name, self.sha1)
    }
}

/// Parses `security find-identity` output: the "Matching identities"
/// section, which lists invalid identities with their problem too.
pub fn parse(text: &str) -> Vec<Identity> {
    let mut identities = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("Valid identities only") {
            break;
        }
        let Some((number, rest)) = line.split_once(") ") else {
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
        let Some(rest) = rest.strip_prefix('"') else {
            continue;
        };
        let Some(end) = rest.rfind('"') else {
            continue;
        };
        let name = rest[..end].to_string();
        let problem = rest[end + 1..]
            .trim()
            .strip_prefix('(')
            .and_then(|p| p.strip_suffix(')'))
            .map(str::to_string);
        if !identities.iter().any(|i: &Identity| i.sha1 == sha1) {
            identities.push(Identity {
                sha1: sha1.to_ascii_uppercase(),
                name,
                problem,
            });
        }
    }
    identities
}

/// The `security find-identity` command.
pub fn list_cmd(keychain: Option<&Path>) -> Cmd {
    let mut cmd = Cmd::tool("security").args(["find-identity", "-p", "codesigning"]);
    if let Some(keychain) = keychain {
        cmd = cmd.arg(keychain);
    }
    cmd.timeout(Duration::from_secs(30))
}

/// The code-signing identities in the keychain (or the search list).
pub fn list(ctx: &Ctx, keychain: Option<&Path>) -> Result<Vec<Identity>> {
    let outcome = ctx.probe(&list_cmd(keychain))?;
    if !outcome.success() {
        return Err(IcmError::new(
            CheckId::IosSignNoIdentity,
            format!(
                "security find-identity failed{}: {}",
                keychain
                    .map(|k| format!(" on {}", crate::paths::display(k)))
                    .unwrap_or_default(),
                outcome.stderr_tail(3)
            ),
        ));
    }
    Ok(parse(&outcome.stdout_text()))
}

fn is_sha1(text: &str) -> bool {
    text.len() == 40 && text.chars().all(|c| c.is_ascii_hexdigit())
}

/// The identities a reference may sign with: for `auto` every valid one of
/// the role (in the team, when the name says), else the one it names.
pub fn candidates(
    reference: &str,
    identities: &[Identity],
    role: Role,
    team: Option<&str>,
) -> Result<Vec<Identity>> {
    let reference = reference.trim();
    let fix = format!(
        "The owner installs their {} certificate with its private key (Xcode > Settings > Accounts > Manage Certificates), or sets [ios.signing] {}.identity to its SHA-1 or name (`security find-identity -v -p codesigning` lists them).",
        role.label(),
        match role {
            Role::Distribution => "distribution",
            Role::Development => "development",
        }
    );
    if reference == "auto" {
        let found: Vec<Identity> = identities
            .iter()
            .filter(|i| i.has_role(role) && i.valid())
            .filter(|i| match (team, i.team()) {
                (Some(team), Some(own)) => team == own,
                _ => true,
            })
            .cloned()
            .collect();
        if found.is_empty() {
            let invalid: Vec<String> = identities
                .iter()
                .filter(|i| i.has_role(role) && !i.valid())
                .map(|i| {
                    format!(
                        "{} is {}",
                        i.label(),
                        i.problem.as_deref().unwrap_or("invalid")
                    )
                })
                .collect();
            let mut detail = format!(
                "no valid {} identity{} is in the keychain",
                role.label(),
                team.map(|t| format!(" for team {t}")).unwrap_or_default()
            );
            if !invalid.is_empty() {
                detail.push_str(&format!(" ({})", invalid.join("; ")));
            }
            return Err(IcmError::new(CheckId::IosSignNoIdentity, detail).fix(fix, &[]));
        }
        return Ok(found);
    }
    let named = identities.iter().find(|i| {
        if is_sha1(reference) {
            i.sha1.eq_ignore_ascii_case(reference)
        } else {
            i.name == reference
        }
    });
    match named {
        Some(identity) => Ok(vec![identity.clone()]),
        None => Err(IcmError::new(
            CheckId::IosSignNoIdentity,
            format!("the identity {reference} is not in the keychain"),
        )
        .fix(fix, &[])),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUTPUT: &str = r#"
Policy: Code Signing
  Matching identities
  1) 88D8D3B3158C1B77A6FCBCF87448B4AA16B13FE5 "Apple Distribution: icm test (ICMTEST001)" (CSSMERR_TP_NOT_TRUSTED)
  2) 0123456789ABCDEF0123456789ABCDEF01234567 "Apple Development: Jo Doe (ABCDE12345)"
  3) 1111111111111111111111111111111111111111 "Apple Distribution: Acme Ltd (ABCDE12345)"
  4) 2222222222222222222222222222222222222222 "Developer ID Application: Acme Ltd (ABCDE12345)"
     4 identities found

  Valid identities only
  1) 0123456789ABCDEF0123456789ABCDEF01234567 "Apple Development: Jo Doe (ABCDE12345)"
     3 valid identities found
"#;

    #[test]
    fn find_identity_output_is_parsed() {
        let identities = parse(OUTPUT);
        assert_eq!(identities.len(), 4);
        assert_eq!(
            identities[0].problem.as_deref(),
            Some("CSSMERR_TP_NOT_TRUSTED")
        );
        assert!(identities[0].has_role(Role::Distribution));
        assert_eq!(identities[0].team(), Some("ICMTEST001"));
        assert!(identities[1].valid());
        assert!(identities[1].has_role(Role::Development));
        assert!(!identities[3].has_role(Role::Distribution));
        assert!(parse("  0 valid identities found\n").is_empty());
    }

    #[test]
    fn references_pick_identities() {
        let identities = parse(OUTPUT);
        let auto = candidates("auto", &identities, Role::Distribution, Some("ABCDE12345")).unwrap();
        assert_eq!(auto.len(), 1);
        assert_eq!(auto[0].sha1, "1111111111111111111111111111111111111111");
        // Another team: the untrusted one is named in the error.
        let error =
            candidates("auto", &identities, Role::Distribution, Some("ICMTEST001")).unwrap_err();
        assert_eq!(error.id, "ios.sign.no_identity");
        assert!(
            error.detail.contains("CSSMERR_TP_NOT_TRUSTED"),
            "{}",
            error.detail
        );
        // Named identities are taken as they are.
        let named = candidates(
            "88d8d3b3158c1b77a6fcbcf87448b4aa16b13fe5",
            &identities,
            Role::Distribution,
            None,
        )
        .unwrap();
        assert!(!named[0].valid());
        let by_name = candidates(
            "Apple Development: Jo Doe (ABCDE12345)",
            &identities,
            Role::Development,
            None,
        )
        .unwrap();
        assert_eq!(by_name[0].sha1, "0123456789ABCDEF0123456789ABCDEF01234567");
        assert!(candidates("Nobody", &identities, Role::Development, None).is_err());
    }
}
