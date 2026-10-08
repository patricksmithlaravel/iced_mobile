//! The check and error catalogue (design §5).
//!
//! One namespace: every CHECK id is also an explainable error id, of the
//! form `<area>.<subject>[.<detail>]` in lower snake case. `icm explain <id>`
//! renders the entry below, plus a hand-written `cli/docs/explain/<id>.md`
//! for the common ones (embedded by `build.rs`).
//!
//! To add an id: add a line to the `catalogue!` invocation. A unit test
//! checks the ids' shape and that every hand-written doc names a real id.

use crate::exit::Exit;
use serde::Serialize;

/// Who acts on a failure (`fix.by`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum By {
    /// Edit code or config.
    #[serde(rename = "agent")]
    Agent,
    /// `icm doctor --fix`: local and idempotent.
    #[serde(rename = "doctor")]
    Doctor,
    /// `icm doctor --fix --yes`: downloads or installs.
    #[serde(rename = "doctor-yes")]
    DoctorYes,
    /// Credentials, certificates, licences, store web UI, product decisions.
    #[serde(rename = "owner")]
    Owner,
}

impl By {
    /// The wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            By::Agent => "agent",
            By::Doctor => "doctor",
            By::DoctorYes => "doctor-yes",
            By::Owner => "owner",
        }
    }

    /// What the value means, for docs.
    pub fn meaning(self) -> &'static str {
        match self {
            By::Agent => "the agent: edit code or config",
            By::Doctor => "`icm doctor --fix` (local, idempotent)",
            By::DoctorYes => "`icm doctor --fix --yes` (downloads or installs)",
            By::Owner => {
                "the owner: credentials, certificates, licences, store web UI, product decisions"
            }
        }
    }
}

/// The severity a check has when it does not pass, unless the emitter says
/// otherwise (e.g. `app.id.placeholder` is a WARN in dev and exit 9 in a
/// signed release).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Fails with the entry's exit code.
    Fail,
    /// Reported; FAIL (exit 1) only under `--strict`.
    Warn,
    /// Informational.
    Info,
    /// A check that only ever reports success (e.g. `run.ready`).
    Pass,
}

impl Level {
    /// The upper-case name.
    pub fn name(self) -> &'static str {
        match self {
            Level::Fail => "FAIL",
            Level::Warn => "WARN",
            Level::Info => "INFO",
            Level::Pass => "PASS",
        }
    }
}

/// One catalogue entry.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Entry {
    /// The id, e.g. `config.unknown_key`.
    pub id: &'static str,
    /// The exit code when it fails.
    #[serde(serialize_with = "serialize_exit")]
    pub exit: Exit,
    /// The default severity.
    pub level: Level,
    /// Who fixes it.
    pub by: By,
    /// One line: what went wrong.
    pub title: &'static str,
    /// The generic fix.
    pub fix: &'static str,
}

fn serialize_exit<S: serde::Serializer>(exit: &Exit, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_u8(exit.code())
}

macro_rules! catalogue {
    ($( $variant:ident = $id:literal, $exit:ident, $level:ident, $by:ident, $title:literal, $fix:literal; )*) => {
        /// Every id icm can report.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum CheckId {
            $(
                #[doc = $title]
                $variant,
            )*
        }

        impl CheckId {
            /// Every id, in catalogue order.
            pub const ALL: &'static [CheckId] = &[$(CheckId::$variant),*];

            /// The entry for this id.
            pub fn entry(self) -> &'static Entry {
                match self {
                    $(CheckId::$variant => &Entry {
                        id: $id,
                        exit: Exit::$exit,
                        level: Level::$level,
                        by: By::$by,
                        title: $title,
                        fix: $fix,
                    },)*
                }
            }
        }
    };
}

impl CheckId {
    /// The id string.
    pub fn id(self) -> &'static str {
        self.entry().id
    }

    /// The exit code when it fails.
    pub fn exit(self) -> Exit {
        self.entry().exit
    }

    /// Looks an id up.
    pub fn from_id(id: &str) -> Option<CheckId> {
        CheckId::ALL.iter().copied().find(|check| check.id() == id)
    }
}

impl std::fmt::Display for CheckId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

catalogue! {
    // ---- usage, internal, steps -------------------------------------------------
    UsageBadArgs = "usage.bad_args", Usage, Fail, Agent,
        "The command line is invalid",
        "Read `icm <command> --help`; the detail quotes the parser's message.";
    UsageNotImplemented = "usage.not_implemented", Usage, Fail, Agent,
        "This command is not implemented in this build of icm",
        "Use a command this build implements (`icm --help`), or install a newer icm.";
    InternalBug = "internal.bug", Internal, Fail, Agent,
        "icm panicked or hit a state it cannot handle",
        "Report it with the `run_dir`; rerunning may work around a transient cause.";
    StepTimeout = "step.timeout", Timeout, Fail, Agent,
        "A step or wait exceeded its time limit",
        "Read the step's log; raise `--timeout` if the work is legitimately slow.";
    ToolFailed = "tool.failed", Tool, Fail, Agent,
        "An external tool exited unsuccessfully",
        "Read the step's log named in the evidence.";
    RunInterrupted = "run.interrupted", Interrupted, Fail, Agent,
        "icm was interrupted by a signal",
        "Rerun the command. Its child processes were stopped.";
    RunStillRunning = "run.still_running", Timeout, Fail, Agent,
        "The detached run is still running",
        "Call `icm wait <run>` again; it can be called any number of times.";
    RunDetachedLost = "run.detached_lost", Internal, Fail, Agent,
        "The detached icm process ended without writing a result",
        "Read `detached.stderr` in the run directory, then rerun the command.";
    RunNotFound = "run.not_found", Usage, Fail, Agent,
        "No run with that id exists",
        "Pass a run id printed by `--detach` (or its run directory).";

    // ---- config ------------------------------------------------------------------
    ConfigNotFound = "config.not_found", Config, Fail, Agent,
        "No icm.toml was found",
        "Run icm in the app's directory, pass `--config <path>`, or create an app with `icm new <dir>`.";
    ConfigInvalid = "config.invalid", Config, Fail, Agent,
        "icm.toml (or host.toml, Cargo.toml) is invalid",
        "Edit the value at the file:line in the evidence.";
    ConfigUnknownKey = "config.unknown_key", Config, Fail, Agent,
        "A configuration file has a key icm does not know",
        "Remove or rename the key at the file:line in the evidence; the detail lists the keys allowed there.";
    ConfigManagedKey = "config.managed_key", Config, Fail, Agent,
        "An overlay sets a key icm generates",
        "Remove the key from the overlay; set the icm.toml key the detail names instead, if it names one (a value only icm writes stays as icm writes it).";
    ConfigRawXmlForbidden = "config.raw_xml_forbidden", Config, Fail, Agent,
        "Raw manifest XML sets something icm manages",
        "Remove the element or attribute from `extra_manifest_xml`/`extra_application_xml`; use the icm.toml key the detail names.";
    ConfigPackageNotFound = "config.package_not_found", Config, Fail, Agent,
        "The package icm.toml names is not in the cargo workspace",
        "Set `[app] package` to a package `cargo metadata` lists, or put icm.toml next to the app's Cargo.toml.";
    ConfigBinMissing = "config.bin_missing", Config, Fail, Agent,
        "The app package has no binary target icm can use",
        "Add `src/main.rs` (or a `[[bin]]`), or set `[app] bin` to an existing binary target.";
    ConfigLibMissing = "config.lib_missing", Config, Fail, Agent,
        "The app package has no library target (Android loads the app as a library)",
        "Add `src/lib.rs` with `iced::android_main!(run);`, or set `[app] lib`.";
    ConfigIdInvalid = "config.id_invalid", Config, Fail, Agent,
        "`[app] id` is not a valid reverse-DNS identifier",
        "Use letters, digits and underscores in at least two dot-separated segments, each starting with a letter (e.g. `com.example.notes`).";
    ConfigTooNew = "config.too_new", Environment, Fail, Agent,
        "icm.toml needs a newer icm than the one installed",
        "Install the icm version `min_icm` names (see the fix command), then rerun.";
    ConfigSchemaStale = "config.schema_stale", CheckFailed, Warn, Doctor,
        "The editor schema `.icm/icm.schema.json` is out of date",
        "Run `icm doctor --fix`.";
    ConfigOwnerDecision = "config.owner_decision", NeedsOwner, Fail, Owner,
        "A release needs a decision only the owner can make",
        "Hand the detail to the owner; they set the named icm.toml key.";

    // ---- app ---------------------------------------------------------------------
    AppIdPlaceholder = "app.id.placeholder", NeedsOwner, Warn, Owner,
        "`[app] id` is still a placeholder (com.example.*)",
        "The owner chooses the permanent id and sets `[app] id`; it cannot change after the first store upload.";
    AppIconPlaceholder = "app.icon.placeholder", NeedsOwner, Warn, Owner,
        "The app icon is still the template's placeholder",
        "The owner replaces `assets/icon.png` with a square PNG of at least 1024x1024.";
    AppIconInvalid = "app.icon.invalid", Config, Fail, Agent,
        "The app icon is not a square PNG of at least 1024x1024",
        "Replace the file `[app] icon` names with a square PNG of at least 1024x1024.";

    // ---- new, doctor's managed devices -----------------------------------------------
    NewDirNotEmpty = "new.dir_not_empty", Usage, Fail, Agent,
        "The directory for the new app is not empty",
        "Choose a new or empty directory, or pass --force to write the template's files into it.";
    NewFrameworkUnknown = "new.framework_unknown", Usage, Fail, Agent,
        "icm cannot tell which framework source the new app should pin",
        "Pass --framework tag:<tag>, rev:<full sha> or path:<checkout of iced_mobile>.";
    EnvSimulatorMissing = "env.simulator_missing", Environment, Fail, Doctor,
        "icm's managed iOS simulator does not exist yet",
        "Run `icm doctor ios-sim --fix`, which creates it with `xcrun simctl create`.";
    EnvAvdMissing = "env.avd_missing", Environment, Fail, Doctor,
        "icm's managed Android emulator (AVD) does not exist yet",
        "Run `icm doctor android --fix`, which creates it with `avdmanager create avd`.";
    EnvDebugKeystoreMissing = "env.debug_keystore_missing", Environment, Fail, Doctor,
        "The Android debug keystore does not exist",
        "Run `icm doctor android --fix`, which creates the debug keystore next to host.toml (~/.config/icm/android/debug.keystore) with keytool; ~/.android is left alone.";

    // ---- review --------------------------------------------------------------------
    ReviewSnapshotStale = "review.snapshot_stale", CheckFailed, Fail, Agent,
        "The review snapshots in platform/generated differ from what icm generates",
        "Run `icm print snapshots --write` and commit the result.";

    // ---- env ---------------------------------------------------------------------
    EnvRustToolchainMissing = "env.rust_toolchain_missing", Environment, Fail, DoctorYes,
        "The project's Rust toolchain is not installed",
        "Run `icm doctor --fix --yes`, which runs `rustup toolchain install <name>`.";
    EnvRustTargetMissing = "env.rust_target_missing", Environment, Fail, DoctorYes,
        "A Rust target is not installed for the project's active toolchain",
        "Run `icm doctor <platform> --fix --yes`, which runs `rustup target add --toolchain <toolchain> <target>`.";
    EnvXcodeMissing = "env.xcode_missing", NeedsOwner, Fail, Owner,
        "Xcode is not installed or not selected",
        "The owner installs Xcode and runs `sudo xcode-select -s /Applications/Xcode.app`.";
    EnvXcodeTooOld = "env.xcode_too_old", NeedsOwner, Fail, Owner,
        "The selected Xcode is older than icm supports",
        "The owner installs a newer Xcode and selects it with `xcode-select -s`.";
    EnvXcodeBeta = "env.xcode_beta", CheckFailed, Warn, Owner,
        "The selected Xcode looks like a beta",
        "Select a release Xcode with `xcode-select -s`; the App Store rejects beta builds (`altool --validate-app` is the real gate).";
    EnvIosRuntimeMissing = "env.ios_runtime_missing", Environment, Fail, DoctorYes,
        "No iOS simulator runtime satisfies `[ios] min_os`",
        "Run `icm doctor ios-sim --fix --yes` (`xcodebuild -downloadPlatform iOS`, about 8 GB).";
    EnvAndroidSdkMissing = "env.android_sdk_missing", NeedsOwner, Fail, Owner,
        "No Android SDK was found",
        "Install the Android command-line tools (e.g. `brew install --cask android-commandlinetools`) or set `android_sdk` in ~/.config/icm/host.toml.";
    EnvAndroidPackageMissing = "env.android_package_missing", Environment, Fail, DoctorYes,
        "An Android SDK package is missing",
        "Run `icm doctor android --fix --yes` (sdkmanager --install ...).";
    EnvNdkTooOld = "env.ndk_too_old", Environment, Fail, DoctorYes,
        "The Android NDK is missing or older than r28",
        "Run `icm doctor android --fix --yes`, or set ANDROID_NDK_HOME to an NDK r28 or newer.";
    EnvJdkMissing = "env.jdk_missing", Environment, Fail, DoctorYes,
        "No JDK 17 or newer was found",
        "Run `icm doctor android --fix --yes` (installs openjdk@21 with Homebrew), or set JAVA_HOME to a JDK 17+.";
    EnvToolMissing = "env.tool_missing", Environment, Fail, DoctorYes,
        "An external tool icm needs was not found",
        "Run `icm doctor <platform> --fix --yes`, or point `ICM_TOOL_<NAME>` at the tool.";
    EnvToolChecksum = "env.tool_checksum", Environment, Fail, DoctorYes,
        "A pinned tool's download does not match its size and sha256",
        "Rerun `icm doctor <platform> --fix --yes` (the download was deleted); if it keeps failing, the pin in icm's tools.toml is wrong: report it.";
    EnvChromeMissing = "env.chrome_missing", Environment, Fail, Owner,
        "Google Chrome (or Chromium) was not found",
        "Install Chrome, or set `chrome` in ~/.config/icm/host.toml or `ICM_CHROME`.";
    EnvLicensesNotAccepted = "env.licenses_not_accepted", NeedsOwner, Fail, Owner,
        "A licence must be accepted by a person",
        "The owner runs the command in the fix (e.g. `sdkmanager --licenses` or `sudo xcodebuild -license`).";
    EnvConsentRequired = "env.consent_required", Environment, Fail, Agent,
        "The fix downloads or installs, which needs `--yes`",
        "Rerun with `--yes` to allow downloads and installs.";
    EnvUnsupportedHost = "env.unsupported_host", Environment, Fail, Owner,
        "This host cannot build or run that platform",
        "Use a host that supports the platform (e.g. macOS for iOS).";
    EnvPolicyStale = "env.policy_stale", CheckFailed, Warn, Agent,
        "icm's store policy table is more than 90 days old",
        "Install a newer icm.";

    // ---- deps ----------------------------------------------------------------------
    DepsSingleIced = "deps.single_iced", Config, Fail, Agent,
        "Cargo.lock holds iced crates from more than one source or revision",
        "Put the same git URL and tag/rev (character for character) on every iced line of every crate, then `cargo update -p iced`.";
    DepsIcedNotFork = "deps.iced_not_fork", Config, Fail, Agent,
        "iced comes from crates.io or upstream instead of the iced_mobile fork",
        "Depend on iced from the fork's git URL with a tag or rev.";
    DepsIcedUnpinned = "deps.iced_unpinned", Config, Fail, Agent,
        "iced is taken from a git branch, not a tag or rev",
        "Pin iced with `tag = \"v0.14.1-mobile.N\"` or `rev = \"<sha>\"` on every iced line.";
    DepsCliFrameworkSkew = "deps.cli_framework_skew", CheckFailed, Warn, Agent,
        "The app's framework revision differs from the one this icm was built from",
        "Install the icm that matches the app's framework tag, or move the app to this icm's framework.";
    DepsSingleWinit = "deps.single_winit", Config, Fail, Agent,
        "Cargo.lock holds more than one winit",
        "Keep only the winit iced brings: drop direct winit dependencies, or give the crate that needs one iced's copy with [patch.crates-io] on iced's git URL and tag or rev.";
    DepsWinitFloor = "deps.winit_floor", Config, Fail, Agent,
        "winit is older than 0.30.13",
        "Run `cargo update -p winit`.";
    DepsSoftbufferFloor = "deps.softbuffer_floor", Config, Fail, Agent,
        "softbuffer is older than 0.4.7 (needed on Android)",
        "Run `cargo update -p softbuffer`.";
    DepsAndroidActivityBackend = "deps.android_activity_backend", Config, Fail, Agent,
        "Zero or two Android activity backends are enabled",
        "Enable exactly one of iced's `android-native-activity` / `android-game-activity` features in every crate.";
    DepsLibcSysinfoIos = "deps.libc_sysinfo_ios", Config, Fail, Agent,
        "A dependency uses an API that does not exist on iOS",
        "Disable the feature that pulls it in for iOS (see the detail).";
    DepsWasmBindgenCli = "deps.wasm_bindgen_cli", Environment, Fail, DoctorYes,
        "The wasm-bindgen CLI does not match the app's wasm-bindgen version",
        "Run `icm doctor web --fix --yes` (cargo install wasm-bindgen-cli --version <lock version>).";
    DepsGetrandomBackend = "deps.getrandom_backend", Config, Fail, Agent,
        "getrandom resolves for wasm32 without a web backend",
        "Add the `[web] rustflags` line and feature the detail names.";
    DepsLegacyEntry = "deps.legacy_entry", CheckFailed, Info, Agent,
        "The app has a hand-written android_main",
        "Optional: replace it with `iced::android_main!(run);`.";

    // ---- build ---------------------------------------------------------------------
    BuildCompileError = "build.compile_error", Build, Fail, Agent,
        "rustc reported errors",
        "Fix the errors in `errors[0].diagnostics` (also the `diagnostic` events), then rerun.";
    BuildLinkError = "build.link_error", Build, Fail, Agent,
        "Linking failed",
        "Read the linker output in the step log; missing NDK or SDK pieces map to `env.*` ids.";
    BuildCargoFailed = "build.cargo_failed", Build, Fail, Agent,
        "cargo failed outside compilation (manifest, resolution or fetch)",
        "Read the cargo output in the step log.";
    BuildWrongPlatform = "build.wrong_platform", Tool, Fail, Agent,
        "The built binary is for a different platform than requested",
        "Clean the target (`cargo clean -p <pkg> --target <triple>`) and rebuild; report it if it persists.";

    // ---- ios -----------------------------------------------------------------------
    IosMachoPlatform = "ios.macho.platform", CheckFailed, Fail, Agent,
        "The executable's Mach-O platform is wrong",
        "Rebuild for the right target (aarch64-apple-ios-sim for the simulator, aarch64-apple-ios for devices).";
    IosMachoMinos = "ios.macho.minos", CheckFailed, Fail, Agent,
        "The executable's minimum iOS version differs from `[ios] min_os`",
        "Rebuild through icm, which sets IPHONEOS_DEPLOYMENT_TARGET and relinks when it changes.";
    IosMachoSdkFloor = "ios.macho.sdk_floor", CheckFailed, Fail, Owner,
        "The executable was built with an SDK older than the App Store accepts",
        "The owner installs a current Xcode.";
    IosMachoArch = "ios.macho.arch", CheckFailed, Fail, Agent,
        "The executable has the wrong architecture",
        "Rebuild for arm64.";
    IosMachoSdkMatchesDt = "ios.macho.sdk_matches_dt", CheckFailed, Fail, Agent,
        "The Mach-O SDK version does not match the DTSDK keys",
        "Rebuild through icm so the plist and binary come from the same Xcode.";
    IosPlistLint = "ios.plist.lint", CheckFailed, Fail, Agent,
        "Info.plist fails plutil -lint",
        "Fix the `[ios.info_plist]` overlay value the detail names.";
    IosPlistRequiredKeys = "ios.plist.required_keys", CheckFailed, Fail, Agent,
        "Info.plist lacks a required key",
        "Report it: icm generates these keys.";
    IosPlistSceneManifest = "ios.plist.scene_manifest", CheckFailed, Fail, Agent,
        "Info.plist lacks UIApplicationSceneManifest (mandatory with the iOS 27 SDK)",
        "Do not remove it in overlays; icm generates it.";
    IosPlistDtKeys = "ios.plist.dt_keys", CheckFailed, Fail, Agent,
        "Info.plist DT* keys are missing or wrong",
        "Rebuild through icm.";
    IosPlistExportCompliance = "ios.plist.export_compliance", CheckFailed, Fail, Owner,
        "ITSAppUsesNonExemptEncryption is missing",
        "The owner sets `[ios] uses_non_exempt_encryption`.";
    IosPlistUsageDescriptions = "ios.plist.usage_descriptions", CheckFailed, Fail, Agent,
        "A permission is used without its usage description",
        "Set the reason string in `[app.permissions]`.";
    IosPlistIpadOrientations = "ios.plist.ipad_orientations", CheckFailed, Fail, Agent,
        "iPad orientation keys are inconsistent",
        "Adjust `[app] orientations`.";
    IosIconOpaque1024 = "ios.icon.opaque_1024", CheckFailed, Fail, Agent,
        "The 1024x1024 App Store icon has transparency",
        "Use an opaque icon, or set `[app] background` for icm to flatten onto.";
    IosPrivacyPresent = "ios.privacy.present", CheckFailed, Fail, Agent,
        "PrivacyInfo.xcprivacy is missing from the bundle",
        "Report it: icm generates it.";
    IosPrivacyReasons = "ios.privacy.reasons", CheckFailed, Fail, Agent,
        "A required-reason API is used without a declared reason",
        "Add the reason to `[ios.privacy] api_reasons`.";
    IosActoolFailed = "ios.actool_failed", Tool, Fail, Agent,
        "actool failed to compile the asset catalog",
        "Read the step log; usually the icon is not a valid PNG.";
    IosSimBootFailed = "ios.sim.boot_failed", Device, Fail, Agent,
        "The simulator did not boot",
        "Shut down that simulator alone (`xcrun simctl shutdown <udid>`, the UDID the error names; never `shutdown all`, which stops other projects' and the owner's simulators), then rerun; `icm run ios-sim --fresh` uses a new simulator.";
    IosSimShared = "ios.sim.shared", Device, Info, Agent,
        "The simulator was booted for another project",
        "`icm stop --shutdown` in the project that booted it shuts it down; to shut it down anyway, run `xcrun simctl shutdown <udid>` for that simulator alone.";
    IosSimOwnerUnknown = "ios.sim.owner_unknown", Device, Warn, Agent,
        "icm could not read or write which project booted the simulator",
        "A simulator whose owner `simctl getenv` cannot read is left running by `icm stop --shutdown`, in case another project uses it: rerun the stop once the simulator answers, or run `xcrun simctl shutdown <udid>` for that simulator alone. A run that could not mark the simulator it booted leaves it to any project's `--shutdown`: the fix sets the mark by hand.";
    IosSimInstallFailed = "ios.sim.install_failed", Device, Fail, Agent,
        "simctl install failed",
        "Read the step log; rerun with `--fresh` if the simulator is in a bad state.";
    IosSimNotFound = "ios.sim.not_found", Device, Fail, Agent,
        "The simulator (or a device type for it) was not found",
        "Pick one from `xcrun simctl list devices available`, or drop `--sim`/`--device` (and host.toml `simulator_udid`) to use icm's managed simulator.";
    IosDeviceNotFound = "ios.device.not_found", Device, Fail, Owner,
        "No paired iOS device was found",
        "Connect and trust the device, or pass `--device <udid>`.";
    IosDeviceDeveloperModeOff = "ios.device.developer_mode_off", NeedsOwner, Fail, Owner,
        "Developer Mode is off on the device",
        "The owner enables Settings > Privacy & Security > Developer Mode.";
    IosSignNoIdentity = "ios.sign.no_identity", NeedsOwner, Fail, Owner,
        "No usable signing identity is in the keychain",
        "The owner installs the certificate (Xcode > Settings > Accounts).";
    IosSignNoProfile = "ios.sign.no_profile", NeedsOwner, Fail, Owner,
        "No provisioning profile matches the app",
        "The owner creates or downloads a matching profile.";
    IosSignProfileExpired = "ios.sign.profile_expired", NeedsOwner, Fail, Owner,
        "The provisioning profile has expired",
        "The owner renews the profile.";
    IosSignProfileMismatch = "ios.sign.profile_mismatch", NeedsOwner, Fail, Owner,
        "The provisioning profile does not match the app id, certificate or device",
        "The owner regenerates the profile for this app id, certificate and device.";
    IosSignKeychainPrompt = "ios.sign.keychain_prompt", NeedsOwner, Fail, Owner,
        "codesign is waiting for keychain access",
        "The owner allows codesign access to the key (Always Allow), then reruns.";
    IosSignVerify = "ios.sign.verify", CheckFailed, Fail, Agent,
        "codesign --verify failed on the bundle",
        "Read the step log.";
    IosEntitlementsNotInProfile = "ios.entitlements.not_in_profile", NeedsOwner, Fail, Owner,
        "An entitlement is not allowed by the provisioning profile",
        "The owner adds the capability to the App ID and regenerates the profile, or the agent removes the entitlement.";
    IosEntitlementsGetTaskAllow = "ios.entitlements.get_task_allow", CheckFailed, Fail, Agent,
        "A release build carries get-task-allow",
        "Report it: release signing must not include it.";
    IosIpaLayout = "ios.ipa.layout", CheckFailed, Fail, Agent,
        "The .ipa layout is wrong",
        "Report it.";
    IosIpaSignature = "ios.ipa.signature", CheckFailed, Fail, Agent,
        "The .ipa's signature does not verify",
        "Report it.";
    IosVersionFormat = "ios.version.format", Config, Fail, Agent,
        "The Cargo.toml version is not a valid CFBundleShortVersionString",
        "Use X[.Y[.Z]] with no pre-release part.";
    IosXcodeNotBeta = "ios.xcode.not_beta", CheckFailed, Warn, Owner,
        "The release was built with what looks like a beta Xcode",
        "Select a release Xcode; `altool --validate-app` is the real gate.";
    IosExportComplianceDocumentation = "ios.export_compliance.documentation", NeedsOwner, Warn, Owner,
        "Export compliance documentation must be uploaded in App Store Connect",
        "The owner uploads it, or sets `[ios] export_compliance_code`.";
    IosDsymUuid = "ios.dsym.uuid", CheckFailed, Fail, Agent,
        "The dSYM's UUID differs from the executable's",
        "Rerun `icm release ios`; the dSYM is made from the executable it ships with.";
    IosDsymLineTables = "ios.dsym.line_tables", CheckFailed, Fail, Agent,
        "The dSYM's line table names none of the app's source files",
        "Build releases through icm, which keeps line tables (`profile.release.debug=\"line-tables-only\"`); do not strip the binary before dsymutil.";
    IosDeviceAmbiguous = "ios.device.ambiguous", Device, Fail, Agent,
        "More than one iOS device is connected",
        "Pass `--device <udid|name>` (`xcrun devicectl list devices` lists them).";
    IosDeviceInstallFailed = "ios.device.install_failed", Device, Fail, Agent,
        "devicectl could not install the app on the device",
        "Read the step log: an unlocked device, a trusted computer and a profile that lists the device are needed.";
    IosDeviceLaunchFailed = "ios.device.launch_failed", Device, Fail, Agent,
        "devicectl could not launch the app on the device",
        "Unlock the device and read the step log; a first launch may need the developer trusted in Settings > General > VPN & Device Management.";
    IosDeviceStopUnconfirmed = "ios.device.stop_unconfirmed", Device, Warn, Agent,
        "icm could not confirm that the recorded process on the device is the app, so it did not terminate it",
        "Connect and unlock the device, then run `icm stop ios-device` again: icm kept the session's record for that. A pid on the phone is reused like any other, so icm ends the app only after `xcrun devicectl device info processes` lists the recorded pid running the app's executable; to end it yourself, check that list first.";
    IosShotStoreSize = "ios.shot.store_size", Device, Fail, Agent,
        "The simulator's screen is not a size App Store Connect takes for iPhone screenshots",
        "Run the app on a 6.9-inch or 6.5-inch iPhone simulator: `icm run ios-sim --store`, then `icm shot ios-sim --store`.";
    IosAscAuth = "ios.asc.auth", NeedsOwner, Fail, Owner,
        "altool could not authenticate with App Store Connect",
        "The owner checks the API key file (~/.appstoreconnect/private_keys/AuthKey_<id>.p8) and the key and issuer variables UPLOAD.md names.";
    IosAscAppRecord = "ios.asc.app_record", NeedsOwner, Fail, Owner,
        "App Store Connect has no app record for this bundle id",
        "The owner creates the app record in App Store Connect (the API cannot) and sets `[ios] asc_app_id`.";
    IosAscRejected = "ios.asc.rejected", CheckFailed, Fail, Agent,
        "App Store Connect rejected the build for a reason icm does not map",
        "Read the message in the detail; `icm verify ios` reruns every gate icm knows.";

    // ---- android -------------------------------------------------------------------
    AndroidDeviceNone = "android.device.none", Device, Fail, Agent,
        "No Android device or emulator is available",
        "Run `icm doctor android --fix` to create the managed emulator, or connect a device.";
    AndroidDeviceAmbiguous = "android.device.ambiguous", Device, Fail, Agent,
        "Several Android devices are online and none was chosen",
        "Pass `--device <serial>` (see `icm devices android`).";
    AndroidEmulatorBootTimeout = "android.emulator.boot_timeout", Timeout, Fail, Agent,
        "The emulator did not finish booting in time",
        "Rerun with a longer `--timeout`; check the emulator log in the run directory.";
    AndroidEmulatorPortsBusy = "android.emulator.ports_busy", Device, Fail, Agent,
        "Every emulator port icm may use is busy",
        "Stop an emulator (`icm stop android --shutdown`) or set `android.emulator_ports` in host.toml.";
    AndroidEmulatorShared = "android.emulator.shared", Device, Info, Agent,
        "The emulator was booted for another project",
        "When two projects run at once, give each its own emulator (`--avd <name>`, or host.toml android.avd); `icm stop --shutdown` in the project that booted it shuts it down.";
    AndroidEmulatorOwnerUnknown = "android.emulator.owner_unknown", Device, Warn, Agent,
        "icm could not read or write which project booted the emulator",
        "An emulator whose `debug.icm.booted_by` adb cannot read is left running by `icm stop --shutdown`, in case another project uses it: rerun the stop once the emulator answers, or run `adb -s <serial> emu kill` for that emulator alone. A run that could not mark the emulator it booted leaves it to any project's `--shutdown`: the fix sets the property by hand.";
    AndroidEmulatorShutdownFailed = "android.emulator.shutdown_failed", Device, Warn, Agent,
        "The emulator is still running after `adb emu kill`, or adb could not confirm that it shut down",
        "Run `adb -s <serial> emu kill` again, or quit the emulator yourself (its window, or the process that listens on its console port); `icm stop android --shutdown` tries again, since icm keeps the emulator's record. When the WARN says adb could not say whether the emulator still runs (`adb devices` failed), make adb answer first: icm does not take a failed question for an emulator that is gone.";
    AndroidEmulatorFailed = "android.emulator.failed", Device, Fail, Agent,
        "The emulator exited while booting",
        "Read the emulator log in the evidence (disk space, memory, a broken AVD), then rerun; `icm devices android` lists the AVDs.";
    AndroidLaunchFailed = "android.launch_failed", Device, Fail, Agent,
        "am start could not launch the app's activity",
        "Read the step log; rebuild with `icm run android` so the installed manifest declares the activity.";
    AndroidInstallFailed = "android.install.failed", Device, Fail, Agent,
        "adb install failed",
        "Read the step log for the INSTALL_FAILED_* reason.";
    AndroidInstallSignatureMismatch = "android.install.signature_mismatch", Device, Fail, Agent,
        "The installed app was signed with a different key",
        "Rerun with `--reinstall --wipe-data`, which uninstalls the app and wipes its data.";
    AndroidSoExport = "android.so.export", Tool, Fail, Agent,
        "The library does not export ANativeActivity_onCreate",
        "Keep `iced::android_main!(run);` in src/lib.rs and build through icm.";
    AndroidSoAlign16k = "android.so.align16k", CheckFailed, Fail, Agent,
        "A PT_LOAD segment is not 16 KB aligned",
        "Use NDK r28 or newer (icm passes the flags).";
    AndroidSoAbis = "android.so.abis", CheckFailed, Fail, Agent,
        "A library is missing for a declared ABI, or has the wrong machine type",
        "Rebuild all `[android] abis`.";
    AndroidManifestTargetSdk = "android.manifest.target_sdk", CheckFailed, Fail, Agent,
        "targetSdk is below the Play floor",
        "Raise `[android] target_sdk`.";
    AndroidManifestConfigChanges = "android.manifest.config_changes", CheckFailed, Fail, Agent,
        "android:configChanges lacks values iced needs",
        "Do not remove configChanges values in overlays.";
    AndroidManifestHasCode = "android.manifest.has_code", CheckFailed, Fail, Agent,
        "android:hasCode is wrong for the activity type",
        "Report it: icm generates it.";
    AndroidManifestDebuggable = "android.manifest.debuggable", CheckFailed, Fail, Agent,
        "A release manifest is debuggable",
        "Report it: release builds must not be debuggable.";
    AndroidManifestLibName = "android.manifest.lib_name", CheckFailed, Fail, Agent,
        "android.app.lib_name does not match the built library",
        "Set `[app] lib` to the library target name.";
    AndroidManifestVersion = "android.manifest.version", CheckFailed, Fail, Agent,
        "versionCode or versionName is wrong",
        "Set `[app] build` and the Cargo.toml version.";
    AndroidManifestBackOptout = "android.manifest.back_optout", CheckFailed, Warn, Agent,
        "The app opts out of predictive back (removed at API 37)",
        "Plan to handle back gestures before targetSdk 37.";
    AndroidBundleAlignment = "android.bundle.alignment", CheckFailed, Fail, Agent,
        "The bundle's native libraries are not 16 KB aligned",
        "Report it.";
    AndroidAabValidate = "android.aab.validate", CheckFailed, Fail, Agent,
        "bundletool validate failed",
        "Read the step log.";
    AndroidAabSigned = "android.aab.signed", CheckFailed, Fail, Agent,
        "The .aab is not signed with the upload key",
        "Check `[android.signing] upload`.";
    AndroidAabUnsigned = "android.aab.unsigned", CheckFailed, Warn, Owner,
        "The .aab is unsigned (`--sign none`, or the owner's upload key was missing)",
        "The owner signs it, or releases with signing configured.";
    AndroidApkZipalign = "android.apk.zipalign", CheckFailed, Fail, Agent,
        "The APK is not zip-aligned",
        "Report it.";
    AndroidApkSignature = "android.apk.signature", CheckFailed, Fail, Agent,
        "The APK signature does not verify",
        "Read the step log.";
    AndroidKeystoreMissing = "android.keystore.missing", NeedsOwner, Fail, Owner,
        "The upload keystore was not found",
        "The owner creates it (keytool -genkeypair) at the configured path.";
    AndroidKeystorePasswordEnvUnset = "android.keystore.password_env_unset", NeedsOwner, Fail, Owner,
        "The keystore password environment variable is not set",
        "The owner exports the variable named in `[android.signing] upload`.";
    AndroidPermissionsReview = "android.permissions.review", CheckFailed, Warn, Owner,
        "The release declares permissions the Play data safety form must cover",
        "The owner reviews the Data safety answers.";
    AndroidScreenSecure = "android.screen.secure", CheckFailed, Info, Agent,
        "The window has FLAG_SECURE, so screenshots are black",
        "Expected for apps that set FLAG_SECURE; use `icm shot --headless` to see the UI.";
    AndroidOrientationLocked = "android.orientation_locked", Config, Warn, Agent,
        "The app is locked to the other orientation, so turning the device does not turn it",
        "Add the orientation to `[app] orientations` and run the app again to test it there; `icm shot --headless --viewport <W>x<H>` renders any size without a device.";
    AndroidAapt2Failed = "android.aapt2_failed", Tool, Fail, Agent,
        "aapt2 failed",
        "Read the step log; usually a resource in platform/android/res is invalid.";
    AndroidBundletoolFailed = "android.bundletool_failed", Tool, Fail, Agent,
        "bundletool failed",
        "Read the step log.";
    AndroidKeystoreUnreadable = "android.keystore.unreadable", NeedsOwner, Fail, Owner,
        "keytool or jarsigner cannot use the upload key (a wrong password, alias or keystore)",
        "The owner checks `[android.signing] upload` (keystore path, alias) and the values of its password variables, then releases again.";
    AndroidSmoke = "android.smoke", CheckFailed, Fail, Agent,
        "The release bundle did not install and start on a device",
        "Read the evidence (logcat and the step logs in the run directory); `icm run android --from-aab` repeats the install.";
    AndroidPlayRejected = "android.play.rejected", CheckFailed, Fail, Agent,
        "Google Play refused the upload for a reason icm does not map",
        "Read Google Play's message in the evidence, fix what it names and upload a new build.";
    AndroidPlayAppMissing = "android.play.app_missing", NeedsOwner, Fail, Owner,
        "The app does not exist in the Play Console yet",
        "The owner creates the app in the Play Console and uploads the first bundle there by hand (UPLOAD.md), then runs `icm ledger mark-uploaded android`.";
    AndroidPlayPermission = "android.play.permission", NeedsOwner, Fail, Owner,
        "The Play Console service account cannot upload (access, key or API)",
        "The owner grants the service account release permissions for the app, renews its JSON key, or enables the Google Play Android Developer API.";
    AndroidPlayWrongKey = "android.play.wrong_key", NeedsOwner, Fail, Owner,
        "Google Play expects the bundle signed with another upload key",
        "The owner signs with the upload key registered in the Play Console, or asks Google to reset the upload key.";

    // ---- web -----------------------------------------------------------------------
    WebPortBusy = "web.port_busy", Device, Fail, Agent,
        "The web server's port is busy",
        "Pass `--port <n>` or `--port 0`, or stop the other server.";
    WebChromeFailed = "web.chrome_failed", Device, Fail, Agent,
        "Headless Chrome failed to start or connect",
        "Read the session log; check the Chrome path with `icm print tools`.";
    WebSizeBudget = "web.size_budget", CheckFailed, Fail, Agent,
        "The gzip size of the .wasm exceeds `[web] size_budget_kb`",
        "Reduce the binary or raise the budget.";
    WebMime = "web.mime", CheckFailed, Fail, Agent,
        "The site serves .wasm with the wrong MIME type",
        "Configure the host to serve application/wasm.";
    WebFontsEmbedded = "web.fonts_embedded", CheckFailed, Fail, Agent,
        "No font is embedded for the web build",
        "Keep `features = [\"fira-sans\"]` on iced.";
    WebRendererFallback = "web.renderer_fallback", CheckFailed, Warn, Agent,
        "The web build has no WebGL fallback for a browser without a WebGPU adapter",
        "Enable iced's `webgl` feature for wasm32 (headless Chrome has no WebGPU adapter).";
    WebHashedAssets = "web.hashed_assets", CheckFailed, Fail, Agent,
        "Release assets are not content-hashed",
        "Make the release again with `icm release web`; never edit its files.";
    WebServeSmoke = "web.serve_smoke", CheckFailed, Fail, Agent,
        "The release site did not load in headless Chrome",
        "Read console.ndjson in the run directory.";

    // ---- desktop -------------------------------------------------------------------
    DesktopShotPermission = "desktop.shot.permission", CheckFailed, Warn, Owner,
        "Screen Recording permission is not granted, so the screenshot is a headless render",
        "The owner may grant Screen Recording to the terminal (System Settings > Privacy & Security).";
    DesktopLogsSecretUnknown = "desktop.logs.secret_unknown", CheckFailed, Warn, Agent,
        "The app inherited a secret this command cannot redact, so its logs come from the run's redacted copies",
        "Run the command where the variable is set, or hand the secret to the app with `icm run desktop --env NAME=…`.";
    MacosSignNoDeveloperId = "macos.sign.no_developer_id", NeedsOwner, Fail, Owner,
        "No Developer ID Application identity is installed",
        "The owner installs the Developer ID certificate.";
    MacosHardenedRuntime = "macos.hardened_runtime", CheckFailed, Fail, Agent,
        "The app is not signed with the hardened runtime",
        "Report it.";
    MacosSignVerify = "macos.sign.verify", CheckFailed, Fail, Agent,
        "codesign --verify failed",
        "Read the step log.";
    MacosMinOs = "macos.min_os", CheckFailed, Fail, Agent,
        "The binary's minimum macOS differs from `[desktop.macos] min_os`",
        "Rebuild through icm.";
    MacosGatekeeper = "macos.gatekeeper", CheckFailed, Fail, Agent,
        "spctl rejects the app",
        "Read the step log.";
    MacosNotStapled = "macos.not_stapled", NeedsOwner, Fail, Owner,
        "The app is not notarized and stapled",
        "The owner notarizes and staples with the commands in UPLOAD.md.";
    WindowsSignNotConfigured = "windows.sign.not_configured", NeedsOwner, Fail, Owner,
        "Windows signing is not configured",
        "The owner sets `[desktop.windows] sign_command`.";
    WindowsSigned = "windows.signed", CheckFailed, Fail, Agent,
        "The Windows installer is not signed",
        "Read the step log.";
    WindowsMsiVersion = "windows.msi_version", Config, Fail, Agent,
        "The version cannot be expressed as an MSI ProductVersion",
        "Use a version with major, minor < 256 and patch < 65536.";
    WindowsSdkMissing = "windows.sdk_missing", Environment, Fail, Owner,
        "The Windows SDK (rc.exe, signtool) is missing",
        "Install the Windows SDK.";
    LinuxGlibcFloor = "linux.glibc_floor", CheckFailed, Fail, Agent,
        "The binary needs a newer glibc than `[desktop.linux] glibc_floor`",
        "Build on an older distribution (e.g. the ubuntu:22.04 container).";
    LinuxDesktopFile = "linux.desktop_file", CheckFailed, Fail, Agent,
        "The .desktop file is invalid",
        "Report it.";
    LinuxDebLint = "linux.deb.lint", CheckFailed, Warn, Agent,
        "The .deb has lint warnings",
        "Read the step log.";
    MacosSignIdentityAmbiguous = "macos.sign.identity_ambiguous", NeedsOwner, Fail, Owner,
        "Several Developer ID Application identities of different teams are installed",
        "The owner names one in `[desktop.macos] identity` (its SHA-1 or its full name).";
    MacosSignKeychainPrompt = "macos.sign.keychain_prompt", NeedsOwner, Fail, Owner,
        "codesign waited for keychain access",
        "The owner unlocks the keychain and lets codesign use the key, or points host.toml `signing_keychain` (ICM_KEYCHAIN) at an unlocked build keychain.";
    MacosArch = "macos.arch", CheckFailed, Fail, Agent,
        "The binary lacks an architecture the release needs",
        "Rebuild through icm; `--universal` needs the x86_64-apple-darwin target (`icm doctor desktop --fix --yes`).";
    MacosBundle = "macos.bundle", CheckFailed, Fail, Agent,
        "The .app's Info.plist or layout is invalid",
        "Read the evidence; a key set by `[app]` may hold a value the plist cannot carry. Otherwise report it.";
    MacosDsym = "macos.dsym", CheckFailed, Warn, Agent,
        "The dSYM has no line tables for the app's sources",
        "Rebuild through icm: release builds keep line tables for the dSYM.";
    MacosDmg = "macos.dmg", CheckFailed, Fail, Agent,
        "The disk image does not verify",
        "Rebuild it with `icm release macos --dmg`.";
    MacosNotarization = "macos.notarization", CheckFailed, Fail, Agent,
        "Apple's notary service did not accept the submission",
        "Fix what `icm diagnose notarytool` lists, rerun `icm release macos`, and the owner notarizes again.";
    MacosNotaryCredentials = "macos.notary_credentials", NeedsOwner, Fail, Owner,
        "notarytool could not use the owner's credentials",
        "The owner stores them once with `xcrun notarytool store-credentials` (UPLOAD.md shows the line).";
    WindowsPeImports = "windows.pe_imports", CheckFailed, Fail, Agent,
        "The executable needs the VC++ runtime DLLs, which a clean Windows lacks",
        "Rebuild through icm (it links the static C runtime); a RUSTFLAGS or CARGO_ENCODED_RUSTFLAGS in the environment replaces icm's flags.";
    WindowsSubsystem = "windows.subsystem", CheckFailed, Warn, Agent,
        "The executable opens a console window next to the app",
        "Keep `#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = \"windows\")]` at the top of src/main.rs.";
    LinuxAppimageLibs = "linux.appimage_libs", CheckFailed, Warn, Agent,
        "The AppImage lacks a library it bundles for older hosts",
        "Build in the ubuntu:22.04 container, or install libxkbcommon0, libxkbcommon-x11-0 and libwayland-cursor0 on the build host.";

    // ---- run -----------------------------------------------------------------------
    RunReady = "run.ready", Ok, Pass, Agent,
        "The app drew its first frame",
        "Nothing to fix.";
    RunAlive = "run.alive", Ok, Pass, Agent,
        "The app is running",
        "Nothing to fix.";
    RunAppDied = "run.app_died", AppDied, Fail, Agent,
        "The app exited or crashed",
        "Read the evidence (stderr, crash report), fix the app, rerun.";
    RunAppPanicked = "run.app_panicked", AppDied, Fail, Agent,
        "The app panicked",
        "Fix the panic at the location in the detail, then rerun.";
    RunAnr = "run.anr", AppDied, Fail, Agent,
        "The app stopped responding (ANR)",
        "Find what blocks the main thread; read the evidence.";
    RunNotReady = "run.not_ready", AppDied, Fail, Agent,
        "The app drew no first frame within --wait-ready",
        "Read `icm logs <platform> --level warn`; raise `--wait-ready` if it is legitimately slow.";
    RunActivityRecreated = "run.activity_recreated", CheckFailed, Warn, Agent,
        "Android recreated the app's activity",
        "Follow the detail: rebuild a stale APK, raise [android] target_sdk to 36 for assetsPaths, or rerun once a fresh emulator has settled; never remove configChanges values. A FAIL means the app did not start over in the new activity: update an iced_mobile pin from before the Android lifecycle fix.";
    RunScreenBlank = "run.screen_blank", CheckFailed, Warn, Agent,
        "The screenshot is a single colour",
        "Compare with `icm shot --headless`; check fonts and theme; read the logs.";
    RunFontMissing = "run.font_missing", CheckFailed, Warn, Agent,
        "The app reported a missing default font",
        "Keep `features = [\"fira-sans\"]` on iced.";
    RunLockBusy = "run.lock_busy", Device, Fail, Agent,
        "Another icm holds the lock for this platform",
        "Wait for it, pass `--wait-lock <dur>`, or stop it.";
    RunNoSession = "run.no_session", Device, Fail, Agent,
        "No running session for that platform",
        "Start one with `icm run <platform>`.";
    RunIdentityUnavailable = "run.identity_unavailable", CheckFailed, Warn, Agent,
        "icm cannot tell whether a process it started is still that process, so it will not signal it",
        "Stop the process yourself when you are done with it (the detail names its pid).";

    // ---- test, harness, hooks, input -----------------------------------------------------
    TestFailed = "test.failed", CheckFailed, Fail, Agent,
        "A test failed",
        "Read the failing test's output in the evidence.";
    TestIceParse = "test.ice_parse", Config, Fail, Agent,
        "A .ice flow file does not parse",
        "Fix the line in the evidence.";
    HarnessMissing = "harness.missing", Config, Fail, Agent,
        "The app has no tests/icm.rs harness",
        "Add the template's tests/icm.rs and its `[[test]] name = \"icm\" harness = false` entry.";
    HarnessProtocolMismatch = "harness.protocol_mismatch", Environment, Fail, Agent,
        "The app's harness speaks a protocol this icm does not",
        "Install the icm that matches the app's framework tag.";
    InputUnsupported = "input.unsupported", Usage, Fail, Agent,
        "Input is not supported on that platform in this build",
        "Use `icm ui --headless` for desktop, or a platform that supports input.";
    TestPassed = "test.passed", Ok, Pass, Agent,
        "Tests passed",
        "Nothing to fix.";
    TestLifecycle = "test.lifecycle", CheckFailed, Fail, Agent,
        "A lifecycle step lost the app: a new process, a recreated activity, no frame or an ANR",
        "Read the step's evidence (events.txt, logcat.txt, its screenshot); the app must keep its process through configuration changes and Home, and start over after Back and a kill.";
    UiSelectorNotFound = "ui.selector_not_found", CheckFailed, Fail, Agent,
        "No widget in the headless view matches the selector",
        "Read `icm ui --headless tree` and match a widget's exact text or `#id`.";

    // ---- release ---------------------------------------------------------------------
    StorePolicyUpcoming = "store.policy_upcoming", CheckFailed, Info, Agent,
        "A store floor in icm's policy table takes effect soon",
        "Plan for it before the date in the detail; `icm print policy` lists every floor.";
    StoreMetadataMissing = "store.metadata_missing", NeedsOwner, Warn, Owner,
        "The store listing needs a URL icm.toml does not have yet",
        "The owner sets the `[store]` key the detail names (a privacy policy or support URL); UPLOAD.md lists it either way.";
    ReleaseDirtyTree = "release.dirty_tree", CheckFailed, Fail, Agent,
        "The release is not built from a clean git commit",
        "Commit the changes and Cargo.lock, or pass --allow-dirty (artifacts.json then records `dirty: true`).";
    ReleaseNotUploadable = "release.not_uploadable", NeedsOwner, Fail, Owner,
        "The release is not uploadable, so it cannot have been uploaded",
        "Record only a build the owner uploaded: release it signed with every gate passing and let upload.sh record it; if the owner signed and uploaded this one by hand, the owner passes --force.";
    ReleaseLockMissing = "release.lock_missing", CheckFailed, Fail, Agent,
        "There is no Cargo.lock for the release to build with --locked",
        "Create it (`icm check <platform>` resolves it), then commit it.";
    ReleaseNotFound = "release.not_found", Usage, Fail, Agent,
        "There is no release of that target (or no artifact at that path)",
        "Make one with `icm release <target>`, or pass `--artifact <path>` of an existing artifact.";
    ReleaseNotices = "release.notices", CheckFailed, Fail, Agent,
        "The release does not carry THIRD_PARTY_NOTICES",
        "The target's pipeline puts the notices inside its artifacts (Release::notices, Release::embed_notices); rerun the release.";
    ReleaseLicenceUnknown = "release.licence_unknown", CheckFailed, Warn, Agent,
        "A package the release ships declares no licence",
        "Check the package's licence before shipping it: ask its authors to declare one, or replace the dependency.";
    ReleaseSecretInArtifacts = "release.secret_in_artifacts", CheckFailed, Fail, Agent,
        "A shipped file holds the value of a secret-named variable in icm's environment",
        "Stop the app reading the variable at build time (`option_env!`, `env!`, a build script), or unset it for the release; a value the app must ship, such as a public client key, goes under a name without TOKEN, KEY, SECRET, PASS or PRIVATE.";
    ReleaseArtifactChanged = "release.artifact_changed", CheckFailed, Fail, Agent,
        "A release file differs from what artifacts.json recorded",
        "Something changed the file after the release (a notarization ticket stapled to a macOS app or DMG is the one change verify accepts); rerun `icm release <target>` rather than editing its outputs.";
    StoreNoAgentBridge = "store.no_agent_bridge", CheckFailed, Fail, Agent,
        "A release build contains the agent bridge",
        "Build releases without the `icm-agent` feature.";
    VersionBuildNotIncreased = "version.build_not_increased", CheckFailed, Fail, Agent,
        "`[app] build` was not increased since the last upload",
        "Run `icm version bump build`.";
    VersionFormat = "version.format", Config, Fail, Agent,
        "The Cargo.toml version has a format the stores reject",
        "Use X.Y.Z with no pre-release part.";

    // ---- bridge (phase 6) ------------------------------------------------------------
    BridgeNotCompiled = "bridge.not_compiled", Config, Fail, Agent,
        "The app was built without the agent bridge",
        "Set `[app] agent = true` and rerun `icm run`.";
    BridgeNoConnection = "bridge.no_connection", Device, Fail, Agent,
        "The app's bridge did not connect",
        "Rerun `icm run`; read the logs.";
    BridgeProtocolMismatch = "bridge.protocol_mismatch", Environment, Fail, Agent,
        "The app's bridge speaks a protocol this icm does not",
        "Install the icm that matches the app's framework tag.";
    BridgeSelectorNotFound = "bridge.selector_not_found", CheckFailed, Fail, Agent,
        "No widget matches the selector",
        "Read `icm ui <platform> tree` and adjust the selector.";
}

mod hand_written {
    include!(concat!(env!("OUT_DIR"), "/explain_docs.rs"));
}

/// The hand-written doc for an id, if there is one.
pub fn hand_written(id: &str) -> Option<&'static str> {
    hand_written::EXPLAIN_DOCS
        .iter()
        .find(|(doc_id, _)| *doc_id == id)
        .map(|(_, doc)| *doc)
}

/// Every hand-written doc's id.
pub fn hand_written_ids() -> impl Iterator<Item = &'static str> {
    hand_written::EXPLAIN_DOCS.iter().map(|(id, _)| *id)
}

/// The doc `icm explain <id>` prints: generated from the entry, followed by
/// the hand-written doc when there is one. `hook.<name>` ids get a generic
/// doc.
pub fn explain(id: &str) -> Option<String> {
    if let Some(name) = id.strip_prefix("hook.") {
        return Some(format!(
            "# {id}\n\nA project hook (`[checks]` in icm.toml) named `{name}` reported a failure, or \
             its script exited non-zero.\n\n- Exit code when it fails: 1 (CHECK_FAILED)\n- Who \
             fixes it: {}\n\n## Fix\n\nRead the hook's output in the run directory and fix what it \
             names.\n",
            By::Agent.meaning()
        ));
    }

    let check = CheckId::from_id(id)?;
    let entry = check.entry();
    let mut doc = format!("# {}\n\n{}.\n\n", entry.id, entry.title);

    match entry.level {
        Level::Pass => doc.push_str("- A check that reports success; it never fails.\n"),
        level => {
            doc.push_str(&format!("- Default severity: {}\n", level.name()));
            doc.push_str(&format!(
                "- Exit code when it fails: {} ({})\n",
                entry.exit.code(),
                entry.exit.name()
            ));
            doc.push_str(&format!("- Who fixes it: {}\n", entry.by.meaning()));
        }
    }

    let extra = hand_written(id);
    if !extra.is_some_and(|text| text.contains("## Fix")) {
        doc.push_str(&format!("\n## Fix\n\n{}\n", entry.fix));
    }

    if let Some(extra) = extra {
        doc.push('\n');
        doc.push_str(extra.trim_end());
        doc.push('\n');
    }

    Some(doc)
}

/// The `icm explain exit-codes` table.
pub fn exit_codes_doc() -> String {
    let mut doc = String::from(
        "# Exit codes\n\nStable across icm versions. `ok == (exit == 0)`; `errors[0]` explains a \
         non-zero exit.\n\n| Code | Name | Meaning | What an agent does next |\n|---|---|---|---|\n",
    );
    for exit in Exit::ALL {
        doc.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            exit.code(),
            exit.name(),
            exit.meaning(),
            exit.next()
        ));
    }
    doc.push_str(
        "\n`doctor` is the one exception: it exits 4 if anything with `fix.by` `doctor` or \
         `doctor-yes` remains, else 9 if owner items remain, else the first remaining error's own \
         exit (4 for an agent item such as `config.too_new`), else 0. Its `errors[]` can hold \
         doctor, agent and owner items at once, so follow each one's `fix.by`.\n",
    );
    doc
}

/// A close id for a mistyped one, for `icm explain`'s error.
pub fn suggest(id: &str) -> Option<&'static str> {
    CheckId::ALL
        .iter()
        .map(|check| check.id())
        .filter(|candidate| {
            candidate.contains(id) || id.contains(candidate) || edit_distance(candidate, id) <= 2
        })
        .min_by_key(|candidate| edit_distance(candidate, id))
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut current = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            current[j + 1] = (previous[j] + cost)
                .min(previous[j + 1] + 1)
                .min(current[j] + 1);
        }
        previous = current;
    }
    previous[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn well_formed(id: &str) -> bool {
        let parts: Vec<&str> = id.split('.').collect();
        parts.len() >= 2
            && parts.iter().all(|part| {
                !part.is_empty()
                    && part.starts_with(|c: char| c.is_ascii_lowercase())
                    && part
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            })
    }

    #[test]
    fn ids_are_unique_and_well_formed() {
        let mut seen = BTreeSet::new();
        for check in CheckId::ALL {
            let entry = check.entry();
            assert!(well_formed(entry.id), "malformed id {}", entry.id);
            assert!(seen.insert(entry.id), "duplicate id {}", entry.id);
            assert!(
                !entry.title.is_empty() && !entry.fix.is_empty(),
                "{} lacks text",
                entry.id
            );
            assert!(
                !entry.title.ends_with('.'),
                "{}: titles take no full stop",
                entry.id
            );
            assert_eq!(CheckId::from_id(entry.id), Some(*check));
        }
    }

    #[test]
    fn levels_and_exits_agree() {
        for check in CheckId::ALL {
            let entry = check.entry();
            match entry.level {
                Level::Pass => assert_eq!(entry.exit, Exit::Ok, "{}", entry.id),
                Level::Fail => assert_ne!(entry.exit, Exit::Ok, "{}", entry.id),
                Level::Warn | Level::Info => assert_ne!(entry.exit, Exit::Ok, "{}", entry.id),
            }
            if entry.by == By::Owner && entry.level == Level::Fail {
                assert!(
                    matches!(
                        entry.exit,
                        Exit::NeedsOwner | Exit::CheckFailed | Exit::Environment | Exit::Device
                    ),
                    "{}: owner failures exit 9 (or 1/4/7)",
                    entry.id
                );
            }
        }
    }

    #[test]
    fn every_hand_written_doc_names_a_catalogue_id() {
        let ids: Vec<&str> = hand_written_ids().collect();
        assert!(
            ids.len() >= 20,
            "expected the ~20 common ids to have hand-written docs"
        );
        for id in ids {
            assert!(
                CheckId::from_id(id).is_some(),
                "docs/explain/{id}.md names no catalogue id"
            );
            assert!(
                hand_written(id).is_some_and(|doc| !doc.trim().is_empty()),
                "docs/explain/{id}.md is empty"
            );
        }
    }

    #[test]
    fn every_id_has_a_rendered_doc() {
        for check in CheckId::ALL {
            let doc = explain(check.id()).expect("doc");
            assert!(doc.starts_with(&format!("# {}\n", check.id())));
            assert!(doc.contains("## Fix"));
        }
        assert!(explain("hook.wallet_smoke").is_some());
        assert!(explain("no.such_id").is_none());
    }

    #[test]
    fn exit_code_doc_lists_every_code() {
        let doc = exit_codes_doc();
        for exit in Exit::ALL {
            assert!(doc.contains(&format!("| {} | {} |", exit.code(), exit.name())));
        }
    }

    #[test]
    fn suggestions_find_near_misses() {
        assert_eq!(suggest("config.unknown_keys"), Some("config.unknown_key"));
        assert_eq!(suggest("single_iced"), Some("deps.single_iced"));
        assert_eq!(suggest("zzzzzzzz"), None);
    }
}
