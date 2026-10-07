# icm

The iced_mobile app tool. The design is `docs/icm/DESIGN.md` (Appendix C
overrides earlier sections; Appendix D records what the code decided).

```sh
cargo install --locked --path cli          # from a fork checkout
cargo run --manifest-path cli/Cargo.toml -- explain --list
```

`cli/` is its own workspace with its own `Cargo.lock`; the root
`Cargo.toml` excludes it, so CLI dependencies never enter the framework's
lock. It links no iced crate.

## Checks for every change

```sh
cd cli
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## Layout

| Module | What it owns |
|---|---|
| `lib.rs` | entry: parse, `--detach`, reporter, panic hook (exit 70), signal watchdog |
| `cli.rs` | the clap surface; later-phase commands parse as `External` |
| `commands/` | one file per command; `mod.rs` dispatches (unimplemented ones exit 2 `usage.not_implemented`) |
| `context.rs` | `Ctx` (flags, reporter, env, deadline, host.toml, project) and `Project` (icm.toml + `cargo metadata` + paths) |
| `output/` | the output contract: `Reporter`, human lines (`human.rs`), run dirs (`rundir.rs`) |
| `catalogue.rs` | every check/error id with exit code, level, `by`, title, fix; `docs/explain/<id>.md` adds detail |
| `error.rs` | `IcmError` (one `errors[]` entry), `Check`, `Evidence`, `Fix`, `Diagnostic` |
| `exit.rs` | the stable exit codes |
| `process.rs` | the runner: stdin null, process groups, timeouts, file-backed output, redaction |
| `signals.rs` | SIGINT/SIGTERM/SIGHUP: record, kill registered groups |
| `plan.rs` | `Plan`/`Step`; `--dry-run` prints, `execute` runs |
| `config/` | icm.toml types, discovery, validation with `file:line` (`source.rs` keeps the spans) |
| `host.rs` | `~/.config/icm/host.toml` |
| `tools.rs` | discovery: Android SDK/NDK, JDK 17+, Xcode, Chrome, wasm-bindgen; `android_env`, `ndk_env` |
| `toolchain.rs` | the project's active toolchain and its installed targets |
| `cargo.rs` | `cargo metadata`, `Cargo.lock`, cargo invocations and JSON messages, deployment-target stamps |
| `deps.rs` | lockfile checks (`deps.single_iced`, ...) |
| `locks.rs` | per-project, per-platform locks |
| `screen.rs` | preview/px/pt coordinates for `shot` and `input` |
| `template.rs` | the embedded `examples/app` and how `icm new` fills it; `icm explain config.<key>` |
| `doctor/` | what each platform needs (`gather`) and the fixes `doctor --fix [--yes]` runs (`fix.rs`) |
| `managed.rs`, `simctl.rs` | the `icm-` simulator and AVD names, Android's per-user dirs; `simctl list -j` parsing |
| `session.rs` | `target/icm/sessions/<platform>.json`, which `run` writes and `stop`/`ps` read |
| `image.rs` | PNG decode/encode, previews, cropping, blank detection |
| `platform/desktop/` | `build`/`run`/`shot`/`logs`/`stop desktop`: launch, readiness, window capture (`macos.rs` FFI, `linux.rs` X11), headless fallback (`headless.rs`), log records (`logs.rs`) |
| `platform/ios_sim/` | `build`/`run`/`logs`/`shot`/`stop`/`input ios-sim`: simulator choice (`simctl.rs`), bundle and plists (`bundle.rs`, `plist.rs`), Mach-O gates (`macho.rs`), PNG preview and blank detection (`image.rs`), log normalization (`logs.rs`), the session file (`session.rs`); fake-tool tests in `tests/ios_sim.rs` |
| `preview.rs` | after a capture: PNG decode, `screen.preview.png`, blank detection (`run.screen_blank`) |
| `sessions.rs` | `target/icm/sessions/<platform>.json` records: write (mode 0600), list, alive (pid plus a command-line marker), terminate |
| `web/` | the web platform: build, wasm-bindgen and the site (`site.rs`); the detached session host (`host.rs`) with its std server (`server.rs`), headless Chrome over `--remote-debugging-pipe` (`cdp.rs`) and console capture (`console.rs`); the control client (`client.rs`); viewports (`viewport.rs`) |
| `version.rs`, `buildinfo.rs`, `gitinfo.rs` | version ordering, what the build embedded, the default framework pin |
| `android/` | `build`/`run`/`stop`/`shot`/`logs`/`input`/`devices`/`doctor` for Android: APK pipeline, managed AVD, adb, logcat, session (`android/mod.rs` has the module map) |

## Writing a command

1. Add the variant and its `Args` to `cli.rs` (two examples in `after_help`).
2. Add `commands/<name>.rs` and route it in `commands/mod.rs`.
3. Get what you need from `Ctx`: `ctx.project()?` (attaches the run
   directory under `target/icm`), `ctx.host()?`, `ctx.env`.
4. Run processes with `ctx.step(name, &cmd)` (a `STEP` line, `step` events,
   `steps/NN-<name>.log`) or build a `Plan` and honour `ctx.dry_run()`.
   Cargo builds go through `ctx.cargo(name, &Invocation, env)`, which turns
   compiler messages into `diagnostic` events and failures into `build.*`.
   Apple builds call `ctx.deployment_target(...)` first and write the stamp
   after success.
5. Report with `ctx.rep`: `check(Check::pass|warn|fail(...))` for findings
   (a FAIL here is non-blocking: exit 1 unless something blocks),
   `artifact(kind, path)`, `ready(...)`, `next(cmd, why)`, `set(key, value)`
   for result fields, `latest(platform)`.
6. Return `Err(IcmError::new(CheckId::..., detail))` for the failure that
   stops the command; it becomes `errors[0]` and sets the exit code.

Never print to stdout yourself: in human mode stdout carries only protocol
lines, with `--json` only NDJSON. Content commands (`print`, `explain`) use
`ctx.rep.content(text)`.

## Adding an id

Add a line to the `catalogue!` block in `catalogue.rs` (id, exit code when it
fails, default level, who fixes it, title, fix). `icm explain <id>` renders
it. For a common failure, also write `docs/explain/<id>.md` (embedded by
`build.rs`; a test checks every file names a real id).

## Testing

- Unit tests live next to the code.
- `tests/cli.rs` runs the binary. Each test sets `ICM_CACHE_DIR` and
  `ICM_HOST_CONFIG` to a temp dir and copies fixtures from `tests/fixtures/`.
- `tests/desktop.rs` runs, logs, captures and stops `fixtures/desktop`, a
  windowless stand-in app (`--env ICM_FIXTURE=ready|panic|exit|hang`), and
  kills whatever it started.
- `tests/web.rs` drives the web pipeline against real headless Chrome with
  a fake cargo and wasm-bindgen (`ICM_TOOL_CARGO`, `ICM_TOOL_WASM_BINDGEN`)
  whose JavaScript "app" speaks `ICM_EVENT`; it skips without Chrome or the
  wasm32 target.
- `icm __test <scenario>` (hidden) exercises the core end to end: `sleep`
  (timeouts, signals, `--detach`), `panic`, `fail <id>`, `checks`, `plan`,
  `project`, `lock`, `deployment`, `busy` (the signal watchdog).
- Fake tools: `ICM_TOOL_<NAME>=/path/to/script` replaces any external tool
  (`xcrun`, `adb`, `cargo`, ...).
- `tests/project.rs` covers `new`, `check`, `doctor`, `stop`/`ps` and
  `explain config.<key>`. `check` compiles `tests/fixtures/checkapp`, whose
  `iced` is a local stand-in, offline in a second; `doctor` runs against a
  fake SDK, JDK, Rust sysroot (`ICM_TOOL_RUSTC`/`RUSTUP`) and Xcode
  (`DEVELOPER_DIR`, `ICM_TOOL_XCRUN`/`XCODEBUILD`), so no test downloads
  anything or touches a real simulator, emulator or `~/.android`.

## Environment

| Variable | Effect |
|---|---|
| `ICM_JSON=1` | same as `--json` |
| `ICM_CONFIG`, `ICM_TIMEOUT` | defaults for `--config`, `--timeout` |
| `ICM_CACHE_DIR` | icm's cache (runs outside a project, pinned tools) |
| `ICM_HOST_CONFIG` | the host.toml to read |
| `ICM_TOOL_<NAME>` | the path of an external tool |
| `ICM_CHROME` | the Chrome executable |
| `ANDROID_USER_HOME`, `ANDROID_AVD_HOME` | where the debug keystore and AVDs live (default `~/.android`, `~/.android/avd`); tests point them at a temp dir |
| `ICM_BUILD_FRAMEWORK` | at build time: force the default framework pin (`tag:`/`rev:`/`path:`) |
| `ICM_RUN_ID`, `ICM_RUN_DIR`, `ICM_RUN_ROOT`, `ICM_DETACHED` | internal: a detached child's run |
