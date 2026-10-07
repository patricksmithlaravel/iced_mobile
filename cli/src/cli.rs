//! The command surface (design §6, cut to phase 1 by Appendix C item 30).

use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use std::time::Duration;

fn duration(value: &str) -> Result<Duration, String> {
    crate::time::parse_duration(value)
}

/// icm: check, build, run, see and test iced_mobile apps on desktop, web,
/// iOS Simulator and Android, from one codebase. Read the last JSON line of
/// every `--json` command; `icm explain <id>` explains any error id.
#[derive(Debug, Parser)]
#[command(
    name = "icm",
    version = crate::buildinfo::VERSION_LINE,
    disable_help_subcommand = true,
    after_help = "Examples:\n  icm run ios-sim --json -q        build, launch, screenshot; print only the result\n  icm explain config.unknown_key   what an error id means and how to fix it\n\nExit codes: 0 ok, 1 check failed, 2 usage, 3 config, 4 environment, 5 build, 6 tool,\n7 device, 8 timeout, 9 owner needed, 10 app died, 70 icm bug, 130 interrupted\n(`icm explain exit-codes`)."
)]
pub struct Cli {
    /// Flags every command takes.
    #[command(flatten)]
    pub global: GlobalArgs,

    /// The command.
    #[command(subcommand)]
    pub command: Command,
}

/// Flags every command takes (design §6 "Global flags").
#[derive(Clone, Debug, Default, Args)]
pub struct GlobalArgs {
    /// NDJSON on stdout; the last line is always the result object [env: ICM_JSON=1]
    #[arg(long, global = true)]
    pub json: bool,

    /// Human: only failures, warnings and RESULT. JSON: only the result line
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// Echo each step's argv to stderr
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// The icm.toml to use (default: the nearest one, walking up from the current directory)
    #[arg(long, global = true, env = "ICM_CONFIG", value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Print the plan; change nothing; exit 0
    #[arg(long, global = true)]
    pub dry_run: bool,

    /// Allow downloads, installs and other machine-state changes (never uploads, never notarizes)
    #[arg(long, global = true)]
    pub yes: bool,

    /// Pass --offline to cargo; refuse anything that needs the network
    #[arg(long, global = true)]
    pub offline: bool,

    /// Overall time limit, e.g. 90s or 10m (for `wait`: how long to wait, default 9m)
    #[arg(long, global = true, env = "ICM_TIMEOUT", value_parser = duration, value_name = "DUR")]
    pub timeout: Option<Duration>,

    /// Treat WARN as FAIL
    #[arg(long, global = true)]
    pub strict: bool,

    /// Wait this long for a held platform lock instead of exiting 7
    #[arg(long, global = true, value_parser = duration, value_name = "DUR")]
    pub wait_lock: Option<Duration>,

    /// Colour in human output (icm prints none today)
    #[arg(long, global = true, value_enum, default_value_t = ColorChoice::Auto)]
    pub color: ColorChoice,

    /// Start the command in the background and return {run, status: "running"} at once;
    /// then call `icm wait <run>` (for builds that outlast an agent's command timeout)
    #[arg(long, global = true)]
    pub detach: bool,
}

/// `--color`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum ColorChoice {
    /// Colour when stdout is a terminal.
    #[default]
    Auto,
    /// Always.
    Always,
    /// Never.
    Never,
}

/// A dev platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, ValueEnum)]
pub enum Platform {
    /// macOS, Linux (Windows later).
    Desktop,
    /// The browser, through a local server and headless Chrome.
    Web,
    /// The iOS Simulator.
    IosSim,
    /// A physical iPhone (phase 2).
    IosDevice,
    /// An Android emulator or device.
    Android,
}

impl Platform {
    /// The platform's name, e.g. `ios-sim`.
    pub fn as_str(self) -> &'static str {
        match self {
            Platform::Desktop => "desktop",
            Platform::Web => "web",
            Platform::IosSim => "ios-sim",
            Platform::IosDevice => "ios-device",
            Platform::Android => "android",
        }
    }

    /// Every platform.
    pub const ALL: [Platform; 5] = [
        Platform::Desktop,
        Platform::Web,
        Platform::IosSim,
        Platform::IosDevice,
        Platform::Android,
    ];
}

/// The commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create an app from the template
    #[command(
        after_help = "Examples:\n  icm new notes --id com.acme.notes\n  icm new demo --framework path:../iced_mobile --no-git"
    )]
    New(NewArgs),

    /// Check the machine for the given platforms; --fix repairs, --fix --yes also downloads
    #[command(
        after_help = "Examples:\n  icm doctor android --json -q\n  icm doctor web ios-sim --fix --yes"
    )]
    Doctor(DoctorArgs),

    /// Validate config and dependencies, and compile every platform without a device
    #[command(after_help = "Examples:\n  icm check --all --json -q\n  icm check android --clippy")]
    Check(CheckArgs),

    /// Build the runnable dev bundle for a platform and stop there
    #[command(
        after_help = "Examples:\n  icm build android --detach   # then: icm wait <run>\n  icm build --all --detach"
    )]
    Build(BuildArgs),

    /// Build, install, launch, wait for the first frame, screenshot, and return
    #[command(
        after_help = "Examples:\n  icm run ios-sim --json -q\n  icm run android --device emulator-5580 --detach"
    )]
    Run(RunArgs),

    /// Stop the app (and with --shutdown, icm's simulators and emulators)
    #[command(after_help = "Examples:\n  icm stop web\n  icm stop --all --shutdown")]
    Stop(StopArgs),

    /// List running sessions
    Ps,

    /// List devices, simulators and emulators
    Devices(DevicesArgs),

    /// Screenshot the running app, or render the real view headlessly
    #[command(
        after_help = "Examples:\n  icm shot android\n  icm shot --headless --all-viewports --json -q"
    )]
    Shot(ShotArgs),

    /// Re-read the app's live logs from the launch mark
    #[command(
        after_help = "Examples:\n  icm logs ios-sim --level warn --json\n  icm logs android --grep panic --tail 50"
    )]
    Logs(LogsArgs),

    /// Send input to the running app, in screenshot-preview pixels by default
    #[command(
        after_help = "Examples:\n  icm input android tap 200 400\n  icm input web text \"hello\""
    )]
    Input(InputArgs),

    /// Inspect the UI through the app's headless harness
    #[command(
        after_help = "Examples:\n  icm ui --headless tree --json\n  icm ui --headless find \"Count: 1\""
    )]
    Ui(UiArgs),

    /// Run unit tests and the .ice flows headlessly
    #[command(
        after_help = "Examples:\n  icm test --json -q\n  icm test --filter smoke\n  icm test --on android --lifecycle --json -q"
    )]
    Test(TestArgs),

    /// Explain an error or check id, the exit codes, or list the catalogue
    #[command(
        after_help = "Examples:\n  icm explain run.app_panicked\n  icm explain --list --json"
    )]
    Explain(ExplainArgs),

    /// Wait for a detached run (--detach) and print its result; call it again while it runs
    #[command(
        after_help = "Examples:\n  icm wait 20261006T210311Z-build-android-7f3a --timeout 9m --json -q\n  icm wait target/icm/runs/<run-id>"
    )]
    Wait(WaitArgs),

    /// Print the resolved config, environment, paths, tools, a plan or the command surface
    #[command(
        after_help = "Examples:\n  eval \"$(icm print env android)\"\n  icm print tools --json -q"
    )]
    Print(PrintArgs),

    /// Remove icm outputs
    Clean(CleanArgs),

    /// Build store-gated release artifacts, artifacts.json, UPLOAD.md and upload.sh (never uploads)
    #[command(
        after_help = "Examples:\n  icm release ios --sign none --allow-dirty --json -q   # unsigned, for CI and agents\n  icm release android --json -q                        # signed; exit 9 hands errors[0].fix to the owner\n\nOutputs go to target/icm/dist/<version>+<build>/<target>/ (dist/latest/<target> points at the newest).\nicm never uploads, publishes or notarizes: the owner runs UPLOAD.md or upload.sh."
    )]
    Release(ReleaseArgs),

    /// Run the store gates on a release artifact
    #[command(
        after_help = "Examples:\n  icm verify ios --json -q                         # the newest release in dist/latest/ios\n  icm verify android --artifact app-1.0.0-12.aab   # any artifact; artifacts.json next to it sets the severities"
    )]
    Verify(VerifyArgs),

    /// Print UPLOAD.md of the newest release of a target
    #[command(
        name = "upload-commands",
        after_help = "Examples:\n  icm upload-commands ios\n  icm upload-commands web --json -q"
    )]
    UploadCommands(UploadCommandsArgs),

    /// The record of store uploads: show it, or mark a build uploaded (upload.sh's last line)
    #[command(
        after_help = "Examples:\n  icm ledger show\n  icm ledger mark-uploaded ios --build 12   # the owner, after the upload"
    )]
    Ledger(LedgerArgs),

    /// Map a store tool's saved output (altool, notarytool, Play) to catalogue ids
    #[command(
        after_help = "Examples:\n  icm diagnose altool target/icm/dist/1.0.0+12/ios/upload.json\n  xcrun notarytool log <id> | icm diagnose notarytool -"
    )]
    Diagnose(DiagnoseArgs),

    /// Internal: the detached session host `run` spawns
    #[command(name = "__session", hide = true)]
    Session(RawArgs),

    /// Internal: scenarios for icm's own tests
    #[command(name = "__test", hide = true)]
    SelfTest(SelfTestArgs),

    /// Commands from later phases, and unknown commands
    #[command(external_subcommand)]
    External(Vec<String>),
}

/// The commands planned for later phases (design §6, Appendix C item 30).
pub const LATER_COMMANDS: &[&str] = &["init", "version", "framework", "docs", "ci", "self", "mcp"];

impl Command {
    /// The command's name and target, for run ids and results.
    pub fn name_and_target(&self) -> (String, Option<String>) {
        let platform = |p: &Option<Platform>| p.map(|p| p.as_str().to_string());
        match self {
            Command::New(_) => ("new".into(), None),
            Command::Doctor(args) => ("doctor".into(), join_platforms(&args.platforms)),
            Command::Check(args) => (
                "check".into(),
                if args.all {
                    Some("all".into())
                } else {
                    join_platforms(&args.platforms)
                },
            ),
            Command::Build(args) => (
                "build".into(),
                if args.all {
                    Some("all".into())
                } else {
                    platform(&args.platform)
                },
            ),
            Command::Run(args) => ("run".into(), Some(args.platform.as_str().into())),
            Command::Stop(args) => (
                "stop".into(),
                if args.all {
                    Some("all".into())
                } else {
                    platform(&args.platform)
                },
            ),
            Command::Ps => ("ps".into(), None),
            Command::Devices(args) => ("devices".into(), platform(&args.platform)),
            Command::Shot(args) => (
                "shot".into(),
                if args.headless {
                    Some("headless".into())
                } else {
                    platform(&args.platform)
                },
            ),
            Command::Logs(args) => ("logs".into(), Some(args.platform.as_str().into())),
            Command::Input(args) => ("input".into(), Some(args.platform.as_str().into())),
            Command::Ui(_) => ("ui".into(), Some("headless".into())),
            Command::Test(args) => ("test".into(), platform(&args.device())),
            Command::Explain(_) => ("explain".into(), None),
            Command::Wait(_) => ("wait".into(), None),
            Command::Print(args) => ("print".into(), Some(args.what.name().into())),
            Command::Clean(args) => ("clean".into(), platform(&args.platform)),
            Command::Release(args) => ("release".into(), Some(args.target.as_str().into())),
            Command::Verify(args) => ("verify".into(), Some(args.target.as_str().into())),
            Command::UploadCommands(args) => {
                ("upload-commands".into(), Some(args.target.as_str().into()))
            }
            Command::Ledger(args) => match &args.action {
                LedgerAction::Show => ("ledger".into(), Some("show".into())),
                LedgerAction::MarkUploaded { target, .. } => {
                    ("ledger".into(), Some(target.as_str().into()))
                }
            },
            Command::Diagnose(args) => ("diagnose".into(), Some(args.tool.as_str().into())),
            Command::Session(_) => ("session".into(), None),
            Command::SelfTest(args) => ("selftest".into(), Some(args.scenario.name().into())),
            Command::External(args) => {
                (args.first().cloned().unwrap_or_else(|| "icm".into()), None)
            }
        }
    }

    /// Whether the command only shows something (no run directory).
    pub fn is_view(&self) -> bool {
        matches!(
            self,
            Command::Explain(_)
                | Command::Wait(_)
                | Command::Print(_)
                | Command::Ps
                | Command::Session(_)
                | Command::UploadCommands(_)
                | Command::Ledger(LedgerArgs {
                    action: LedgerAction::Show
                })
        )
    }

    /// Whether the command prints content on stdout in human mode.
    pub fn is_content(&self) -> bool {
        matches!(
            self,
            Command::Explain(_)
                | Command::Print(_)
                | Command::Ps
                | Command::Ui(_)
                | Command::UploadCommands(_)
                | Command::Ledger(LedgerArgs {
                    action: LedgerAction::Show
                })
        )
    }
}

fn join_platforms(platforms: &[Platform]) -> Option<String> {
    if platforms.is_empty() {
        None
    } else {
        Some(
            platforms
                .iter()
                .map(|p| p.as_str())
                .collect::<Vec<_>>()
                .join("+"),
        )
    }
}

/// `icm new`.
#[derive(Debug, Args)]
pub struct NewArgs {
    /// The directory to create
    pub dir: PathBuf,
    /// The display name (default: from the directory)
    #[arg(long)]
    pub name: Option<String>,
    /// The reverse-DNS id (default: com.example.<name>, a placeholder)
    #[arg(long)]
    pub id: Option<String>,
    /// The framework source: tag:<t> | rev:<sha> | path:<dir>
    #[arg(long, value_name = "SOURCE")]
    pub framework: Option<String>,
    /// Do not run git init
    #[arg(long)]
    pub no_git: bool,
    /// Write into a non-empty directory
    #[arg(long)]
    pub force: bool,
}

/// `icm doctor`.
#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Platforms to check (default: those in [app] platforms, or all)
    #[arg(value_enum)]
    pub platforms: Vec<Platform>,
    /// Apply local, idempotent fixes (with --yes: downloads and installs too)
    #[arg(long)]
    pub fix: bool,
}

/// `icm check`.
#[derive(Debug, Args)]
pub struct CheckArgs {
    /// Platforms to compile
    #[arg(value_enum)]
    pub platforms: Vec<Platform>,
    /// Every platform in [app] platforms
    #[arg(long)]
    pub all: bool,
    /// Use the release profile
    #[arg(long)]
    pub release: bool,
    /// Run clippy instead of cargo check
    #[arg(long)]
    pub clippy: bool,
}

/// `icm build`.
#[derive(Debug, Args)]
pub struct BuildArgs {
    /// The platform
    #[arg(value_enum)]
    pub platform: Option<Platform>,
    /// Every platform in [app] platforms
    #[arg(long)]
    pub all: bool,
    /// Use the release profile
    #[arg(long)]
    pub release: bool,
    /// The device (its ABI decides the Android target)
    #[arg(long)]
    pub device: Option<String>,
    /// The Android ABI to build
    #[arg(long)]
    pub abi: Option<String>,
}

/// `icm run`.
#[derive(Debug, Args)]
pub struct RunArgs {
    /// The platform
    #[arg(value_enum)]
    pub platform: Platform,
    /// Use the release profile
    #[arg(long)]
    pub release: bool,
    /// Launch the last build
    #[arg(long)]
    pub no_build: bool,
    /// The device serial or simulator UDID
    #[arg(long)]
    pub device: Option<String>,
    /// The simulator (name or UDID)
    #[arg(long)]
    pub sim: Option<String>,
    /// The AVD to boot
    #[arg(long)]
    pub avd: Option<String>,
    /// Use a new simulator, deleted at `stop`
    #[arg(long)]
    pub fresh: bool,
    /// iOS Simulator runtime: newest (default), min (the lowest at or above [ios] min_os) or a version such as 18.3
    #[arg(long, value_name = "newest|min|X.Y")]
    pub runtime: Option<String>,
    /// Show the simulator, emulator or browser window
    #[arg(long)]
    pub show: bool,
    /// Extra app environment (repeatable)
    #[arg(long = "env", value_name = "K=V")]
    pub env: Vec<String>,
    /// How long to wait for the first frame
    #[arg(long, value_parser = duration, default_value = "30s", value_name = "DUR")]
    pub wait_ready: Duration,
    /// How long to wait after the first frame before the screenshot
    #[arg(long, value_parser = duration, default_value = "1.5s", value_name = "DUR")]
    pub settle: Duration,
    /// Skip the screenshot
    #[arg(long)]
    pub no_shot: bool,
    /// FAIL (not WARN) when the screenshot is a single colour
    #[arg(long)]
    pub expect_content: bool,
    /// Uninstall first (with --wipe-data on Android when the signature changed)
    #[arg(long)]
    pub reinstall: bool,
    /// Allow --reinstall to wipe the app's data
    #[arg(long)]
    pub wipe_data: bool,
    /// The web viewport: a preset or WxH
    #[arg(long, value_name = "PRESET|WxH")]
    pub viewport: Option<String>,
    /// The web server port (0: any free port)
    #[arg(long, default_value_t = 8787)]
    pub port: u16,
    /// Stay in the foreground and stream logs until the app exits or Ctrl-C
    #[arg(long)]
    pub attach: bool,
    /// Android: install through an .aab and bundletool
    #[arg(long)]
    pub from_aab: bool,
    /// iOS Simulator: a store-size iPhone (the newest Pro Max), for `icm shot ios-sim --store`
    #[arg(long)]
    pub store: bool,
}

/// `icm stop`.
#[derive(Debug, Args)]
pub struct StopArgs {
    /// The platform
    #[arg(value_enum)]
    pub platform: Option<Platform>,
    /// Every platform
    #[arg(long)]
    pub all: bool,
    /// Also shut down icm-managed simulators and emulators
    #[arg(long)]
    pub shutdown: bool,
}

/// `icm devices`.
#[derive(Debug, Args)]
pub struct DevicesArgs {
    /// Only this platform
    #[arg(value_enum)]
    pub platform: Option<Platform>,
}

/// `icm shot`.
#[derive(Debug, Args)]
pub struct ShotArgs {
    /// The platform whose running app to capture
    #[arg(value_enum)]
    pub platform: Option<Platform>,
    /// Render through the app's test harness instead (no device)
    #[arg(long)]
    pub headless: bool,
    /// Where to write the PNG
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// A label for the screenshot
    #[arg(long)]
    pub name: Option<String>,
    /// Headless viewports: presets or WxH[@scale] (repeatable)
    #[arg(long, value_name = "PRESET|WxH[@SCALE]")]
    pub viewport: Vec<String>,
    /// Every viewport in [test] viewports
    #[arg(long)]
    pub all_viewports: bool,
    /// The theme for headless renders
    #[arg(long, value_enum)]
    pub theme: Option<Theme>,
    /// A named app state for headless renders
    #[arg(long)]
    pub preset: Option<String>,
    /// How long the headless render waits
    #[arg(long, value_parser = duration, default_value = "500ms", value_name = "DUR")]
    pub wait: Duration,
    /// Where headless renders go
    #[arg(long)]
    pub out_dir: Option<PathBuf>,
    /// iOS Simulator: keep the capture as an App Store screenshot (needs `icm run ios-sim --store`)
    #[arg(long)]
    pub store: bool,
}

/// A theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Theme {
    /// Light.
    Light,
    /// Dark.
    Dark,
}

/// A log level.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub enum Level {
    /// Everything.
    Trace,
    /// Debug and up.
    Debug,
    /// Info and up.
    Info,
    /// Warnings and errors.
    Warn,
    /// Errors.
    Error,
}

/// A log source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum LogSource {
    /// The app's own output.
    App,
    /// The system log.
    System,
    /// Crash reports.
    Crash,
    /// Everything.
    All,
}

/// `icm logs`.
#[derive(Debug, Args)]
pub struct LogsArgs {
    /// The platform
    #[arg(value_enum)]
    pub platform: Platform,
    /// From the launch mark, or a duration ago
    #[arg(long, default_value = "launch")]
    pub since: String,
    /// The lowest level shown
    #[arg(long, value_enum)]
    pub level: Option<Level>,
    /// Which sources
    /// (default: `all`; on ios-sim `app` and `crash`, because its system
    /// log is mostly other processes' errors that mention the app)
    #[arg(long, value_enum)]
    pub source: Option<LogSource>,
    /// Only records whose tag or message contains one of these
    /// `|`-separated substrings, ignoring case (not a regular expression):
    /// `--grep 'panic|error'`
    #[arg(long)]
    pub grep: Option<String>,
    /// The last N records
    #[arg(long, default_value_t = 200)]
    pub tail: usize,
    /// Stream until Ctrl-C
    #[arg(long)]
    pub follow: bool,
    /// Print the original files
    #[arg(long)]
    pub raw: bool,
}

/// The coordinate space of `icm input` (Appendix C item 25).
pub use crate::screen::Space;

/// `icm input`.
#[derive(Debug, Args)]
pub struct InputArgs {
    /// The platform
    #[arg(value_enum)]
    pub platform: Platform,
    /// The coordinate space of x and y
    #[arg(long, value_enum, default_value_t = Space::Preview, global = true)]
    pub space: Space,
    /// The action
    #[command(subcommand)]
    pub action: InputAction,
}

/// An input action.
#[derive(Debug, Subcommand)]
pub enum InputAction {
    /// Tap at x y
    Tap {
        /// x
        x: f64,
        /// y
        y: f64,
    },
    /// Swipe from x1 y1 to x2 y2
    Swipe {
        /// Start x
        x1: f64,
        /// Start y
        y1: f64,
        /// End x
        x2: f64,
        /// End y
        y2: f64,
        /// Duration in milliseconds
        ms: Option<u64>,
    },
    /// Type text
    Text {
        /// The text
        text: String,
    },
    /// Press a key
    Key {
        /// The key
        #[arg(value_enum)]
        key: Key,
    },
    /// Switch light or dark appearance
    Appearance {
        /// The appearance
        #[arg(value_enum)]
        mode: Theme,
    },
    /// Rotate the device
    Rotate {
        /// The orientation
        #[arg(value_enum)]
        orientation: Rotation,
    },
    /// Set the system font scale
    FontScale {
        /// The scale, e.g. 1.3
        scale: f64,
    },
    /// Send the app to the background
    Background,
    /// Bring the app back
    Foreground,
}

/// A key for `icm input key`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Key {
    /// Back.
    Back,
    /// Home.
    Home,
    /// Enter.
    Enter,
    /// Tab.
    Tab,
    /// Escape.
    Escape,
}

/// An orientation for `icm input rotate`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Rotation {
    /// Portrait.
    Portrait,
    /// Landscape.
    Landscape,
}

/// `icm ui`.
#[derive(Debug, Args)]
pub struct UiArgs {
    /// Use the app's headless harness (the only mode in phase 1)
    #[arg(long)]
    pub headless: bool,
    /// The viewport for `tree` and `find`: a preset (iphone-17, pixel-9,
    /// ...) or WxH[@scale]; default the first of `[test] viewports`
    #[arg(long, global = true)]
    pub viewport: Option<String>,
    /// The query
    #[command(subcommand)]
    pub action: UiAction,
}

/// A UI query.
#[derive(Debug, Subcommand)]
pub enum UiAction {
    /// The widget tree with bounds
    Tree,
    /// Widgets matching a selector
    Find {
        /// The selector (text or id)
        selector: String,
    },
    /// Run an .ice flow
    Ice {
        /// The .ice file
        file: PathBuf,
    },
}

/// `icm test`.
#[derive(Debug, Args)]
pub struct TestArgs {
    /// The platform's device to test on (same as --on)
    #[arg(value_enum, conflicts_with = "on")]
    pub platform: Option<Platform>,
    /// Only host tests (unit tests and flows); the default
    #[arg(long)]
    pub host: bool,
    /// Only tests matching this pattern
    #[arg(long)]
    pub filter: Option<String>,
    /// Run on a platform's device instead
    #[arg(long, value_enum)]
    pub on: Option<Platform>,
    /// The lifecycle suite (with --on)
    #[arg(long)]
    pub lifecycle: bool,
}

impl TestArgs {
    /// The device platform: `--on` or the positional platform.
    pub fn device(&self) -> Option<Platform> {
        self.on.or(self.platform)
    }
}

/// `icm explain`.
#[derive(Debug, Args)]
pub struct ExplainArgs {
    /// A check or error id (e.g. run.app_panicked), or `exit-codes`
    pub id: Option<String>,
    /// List the whole catalogue
    #[arg(long)]
    pub list: bool,
}

/// `icm wait`.
#[derive(Debug, Args)]
pub struct WaitArgs {
    /// The run id printed by --detach, or its run directory
    pub run: String,
}

/// `icm print`.
#[derive(Debug, Args)]
pub struct PrintArgs {
    /// What to print
    #[command(subcommand)]
    pub what: PrintWhat,
}

/// What `icm print` prints.
#[derive(Debug, Subcommand)]
pub enum PrintWhat {
    /// The resolved icm.toml (with defaults)
    Config,
    /// Shell exports for running raw cargo or tools for a platform
    Env {
        /// The platform
        #[arg(value_enum)]
        platform: Platform,
    },
    /// The project's icm paths
    Paths,
    /// The discovered tools (SDKs, JDK, Xcode, Chrome, toolchain)
    Tools,
    /// The plan a command would run (same as adding --dry-run)
    Plan {
        /// The command and its arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        command: Vec<String>,
    },
    /// The whole command surface
    Commands,
    /// The dated store policy table the release gates enforce
    Policy,
}

impl PrintWhat {
    /// The subcommand's name.
    pub fn name(&self) -> &'static str {
        match self {
            PrintWhat::Config => "config",
            PrintWhat::Env { .. } => "env",
            PrintWhat::Paths => "paths",
            PrintWhat::Tools => "tools",
            PrintWhat::Plan { .. } => "plan",
            PrintWhat::Commands => "commands",
            PrintWhat::Policy => "policy",
        }
    }
}

/// `icm clean`.
#[derive(Debug, Args)]
pub struct CleanArgs {
    /// Only this platform's outputs
    #[arg(value_enum)]
    pub platform: Option<Platform>,
    /// Only prune old run directories
    #[arg(long)]
    pub runs: bool,
}

/// A release target (design §6 "Release targets").
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, ValueEnum)]
pub enum ReleaseTarget {
    /// The App Store (an .ipa).
    Ios,
    /// Google Play (an .aab).
    Android,
    /// A static web host (a site).
    Web,
    /// macOS (.app and .dmg, notarized by the owner).
    Macos,
    /// Windows (.msi and NSIS .exe).
    Windows,
    /// Linux (.deb and AppImage).
    Linux,
}

impl ReleaseTarget {
    /// The target's name, e.g. `ios`.
    pub fn as_str(self) -> &'static str {
        match self {
            ReleaseTarget::Ios => "ios",
            ReleaseTarget::Android => "android",
            ReleaseTarget::Web => "web",
            ReleaseTarget::Macos => "macos",
            ReleaseTarget::Windows => "windows",
            ReleaseTarget::Linux => "linux",
        }
    }

    /// Every target.
    pub const ALL: [ReleaseTarget; 6] = [
        ReleaseTarget::Ios,
        ReleaseTarget::Android,
        ReleaseTarget::Web,
        ReleaseTarget::Macos,
        ReleaseTarget::Windows,
        ReleaseTarget::Linux,
    ];
}

/// `--sign`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum SignMode {
    /// Sign with the configured assets; missing ones exit 9 with the owner's steps.
    #[default]
    Auto,
    /// Unsigned artifacts (`uploadable: false`): owner-dependent checks become WARNs.
    None,
}

impl SignMode {
    /// The mode's name.
    pub fn as_str(self) -> &'static str {
        match self {
            SignMode::Auto => "auto",
            SignMode::None => "none",
        }
    }
}

/// `icm release`.
#[derive(Clone, Debug, Args)]
pub struct ReleaseArgs {
    /// The target
    #[arg(value_enum)]
    pub target: ReleaseTarget,
    /// auto: sign (exit 9 when the owner's assets are missing); none: unsigned, owner items WARN
    #[arg(long, value_enum, default_value_t = SignMode::Auto)]
    pub sign: SignMode,
    /// Release from a git tree with uncommitted changes (artifacts.json records it)
    #[arg(long)]
    pub allow_dirty: bool,
    /// Android: skip the smoke install on a device or emulator
    #[arg(long)]
    pub no_smoke: bool,
    /// Android: also build a universal APK for sideloading
    #[arg(long)]
    pub apk: bool,
    /// macOS: stage 2, the DMG of the notarized and stapled app
    #[arg(long)]
    pub dmg: bool,
    /// macOS: arm64 and x86_64 in one binary
    #[arg(long)]
    pub universal: bool,
    /// iOS: package through `xcodebuild -exportArchive` (the fallback)
    #[arg(long)]
    pub via_xcode_export: bool,
}

/// `icm verify`.
#[derive(Clone, Debug, Args)]
pub struct VerifyArgs {
    /// The target
    #[arg(value_enum)]
    pub target: ReleaseTarget,
    /// The artifact (default: the upload file of the newest release of the target)
    #[arg(long, value_name = "PATH")]
    pub artifact: Option<PathBuf>,
    /// macOS: also check notarization and Gatekeeper (spctl, stapler)
    #[arg(long)]
    pub after_notarize: bool,
    /// Web: check the deployed site at this URL instead
    #[arg(long, value_name = "URL")]
    pub url: Option<String>,
}

/// `icm upload-commands`.
#[derive(Clone, Debug, Args)]
pub struct UploadCommandsArgs {
    /// The target
    #[arg(value_enum)]
    pub target: ReleaseTarget,
}

/// `icm ledger`.
#[derive(Clone, Debug, Args)]
pub struct LedgerArgs {
    /// What to do
    #[command(subcommand)]
    pub action: LedgerAction,
}

/// A ledger action.
#[derive(Clone, Debug, Subcommand)]
pub enum LedgerAction {
    /// List the recorded uploads
    Show,
    /// Record that the owner uploaded a build (upload.sh's last line)
    MarkUploaded {
        /// The target
        #[arg(value_enum)]
        target: ReleaseTarget,
        /// The build number (default: the newest release of the target)
        #[arg(long)]
        build: Option<u64>,
    },
}

/// A store tool whose output `icm diagnose` reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum DiagnoseTool {
    /// `xcrun altool --output-format json`.
    Altool,
    /// `xcrun notarytool --output-format json` (or its log).
    Notarytool,
    /// Google Play's upload errors (from fastlane or the Play Console).
    Play,
}

impl DiagnoseTool {
    /// The tool's name.
    pub fn as_str(self) -> &'static str {
        match self {
            DiagnoseTool::Altool => "altool",
            DiagnoseTool::Notarytool => "notarytool",
            DiagnoseTool::Play => "play",
        }
    }
}

/// `icm diagnose`.
#[derive(Clone, Debug, Args)]
pub struct DiagnoseArgs {
    /// The tool
    #[arg(value_enum)]
    pub tool: DiagnoseTool,
    /// Its saved output, or `-` for stdin
    pub file: String,
}

/// Arguments passed through untouched.
#[derive(Debug, Args)]
pub struct RawArgs {
    /// The arguments
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

/// `icm __test`.
#[derive(Debug, Args)]
pub struct SelfTestArgs {
    /// The scenario
    #[command(subcommand)]
    pub scenario: Scenario,
}

/// icm's own test scenarios.
#[derive(Debug, Subcommand)]
pub enum Scenario {
    /// Run a child (and grandchild) that sleeps; records their pids in the run dir
    Sleep {
        /// How long
        #[arg(value_parser = duration)]
        duration: Duration,
    },
    /// Panic
    Panic,
    /// Fail with an id
    Fail {
        /// The id
        id: String,
    },
    /// Report a passing, a warning and (with --fail) a failing check
    Checks {
        /// Also report a non-blocking FAIL
        #[arg(long)]
        fail: bool,
    },
    /// Build and run (or with --dry-run, print) a two-step plan
    Plan,
    /// Resolve the project and report it
    Project,
    /// Hold the platform lock for a while
    Lock {
        /// The platform
        #[arg(value_enum)]
        platform: Platform,
        /// How long
        #[arg(value_parser = duration)]
        duration: Duration,
    },
    /// Prepare the iOS Simulator deployment target and write its stamp
    Deployment {
        /// The minimum iOS version
        min_os: String,
    },
    /// Stay busy without looking at signals (exercises the watchdog)
    Busy {
        /// How long
        #[arg(value_parser = duration)]
        duration: Duration,
    },
    /// Run the project's [checks] hooks for a platform, as `run` does after a launch
    Hooks {
        /// The platform
        #[arg(value_enum)]
        platform: Platform,
    },
    /// Find a pinned tool from tools.toml, installing it with --yes
    Pinned {
        /// The tool's name
        name: String,
    },
    /// `icm release` with a stand-in pipeline that writes a small file
    Release(ReleaseArgs),
    /// `icm verify` with the stand-in pipeline's gates
    Verify(VerifyArgs),
    /// One release cargo build of the app's binary for a target (profile, dedicated target dir, stamps)
    ReleaseBuild {
        /// The target
        #[arg(value_enum)]
        target: ReleaseTarget,
        /// The deployment target (Apple triples)
        #[arg(long)]
        min_os: Option<String>,
    },
}

impl Scenario {
    /// The scenario's name.
    pub fn name(&self) -> &'static str {
        match self {
            Scenario::Sleep { .. } => "sleep",
            Scenario::Panic => "panic",
            Scenario::Fail { .. } => "fail",
            Scenario::Checks { .. } => "checks",
            Scenario::Plan => "plan",
            Scenario::Project => "project",
            Scenario::Lock { .. } => "lock",
            Scenario::Deployment { .. } => "deployment",
            Scenario::Busy { .. } => "busy",
            Scenario::Hooks { .. } => "hooks",
            Scenario::Pinned { .. } => "pinned",
            Scenario::Release(_) => "release",
            Scenario::Verify(_) => "verify",
            Scenario::ReleaseBuild { .. } => "release-build",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_surface_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn global_flags_go_anywhere() {
        let cli = Cli::try_parse_from(["icm", "run", "ios-sim", "--json", "-q"]).unwrap();
        assert!(cli.global.json && cli.global.quiet);
        assert!(matches!(
            cli.command,
            Command::Run(RunArgs {
                platform: Platform::IosSim,
                ..
            })
        ));

        let cli = Cli::try_parse_from(["icm", "--detach", "build", "android"]).unwrap();
        assert!(cli.global.detach);
    }

    #[test]
    fn unknown_platforms_are_usage_errors() {
        let error = Cli::try_parse_from(["icm", "run", "nowhere"]).unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::InvalidValue);
    }

    #[test]
    fn later_commands_parse_as_external() {
        let cli = Cli::try_parse_from(["icm", "version", "show"]).unwrap();
        match cli.command {
            Command::External(args) => assert_eq!(args, vec!["version", "show"]),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn release_commands_parse() {
        let cli = Cli::try_parse_from(["icm", "release", "ios", "--sign", "none", "--allow-dirty"])
            .unwrap();
        match cli.command {
            Command::Release(args) => {
                assert_eq!(args.target, ReleaseTarget::Ios);
                assert_eq!(args.sign, SignMode::None);
                assert!(args.allow_dirty);
            }
            other => panic!("{other:?}"),
        }
        let cli = Cli::try_parse_from(["icm", "release", "android"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Release(ReleaseArgs {
                sign: SignMode::Auto,
                ..
            })
        ));
        assert!(Cli::try_parse_from(["icm", "release", "tvos"]).is_err());

        let cli = Cli::try_parse_from(["icm", "ledger", "mark-uploaded", "ios", "--build", "12"])
            .unwrap();
        assert_eq!(
            cli.command.name_and_target(),
            ("ledger".to_string(), Some("ios".to_string()))
        );
        assert!(!cli.command.is_view());
        let show = Cli::try_parse_from(["icm", "ledger", "show"]).unwrap();
        assert!(show.command.is_view() && show.command.is_content());
        let upload = Cli::try_parse_from(["icm", "upload-commands", "web"]).unwrap();
        assert!(upload.command.is_view() && upload.command.is_content());
        let verify =
            Cli::try_parse_from(["icm", "verify", "web", "--url", "https://x.dev"]).unwrap();
        assert_eq!(
            verify.command.name_and_target(),
            ("verify".to_string(), Some("web".to_string()))
        );
    }

    #[test]
    fn input_space_defaults_to_preview() {
        let cli = Cli::try_parse_from(["icm", "input", "android", "tap", "200", "400"]).unwrap();
        match cli.command {
            Command::Input(args) => {
                assert_eq!(args.space, Space::Preview);
                assert!(
                    matches!(args.action, InputAction::Tap { x, y } if x == 200.0 && y == 400.0)
                );
            }
            other => panic!("{other:?}"),
        }
        let cli =
            Cli::try_parse_from(["icm", "input", "android", "tap", "1", "2", "--space", "px"])
                .unwrap();
        assert!(matches!(
            cli.command,
            Command::Input(InputArgs {
                space: Space::Px,
                ..
            })
        ));
    }

    #[test]
    fn durations_parse_in_flags() {
        let cli = Cli::try_parse_from(["icm", "wait", "x", "--timeout", "9m"]).unwrap();
        assert_eq!(cli.global.timeout, Some(Duration::from_secs(540)));
        assert!(Cli::try_parse_from(["icm", "wait", "x", "--timeout", "soon"]).is_err());
    }
}
