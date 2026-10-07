//! Exit codes (design §4.5). They are stable: agents branch on them.

use serde::Serialize;

/// How an icm invocation ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub enum Exit {
    /// Success; WARNs allowed.
    Ok,
    /// The app or artifact failed a gate, test, hook or verify.
    CheckFailed,
    /// Bad flags, or an unsupported platform/command pair.
    Usage,
    /// icm.toml or Cargo.toml invalid or inconsistent, or a bad lockfile shape.
    Config,
    /// A tool, target, SDK or package is missing, or versions are skewed.
    Environment,
    /// rustc or the linker failed.
    Build,
    /// An external tool failed unexpectedly.
    Tool,
    /// No device, simulator, emulator or browser; boot or install failed; a
    /// lock or port is busy.
    Device,
    /// A tool or infrastructure wait exceeded its limit.
    Timeout,
    /// Certificates, profiles, licences, store web steps, product decisions.
    NeedsOwner,
    /// The app crashed, panicked, hung, or never drew its first frame.
    AppDied,
    /// A bug in icm.
    Internal,
    /// Interrupted by a signal.
    Interrupted,
}

impl Exit {
    /// Every exit code, in numeric order.
    pub const ALL: [Exit; 13] = [
        Exit::Ok,
        Exit::CheckFailed,
        Exit::Usage,
        Exit::Config,
        Exit::Environment,
        Exit::Build,
        Exit::Tool,
        Exit::Device,
        Exit::Timeout,
        Exit::NeedsOwner,
        Exit::AppDied,
        Exit::Internal,
        Exit::Interrupted,
    ];

    /// The process exit code.
    pub fn code(self) -> u8 {
        match self {
            Exit::Ok => 0,
            Exit::CheckFailed => 1,
            Exit::Usage => 2,
            Exit::Config => 3,
            Exit::Environment => 4,
            Exit::Build => 5,
            Exit::Tool => 6,
            Exit::Device => 7,
            Exit::Timeout => 8,
            Exit::NeedsOwner => 9,
            Exit::AppDied => 10,
            Exit::Internal => 70,
            Exit::Interrupted => 130,
        }
    }

    /// The exit code for a process exit code, if it is one of icm's.
    pub fn from_code(code: i64) -> Option<Exit> {
        Exit::ALL
            .into_iter()
            .find(|exit| i64::from(exit.code()) == code)
    }

    /// The stable upper-case name, e.g. `NEEDS_OWNER`.
    pub fn name(self) -> &'static str {
        match self {
            Exit::Ok => "OK",
            Exit::CheckFailed => "CHECK_FAILED",
            Exit::Usage => "USAGE",
            Exit::Config => "CONFIG",
            Exit::Environment => "ENVIRONMENT",
            Exit::Build => "BUILD",
            Exit::Tool => "TOOL",
            Exit::Device => "DEVICE",
            Exit::Timeout => "TIMEOUT",
            Exit::NeedsOwner => "NEEDS_OWNER",
            Exit::AppDied => "APP_DIED",
            Exit::Internal => "INTERNAL",
            Exit::Interrupted => "INTERRUPTED",
        }
    }

    /// What the code means.
    pub fn meaning(self) -> &'static str {
        match self {
            Exit::Ok => "success; WARNs allowed",
            Exit::CheckFailed => "the app or artifact failed a gate, test, hook or verify",
            Exit::Usage => {
                "bad flags, an unsupported platform/command pair, or a command this build does not implement"
            }
            Exit::Config => {
                "icm.toml or Cargo.toml invalid or inconsistent, or a bad lockfile shape"
            }
            Exit::Environment => {
                "a tool, target, SDK or package is missing, or versions are skewed"
            }
            Exit::Build => "rustc or the linker failed",
            Exit::Tool => {
                "actool, aapt2, bundletool, wasm-bindgen, codesign, ... failed unexpectedly"
            }
            Exit::Device => {
                "no device, simulator, emulator or browser; boot or install failed; lock or port busy"
            }
            Exit::Timeout => {
                "a tool or infrastructure wait exceeded its limit; `detail` names which"
            }
            Exit::NeedsOwner => {
                "certificates, profiles, keystore password env, licences, store web steps, product decisions"
            }
            Exit::AppDied => {
                "crashed, panicked, ANR, or alive but no first frame within --wait-ready"
            }
            Exit::Internal => "bug in icm",
            Exit::Interrupted => "interrupted by SIGINT, SIGTERM or SIGHUP",
        }
    }

    /// What an agent does next.
    pub fn next(self) -> &'static str {
        match self {
            Exit::Ok => "continue",
            Exit::CheckFailed => "fix what `errors[]` names",
            Exit::Usage => "`icm <cmd> --help`",
            Exit::Config => "edit the field named at `file:line`",
            Exit::Environment => "`icm doctor <p> --fix [--yes]`",
            Exit::Build => "read the `diagnostic` events (also in `errors[0].diagnostics`)",
            Exit::Tool => "read the step's `log`",
            Exit::Device => "`icm devices`, `--device`, `--wait-lock`, `--port`",
            Exit::Timeout => {
                "raise `--timeout`; read the step log; for detached runs call `icm wait` again"
            }
            Exit::NeedsOwner => "stop and hand `errors[0].fix` to the owner",
            Exit::AppDied => "fix the app; the evidence is attached",
            Exit::Internal => "report it with `run_dir`",
            Exit::Interrupted => "rerun",
        }
    }

    /// Whether the exit code means success.
    pub fn is_ok(self) -> bool {
        self == Exit::Ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unique_and_round_trip() {
        let mut seen = std::collections::BTreeSet::new();
        for exit in Exit::ALL {
            assert!(seen.insert(exit.code()), "duplicate code {}", exit.code());
            assert_eq!(Exit::from_code(i64::from(exit.code())), Some(exit));
            assert!(!exit.name().is_empty() && !exit.meaning().is_empty());
        }
        assert_eq!(Exit::from_code(42), None);
        assert_eq!(Exit::NeedsOwner.code(), 9);
        assert_eq!(Exit::Internal.code(), 70);
        assert_eq!(Exit::Interrupted.code(), 130);
    }
}
