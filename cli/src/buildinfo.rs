//! What this icm was built from (embedded by `build.rs`; Appendix C item 3).

use serde_json::{Value, json};

/// The package version, e.g. `0.14.1-mobile.1`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// What `icm --version` prints after `icm `.
pub const VERSION_LINE: &str = env!("ICM_VERSION_LINE");

/// The full commit hash icm was built from, or empty.
pub const GIT_REV: &str = env!("ICM_GIT_REV");

/// Whether the source tree had uncommitted changes.
pub const GIT_DIRTY: bool = matches!(env!("ICM_GIT_DIRTY").as_bytes(), b"1");

/// The framework pin `icm new` defaults to: `tag:<tag>`, `rev:<sha>`,
/// `path:<dir>`, or empty when the build had no git metadata.
pub const FRAMEWORK: &str = env!("ICM_FRAMEWORK");

/// The fork's git URL for `tag:`/`rev:` pins, or empty.
pub const GIT_URL: &str = env!("ICM_GIT_URL");

/// The protocol versions this icm speaks (design §2.4 item 5).
pub mod protocols {
    /// The output contract (`icm.output/1`).
    pub const OUTPUT: u32 = 1;
    /// `ICM_EVENT` lines from apps.
    pub const EVENT: u32 = 1;
    /// The `tests/icm.rs` harness.
    pub const HARNESS: u32 = 1;
    /// The in-app agent bridge (phase 6).
    pub const BRIDGE: u32 = 1;
}

/// The release tag matching this version: `v<version>`.
pub fn tag() -> String {
    format!("v{VERSION}")
}

/// The parsed version.
pub fn version() -> semver::Version {
    semver::Version::parse(VERSION).expect("CARGO_PKG_VERSION is semver")
}

/// The `icm` object of `start` events.
pub fn json() -> Value {
    json!({
        "version": VERSION,
        "rev": GIT_REV,
        "dirty": GIT_DIRTY,
        "framework": FRAMEWORK,
        "protocols": {
            "output": protocols::OUTPUT,
            "event": protocols::EVENT,
            "harness": protocols::HARNESS,
            "bridge": protocols::BRIDGE,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_line_starts_with_the_version() {
        assert!(VERSION_LINE.starts_with(VERSION));
        assert!(VERSION.contains("-mobile."));
        assert_eq!(tag(), format!("v{VERSION}"));
        let _ = version();
    }
}
