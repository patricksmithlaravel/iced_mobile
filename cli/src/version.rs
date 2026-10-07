//! Comparing icm versions (Appendix C item 5).
//!
//! A `VersionReq` like `>=0.14.1-mobile.1` matches `0.14.1-mobile.2` but not
//! `0.14.2-mobile.1` or `0.15.0-mobile.1` (semver's pre-release rule), so the
//! first upstream rebase would make every project "too new". icm therefore
//! compares with `Version`'s *ordering* against a plain minimum.

use semver::Version;

/// The fork's repository, when the build did not record one.
pub const DEFAULT_GIT_URL: &str = "https://github.com/patricksmithlaravel/iced_mobile";

/// Parses a minimum version: `0.14.1-mobile.1`, or the older `>=0.14.1-mobile.1`.
pub fn parse_min(raw: &str) -> Result<Version, String> {
    let trimmed = raw.trim();
    let plain = trimmed.strip_prefix(">=").map(str::trim).unwrap_or(trimmed);
    let plain = plain.strip_prefix('v').unwrap_or(plain);

    Version::parse(plain).map_err(|error| {
        format!(
            "`{raw}` is not a version ({error}); write a plain minimum like `min_icm = \"0.14.1-mobile.1\"`"
        )
    })
}

/// Whether `current` is at least `min`, by version ordering.
pub fn meets(current: &Version, min: &Version) -> bool {
    current >= min
}

/// The command that installs icm `min` (or the newest tag).
pub fn install_command(min: Option<&Version>) -> String {
    let url = if crate::buildinfo::GIT_URL.is_empty() {
        DEFAULT_GIT_URL
    } else {
        crate::buildinfo::GIT_URL
    };
    match min {
        Some(version) => format!("cargo install --locked --git {url} --tag v{version} icm"),
        None => format!("cargo install --locked --git {url} --tag <newest v*-mobile.* tag> icm"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn ordering_survives_upstream_bumps() {
        let min = parse_min("0.14.1-mobile.1").unwrap();
        for newer in [
            "0.14.1-mobile.1",
            "0.14.1-mobile.2",
            "0.14.1-mobile.10",
            "0.14.2-mobile.1",
            "0.15.0-mobile.1",
            "0.14.1",
        ] {
            assert!(meets(&v(newer), &min), "{newer} should satisfy {min}");
        }
        for older in ["0.14.0-mobile.9", "0.14.1-mobile.0", "0.13.9"] {
            assert!(!meets(&v(older), &min), "{older} should not satisfy {min}");
        }

        // What VersionReq would have done: the bug this module avoids.
        let req = semver::VersionReq::parse(">=0.14.1-mobile.1").unwrap();
        assert!(!req.matches(&v("0.14.2-mobile.1")));
    }

    #[test]
    fn legacy_and_prefixed_forms_parse() {
        assert_eq!(
            parse_min(">=0.14.1-mobile.1").unwrap(),
            v("0.14.1-mobile.1")
        );
        assert_eq!(
            parse_min(">= 0.14.1-mobile.3").unwrap(),
            v("0.14.1-mobile.3")
        );
        assert_eq!(parse_min("v0.14.1-mobile.3").unwrap(), v("0.14.1-mobile.3"));
        assert!(parse_min("^0.14").is_err());
        assert!(parse_min("0.14").is_err());
    }

    #[test]
    fn install_commands_name_the_tag() {
        assert!(
            install_command(Some(&v("0.14.1-mobile.4"))).ends_with("--tag v0.14.1-mobile.4 icm")
        );
    }
}
