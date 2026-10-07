> Design for `icm`, the iced_mobile app tool, written 2026-10-06 before implementation. Appendix C (the review's corrections) overrides earlier sections. Where the code and this document disagree, the code and its tests win; update this file in the same commit.

# icm: the iced_mobile app tool and template (final design)

Since then, phases 0 and 1 (the framework prerequisites and the dev loop) have been built and released as `v0.14.1-mobile.1`. Phases 2 to 5 (releases) are still plans.

**Evidence tags.** **[V]** means verified on this Mac, by the research brief, by a judge or during this synthesis. **[S]** means it comes from a source cited in the research brief. **[I]** means inference that has not been tested yet. Every [I] has a spike or CI check in §18.

**What this is.** This is the synthesis of three candidate designs, cargo-icm, icepack and icm. cargo-icm and icm tied on the judges' totals (22 each); icepack scored 16.

- **Core:** cargo-icm's minimal-dependency core and release pipelines, the base two judges recommended.
- **From icm:** the agent contract, i.e. the result object with `errors[]`, the exit code for "owner needed", the run directory, the check catalogue, policy kept as data, a CLI that links no iced crate, and 16 KB AAB alignment.
- **From icepack:** `object`-based binary checks, the `--sign none` path, the session TTL, and wasm-opt feature flags derived from the compiler.

Every error the judges found is fixed. Appendix A lists each one and where it is resolved.

**State of the fork when this was written [V].** Local `main` at `4ed24aa73` already contained:
- the P0 floors: winit 0.30.13, softbuffer 0.4.7, and the display-server exemption;
- NativeActivity by default, plus `iced::mobile::{init_logger, set_android_app, on_lifecycle, AndroidApp}` and `iced::android_main!` (`33a00d56c`);
- Fira Sans as the default font on Android and iOS (`4ba8c1edd`), with a fallback to system fonts (`5802255ff`);
- the keep-alive guards (`476dff628`) and iOS Return/Tab mapping (`4ed24aa73`).

None of this was pushed then: the remote had only `tawara/0.14-mobile`, and no `v0.14.1-mobile.*` tag existed [V]. Since then, `main` and the tag `v0.14.1-mobile.1` are on the remote.

---

## 0. Decisions at a glance

| Question | Decision |
|---|---|
| Name | **`icm`**: binary and package `icm`, config `icm.toml`, env prefix `ICM_`, outputs in `target/icm/`. There is no `cargo icm` form. |
| Shape | One synchronous Rust binary that calls first-party tools directly: cargo/rustc, `xcrun`, `codesign`, `security`, `aapt2`, `zipalign`, `apksigner`, `bundletool`, `jarsigner`, `keytool`, `adb`, `emulator`, `wasm-bindgen`, `wasm-opt`, `hdiutil`, `iconutil`, `rc.exe`, `wix`, `makensis`, `dpkg-deb`, `appimagetool`. **No** Gradle or Xcode project, no cargo-packager, no trunk, no async runtime, no HTTP-client crate. The CLI **links no iced crate**; it talks to apps through versioned protocols. |
| Location | `cli/` in the fork, as its **own Cargo workspace** with its own `Cargo.lock`. The root `Cargo.toml` gets `exclude = ["cli"]`. The template is **`examples/app/`**, a member of the root workspace that framework CI compiles and `cli/build.rs` embeds. |
| Install | `cargo install --locked --git https://github.com/patricksmithlaravel/iced_mobile --tag v0.14.1-mobile.N icm`. Before the first tag: `--rev <sha>`, or `cargo install --locked --path cli` from a checkout. |
| Versioning | One tag, `v0.14.1-mobile.N`, releases the framework and `icm` together. `icm new` pins that tag, or the CLI's full git rev if it was built from an untagged commit. `check` and `doctor` warn on skew. |
| Config | **`icm.toml`** next to the app crate. The marketing version stays in Cargo.toml `version`, and the build number is `[app] build`. Machine paths go in `~/.config/icm/host.toml` and never in the repo. |
| Native files | **Generated only**, on every build, into `target/icm/gen/`, from icm.toml plus typed overlays. Existing hand-written files (Tawara's) are converted once by `icm init --adopt-*`. Reviewable snapshots are opt-in. |
| Output | stdout carries `STEP` / `CHECK` / `ARTIFACT` / `READY` / `NEXT` / `RESULT` lines. With `--json` it carries NDJSON, and **the last line is always the result object**. Every invocation writes `events.ndjson` and `result.json` into a run directory. |
| Exit codes | 0 ok · 1 check/test failed · 2 usage · 3 config · 4 environment · 5 build · 6 tool · 7 device · 8 timeout · **9 owner needed** · 10 app died or never drew · 70 icm bug · 130 interrupted |
| Dev loop | `icm run <desktop\|web\|ios-sim\|android>` builds, installs, launches, waits for the first frame, takes a screenshot and a preview, reads logs, and **returns** while the app keeps running. Readiness comes from the framework's `ICM_EVENT ready`, with platform probes as fallback. `icm logs` re-reads live sources. Web runs in a detached session. |
| Seeing and acting | **Phase 1:** OS screenshots; a headless `iced_test` harness (`shot`, `tree`, `.ice`) that needs no device; adb input on Android; CDP input on web; AXe on ios-sim if installed. **Phase 6:** a debug-only in-app bridge (tree, find, tap, type, screenshot) on every platform except physical iOS. |
| Release | `icm release <ios\|android\|web\|macos\|windows\|linux>` writes signed, store-gated artifacts plus `artifacts.json`, `UPLOAD.md` and `upload.sh` to `target/icm/dist/<version>+<build>/<target>/`. **icm never uploads and never notarizes.** Secrets are passed only as env-var names or keychain-profile names. |
| Agent surface | A generated `AGENTS.md`, `icm explain <id>`, `icm docs <topic>`, `icm print schema …`, `icm print commands --json`, and `--dry-run` plans. An MCP wrapper is optional, in phase 7. |

**How the owner's answers are reflected**

1. **General-purpose framework.** The template, the config and the commands are app-agnostic, and Tawara is one consumer (§15).
2. **"You decide."** The fork may carry winit patches only for fixes that cannot live in iced_winit: the A3 recreation freeze, C2 composition and C1 insets. iced_winit takes such a winit as a direct git dependency, never through `[patch]`, because `[patch]` does not reach apps. `icm check` enforces one winit (`deps.single_winit`), so either state of the fork is safe to detect. As built, the fork vendors that winit in its own repository (`vendor/winit`, a path dependency of the workspace), so an app's lock takes it from iced's git source, and `deps.single_winit` names that copy as iced's own.
3. **Stay on 0.14.** Tags are `v0.14.x-mobile.N`, and changes from 0.15 are cherry-picked when they materially improve things. icm does not care which base is underneath, because it never links iced.
4. **Borrow, don't join.** The Android system-font loading idea and the `run_android` naming are borrowed. icm itself never depends on the iced-mobile org's crates.

The template defaults to iOS 16 and Android minSdk 26 / targetSdk 36 (review open question 7).

---

## 1. Goals, principles, non-goals

**Goals.**
- **For an agent:** go from a clean clone to `new → check → run → see → test → release` on desktop, web, iOS Simulator and Android emulator, with no prompts and no guessing.
- **For the owner:** upload-ready artifacts for the App Store (`.ipa`), Google Play (`.aab`), static web hosting, macOS (`.app`/`.dmg`, signed and ready to notarize), Windows (`.msi`/`.exe`) and Linux (`.deb`/AppImage), each with the exact commands to run.

**Principles.**

1. **Plan, then run.** Every command builds a `Plan`, which is a list of steps. Each step records:
   - argv and the env delta
   - cwd and timeout
   - its gates

   `--dry-run` and `icm print plan <cmd…>` print the plan. Every executed step leaves a log file with argv, the redacted env delta, the exit code and the duration.
2. **One source of truth.** App identity comes from `icm.toml` plus Cargo.toml `version`. Every native file is derived from them. Keys the tool manages cannot be overridden.
3. **Prove, never assume.** Anything that can fail silently gets a gate:
   - `simctl launch` exiting 0 is not success.
   - altool's exit code is not trusted.
   - `jarsigner -verify` output is parsed.
   - ELF alignment, the Mach-O SDK, the DT keys and the scene manifest are all read from the artifact.
4. **Agent-first output.** Results are machine-readable and exit codes are stable. Each failure carries an id, evidence and a fix that says who must act. The tool never prompts. Every wait is bounded and named.
5. **No secret values.** Config holds references only:
   - identity SHA-1s or names
   - profile UUIDs
   - keystore paths
   - env-var names
   - keychain-profile names

   Passwords reach tools through indirection: `jarsigner -storepass:env`, `apksigner --ks-pass env:`, `keytool -storepass:env`, `notarytool --keychain-profile`. Log redaction covers env names matching `*PASS*|*SECRET*|*TOKEN*|*KEY*|*PRIVATE*`.
6. **Few, boring dependencies** (§16). Downloads only happen with `--yes`, through `curl`, `cargo install`, `rustup` or `sdkmanager` subprocesses, and each download is sha256-pinned where the tool is fetched by URL.

**Non-goals for v1.**
- Running uploads or notarization
- Java/Kotlin/Swift sources (GameActivity, JNI `InputConnection`, app extensions)
- iPad
- Mac App Store, Microsoft Store, Flathub
- A scenario DSL. Tawara's checks become hook scripts (§13.6), and lifecycle checks are built in (§13.5).

---

## 2. Name, location, install, versioning

### 2.1 Why `icm`
- It is short for agents to type, and it is one name everywhere: binary, config, env, output directory.
- It is not `cargo-iced`, which would suggest upstream endorsement. The review ranked fork/upstream confusion as trap #1.
- crates.io shows `icm` and `cargo-icm` unclaimed [V]. The tool is installed from git, never published.

### 2.2 Layout in the fork
```
iced_mobile/
  Cargo.toml                 # + exclude = ["cli"]   (the only edit to an upstream file)
  examples/app/              # THE template: a root-workspace member compiled by framework CI
  docs/agents/limitations.md # the known-limitations table for this tag (embedded into AGENTS.md)
  cli/
    Cargo.toml               # [package] name = "icm", version = "0.14.1-mobile.N"; [[bin]] icm; [workspace]
    Cargo.lock               # the CLI's own lock: CLI deps never enter the framework lock
    build.rs                 # embeds git rev/tag, examples/app/**, docs/agents/*, policy/*, tools.toml
    policy/stores.toml       # dated store floors (§12.0)
    tools.toml               # pinned external tools: version, URL, sha256
    schema/                  # icm.schema.json, output.schema.json, artifacts.schema.json (generated; CI checks freshness)
    docs/                    # explain/<check-id>.md, topics/*.md (embedded)
    src/ …                   # §3
    tests/ …                 # §17
```

**Why its own workspace.** The CLI's dependencies stay out of the framework's `Cargo.lock`. Rebases onto upstream 0.14.x touch one `exclude` line. `cargo test --workspace` and upstream CI ignore the CLI.

icepack verified today that `cargo install --git … --locked` finds a nested-workspace package and uses its own lock [V]. The install-check CI job keeps that true.

**The template lives in the root workspace on purpose**, so that framework API changes break framework CI and not users. One rule applies to it: it may depend only on crates already in the root lock (`iced`, `iced_test`, `log`). A CI check (`template.no_new_lock_entries`) diffs the root `Cargo.lock` package set.

`examples/app` uses path deps (`iced = { path = "../..", … }`). `icm new` rewrites them with toml_edit into git deps.

### 2.3 Install
```sh
# released
cargo install --locked --git https://github.com/patricksmithlaravel/iced_mobile --tag v0.14.1-mobile.N icm
# from a checkout (fork development)
cargo install --locked --path cli
# without installing
cargo run --manifest-path cli/Cargo.toml -- <args>
```

### 2.4 Versioning
1. `cli/Cargo.toml` version is `0.14.1-mobile.N`. The framework crates stay `0.14.1`, so `[patch.crates-io]` keeps matching.
2. `build.rs` embeds:
   - `ICM_GIT_REV` (`git rev-parse HEAD`, walking up from `CARGO_MANIFEST_DIR`; cargo git checkouts keep `.git` [V])
   - `ICM_GIT_TAG` (`git describe --tags --exact-match`, or empty)
3. `icm new` pins `tag = "<ICM_GIT_TAG>"` when the CLI was built from a tag. Otherwise it pins `rev = "<full ICM_GIT_REV>"`. If there is no git metadata, it fails with exit 2 and suggests `--framework path:<dir>`.
4. `icm check` and `icm doctor` read the app's `Cargo.lock` and check the iced sources:

   | Check | Result |
   |---|---|
   | Every `iced*` package from one source and rev | PASS `deps.single_iced` |
   | iced from crates.io or upstream | FAIL `deps.iced_not_fork` (3) |
   | A branch with no tag or rev | FAIL `deps.iced_unpinned` (3) |
   | Lock rev differs from the CLI's rev | WARN `deps.cli_framework_skew`, with `icm self update --to-lock` as the fix |

5. **Protocols.** The CLI speaks output v1, event v1 (§13.3), harness v1 (§13.2) and bridge v1 (§13.7). CLI tag N supports framework tags N and N−1. The app announces its protocols in `ICM_EVENT start`, and the harness prints them first. A mismatch exits 4 with the reinstall command.
6. `icm framework set tag:<t>|rev:<sha>|path:<dir>` rewrites every iced line in the project, then runs `cargo update -p iced`.
7. Release discipline (fork CI, §17):
   - The tag must equal the `cli/Cargo.toml` version.
   - `icm new` output, normalized, must equal `examples/app`.
   - Schemas must be fresh.
   - Existing tags are never moved. Upstream 0.14.x patches are rebased onto `mobile/0.14` and re-tagged `-mobile.N+1` (review §6.10).
8. **0.15:** if a 0.15 base is ever adopted, tags become `v0.15.x-mobile.N` and nothing else in icm changes.

---

## 3. Architecture

```
cli/src/
  main.rs            clap surface, dispatch, panic hook → result with exit 70
  output/            Reporter: human lines | NDJSON; run dir writer; result builder; last.json; latest/ links
  catalogue.rs       CheckId enum → exit code, title, explain doc (one namespace for checks and errors)
  plan.rs            Plan/Step/Gate; execute | dry-run | print
  process.rs         spawn: stdin=/dev/null, timeouts, process groups (setsid; Windows job objects),
                     kill-tree, capture to files, redaction, LC_ALL=C, -J-Duser.language=en for JDK tools
  locks.rs           target/icm/locks/<platform>.lock + ~/.cache/icm/locks/device-<id>.lock (flock)
  config/            icm.toml load (toml spans → file:line), validate, resolve, schema (schemars)
  host.rs            host.toml + ICM_* env + autodetect (Xcode, SDK, NDK, JDK, Chrome)
  cargo.rs           cargo metadata (own serde structs), Cargo.lock parse, build/rustc invocations,
                     --message-format=json-render-diagnostics → artifact paths + diagnostic events
  deps.rs            lockfile checks (§12.1)
  policy.rs          reads embedded policy/stores.toml
  inspect/           object crate: Mach-O LC_BUILD_VERSION, undefined symbols, archs, UUID;
                     ELF dynsym, PT_LOAD p_align, e_machine, GLIBC version needs; PNG stats (blank, alpha)
  gen/               plist, entitlements, xcprivacy, AndroidManifest, res, icons, index.html, rc, wxs, nsi, deb control, .desktop
  zip.rs             deterministic ZIP writer (stored + deflate via flate2; sorted entries; 1980-01-01 mtimes)
  platform/          dev drivers: desktop, web, ios_sim, ios_device, android  (trait DevPlatform)
  release/           ios, android, web, macos, windows, linux; owner_plans.rs (the ONLY file allowed to
                     contain upload/notarize argv, enforced by CI)
  verify/            store gates (§12), diagnose parsers (altool, notarytool, play)
  harness.rs         drives the app's tests/icm.rs harness (§13.2)
  session/           `icm __session` host: web static server, CDP pipe to headless Chrome, (phase 6) bridge
  signatures.rs      known failure signatures (§13.4)
```

**One platform trait.** `DevPlatform` has the methods `doctor`, `build`, `bundle`, `install`, `launch`, `wait_ready`, `screenshot`, `logs` and `stop`. `run` composes them the same way for every platform, so every result has the same shape.

**Process hygiene** applies to every child process, including cargo:
- stdin is `/dev/null`
- `GIT_TERMINAL_PROMPT=0` and `GIT_SSH_COMMAND="ssh -oBatchMode=yes"`
- colour is off when not a TTY
- each step has a timeout and kills its whole process group

So jarsigner, keytool, sdkmanager, codesign or a git fetch fail fast instead of waiting for a prompt [V: jarsigner prompts with stdin open and fails fast with stdin closed].

**Concurrency.**
- Per-project, per-platform flock. A busy lock exits 7 `run.lock_busy` unless `--wait-lock <dur>` is given.
- A per-device lock in `~/.cache/icm/locks/` stops two projects from driving the same simulator or emulator at once.

**Sessions.**
- `icm __session` is the CLI re-executed and detached (setsid). It writes `target/icm/sessions/<platform>.json` and a pid file, and exits after 2 h idle (`--session-ttl`).
- Phase 1 uses it **only for web**: the static server plus headless Chrome over a CDP pipe.
- Other platforms need no daemon. `run` records the pid, device and launch mark in the session file and returns. `logs` re-queries the live sources.
- In phase 6 every platform gets a session, to hold the bridge.
- `icm ps` lists sessions. `icm stop` ends them.

**Precedence.** Values resolve as: flag > `ICM_*` env > `icm.toml` > `~/.config/icm/host.toml` (machine keys only) > autodetect.

**Tool discovery** (recorded in `doctor --json`):

| Tool | Search order |
|---|---|
| Android SDK | `host.toml android_sdk` → `$ANDROID_HOME` → `$ANDROID_SDK_ROOT` → `~/Library/Android/sdk` → `/opt/homebrew/share/android-commandlinetools` [V present] → `~/Android/Sdk` |
| NDK | `$ANDROID_NDK_HOME` → highest `$SDK/ndk/*` ≥ r28 [V 29.0.14206865; host dir `darwin-x86_64` holds universal binaries] |
| JDK | `$JAVA_HOME` → `/usr/libexec/java_home -v 17+` → `/opt/homebrew/opt/openjdk@21` [V] |
| Xcode | `$DEVELOPER_DIR` → `xcode-select -p`. All Xcode paths, including `DTXcode`'s Info.plist, derive from it and are never hard-coded |
| Chrome | `host.toml chrome` → `$ICM_CHROME` → `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome` [V] → `google-chrome` → `chromium` |
| Pinned tools | `~/Library/Caches/icm/tools/<name>/<version>/`, `$XDG_CACHE_HOME/icm/tools/…` on Linux, `%LOCALAPPDATA%\icm\tools\…` on Windows |
| wasm-bindgen | `tools/wasm-bindgen/<app lock version>/bin/wasm-bindgen`. A PATH copy is accepted only if `--version` equals the app's lock version |

`ICM_TOOL_<NAME>=<path>` overrides any external tool, for example `ICM_TOOL_XCRUN`. The fake-tool tests use it (§17).

---

## 4. Output contract

### 4.1 Streams
- **Human mode (default).** Only protocol lines go to stdout. Progress goes to stderr. Subprocess output goes to `runs/<id>/steps/NN-<step>.log`, and is mirrored to stderr only with `-v`.
- **`--json` (or `ICM_JSON=1`).** stdout carries only NDJSON. **The last line is always `{"type":"result",…}`**, written from the panic hook too.
- **`-q`.**
  - Human mode prints only `CHECK FAIL` and `CHECK WARN` lines plus `RESULT`.
  - JSON mode prints only the result line, so `icm … --json -q | jq …` works directly.
- **Files written either way:** `runs/<id>/events.ndjson`, `runs/<id>/result.json`, and `target/icm/last.json` (a copy of the newest result).

### 4.2 Human line protocol
Each line starts with a fixed keyword, and continuation lines are indented two spaces, so `grep '^CHECK FAIL'` always works:
```
STEP cargo.build ok 41.2s  (log: target/icm/runs/20261006T210311Z-run-ios-sim-7f3a/steps/01-cargo.build.log)
CHECK PASS ios.macho.platform: IOSSIMULATOR minos 16.0 sdk 27.0
CHECK PASS ios.plist.scene_manifest: UIApplicationSceneManifest present
CHECK PASS run.ready: first frame 402x874@3 after 0.8s (source: icm_event)
CHECK WARN run.screen_blank: 99.8% of pixels are #000000
  evidence: target/icm/latest/ios-sim/screen.png
  fix: icm logs ios-sim --level warn ; icm explain run.screen_blank
ARTIFACT screenshot target/icm/latest/ios-sim/screen.png
ARTIFACT preview target/icm/latest/ios-sim/screen.preview.png
ARTIFACT logs target/icm/latest/ios-sim/app.log
NEXT icm logs ios-sim --level warn   # read the app's output
NEXT icm stop ios-sim                # terminate the app
RESULT ok run ios-sim exit=0 run=20261006T210311Z-run-ios-sim-7f3a (23.1 s)
```
A step that needs the owner prints `  fix (owner): …`.

### 4.3 NDJSON events (schema `icm.output/1`)
Every event has `v`, `type`, `run` and `t` (milliseconds since start).

| type | Fields |
|---|---|
| `start` | `command`, `target`, `argv`, `cwd`, `icm{version,rev,protocols{output,event,harness,bridge}}`, `app{id,name,version,build}` |
| `step` | `name`, `phase` (`begin`\|`end`), `argv`, `env` (redacted delta), `cwd`, `ok`, `ms`, `log` |
| `diagnostic` | from cargo: `level`, `code`, `message`, `rendered`, `file`, `line`, `col`, `targets[]` (de-duplicated across targets) |
| `check` | `id`, `status` (`pass`\|`fail`\|`warn`\|`skip`\|`info`), `detail`, `evidence[{path,line?,excerpt?}]`, `fix{summary,commands[],by}` |
| `artifact` | `kind`, `path`, `bytes`, `sha256` (release only), `blank` (screenshots) |
| `ready` | `url` (web) or `session`, `source` (`icm_event`\|`probe`), `ms_since_launch`, `window{size,scale}` |
| `log` | only with `logs --follow`: a normalized record (§13.3) |
| `result` | always last (§4.4) |

`fix.by` is one of:
- `agent`: edit code or config
- `doctor`: `icm doctor --fix`, local and idempotent
- `doctor-yes`: `icm doctor --fix --yes`, which downloads or installs
- `owner`: credentials, certificates, licences, store web UI, product decisions

### 4.4 Result object
```json
{"v":1,"type":"result","schema":"icm.result/1","run":"20261006T210311Z-run-ios-sim-7f3a",
 "command":"run","target":"ios-sim","profile":"debug","ok":false,"exit":10,
 "summary":"the app exited 0.9s after launch; a panic was found in stderr",
 "app":{"id":"com.example.notes","name":"Notes","version":"0.1.0","build":1},
 "device":{"kind":"simulator","udid":"6F1…","name":"icm iPhone 17 (iOS 27.0)","os":"27.0"},
 "process":{"pid":14879,"alive":false,"ready":{"source":"none","ms":null}},
 "checks":{"pass":7,"fail":1,"warn":0,"skip":0,"info":0,"failed":["run.app_panicked"]},
 "errors":[{"id":"run.app_panicked","exit":10,"title":"The app panicked before its first frame",
   "detail":"panicked at src/lib.rs:41:9: index out of bounds",
   "evidence":[{"path":"target/icm/runs/…/app.stderr","line":3,"excerpt":"thread 'main' panicked at src/lib.rs:41:9"}],
   "likely_causes":["a bug in App::view at src/lib.rs:41"],
   "fix":{"summary":"Fix the panic at src/lib.rs:41, then rerun","commands":["icm run ios-sim --json -q"],"by":"agent"},
   "docs":"icm explain run.app_panicked"}],
 "warnings":[],
 "artifacts":{"bundle":"target/icm/build/ios-sim/debug/Notes.app","app_log":"target/icm/latest/ios-sim/app.log",
   "logs":"target/icm/latest/ios-sim/logs.ndjson","system_log":"target/icm/latest/ios-sim/system.ndjson","crash":[]},
 "session":"target/icm/sessions/ios-sim.json","owner_steps":[],
 "inputs":{"git_rev":"1f2e3d4","dirty":true,"cargo_lock_sha256":"…","icm_toml_sha256":"…"},
 "tools":{"rustc":"1.98.0","xcode":"27.0 (27A266a)","sdk":"iphonesimulator27.0"},
 "next":[{"cmd":"icm logs ios-sim --json","why":"full output"}],
 "ms":14012,"run_dir":"target/icm/runs/20261006T210311Z-run-ios-sim-7f3a"}
```
On success, `artifacts` also has `screenshot` and `preview`. `preview` is a PNG at most 1024 px on its long edge, to save tokens. For web it also has `url`. Release results add one key per produced file (`ipa`, `dsym`, `aab`, `apk`, `symbols`, `site`, `site_zip`, `app`, `app_zip`, `dmg`, `msi`, `exe`, `deb`, `appimage`), plus `manifest` (the path of artifacts.json), `upload_md`, `upload_sh` and a non-empty `owner_steps`.

**Rules.**
- `ok == (exit == 0)`.
- `errors[0]` explains the exit code.
- The first *blocking* failure stops the pipeline and sets the exit code. Non-blocking gate FAILs collected on the way set exit 1 only if nothing blocking happened.
- A panic (exit 70) overrides everything.
- WARN never changes the exit code, except under `--strict`, where WARN becomes FAIL.
- `doctor` is the one exception: it exits 4 if anything with `by: doctor|doctor-yes` remains, else 9 if only owner items remain, else 0.
- Within `/1`, fields are only added. A rename or removal bumps the schema to `/2`.
- `icm print schema output` emits the JSON Schema.

### 4.5 Exit codes (stable; `icm explain exit-codes`)

| Code | Name | Meaning | What an agent does next |
|---|---|---|---|
| 0 | OK | success; WARNs allowed | continue |
| 1 | CHECK_FAILED | the app or artifact failed a gate, test, hook or verify | fix what `errors[]` names |
| 2 | USAGE | bad flags or an unsupported platform/command pair | `icm <cmd> --help` |
| 3 | CONFIG | icm.toml or Cargo.toml invalid or inconsistent, or a bad lockfile shape | edit the field named at `file:line` |
| 4 | ENVIRONMENT | a tool, target, SDK or package is missing, or versions are skewed | `icm doctor <p> --fix [--yes]` |
| 5 | BUILD | rustc or the linker failed | read the `diagnostic` events |
| 6 | TOOL | actool, aapt2, bundletool, wasm-bindgen, codesign, … failed unexpectedly | read the step's `log` |
| 7 | DEVICE | no device, simulator, emulator or browser; boot or install failed; lock or port busy | `icm devices`, `--device`, `--wait-lock`, `--port` |
| 8 | TIMEOUT | a tool or infrastructure wait exceeded its limit; `detail` names which | raise `--timeout`; read the step log |
| 9 | NEEDS_OWNER | certificates, profiles, keystore password env, licences, store web steps, product decisions (export compliance, bundle id, icon) | **stop and hand `errors[0].fix` to the owner** |
| 10 | APP_DIED | crashed, panicked, ANR, or alive but no first frame within `--wait-ready` | fix the app; the evidence is attached |
| 70 | INTERNAL | bug in icm | report with `run_dir` |
| 130 | INTERRUPTED | SIGINT | — |

### 4.6 Fixed paths (under cargo's target dir, from `cargo metadata`)
```
target/icm/
  gen/<platform>/<profile>/      generated inputs, hash-stamped (Info.plist, Assets.xcassets/, PrivacyInfo.xcprivacy,
                                 entitlements.plist, AndroidManifest.xml, res/, BundleConfig.json, index.html, app.rc, app.wxs, …)
  build/<platform>/<profile>/    runnable bundles: Notes.app | notes.apk | site/ | notes (desktop exe)
  runs/<run-id>/                 events.ndjson, result.json, steps/NN-<step>.log, app.stdout, app.stderr, app.log,
                                 logs.ndjson, screen.png, screen.preview.png, system.ndjson | logcat.txt | console.ndjson, crash/
  latest/<platform>/             symlink to the newest run for that platform (Windows: a dir containing RUN_ID)
  host/shots/<viewport>-<theme>[-<preset>].png
  sessions/<platform>.json       pid, device, launch mark, ports, log paths
  dist/<version>+<build>/<target>/   release artifacts + artifacts.json + UPLOAD.md + upload.sh
  dist/latest/<target>           symlink to the newest release for that target
  locks/<platform>.lock
  last.json
```
- Run ids have the form `YYYYMMDDTHHMMSSZ-<cmd>-<target>-<4 hex>` and sort by time.
- icm keeps the newest 30 run dirs. `icm clean --runs` prunes them, and `icm clean` removes `target/icm/`.

### 4.7 `artifacts.json` (schema `icm.artifacts/1`)
```json
{"schema":"icm.artifacts/1","target":"ios","created":"2026-10-06T21:03:11Z",
 "app":{"id":"com.example.notes","name":"Notes","version":"1.0.0","build":12},
 "source":{"git_rev":"1f2e3d4…","dirty":false,"cargo_lock_sha256":"…"},
 "framework":{"source":"git+https://github.com/patricksmithlaravel/iced_mobile?tag=v0.14.1-mobile.3#…"},
 "tools":{"icm":"0.14.1-mobile.3 (rev …)","rustc":"1.98.0","xcode":"27.0 (27A266a)","sdk":"iphoneos27.0"},
 "files":[{"role":"upload","kind":"ipa","path":"Notes.ipa","bytes":9437184,"sha256":"…"},
          {"role":"symbols","kind":"dsym-zip","path":"Notes.app.dSYM.zip","bytes":0,"sha256":"…"}],
 "signed":true,"uploadable":true,
 "signing":{"identity":"Apple Distribution: … (TEAMID)","identity_sha1":"…",
            "profile":{"name":"…","uuid":"…","type":"app-store","expires":"2027-09-29"}},
 "checks":{"pass":34,"warn":1,"fail":0,"ids_warn":["android.manifest.back_optout"]},
 "owner_steps":[{"title":"Validate","argv":["xcrun","altool","--validate-app","Notes.ipa","…"]}]}
```
**Determinism.**
- `release` refuses a dirty git tree unless `--allow-dirty` is given, and records `dirty`.
- Every zip icm writes has sorted entries, 1980-01-01 mtimes and no extended attributes, and honours `SOURCE_DATE_EPOCH`.
- Signed artifacts are not byte-reproducible, because signatures carry timestamps. Their hashes are recorded.

---

## 5. Check and error catalogue

**One namespace.** Every CHECK id is also an explainable error id. The format is `<area>.<subject>[.<detail>]`, in lower snake case.

- `icm explain <id>` prints the embedded doc: what the check means, how icm detects it, the evidence files it reads, and the fix with its `by`.
- `icm explain --list --json` dumps the catalogue.
- A unit test fails if code emits an id with no doc, or if a doc exists for an id nothing emits.

| Group | Ids (exit code) |
|---|---|
| config | `config.not_found` (3), `config.invalid` (3, with file:line), `config.unknown_key` (3), `config.managed_key` (3, names the icm.toml key to use instead), `config.raw_xml_forbidden` (3, §7.4), `config.package_not_found` / `config.bin_missing` / `config.lib_missing` (3), `config.id_invalid` (3), `config.too_new` (4), `config.schema_stale` (WARN), `config.owner_decision` (9: e.g. `ios.uses_non_exempt_encryption`, `ios.team_id`, `android.signing.upload` unset for `release`) |
| app | `app.id.placeholder` and `app.icon.placeholder` (WARN in dev, for web, and under `release --sign none`; 9 in a signed release), `app.icon.invalid` (3: not a ≥1024² square PNG) |
| review | `review.snapshot_stale` (1, only with `[review] snapshot = true`) |
| env | `env.rust_target_missing` (4 doctor-yes), `env.xcode_missing` / `env.xcode_too_old` (9), `env.xcode_beta` (WARN in dev, 1 in iOS release), `env.ios_runtime_missing` (4 doctor-yes, about 8 GB, size stated), `env.android_sdk_missing` (9), `env.android_package_missing` (4 doctor-yes: sdkmanager), `env.ndk_too_old` (4 doctor-yes), `env.jdk_missing` (4 doctor-yes), `env.tool_missing` (4 doctor-yes, or `agent` for AXe), `env.chrome_missing` (4), `env.licenses_not_accepted` (9), `env.consent_required` (4: rerun with `--yes`), `env.unsupported_host` (4), `env.policy_stale` (WARN, policy table > 90 days old) |
| deps | `deps.single_iced` (3), `deps.iced_not_fork` (3), `deps.iced_unpinned` (3), `deps.cli_framework_skew` (WARN), `deps.single_winit` (3), `deps.winit_floor` (3, < 0.30.13), `deps.softbuffer_floor` (3, < 0.4.7 when building for Android), `deps.android_activity_backend` (3, zero or two backends enabled), `deps.libc_sysinfo_ios` (3, review A11), `deps.wasm_bindgen_cli` (4 doctor-yes), `deps.getrandom_backend` (3), `deps.legacy_entry` (INFO: hand-written `android_main`) |
| build | `build.compile_error` (5), `build.link_error` (5; NDK clang failures map to `env.ndk_*`), `build.wrong_platform` (6) |
| ios | `ios.macho.platform`, `.minos`, `.sdk_floor`, `.arch`, `.sdk_matches_dt` (1); `ios.plist.lint`, `.required_keys`, `.scene_manifest`, `.dt_keys`, `.export_compliance`, `.usage_descriptions`, `.ipad_orientations` (1); `ios.icon.opaque_1024` (1); `ios.privacy.present`, `.reasons` (1); `ios.actool_failed` (6); `ios.sim.boot_failed`, `.install_failed` (7); `ios.device.not_found` (7), `ios.device.developer_mode_off` (9); `ios.sign.no_identity`, `.no_profile`, `.profile_expired`, `.profile_mismatch`, `.keychain_prompt` (9); `ios.sign.verify` (1); `ios.entitlements.not_in_profile` (9), `ios.entitlements.get_task_allow` (1); `ios.ipa.layout`, `.signature` (1); `ios.version.format` (3); `ios.xcode.not_beta` (1); `ios.export_compliance.documentation` (WARN, owner) |
| android | `android.device.none`, `.ambiguous` (7); `android.emulator.boot_timeout` (8), `.ports_busy` (7), `.shared` (INFO); `android.install.failed`, `.signature_mismatch` (7); `android.so.export` (6), `.align16k` (1), `.abis` (1); `android.manifest.target_sdk`, `.config_changes`, `.has_code`, `.debuggable`, `.lib_name`, `.version` (1), `.back_optout` (WARN); `android.bundle.alignment` (1); `android.aab.validate`, `.signed` (1), `.unsigned` (WARN, `--sign none`); `android.apk.zipalign`, `.signature` (1); `android.keystore.missing`, `.password_env_unset` (9); `android.permissions.review` (WARN); `android.screen.secure` (INFO); `android.aapt2_failed`, `.bundletool_failed` (6) |
| web | `web.port_busy` (7), `web.chrome_failed` (7), `web.size_budget` (1), `web.mime` (1), `web.fonts_embedded` (1), `web.renderer_fallback` (WARN), `web.hashed_assets` (1), `web.serve_smoke` (1) |
| desktop | `desktop.shot.permission` (WARN; falls back to a headless render), `macos.sign.no_developer_id` (9), `macos.hardened_runtime`, `macos.sign.verify`, `macos.min_os`, `macos.gatekeeper` (1), `macos.not_stapled` (9), `windows.sign.not_configured` (9), `windows.signed` (1), `windows.msi_version` (3), `windows.sdk_missing` (4), `linux.glibc_floor` (1), `linux.desktop_file` (1), `linux.deb.lint` (WARN) |
| run | `run.ready` / `run.alive` (PASS checks), `run.app_died`, `run.app_panicked`, `run.anr`, `run.not_ready` (10), `run.activity_recreated` (WARN; 1 when the app does not start over), `run.screen_blank` (WARN; 1 with `--expect-content`), `run.font_missing` (WARN), `run.lock_busy` (7), `run.no_session` (7) |
| test, harness, hooks | `test.failed` (1), `test.ice_parse` (3), `harness.missing` (3), `harness.protocol_mismatch` (4), `hook.<name>` (1), `input.unsupported` (2) |
| release | `store.no_agent_bridge` (1), `version.build_not_increased` (1), `version.format` (3) |
| bridge (phase 6) | `bridge.not_compiled` (3), `bridge.no_connection` (7), `bridge.protocol_mismatch` (4), `bridge.selector_not_found` (1) |
| internal | `internal.bug` (70) |

---

## 6. Commands

Global flags:
```
--json            NDJSON on stdout; last line = result                     (env ICM_JSON=1)
-q, --quiet       human: failures/warnings + RESULT; json: result line only
-v, --verbose     echo each step's argv/env delta and tool output to stderr
--config <path>   icm.toml to use (default: nearest, walking up from cwd)   (env ICM_CONFIG)
--dry-run         print the plan; change nothing; exit 0
--yes             allow downloads, installs and other machine-state changes (never uploads, never notarizes)
--offline         pass --offline to cargo; refuse anything needing the network (exit 4)
--timeout <dur>   overall limit, e.g. 90s, 10m                              (env ICM_TIMEOUT)
--strict          WARN → FAIL
--wait-lock <dur> wait for a held platform/device lock instead of exiting 7
--color auto|always|never   (off when stdout is not a TTY or NO_COLOR/CI is set)
```

**Dev platforms:** `desktop`, `web`, `ios-sim`, `ios-device`, `android`. For `android`, the device is chosen in this order:
1. `--device <serial>`
2. `host.toml android.device`
3. an icm-managed emulator that is already running
4. the single online device
5. the managed AVD, booted for the run

If several devices are online and none is chosen, icm exits 7 `android.device.ambiguous`.

**Release targets:** `ios`, `android`, `web`, `macos`, `windows`, `linux`.

```
icm new <dir> [--name <Display>] [--id <reverse.dns>] [--framework tag:<t>|rev:<sha>|path:<dir>] [--no-git] [--force]
icm init [--package <pkg>] [--adopt-ios <Info.plist[.in]>] [--adopt-android <AndroidManifest.xml>]
         [--adopt-android-res <dir>] [--write]          # writes icm.toml for an existing crate; prints the diff (§15)
icm doctor [<platform|target>...] [--fix] [--yes]
icm check [<platform>...|--all] [--release] [--clippy]
icm build <platform> [--release] [--device <id>] [--abi <abi>]
icm run <platform> [--release] [--no-build] [--device <id>] [--sim <name|udid>] [--avd <name>] [--fresh]
        [--show] [--env K=V]... [--wait-ready <dur>=30s] [--settle <dur>=1.5s] [--no-shot] [--expect-content]
        [--reinstall] [--viewport <preset|WxH>] [--port <n>=8787] [--watch] [--attach] [--from-aab]
icm stop [<platform>|--all] [--shutdown]                 # --shutdown also stops icm-managed sims/emulators
icm ps
icm devices [<platform>]
icm shot <platform> [--out <png>] [--name <label>]
icm shot --headless [--viewport <preset|WxH[@scale]>]... [--all-viewports] [--theme light|dark] [--preset <name>]
        [--wait <dur>=500ms] [--out-dir <dir>]
icm logs <platform> [--since launch|<dur>] [--level trace|debug|info|warn|error] [--source app|system|crash|all]
        [--grep <re>] [--tail <n>=200] [--follow] [--raw]
icm input <platform> tap <x> <y> | swipe <x1> <y1> <x2> <y2> [<ms>] | text <s> | key back|home|enter|tab|escape
        | appearance light|dark | rotate portrait|landscape | font-scale <f> | background | foreground
icm ui --headless tree | find <selector> | ice <file>     # phase 1: through the app's test harness
icm ui <platform> tree | find <selector> | tap <selector|x,y> | type <s> | key <k> | ice <file> | messages   # phase 6
icm test [--host] [--filter <re>] | --on <platform> [--lifecycle] [--flows]   # --flows: phase 6
icm release <target> [--sign auto|none] [--allow-dirty] [--no-smoke] [--apk] [--dmg] [--universal] [--via-xcode-export]
icm verify <target> [--artifact <path>] [--after-notarize] [--url <deployed web url>]
icm upload-commands <target>                             # reprint UPLOAD.md for dist/latest/<target>
icm diagnose altool|notarytool|play <file|->             # map owner-pasted tool output to catalogue ids
icm ledger show | mark-uploaded <target> [--build <n>]   # the owner runs mark-uploaded (last line of upload.sh)
icm version show | bump major|minor|patch|build | set <x.y.z> [--build <n>]
icm framework status | set tag:<t>|rev:<sha>|path:<dir>
icm print config | info-plist <platform|target> | manifest <platform|target> | privacy | entitlements <platform|target>
        | index-html | env <platform> | plan <cmd…> | paths | commands | schema config|output|artifacts [--write]
        | snapshots [--write]
icm explain <id> | exit-codes | --list
icm docs [agents|config|ios|android|web|desktop|release|troubleshooting] [--write]   # `docs agents --write` refreshes AGENTS.md
icm ci init [--targets <t,...>]
icm clean [<platform>] [--runs]
icm self update [--to-lock] [--yes] | self version
icm mcp                                                  # phase 7, optional: MCP over stdio on the same command layer
icm __session …                                          # internal: the detached session host spawned by `run`
```

**What each command guarantees**

- **`new`**
  - Copies the embedded `examples/app`, substitutes the identifiers, and rewrites the iced lines to the pinned source.
  - Writes `.icm/icm.schema.json`, generates a placeholder icon, and runs `git init` unless `--no-git`.
  - Does no network work.
  - Defaults: the name comes from the directory, and `--id` is `com.example.<name>`. That id is a placeholder: a WARN in dev, exit 9 in release.
- **`init`** adopts an existing crate. It reads the given native files, writes every non-managed key as an icm.toml overlay, and prints the differences between the files icm would generate and the adopted ones. This is a one-time conversion, not a permanent mode (§15).
- **`doctor`** reports one CHECK per requirement.
  - `--fix` applies local, idempotent fixes:
    - create the managed simulator and AVD
    - create `~/.android/debug.keystore`
    - write `.icm/icm.schema.json`
  - `--fix --yes` also runs fixes that download or install, each one printed first:
    - `rustup target add`
    - `sdkmanager --install "ndk;29.0.14206865" "platforms;android-36" "build-tools;36.0.0" "system-images;android-36;google_apis;arm64-v8a"`
    - `xcodebuild -downloadPlatform iOS`, about 8 GB
    - `cargo install wasm-bindgen-cli --version <lock> --locked --root <cache>`
    - pinned downloads of bundletool, binaryen and appimagetool
  - **Licence acceptance is never automated.** `sdkmanager --licenses` and `sudo xcodebuild -license` are owner steps (exit 9).
- **`check`** works without running anything on a device:
  - validates the config, then runs the lockfile checks
  - runs `cargo check -p <pkg> --target <triple>` for each platform: `--lib` for Android, `--bin` elsewhere. No NDK is needed with NativeActivity.
  - validates the generated native files
- **`build`** produces the runnable dev bundle and stops there.
- **`run`** follows §10 and returns while the app keeps running. On web it starts a detached session and returns after `READY`. `--attach` stays in the foreground and streams logs until the app exits or Ctrl-C (on web, until Ctrl-C).
- **`shot`** captures the running app. `shot --headless` renders the real view through the app's harness and needs no device (§13.2).
- **`logs`** re-reads live sources from the launch mark (§13.3). `--follow` is the only streaming mode.
- **`input`** behaves differently per platform in phase 1:
  - android: adb
  - web: CDP, through the session
  - ios-sim: AXe if it is on PATH; otherwise exit 4 with the install command, `by: agent`
  - desktop and ios-device: exit 2 `input.unsupported`, pointing at `icm ui --headless` and phase 6
- **`release`** = build + bundle + sign + every store gate + `dist/` + `artifacts.json` + `UPLOAD.md` + `upload.sh`.
  - `--sign auto` (default) requires the signing assets and exits 9 with owner steps when they are missing.
  - `--sign none` produces unsigned artifacts and marks `uploadable:false`. Every owner-dependent precondition and gate (placeholder id or icon, `config.owner_decision`, export compliance, signing, profile, keystore) becomes a WARN; every other gate keeps its severity. This is the path CI and agents use without signing assets.
- **`verify`** runs the store gates on an existing artifact. When an `artifacts.json` sits next to it, `verify` uses the severities of the release that produced it (so an unsigned `--sign none` artifact verifies ok with WARNs). Artifacts built elsewhere get every gate at full severity.
- **`version`** edits Cargo.toml `version` and icm.toml `build` with toml_edit, keeping the files' formatting.

`icm print commands --json` emits the whole surface, with argument schemas, from the clap definitions. Every command's `--help` has two worked examples.

---

## 7. Configuration: `icm.toml`

### 7.1 Why a separate file, not `[package.metadata.icm]`
1. **App identity is not a crate property.** Tawara builds its mobile targets from `tawara-mobile` in one repo and its desktop target from `tawara-desktop` in another, and any app can use different packages per platform.
2. **Size.** Signing references, privacy reasons, permissions and overlays would swamp the Cargo.toml that agents and `cargo add` edit, and Cargo silently ignores typos in metadata.
3. **Validation and discoverability.** icm.toml has a JSON Schema (`#:schema ./.icm/icm.schema.json`, local, so it works offline). Unknown keys are rejected with `file:line`, and `ls` shows the file.
4. **No second source for the version.** The marketing version stays in Cargo.toml (`env!("CARGO_PKG_VERSION")` works). The store build number has no home in Cargo, so it lives here.

### 7.2 Annotated example (the template's file)
```toml
#:schema ./.icm/icm.schema.json        # written by `icm new` / `icm print schema config --write`
schema = 1
icm = ">=0.14.1-mobile.1"              # older icm binaries exit 4 config.too_new

[app]
name = "App"                           # CFBundleDisplayName/Name, android:label, <title>, desktop product name
id = "com.example.app"                 # permanent after the first store upload; placeholder → release exit 9
build = 1                              # CFBundleVersion, versionCode, deb revision; `icm version bump build`
platforms = ["desktop", "web", "ios", "android"]
package = "app"                        # default: the package whose Cargo.toml sits next to this file
lib = "app"                            # Android: lib<lib>.so, android.app.lib_name
bin = "app"                            # iOS/desktop/web executable
icon = "assets/icon.png"               # ≥1024×1024 square PNG; alpha is flattened onto `background`
background = "#FFFFFF"                 # launch colour, icon flattening, adaptive-icon background, web theme
orientations = ["portrait"]            # portrait | portrait-upside-down | landscape-left | landscape-right
publisher = "Example Ltd"
copyright = "© 2026 Example Ltd"
description = "A starter app."
category = "utilities"                 # → LSApplicationCategoryType, .desktop Categories, deb Section
resources = []                         # globs bundled into .app root, Android assets/, web site/, desktop resources
agent = true                           # phase 6: compile the debug-only agent bridge into `icm run` builds

[app.permissions]                      # one vocabulary, mapped per platform (§7.5)
internet = true

[ios]
min_os = "16.0"                        # MinimumOSVersion = IPHONEOS_DEPLOYMENT_TARGET = actool target (store: ≥13)
devices = ["iphone"]                   # schema 1 accepts only iphone
# team_id = "ABCDE12345"               # required for ios-device and release
# uses_non_exempt_encryption = false   # REQUIRED for release; no default; the owner answers it
# export_compliance_code = ""          # ITSEncryptionExportComplianceCode when the answer is true
# asc_app_id = "1234567890"            # numeric App Store Connect id, used in printed commands
[ios.signing]
development = { identity = "auto", profile = "auto" }   # "auto" | SHA-1 | common name ; "auto" | UUID | path
distribution = { identity = "auto", profile = "auto" }
[ios.privacy]
tracking = false
tracking_domains = []
collected_data = []
api_reasons = { FileTimestamp = ["C617.1"], SystemBootTime = ["35F9.1"] }   # what iced itself imports [V nm]
[ios.entitlements]                     # added to the minimal set; each must be allowed by the profile
[ios.info_plist]                       # extra keys; managed keys are refused (config.managed_key)

[android]
min_sdk = 26
target_sdk = 36                        # Play floor since 2026-08-31 [S]
abis = ["arm64-v8a", "x86_64"]         # release; dev builds only the device's ABI
activity = "native"                    # "game" reserved (needs a dex path; phase 7)
back = "system"                        # Android's Back; "key": enableOnBackInvokedCallback="false", Back as a key for an app that handles it (temporary opt-out; WARN in release)
allow_backup = true
res = "platform/android/res"           # optional; compiled with aapt2 and layered over generated res
extra_permissions = []
[android.manifest]
application = {}                       # extra <application> attributes, e.g. { "android:dataExtractionRules" = "@xml/rules" }
activity = {}                          # extra <activity> attributes
extra_manifest_xml = ""                # raw XML inside <manifest> (<queries>, <uses-feature>); validated (§7.4)
extra_application_xml = ""             # raw XML inside <application> (<meta-data>, <provider>)
# [android.signing]
# upload = { keystore = "~/.icm/keys/app-upload.jks", alias = "upload",
#            store_pass_env = "ICM_ANDROID_STORE_PASS", key_pass_env = "ICM_ANDROID_KEY_PASS" }
[android.play]
track = "internal"
service_account_json_env = "PLAY_SERVICE_ACCOUNT_JSON"   # printed commands only

[web]
public_url = "/"
host = "generic"                       # generic | cloudflare-pages | netlify | github-pages | s3
project = ""                           # host project or bucket, for printed commands
size_budget_kb = 4096                  # gzip size of the .wasm; release FAIL above
rustflags = []

[desktop.macos]
min_os = "12.0"                        # MACOSX_DEPLOYMENT_TARGET and LSMinimumSystemVersion
universal = false                      # true needs rustup target x86_64-apple-darwin (missing here [V])
identity = "auto"                      # Developer ID Application
# notary_profile = "icm-notary"        # created by the owner with `notarytool store-credentials`
[desktop.windows]
formats = ["msi", "nsis"]
# sign_command = "jsign --storetype TRUSTEDSIGNING ... {file}"   # credentials only via env
[desktop.linux]
formats = ["deb", "appimage"]
glibc_floor = "2.35"                   # Ubuntu 22.04+, Debian 12+
# maintainer = "Example Ltd <dev@example.com>"   # required for .deb
deb_depends = []
deb_recommends = []

[test]
flows = "tests/flows"
viewports = ["iphone-17", "pixel-9", "web-mobile", "desktop"]

[checks]                               # post-run hook scripts per platform (§13.6)
# ios-sim = ["platform/ios/checks.sh"]

[review]
snapshot = false                       # true: `check` fails when platform/generated/* is stale
```
Per-platform tables may override `package` and `bin`. Release-only fields are checked when `release` runs, as `config.owner_decision`, exit 9.

### 7.3 Key mapping (reference)

| Key | Maps to |
|---|---|
| `app.name` | CFBundleDisplayName, CFBundleName, `android:label`, `<title>`, `.desktop` Name, Windows ProductName |
| `app.id` | CFBundleIdentifier, `package`/applicationId, macOS bundle id, Windows UpgradeCode seed, deb package base (unless `deb_package`) |
| Cargo `version` | CFBundleShortVersionString (gate X[.Y[.Z]], no pre-release), versionName, MSI ProductVersion, deb Version |
| `app.build` | CFBundleVersion, versionCode (≤ 2 100 000 000), Windows VERSIONINFO 4th field, deb revision |
| `app.orientations` | UISupportedInterfaceOrientations; Android `screenOrientation` only when one direction is locked (ignored at ≥600dp under API 36 [S]) |
| `ios.min_os` | MinimumOSVersion, `IPHONEOS_DEPLOYMENT_TARGET`, actool `--minimum-deployment-target`. icm sets the env var itself; there is no `.cargo/config.toml` copy (`icm print env ios` exports it for raw cargo) |
| `android.min_sdk` / `target_sdk` | `<uses-sdk>` in the generated manifest; aapt2 is also passed `--replace-version`; gates read the linked artifact |
| `desktop.macos.min_os` | `MACOSX_DEPLOYMENT_TARGET`, LSMinimumSystemVersion (gate: the Mach-O minos must equal it) |

### 7.4 Managed keys, overlays, raw XML
**Managed keys** are always generated. Setting one in an overlay is `config.managed_key` (exit 3), naming the right icm.toml key.

- **iOS:** CFBundleIdentifier, CFBundleExecutable, CFBundleName, CFBundleDisplayName, CFBundlePackageType, CFBundleShortVersionString, CFBundleVersion, CFBundleSupportedPlatforms, CFBundleInfoDictionaryVersion, MinimumOSVersion, LSRequiresIPhoneOS, UIDeviceFamily, UIRequiredDeviceCapabilities, UISupportedInterfaceOrientations, UIApplicationSceneManifest, UILaunchScreen, CFBundleIcons, CFBundleIconName, ITSAppUsesNonExemptEncryption, ITSEncryptionExportComplianceCode, every `DT*` key, BuildMachineOSBuild, and the NS*UsageDescription keys produced by `[app.permissions]`.
- **Android:** `package`, `versionCode`, `versionName`, `<uses-sdk>`, `android:hasCode`, `android:extractNativeLibs`, `android:debuggable`, `android:label`, `android:allowBackup`, `android:enableOnBackInvokedCallback` (`[android] back`), the application's `android:theme`, the activity name, `android:exported`, `android:launchMode`, `android:windowSoftInputMode`, `android:screenOrientation`, `android.app.lib_name`, the launcher intent filter, `android:icon`, `android:roundIcon`, and `android:configChanges`. The configChanges list already names every change the target SDK knows, and no value can be removed. An overlay attribute would be written next to the generated one, which aapt2 rejects as a duplicate attribute only at link time, so validation refuses it first; a value only icm writes is refused with the reason it is fixed.

**Overlays.**
- `[ios.info_plist]`, `[ios.entitlements]` and `[android.manifest] application|activity` add keys.
- `platform/android/res/` adds resources, which win over generated ones.
- `platform/ios/resources/` is copied into the bundle root.

**Raw XML** (`extra_manifest_xml`, `extra_application_xml`) is parsed with quick-xml. icm rejects `<uses-sdk>`, `<application>`, `<activity android:name="android.app.NativeActivity">`, and the attributes `android:versionCode`, `android:versionName`, `android:debuggable` and `package` (`config.raw_xml_forbidden`). This closes the hole where aapt2's `--version-code`/`--min-sdk-version` silently fail to override a value already in the manifest [V judge 2].

**Review snapshots** are opt-in with `[review] snapshot = true`. `icm print snapshots --write` writes `platform/generated/{ios-Info.plist,AndroidManifest.xml,PrivacyInfo.xcprivacy}` for code review, and `check` fails `review.snapshot_stale` when they differ. Nothing reads these files as input.

### 7.5 Permission vocabulary

| Key | iOS | Android | macOS |
|---|---|---|---|
| `internet = true` | — | `INTERNET` | — |
| `camera = "<why>"` | NSCameraUsageDescription | `CAMERA` | NSCameraUsageDescription |
| `microphone = "<why>"` | NSMicrophoneUsageDescription | `RECORD_AUDIO` | same |
| `face_id = "<why>"` | NSFaceIDUsageDescription | `USE_BIOMETRIC` | — |
| `photos = "<why>"` | NSPhotoLibraryUsageDescription | `READ_MEDIA_IMAGES` | — |
| `location = "<why>"` | NSLocationWhenInUseUsageDescription | `ACCESS_FINE_LOCATION` | same |
| `notifications = true` | (runtime request) | `POST_NOTIFICATIONS` | — |

Escape hatches are `android.extra_permissions` and `[ios.info_plist]`. Debug Android builds always add `INTERNET` when `agent = true`, because the bridge goes over `adb reverse` (phase 6). Release builds never add it implicitly.

### 7.6 `~/.config/icm/host.toml` (per machine, never committed)
```toml
android_sdk = "/opt/homebrew/share/android-commandlinetools"
chrome = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
[ios]
simulator_type = "iPhone 17"      # default: newest "iPhone <n>" device type installed
simulator_udid = ""               # pin an existing simulator instead of the managed one
[android]
avd = ""                          # default: managed icm_api<target_sdk>
device = ""                       # pin a serial
emulator_ports = [5580, 5582, 5584]   # even ports ≤ 5584 only [V emulator help]
```

---

## 8. The template

### 8.1 File tree (`icm new demo --id com.example.demo`)
```
demo/
  Cargo.toml
  Cargo.lock                 # committed: release builds use --locked
  icm.toml
  rust-toolchain.toml
  .gitignore                 # /target
  .icm/icm.schema.json       # editor completion for icm.toml (local; works offline)
  .icm/ledger.toml           # upload ledger; written only by `icm ledger mark-uploaded`
  AGENTS.md                  # generated by `icm docs agents --write` (§8.4)
  README.md                  # the six commands
  assets/icon.png            # 1024×1024 placeholder (WARN in dev, release exit 9 until replaced)
  src/lib.rs                 # the whole app; identical on every platform
  src/main.rs                # desktop, web and iOS entry
  tests/icm.rs               # harness = false: .ice flows, headless shots and tree (framework F4)
  tests/flows/smoke.ice
  platform/README.md         # where overrides go (android/res, ios/resources); empty otherwise
```

### 8.2 Key files
`Cargo.toml` (as written by `icm new`; in the repo `examples/app` uses `path = "../.."` and `path = "../../test"`):
```toml
[package]
name = "demo"
version = "0.1.0"              # the marketing version; nowhere else
edition = "2024"
rust-version = "1.88"
publish = false

[lib]                          # no crate-type: icm builds Android with `cargo rustc --lib --crate-type cdylib`
path = "src/lib.rs"

[[bin]]
name = "demo"
path = "src/main.rs"

[[test]]
name = "icm"
path = "tests/icm.rs"
harness = false

[features]
icm-agent = ["iced/agent"]     # never in `default`; `icm run` adds it to dev builds when [app] agent = true
                               # (iced's `agent` feature exists, empty, from v0.14.1-mobile.1; F7 fills it in phase 6)

[dependencies]
# Same URL and tag on every iced line, character for character (`icm framework set` changes them all).
iced = { git = "https://github.com/patricksmithlaravel/iced_mobile", tag = "v0.14.1-mobile.1", features = ["fira-sans"] }
log = "0.4"

[target.'cfg(target_arch = "wasm32")'.dependencies]
iced = { git = "https://github.com/patricksmithlaravel/iced_mobile", tag = "v0.14.1-mobile.1", features = ["fira-sans", "webgl"] }

[dev-dependencies]
iced_test = { git = "https://github.com/patricksmithlaravel/iced_mobile", tag = "v0.14.1-mobile.1" }

[profile.dev.package."*"]
opt-level = 2                  # keep debug builds usable on phones

[profile.release]
lto = "thin"

[profile.web-release]          # used by `icm release web`
inherits = "release"
opt-level = "z"
lto = true
codegen-units = 1
panic = "abort"
```
iced's default features already select NativeActivity and the mobile logger, and they are inert elsewhere [V `Cargo.toml:25`]. `fira-sans` gives every platform, including the headless renderer, the same glyphs. There is no exact `wasm-bindgen` pin; icm installs the CLI version that matches the app's lock.

`src/lib.rs` uses only the fork's public API. These items exist [V]: `iced::Program` (re-exported at `src/lib.rs:661`), `iced::application`, `iced::mobile::init_logger` and `iced::android_main!`. Framework CI compiles the file as `examples/app`.
```rust
//! The whole app. The same code runs on desktop, web, iOS and Android.
use iced::widget::{button, column, container, text, text_input};
use iced::{Application, Element, Fill, Font, Program, Task, Theme};

#[derive(Default)]
pub struct App {
    count: i64,
    name: String,
}

#[derive(Debug, Clone)]
pub enum Message {
    Increment,
    NameChanged(String),
    Submit,
}

impl App {
    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Increment => self.count += 1,
            Message::NameChanged(name) => self.name = name,
            Message::Submit => log::info!("submitted {:?}", self.name),
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let content = column![
            text(format!("Count: {}", self.count)).size(32),
            button("Increment").on_press(Message::Increment),
            text_input("Your name", &self.name)
                .id("name")
                .on_input(Message::NameChanged)
                .on_submit(Message::Submit),
        ]
        .spacing(16);

        // Room for status bar, notch and home indicator (targetSdk 36 is edge-to-edge). As built (§8.5),
        // the template pads with iced::mobile::safe_area() and keeps a fixed padding until it arrives.
        container(content).padding(48).width(Fill).height(Fill).into()
    }
}

/// The program, shared by run() and by tests/icm.rs (flows, headless shots, widget tree).
pub fn application() -> Application<impl Program<Message = Message, Theme = Theme>> {
    iced::application(App::default, App::update, App::view)
        .title("App")
        .default_font(Font::with_name("Fira Sans"))
}

/// Desktop, web and iOS call this from main.rs; Android from android_main below.
pub fn run() -> iced::Result {
    iced::mobile::init_logger(); // platform logger + panic hook (framework F2); idempotent
    application().run()
}

// Android's entry point; expands to nothing on other targets.
iced::android_main!(run);
```
`src/main.rs`:
```rust
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
fn main() -> iced::Result {
    demo::run() // never returns on iOS
}
```
`tests/icm.rs`:
```rust
//! `cargo test` runs tests/flows/*.ice. `icm shot --headless` and `icm ui --headless` call this
//! binary with arguments (harness protocol 1).
fn main() -> std::process::ExitCode {
    iced_test::agent::main(demo::application(), env!("CARGO_MANIFEST_DIR"))
}
```
`tests/flows/smoke.ice`:
```
viewport: 402x874
mode: Immediate
-----
click "Increment"
expect "Count: 1"
```
`rust-toolchain.toml`:
```toml
[toolchain]
channel = "1.98.0"     # the toolchain this icm release was tested with
components = ["clippy", "rustfmt"]
targets = ["aarch64-apple-ios", "aarch64-apple-ios-sim", "aarch64-linux-android", "x86_64-linux-android", "wasm32-unknown-unknown"]
```
`README.md` lists six commands: `icm doctor`, `icm run desktop|web|ios-sim|android`, `icm test`, `icm shot --headless --all-viewports`, `icm release <target>`, and `icm explain <id>`.

### 8.3 Placeholder handling
`icm new` records the sha256 of the generated icon in `.icm/ledger.toml` (`[template] icon_sha256`). It fires `app.icon.placeholder` while `assets/icon.png` still matches. `app.id.placeholder` fires while `app.id` starts with `com.example.`.

### 8.4 AGENTS.md (generated; `icm docs agents --write` refreshes it for the installed icm and framework tag)
```markdown
# AGENTS.md — {{name}} ({{id}}) · iced_mobile {{framework_tag}} · icm {{icm_version}}

One Rust codebase (src/lib.rs) runs on desktop, web, iOS and Android. Build, run, see, test and
package it ONLY through `icm`, and read the last JSON line of every command.

## The loop
1. `icm check --all --json -q` — compiles every platform. Fix the `diagnostic` events.
2. `icm run <desktop|web|ios-sim|android> --json -q` — builds, installs, launches, waits for the first
   frame, screenshots, returns. Then OPEN `artifacts.preview` and look at it. Every run, every time.
3. `icm logs <platform> --level warn --json` when anything looks wrong (re-read live, not cached).
4. `icm test --json -q` — unit tests + tests/flows/*.ice in a headless renderer.
5. `icm shot --headless --all-viewports --json -q` — layout at phone and desktop sizes, no device.
6. `icm ui --headless tree --json` — the widget tree with bounds, for writing .ice flows.
7. `icm stop --all` when done.
Input on a device: `icm input android tap X Y`, `icm input web tap X Y`, `icm input ios-sim tap X Y` (needs AXe).

## Results
- Exit 0 ok · 1 check/test failed · 2 usage · 3 config · 4 environment (`icm doctor <p> --fix --yes`)
  · 5 build · 6 tool · 7 device · 8 timeout · 9 OWNER NEEDED · 10 app crashed / never drew · 70 icm bug.
- Exit 9: STOP. Give the owner `errors[0].fix`. Do not work around it, do not guess credentials.
- Every error has an id: `icm explain <id>`. Read the files in `errors[].evidence`.
- `fix.by` says who acts: agent | doctor | doctor-yes | owner.
- Raw `simctl launch` / `adb install` exit codes prove nothing; `icm run` checks the app is alive and drew.

## Where things are
- App code: src/lib.rs. src/main.rs, tests/icm.rs and `iced::android_main!(run)` are fixed: don't edit them.
- Identity, icon, permissions, orientations, signing references: icm.toml (`icm explain config.<key>`).
- Version: Cargo.toml `version`. Build number: icm.toml `[app] build` (`icm version bump build`).
- Info.plist, AndroidManifest.xml, PrivacyInfo.xcprivacy, index.html are GENERATED from icm.toml on every
  build. View them with `icm print info-plist ios` / `icm print manifest android`. Never create them.
- Outputs: target/icm/latest/<platform>/ (screen.png, screen.preview.png, app.log, result.json).
- Raw cargo for a target: `eval "$(icm print env android)"` first. `cargo check` needs no NDK.

## Rules that fail silently when broken
- Keep every iced line on the same git URL and tag (`icm framework set tag:<t>` changes them all).
- Never call `iced::exit()` or close the last window on Android or iOS.
- Keep padding the root with `iced::mobile::safe_area()` (Android targetSdk 36 is edge-to-edge); a fixed padding
  stands in until it arrives. Headless, a phone preset's viewport gets that phone's insets.
- .ice `click` and host tests use a mouse; phones use touch. Confirm UI changes with `icm run` on
  ios-sim and android and look at the screenshot.
- Keep `features = ["fira-sans"]`; text with no font renders as nothing.

## Releases belong to the owner
- `icm version bump build`, then `icm release <ios|android|web|macos|windows|linux> --json -q`.
  Artifacts, UPLOAD.md and upload.sh land in target/icm/dist/<version>+<build>/<target>/.
- NEVER run upload.sh or any command in UPLOAD.md (altool, notarytool, fastlane, wrangler, …).
- No signing assets? `icm release <target> --sign none` checks everything else.

## Known limitations of iced_mobile {{framework_tag}}
{{docs/agents/limitations.md of that tag: safe area only from a running app; no edit menu on mobile; Android IME
is key events only (no composition, accents and CJK input unreliable); a drag starting on a button does not
scroll; text_editor and rich-text links ignore touch; dark-mode switches keep Android's bar icons; Lifecycle::Suspended
means "inactive" on iOS and "window lost" on Android, so don't lock on it alone; one window on Android;
iPad unsupported; non-Latin text depends on system fonts (verify on device)}}
```
The fork's own root `AGENTS.md` gets a `cli/` section:
- Run `cargo test --manifest-path cli/Cargo.toml`.
- Regenerate golden files with `ICM_BLESS=1`.
- Fake tools go through `ICM_TOOL_*`.
- Never put upload or notarize argv outside `cli/src/release/owner_plans.rs`; CI scans for it.
- A new check id needs an `explain` doc.

### 8.5 As built (phase 1)
`examples/app` is the template. Where it differs from §7.2 and §8.1–8.4, it wins:
- `icm.toml` has `min_icm = "0.14.1-mobile.1"`, a plain version compared by semver ordering (Appendix C 5), instead of `icm = ">=…"`. It has no `#:schema` line while schema printing waits (Appendix C 30).
- `Cargo.toml` has no `[profile.*]` (Appendix C 4). Its `icm-agent = ["iced/agent"]` feature resolves against iced's empty `agent` feature (the F7 stub).
- `tests/icm.rs` is the one line of §13.2: `iced_test::agent::main(app::application(), env!("CARGO_MANIFEST_DIR"))`. Besides protocol 1 (`ICM_HARNESS {"protocol":1}` first), every command ends with an `ICM_HARNESS_RESULT <json>` line, and the harness exits 0 (all passed), 1 (a flow failed) or 2 (usage, or a file it cannot read or write); `iced_test::agent` documents the fields.
- `src/lib.rs` is a counter, a text field that submits on Return, and a scrollable list whose rows keep their buttons small (review A2). `safe_area()` pads 64 top, 48 bottom and 16 at the sides on Android and iOS, 16 elsewhere. Its unit tests send touch events, not mouse events.
- Since the platform services landed, `src/lib.rs` also uses them: the root padding is `SafeArea::padding(16)` from `iced::mobile::safe_area()` (`App::padding`), with the 64/48 padding above as `fallback_padding` until the safe area arrives; no `.theme(..)`, so the default theme follows the system, and a status line shows the mode from `iced::system::theme_changes()` and the state from `iced::mobile::lifecycle()`; a Paste button beside the field and a Copy button on each row use `iced::clipboard`. `tests/flows/copy_paste.ice` copies, removes and pastes an item (`mode: Zen`).
- `icm new` substitutes:
  - the package, library and binary name `app`: Cargo.toml, `app::` in `src/main.rs` and `tests/icm.rs`, and icm.toml `package`, `lib` and `bin`;
  - the display name `App`: icm.toml `name` and `.title("App")`;
  - the id `com.example.app`;
  - the path dependencies `path = "../.."` and `path = "../../test"`, which become the pinned git source.
- `AGENTS.md` is the template `icm docs agents` fills: `{{name}}`, `{{id}}`, `{{framework_tag}}`, `{{icm_version}}`, and `{{limitations}}`, which is `docs/agents/limitations.md` after its `<!-- icm: … -->` marker line. It describes only phase 1 commands.
- `assets/icon.png` is the placeholder icon (1024×1024, opaque RGB). icm keeps its sha256 for `app.icon.placeholder` (Appendix C 12).
- `rust-toolchain.toml` pins 1.98.0. Cargo run from the fork's root ignores it, but cargo run inside `examples/app` (or a new app) uses 1.98.0, whose wasm and Android targets are missing on this host: icm runs children with `RUSTUP_AUTO_INSTALL=0` and `doctor --fix --yes` adds them (Appendix C 8).

---

## 9. Generated platform files

### 9.1 Info.plist
**Simulator.** The managed keys plus:
- `CFBundleSupportedPlatforms=[iPhoneSimulator]`
- the scene manifest:
  - `UIApplicationSceneManifest = { UIApplicationSupportsMultipleScenes = false; UISceneConfigurations = { UIWindowSceneSessionRoleApplication = ( { UISceneConfigurationName = Default } ) } }` (the same keys as Tawara's `Info.plist.in` [V])
- `UILaunchScreen = { UIColorName = LaunchBackground }`
- `UIRequiredDeviceCapabilities=[arm64]`, `UIDeviceFamily=[1]`, `LSRequiresIPhoneOS=true`
- `CFBundleIcons`/`CFBundleIconName` merged from actool's partial plist

The simulator plist has **no DT keys**, so the dev loop runs no `xcodebuild` queries.

**Device and release** add `CFBundleSupportedPlatforms=[iPhoneOS]`, `ITSAppUsesNonExemptEncryption` (release requires it), `ITSEncryptionExportComplianceCode` (if set), and these keys, read at build time:

| Key | Read from |
|---|---|
| `DTSDKBuild`, `DTPlatformBuild` | `xcodebuild -version -sdk iphoneos ProductBuildVersion` (24A430 here [V]) |
| `DTPlatformVersion` / `DTSDKName` | `xcodebuild -version -sdk iphoneos PlatformVersion` → `27.0` / `iphoneos27.0` |
| `DTPlatformName` | `iphoneos` |
| `DTXcode` | `<xcode-select -p>/../Info.plist` key `DTXcode`, read with the plist crate (2700 [V]) |
| `DTXcodeBuild` | `xcodebuild -version` (27A266a [V]) |
| `DTCompiler` | `<xcode-select -p>/Platforms/iPhoneOS.platform/Info.plist` `DefaultProperties.DEFAULT_COMPILER` |
| `BuildMachineOSBuild` | `sw_vers -buildVersion` |

### 9.2 PrivacyInfo.xcprivacy
```xml
<dict>
  <key>NSPrivacyTracking</key><false/>
  <key>NSPrivacyTrackingDomains</key><array/>
  <key>NSPrivacyCollectedDataTypes</key><array/>
  <key>NSPrivacyAccessedAPITypes</key><array>
    <dict><key>NSPrivacyAccessedAPIType</key><string>NSPrivacyAccessedAPICategoryFileTimestamp</string>
          <key>NSPrivacyAccessedAPITypeReasons</key><array><string>C617.1</string></array></dict>
    <dict><key>NSPrivacyAccessedAPIType</key><string>NSPrivacyAccessedAPICategorySystemBootTime</string>
          <key>NSPrivacyAccessedAPITypeReasons</key><array><string>35F9.1</string></array></dict>
  </array>
</dict>
```

### 9.3 Entitlements
- **Development:** `application-identifier` = `<TEAM>.<id>`, `com.apple.developer.team-identifier`, `get-task-allow=true`, plus `[ios.entitlements]`.
- **Distribution:** `application-identifier`, `com.apple.developer.team-identifier`, `get-task-allow=false`, `beta-reports-active=true`, plus `[ios.entitlements]`. `keychain-access-groups` is added only when configured.

Each key must be allowed by the profile's `Entitlements`, with wildcards resolved. The profile's entitlements are **never copied wholesale**.

### 9.4 AndroidManifest.xml (dev variant; aapt2 `--debug-mode` adds `debuggable`, release does not)
```xml
<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="com.example.app"
    android:versionCode="1" android:versionName="0.1.0">
  <uses-sdk android:minSdkVersion="26" android:targetSdkVersion="36"/>
  <uses-permission android:name="android.permission.INTERNET"/>
  <application android:label="App" android:icon="@mipmap/ic_launcher" android:roundIcon="@mipmap/ic_launcher_round"
      android:hasCode="false" android:extractNativeLibs="false" android:allowBackup="true"
      android:theme="@style/IcmTheme">
    <activity android:name="android.app.NativeActivity" android:exported="true" android:launchMode="singleTask"
        android:windowSoftInputMode="adjustResize|stateHidden"
        android:configChanges="mcc|mnc|locale|touchscreen|keyboard|keyboardHidden|navigation|screenLayout|fontScale|uiMode|orientation|density|screenSize|smallestScreenSize|layoutDirection|colorMode|fontWeightAdjustment|grammaticalGender|assetsPaths">
      <meta-data android:name="android.app.lib_name" android:value="app"/>
      <intent-filter>
        <action android:name="android.intent.action.MAIN"/>
        <category android:name="android.intent.category.LAUNCHER"/>
      </intent-filter>
    </activity>
  </application>
</manifest>
```
- The configChanges list is the review §6.6 list: Tawara's values plus `mcc|mnc|grammaticalGender`, and `assetsPaths` from API 36 (Appendix D, Android). It is policy data keyed by API level.
- `android:enableOnBackInvokedCallback="false"` is added only with `[android] back = "key"` (API 33 and later), for an app that handles Back itself: Back then reaches it as `Key::Named(Named::BrowserBack)`. With the default `"system"`, Back at the app's root finishes the activity, and the application starts over at the next launch (the framework ends it with its activity).
- Generated resources:
  - `values/themes.xml`: `IcmTheme`, parent `@android:style/Theme.Material.NoActionBar`, with `windowBackground` set from `background`
  - `values/window.xml` and `values-night/window.xml`: the window background and the bar icons' colour that `IcmTheme` reads, in light and dark mode (Appendix D, Android)
  - legacy `mipmap-{m,h,xh,xxh,xxxh}dpi/ic_launcher.png` and `ic_launcher_round.png`, 48 to 192 px
  - `mipmap-anydpi-v26/ic_launcher.xml` and `ic_launcher_round.xml`: adaptive icons with a background colour and the foreground padded to the 66% safe zone
- Release also writes `play-icon-512.png` into `dist/` for the store listing.

### 9.5 Web
**Dev `index.html`**:
```html
<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<title>App</title><link rel="icon" href="icon-32.png"><link rel="manifest" href="manifest.webmanifest">
<style>html,body{margin:0;height:100%;background:#FFFFFF}canvas{display:block;width:100%;height:100%}</style>
<script>/* dev only: forward console/errors to /__icm/log; SSE reload with --watch */</script>
</head><body>
<script type="module">import init from "./pkg/app.js"; init({ module_or_path: "./pkg/app_bg.wasm" });</script>
</body></html>
```
**Release** is the same page without the dev script, with `<base href="{public_url}">`, and with hashed names passed explicitly:
```js
import init from "./pkg/app-<h8>.js"; init({ module_or_path: "./pkg/app_bg-<h8>.wasm" });
```
Release also writes:
- `manifest.webmanifest`
- icons at 32, 180, 192 and 512 px, plus a maskable icon
- `_headers`: `/*.wasm  Content-Type: application/wasm`, immutable caching on hashed files, `index.html` no-cache
- `404.html` = index
- `.nojekyll`

### 9.6 Desktop
- **macOS `Contents/Info.plist`:** identity and version keys, `LSMinimumSystemVersion`, `CFBundleIconFile=AppIcon`, `NSHighResolutionCapable`, `LSApplicationCategoryType`, `NSHumanReadableCopyright`.
- **Windows:**
  - `app.rc`: the icon (`app.ico`, PNG frames 16–256, own writer) and VERSIONINFO (FileVersion `X.Y.Z.build`, ProductVersion `X.Y.Z`)
  - `app.wxs` (WiX v5): per-machine; `UpgradeCode` = a GUID derived from SHA-256(`app.id`), or `[desktop.windows] upgrade_code`; `MajorUpgrade AllowSameVersionUpgrades="yes"`; a Start-menu shortcut
  - `installer.nsi`: per-user, installs to `$LOCALAPPDATA\Programs\<Name>`, with an uninstaller
- **Linux:**
  - `<id>.desktop`
  - `hicolor/<size>/apps/<id>.png`
  - `DEBIAN/control` (Package, `Version: <version>-<build>`, Architecture, Maintainer, Depends, Recommends, Section, Description)
  - an `AppRun` that sets `LD_LIBRARY_PATH=$APPDIR/usr/lib`

---

## 10. Dev-loop pipelines (phase 1, except ios-device in phase 2)

**Common prelude.** Resolve the config. Take the platform and device locks. Run the doctor subset for this platform, which exits 4 or 9 *before* a long build. Create the run dir, emit `start`, and generate files (hash-stamped, so unchanged inputs skip regeneration).

**Build env for every dev build:**
- `--features icm-agent` when phase 6 is present and `[app] agent = true`
- `RUST_BACKTRACE=1` in the app env
- `ICM_RUN_ID=<run id>`

### 10.1 desktop
1. `cargo build -p <pkg> --bin <bin> [--release] --message-format=json-render-diagnostics`. The exe path comes from the `compiler-artifact` message. On macOS, `MACOSX_DEPLOYMENT_TARGET=<desktop.macos.min_os>`.
2. Spawn the app detached, in its own process group: stdout to `runs/<id>/app.stdout`, stderr to `runs/<id>/app.stderr`. The env adds `RUST_LOG`, `ICM_RUN_ID`, and `ICM_EVENTS=1` for every build (Appendix C 27). Record the pid in the session file.
3. Ready means `ICM_EVENT ready` in `app.stderr` within `--wait-ready`. The fallback is that the pid is alive after 3 s and owns a window: `CGWindowListCopyWindowInfo` filtered by pid (objc2-core-graphics, in-process).
4. Screenshot:
   - **macOS:** `CGPreflightScreenCaptureAccess()` first.
     - Granted: `screencapture -x -o -l <CGWindowID> runs/<id>/screen.png`.
     - Denied: **WARN** `desktop.shot.permission` (the owner may grant Screen Recording to the terminal), then a headless render at the window's size. The result is still ok.
   - **Linux X11:** `import -window <id>`. CI uses Xvfb.
   - **Wayland and Windows:** a headless render, until the phase 6 bridge.
5. Write the preview, run blank detection, assemble `app.log`, run the hooks, write the result. `icm stop desktop` sends SIGTERM to the process group.

### 10.2 web
1. Gates:
   - the `wasm32-unknown-unknown` target is installed for the project's active toolchain
   - `deps.wasm_bindgen_cli`: the cached wasm-bindgen equals the app lock's version (`doctor web --fix --yes` runs `cargo install wasm-bindgen-cli --version <v> --locked --root <cache>/wasm-bindgen/<v>`)
   - `deps.getrandom_backend`: if getrandom ≥ 0.3 resolves for wasm32 without its wasm_js backend configured, FAIL with the exact `[web] rustflags` line and feature to add [I]
2. `cargo build -p <pkg> --bin <bin> --target wasm32-unknown-unknown [--release] --message-format=json-render-diagnostics`. `[web] rustflags` go into `CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS`, so global RUSTFLAGS are not clobbered. No `web_sys_unstable_apis` is needed [V].
3. `wasm-bindgen <wasm> --target web --no-typescript --out-dir build/web/<profile>/site/pkg --out-name app [--debug --keep-debug]`.
4. Write `index.html` (§9.5), `manifest.webmanifest` and the icons.
5. **Detached session host** (`icm __session --platform web`):
   - Serves `site/` on `127.0.0.1:<port>` with a std `TcpListener`: GET and HEAD only, path sanitization, `application/wasm`, `Cache-Control: no-store`.
     - Endpoints: `POST /__icm/log` (console forwarder) and `/__icm/reload` (SSE, `--watch`).
     - A busy port exits 7 `web.port_busy`; `--port 0` picks a free port.
   - Starts headless Chrome:

     ```
     "<chrome>" --headless=new --remote-debugging-pipe --use-angle=swiftshader --enable-unsafe-swiftshader --user-data-dir=<run>/chrome --no-first-run --no-default-browser-check --hide-scrollbars about:blank
     ```

     CDP runs over fds 3 and 4 as NUL-delimited JSON, so no WebSocket client is needed. It sends `Runtime.enable`, `Log.enable`, `Page.enable`, then `Emulation.setDeviceMetricsOverride` from `--viewport` (default `desktop`), then `Page.navigate`.
   - Collects `Runtime.consoleAPICalled`, `Runtime.exceptionThrown`, `Log.entryAdded` and failed loads into `console.ndjson`.
   - Writes `sessions/web.json` and prints `READY http://127.0.0.1:8787/`.
   - [V] Headless Chrome 154 rendered WebGL2 through SwiftShader. Holding the browser over CDP avoids the `--screenshot` process that never exits.
6. Ready means `ICM_EVENT ready` on the console (30 s limit). The screenshot is `Page.captureScreenshot`. `run` returns, and the session keeps serving. `--attach` keeps everything in the foreground. `--show` also opens the system browser at the URL, and the console forwarder captures its logs.

### 10.3 ios-sim
1. `IPHONEOS_DEPLOYMENT_TARGET=<min_os> cargo build -p <pkg> --bin <bin> --target aarch64-apple-ios-sim [--release] --message-format=json-render-diagnostics`. An Intel host uses `x86_64-apple-ios`.
2. Gates via `object`: `ios.macho.platform` (IOSSIMULATOR, 7) and `ios.macho.minos` = `min_os`.
3. Generate the simulator Info.plist (§9.1), `Assets.xcassets` (AppIcon flattened to an RGB 1024 PNG, plus a LaunchBackground colour set) and PrivacyInfo. Then compile the assets:

   ```
   xcrun actool gen/…/Assets.xcassets --compile build/ios-sim/<p>/<Name>.app --platform iphonesimulator --minimum-deployment-target <min_os> --app-icon AppIcon --target-device iphone --output-partial-info-plist gen/…/actool.plist --errors --warnings
   ```

   Merge the partial plist [V flags]. actool compiles straight into the `.app`, because it writes loose `AppIcon60x60@2x.png` files that the plist references [V judge 2].
4. Copy the exe, Info.plist, PrivacyInfo, `[app] resources` and `platform/ios/resources/*`.
5. Gates: `plutil -lint`, `ios.plist.scene_manifest`, executable present.
6. `xattr -cr <App.app>`, then `codesign --force --sign - --timestamp=none <App.app>`.
7. Simulator:
   - Use `host.toml simulator_udid`, or reuse the managed one named `icm <type> (iOS <ver>)`. Otherwise `xcrun simctl create` it, choosing the device type from `simctl list -j devicetypes` and the newest runtime ≥ `min_os` from `simctl list -j runtimes available` (iPhone 17 / iOS 27.0 here [V]).
   - `xcrun simctl boot <udid>`, tolerating "already Booted", then `xcrun simctl bootstatus <udid> -b` (180 s, `ios.sim.boot_failed`).
   - `--fresh` creates a new simulator and deletes it at `stop`. `--show` runs `open -a Simulator --args -CurrentDeviceUDID <udid>`.
8. `xcrun simctl install <udid> <App.app>` (`ios.sim.install_failed`).
9. Record the launch mark (host clock), then launch:

   ```
   xcrun simctl launch --terminate-running-process --stdout=<run>/app.stdout --stderr=<run>/app.stderr <udid> <id>
   ```

   The env carries `SIMCTL_CHILD_RUST_BACKTRACE=1`, `SIMCTL_CHILD_RUST_LOG=<level>`, `SIMCTL_CHILD_ICM_RUN_ID` and `SIMCTL_CHILD_ICM_EVENTS=1` (every build, Appendix C 27), plus `--env` values with the prefix added. Parse `<id>: <pid>`.
10. Ready:
    - **Primary:** `ICM_EVENT ready` in `app.stderr`.
    - **Fallback:** `xcrun simctl spawn <udid> launchctl list` shows `UIKitApplication:<id>` with a numeric pid on 3 consecutive polls a second apart. `simctl launch` exiting 0 proves nothing.
    - **On death:**
      - `xcrun simctl spawn <udid> log show --style ndjson --start "<mark>" --predicate 'process == "<exe>" OR eventMessage CONTAINS "<id>"'` → `system.ndjson`
      - `~/Library/Logs/DiagnosticReports/<exe>-*.ips` newer than the mark → `crash/`
      - match the failure signatures, then exit 10
11. After `--settle`: `xcrun simctl io <udid> screenshot --type=png <run>/screen.png`, then the preview and blank detection.
12. Merge `app.stdout`, `app.stderr`, `system.ndjson` and crash reports into `app.log` and `logs.ndjson`. Run the `[checks] ios-sim` hooks. Write the session file and the result.
13. `icm stop ios-sim` runs `xcrun simctl terminate <udid> <id>`, plus `simctl shutdown` with `--shutdown`.

### 10.4 android
1. **Device** (order in §6). The managed AVD is `icm_api<target_sdk>`, created by `doctor android --fix`:

   ```
   avdmanager create avd -n icm_api36 -k "system-images;android-36;google_apis;arm64-v8a" -d pixel_9
   ```

   It needs no download when the image is present [V present]; otherwise `--fix --yes` runs sdkmanager. Boot it with:

   ```
   emulator -avd icm_api36 -port <first free of 5580|5582|5584> -no-boot-anim -no-audio -no-snapshot-save -no-window -gpu swiftshader_indirect
   ```

   (`--show`: a window and `-gpu auto`.) It runs detached and its pid goes into the session.
   - `adb -s emulator-<port> wait-for-device`, then poll `getprop sys.boot_completed` = 1 (300 s, `android.emulator.boot_timeout` exit 8). All three ports busy is `android.emulator.ports_busy`.
   - On icm-managed emulators only: animations off, stay awake, `wm dismiss-keyguard`.
2. **ABI.** `adb -s S shell getprop ro.product.cpu.abi`: `arm64-v8a` maps to `aarch64-linux-android`, `x86_64` to `x86_64-linux-android`.
3. **NDK env, computed in Rust** (no bash-3.2 or BSD-sed traps). `icm print env android` prints the same values:
   - `CARGO_TARGET_<T>_LINKER=$TC/<triple><min_sdk>-clang`
   - `CC_<t>`, `CXX_<t>`, `AR_<t>=$TC/llvm-ar`, `RANLIB_<t>`
   - `ANDROID_NDK_HOME`
4. **Build.** `cargo rustc -p <pkg> --lib --crate-type cdylib --target <triple> [--release] --message-format=json-render-diagnostics`. This is the mechanism Tawara already uses [V].
5. **ELF gates via `object`:** `android.so.export` (dynsym `ANativeActivity_onCreate`), `android.so.align16k` (every PT_LOAD `p_align` ≥ 0x4000), and e_machine = ABI. Then `llvm-strip --strip-debug` into `gen/android/<p>/apk/lib/<abi>/lib<lib>.so`.
6. **Resources and link.**

   ```
   aapt2 compile --dir gen/…/res -o gen/…/res.zip     # [-R user res compiled the same way, layered last]
   aapt2 link --output-to-dir -o gen/…/apkdir -I $SDK/platforms/android-<target_sdk>/android.jar --manifest gen/…/AndroidManifest.xml -R gen/…/res.zip [-R gen/…/user-res.zip] --auto-add-overlay --replace-version --version-code <build> --version-name <version> --debug-mode
   ```

   build-tools 36 [V]; the output dir must exist [V].
7. **Package.** The own zip writer packs `apkdir/*`, `lib/<abi>/*.so` and `assets/**` into `gen/…/unaligned.apk`, **all stored**.
8. **Align and sign.**

   ```
   zipalign -f -P 16 4 unaligned.apk aligned.apk
   apksigner sign --ks ~/.android/debug.keystore --ks-pass pass:android --key-pass pass:android --ks-key-alias androiddebugkey --out build/android/<p>/<name>.apk aligned.apk
   ```

   The debug keystore is stable, created once with the standard `keytool -genkeypair … -alias androiddebugkey` command, so `install -r` works across runs. Then **verify the signed APK**: `apksigner verify` and `zipalign -c -P 16 4 <name>.apk` (`android.apk.signature`, `android.apk.zipalign`).
9. **Install.** `adb -s S install -r <name>.apk`. `INSTALL_FAILED_UPDATE_INCOMPATIBLE` exits 7 `android.install.signature_mismatch`, fixed with `icm run android --reinstall --yes`, which uninstalls and **wipes app data**.
10. **Log mark.** `adb -s S shell date +%s.%N` gives `<epoch>` [V]. The log is **never cleared**: no `logcat -c`.
11. **Launch.** For release runs, first `adb -s S shell setprop debug.icm.events 1`. Then `adb -s S shell am start -W -S -n <id>/android.app.NativeActivity`, parsing Status and TotalTime.
12. **Ready.**
    - **Primary:** `ICM_EVENT ready` in `adb -s S logcat -d -v threadtime,epoch -T <epoch> -s ICM_EVENT:I` [V `-v threadtime,epoch`, `-T <epoch>`].
    - **Fallback:** a stable `pidof <id>` plus `dumpsys activity activities` showing `topResumedActivity` for the package.
    - **Evidence on failure:** `logcat -d -b crash -T <epoch>`, and `logcat -d -b events -T <epoch> am_crash:I am_anr:I am_proc_died:I am_destroy_activity:I wm_destroy_activity:I *:S`.
13. **Screenshot.** `adb -s S exec-out screencap -p > screen.png`. If it is blank and `dumpsys window` shows the focused window has `FLAG_SECURE`, report **INFO `android.screen.secure`** instead of a font or theme diagnosis (Tawara sets FLAG_SECURE).
14. **Logs.** `logcat -d -v threadtime,epoch -T <epoch>`, normalized: the app's pid plus the tags `ICM_EVENT`, `iced`, `RustStdoutStderr`, `AndroidRuntime`, `DEBUG`, and `ActivityManager` lines that mention `<id>`. Then the hooks and the result.
15. **Stop.** `adb -s S shell am force-stop <id>`. With `--shutdown`, `adb -s S emu kill` for an icm-managed emulator.

### 10.5 ios-device (phase 2)
**Preconditions (exit 9, none present on this host [V]):** an Apple Development identity, a development profile that contains the device UDID, and Developer Mode on the device.

1. `xcrun devicectl list devices --json-output <tmp>`. Pick `--device` or the single paired device; otherwise exit 7.
2. Identity: `security find-identity -v -p codesigning` → the `Apple Development:` SHA-1.
3. Profile: scan `~/Library/Developer/Xcode/UserData/Provisioning Profiles/` and `~/Library/MobileDevice/Provisioning Profiles/`, decoding each with `security cms -D -i`. The profile must match all of:
   - application-identifier (or a wildcard)
   - the UDID in `ProvisionedDevices`
   - not expired
   - the identity's certificate SHA-1 in `DeveloperCertificates`
4. Build `aarch64-apple-ios` (gate: platform IOS, 2). Bundle as in §10.3 with the device plist (DT keys) and `actool --platform iphoneos`. Development entitlements as in §9.3. Embed the profile.
5. `xattr -cr`, then `codesign --force --sign <SHA-1> --entitlements gen/…/entitlements.plist --timestamp=none --generate-entitlement-der <App.app>`, then `codesign --verify --strict -vv`.
6. `xcrun devicectl device install app --device <udid> <App.app> --json-output <run>/install.json`.
7. Launch as a detached child: `DEVICECTL_CHILD_ICM_RUN_ID=<id> xcrun devicectl device process launch --device <udid> --terminate-existing --console <bundle-id>`, with output to `<run>/console.log`. `--console` rejects `--json-output -` [V]. Ready is `ICM_EVENT ready` in `console.log`; the fallback polls `xcrun devicectl device info processes --device <udid> --json-output <tmp>`.
8. Screenshot: `xcrun devicectl device capture screenshot --device <udid> --destination <run>/screen.png` [V].

---

## 11. Release pipelines and owner commands

`icm release <target>` ends by writing `artifacts.json`, `UPLOAD.md` and `upload.sh` (`set -euo pipefail`; checks that the env vars it needs are set; saves each tool's JSON; runs `icm diagnose` on it; ends with `icm ledger mark-uploaded`). Then it prints `owner_steps`.

**icm never executes any of these.** The only file that may contain upload or notarize argv is `src/release/owner_plans.rs`. A CI scan (§17) fails the build on `altool --upload`, `notarytool submit`, `supply`, `wrangler`, `edits` or `bundles?uploadType` anywhere else.

### 11.1 iOS → App Store Connect (phase 2)
**Preconditions** (exit 9 unless noted):
- `team_id` and `uses_non_exempt_encryption` are set.
- Cargo version is X[.Y[.Z]] with no pre-release (`ios.version.format`, 3).
- `build` > the ledger max for ios.
- Xcode ≥ 26 and **not a beta**: `ios.xcode.not_beta` fails when the `xcodebuild -version` build number ends in a lowercase letter or the developer dir path contains "beta".
- An Apple Distribution identity [V present].
- An App Store profile [V: the only one on this host is for ClearNights, so `ios.sign.profile_mismatch` gives exit 9 with owner steps]. It must have no `ProvisionedDevices`/`ProvisionsAllDevices`, `get-task-allow=false`, and matching team, app id and certificate. It FAILs if it expires within 7 days and WARNs if within 30.

**Pipeline**
1. Build:

   ```
   IPHONEOS_DEPLOYMENT_TARGET=<min_os> CARGO_PROFILE_RELEASE_DEBUG=line-tables-only cargo build --release --locked -p <pkg> --bin <bin> --target aarch64-apple-ios --message-format=json-render-diagnostics
   ```

2. Symbols: `xcrun dsymutil <exe> -o gen/ios/<bin>.dSYM`. Gate: `dwarfdump --uuid` on the dSYM equals the binary's UUID. Then `xcrun strip -S -x` on the bundled copy. Zip the dSYM with `/usr/bin/zip -qry -X`.
3. Mach-O gates via `object` (`ios.macho.*`):
   - platform IOS (2)
   - minos = plist MinimumOSVersion ≥ 13
   - **sdk ≥ 26.0**
   - arm64 only
   - `ios.macho.sdk_matches_dt`: LC_BUILD_VERSION sdk = the DTSDKName version
4. Privacy scan: undefined symbols via `object`, plus a byte scan for ObjC strings.

   | Category | Symbols or strings |
   |---|---|
   | FileTimestamp | `_stat _fstat _fstatat _lstat _getattrlist*` |
   | SystemBootTime | `_mach_absolute_time`, `systemUptime` |
   | DiskSpace | `_statfs* _statvfs* _fstatfs*`, `NSURLVolumeAvailableCapacity*` |
   | UserDefaults | `_OBJC_CLASS_$_NSUserDefaults` |
   | ActiveKeyboards | `activeInputModes` |

   Every detected category needs a reason in config (`ios.privacy.reasons`, ITMS-91053). The FAIL prints the exact `api_reasons` line to add.
5. Icon: flatten the source onto `background` into a **1024 RGB PNG**, so RGBA sources are fine. Compile with `actool --platform iphoneos`. Gate `ios.icon.opaque_1024`: `xcrun assetutil --info <App.app>/Assets.car` reports AppIcon with `"Opaque": true` and `PixelHeight 1024` [V].
6. Write the device Info.plist (§9.1) and PrivacyInfo. Write the distribution entitlements (§9.3, subset-checked against the profile). Embed `embedded.mobileprovision`.
7. **Sign last:**

   ```
   xattr -cr <App.app>
   codesign --force --sign <SHA-1> --entitlements gen/ios/entitlements.plist --timestamp=none --generate-entitlement-der <App.app>
   ```

   It runs under a **60 s keychain-prompt watchdog** with stdin closed; a hang gives exit 9 `ios.sign.keychain_prompt` ("Always Allow" once, or `security set-key-partition-list` for CI keychains). Nothing touches the bundle after this step.
8. Verify: `codesign --verify --strict --deep -vv`, and `codesign -d --entitlements - --xml` compared with the expected entitlements.
9. IPA:

   ```
   ditto <App.app> gen/ios/ipa/Payload/<Name>.app
   (cd gen/ios/ipa && /usr/bin/zip -qry -X <dist>/ios/<Name>.ipa Payload)
   ```

   `-X` writes no AppleDouble `._*` entries, which a plain `ditto -c -k` would include because every build output carries `com.apple.provenance` [V judges 2 and 3]. Gates:
   - `ios.ipa.layout`: `zipinfo -1` shows only `Payload/<Name>.app/…`, with no `__MACOSX/` and no `._` entries
   - `ios.ipa.signature`: unzip to a temp dir, then `codesign --verify --strict --deep` on the extracted app, which catches unsealed files
10. Store gates (§12.2), then the outputs: `<Name>.ipa`, `<Name>.app.dSYM.zip`, copies of `Info.plist` and `PrivacyInfo.xcprivacy`, `artifacts.json`, `UPLOAD.md`, `upload.sh`.

**Fallback** `--via-xcode-export` [I; used only if the phase 2 owner spike shows App Store Connect refuses the hand-built IPA]:
- Write `gen/ios/<Name>.xcarchive/{Info.plist (ArchiveVersion 2, ApplicationProperties), Products/Applications/<Name>.app, dSYMs/}`.
- Run `xcodebuild -exportArchive -archivePath … -exportOptionsPlist gen/ios/ExportOptions.plist -exportPath <dist>/ios`, with `method=app-store-connect`, `signingStyle=manual`, `teamID`, and `provisioningProfiles={<id>: <profile name>}`.

This still needs no Xcode project.

**Owner commands (`UPLOAD.md`)**
```sh
# once (web): create the App ID with its capabilities, the App Store profile, and the App Store Connect app
#   record (the API refuses POST /v1/apps [S]); put its numeric id in icm.toml [ios] asc_app_id.
# once: API key at ~/.appstoreconnect/private_keys/AuthKey_$ASC_KEY_ID.p8 (an altool search path [V]).
# listing (web): screenshots per device class, description, privacy answers, age rating.
# If uses_non_exempt_encryption = true and no export_compliance_code: upload the export documentation in ASC.
D=target/icm/dist/1.0.0+12/ios
xcrun altool --validate-app "$D/Notes.ipa" --api-key "$ASC_KEY_ID" --api-issuer "$ASC_ISSUER_ID" --output-format json | tee "$D/validate.json"
icm diagnose altool "$D/validate.json"
xcrun altool --upload-package "$D/Notes.ipa" --api-key "$ASC_KEY_ID" --api-issuer "$ASC_ISSUER_ID" --wait --output-format json | tee "$D/upload.json"
icm diagnose altool "$D/upload.json"        # never trusts the exit code [S A11]; prints the delivery id
xcrun altool --build-status --apple-id 1234567890 --bundle-version 12 --bundle-short-version-string 1.0.0 --platform ios \
  --api-key "$ASC_KEY_ID" --api-issuer "$ASC_ISSUER_ID" --wait --output-format json   # or --delivery-id <id> [V]
icm ledger mark-uploaded ios --build 12
```
altool 27.0.5 has no `--apple-id` for `--upload-package` [V]. `--build-status --apple-id` confirms the build landed in the right app, which covers the prefix-collision bug [S A10]. The documented alternatives are Transporter.app and the third-party `asc` CLI (`ASC_TELEMETRY_DISABLED=1 asc builds upload --app <id> --ipa …`).

### 11.2 Android → Google Play (phase 3)
**Preconditions:**
- `target_sdk` ≥ 36.
- `build` > the ledger max.
- bundletool ≥ 1.18 (tool cache, sha256) and JDK ≥ 17 [V 21].
- With `--sign auto`, `[android.signing] upload` is configured, the keystore exists, and both password env vars are set. Otherwise exit 9 `android.keystore.*`. An unsigned AAB is still produced, and the jarsigner line is printed for the owner.

**Pipeline**
1. For each ABI in `abis`, run §10.4 steps 3–5 with `--release --locked`. Keep the unstripped `.so` files in `native-debug-symbols.zip` (`<abi>/lib<lib>.so`), then `llvm-strip --strip-unneeded`.
2. Proto link:

   ```
   aapt2 link --proto-format --output-to-dir -o gen/android/proto -I android.jar --manifest gen/android/AndroidManifest.xml -R … --auto-add-overlay --replace-version --version-code <build> --version-name <version>
   ```

   No `--debug-mode`.
3. `base.zip` (own writer, deterministic):
   - `manifest/AndroidManifest.xml` (aapt2 puts it at the root [V]; icm moves it)
   - `resources.pb`
   - `res/**`
   - `lib/<abi>/lib<lib>.so`
   - `assets/**`

   There is **no `dex/`**, consistent with `hasCode=false`.
4. `gen/android/BundleConfig.json`:

   ```json
   {"optimizations":{"uncompressNativeLibraries":{"enabled":true,"alignment":"PAGE_ALIGNMENT_16K"}}}
   ```

   bundletool's default is 4K [V config.proto], so this setting is mandatory.
5. `java -jar <cache>/bundletool-all-1.18.3.jar build-bundle --modules=gen/android/base.zip --config=gen/android/BundleConfig.json --output=gen/android/app-unsigned.aab`, then `… validate --bundle=gen/android/app-unsigned.aab`.
6. Sign:
   - First run `keytool -J-Duser.language=en -list -v -keystore <ks> -alias <alias> -storepass:env <store_pass_env>`. It yields the key algorithm, which picks `-sigalg SHA256withRSA` or `SHA256withECDSA`, and the certificate SHA-256.
   - Then:

     ```
     jarsigner -J-Duser.language=en -keystore <ks> -storepass:env <store_pass_env> -keypass:env <key_pass_env> -sigalg <alg> -digestalg SHA-256 -signedjar <dist>/android/<name>-<version>-<build>.aab gen/android/app-unsigned.aab <alias>
     ```

   - `key_pass_env` defaults to `store_pass_env`, and stdin is closed, so jarsigner never prompts [V judge 1]. `apksigner` cannot sign AABs [S].
7. **Verify the signature (`android.aab.signed`).**
   - `jarsigner -J-Duser.language=en -verify -verbose -certs <aab>` **without `-strict`**, which exits 4 on every self-signed upload key [V judges 1 and 2]. The output must contain `jar verified.` and must not contain `jar is unsigned`, because an unsigned jar exits 0 [V].
   - `keytool -printcert -jarfile <aab>` SHA-256 must equal the alias certificate's.
   - A missing timestamp is INFO.
8. **Gates on the linked artifact, never on config:**
   - `bundletool dump manifest --bundle=<aab>`: targetSdk, versionCode, versionName, no `debuggable`, `hasCode=false`, the full configChanges list, lib_name
   - `bundletool dump config --bundle=<aab>`: `PAGE_ALIGNMENT_16K`
   - ELF gates on every `.so` extracted from the bundle
9. Smoke test (default when a device or emulator is available; `--no-smoke` skips it):
   - `bundletool build-apks --bundle=<aab> --output=gen/android/app.apks --connected-device --device-id=<S> --ks=~/.android/debug.keystore --ks-pass=pass:android --ks-key-alias=androiddebugkey --key-pass=pass:android`
   - `bundletool install-apks --apks=gen/android/app.apks --device-id=<S>`
   - then launch, ready, screenshot and gates as in §10.4

   Smoke APKs use the same debug key as dev installs, so there is no signature clash. `icm run android --from-aab` runs the same flow.
10. `--apk` builds a universal APK:

    ```
    bundletool build-apks --mode=universal …
    apksigner sign --ks <ks> --ks-key-alias <alias> --ks-pass env:<store_pass_env> --key-pass env:<key_pass_env>
    ```

    It is for sideloading; `adb` installs are exempt from the 2026 developer-verification rules [S G7].
11. Outputs: `<name>-<version>-<build>.aab`, `native-debug-symbols.zip`, `play-icon-512.png`, `artifacts.json`, `UPLOAD.md`, `upload.sh`.

**Owner commands**
```sh
# once: upload key (prompts for passwords; keep them in your password manager)
keytool -genkeypair -v -keystore ~/.icm/keys/app-upload.jks -alias upload -keyalg RSA -keysize 4096 -validity 9125 -storetype PKCS12
# FIRST release only (Play Console web UI): create the app; upload the AAB to Internal testing by hand; store listing
#   (512 px icon = play-icon-512.png, 1024×500 feature graphic, ≥2 phone screenshots), content rating, data safety
#   (must match the manifest permissions), target audience. Personal accounts created after 2023-11-13 need a closed
#   test with ≥12 testers for 14 days before production [S]. Upload native-debug-symbols.zip in App bundle explorer.
# later releases:
fastlane supply --aab target/icm/dist/1.0.0+12/android/app-1.0.0-12.aab --package_name com.example.app \
  --track internal --release_status draft --json_key "$PLAY_SERVICE_ACCOUNT_JSON"
icm ledger mark-uploaded android --build 12
```
Before any android entry exists in the ledger, icm prints only the first-release checklist. The alternative to fastlane is the Play Developer API edits flow (`edits.insert` → `edits.bundles.upload` → `edits.tracks.update` → `edits.commit`) with `curl`. The token step is [I] (`gcloud` scope support is unverified), and phase 3 settles it.

### 11.3 Web → static hosting (phase 4)
1. `cargo build --profile web-release --locked -p <pkg> --bin <bin> --target wasm32-unknown-unknown`, then `wasm-bindgen … --target web --no-typescript --out-name app`.
2. `wasm-opt -Oz <features> app_bg.wasm -o app_bg.wasm`, using binaryen `version_133` from the tool cache [V latest].
   - `<features>` comes from `rustc --print cfg --target wasm32-unknown-unknown` for the project's toolchain: each `target_feature="x"` maps to `--enable-x` (bulk-memory, mutable-globals, nontrapping-float-to-int, sign-ext, reference-types, multivalue). This mapping is [I], and phase 4 tests it.
3. Content-hash the names (`app-<h8>.js`, `app_bg-<h8>.wasm`) and write the release `index.html` with the explicit `module_or_path` (§9.5), `_headers`, `404.html`, `.nojekyll`, `manifest.webmanifest` and the icons. Pack `site/` and the deterministic `site.zip`.
4. Gates:
   - `web.size_budget`: gzip size of the wasm, via flate2
   - `web.fonts_embedded`: `fira-sans` resolved for wasm32, or a font in `resources`; wasm has no system fonts [V]
   - `web.renderer_fallback`: WARN without `webgl`
   - `web.hashed_assets`
   - `web.mime`
   - **`web.serve_smoke`**: serve `dist/…/site` with the dev server code, fetch the `.wasm` and check its MIME type, load `?icm_events=1` in headless Chrome, wait for `ICM_EVENT ready`, check the screenshot is not blank and the console has no errors
5. Owner commands by `[web] host`:

   | Host | Command |
   |---|---|
   | Cloudflare Pages | `npx wrangler pages deploy <site> --project-name <project>` |
   | Netlify | `npx netlify deploy --dir <site> --prod` |
   | S3 | `aws s3 sync <site> s3://<project>/ --delete`, plus a `--content-type application/wasm` copy for `*.wasm` |
   | GitHub Pages | the `icm ci init --targets web` workflow |
   | generic | `rsync -av --delete <site>/ <host>:<dir>/` |

   After deploying, the owner runs `icm verify web --url <deployed url>`, which runs the same headless check against the real host.

### 11.4 macOS → .app and .dmg (phase 5)
**Precondition:** a Developer ID Application identity. None exists on this host [V], so this exits 9 `macos.sign.no_developer_id` with the portal steps. `--sign none` builds an unsigned app and DMG for local testing.

Because the owner runs notarization, the flow has **two stages**. That way the app is stapled before it goes into the DMG, and Gatekeeper works offline.

**Stage 1: `icm release macos`**
1. Build:

   ```
   MACOSX_DEPLOYMENT_TARGET=<min_os> CARGO_PROFILE_RELEASE_DEBUG=line-tables-only cargo build --release --locked --target aarch64-apple-darwin
   ```

   With `universal`, also build `x86_64-apple-darwin` (`doctor --fix --yes` adds the target) and `lipo -create`. Gates: `lipo -archs`, and `macos.min_os` (Mach-O minos = `min_os`). Make the dSYM.
2. Assemble `<Name>.app/Contents/{Info.plist, MacOS/<bin>, Resources/AppIcon.icns, Resources/<resources>}`. The `.icns` comes from `iconutil -c icns gen/macos/AppIcon.iconset`.
3. Sign:

   ```
   xattr -cr <App.app>
   codesign --force --options runtime --timestamp --entitlements gen/macos/entitlements.plist --sign <Developer ID SHA-1> <App.app>
   ```

   Gates: `codesign --verify --strict --deep -vv`, and `macos.hardened_runtime` (`codesign -d -vv` shows `flags=0x10000(runtime)`).
4. Zip for notarization: `ditto -c -k --keepParent --norsrc --noextattr --noqtn --noacl <App.app> <dist>/macos/<Name>-<version>.app.zip`.
5. `UPLOAD.md`, stage 1 (owner):

   ```sh
   xcrun notarytool store-credentials icm-notary --key ~/.appstoreconnect/private_keys/AuthKey_$ASC_KEY_ID.p8 --key-id "$ASC_KEY_ID" --issuer "$ASC_ISSUER_ID"   # once
   xcrun notarytool submit <dist>/macos/<Name>-<version>.app.zip --keychain-profile icm-notary --wait --timeout 30m --output-format json | tee <dist>/macos/notary-app.json
   icm diagnose notarytool <dist>/macos/notary-app.json
   xcrun stapler staple <dist>/macos/<Name>.app
   icm release macos --dmg
   ```

**Stage 2: `icm release macos --dmg`**
1. Gate `macos.not_stapled`: `xcrun stapler validate <App.app>` must pass, else exit 9 with the stage 1 commands.
2. Stage the app plus an `Applications` symlink, then `hdiutil create -volname <Name> -srcfolder gen/macos/dmg -fs HFS+ -format UDZO -ov <dist>/macos/<Name>-<version>.dmg`. This is first-party; no create-dmg download.
3. `codesign --force --timestamp --sign <SHA-1> <dmg>`.
4. `UPLOAD.md`, stage 2: `notarytool submit <dmg> … --wait`, then `xcrun stapler staple <dmg>`, then `icm verify macos --after-notarize`. That runs:
   - `spctl -a -vvv -t exec <App.app>`
   - `spctl -a -t open --context context:primary-signature -vv <dmg>`
   - `xcrun stapler validate` on both

   Distribution then goes through GitHub Releases or the owner's site.

### 11.5 Windows → .msi and NSIS .exe (phase 5, Windows runner)
1. `rc.exe /nologo /fo gen/windows/app.res gen/windows/app.rc`. `rc.exe` is located under `Windows Kits\10\bin\<newest>\x64\`; if it is missing, exit 4 `windows.sdk_missing`.
2. `cargo rustc --release --locked -p <pkg> --bin <bin> --target x86_64-pc-windows-msvc -- -C link-arg=<abs>/gen/windows/app.res`. MSVC `link.exe` accepts `.res` inputs, which is the mechanism the winres crates use. CI verifies it.
3. `windows.msi_version`: Cargo version X.Y.Z must satisfy X ≤ 255, Y ≤ 255, Z ≤ 65535.
   - The MSI ProductVersion is X.Y.Z, because Windows Installer ignores a 4th field [V judge 2], and `MajorUpgrade AllowSameVersionUpgrades="yes"` makes a rebuild with the same version upgrade.
   - The build number goes into VERSIONINFO only.
4. Sign the exe with `sign_command`, `{file}` substituted; credentials come only from env (for example jsign ≥ 7 against Azure Artifact Signing [S D6]). With no command: exit 9 `windows.sign.not_configured` under `--sign auto`.
5. `wix build -arch x64 -o <dist>/windows/<Name>-<version>.msi gen/windows/app.wxs`. WiX v5 is installed with `dotnet tool install --global wix --version 5.0.2` [I version], pinned in the workflow `ci init` writes.
6. `makensis -V2 gen/windows/installer.nsi` → `<Name>-<version>-setup.exe`.
7. Sign both installers. Gate `windows.signed`: `signtool verify /pa /v` on all three files.

### 11.6 Linux → .deb and AppImage (phase 5, Linux runner)
1. The build runs in a container `ubuntu:22.04` (glibc 2.35), pinned by digest in the `ci init` workflow: `cargo build --release --locked -p <pkg> --bin <bin>`.
   - Gate `linux.glibc_floor`: the highest `GLIBC_x.y` in `.gnu.version_r` (read via `object`) ≤ `[desktop.linux] glibc_floor`. A local build on a newer host fails this gate unless the floor is raised; the fix points to the workflow.
2. `.deb`:
   - Lay out `gen/linux/deb/{DEBIAN/control, usr/bin/<bin>, usr/share/applications/<id>.desktop, usr/share/icons/hicolor/*/apps/<id>.png}`.
   - Depends = `dpkg-shlibdeps -O` output (run against a stub `debian/control`) + `deb_depends`.
   - Recommends = `libxkbcommon0, libxkbcommon-x11-0, libwayland-client0, libvulkan1, libegl1` (dlopened by winit and wgpu, so `ldd` cannot see them; wgpu falls back from Vulkan to GLES through EGL and has no GLX path, so `libgl1` is no fallback) + `deb_recommends`.
   - `[desktop.linux] maintainer` is required.
   - `dpkg-deb --build --root-owner-group gen/linux/deb <dist>/linux/<pkg>_<version>-<build>_amd64.deb`.
   - Gates: `dpkg-deb --info`; `desktop-file-validate` (`linux.desktop_file`) if installed; `lintian` as WARN only.
3. AppImage:
   - Lay out `gen/linux/AppDir/{AppRun, <id>.desktop, <id>.png, usr/bin/<bin>, usr/lib/}`.
   - `usr/lib` bundles `libxkbcommon.so.0`, `libxkbcommon-x11.so.0` and `libwayland-cursor.so.0`, copied from the same 22.04 image, so they match the glibc floor. GPU drivers and `libwayland-client.so.0` come from the host: the AppImage excludelist forbids bundling the latter, since a newer Mesa needs symbols an older copy lacks. `AppRun` sets `LD_LIBRARY_PATH`.
   - `appimagetool --appimage-extract-and-run gen/linux/AppDir <dist>/linux/<Name>-<version>-x86_64.AppImage`, with `appimagetool` pinned in `tools.toml`, sha256-checked.
   - Smoke: run under Xvfb with `ICM_EVENTS=1` and wait for `ICM_EVENT ready`.

---

## 12. Gates (store preflight; `release` runs them, `verify` runs them on any artifact)

### 12.0 Policy table (`cli/policy/stores.toml`, embedded, dated)

| Rule | Value | Effective | Source |
|---|---|---|---|
| App Store minimum Xcode/SDK | 26 | 2026-04-28 | [S A1] |
| App Store minimum deployment | iOS 13 | 2026-09-09 | [S A1] |
| Privacy manifest required reasons | on | 2024-05-01 | [S A3] |
| Play targetSdk | 36 | 2026-08-31 (extension to 2026-11-01) | [S G1] |
| Play 16 KB page size | **FAIL now** | 2025-11-01 (extension to 2026-05-31) per judge 2's sources. The brief's "2027-02-01" conflicts with this; the gate is unconditional either way | judge 2 |
| Android back-key opt-out | WARN | removed at API 37 | [S G4] |
| configChanges complete list | §9.4 | API 34+ | review §6.6 |

`doctor` warns `env.policy_stale` when the table is more than 90 days old, and lists floors that take effect within the next 60 days.

### 12.1 Config and dependency checks (every `check` and build)
- `config.*`
- `deps.*`: from `Cargo.lock` and `cargo metadata --filter-platform <triple>`, covering review traps 1–3, 6 and 11
- `review.snapshot_stale`

The lockfile fixtures come from the agent-usability probes `gitdep`, `mixdep`, `patchdep`, `nofeat` and `nodef` [V present], and each must produce its expected `deps.*` FAIL.

### 12.2 iOS

| Id | Rule | Prevents |
|---|---|---|
| `ios.macho.platform` / `.minos` / `.sdk_floor` / `.arch` / `.sdk_matches_dt` | platform 2; minos = MinimumOSVersion ≥ 13; sdk ≥ 26; arm64 only; sdk = DTSDKName | the SDK floor; "beta Xcode" treatment |
| `ios.xcode.not_beta` | no beta Xcode or SDK | rejection of beta builds |
| `ios.plist.required_keys` / `.lint` | managed keys present and well-typed | various ITMS errors |
| `ios.plist.scene_manifest` | UISceneConfigurations present | kill at launch with the iOS 27 SDK [S A2] |
| `ios.plist.dt_keys` | all DT* keys present and equal to this Xcode | [S A5] |
| `ios.icon.opaque_1024` | `CFBundleIconName` + `Assets.car`; `assetutil` shows AppIcon 1024 with `Opaque: true` | ITMS-90713, 90717 |
| `ios.plist.ipad_orientations` | UIDeviceFamily contains 2 ⇒ all four orientations (for externally built artifacts) | ITMS-90474 |
| `ios.plist.export_compliance` | ITSAppUsesNonExemptEncryption set; WARN `ios.export_compliance.documentation` when true and no code | prompt on every upload |
| `ios.plist.usage_descriptions` | an NS*UsageDescription for every mapped permission and for linked sensitive classes (`AVCaptureDevice`, `LAContext`, …) | ITMS-90683 |
| `ios.privacy.present` / `.reasons` | xcprivacy at the bundle root; detected categories ⊆ declared | ITMS-91053 |
| `ios.sign.verify` | `codesign --verify --strict --deep`; identity is Apple Distribution | invalid signature |
| `ios.sign.profile_*` | App Store type; expiry (FAIL < 7 days, WARN < 30); app id, team and certificate match | upload rejection |
| `ios.entitlements.not_in_profile` / `.get_task_allow` | signed ⊆ profile; get-task-allow false | upload rejection |
| `ios.version.format` / `version.build_not_increased` | X[.Y[.Z]]; build > ledger | redundant-binary rejection |
| `ios.ipa.layout` / `.signature` | `Payload/<Name>.app` only, no `._*`; extracted app verifies | unsealed contents |
| `store.no_agent_bridge` | marker `ICM_AGENT_BRIDGE_V1` absent from the binary | shipping debug tooling |
| `app.id.placeholder` / `app.icon.placeholder` | not the template's | shipping the placeholder |

### 12.3 Android

| Id | Rule |
|---|---|
| `android.aab.validate` | `bundletool validate` |
| `android.aab.signed` | §11.2 step 7, which parses output and does not use `-strict` |
| `android.manifest.target_sdk` | ≥ 36, read from `dump manifest` |
| `android.manifest.config_changes` | the full list |
| `android.manifest.has_code` | `hasCode=false` ⇔ no `dex/` |
| `android.manifest.debuggable` | absent |
| `android.manifest.lib_name` | equals the `.so` name in every ABI |
| `android.manifest.version` | versionCode = build ≤ 2.1e9 and > ledger; versionName = Cargo version |
| `android.so.export` / `.align16k` / `.abis` | per ABI; arm64-v8a present (WARN without x86_64) |
| `android.bundle.alignment` | `dump config` shows `PAGE_ALIGNMENT_16K` |
| `android.manifest.back_optout` | WARN while `back = "key"` |
| `android.permissions.review` | WARN listing dangerous permissions (the data safety form must match) |
| `store.no_agent_bridge` | marker absent from every `.so` |

### 12.4 Web, macOS, Windows, Linux
- **Web:** §11.3 step 4.
- **macOS:** `macos.sign.verify`, `.hardened_runtime`, `.min_os`, `.not_stapled`, and `.gatekeeper` (after notarization).
- **Windows:** `windows.msi_version`, `.signed`.
- **Linux:** `linux.glibc_floor`, `.desktop_file`, `.deb.lint`.

---

## 13. Seeing, acting, testing

### 13.1 Screenshots

| Platform | Phase 1 | Phase 6 |
|---|---|---|
| ios-sim | `simctl io <udid> screenshot` | + bridge |
| ios-device | `devicectl device capture screenshot` [V] (phase 2) | unchanged (no bridge on device in v1) |
| android | `adb exec-out screencap -p`; FLAG_SECURE gives INFO | + bridge |
| web | CDP `Page.captureScreenshot` from the session | + bridge |
| desktop | macOS `screencapture -l` after the permission preflight, else WARN plus a headless render; Linux X11 `import`; Wayland and Windows use a headless render | bridge `window::screenshot` on every OS |
| headless | `icm shot --headless` through the app's harness: real view, real fonts, tiny-skia, deterministic | unchanged |

Every capture writes `screen.png` and `screen.preview.png` (long edge ≤ 1024 px). Blank detection: ≥ 99.5 % of pixels within a small distance of one colour gives `run.screen_blank` (WARN; FAIL with `--expect-content`).

Viewport presets:

| Preset | Size |
|---|---|
| `iphone-17` | 402×874 @3 [V] |
| `iphone-se` | 375×667 @2 |
| `pixel-9` | 412×915 @2.625 |
| `web-mobile` | 390×844 @3 |
| `desktop` | 1024×768 @1 |

### 13.2 Headless harness (framework F4; phase 1)
`iced_test::agent::main(program, manifest_dir) -> ExitCode` is the body of the template's `tests/icm.rs` (`harness = false`).
- With no arguments, which is plain `cargo test`, it runs `<manifest_dir>/tests/flows/*.ice` through `iced_test::run`.
- `icm-shot --viewport WxH --scale F --theme light|dark [--preset P] --wait-ms N --out PATH` calls `iced_test::screenshot`.
- `icm-tree --viewport WxH --out PATH` writes the selector candidates (kind, id, text, bounds) as JSON.
- `icm-ice <file> --report PATH` writes a result for each instruction.
- The harness prints `ICM_HARNESS {"protocol":1}` first.

As built (`test/src/agent.rs`, whose module docs are the reference):
- `--viewport` also takes the presets of §13.1 (`iphone-17`, the default, `iphone-se`, `pixel-9`, `web-mobile`, `desktop`); `--scale` defaults to the preset's. `icm-tree` also takes `--preset` and `--wait-ms`, and each widget has `visible` (its on-screen rectangle, or null when scrolled or clipped away) and, for text inputs and focusables, `focused`. `icm-ice` also takes `--timeout-ms` (default 30 s); a failed step carries `reason` and the visible `texts`.
- Every command ends with `ICM_HARNESS_RESULT <json>`: `{"protocol":1,"kind":"shot|tree|ice|flows","ok":…}` plus the kind's fields. The `--report`/`--out` files hold the same object.
- Exit codes: 0 everything passed, 1 a flow failed, 2 usage error or a file that cannot be read or written.
- With no command, libtest's `--list`, `--ignored`, `--skip` and name filters apply to the flow names `flows::<stem>`, so nextest can list them.
- It draws with tiny-skia unless `ICED_TEST_BACKEND` names another backend (`Emulator::with_backend`, `iced_test::run_with_backend` and `screenshot_with_backend`, Appendix C 7), and `Font::DEFAULT` is Fira Sans.
- `iced::clipboard::read` and `write` tasks use the `Emulator`'s clipboard, the one its text fields copy to and paste from, so a flow can copy and paste (in `mode: Zen`, which waits for the read's message). It starts empty for each flow.
- It stands in for the shell's safe area (F6): before the program boots, a viewport the size of a §13.1 preset gets that device's insets through `iced::mobile::safe_area()` (`iphone-17` 62 top and 34 bottom, as the iPhone 17 simulator reports; `iphone-se` 20 top; `pixel-9` 54.1 top and 24 bottom, icm's `pixel_9` emulator at API 36; zero at `web-mobile` and `desktop`). `.ice` flows get it from their `viewport:` line. Any other size gets none.

icm runs it as `ICED_TEST_BACKEND=tiny-skia cargo test -p <pkg> --test icm -- <subcommand> …`. That powers `icm shot --headless`, `icm ui --headless tree|find|ice` and `icm test --host`. It needs no device, no OS permission and no GPU, and it runs in CI.

### 13.3 Readiness and logs
**`ICM_EVENT` protocol v1 (framework F3, `iced_winit::icm`).** Events are emitted only when the run opts in (Appendix C 27; `1` and `true` count): `ICM_EVENTS=1` env (desktop, ios-sim through `SIMCTL_CHILD_`), sysprop `debug.icm.events=1` (Android), or `?icm_events=1` (web). Each event is one line:

| Platform | Where the line goes |
|---|---|
| desktop, iOS | stderr: `ICM_EVENT <json>` |
| Android | `__android_log_write(INFO, "ICM_EVENT", json)`; android-log-sys is already in the graph, and this gives exact filtering with `-s ICM_EVENT:I` |
| web | `console.log("ICM_EVENT " + json)` |

Each JSON object starts with `"v":1,"kind":"<kind>"`, for example `ICM_EVENT {"v":1,"kind":"ready","ms":812,"window":{"size":[402,874],"scale":3.0},"backend":"wgpu"}`.

| Kind | Fields |
|---|---|
| `start` | `protocol`, `framework`, `pid`, `platform`, `bridge` |
| `theme` | `mode` (`light`, `dark`, `none`): Android and iOS, once at start and on every switch |
| `ready` | after the first presented frame: `ms`, `window{size,scale}`, `backend` |
| `lifecycle` | `state` |
| `app_state` | `state` (`foreground`, `active`, `inactive`, `background`, `memory_warning`), what `iced::mobile::lifecycle()` delivers to `update` |
| `panic` | `message`, `location`, `thread` |
| `warning` | `code` (e.g. `font.default_missing`, `compositor.fallback`) |
| `safe_area` | `insets` (`[top, right, bottom, left]`), `keyboard`, in logical pixels |
| `exit` | `code` |

As built: `ready` also carries `window.physical`, `adapter` and `api` (`Metal`, `Vulkan`, `BrowserWebGpu` or `tiny-skia`), for example `{"v":1,"kind":"ready","ms":652,"window":{"size":[1024,768],"physical":[2048,1536],"scale":2},"backend":"wgpu","adapter":"Apple M4 Max","api":"Metal"}`. `start` has `pid: null` on the web and `bridge: null` until phase 6. The shell sends `warning` `font.default_missing` once its compositor exists, when the default font is a named family that no face in the font system has (not embedded, not in `Settings::fonts`, not a system font; `iced_graphics::text::is_loaded`); `compositor.fallback` is not sent yet (the `ready` event's `backend` shows which renderer runs). `exit` is never sent on iOS or the web, where winit's run does not return. On Android, where the process can outlive the shell, `exit` also carries `destroyed` (`true` when Android destroyed the Activity), and each new Activity of the process starts the shell again: its own `start` (same `pid`) and `ready`. A panic inside winit's callbacks on macOS cascades into more panics and an abort (134), so several `panic` events can arrive: report the first. On the web, the first `ready` gives the canvas attribute size, which can differ from the page's. On Android, the sysprop `debug.iced.backend` chooses the renderer when `ICED_BACKEND` is unset. The shell sends `safe_area` when its first window opens (zeros on the desktop and the web) and then whenever the safe area changes on Android and iOS (rotation, a display cutout, the keyboard showing or hiding), with the values `iced::mobile::safe_area()` delivers, for example `{"v":1,"kind":"safe_area","insets":[62,0,34,0],"keyboard":0}` on an iPhone 17.

If no event arrives, icm uses platform probes and reports `ready.source = "probe"`. This covers today's Tawara pin and release builds that have not opted in. If no ready signal arrives while the app is alive, the result is `run.not_ready` (10).

**Logs.**
- `runs/<id>/logs.ndjson` records have the shape `{ts, platform, source: app|stdout|stderr|oslog|logcat|console|crash|system, level, tag, pid, msg}`, and `app.log` is the readable merge.
- `icm logs <platform>` **re-queries live sources** from the launch mark. It never reads a snapshot from the end of `run`:

  | Platform | Source |
  |---|---|
  | ios-sim | `log show --start <mark>`, plus the app's stdout and stderr files, which the app keeps writing |
  | android | `logcat -d -T <epoch>` |
  | web | the session's `console.ndjson` |
  | desktop | the app's stdout and stderr files |

- Filters are `--level`, `--source`, `--grep` and `--tail`. `--raw` prints the original files.

### 13.4 Failure signatures (`signatures.rs`; matched on failure; reported as `likely_causes`)

| Signature (abbreviated) | Cause and fix |
|---|---|
| FrontBoard/RunningBoard `failed to launch` + scene | `ios.plist.scene_manifest` |
| `Call set_android_app` / `android_main` ran twice / `RecreationAttempt` | duplicate iced copies (`deps.single_iced`), or Activity recreation on an iced_mobile from before the Android lifecycle fix, whose winit allows one event loop per process (update the pin; until then the full configChanges list and `[android] back = "key"`); never `iced::exit` |
| `an event loop is already running in this process` | a second Activity started while another still ran the app (a launch right after Back, or another task; `singleTask` prevents the latter) |
| `No Unix display server backend` | framework floor not met |
| `dlopen failed: library "lib…so" not found` | `android.manifest.lib_name` |
| `INSTALL_FAILED_UPDATE_INCOMPATIBLE` / `INSTALL_FAILED_NO_MATCHING_ABIS` | signature changed (`--reinstall --yes`); ABI not built |
| `ANR in <id>` / `am_destroy_activity` / `wm_relaunch_resume_activity` | blocked main thread or recreation (`run.activity_recreated`) |
| `panicked at <file>:<line>` | panic at that location (`run.app_panicked`) |
| `Failed to find an appropriate adapter` / surface creation errors | GPU: retry with `--env ICED_BACKEND=tiny-skia` (Android: sysprop `debug.iced.backend`, framework F3) |
| `CODESIGNING` termination / missing provisioning profile | exit 9 signing |
| `Incorrect response MIME type` / wasm-bindgen schema mismatch | server MIME; `deps.wasm_bindgen_cli` |
| Blank screen + alive + no panic | Android with `FLAG_SECURE` → INFO `android.screen.secure`; otherwise fonts or theme: compare with `icm shot --headless` |

### 13.5 Acting
- **Phase 1 input:**
  - android uses `adb shell input tap|swipe|text|keyevent`
  - web uses CDP `Input.dispatchMouseEvent`/`dispatchTouchEvent`/`dispatchKeyEvent`
  - ios-sim uses AXe when it is installed (`brew install cameroncooke/axe/axe`, an agent or owner step; exact tap flags [I])
- **Device-state helpers** (`icm input <p> appearance|rotate|font-scale|background|foreground`):
  - android: `cmd uimode night yes|no`, `settings put system user_rotation`, `settings put system font_scale`, HOME then `am start`
  - ios-sim: `simctl ui <udid> appearance`; background by launching `com.apple.Preferences`; foreground by relaunching the app without terminating it
- **`icm test --on <platform> --lifecycle`** (phase 3) is a built-in suite. Each step is a CHECK, and the suite FAILs on `am_destroy_activity`, `ANR in`, a changed pid or a lost first frame:
  - **android:** uiMode night/day, rotation (`accelerometer_rotation 0` then `user_rotation 1/0`), `font_scale 1.3`, `font_weight_adjustment 300`, Home → relaunch with the same pid, Back with `back = "key"` (stays alive)
  - **ios-sim:** appearance dark/light, background → foreground with the same pid

### 13.6 Hooks (how Tawara keeps its checks)
`[checks] <platform> = ["script", …]` run after a successful `run` and after each `test --on` step. The env provides:
- `ICM_PLATFORM`, `ICM_RUN_DIR`, `ICM_PID`
- `ICM_DEVICE` (UDID or serial), `ICM_ADB` (`adb -s <serial>`)
- `ICM_APP_ID`, `ICM_BIN`
- `ICM_APP_STDERR`, `ICM_LOGS`, `ICM_LOG_MARK`
- `ICM_SIM_DATA` (`simctl get_app_container <udid> <id> data`)

Hooks run with stdin closed and a 300 s timeout. Output lines of the form `CHECK PASS|FAIL <name>: …` become `hook.<name>`. A non-zero exit is FAIL `hook.<script>`.

### 13.7 Agent bridge (phase 6; framework F7 + `icm ui`)
- **Gating.** The `iced/agent` feature (template feature `icm-agent`) compiles to nothing without `debug_assertions`. `icm run` adds the feature only to dev builds when `[app] agent = true`. `icm release` refuses builds with the feature enabled, and `store.no_agent_bridge` checks for the marker.
- **Transport: the app always connects out to the session host.**

  | Platform | How the app reaches the host |
  |---|---|
  | desktop | `ICM_AGENT=tcp://127.0.0.1:<port>` and `ICM_AGENT_TOKEN` env |
  | ios-sim | the same, through `SIMCTL_CHILD_` (shared loopback) |
  | android | `adb reverse tcp:<port> tcp:<port>`. Address and token are written to the app's private `files/icm-agent.json` with `adb shell run-as <id> sh -c 'cat > files/icm-agent.json'` before launch (debuggable builds only), never through world-readable sysprops |
  | web | `globalThis.__ICM_AGENT__ = {url, token}`, injected by the dev server; HTTP POST plus SSE, no WebSocket |

  Physical iOS devices get **no bridge in v1**. Their runs use the devicectl console and screenshots.
- **Protocol:** JSON lines. The app sends `{"op":"hello","protocol":1,"token":…,"app_id":…,"windows":[…]}`. The host can call:
  - `ui.tree`, `ui.find {selector}`, through `iced_selector` as a widget operation
  - `ui.tap`, `ui.swipe`, `ui.type`, `ui.key`, which inject `Touch` events on mobile and mouse events on desktop and web through a ~50-line debug-only queue in iced_winit
  - `ui.ice`
  - `window.screenshot`
  - `events.subscribe {message}`, which streams the `Debug` form of each `Message` into `messages.ndjson`
- **What it gives agents:** `icm ui <p> tap "Increment"`, `icm ui <p> find "Count: 1"`, `icm test --on <p> --flows` (replays `.ice` on the device), and `icm ui <p> messages` (shows that a tap produced `Message::Increment`).

---

## 14. Framework contract (fork work icm relies on; the CLI checks capabilities, never links)

| # | Item | Status | Phase |
|---|---|---|---|
| F0 | winit ≥ 0.30.13, softbuffer ≥ 0.4.7, display-server exemption, Fira default on mobile, system-font fallback, keep-alive guards | **done locally** (`cb749c5a4`, `697d8d25d`, `4ba8c1edd`, `5802255ff`, `476dff628`); push as branch `mobile/0.14` and tag `v0.14.1-mobile.1`. Add `graphics/fonts/OFL.txt` (Fira Sans licence) and fix the `repository` metadata (review §6.3) | 0 |
| F1 | `iced::mobile` module, `android_main!`, NativeActivity default, `init_logger` | **done locally** (`33a00d56c`, `685e14db6`). Update the `src/mobile.rs` doc: `crate-type` is optional when building through icm | 0 |
| F2 | `init_logger` also installs a stderr logger on desktop and a console logger on wasm, plus a panic hook on every target (once); no new dependencies | to do (small) | 0 |
| F3 | `ICM_EVENT` protocol v1 (§13.3); Android reads sysprops `debug.icm.events` and `debug.iced.backend` | to do | 0 |
| F4 | `iced_test::agent::main` harness (§13.2) | to do (~200 lines on the public iced_test API) | 0 |
| F5 | `iced_test` touch helpers (`Simulator::tap` as touch, `.ice tap`); template adds `tests/touch.rs` | to do | 6 |
| F6 | `iced::mobile::safe_area` (fixed fallback insets first, then real ones); new types in `iced::mobile`, not new enum variants (review §6.8) | **done**: `iced::mobile::safe_area() -> Subscription<SafeArea>` (insets and keyboard on Android and iOS, zero elsewhere; headless, a device preset's insets at its viewport size, §13.2) and the `safe_area` event. The template keeps its fixed padding | 6 |
| F7 | `agent` feature: bridge client, synthetic-event queue, tree operation, marker string. Phase 0 adds an **empty** `agent = []` feature to iced so the template's `icm-agent` feature resolves from the first tag | to do | 0 (stub), 6 |
| F8 | Loud failure messages (RecreationAttempt text, iOS exit warning); partly done in `c6b2ebe16` | ongoing | — |

---

## 15. Tawara: fit and migration

**What already fits [V].**
- `crates/mobile` has `crate-type = ["lib"]` and is built with `cargo rustc --crate-type cdylib`, which is exactly icm's Android build.
- Package `tawara-mobile`, lib `tawara_mobile`, bin `tawara`.
- NativeActivity, `hasCode=false`, stored and aligned libraries, a toolchain pinned at 1.98.0.
- It pins iced and iced_winit at `71f00e8` and winit `=0.30.13` (native-activity).
  - **icm needs no framework bump.** It never links iced, and without `ICM_EVENT` it uses probes (`ready.source = "probe"`).
  - `deps.cli_framework_skew` WARNs. `deps.single_iced` and `deps.android_activity_backend` pass.
- `[app] agent = false` (a wallet): everything in phases 1–5 works without the bridge.

**Steps** (about 1–1.5 days, after phase 1; done first in a scratch clone, then in the repo by the owner):

1. `icm init --package tawara-mobile --adopt-ios platform/ios/Info.plist.in --adopt-android platform/android/AndroidManifest.xml --adopt-android-res platform/android/res --write` writes:
   ```toml
   schema = 1
   [app]
   name = "Tawara"
   id = "com.patricksmithlaravel.tawara"
   build = 1
   platforms = ["ios", "android"]            # desktop is packaged from Tawara-wallet with its own icm.toml
   package = "tawara-mobile"
   lib = "tawara_mobile"
   bin = "tawara"
   orientations = ["portrait", "landscape-left", "landscape-right"]
   # icon = "platform/icon.png"               # Tawara has no icon yet: WARN in dev, exit 9 in release
   agent = false
   [app.permissions]
   internet = true
   [ios]
   min_os = "16.0"
   # team_id, uses_non_exempt_encryption: owner decisions (a wallet does cryptography)
   [ios.entitlements]
   "com.apple.developer.default-data-protection" = "NSFileProtectionComplete"   # also enable it on the App ID
   [android]
   min_sdk = 26
   target_sdk = 36                            # tawara.sh links at 35; Play requires 36
   abis = ["arm64-v8a", "x86_64"]
   back = "key"
   allow_backup = false
   res = "platform/android/res"               # keeps res/xml/data_extraction_rules.xml
   [android.manifest]
   application = { "android:dataExtractionRules" = "@xml/data_extraction_rules" }
   [checks]                                   # added by hand in step 4, once the scripts exist
   # ios-sim = ["platform/ios/checks.sh"]     # TAWARA lines, xattr, 0700 mode, background lock
   # android = ["platform/android/checks.sh"] # FLAG_SECURE, no_backup mode, Home/Back
   ```
   Tawara's `android:windowSoftInputMode="adjustResize|stateHidden"` is not carried over: icm generates that value, and an overlay of a generated attribute is `config.managed_key` (§7.4).
2. **Review the reported diff.** CFBundleVersion stops being a literal `1` and comes from `build`. configChanges gains `mcc|mnc|grammaticalGender` (the one manifest fix Tawara needs). The output also gains icons, `IcmTheme` and `targetSdk 36`.
3. **Run on the emulator** to cover the targetSdk 36 behaviour (edge-to-edge, Back as a key):
   - `icm run ios-sim --json -q`
   - `icm run android --json -q`
   - `icm test --on android --lifecycle` (phase 3)
4. **Move the checks.** tawara.sh's app-specific assertions move into the hook scripts, which read `ICM_*`. Build, bundle, install and launch become `icm run`. Android screenshots come back black because of FLAG_SECURE, and icm reports INFO `android.screen.secure`. Run both pipelines side by side until the CHECK sets match, then delete `tawara.sh`, `Info.plist.in` and `AndroidManifest.xml`.
5. **When bumping to `v0.14.1-mobile.1`** (optional): drop the direct `iced_winit` and `winit` dependencies in favour of `iced::android_main!` and `iced::mobile`. Tawara's policy test keeps working, because icm only reads the lock.
6. **Owner prerequisites for phases 2 and 3:**
   - App ID with Data Protection
   - App Store profile
   - ASC record (`asc_app_id`)
   - the export-compliance answer
   - upload keystore
   - Play app plus the first manual AAB upload
   - Developer ID certificate for Tawara-wallet's desktop DMG, which gets its own `icm.toml` with `[desktop] package = "tawara-desktop"` and the same `[app]` identity, with a Tawara policy test comparing the two
   - Web is out of scope for Tawara: wallet-core is not wasm-ready.

---

## 16. Dependencies

### 16.1 CLI crates (versions are current on crates.io as of 2026-10-06 [V judges]; pinned through `cli/Cargo.lock`)

| Crate | Use |
|---|---|
| clap 4.6 (derive; no colour feature) | command surface, `--help` with examples, `print commands` |
| serde 1 / serde_json 1 | events, results, cargo/simctl/devicectl/bundletool JSON |
| toml 1.1 / toml_edit 0.25 | icm.toml and Cargo.lock with spans; format-preserving writes (`version`, `framework set`, `init`) |
| schemars 1.2 | JSON Schemas for config, output and artifacts |
| plist 1.10 | Info.plist, entitlements, xcprivacy, decoded profiles, actool partial, Xcode Info.plists |
| quick-xml 0.42 | raw-XML overlay validation |
| object 0.40 | Mach-O and ELF inspection on every host (replaces otool/nm/vtool/readelf) |
| png 0.18 / tiny-skia 0.11 | icon decode, flatten, resample, encode; previews; blank detection (tiny-skia is the same version iced uses) |
| flate2 1.1 / sha2 0.10 | deflate for site.zip, gzip size; hashes and stamps |
| regex 1 / semver 1 | signatures and log waits; `icm = ">=…"` |
| libc 0.2 | setsid, kill, flock, dup2 (CDP pipe) |
| objc2-core-graphics (macOS only, target-gated) | window id lookup; screen-capture preflight |
| dev: tempfile, insta, jsonschema | tests |

**Deliberately not used:**
- tokio or any async runtime
- reqwest/ureq/tiny_http/axum/tungstenite: icm uses its own std server and CDP over a pipe, and `curl` for pinned downloads
- the zip crate: icm has its own deterministic writer, validated by zipalign, apksigner and bundletool
- cargo_metadata
- image and resvg: PNG icon sources only in v1
- cargo-packager: its DMG step downloads create-dmg, it notarizes on its own when `APPLE_*` env vars are present, and it downloads WiX 3.11 (end of life), NSIS and linuxdeploy at package time [V judges 2 and 3]
- wasm-bindgen-cli-support: embedding it would force each app's wasm-bindgen version onto icm
- trunk, dx, cargo-mobile2, cargo-ndk

### 16.2 External tools (`doctor` checks the minimums; `tools.toml` pins the fetched ones)

| Tool | Minimum / pin | On this host |
|---|---|---|
| Xcode | ≥ 26, not a beta | 27.0 (27A266a) [V] |
| iOS simulator runtime | ≥ `min_os` | iOS 27.0 [V] |
| Android build-tools / platform | 35+ (36 preferred) / `android-<target_sdk>` | 35.0.0, 36.0.0 / android-36 [V] |
| NDK | ≥ r28 | 29.0.14206865 [V] |
| emulator + arm64 image | — | 37.1.11, android-36/37 [V] |
| JDK | ≥ 17 | 21.0.12.1 [V] |
| bundletool | 1.18.3 jar, sha256 | not installed; fetched by `doctor --fix --yes` |
| wasm-bindgen-cli | = app lock version | not installed; `cargo install … --root <cache>` |
| binaryen (wasm-opt) | version_133, sha256 | not installed |
| Chrome | any current with `--headless=new` | 154 [V] |
| AXe | optional | not installed |
| WiX v5, makensis, rc.exe/signtool | CI Windows image | — |
| dpkg-deb, dpkg-shlibdeps, appimagetool (pinned) | CI Linux container | — |
| Owner-only (printed) | altool/notarytool/stapler (Xcode), fastlane ≥ 2.240, wrangler/netlify/aws, asc (optional) | — |

---

## 17. CI for icm (`.github/workflows/icm.yml` in the fork)

Before enabling Actions, the 7 inherited upstream workflows are disabled on `mobile/0.14` (review §6.9).

As built (Appendix D, "CI"): `framework.yml` and `icm.yml`, with upstream's `check.yml` kept; the jobs' logic is in `.github/ci/`.

1. **Unit and golden tests** (ubuntu, macos, windows; `cargo test --locked` in `cli/`):
   - config validation with `file:line` golden errors; managed-key and raw-XML rejection; permission, orientation and version mapping
   - the privacy symbol classifier
   - profile matching on decoded fixture profiles (development, App Store, expired, wildcard)
   - the entitlement subset check
   - the zip writer (round-trip, deterministic bytes) and the ICO writer
   - insta goldens for every generated file: Info.plist (sim, device, release), PrivacyInfo, entitlements, AndroidManifest, themes, BundleConfig, index.html dev and release, `_headers`, rc, wxs, nsi, control, .desktop
   - `ICM_BLESS=1` updates the goldens
2. **Binary fixtures:** small Mach-O files (sim and device) and ELF `.so` files (16 KB aligned, 4 KB aligned, with and without the NativeActivity export, a high GLIBC need). They test the `object` gates without SDKs.
3. **Lockfile fixtures** from the agent-usability probes, one per `deps.*` FAIL.
4. **Fake-tool plan tests.** `ICM_TOOL_<NAME>` stubs record argv and replay canned outputs:
   - simctl JSON, and simctl exiting 0 for a dead app
   - adb `INSTALL_FAILED_UPDATE_INCOMPATIBLE`
   - a codesign hang, to exercise the watchdog
   - jarsigner unsigned/verified/self-signed output
   - altool error JSON, notarytool `Invalid`

   `--dry-run` plans are compared with golden `commands.txt`, so every pipeline's argv sequence is pinned. These tests run on Linux in seconds.
5. **Contract tests:**
   - every NDJSON line validates against `schema/output.schema.json`, and the last line is `result`, including for a forced panic (exit 70)
   - human mode emits only the protocol line types
   - the exit-code table holds under injected failures at every stage
6. **Catalogue coverage:** every emitted id has `docs/explain/<id>.md`, every doc maps to an id, and `explain --list` matches the enum.
7. **Upload-argv scan:** grep `cli/src` for the forbidden upload and notarize patterns outside `release/owner_plans.rs`.
8. **Schema freshness:** `icm print schema …` must equal `cli/schema/*.json`.
9. **Template drift:**
   - `icm new` output, normalized, equals `examples/app`
   - `template.no_new_lock_entries`
   - the framework CI builds `examples/app` for desktop, wasm32, aarch64-apple-ios-sim and aarch64-linux-android (check)
10. **Install check:** `cargo install --locked --git file://$GITHUB_WORKSPACE --rev $GITHUB_SHA icm`.
11. **Real-tool jobs** (each uploads `target/icm/latest/` and the screenshots):

    | Runner | Steps |
    |---|---|
    | macos-latest | `xcode-select` the newest Xcode ≥ 26 → `icm new /tmp/a --framework path:$GITHUB_WORKSPACE` → `doctor ios-sim desktop web --fix --yes` → `run ios-sim` → `run desktop` (permission WARN accepted) → `test` → `shot --headless --all-viewports` → `release ios --sign none` + `verify ios` → `release macos --sign none` |
    | ubuntu-latest + KVM | `doctor android --fix --yes` (licence acceptance written into the workflow by the owner) → `run android` (x86_64 image) → `test --on android --lifecycle` → `release android --sign none --apk` with a throwaway keystore test |
    | ubuntu-latest | `run web` + `shot web` → `release web` (serve smoke) |
    | windows-latest | `release windows --sign none`; `msiexec /i … /qn` install and uninstall |
    | ubuntu-latest, container `ubuntu:22.04` | `release linux --sign none`; `dpkg -i`; AppImage under Xvfb waits for `ICM_EVENT ready` |
12. **Release on tag `v*-mobile.*`:** asserts the CLI version equals the tag, and runs every job above. Prebuilt binaries come in phase 7.
13. **Downstream canary:** Tawara-mobile's CI runs `icm check --all` and `icm run android|ios-sim` with the new tag before Tawara bumps.

---

## 18. Implementation plan and acceptance tests

Estimates are focused engineer-days with agent help. The acceptance scripts live in `cli/tests/accept/phase<N>.sh`; each is `set -euo pipefail`, uses `/usr/bin/jq` [V], and runs from a fork checkout with `FORK=$(pwd)`, `F="$FORK/cli/tests/fixtures"` and `ACCEPT=$(mktemp -d)`, which holds every scratch output. Owner acceptance steps are listed separately and are never run by agents.

### Phase 0: framework prerequisites (1.5 d)
- Push local `main` as `mobile/0.14`. Add F2, F3, F4, the empty `agent` feature (F7 stub), `examples/app`, `docs/agents/limitations.md`, `OFL.txt` and the metadata fix. Tag `v0.14.1-mobile.1`.
- The push and tag are owner actions on the owner's repo; an agent prepares the commits.

**Acceptance:**
```sh
cargo check -p iced --target aarch64-apple-ios-sim && cargo check -p iced --target aarch64-linux-android   # iced_winit alone enables no Android activity
cargo build -p app
ICM_EVENTS=1 ./target/debug/app 2> "$ACCEPT/ev.log" & P=$!                           # events are opt-in (Appendix C 27)
for i in $(seq 60); do grep -q '^ICM_EVENT {"v":1,"kind":"ready"' "$ACCEPT/ev.log" && break; sleep 1; done; kill "$P"
grep -q '^ICM_EVENT {"v":1,"kind":"ready"' "$ACCEPT/ev.log"
ICED_TEST_BACKEND=tiny-skia cargo test -p app --test icm                            # runs tests/flows/*.ice
ICED_TEST_BACKEND=tiny-skia cargo test -p app --test icm -- icm-shot --viewport 402x874 --scale 3 --theme light --wait-ms 500 --out "$ACCEPT/h.png" && test -s "$ACCEPT/h.png"
git ls-remote --tags origin 'v0.14.1-mobile.1' | grep -q mobile.1                     # after the owner pushes
```

### Phase 1: dev loop on desktop, web, ios-sim, android emulator (14 d)

| Step | Work | Days |
|---|---|---|
| 1.1 | Core: config + spans + schema, host.toml, cargo metadata and lock, Reporter (human, NDJSON, run dir, last.json, latest/), catalogue + explain, exit codes, Plan/dry-run, process runner (stdin null, timeouts, groups, redaction), locks | 3 |
| 1.2 | `new` (embedded `examples/app`), `init --adopt-*`, `docs agents`, `doctor` (+ `--fix`, `--fix --yes`), `check` + deps checks, `print`, `framework`, `version` | 2 |
| 1.3 | ios-sim: managed simulator, plist/actool/privacy generation, ad-hoc signing, launch, ready (event + probe), crash collection, screenshot + preview, live logs | 2 |
| 1.4 | android: SDK/NDK discovery, NDK env, `cargo rustc` cdylib, manifest/res/icons, aapt2 → zip → zipalign → apksigner → verify, managed AVD and ports, install/launch/ready/screencap/logcat, `input` | 2.5 |
| 1.5 | desktop: run, ready, objc2 window lookup, screencapture with permission fallback, X11 | 1 |
| 1.6 | web: wasm-bindgen version pinning, site generation, session host (std server, CDP pipe, console), detached run, `stop`/`ps`, `input web` | 2 |
| 1.7 | Harness driver (`test`, `shot --headless`, `ui --headless`), signatures, hooks, AXe input, explain docs for every phase 1 id, CI items 1–11 (minus release jobs) | 1.5 |

**Acceptance** (`cli/tests/accept/phase1.sh`, on this Mac; must exit 0):
```sh
cargo install --locked --path cli
icm --version | grep -E '^icm 0\.14\.1-mobile\.[0-9]+ '
icm doctor desktop web ios-sim android --fix --yes --json -q | jq -e '.exit == 0'
icm new "$ACCEPT/demo" --id com.example.demo --framework path:"$FORK" --json -q | jq -e '.ok'
cd "$ACCEPT/demo"
icm check --all --json -q | jq -e '.ok and .checks.fail == 0'
for p in desktop web ios-sim android; do
  icm run "$p" --json -q > "$ACCEPT/run-$p.json"
  jq -e '.ok and .exit == 0 and .process.ready.source == "icm_event"' "$ACCEPT/run-$p.json"
  jq -e '(.warnings | map(.id) | index("run.screen_blank")) == null' "$ACCEPT/run-$p.json"
  test -s "$(jq -r .artifacts.preview "$ACCEPT/run-$p.json")"
done
icm run web --json | tail -n1 | jq -e '.type == "result"'                                  # last line contract
icm run ios-sim --json | while read -r l; do echo "$l" | jq -e '.v == 1' >/dev/null; done
icm logs ios-sim --level info --json -q | jq -e '.ok'
icm input android tap 200 400 --json -q | jq -e '.ok'
icm input web tap 100 100 --json -q | jq -e '.ok'
icm test --json -q | jq -e '.ok'
icm shot --headless --all-viewports --json -q | jq -e '.ok'
icm ui --headless tree --json -q | jq -e '.ok'
icm stop --all --shutdown --json -q | jq -e '.ok'
# negative cases (fixtures in cli/tests/fixtures/); `|| true` only lets the expected non-zero exit through,
# and the jq line on the saved file is the assertion
icm run ios-sim --config "$F/panics/icm.toml" --json -q > "$ACCEPT/p1.json" || true
jq -e '.exit == 10 and .errors[0].id == "run.app_panicked" and (.errors[0].evidence | length) > 0' "$ACCEPT/p1.json"
icm run android --config "$F/panics/icm.toml" --json -q > "$ACCEPT/p2.json" || true
jq -e '.exit == 10 and .errors[0].id == "run.app_panicked"' "$ACCEPT/p2.json"
icm check --config "$F/twocopies/icm.toml" --json -q > "$ACCEPT/p3.json" || true
jq -e '.exit == 3 and .errors[0].id == "deps.single_iced"' "$ACCEPT/p3.json"
icm run nowhere --json -q > "$ACCEPT/p4.json" || true
jq -e '.exit == 2' "$ACCEPT/p4.json"
```
Notes:
- On a host without Screen Recording permission, the desktop run passes with WARN `desktop.shot.permission` and a headless preview.

**Tawara acceptance** (scratch clone, never the real repo):
```sh
git clone ~/Tawara-mobile "$ACCEPT/tawara" && cd "$ACCEPT/tawara"
icm init --package tawara-mobile --adopt-ios platform/ios/Info.plist.in --adopt-android platform/android/AndroidManifest.xml --adopt-android-res platform/android/res --write --json -q | jq -e '.ok'
icm run ios-sim --json -q | jq -e '.ok and .process.ready.source == "probe"'
icm run android --json -q | jq -e '.ok and ((.checks.failed | length) == 0)'
```

### Phase 2: iOS release + iOS device (6 d + owner time)

| Step | Work | Days |
|---|---|---|
| 2.1 | Identity and profile discovery and decoding, minimal entitlements, DT keys, dSYM, privacy scan, icon flattening, sign-last with watchdog, `zip -X` IPA, §12.2 gates, artifacts.json, UPLOAD.md, upload.sh, `verify ios`, ledger, `diagnose altool` | 4 |
| 2.2 | ios-device dev loop (devicectl), development profile matching | 1 |
| 2.3 | `--via-xcode-export` fallback, only if the owner spike fails | 1 |

**Acceptance (agent):**
```sh
cd "$ACCEPT/demo"
icm release ios --json -q > "$ACCEPT/r.json" || true
jq -e '.exit == 9 and .errors[0].fix.by == "owner"' "$ACCEPT/r.json"      # this host: no matching profile / placeholder id
icm release ios --sign none --allow-dirty --json -q > "$ACCEPT/u.json"
jq -e '.ok and (.artifacts.ipa | test("\\.ipa$")) and ([.warnings[].id] | index("config.owner_decision") != null)' "$ACCEPT/u.json"
IPA=$(jq -r .artifacts.ipa "$ACCEPT/u.json")
! zipinfo -1 "$IPA" | grep -E '(^|/)\._|__MACOSX'
icm verify ios --artifact "$IPA" --json -q | jq -e '.ok'
unzip -q -o "$IPA" -d "$ACCEPT/ipa" && plutil -extract DTXcodeBuild raw "$ACCEPT/ipa/Payload/"*.app/Info.plist | grep -q "$(xcodebuild -version | awk '/Build version/{print $3}')"
xcrun assetutil --info "$ACCEPT/ipa/Payload/"*.app/Assets.car | grep -q '"Opaque" : true'
grep -q C617.1 "$ACCEPT/ipa/Payload/"*.app/PrivacyInfo.xcprivacy
```
Under `--sign none` the placeholder id, the unanswered encryption question and the missing profile are WARNs. A fixture app whose icon is RGBA must also pass `ios.icon.opaque_1024`, which proves the flattening.

**Owner acceptance:**
1. With a real id, the profile and the encryption answer, run `icm release ios`.
2. Run `upload.sh`.
3. The build shows as processed in TestFlight (internal).
4. `icm ledger mark-uploaded ios` records it.

If App Store Connect rejects the IPA, step 2.3 becomes mandatory and the owner acceptance is repeated.

### Phase 3: Google Play release + lifecycle suite (5 d + owner time)

| Step | Work | Days |
|---|---|---|
| 3.1 | Multi-ABI release build, proto link, base.zip, BundleConfig 16K, bundletool build/validate/dump, keytool/jarsigner via env names, signature verification by parsing, `--sign none`, smoke via build-apks, `--apk`, symbols zip, §12.3 gates, UPLOAD.md first-release checklist, `diagnose play`, `run --from-aab` | 3.5 |
| 3.2 | `test --on android|ios-sim --lifecycle` | 1.5 |

**Acceptance (agent):** test values only, in scratch:
```sh
cd "$ACCEPT/demo"
sed -i '' 's/^id = "com.example.demo"/id = "dev.accept.demo"/' icm.toml        # leave the placeholder id
cp "$F/icon-1024.png" assets/icon.png               # and the placeholder icon
export ICM_TEST_STOREPASS="$(openssl rand -hex 16)"
keytool -genkeypair -keystore "$ACCEPT/up.jks" -storetype PKCS12 -alias upload -keyalg RSA -keysize 2048 -validity 365 \
  -dname CN=test -storepass:env ICM_TEST_STOREPASS -keypass:env ICM_TEST_STOREPASS </dev/null
cat >> icm.toml <<EOF
[android.signing]
upload = { keystore = "$ACCEPT/up.jks", alias = "upload", store_pass_env = "ICM_TEST_STOREPASS" }
EOF
icm release android --allow-dirty --json -q > "$ACCEPT/a.json"
jq -e '.ok and ((.checks.failed | length) == 0)' "$ACCEPT/a.json"
AAB=$(jq -r .artifacts.aab "$ACCEPT/a.json")
java -jar ~/Library/Caches/icm/tools/bundletool/1.18.3/bundletool-all-1.18.3.jar dump config --bundle="$AAB" | grep -q PAGE_ALIGNMENT_16K
java -jar ~/Library/Caches/icm/tools/bundletool/1.18.3/bundletool-all-1.18.3.jar dump manifest --bundle="$AAB" | grep -q 'targetSdkVersion="36"'
env -u ICM_TEST_STOREPASS icm release android --allow-dirty --json -q > "$ACCEPT/a2.json" || true
jq -e '.exit == 9 and .errors[0].id == "android.keystore.password_env_unset"' "$ACCEPT/a2.json"
icm release android --sign none --apk --allow-dirty --json -q | jq -e '.ok'
icm test --on android --lifecycle --json -q | jq -e '.ok'
icm test --on ios-sim --lifecycle --json -q | jq -e '.ok'
```
**Owner acceptance:** the first manual AAB upload to Internal testing is accepted by Play Console, and a later `fastlane supply` draft upload succeeds.

### Phase 4: web release (2.5 d)
**Acceptance (agent):**
```sh
icm release web --allow-dirty --json -q > "$ACCEPT/w.json" && jq -e '.ok' "$ACCEPT/w.json"
SITE=$(jq -r .artifacts.site "$ACCEPT/w.json")
grep -q 'module_or_path: "./pkg/app_bg-' "$SITE/index.html"
grep -q 'application/wasm' "$SITE/_headers"
jq -e '[.checks.failed[]] | length == 0' "$ACCEPT/w.json"     # includes web.serve_smoke
```
**Owner acceptance:** deploy to one host and run `icm verify web --url <url> --json -q`; it reports ok.

As built, `cli/tests/accept/phase4.sh` runs these steps on a new template app (after `icm doctor web --fix --yes`), then `icm verify web` on the files and `icm verify web --url` against a local static server that serves the site, once with `.wasm` as `application/wasm` and once with the wrong type (exit 1, `web.mime`). Appendix D, "Web release", has the details.

### Phase 5: desktop releases (8 d)

| Step | Work | Days |
|---|---|---|
| 5.1 | macOS: universal, .app, icns, hardened runtime, two-stage notarization plan, hdiutil DMG, post-notarize verify, `diagnose notarytool` | 2.5 |
| 5.2 | Windows: rc/res/ico, VERSIONINFO, wxs (WiX v5), nsi, sign_command, verification | 3 |
| 5.3 | Linux: container build, glibc gate, deb (shlibdeps, Recommends), AppImage with bundled libs | 1.5 |
| 5.4 | `ci init` (per-host matrix, release on tag, artifacts as release assets, secrets referenced only by name) | 1 |

**Acceptance (agent):**
- **This Mac:**
  - `icm release macos --json -q` exits 9, with `macos.sign.no_developer_id` among `errors[].id`.
  - `icm release macos --sign none --json -q` is ok; `lipo -archs` and `macos.min_os` pass; the DMG mounts with `hdiutil attach -nobrowse`.
- **CI:** the Windows and Linux jobs from §17 item 11 pass (MSI install and uninstall, `dpkg -i`, AppImage ready under Xvfb, `linux.glibc_floor` PASS).

**Owner acceptance:** stage 1 → notarize → staple → `--dmg` → notarize → staple → `icm verify macos --after-notarize` passes `spctl`.

### Phase 6: agent hands (8 d; can run in parallel with phases 2–5 after phase 1)
- F5, F6 and F7 in the fork.
- The session bridge server for all platforms except ios-device.
- `icm ui`, `test --flows`, `ui messages`, and bridge screenshots for desktop on every OS.

**Acceptance:**
```sh
cd "$ACCEPT/demo"
for p in desktop web ios-sim android; do
  icm run "$p" --json -q | jq -e '.ok'
  icm ui "$p" tree --json -q | jq -e '.ok and (.artifacts.tree != null)'
  icm ui "$p" tap '"Increment"' --json -q | jq -e '.ok'
  icm ui "$p" find '"Count: 1"' --json -q | jq -e '.ok'
  icm ui "$p" messages --json -q | jq -e '.ok'
  icm test --on "$p" --flows --json -q | jq -e '.ok'
done
icm verify ios --artifact "$F/bridge-in-release.ipa" --json -q > "$ACCEPT/b.json" || true
jq -e '.exit == 1 and (.checks.failed | index("store.no_agent_bridge") != null)' "$ACCEPT/b.json"
```

### Phase 7: optional, on demand
- prebuilt icm binaries on tags
- `icm mcp` (stdio MCP over the same command layer)
- a non-blocking scheduled cold-agent evaluation
- iPad
- a dex path for JNI glue (GameActivity, `InputConnection`)
- localized names
- universal macOS by default
- a bridge on physical iOS (CoreDevice tunnel spike)

**Totals.** Phases 0–3, which give Tawara its dev loop and both stores, take about 26.5 days. Phases 0–6 take about 45 days.

---

## 19. Risks and unverified items

| Risk | Likelihood / impact | Mitigation |
|---|---|---|
| App Store Connect refuses a hand-built IPA even with the DT keys [S A5, A6] | medium / high | Phase 2 owner spike at TestFlight-internal before more iOS work. Fallback `--via-xcode-export` (Apple's exporter, still no Xcode project) [I] |
| Play rejects a no-dex `hasCode=false` AAB [I] | low / high | Phase 3 internal upload. Fallback: compile an empty class with `javac` + `d8` (both present [V]) and set `hasCode=true` |
| altool misbehaviour (wrong app on prefix collision [S A10]; exit 0 on failure [S A11]) | medium / medium | Parse JSON with `icm diagnose`; confirm with `--build-status --apple-id` or `--delivery-id`; Transporter or asc as alternatives |
| codesign keychain prompt hangs a non-interactive run | medium / medium | 60 s watchdog → exit 9 with the remedy |
| Headless GPU quirks (emulator `-no-window` with SwiftShader Vulkan; Chrome WebGPU without an adapter) | medium / medium | SwiftShader WebGL2 by default; `ICED_BACKEND=tiny-skia` and `debug.iced.backend` fallbacks; doctor probes the emulator GPU; signature table |
| Policy churn (targetSdk 37, Xcode floors, new ITMS codes, conflicting 16 KB dates) | certain / medium | Dated `policy/stores.toml`; `env.policy_stale`; 16 KB FAIL regardless of date |
| CLI/framework skew | medium / low | Embedded rev, skew WARN, protocol numbers, N/N−1 support, `self update --to-lock` |
| wasm-bindgen and wasm-opt coupling | certain / low | Per-version tool cache keyed by the app lock; features from rustc; serve smoke |
| Desktop formats need native hosts; Windows signing cost and eligibility [S D5] | certain / medium | CI matrix from `ci init`; generic `sign_command`; `--sign none` |
| Own WiX/NSIS/deb/AppImage templates are hard to test from a Mac | medium / medium | Golden files plus real CI install tests; phase 5 has 3 days for Windows |
| Linux glibc and libraries | medium / medium | ubuntu:22.04 container, `linux.glibc_floor`, Recommends for dlopened libraries, AppImage bundles matching libs |
| macOS Screen Recording permission | certain / low | WARN + headless render; bridge in phase 6 |
| No Apple Development or Developer ID certificate on this host [V] | certain / medium | Exit 9 with exact owner steps; the simulator and `--sign none` keep agents productive |
| Agents run owner commands | low / high | icm never executes them; the CI scan; AGENTS.md rule; the commands need credentials agents don't hold |
| Bridge in a shipped build | low / high | `debug_assertions` + feature + `release` refusal + marker gate; Tawara sets `agent = false` |
| Maintenance load of re-implementing Xcode/Gradle steps | medium / medium | Small modules, goldens, fake-tool plans, real-tool CI, Tawara as a standing consumer |

**Still unverified, each with a planned check:**

| Item | Checked in |
|---|---|
| IPA acceptance | phase 2 owner spike |
| AAB acceptance | phase 3 owner upload |
| WiX version, `rc.exe` via `-C link-arg` | phase 5 CI |
| wasm-opt feature mapping | phase 4 |
| AXe tap flags | phase 1.7 |
| `gcloud` token scopes | phase 3; fastlane is the default |
| Whether `devicectl --console` carries stderr | phase 2 |
| getrandom wasm backend detection | phase 1.6 |
| emulator `-no-window -gpu swiftshader_indirect` with wgpu | phase 1.4; tiny-skia fallback |

---

## Appendix A. Judge findings and their resolutions

| # | Finding (judge, design) | Resolution |
|---|---|---|
| 1 | Template uses `log::info!` without a `log` dependency (J1, cargo-icm) | `log = "0.4"` in the template (§8.2) |
| 2 | `run web` blocks in the foreground (J1, cargo-icm) | Detached session by default; `--attach` for foreground (§10.2) |
| 3 | `verify-upload` / `--write-snapshots` used but not in the command list (J1, cargo-icm) | Replaced by `icm diagnose`; `print snapshots --write` is in §6. Every command in this document is in §6 |
| 4 | `adb shell date +%m-%d\ …` mark breaks (J1, J3, cargo-icm) | `date +%s.%N` + `logcat -T <epoch>` [V] (§10.4) |
| 5 | AAB missing `PAGE_ALIGNMENT_16K` (J1, J2, J3; cargo-icm, icepack) | Set in BundleConfig, gated on `dump config` (§11.2) |
| 6 | iOS logs go stale after `run` (J1, cargo-icm) | `logs` re-queries live sources (§13.3) |
| 7 | Blank-screen diagnosis ignores FLAG_SECURE (J1, cargo-icm) | `dumpsys window` check → INFO `android.screen.secure` (§10.4) |
| 8 | No single "owner needed" exit; licence loops (J1, cargo-icm) | Exit 9 `NEEDS_OWNER`; licences are 9 (§4.5) |
| 9 | No iOS-sim input in phase 1 (J1, cargo-icm, icm) | AXe input in phase 1; headless `ui` harness (§13.5) |
| 10 | `iced::program::Program` is private (J1, icepack) | Template uses `iced::Program` [V `src/lib.rs:661`] |
| 11 | iOS-device beacon on a LAN IP, Local Network prompt (J1, J3, icepack) | No bridge on physical iOS in v1; loopback only (§13.7) |
| 12 | `iced/debug` changes the dev build and pulls devtools/tokio (J1, J3, icepack) | Not used; dedicated debug-only `agent` feature (§13.7) |
| 13 | Exit codes overloaded; panic = 1 (J1, J3, icepack) | Distinct codes; panic = 70 (§4.5) |
| 14 | Missing Screen Recording permission fails the whole run (J1; icepack, icm) | WARN `desktop.shot.permission` + headless render (§10.1) |
| 15 | `adb logcat -c` wipes global logs (J1, J3, icepack) | Never cleared; epoch marks (§10.4) |
| 16 | Host-specific values (AVD `cn_api36`, port 5554) in project config (J1, icepack) | Managed `icm_api<sdk>`; overrides in `host.toml`; ports 5580/5582/5584 (§7.6) |
| 17 | Release ledger appended on release, not upload (J1, icepack) | Written only by `ledger mark-uploaded`, the last line of upload.sh; `diagnose` maps duplicate-build errors (§11) |
| 18 | `--show` depends on the TTY (J1, icepack) | Headless always; `--show` / `ICM_SHOW=1` only |
| 19 | jarsigner prompts for a differing key password (J1, icepack) | `-keypass:env` always passed (defaults to the store env); stdin closed (§11.2) |
| 20 | `#:schema` URL to a missing tag or branch (J1; icepack, icm) | Local `./.icm/icm.schema.json` (§7.2) |
| 21 | IPA built with `ditto -c -k` contains `._*` files (J1, J2, J3; icepack) | `ditto` copy + `/usr/bin/zip -qry -X`; `ios.ipa.layout` + extracted-signature gate (§11.1) |
| 22 | `jarsigner -verify -strict` fails self-signed keys; plain verify exits 0 on unsigned (J1, J2; icm, cargo-icm) | Plain `-verify -verbose -certs`, parse `jar verified.`, compare the certificate with keytool (§11.2) |
| 23 | Android bridge token in a world-readable sysprop (J1, icm) | Written to app-private `files/` with `run-as` (§13.7) |
| 24 | `zipalign -c` run before signing (J1, icm) | Verified on the signed APK (§10.4) |
| 25 | DT keys computed for simulator builds (J1, icm) | Device and release only (§9.1) |
| 26 | No stdin=/dev/null or `GIT_TERMINAL_PROMPT=0` (J1, all) | Process hygiene for every child (§3) |
| 27 | Templates pin a tag that doesn't exist yet (J1, all) | `new` pins the tag or the full rev; `--framework path:`; phase 0 cuts the tag (§2.4) |
| 28 | altool build-status should also offer `--delivery-id` (J1) | Printed as an alternative; `diagnose` extracts the id (§11.1) |
| 29 | Wrong 16 KB date (J2; cargo-icm, icepack) | Policy table records 2025-11-01; gate FAILs unconditionally (§12.0) |
| 30 | MSI 4th-field versioning (J2; cargo-icm, icepack) | ProductVersion X.Y.Z + `AllowSameVersionUpgrades`; build in VERSIONINFO only (§11.5) |
| 31 | `release macos --notarize` uploads (J2, cargo-icm) | Removed; two-stage owner flow (§11.4) |
| 32 | No `MACOSX_DEPLOYMENT_TARGET` (J2, cargo-icm) | Set from `min_os`; `macos.min_os` gate (§11.4) |
| 33 | Empty dSYMs (J2, cargo-icm) | `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only` + UUID gate (§11.1) |
| 34 | Profile entitlements copied wholesale (J2, cargo-icm) | Minimal set + subset check (§9.3) |
| 35 | deb Depends incomplete; AppImage without libraries; glibc 2.39 build (J2, all) | `dpkg-shlibdeps` + Recommends; 22.04 container; bundled libraries; `linux.glibc_floor` (§11.6) |
| 36 | `gcloud --scopes` unverified (J2, cargo-icm) | fastlane supply is the printed default; the edits API is documented as [I] (§11.2) |
| 37 | `ic_launcher_round` referenced but not generated (J2; cargo-icm, icm) | Generated, both legacy and adaptive (§9.4) |
| 38 | cargo-packager notarizes when `APPLE_*` env is set (J2, icepack) | cargo-packager not used (§16.1) |
| 39 | Custom manifests: aapt2 flags don't override existing values (J2; icepack, icm) | No template mode; raw-XML rejection; `--replace-version`; gates on the linked artifact (§7.4) |
| 40 | App inside the DMG never stapled (J2; icepack, icm) | Staple the app before building the DMG (§11.4) |
| 41 | No gate for debug tooling in releases (J2, icepack) | `store.no_agent_bridge` marker gate + release refusal (§12) |
| 42 | No version-format gate (J2, icepack) | `ios.version.format` (§12.2) |
| 43 | No Windows exe icon or VERSIONINFO (J2; icepack, icm) | Generated `.rc` → `.res` linked into the exe (§11.5) |
| 44 | Package-time tool downloads, WiX 3.11 EOL (J2, J3; icepack) | Pinned, sha256-checked tool cache; WiX v5; only with `--yes` (§16) |
| 45 | Third-party uploaders as the primary path (J2, icepack) | altool first; asc documented as optional (§11.1) |
| 46 | Template pins `wasm-bindgen =0.2.129`, perturbing the framework lock (J2, J3; icepack) | No pin; icm installs the CLI version matching the app lock; `template.no_new_lock_entries` (§2.2) |
| 47 | Web release `init()` without the hashed wasm path (J2, icm) | Explicit `module_or_path` + `web.serve_smoke` (§9.5, §11.3) |
| 48 | `ios.icon.has_alpha` rejects RGBA (J2, icm) | Flatten onto `background`; gate on `assetutil` Opaque (§11.1) |
| 49 | No beta-Xcode gate (J2, all) | `ios.xcode.not_beta` (§12.2) |
| 50 | Non-exempt encryption follow-up missing (J2, all) | `export_compliance_code` + WARN with owner step (§7.2, §12.2) |
| 51 | Store listing assets missing from the owner checklist (J2, all) | Listed in UPLOAD.md (§11.1, §11.2) |
| 52 | No gate against unsealed files added after signing (J2, all) | Sign last + `xattr -cr` + extracted-IPA `codesign --verify --strict --deep` (§11.1) |
| 53 | `beta-reports-active` missing (J2) | In the distribution entitlements (§9.3) |
| 54 | Emulator port "5580 and up" (J3, cargo-icm) | Even ports 5580/5582/5584 only (§7.6, §10.4) |
| 55 | Minimum OS duplicated in `.cargo/config.toml` (J3, cargo-icm) | Removed; icm sets the env; `print env` for raw cargo (§7.3) |
| 56 | `DTXcode` read from a hard-coded `/Applications/Xcode.app` (J3; cargo-icm, icm) | Derived from `xcode-select -p` / `DEVELOPER_DIR` (§9.1) |
| 57 | Nested-workspace install marked unverified (J3, cargo-icm) | Verified by icepack's probe; CI install check (§2.2) |
| 58 | Android beacon needs INTERNET (J1, J3; icepack) | Debug builds add INTERNET when `agent = true` (§7.5) |
| 59 | Private cargo-packager cache layout pre-seeding (J3, icepack) | Not applicable (no cargo-packager) |
| 60 | cargo-packager compiled ungated on macOS; tiny_http unmaintained; core-graphics superseded (J3, icm) | None of them used; own std server; objc2-core-graphics, target-gated (§16.1) |
| 61 | Bridge direction inconsistent (J3, icm) | The app always connects out; no device bridge in v1 (§13.7) |
| 62 | Dev `index.html` imports a hashed name (J3, icm) | Dev is unhashed `app.js` with explicit `module_or_path`; release is hashed (§9.5) |
| 63 | Release runs never report ready (J3, icm) | Opt-in `ICM_EVENTS=1` / `debug.icm.events` / `?icm_events=1`; `ready.source` reported (§13.3) |
| 64 | Bridge feature in the template's default features (J3, icm) | Never in `default`; added only by `icm run` for dev builds (§8.2) |
| 65 | Swift helper compiled at run time (J3, cargo-icm) | objc2-core-graphics in-process (§10.1) |
| 66 | Gates parse vtool/nm/readelf text and need those tools (J3, cargo-icm) | `object` crate; portable fixture tests (§3, §17) |
| 67 | Scope: MCP, diagnose and the blocking agent eval too early (J3, icm) | MCP and the agent eval move to phase 7 (non-blocking); `diagnose` arrives with the releases that need it (phases 2, 3, 5) |
| 68 | The judges' common recommendation: no path dependency on `iced_beacon`; the CLI links no iced | Adopted (§0, §3) |


---

## Appendix C. Corrections from the final design review (these override the sections they name)


## A. Steps that fail on this machine, or factual errors

1. **The beta-Xcode gate rejects this machine's release Xcode.**
   - `ios.xcode.not_beta` and `env.xcode_beta` treat a build number ending in a lowercase letter as a beta.
   - Evidence: `xcodebuild -version` gives Xcode 27.0, build `27A266a`. It is a Mac App Store release (`/Applications/Xcode.app/Contents/_MASReceipt` exists).
   - Effect: every `icm release ios` here would FAIL with exit 1, and every dev run would WARN.
   - Fix: treat it as a beta only when the build's numeric part has 4 or more digits starting with 5 (e.g. `16A5230g`), or the Xcode path or name contains "beta". Make it a WARN, and leave `altool --validate-app` as the real gate.

2. **The JDK is found but never passed to child processes.**
   - Evidence:
     - `JAVA_HOME` is unset, `java -version` reports 1.8.0_431, and `java_home -V` lists only the Java 8 applet plugin.
     - `avdmanager list device` fails: "This tool requires JDK 17 or later. Your version was detected as 1.8.0_431".
     - `/usr/bin/jarsigner -help` fails: "Unable to locate a Java Runtime that supports jarsigner".
     - `which adb emulator` finds nothing.
   - Fix:
     - Export `JAVA_HOME=<jdk>` and put `<jdk>/bin` first on `PATH` for every child: sdkmanager, avdmanager, bundletool, keytool, jarsigner, apksigner and hooks.
     - Include both in `icm print env android`.
     - Resolve `platform-tools/adb`, `emulator/emulator` and `cmdline-tools/latest/bin/*` under the SDK, and add them to the tool-discovery table.

3. **`icm new` will never pin a tag.**
   - Evidence (`critic-cli/tagprobe`): I ran `cargo install --git file://… --tag v0.14.1-mobile.1`.
     - In the build script, `git rev-parse HEAD` works.
     - `git describe --tags --exact-match` fails with "fatal: No names found".
     - Cargo keeps the tag only as `refs/remotes/origin/tags/v0.14.1-mobile.1` in its git cache. The checkout has only `refs/heads/master`.
   - Effect: `ICM_GIT_TAG` is always empty for released installs.
   - Fix: derive the tag from `CARGO_PKG_VERSION`. CI already forces the version to equal the tag.
   - Second problem: all 21 local commits are unpushed (`git log origin/tawara/0.14-mobile..main` gives 21). A `--path` build would pin a rev GitHub cannot serve.
     - Fix: at build time, check that the rev exists on a remote. Otherwise default to `path:<checkout>`, or exit 2.

4. **The template's `[profile.*]` sections are ignored inside a workspace.**
   - Evidence (`critic-cli/ws`): a workspace member crate with `[profile.web-release]` and `[profile.dev.package."*"]`:
     - prints "warning: profiles for the non root package will be ignored";
     - fails `--profile web-release` with "error: profile `web-release` is not defined".
   - `examples/app` is in the root workspace (`members` includes `examples/*`), and so is Tawara-mobile's crate. So framework CI cannot build the template's web release in-tree, every fork build warns, and workspace apps lose the settings.
   - Fix: remove the profiles from the template and have icm pass them. I verified this works: `cargo build --profile icm-web --config 'profile.icm-web.inherits="release"' --config 'profile.icm-web.opt-level="z"' --config 'profile.dev.package."*".opt-level=2'`.

5. **`icm = ">=0.14.1-mobile.1"` breaks at the first patch or 0.15 bump.**
   - Evidence (`critic-cli/semvert`, semver 1.0.28): the requirement matches `0.14.1-mobile.2` but not `0.14.2-mobile.1` or `0.15.0-mobile.1`. That is semver's pre-release rule.
   - Effect: `config.too_new` (exit 4) fires right after the first upstream 0.14.x rebase.
   - Fix: compare with `Version`'s ordering against a plain `min_icm`, not `VersionReq`.

6. **Changing the minimum OS does not rebuild the binary.**
   - Evidence (`critic-cli/dsym`):
     - A macOS binary built without the env var had minos 11.0. Rebuilt with `MACOSX_DEPLOYMENT_TARGET=13.0`, cargo reported Fresh and minos stayed 11.0 until a source file was touched.
     - The same happened for iOS: 16.0 stayed 16.0 after setting 17.0.
   - Effect: editing `[ios] min_os`, or an earlier raw `cargo build`, makes `ios.macho.minos` or `macos.min_os` FAIL with nothing the agent can fix.
   - Fix: stamp the deployment target per target and profile. When it changes, run `cargo clean -p <pkg> --target <t>` so only the app relinks. Build releases in a dedicated `--target-dir`.

7. **The headless harness is not tiny-skia and not deterministic.**
   - Evidence:
     - `ICED_TEST_BACKEND` is read only by `Simulator` (`test/src/simulator.rs:77`).
     - `iced_test::run` and `iced_test::screenshot` use `Emulator`, which calls `Renderer::new(.., None)` (`test/src/emulator.rs:98-104`).
     - The fallback renderer then tries wgpu first (`renderer/src/fallback.rs:681-697`, `wgpu/src/lib.rs:918`).
   - Effect: on this Mac it renders with Metal; on a GPU-less CI runner it uses tiny-skia. Screenshot goldens differ by machine.
   - Fix: F4 adds a backend parameter to `Emulator`, `run` and `screenshot`. Correct the claims in §13.1 and §13.2.

8. **The template's pinned toolchain lacks Android and wasm targets here.**
   - Evidence: `rustup target list --installed --toolchain 1.98.0` shows only darwin, ios, ios-sim and windows-msvc. The android and wasm32 targets are installed on `stable`, which is also 1.98.0.
   - Effect: the first cargo call either fails or makes rustup download in the background. That breaks "downloads only with `--yes`" and `--offline`.
   - Fix:
     - `doctor` resolves the project's active toolchain (`rustup show active-toolchain` in the app directory) and checks targets against it.
     - Run children with `RUSTUP_AUTO_INSTALL=0` (rustup is 1.29.1 here).
     - `--fix --yes` adds the targets.

9. **The design's description of the fork is out of date.**
   - HEAD is `8308d8fdd`, four commits after `4ed24aa73`:
     - iced's default features now include `mobile-logger`, `mobile-fira-sans` and `mobile-system-fonts`;
     - `android_main!` now ends the process (`db67504e6`);
     - `graphics/fonts/OFL.txt` already exists, so F0's "add OFL.txt" is done;
     - `src/mobile.rs`, `runtime/src/window.rs` and `wgpu/src/window/compositor.rs` have uncommitted changes.
   - Update §0, §14 F0/F1 and `limitations.md`.

10. **The Android emulator image is hard-coded to arm64.**
    - The managed AVD and doctor's sdkmanager line both name `arm64-v8a`. §17's ubuntu+KVM job needs an x86_64 image.
    - Fix: pick the image ABI from the host architecture.
    - The port rule "even ports ≤ 5584 only [V emulator help]" is too strict. This host has run emulators on 5600, 5610 and 5620 (`~/.android/modem-nv-ram-56{00,10,20}`). `scratchpad/e2e-android/e2e.sh` drove `adb -s emulator-5620` and saved screenshots. Make the port range configurable.

## B. Contradictions

11. **`-q` hides the diagnostics that AGENTS.md tells agents to fix.**
    - §4.1 says `--json -q` prints only the result line. AGENTS.md step 1 says `icm check --all --json -q`, then "fix the `diagnostic` events".
    - Fix: put the first N rendered diagnostics (file, line, rendered text) into `errors[]` in every output mode.

12. **Who writes the ledger.** §8.1 says `.icm/ledger.toml` is written only by `ledger mark-uploaded`. §8.3 has `icm new` writing `[template] icon_sha256` into it. Keep the placeholder hash inside the icm binary instead.

13. **Running `icm run web` twice.** The phase 1 script runs it a second time while the first session still holds port 8787. §10.2 says a busy port exits 7. Define that `run <p>` replaces this project's existing session for that platform.

14. **`--yes` has two meanings.** It allows downloads, and `--reinstall --yes` also wipes app data. AGENTS.md teaches agents to add `--yes` routinely. Use a separate `--wipe-data` flag.

15. **`doctor`'s exit code is undefined for agent-only items.** If AXe is missing (`env.tool_missing`, `by: agent`), the exit code is unspecified, yet the phase 1 script requires exit 0. Make optional tools a WARN.

16. **Tawara §15 step 5 is wrong.**
    - Tawara-mobile has no direct `iced` dependency. It gets iced through `tawara-app` from Tawara-wallet at rev `38dd452`.
    - Its `iced_winit` dependency uses `default-features = false`, and Tawara forbids system-font fallback (D6, quoted in the `8308d8fdd` commit message).
    - Using `iced::android_main!` therefore means adding `iced` with exactly Tawara-wallet's source, explicitly enabling `android-native-activity`, and leaving out `mobile-system-fonts`. That is a Tawara-wallet change first.

17. **A git winit would break Tawara.**
    - Under owner answer 2, iced_winit would take a git winit. Tawara-mobile also depends on crates.io `winit = "=0.30.13"` directly, so it would get two winits and FAIL `deps.single_winit`.
    - That fork tag must also ship `iced::mobile` re-exports covering every winit type apps use (`AndroidApp`, the linked objc2 versions) and a migration note.

## C. Missing publish requirements

18. **No release ships licence notices.**
    - The fork's own `mobile-fira-sans` feature comment says apps that ship Fira Sans "must ship `graphics/fonts/OFL.txt`". iced and its MIT/Apache dependencies also need notices.
    - Fix: add a step that generates `THIRD_PARTY_NOTICES` (licences from `cargo metadata`, plus OFL when a fira feature resolves). Put it in the `.app`, Android `assets/`, the web site and desktop packages, with a gate that it is present.

19. **Store screenshots and required URLs are missing.**
    - App Store Connect needs 6.9-inch (1320×2868) or 6.5-inch iPhone screenshots [S]. The managed simulator is an iPhone 17 (402×874 @3 = 1206×2622), which is not a required size. An iPhone 17 Pro Max simulator type is installed here.
    - Fix: add `icm shot ios-sim --store` and a Play phone-screenshot preset.
    - Add a privacy policy URL and a support URL to UPLOAD.md. Both are required App Store Connect fields, and Play's Data safety section requires a privacy policy [S].

20. **Windows releases depend on the VC++ runtime.**
    - Rust's `x86_64-pc-windows-msvc` target links `vcruntime140.dll` dynamically by default. `windows-latest` runners have it installed, so the msiexec CI test cannot catch a clean-machine failure.
    - Fix: build releases with `-C target-feature=+crt-static`, or bundle the redistributable. Add a gate that reads the PE imports with `object`.

21. **Tawara is a crypto wallet; the owner checklist misses the store rules for that** [S].
    - App Store Guideline 3.1.5(i): wallets that store virtual currency must come from developers enrolled as an organization.
    - Play: the financial-features declaration and crypto-wallet policy.
    - Add both to §15 step 6. They decide whether the app can be listed at all.

22. **Nothing tests the minimum OS versions.**
    - The template targets iOS 16, but `run` always picks the newest runtime (iOS 27). iOS 18.1 and 18.3 runtimes are also installed here.
    - No Android API 26–29 image exists (only android-36 and android-37.0).
    - Fix: add `--runtime min` and a CI job on the lowest runtime.

23. **The dSYM gate cannot detect an empty dSYM.** An empty dSYM has the same UUID as the binary. Gate on `dwarfdump --debug-line` naming one of the crate's source files. The pipeline itself works: a post-build `dsymutil` produced line tables in the probe, because the object files stay in `deps/`.

## D. Agent-operability gaps

24. **Builds outlast an agent's command timeout.**
    - Tawara's CI log (`scratchpad/a18/run1.log`) shows release builds of 2m19s to 5m12s per target.
    - A typical agent shell defaults to a 120 s command timeout and allows at most 600 s for a foreground command. A cold `icm run android` or a two-ABI `icm release android` can be killed mid-build, orphaning cargo and the emulator.
    - Fix: `--detach` returns `{run, status: "running"}` right away; `icm wait <run> --timeout 9m` can be called repeatedly; SIGTERM or SIGHUP kills the process group and still writes a result. AGENTS.md should say to prewarm with `icm build --all --detach`.

25. **There is no coordinate system for input.**
    - The preview's long edge is at most 1024 px. adb uses device pixels (e.g. 1080×2400), AXe and the simulator use points (402×874), and CDP uses CSS pixels. An agent reading a position off `screen.preview.png` will tap the wrong place on every platform.
    - Fix: give `icm input` one coordinate space (preview pixels by default, `--space px|pt`), and report `screen{px, pt, preview, scale}` in the result.

26. **`icm logs ios-sim` loses debug output.**
    - `oslog-0.2.0/src/lib.rs:29-35` maps `log` Info to os_log Default, Debug to os_log Info, and Trace to os_log Debug. `log show` hides the Info and Debug types unless given `--info --debug`, and those types are held only in memory. So re-querying later returns nothing at `--level debug`.
    - Fix: start a detached `simctl spawn <udid> log stream --level debug --style ndjson` collector at launch. That means ios-sim needs a session in phase 1 too, not only web.

27. **Make `ICM_EVENT` opt-in everywhere.** Emitting it whenever `debug_assertions` is on adds lines to every iced app's debug stderr. Instead, icm always sets `ICM_EVENTS=1`, the Android sysprop, or `?icm_events=1`, so debug and release behave the same.

28. **Web readiness is unverified for iced.**
    - The [V] covers a raw WebGL2 page. `counter.wasm` was only compiled, never run.
    - The same probe printed `webgpu=true` for headless Chrome 154, and iced's default `wgpu` feature includes the WebGPU backend. wgpu may therefore choose WebGPU and fail to get an adapter.
    - Fix: add a day-0 spike to phase 1.6 (wasm-bindgen + headless Chrome: ready, not blank) before building the CDP session.

29. **Nothing tests what new apps actually resolve.**
    - §8.1 lists a committed `Cargo.lock`, but no offline step creates it. The first cargo call resolves fresh from crates.io, which framework CI never tested. Review §6.1 shows a fresh resolve adds objc2 0.6.5 through softbuffer 0.4.8.
    - Fix: add a CI job that runs `icm new`, a fresh `cargo generate-lockfile`, then `icm check --all`, like review §4.9's `fresh-resolve` job.

## E. Too much for phase 1

30. **Cut phase 1 to the dev loop.**
    - Move to later phases: `init --adopt-*`, `framework set`, `self update`, `version`, `ledger`, `diagnose`, schema printing and its freshness CI, review snapshots, cross-project device locks, session TTL, `--watch`, `--dry-run` golden plans for every pipeline, AXe, and Windows host support.
    - Tawara's `icm.toml` is about 30 lines, already written out in §15. Write it by hand instead of building the adopt command.
    - §16.1 never lists `windows-sys`, though the design relies on Windows job objects. `std::fs::File::lock` (stable since 1.89; Tawara already relies on `try_lock` for its 1.89 floor) replaces `libc` flock.
    - Generate the explain docs from the catalogue enum (title, fix, `by`), and hand-write only the ~20 most common. Step 1.7 does not fit in its 1.5 days.

Small fix: the phase 3 script's comment "# leave the placeholder id" sits on a `sed` that changes the id.

---

## Appendix D. What the CLI core decided (implementation notes)

These record where the code (`cli/`, with its tests) settles something the sections above left open or stated differently. The code wins; keep this list current.

1. **`min_icm`.** The minimum icm version is `min_icm = "0.14.1-mobile.1"` (a plain version, compared by semver ordering, item 5). `icm = ">=X"` is still read as the same minimum; both at once is `config.invalid`. `schema` above 1 is `config.too_new` (exit 4).
2. **Ids added to §5.** `usage.bad_args` and `usage.not_implemented` (2); `step.timeout` (8, any step or `--timeout`); `tool.failed` (6, generic step failure); `run.interrupted` (130); `run.still_running` (8, `icm wait` ran out while the detached run continues); `run.detached_lost` (70); `run.not_found` (2); `env.rust_toolchain_missing` (4, doctor-yes, item 8); `build.cargo_failed` (5, cargo failed without rustc diagnostics). `config.too_new` is by `agent` (it names the `cargo install` command).
3. **Output.** Every event line starts `{"v":1,"type":…,"run":…,"t":…`. The result object also carries `v`, `run`, `schema`, and every key of §4.4 even when null. A blocking error that was already reported as a non-blocking FAIL moves to `errors[0]` instead of appearing twice. Build failures attach the first 10 error diagnostics to `errors[0].diagnostics` (item 11). `events.ndjson` gets the result line before `result.json` is written, so a reader that sees `result.json` has the whole stream.
4. **Run directories.** Commands that do work keep `runs/<id>/` under `<target>/icm` once the project resolves, otherwise under the cache dir (`~/Library/Caches/icm`, `ICM_CACHE_DIR`). `explain`, `print` and `wait` are views and keep none; usage errors (exit 2) keep none. Paths in results are relative to the current directory when inside it. `prune` keeps the newest 30 runs and never removes a run without `result.json` whose icm (`owner.json`, or `detached.json` for a detached one) still runs, nor a run that a file in `<root>/sessions/` names.
5. **Content commands.** `print` and `explain` write their content to stdout in human mode (so `eval "$(icm print env android)"` works) and protocol lines to stderr, only on failure. With `--json` the content is a result field.
6. **`--detach` / `icm wait`** (item 24). The parent creates the run directory, re-executes icm in a new session with `ICM_RUN_ID`/`ICM_RUN_DIR`/`ICM_RUN_ROOT`/`ICM_DETACHED=1`, writes `detached.json`, and returns `status: "running"` with a `next` of `icm wait <run> --timeout 9m --json -q`. `icm wait` (default 9 minutes) replays the run's events and exits with its exit code; on timeout it returns `run.still_running` (exit 8, `status: "running"`).
7. **Signals.** SIGINT, SIGTERM and SIGHUP are recorded; the runner kills its child's process group (SIGTERM, then SIGKILL after 2 s) and the command fails `run.interrupted` (130) with a written result. A command that returns any error after a signal finishes `run.interrupted`, its own error next in `errors[]` (a child icm killed looks like a failed child: `cargo metadata` must not become `config.invalid`); processes run outside `Ctx::step`/`probe` (`cargo metadata`, `xcodebuild -version`) check `context::end_error` first, and `cargo metadata` is bounded by what is left of `--timeout`. A watchdog thread does the same after 5 s if the main thread does not notice; while the main thread is stopping what it started (`signals::cleanup`: desktop's SIGTERM grace, SIGKILL and session removal, with the app's group still registered) it waits, up to 20 s in all.
8. **Runner.** Child output goes to files, never pipes (a daemon such as the adb server that inherits stdout cannot hang icm). Children get `RUSTUP_AUTO_INSTALL=0`, `CARGO_TERM_COLOR=never`, `GIT_TERMINAL_PROMPT=0`, `GIT_SSH_COMMAND="ssh -oBatchMode=yes"` (unless set) and `LC_ALL=C` (opt-out per step). Secrets are redacted in argv (`pass:…`, values after password flags, `NAME=value` for secret names; Android's published debug keystore password `android` is left readable, so a printed keytool or apksigner command still works), in the env delta, and in the step logs when a tool echoes a secret it was given. The reporter redacts the same values in every string of every event and of the result (`events.ndjson`, `result.json`, `last.json`, stdout) and in progress lines, whatever produced them: the values of secret-named variables in icm's environment (6 bytes or more, not a path) and those icm handed to a child under a secret name (4 or more), a multi-line value also line by line. A hook's stdout line is redacted before it is parsed, so a secret never reaches a check id. Captured stdout stays raw inside icm, as parser input (a cargo artifact path must stay a path).
9. **host.toml** also accepts `java_home` and `android_ndk`. `emulator_ports` must be even ports in 5554–5682 (item 10); the default stays `[5580, 5582, 5584]`.
10. **JDK discovery** reads every candidate's version (`release` file or `java -version`); `/usr/libexec/java_home -v 17+` returns the Java 8 applet plugin on this host and is rejected. Order: host.toml, `$JAVA_HOME`, `java_home -v 17+`, Homebrew `openjdk@21`/`@17`/`openjdk`, `/Library/Java/JavaVirtualMachines/*`, Android Studio's JBR, `/usr/lib/jvm/*`.
11. **Version embedding** (item 3). `build.rs` embeds the rev and the default framework pin: in a cargo git checkout (`.cargo-ok`), `tag:v<version>` when cargo's database has `refs/remotes/origin/tags/v<version>` at HEAD, else `rev:<sha>`; in a local checkout, `rev:<sha>` only when a configured non-local remote's branches contain HEAD and the tree is clean, else `path:<checkout>`. `ICM_BUILD_FRAMEWORK` forces it.
12. **Deployment targets** (item 6). Stamps live in `target/icm/stamps/deployment-<triple|host>-<profile>.txt`; a changed value, or an existing build icm did not stamp, runs `cargo clean -p <pkg> --target <t>` first.
13. **Input coordinates** (item 25). `--space preview|px|pt` (default `preview`); `screen.rs` converts and renders `screen{px, pt, preview, scale}`.
14. **Later-phase commands** (`init`, `version`, `framework`, `docs`, `ci`, `self`, `mcp`) parse and exit 2 `usage.not_implemented`, as do phase-1 commands a build does not implement yet. `release`, `verify`, `upload-commands`, `ledger` and `diagnose` have their own surface since the release core (section "Release core" below); a target pipeline that is not implemented yet exits 2 `usage.not_implemented` from there. `icm print tools` (discovery report) and `icm print commands` (the clap surface as JSON) exist from the core on.
15. **Cargo messages.** Invocations pass `--message-format=json`, not `json-render-diagnostics`: the latter makes cargo render rustc's diagnostics to stderr itself and emit no `compiler-message`, so `diagnostic` events and `errors[].diagnostics` stayed empty.
16. **`deps.single_iced`** groups iced crates by source and revision only. The fork's crates do not share one version (`iced_widget` is 0.14.2 next to `iced` 0.14.1), so a version in the key failed every app.
17. **`icm new`.** `build.rs` embeds `examples/app/**` (minus `target/`) and `docs/agents/limitations.md`; `src/template.rs` fills them (§8.5) and a unit test checks that rendering with the template's own names and `path:../..` gives the template back byte for byte. The package and binary name come from the directory (`My Notes` → `my-notes`, library `my_notes`), the display name too (`My Notes`) unless `--name`, the id is `com.example.<lib>` unless `--id` (a WARN `app.id.placeholder`). The source is `--framework`, else the build's pin (item 11 above); a build without git metadata exits 2 `new.framework_unknown`, a non-empty directory exits 2 `new.dir_not_empty` unless `--force`. Inside another cargo workspace the new Cargo.toml gets its own `[workspace]`. `new` writes no Cargo.lock: `check` (or `doctor web --fix --yes`) resolves it first (Appendix C item 29). AGENTS.md's `{{framework_tag}}` is the tag, `rev <12 hex>` or `from <checkout>`.
18. **`icm doctor`** probes without changing anything and reports one CHECK per requirement (a PASS uses the failure's id). `--fix` runs the `by: doctor` fixes (managed simulator and AVD, the debug keystore `<host.toml dir>/android/debug.keystore` that the Android build signs with); `--fix --yes` (not with `--offline`) also the `by: doctor-yes` ones: `rustup toolchain install`, `rustup target add --toolchain <project toolchain>`, `brew install openjdk@21`, one merged `sdkmanager --sdk_root=<sdk> --install …`, `xcodebuild -downloadPlatform iOS`, `cargo generate-lockfile`, `cargo install wasm-bindgen-cli --version <lock> --locked --root <cache>/tools/wasm-bindgen/<v>`. Fixes run in dependency order and everything is probed again, up to three rounds (a system image, then the AVD that uses it). Exit: 4 while a doctor/doctor-yes FAIL remains, else 9 for owner FAILs (SDK licences, Xcode, Chrome, cmdline-tools), else the failure's own exit, else 0; `git` is optional (WARN). Without platforms it checks `[app] platforms` (iOS as ios-sim, only on macOS), or all four outside a project. The result's `tools` records what was found. New ids: `env.simulator_missing`, `env.avd_missing`, `env.debug_keystore_missing` (4, doctor).
19. **Managed devices** (`src/managed.rs`): the AVD is `icm-api<target_sdk>` (system image `system-images;android-<api>;google_apis;<host ABI>`, hardware `pixel_9`), the simulator `icm-<type>-ios-<runtime>` (the type slugged: `icm-iphone-17-ios-27.0`, the name `icm run ios-sim` uses too) with the newest plain `iPhone <n>` the newest runtime ≥ `[ios] min_os` supports, or host.toml `[ios] simulator_type`. Pinned devices in host.toml (`simulator_udid`, `[android] avd`/`device`) replace them. Android's per-user files follow `ANDROID_USER_HOME` / `ANDROID_AVD_HOME`, so tests never touch `~/.android`. icm never shuts down a device whose name does not start with `icm-`, nor an `icm-test-` one (Android makes one exception: the emulator `icm run android` booted itself for the session).
20. **`icm check`** runs config (icm.toml, icon: a square PNG ≥ 1024 read from its IHDR, placeholder WARN by sha256; the package, binary or library per platform; INFO `deps.legacy_entry` / WARN `android.so.export` for Android's entry), toolchain targets (exit 4), `cargo generate-lockfile` when there is no lock, the lockfile checks and, with Android, `deps.android_activity_backend` (exit 3: `cargo tree -p <pkg> --target <android triple> -e normal,build -f '{p}|{f}'` must show android-activity with exactly one of `native-activity`/`game-activity`, the one `[android] activity` names; a failing `cargo tree` is a SKIP), then `cargo check` (or `clippy`) per platform, all platforms even after a failure. Triples: host, `wasm32-unknown-unknown`, the simulator triple, `aarch64-apple-ios` for ios-device, the first `[android] abis` entry. Each failed platform is a FAIL whose `diagnostics` hold its first 10 rustc errors and whose evidence lists their `file:line` (paths relative to the current directory), so `-q` in either mode shows them; `errors[0]` is a failure with rustc errors. `--all` or no platform means `[app] platforms`.
21. **Sessions** (`src/session.rs`, schema `icm.session/1`): `target/icm/sessions/<platform>.json` with `platform`, `run`, `started`, `pid`, `pids[{pid,what}]`, `app`, `device{kind,id,name,managed}`, `stop` and `shutdown` (argv lists), `url`, plus any other keys. `icm stop` runs `stop`, then SIGTERM (SIGKILL after 3 s) to each pid, to its group when it leads one, and only while the process started before the file was last written (a pid reused after a reboot is left alone); `--shutdown` runs `shutdown` for managed devices and also shuts down the project's managed simulator or emulator when it runs without a session. Stopping nothing is ok; `icm stop` without a platform or `--all` exits 2. `icm ps` lists sessions with `running` and `alive` pids; Android and web records also ask the platform whether the app runs (`app_running`: `pidof` on the recorded device and an activity of the app in `dumpsys activity activities`, since a process that outlived its destroyed activity is not the app running; the session's `status`, where a panicked page or a crashed renderer is not running), otherwise print `session open (app state unknown)`, and a record whose run ended with `ok: false` says `(its run failed)`. The dev platforms keep their own records and stop them themselves: `icm stop <platform>` goes to the platform, and `icm stop --all` calls desktop's, ios-sim's, android's and web's own stop in turn (a failure there is a WARN, so cleanup goes on), then ends any other record as above. An Android record's app pid is on the device (`app_pid`; `pid` in older files is read as that), never a host pid to probe or signal.
22. **`icm explain config.<key>`** explains an icm.toml key (or table) from the template's annotated icm.toml; catalogue ids keep precedence.

### Desktop (`cli/src/platform/desktop/`, §10.1, §13.1, §13.3)

- **Build.** `cargo build -p <pkg> --bin <bin>` with icm's profiles as `--config` (`cargo::profile_config`: dev `profile.dev.package."*".opt-level=2`, release `profile.release.lto="thin"`; Appendix C 4) and `MACOSX_DEPLOYMENT_TARGET` from `[desktop.macos] min_os` (stamped, item 12). The executable is hard-linked to `target/icm/build/desktop/<debug|release>/<bin>`; `run` launches that path and `--no-build` reuses it. `icm build desktop` stops there.
- **Launch.** The app gets its own session and process group (setsid), the project directory as its cwd, `ICM_EVENTS=1`, `ICM_RUN_ID`, `RUST_BACKTRACE=1` and the `--env` pairs, with stdout and stderr in `app.stdout` and `app.stderr` of `target/icm/sessions/desktop/<run>/` (like ios-sim, outside the run directories that pruning removes; the next launch removes them), copied into the run directory with `app.log` and `logs.ndjson` once the run ends or is ready, so evidence names the copies. `target/icm/sessions/desktop.json` (copied into the run and the live directory as `session.json`) records pid, pgid, run, executable, log files, launch mark, window and the `ready` event. `run` stops the project's previous desktop app first (result `replaced`, Appendix C 13).
- **Ready.** The first `ICM_EVENT ready`. Only when no `start` event came: the probe, alive after 3 s and (macOS) owning a layer-0 window. A panic (the event, or std's `panicked at` line; icm waits 0.5 s for its message) is `run.app_panicked` with `app.stderr:<line>` and the source `file:line` as evidence (rustc's path resolved against the package, then the workspace root); an exit is `run.app_died` with the code or signal and the last stderr lines; no frame is `run.not_ready`. A failed run stops the app (`process.exit.stopped_by_icm`). `--settle` is watched the same way, then `run.alive` passes. `font.default_missing` warnings become WARN `run.font_missing`.
- **Screenshot.** macOS: the window id from `CGWindowListCopyWindowInfo` by pid and `CGPreflightScreenCaptureAccess`, through plain CoreGraphics/CoreFoundation FFI (no objc2 crates); then `screencapture -x -o -l <id>`, cropped to the content's physical size from the `ready` event (the title bar removed). Without Screen Recording (the preflight, or screencapture's "could not create image from window"): WARN `desktop.shot.permission`, then `ICED_TEST_BACKEND=tiny-skia cargo test -p <pkg> --test icm -- icm-shot --viewport WxH --scale S --theme <system appearance> --out <run>/screen.png` at the window's size. Linux: X11 `xdotool search --onlyvisible --pid` and `import -window` (not verified yet); Wayland or no display: the headless render. Without a harness the run is still ok, with WARN `harness.missing` and no screenshot. The result's `screen` adds `source` (`window` or `headless`) and, for a fallback, `note`.
- **Images** (`image.rs`, png 0.18): previews are box-filtered to a long edge of at most 1024 px; blank means at least 99.5 % of pixels within 8 per channel of the dominant colour.
- **Logs.** stderr lines in `init_logger`'s format become `app` records with their time, level and target; `ICM_EVENT` lines are `app` records tagged `ICM_EVENT`; a panic and its message and backtrace are one `error` record; other lines are `stderr` or `stdout` records (`error`/`warn` when they start so). `run` writes `app.log` and `logs.ndjson`. `icm logs desktop` reads the session's files, or the last run's once the app is gone, prints each record as a `log` event (`LOG` line) and returns `records` and `counts{total, matched, shown}`. `--grep` takes substrings separated by `|`, matched ignoring case (no regex; `src/grep.rs`, shared by every platform, WARNs `usage.bad_args` when a pattern that looks like a regex or alternation matches nothing); `--since <dur>` compares record times; `--source system|crash` has nothing on the desktop; `--raw` prints the files' last `--tail` lines; `--follow` streams until the app ends or Ctrl-C, and exits 0. `run --attach` streams from the launch: the app's exit decides the result (code 0, SIGTERM, SIGINT or SIGHUP are ok) and Ctrl-C stops the app.
- **shot, stop, input.** `icm shot desktop [--out P] [--name L]` (`screen-<L>.png`) needs the running app (`run.no_session`, exit 7). `icm stop desktop` sends SIGTERM to the group and SIGKILL after 5 s, and is idempotent (`stopped: []`); before any signal the pid is checked against the session's executable (`ps -o command=`), so a reused pid is left alone. `platform::desktop::stop_session` is public for `stop --all`. `icm input desktop …` exits 2 `input.unsupported`. Only `run` moves `latest/desktop`.

### iOS Simulator (`cli/src/platform/ios_sim/`; §10.3, Appendix C items 6, 22, 26)

- Simulator: `--sim`/`--device` (name or UDID), host.toml `simulator_udid`, else the managed `icm-<type>-ios-<version>` (e.g. `icm-iphone-17-ios-27.0`; the type is the newest plain `iPhone <n>` the runtime supports), created when missing. `--runtime newest|min|X.Y` picks the runtime (`min`: the lowest at or above `min_os`; item 22). `--fresh` creates `icm-fresh-…`, which `stop` deletes. icm shuts down or deletes only `icm-*` simulators. `simctl boot` runs before the build and `bootstatus -b` after it, so the boot overlaps cargo. A missing simulator is `ios.sim.not_found` (7, new).
- Bundle: Info.plist and PrivacyInfo are written by icm's own XML writer (no plist crate; tool plists are read with `plutil -convert json`). actool output is kept in `gen/` and reused while a stamp (icon hash, background, `min_os`, Xcode build) is unchanged; the `.app` is rebuilt each time, then `plutil -lint`, the required-key, scene-manifest and privacy gates, `xattr -cr`, `codesign --sign -`. The Mach-O gates parse `LC_BUILD_VERSION` directly (no `object` crate). `png` 0.18 is the one new dependency (icon flattening, previews, blank detection).
- Launch: the app's live stdout/stderr files go to `target/icm/sessions/ios-sim/<run>/`, not the run directory, so pruning never removes files the app still writes; `run` copies a snapshot (and `app.log`, `logs.ndjson`) into its run directory. The simulator maps `/tmp` and `/private/tmp` to `<device>/data/tmp`, so for projects there the session records the remapped paths. The collector is `simctl spawn <udid> log stream --level debug --style ndjson --predicate 'process == "<exe>" AND (subsystem == "iced" OR messageType == error OR messageType == fault)'`, detached, killed by `stop` or the next `run` (its group, and only while the recorded pid's command line still holds `spawn <udid> log stream`, so a reused pid is never signalled).
- Ready: `ICM_EVENT ready` in stderr; the app's pid is a host process, so liveness is `kill(pid, 0)`. Without `ICM_EVENT start` after 5 s, the probe takes over: three consecutive `launchctl list` polls showing the app, then a non-blank screenshot. `ready.ms` counts from `simctl launch`. The scale comes from the event, else the device type's `capabilities.plist` (`main-screen-scale`).
- Death: the panic (ICM_EVENT `panic`, else Rust's `panicked at` lines) gives `run.app_panicked`; crash reports are matched by pid or simulator UDID (every template app's executable is `app`, so desktop crash reports share the name) and written up to ~10 s late, so `run` waits 2 s after a panic and 15 s without one; `icm logs ios-sim --source crash` finds later ones. `run --attach` releases the platform lock before it streams (so `icm stop ios-sim` works meanwhile) and ends ok when that stop marked the session stopped; any other exit goes through the same death report (panic, crash report, system log; exit 10). `system.ndjson` is `log show` of the app's process and of messages naming its id.
- `icm logs ios-sim` records: `stderr`/`stdout` (no time), `oslog` (the collector; iced's subsystem levels are mapped back from the oslog crate's), `system` (live `log show` of errors and faults about the app; left out unless `--source system|all`, because on a healthy app it is mostly other processes' errors that mention it), `crash`. Errors Apple frameworks log in the app's process for routine events (Metal's "Compilation succeeded" with shader warnings, "fopen failed for data file") are rated by their text: `debug`, or `warn` for a "Warning:" message. Without `--follow` they are `log` events (`LOG` lines) and the result's `records`; `--grep` takes `|`-separated case-insensitive substrings. `icm input ios-sim` does `appearance`, `font-scale` (content size), `background` and `foreground` through simctl; touches, text and keys exit 2 `input.unsupported` (AXe is cut, item 30).

### Android (`cli/src/android/`, §9.4, §10.4; verified with the template on an android-36 arm64 emulator)

- The managed AVD is `icm-api<target_sdk>` (not `icm_api…`), made from the host ABI's image (arm64-v8a on Apple Silicon, x86_64 elsewhere; `google_apis` preferred), device profile `pixel_9`. `run` creates it when it is missing and the image is installed, as `doctor android --fix` does. icm creates only AVDs named `icm-*`; `stop --shutdown` stops the emulators it booted for the project (recorded per serial in `target/icm/sessions/android-booted/<serial>.json` while the emulator process lives, so a rerun on the running emulator or a plain `icm stop android` does not forget them) and otherwise only `icm-*` AVDs that are not `icm-test-*` (a test run's own) and that icm did not boot for another project; the session's emulator it leaves running gets an INFO saying so. A project's records live in its own target directory, which another project cannot read, so once an emulator it booted is up, `run` also sets the device property `debug.icm.booted_by` to the first 16 hex digits of the SHA-256 of the project's sessions directory: another project's `stop --shutdown` leaves that emulator running (INFO `android.emulator.shared`), and its `run`, which may still pick the running managed emulator, says with the same INFO that the two apps share it. An emulator with no such property (booted by hand or by an older icm) is shut down as before. `--avd <name>` boots any existing AVD.
- Device order: `--device`, then `$ANDROID_SERIAL` (adb's own variable, the way to pick a device for `shot`, `logs` and `input`), host.toml `android.device`, `--avd`, a running emulator of exactly the configured AVD (host.toml `android.avd`, else `icm-api<target_sdk>`), the single online device, else boot the configured AVD. The emulator boots while the app builds.
- Emulators: `-no-boot-anim -no-audio -no-snapshot-save -no-metrics -skip-adb-auth -no-window -gpu swiftshader_indirect`. Measured here (emulator 37.1): `swiftshader_indirect` and `host` both render the template and send `ICM_EVENT ready` in under a second, but the `-gpu host -no-window` emulator stopped answering `adb shell` after about ten minutes (on a heavily loaded host, so not conclusive). SwiftShader needs no GPU and draws the same on every host and CI runner, so it stays the default; host.toml `android.emulator_gpu` overrides it. A device listed online that does not answer `adb shell` is `android.device.none` with that wording. A cold boot took 24 s. Boot ends at `sys.boot_completed=1` and `pm path android` answering; a dead emulator (reaped with `waitpid`) fails at once with `android.emulator.failed`.
- Build: `cargo rustc --lib --crate-type cdylib --target <triple>` with the NDK env and the Android child env; no extra link arguments (NDK r28+ already aligns to 16 KB; `android.so.align16k` FAIL is non-blocking in dev). `llvm-strip --strip-debug`, generated res (stamped), `aapt2 compile`/`link --debug-mode`, a stored ZIP, `zipalign -f -P 16 4`, `apksigner sign --v4-signing-enabled false` (no `.idsig`, so `adb install` stays a plain install), then `apksigner verify` and `zipalign -c -P 16 4`. The APK is `target/icm/build/android/<profile>/<package>.apk`.
- `values/themes.xml` (§9.4) also sets `android:windowLightStatusBar` and `android:windowLightNavigationBar` to whether `[app] background` is light (its WCAG contrast with black beats its contrast with white). From targetSdk 35 the app draws behind transparent system bars; with only the dark parent theme the bars' icons were white, and on the template's white background the clock and battery were invisible. The navigation bar's flag is API 27; older devices ignore it.
- **Dark mode.** The framework follows the system's dark mode on Android: an app without `.theme(..)` draws iced's `Theme::Dark` there, on `#2B2D31`. With the white template background, the theme's dark bar icons would sit on that dark UI. So `IcmTheme` reads the window background (`@color/icm_window_background`) and the icons' flag (`@bool/icm_light_bars`) from `values/window.xml`, and `values-night/window.xml` gives them dark-mode values: a light `[app] background` becomes `#2B2D31` with white icons; a dark one stays as it is. Android resolves them when it creates the Activity, so the launch window matches the first frame in both modes. `icm_background` (the adaptive icon's background) has no night value: the launcher icon keeps its colours. An app that draws a light UI in dark mode (it forces a light theme) declares its own `IcmTheme` in `[android] res`, which replaces the generated one in both modes. The flags are not re-read on a dark-mode switch while the app runs (the manifest's `configChanges` keeps the Activity), so the icons keep the launch mode's colour until the next launch; the framework cannot change them itself: `WindowInsetsController.setSystemBarsAppearance` must run on the UI thread, which a NativeActivity app reaches only through Java code. The generator stamp is `icm-android-res/3`.
- The debug keystore is `<host.toml dir>/android/debug.keystore` (`~/.config/icm/android/debug.keystore`; PKCS12, alias `androiddebugkey`, password `android`), created once with keytool. `~/.android/debug.keystore` is never touched.
- Install is `adb install -r -d`; `--reinstall` uninstalls keeping data (`pm uninstall -k`), `--reinstall --wipe-data` wipes it. Every run sets `debug.icm.events 1` and sets `debug.iced.backend` from `--env ICED_BACKEND=…` or clears it; other `--env` keys are a WARN (apps get no environment). `--from-aab` installs the newest release's bundle instead (see "Android release" below).
- Ready: `logcat -d -v threadtime,epoch -T <mark> -s ICM_EVENT:I` every 0.5 s; a `panic` event is `run.app_panicked`; without a `start` event after 6 s, three polls with the app alive and the top resumed activity are `ready.source = "probe"`. A failed run still writes `logcat.txt`, `logs.ndjson` and `app.log` and attaches the panic line, an `ANR in` (`run.anr`) and the §13.4 signatures.
- Recreation (A3): every run writes the events buffer since the mark to `events.txt`. A `wm_relaunch_resume_activity`/`wm_relaunch_activity` (`am_*` before API 29) of the app's activity is `run.activity_recreated`, with the change mask named as `configChanges` names it and compared with the manifest icm generates (`assetsPaths` below target_sdk 36, a stale APK, or an unlisted change). The framework vendors winit with the Android destroy fix (`vendor/winit`), so a relaunch ends the application and the new activity starts it again in the same process, with its own `start` and `ready`: when a new `ICM_EVENT start` from the app follows the last relaunch within 10 s, the check is a WARN (the app lost its in-memory state). Otherwise it is a FAIL (exit 1 when nothing else fails) saying the app did not start over: a framework from before the fix freezes once Android recreates its activity (its winit does not end the event loop), and an app that sends no `ICM_EVENT` gets no wait. When the app's `Cargo.lock` has no winit from iced's own source (`deps::winit_outside_iced`, e.g. winit from crates.io), the detail names that winit and the fix says to update the pin, without waiting. The wait polls for relaunches too: the probe stops counting, and 10 s after a relaunch without `ICM_EVENT ready` the run fails `run.not_ready` at once, its likely cause, fix and first evidence the relaunch's. Measured on a fresh android-36 emulator: SystemUI applies its theme overlays (`com.android.systemui-*.frro`) during the first boots, and Android relaunches every activity whose `configChanges` lacks `assetsPaths` (`wm_relaunch_resume_activity … 80000000`); a launch that met it froze on the framework from before the fix. The manifest lists `assetsPaths` from API 36 (aapt2 knows the name from that android.jar); with it the same overlay change mid-launch leaves the app running (`settings put secure theme_customization_overlay_packages …` reproduces it).
- `run --attach` streams logcat until the app's process is gone, the app ends with its activity, Ctrl-C or `--timeout`; the app's exit decides the result, as on the desktop: an `am force-stop` (ActivityManager's `Force stopping <id>`: `icm stop android`) is ok, any other exit `run.app_died` or, with a panic in its logcat, `run.app_panicked` (10). An `ICM_EVENT exit` with `destroyed: true` and no new `start` within 3 s, with no activity of the app left, is the app ending with its destroyed activity (Back at its root) while the process lives on, cached: ok, like closing the last window on the desktop. A relaunch sends its `start` well within that and goes on streaming. The run's summary names the app, the device, its AVD or model and API level, and the readiness ("… is running on emulator-5580 (icm-api36, API 36); first frame … after 0.4 s (source: icm_event)").
- `logs` re-queries `logcat -d` (main, system, crash) from the session's mark or `--since <dur>`; `--grep` is the shared matcher (`|`-separated case-insensitive substrings). `shot` WARNs `run.app_died` when the app is not running (the screenshot shows the launcher), and `input` touches and keys fail `run.app_died` (exit 10; `run.no_session`, 7, without a session) instead of driving the launcher. Not running includes a process without an activity: the framework ends the app with its activity, and after Back at the app's root Android destroys the activity but keeps the process cached, so a live `pidof` alone would send the input to the launcher. Both read `dumpsys activity activities` (an `ActivityRecord` of the app; `topResumedActivity` for the one in front), and `shot`'s `process` object carries `activity` and `front`. `input` uses the last screenshot's geometry from `sessions/android.json` (schema `icm.session.android/1`; the app's device pid is `app_pid`), else `dumpsys window displays` (`cur=`) and `wm density`; `text` is printable ASCII.
- New ids: `android.emulator.failed` and `android.launch_failed` (both exit 7). `doctor android` is the generic doctor (item 18): it reports and fixes the SDK pieces (`--fix --yes` runs `sdkmanager --install` only when the licences are already accepted) and creates the managed AVD from the installed image `run` would pick (google_apis first).
- The CLI depends on `png` for previews, blank detection and launcher icons.

### Android release and the lifecycle suite (phase 3; `cli/src/release/android.rs`, `cli/src/android/bundle.rs`, `cli/src/android/lifecycle.rs`; §11.2, §12.3, §13.5)

Verified on 2026-10-07 with the template (`icm new demo --id dev.accept.demo`, a 1024 px test icon), bundletool 1.18.3, JDK 21, NDK r29, build-tools 36 and the managed `icm-api36` emulator (arm64), with a throwaway PKCS12 upload key whose password came from `ICM_TEST_STOREPASS`.

- **Preconditions** (before any build). `[android] target_sdk` at or above the policy's `play.target_sdk` (else FAIL `android.manifest.target_sdk`, exit 1), arm64-v8a among `[android] abis` (`android.so.abis`), a JDK 17+, an NDK r28+, build-tools 35+, the platform's android.jar, every ABI's Rust target, and bundletool through the pinned-tool table (`--yes` downloads it). Their versions go into `artifacts.json` `tools`.
- **Libraries.** One `cargo rustc --lib --crate-type cdylib --release --locked` per `[android] abis` entry (the template: arm64-v8a and x86_64) in `target/icm/release-target` with the release profile (thin LTO, line tables). The unstripped library goes into `native-debug-symbols.zip` as `<abi>/lib<lib>.so` (role `symbols`); `llvm-strip --strip-unneeded` makes the shipped one (template: 66 MB unstripped, 10.5 MB stripped, for arm64). The symbols zip is written by icm's own stored-ZIP writer, so it is uncompressed (130 MB for the template's two ABIs).
- **The bundle.** Intermediates live in `target/icm/gen/android/release/bundle/`, apart from a dev release build's `gen/android/release/`. The manifest and resources are the dev build's (§9.4), linked with `aapt2 link --proto-format` and never `--debug-mode`; `base.zip` (icm's writer: `manifest/AndroidManifest.xml`, `resources.pb`, `res/`, `lib/<abi>/`, `assets/` with `THIRD_PARTY_NOTICES.txt` and `[app] resources`, no `dex/`), `BundleConfig.json` (`PAGE_ALIGNMENT_16K`), `bundletool build-bundle` and `validate` (`android.aab.validate`). bundletool deflates every entry of the `.aab`, native libraries included.
- **Signing.** Under `--sign auto`, with the keystore present and its variables set (the core reports what is missing): `keytool -J-Duser.language=en -list -v -keystore <ks> -alias <a> -storepass:env <VAR>` names the key's algorithm (`Subject Public Key Algorithm`: RSA, EC or DSA picks `-sigalg SHA256with…`) and certificate, then `jarsigner … -storepass:env -keypass:env … -signedjar <dist>/<package>-<version>-<build>.aab`. A wrong password, an unknown alias or a missing variable (from keytool's or jarsigner's output) is `android.keystore.unreadable` (new, exit 9, owner), deferred like the core's own signing items: the dist gets `<package>-<version>-<build>-unsigned.aab`, WARN `android.aab.unsigned`, and the owner's plan starts with the jarsigner line. Verification parses `jarsigner -verify -verbose -certs` (`jar verified.`, not `jar is unsigned.`; an unsigned jar exits 0) and compares `keytool -printcert -jarfile` with the key's SHA-256, which `artifacts.json` keeps in `signing.certificate_sha256` for `icm verify`. With JDK 21 here `jarsigner -verify -strict` exits 0 on the self-signed key (the design's exit 4 is older JDKs'); icm does not use `-strict` either way. The missing timestamp is INFO.
- **Gates on the linked bundle** (`gate_bundle`, shared with `icm verify android`). `bundletool dump manifest` prints `configChanges` as a hex mask (`0xd000ffff` for the API 36 list) and `launchMode` as a number, so `android.manifest.config_changes` compares bit masks with the list icm generates for the dumped targetSdk. Then `hasCode="false"` ⇔ no `dex/`, no `debuggable`, `android.app.lib_name` equal to the library of every ABI directory, versionCode = `[app] build` (at most 2,100,000,000) and versionName = the Cargo version, WARN `android.manifest.back_optout` for `enableOnBackInvokedCallback="false"`, WARN `android.permissions.review` listing dangerous permissions (INTERNET alone passes), `android.so.abis` (arm64-v8a required, a declared ABI missing fails, no x86_64 WARNs), and `dump config` showing `PAGE_ALIGNMENT_16K`. The libraries are extracted from the bundle with the JDK's `jar xf` (they are deflated there) for the ELF gates: machine of the ABI directory, `ANativeActivity_onCreate` exported, every `PT_LOAD` aligned to 16 KB, and no `ICM_AGENT_BRIDGE_V1` marker (`store.no_agent_bridge`). The dumps are kept in the run directory as the evidence.
- **Listing and metadata.** `play-icon-512.png` (`[app] icon` flattened onto `[app] background` at 512 px; role `listing`) and a copy of `AndroidManifest.xml` (role `metadata`).
- **`--apk`.** bundletool always signs what it builds (without `--ks` it reads `~/.android/debug.keystore`), so `bundletool build-apks --mode=universal --output-format=directory` signs with icm's debug key, and `apksigner sign --ks-pass env:<VAR> --key-pass env:<VAR> --v4-signing-enabled false` then replaces that signature with the upload key's (one signer left, 16 KB alignment kept): `<stem>-universal.apk`, role `sideload`, with `apksigner verify` and `zipalign -c -P 16 4`. An unsigned release keeps the debug-signed `<stem>-universal-debugkey.apk`, for testing only. Verified: the upload-key APK installs on the emulator with `adb install`, is not debuggable and sends `ICM_EVENT ready`.
- **Smoke install** (unless `--no-smoke`; SKIP `android.smoke`, new, otherwise). Only on a device the owner chose (`$ANDROID_SERIAL`, host.toml `android.device`) or a running emulator of icm's AVD: never the single online device, and the release never boots one. `bundletool build-apks --connected-device --device-id <serial> --adb <adb>` with icm's debug keystore (so it installs over dev builds without a signature clash), `bundletool install-apks --allow-downgrade`, then `setprop debug.icm.events 1`, launch, `ICM_EVENT ready` and `smoke.png` as `icm run` does. A failure is a FAIL with the run's evidence (not uploadable); the release still finishes. Measured: the template's release build drew 0.23 s after launch.
- **`icm run android --from-aab`** installs the `.aab` of `dist/latest/android` the same way instead of building (`release.not_found`, exit 2, when there is none; `android.so.abis` when it lacks the device's ABI; `--reinstall` uninstalls first), then runs as usual; the result reports the bundle as `artifacts.aab`.
- **The owner's plan.** For the first Android release `UPLOAD.md` shows the Play Console steps (create the app, upload the bundle to Internal testing by hand, the listing with the closed test of 12 testers for 14 days that personal accounts created after 2023-11-13 need, the native debug symbols) and, below them, the commands later releases run (`fastlane supply … --release_status draft`, `icm diagnose play`, `icm ledger mark-uploaded`); `upload.sh` still exits 9 for the first release. §11.2 printed only the checklist: showing the later commands once costs nothing and saves the owner a second look. A once-step gives the Play Developer API alternative (`edits` insert, `bundles?uploadType=media`, `tracks/<track>`, `:commit` with `curl`); its token step (`gcloud auth print-access-token` for the service account) is still unverified.
- **`icm diagnose play <file|->`** reads fastlane's output (`upload.sh` saves it as `supply.log`) or a Play Developer API error body: a version code already used is `version.build_not_increased`; "Package not found" `android.play.app_missing` (new, 9: the first upload is manual); the caller's permission, `PERMISSION_DENIED`, `invalid_grant`, `UNAUTHENTICATED` or a disabled API `android.play.permission` (new, 9); the wrong upload key `android.play.wrong_key` (new, 9); unsigned `android.aab.signed`; the target API level `android.manifest.target_sdk`; 16 KB `android.so.align16k`; any other error line `android.play.rejected` (new, 1). Each finding is a FAIL with the file and line as evidence; "Successfully finished the upload" with no error is ok (`diagnosis.uploaded`).
- **`icm verify android`** takes an `.aab` (anything else is `usage.bad_args`), runs `validate`, the signature checks (the unsigned bundle of a `--sign none` release, or of a `--sign auto` release whose `artifacts.json` records `signed: false` because the owner's upload key was missing, is WARN `android.aab.unsigned` by the owner, so the `icm verify` that the release's exit 9 suggests next does not hand the key to the agent with an exit 1; an unsigned one built elsewhere is FAIL `android.aab.signed`) and `gate_bundle`, and keeps a run directory for its dumps (the project's, else icm's cache).
- **The lifecycle suite** (`icm test --on android --lifecycle`, also `icm test android --lifecycle`). It launches the app as `icm run android` does (same device choice, build, install, readiness, session), reads the settings it will change, then runs `dark-mode` and `light-mode` (`cmd uimode night yes|no`), `landscape` and `portrait` (auto-rotate off, `user_rotation 1|0`), `font-scale` (1.3), `font-weight` (`font_weight_adjustment 300`, API 31+), `home`, `home-relaunch` (`am start -W -n`, never `-S`), `back`, `back-relaunch`, `kill` (Home, then `am kill`) and `kill-relaunch`, one `test.lifecycle` check each (new id, exit 1), and restores the settings at the end whatever happened. A step fails on a `wm_relaunch_*` or `wm_destroy_activity` (`am_*` before API 29) of the app's activity where none is due, `ANR in <id>`, a panic or a crash, a changed pid where the process must stay, a new `ICM_EVENT start` where the app must carry on, no `ICM_EVENT ready` within 30 s where it must start over, or a blank screenshot (FLAG_SECURE windows exempt). With `[android] back = "system"` Back must end the app with its activity (or Android may move it behind) and the relaunch must start over; with `"key"` the app must stay. A kill Android refuses is SKIP. After a failed step that leaves the app elsewhere the suite brings it back to front, and stops when it cannot. The project's `[checks] android` scripts run after every step that leaves the app in front, with `ICM_LIFECYCLE_STEP`. The result's `lifecycle` lists the steps with their pid and screenshot (`screen-<step>.png`), and the session records the app's pid after the kill. Unlike host `icm test`, the suite honours `--dry-run` (it touches a device). Measured on `icm-api36`: all twelve steps passed in 47 s; configuration changes and Home kept pid and activity with no event; Back finished and destroyed the activity (`wm_destroy_activity … finish-imm`, `ICM_EVENT exit` with `destroyed: true`) and the relaunch started over in the same pid; `am kill` after Home logged `am_kill … kill background` and the relaunch ran in a new process; the portrait-locked template's window stayed portrait at `user_rotation 1`. A frozen app that keeps showing its last frame passes the frame check: screenshots cannot tell it from an app with nothing to redraw.
- **New ids:** `android.keystore.unreadable`, `android.smoke`, `android.play.rejected`, `android.play.app_missing`, `android.play.permission`, `android.play.wrong_key`, `test.lifecycle`.
- **Tests:** `cli/tests/android_release.rs` runs the release, verify and diagnose against fake cargo, aapt2, bundletool (`java -jar`), jar, keytool, jarsigner, apksigner and zipalign that record their argv and write real zips with `zip`/`unzip` (skipped without them): the unsigned and signed paths, the argv (`-storepass:env`, no `-strict`, `env:` for apksigner), that the password is written nowhere, the owner items for a wrong password and an unset variable, the gates on a bad manifest and a low targetSdk, and Google Play's answers. The suite's steps and settings restore are unit tests; the suite itself and the smoke install need a device. `cli/tests/accept/phase3.sh` runs §18's phase 3 acceptance on this Mac (16 steps passed on 2026-10-07; the owner's Play Console uploads and the ios-sim suite, which another phase brings, are SKIPs), and `.github/workflows/icm-android.yml` is §17's Linux KVM job, which has not run because Actions is not enabled on the fork.

### The web platform (phase 1.6; `cli/src/web/`)

- **Day-0 spike (Appendix C item 28), done 2026-10-06.** `examples/app` builds for `wasm32-unknown-unknown` with no `web_sys_unstable_apis`; wasm-bindgen 0.2.106 (the root lock's) turns the 245 MB debug `.wasm` into a 43 MB `app_bg.wasm`; served on loopback, headless Chrome 154 draws the app (not blank) and logs `ICM_EVENT ready` about 1 to 2.5 s after load. Chrome exposes `navigator.gpu` but has no adapter ("No available adapters"); iced's `new_instance_with_webgpu_detection` then picks WebGL2 (ANGLE on SwiftShader, `api: "Gl"`), so no backend change was needed for apps with iced's `webgl` feature (the template has it). Without `webgl` the app panics there: wgpu takes the canvas for WebGPU, finds no adapter, and iced's tiny-skia fallback fails to create its softbuffer surface on that canvas ("A canvas context other than `CanvasRenderingContext2d` was already created"); `web.renderer_fallback` WARNs before the run, and the panic's `likely_causes` names the missing feature. `web.fonts_embedded` WARNs in dev (a release FAILs) when `fira-sans` is off for wasm32. Both read iced's resolved features from `cargo metadata --offline --filter-platform wasm32-unknown-unknown`.
- **Phone viewports need a real device scale factor.** With only `Emulation.setDeviceMetricsOverride`'s `deviceScaleFactor`, Chrome reports `devicePixelContentBoxSize` at the real ratio (1 when headless) while `devicePixelRatio` is 3, so winit sized the canvas in CSS pixels and iced drew everything three times too large. The session starts Chrome with `--force-device-scale-factor=<scale>` as well.
- **Build.** A plain `cargo build -p <pkg> --bin <bin> --target wasm32-unknown-unknown` (no extra `--config`, so raw cargo builds stay fresh), `[web] rustflags` in `CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS` when set; `wasm-bindgen --target web --no-typescript --out-name app [--debug]` (no `--keep-debug`: the DWARF makes the page slow to load). The wasm-bindgen CLI is checked before the build when the lock exists, else after it. A failed build whose diagnostics name getrandom's missing web backend becomes `deps.getrandom_backend` (exit 3) with the lines to add; a static lockfile check would fire on every app, because getrandom 0.3 is in the template's lock without being built for wasm32.
- **Site.** `target/icm/build/web/<profile>/site/`: `index.html` (§9.5, plus `outline:none` on the canvas and an empty icon when `[app] icon` is unset, so Chrome logs no favicon 404), `manifest.webmanifest`, `icon.png`, and `[app] resources` copied with their relative paths (`*`, `?` and `**` globs inside the project). Fonts are not files: they are in the `.wasm`.
- **Session.** `icm __session web --request <file>` (a view: no run directory) is started with `setsid` and replaces this project's running web session first (item 13); a busy port is then someone else's: `web.port_busy` (7), `--port 0` picks a free one. The record is `target/icm/sessions/web.json` (mode 0600: it holds the control token); its live files are in `target/icm/sessions/web/` (`console.ndjson`, `chrome.log`, `session.log`, `chrome-profile/`), outside the run directories that `prune` removes. Startup is a handshake through `startup.json`. Chrome runs in the session's process group; `icm stop` asks the session over its control channel, then signals the group. Every record carries a `marker` that the pid's command line (`ps -o command=`) must contain before icm signals it, so a reused pid is never killed.
- **Control channel.** `POST /__icm/control` on the session's own port with `X-Icm-Token`; ops `status`, `screenshot` (the session writes the PNG where asked), `tap`, `swipe`, `text`, `key`, `appearance`, `rotate`, `stop`, and `eval` (diagnostics only). A POST with an `Origin` other than the server's own is refused, so another site in the user's browser cannot write to the console log. The page loads with `?icm_events=1&icm_cdp=1`; results report `?icm_events=1`, where the page's dev script forwards the console to `POST /__icm/log` (`--show`, a system browser).
- **Readiness.** `ICM_EVENT ready` (`source: icm_event`, `ms_since_launch` from `Page.navigate`); an `ICM_EVENT panic` before or during `--settle` is `run.app_panicked` (10); an error record before ready is `run.app_died` (10); nothing is `run.not_ready` (10); with no `ICM_EVENT start` at all, a sized canvas seen twice after 5 s is `source: probe`. A failed run copies the console into its run directory, points the evidence there, and leaves the session running for `icm logs web` and `icm shot web`.
- **Input.** Desktop and `WxH` viewports get mouse events; phone presets get touch emulation and `Input.dispatchTouchEvent`. `text` sends one `keyDown` with `text` per character (winit reads `keydown`); `key` takes enter, tab and escape; `back`, `home`, `font-scale`, `background` and `foreground` are `input.unsupported` (2); `appearance` emulates `prefers-color-scheme`; `rotate` swaps the viewport's edges.
- **Logs.** Records `{ts, epoch_ms, platform, source, level, tag, pid, msg}`; sources `console`, `exception` and `forwarder` count as `--source app`, `browser` as `system`, `crash` as `crash`. `icm logs web` emits `log` events (human `LOG` lines) and also returns the tailed `records` in the result, so `--json -q` carries them. `--grep` is the shared matcher (`|`-separated case-insensitive substrings; the CLI has no regex dependency). SwiftShader's "GPU stall due to ReadPixels" (each screenshot) and WebGPU's "No available adapters" are logged at `debug`. After the session ends, `logs` still reads its console.
- **`ps`, `stop`.** `ps` is a content command and a view: it prints one line per session (human mode) and works outside a project (no sessions). `stop <platform>` with nothing running exits 0 ("nothing was running"), so `icm stop --all` is always safe cleanup. `stop` asks the web session to end over its control channel (then signals it); the other platforms' sessions are ended as item 21 says, or by their own `stop` (desktop, ios-sim, android). A record with a `marker` counts as running in `ps` only while its pid's command line contains it.
- **Dependencies.** `png` 0.18 (§16.1) for previews and blank detection (`cli/src/preview.rs`, shared by every platform's capture).

### Harness, hooks and signatures (`cli/src/harness/`, `hooks.rs`, `signatures.rs`; §13.2, §13.4, §13.6)

- **Harness driver** (§13.2). icm builds the harness once (`cargo test -p <pkg> --test icm --no-run`, so compiler errors are diagnostics and exit 5) and runs the built executable from the package directory with `ICED_TEST_BACKEND=tiny-skia` (an `ICED_TEST_BACKEND` in icm's environment wins) instead of one `cargo test … -- <command>` per render: no cargo freshness check per command, and up to four renders run at once. Answers: a protocol other than 1, or "unknown command/option" from the harness, is `harness.protocol_mismatch` (4); no `ICM_HARNESS` line (a libtest target, a missing `harness = false`) or no `icm` test target is `harness.missing` (3); exit 2 is `usage.bad_args` (an unknown `--preset`) or `tool.failed` (a file it cannot write); a panic is `run.app_panicked` (10) with the app's source line as evidence; any other crash `run.app_died` (10). The failure signatures are its likely causes.
- **Phone layouts headless.** The harness runs on the host, so `cfg!(target_os)` there is the desktop's. The template pads its root with the safe area, which the harness gives a viewport the size of a phone preset (§13.2), so `shot --headless`, `ui` and `.ice` flows at phone viewports lay out as that phone does: `Increment`'s centre at y 100.4 at `iphone-17` (its row 16 below the 62-point inset, as on the iPhone 17 simulator) and y 92.5 at `pixel-9` (16 below 54.1, as on icm's emulator). Until a safe area arrives (the first frames on a device, unit tests, other viewport sizes) the template's `fallback_padding` picks the phone padding (64 top, 48 bottom) at run time: on iOS and Android, and in any window narrower than 600 logical pixels (a `responsive` root).
- **`shot --headless`.** Writes `target/icm/host/shots/<viewport>-<theme>[-<preset>].png` (or `--out`, `--out-dir`) and a `.preview.png` next to each. Without `--viewport` it renders the first of `[test] viewports`; `--all-viewports` adds all of them. One shot reports `artifacts.screenshot`/`preview` and `screen`; several report `screenshot.<label>`/`preview.<label>`. Every result has `shots[]` (label, paths, size, scale, `screen`, `blank`, `dominant`). A single-colour render is WARN `run.screen_blank` (FAIL under `--strict`). Previews and blank detection use `png` 0.18 (§16.1), in `raster.rs`.
- **`ui`.** `icm ui tree|find|ice` is headless with or without `--headless` (the only mode until phase 6) and is a content command: human mode prints the answer on stdout. `--viewport` (a preset or `WxH[@scale]`, for `tree` and `find`; default the first of `[test] viewports`; `ice` takes the flow's own header) picks the size. `find` takes `#id` (or `id:`), else a text, matched exactly, then as a case-insensitive substring; each match carries `center` in logical pixels; no match is `ui.selector_not_found` (1). The tree and the `.ice` report are files in the run directory (`artifacts.tree`, `artifacts.report`).
- **`test`.** `cargo test --no-run` (build), then `cargo test --no-fail-fast [-- <filter>]`, with the libtest output read back into suites: a `test.passed` check per suite that ran tests and per passing flow, a `test.failed` per failing test (its panic line as evidence) or flow (the `.ice` line), and for a binary that crashed. Flows in a `[test] flows` directory other than `tests/flows` run through `icm-ice`. Without a harness the unit tests still run and `harness.missing` is a WARN. A flow that does not parse (`test.ice_parse`, 3) blocks after everything else is reported. `--on` and `--lifecycle` are `usage.not_implemented` until phase 3.
- **Hooks** (§13.6). `hooks::run(ctx, &HookContext)` runs the `[checks] <platform>` scripts; `run` calls it after a successful launch, and `test --on android --lifecycle` after each step that leaves the app in front. Scripts run from the project directory with stdin closed and 300 s each (the overall `--timeout` still blocks); an executable script runs directly, any other with `/bin/sh`. The environment adds `ICM_PROJECT_DIR`, removes the `ICM_*` variables the context leaves unset, and takes the platform's tool environment from `HookContext.env` (Android: `JAVA_HOME` and `PATH`, item 2). `CHECK PASS|FAIL|WARN|SKIP|INFO <name>: …` lines become `hook.<name>`; a non-zero exit, a timeout or a missing script is FAIL `hook.<script stem>`; all are non-blocking (exit 1). The result gets `hooks[]`. `icm __test hooks <platform>` runs them without a device. Every platform's `run` calls them once the app is ready, screenshotted and alive (`hooks::run_for`; an ios-sim run first looks up `ICM_SIM_DATA` with `simctl get_app_container`, only when there are scripts), before `--attach` starts streaming: desktop passes `ICM_PID`, `ICM_BIN`, `ICM_APP_STDERR`, `ICM_LOGS`, `ICM_LOG_MARK`; ios-sim adds `ICM_DEVICE` (the UDID) and `ICM_SIM_DATA`; android passes the device pid, `ICM_DEVICE` (the serial), `ICM_ADB`, the APK as `ICM_BIN` and the JDK/SDK environment; web passes the session host's pid, the console as `ICM_LOGS` and the page as `ICM_URL`.
- **Signatures** (§13.4). `signatures::annotate(error, text, &Facts)` adds `likely_causes` from text (stderr, logcat, the system log, tool output) and facts (a blank screen of a live app; `FLAG_SECURE`). It knows the scene manifest, RecreationAttempt and a second `android_main`, a missing AndroidApp, the Android activity features, the display-server floor, `lib_name`, the install failures, ANR and destroyed activities, GPU adapter or surface refusal (per platform: `ICED_BACKEND`, `debug.iced.backend`, `ICED_TEST_BACKEND`), code signing, the web MIME type and wasm-bindgen skew, `font.default_missing`, and panics (`first_panic` reads `thread 'main' (id) panicked at …` of Rust 1.98, the older forms and `ICM_EVENT` panic events).
- **Ids added to §5.** `test.passed` (PASS) and `ui.selector_not_found` (1).

### CI (`.github/workflows/`, `.github/ci/`; §17, Appendix C item 29)

- **Two workflows for every change**, on pushes to `main`, `v*-mobile.*` tags, pull requests and manual runs, with read-only permissions and no secrets. `framework.yml`: `fmt` (plain `cargo fmt -- --check`); `check` on macOS (AGENTS.md's four `cargo check -p iced` targets, `examples/app` for the four platforms icm builds it for, `--lib` on Android and `--bin` elsewhere, AGENTS.md's two `iced_winit` clippy lines, and `cargo metadata --locked`); `test` (`ICED_TEST_BACKEND=tiny-skia cargo test --workspace` on macOS, Linux and Windows); `msrv` (`cargo check -p iced` on the workspace's `rust-version`, on macOS, as `phase0.sh` does). `icm.yml`: `cli` (fmt, clippy and test on macOS and Linux; macOS adds wasm32, so `tests/web.rs` drives the image's headless Chrome); `install` (item 10 as `cargo install --locked --git file://<checkout> --rev <sha>`, or `--tag <tag>` on a tag; it asserts that `icm --version` names that commit and that `icm new` pins `rev:<sha>` or `tag:<tag>`; cargo fetches a commit no branch contains, such as a pull request's merge commit, from a `file://` URL); `fresh-resolve` (macOS: `icm new --framework path:`, `icm doctor desktop --fix --yes`, `cargo generate-lockfile` from no lock, `icm check --all`; it keeps the lock and the crate versions the fork's lock lacks, 165 of 409 on 2026-10-07); `release-macos|windows|linux` (item 11's desktop rows, on a new app: `icm release <target> --sign none --allow-dirty --yes` and `icm verify`; macOS adds stage 2 with `--dmg`, `lipo -archs`, the Mach-O minos against `[desktop.macos] min_os`, the DMG mounted read-only with the app and an `Applications` link, and the app's `ICM_EVENT` ready; Windows installs WiX 5.0.2 with its UI extension and NSIS, then installs and uninstalls the `.msi` silently; Linux runs in `ubuntu:22.04` pinned by digest, installs and removes the `.deb` with dpkg, and waits for the AppImage's ready event under Xvfb; each keeps the dist directory, the result objects and the run directories for 14 days); `tag` (on a tag: the tag is `v` plus `cli/Cargo.toml`'s version). Releases stay workflow artifacts; nothing is attached to a GitHub release.
- **Toolchain.** Every job but `msrv` builds with the toolchain `examples/app/rust-toolchain.toml` pins (`.github/ci/toolchain.sh pinned`), so a new stable Rust cannot turn CI red by itself; raising the template's pin moves CI with it.
- **Scripts.** `.github/ci/*.sh` hold the jobs' logic and run on a workstation too (bash 3.2, Git Bash). `cli/tests/ci.rs` runs the tag rule, checks that the workflows run every command AGENTS.md's Checks list and only scripts that exist, and fails when `.github/` has `cargo fmt --all`, a secret or an upload, publish or notarize command.
- **Upstream's workflows** (evidence of 2026-10-07; upstream's own runs at the fork's base `38237dd29` failed Lint, Test and Audit and passed Format, Check and Document). `check.yml` passes on the fork and stays. Removed: `document.yml` (it deploys to iced-rs/docs from `master` with a secret the fork does not have), `build.yml` (`master` only), `lint.yml` (`cargo lint` fails in upstream's `graphics/src/geometry/cache.rs:119`, a lint of the current clippy), `test.yml` (its `RUSTFLAGS=--deny warnings` fails on upstream's `tester/src/lib.rs:908`, a future-incompatibility warning; `framework.yml`'s `test` replaces it), `audit.yml` (`cargo update` then `cargo audit`; upstream's run found 5 vulnerabilities and the committed lock has 7 advisories, all in upstream's dependencies) and `format.yml` (the same command as `framework.yml`'s `fmt`).
- **Per-platform workflows** (§17 item 11, written with each phase; they run when `cli/`, `examples/app/` or, for Android and the web, the framework crates change): `icm-ios.yml` (the iOS unit and fake-tool tests on Linux and macOS, `phase2.sh` on macOS), `icm-android.yml` (the KVM emulator job: doctor, a dev run, the lifecycle suite, an unsigned and a throwaway-key release), `icm-web.yml` (the web dev loop and `icm release web` on Linux with the runner's Chrome) and `icm-desktop.yml` (`phase5.sh` on macOS; in the same `ubuntu:22.04` digest as `release-linux`, the desktop release tests with the real dpkg-deb, a release, `dpkg -i`, the AppImage under Xvfb and `icm verify linux`; on Windows the MSI and NSIS installers installed and uninstalled). `icm-desktop.yml` overlaps `icm.yml`'s `release-*` jobs, which release the same targets through `.github/ci/release.sh`; whether to keep both is the owner's call once they have run.
- **Not run yet.** GitHub Actions has never run on the fork: the owner enables workflows on its Actions tab. Both Windows release jobs (`icm.yml`'s `release-windows`, `icm-desktop.yml`'s `windows`) run only when the repository variable `ICM_WINDOWS_HOST` is `true`, because icm does not build on a Windows host yet.

### Integration (where the branches meet)

- **`icm build --all`** (or `icm build` with no platform) builds every platform in `[app] platforms` in turn (iOS as the simulator, skipped with SKIP `env.unsupported_host` off macOS) through each platform's own `build`, so `icm build --all --detach` prewarms everything (item 24). Every platform is built even after one fails; each failure is a FAIL and the first is `errors[0]`. The result lists `built` and `skipped`.
- **`icm devices [<platform>]`** lists what each platform runs on: the desktop (this machine), the web (the Chrome the session drives), the available iOS simulators (`simctl list -j devices available`, booted and `icm-*` first, host.toml's pinned UDID marked) and Android's devices and AVDs (`icm devices android`, unchanged). Without a platform it lists all of them (the simulators only on macOS), and a platform it cannot list there (no SDK, no Xcode) is a WARN; with one, that failure is the command's error. `ios-device` lists the paired physical devices (macOS only; "iOS devices" below).
- **`icm stop --all`** calls each dev platform's own stop (item 21 above); **hooks** run after every platform's `run` (harness section above).
- **One managed simulator name** for `doctor` and `run` (item 19), and **one debug keystore** for `doctor` and the Android build (item 18).
- **Failure signatures on `run`.** A failed `icm run desktop|web` gets the §13.4 signatures (`signatures.rs`) found in the text files its evidence names, as `likely_causes` (the panic itself stays the platform's own cause). ios-sim and android keep their own signature lists for the system log and logcat, so their causes are not doubled.


### Release core (phases 2 to 5, shared by every target; `cli/src/release/`, `policy.rs`, `pinned.rs`)

- **Store policy table** (§12.0). `cli/policy/stores.toml` (schema 1) is embedded: `reviewed` (2026-10-06), `stale_after_days` (90), `upcoming_days` (60), and `[[rule]]`s with an `id`, the `gates` that enforce it, a `source`, and `values` in date order, each with an optional `effective` date and `extension`. The value in force on a day is the last one whose date has come; an undated value (an API-level rule) is always in force. Gates read floors from it (`policy::get().int("play.target_sdk")`), and `[ios] min_os` validation takes its floor from `app_store.min_deployment`. `icm doctor` (and `icm release`) report PASS or WARN `env.policy_stale` and INFO `store.policy_upcoming` (new id) for each floor that takes effect within 60 days. `icm print policy` prints the table and the value in force today. `ICM_TODAY=YYYY-MM-DD` sets "today" for icm's tests.
- **Pinned tools** (§16.2). `cli/tools.toml` (schema 1, embedded) pins each tool icm downloads itself by version and, per Rust host triple (or `any`), URL, size and sha256: bundletool 1.18.3 (Android releases), binaryen `version_133`'s `wasm-opt` (web), appimagetool 1.9.1 and the AppImage type 2 runtime `20251108` (Linux; appimagetool otherwise downloads the runtime at build time, so its pin keeps the build offline). `pinned::find` takes `ICM_TOOL_<NAME>`, else `<cache>/tools/<name>/<version>/` only when its marker `.icm-pinned.json` records this table's sha256 (nothing on `PATH` counts). `pinned::install` runs `curl -fsSL --proto =https --tlsv1.2` into a staging directory, checks the size and sha256 before unpacking (new id `env.tool_checksum`, exit 4, `doctor-yes`; the download is deleted), unpacks with `tar`, then moves the directory into place and writes the marker last. `pinned::require` installs only with `--yes` and without `--offline`, else `env.tool_missing` naming `icm doctor <platform> --fix --yes`. `icm doctor android|web|desktop` reports the tools that platform's releases need as WARN while missing (the dev loop does not need them, so the exit code is unchanged) and `--fix --yes` installs them (`Fix::Pinned`). `icm print tools` lists them. `ICM_TOOLS_TOML` replaces the table (a mirror, or icm's tests, which serve `file://` URLs). Verified 2026-10-07 on macOS arm64: bundletool and the arm64 macOS `wasm-opt`, downloaded through the mechanism, match their pins, and `wasm-opt --version` prints `version 133`.
- **Signing references and store metadata in icm.toml** (§7.2, Appendix C item 19). New `[store]` table: `privacy_policy_url`, `support_url` (App Store Connect requires both, Google Play the first), `marketing_url`, and `asc_key_id_env` / `asc_issuer_id_env` (default `ASC_KEY_ID` / `ASC_ISSUER_ID`: the variables the printed altool and notarytool commands read). `[desktop.windows] sign_env` lists the variables `sign_command` reads. Validation (exit 3 at `file:line`): every `*_env` key and `sign_env` entry must be an upper-case variable name (`A-Z`, `0-9`, `_`), and a value that is not, being possibly a pasted secret, is never quoted in the detail or the evidence; `sign_command` must contain `{file}` and must not pass a literal to a secret flag (only `$VAR`, `${VAR}`, `%VAR%`, `env:VAR`): flags start with `-` or, as Windows tools write them, `/` followed by letters or digits, and a secret flag's name contains `pass`, `password`, `secret` or `token`, or is AzureSignTool's `-kvs`/`-kvt`/`-kvp` or signtool's password `/p` (`-p` when the command runs signtool); no finding quotes a line that holds a secret (an inline table or `sign_command` keeps several values on one line, so every finding on a secret's line drops its excerpt), and findings about `[android.signing]` keys and `sign_command` never quote their line; `[ios] team_id` is 10 upper-case letters or digits, `asc_app_id` digits, `export_compliance_code` only with `uses_non_exempt_encryption = true`; signing identities are `auto`, a SHA-1 or a name, profiles `auto`, a UUID or a `.mobileprovision` path; the `[store]` URLs are `https://`; Windows and Linux `formats` are known, `glibc_floor` is `X.Y`, `maintainer` is `Name <email>`. host.toml gains `signing_keychain` (overridden by `ICM_KEYCHAIN`): the keychain file Apple signing searches instead of the user's search list, so a CI or test keychain never joins it. `icm explain config.<key>` now reads commented-out tables (`config.android.signing.upload`) and multi-line values.
- **The release command** (`cli/src/release/mod.rs`; §6, §11). `icm release <ios|android|web|macos|windows|linux> [--sign auto|none] [--allow-dirty] [--no-smoke] [--apk] [--dmg] [--universal] [--via-xcode-export]`; a flag of another target is `usage.bad_args`. Each target is a `Pipeline` (`plan`, `preconditions`, `keeps_dist`, `build`, `verify`) around one core: the platform lock `release-<target>`; `version.format` (exit 3, the `[package] version` line as evidence); owner items; `store.metadata_missing` (new, WARN, `by: owner`: `[store] privacy_policy_url`/`support_url` for iOS, the first for Android); the policy table's checks; the pipeline's preconditions; then every owner item blocks at once (exit 9, `owner_steps` lists them all, the summary starts "the owner must act"); then `version.build_not_increased` (exit 1 against the ledger; a WARN under `--sign none`, whose artifacts are not uploadable anyway) then `release.lock_missing` (new, exit 1: the lock `cargo metadata` names must exist, since the build passes `--locked`; the fix is `icm check <ios-device|android|desktop>` or, for the web, `icm doctor web --fix --yes`, then committing it) and `release.dirty_tree` (new, exit 1 unless `--allow-dirty`, then INFO and `source.dirty: true`; a WARN when the project has no commit): changed tracked files, or a `Cargo.lock` that `git ls-files --error-unmatch` says is not tracked (a new app's lock appears after its first commit, and a commit without it cannot rebuild the release). Owner items are a placeholder `[app] id` or icon (a WARN for `web`), and per target: iOS `[ios] team_id` and `uses_non_exempt_encryption` (`config.owner_decision`; `true` without `export_compliance_code` is WARN `ios.export_compliance.documentation`); Android `[android.signing] upload`, its keystore file and its password variables, deferred so the unsigned bundle is still built before exit 9; Windows `sign_command` and every `sign_env` variable (`windows.sign.not_configured`); Linux `[desktop.linux] maintainer` for a `.deb` (`config.owner_decision`, from the pipeline's preconditions). `icm explain config.owner_decision` lists them, and `app.id.placeholder` says the web treats the placeholder as a WARN. `--dry-run` reports the core's steps around the pipeline's and writes nothing; its `release.dist` step says the directory is kept when the pipeline keeps it (macOS `--dmg`). Each of the six targets has its pipeline (the iOS, Android, web and desktop sections below); a pipeline step that is not built yet answers `usage.not_implemented` (exit 2), as `--via-xcode-export` does. `icm __test release|verify <target>` runs the core with a stand-in pipeline (`release/fake.rs`) for icm's tests.
- **Gates under `--sign`** (`release/gates.rs`; §12). A check is owner-dependent when its catalogue entry fails with exit 9. Under `--sign none` it is reported as a WARN ("a WARN under --sign none: …"); under `--sign auto` as a FAIL that ends the command with exit 9, at the next checkpoint (`needs_owner`) or after the artifacts are written (`needs_owner_later`, and owner-dependent FAILs from gates). Every other FAIL keeps its severity. Pipelines report through `Release::check`, never `ctx.rep.check`, so the tally reaches `artifacts.json`.
- **Dist and `artifacts.json`** (`release/dist.rs`, `release/manifest.rs`; §4.6, §4.7). `target/icm/dist/<version>+<build>/<target>/` is emptied before a build (unless the pipeline keeps it, macOS `--dmg`); `dist/latest/<target>` is a relative symlink replaced atomically. Each shipped file is a `files[]` entry `{role, kind, path, bytes, sha256}`, plus `cdhash` for a signed macOS `.app` or `.dmg` (its signature's code directory hash, `codesign -d -vvv`'s `CDHash`) (roles `upload`, `symbols`, `notices`, `listing`, `metadata`, `sideload`, `stage`); a directory's sha256 is that of its sorted `shasum -a 256` listing (symlinks as `link:<target>`). `artifacts.json` adds to §4.7: `icm` (the version line), `sign` (`auto`/`none`, which `verify` reads), `checks.ids_fail`, `notices[]` and structured `owner_steps`. `uploadable` is false when unsigned, when a gate failed, or while owner items remain. The result reports each file under its kind in `artifacts`, plus `manifest`, `upload_md`, `upload_sh` and `dist`, and `release{target, version, build, sign, dist, signed, uploadable, not_uploadable}`.
- **`UPLOAD.md` and `upload.sh`** (`release/upload.rs`, `release/owner_plans.rs`; §11). An owner plan is steps of four kinds (`once`, `web`, `upload`, `after`) whose commands are words of literals, `$VARIABLE` references and `$D/` dist paths, so secrets appear only as variable names or keychain profiles and every variable an upload step reads is declared (a unit test). `upload.sh` is bash with `set -euo pipefail`, `D` = its own directory and `ICM=${ICM:-icm}`; it exits 9 when a declared variable is unset, when the release is not uploadable, or when only the store's web UI can take the upload (Google Play's first release, GitHub Pages); it tees each tool's output into the dist directory and runs `icm diagnose` on it even when the tool failed (the pipeline runs under `set +e` and keeps `PIPESTATUS[0]`; a finding diagnose recognises sets the exit, such as 9 for `ios.asc.auth` or `macos.notary_credentials`, else the tool's own code ends the script), and ends with `icm ledger mark-uploaded <target> --build <n> --config <icm.toml>`. `UPLOAD.md` has the files with their hashes, why the release is not uploadable if it is not, the once and web steps, the listing URLs from `[store]`, the variables and the commands. `owner_plans.rs` is the only file that may contain upload, publish or notarize argv; a unit test scans `cli/src` for it (§17 item 7, since CI is not enabled yet). Plans: iOS altool validate, upload and `--build-status --apple-id` (`[ios] asc_app_id`, else `$ASC_APP_ID`) with `[store] asc_key_id_env`/`asc_issuer_id_env`; Android the manual first release, then `fastlane supply … --release_status draft`, and the keytool and jarsigner lines (`-storepass:env`) for the owner; web per `[web] host` (wrangler, netlify, `aws s3 sync` plus the `application/wasm` copy, a GitHub Pages workflow, rsync to `$WEB_DEPLOY_TARGET`) then `icm verify web --url`; macOS `notarytool store-credentials` once, then submit with `--keychain-profile` (`[desktop.macos] notary_profile`, default `icm-notary`), staple, and `icm release macos --dmg` / `icm verify macos --after-notarize`; Windows and Linux `gh release upload "$RELEASE_TAG" …`.
- **The ledger** (`release/ledger.rs`; Appendix A item 17). `.icm/ledger.toml` (schema 1, `[[upload]]` with `target`, `version`, `build`, `date`, `artifact`, `sha256`, `git_rev`), written only by `icm ledger mark-uploaded <target> [--build <n>] [--force]` from `dist/latest/<target>` (or the newest release with that build; none is a WARN `release.not_found` and an entry without a hash, and the `release.not_found` of a target with no release names `--build <n>` as the owner's fix), once per target and build. A release whose `artifacts.json` says `uploadable: false` cannot have been uploaded, so recording it, `--dry-run` included, is `release.not_uploadable` (new, exit 9, the owner's) with the reason (unsigned, not signed for the store, failed gates, owner items); `--force` records it with a WARN, for a build the owner signed and uploaded by hand. A dry run's summary says nothing was recorded. `sha256` is the upload file's as it is when marked, which is what shipped: a DMG stapled after the release differs from `artifacts.json` (INFO `release.artifact_changed`). `icm ledger show` lists it (a content command).
- **`icm verify`** (`release/verify.rs`). The artifact is `--artifact`, else the first `upload` file of `dist/latest/<target>` (none: `release.not_found`, new, exit 2). Inside a project the run directory is the project's, with `--artifact` and `--url` too; outside one it is in icm's cache. An `artifacts.json` beside it that lists it sets the gates' `--sign` mode and every listed file is checked against its size and sha256 (`release.artifact_changed`, new, exit 1); an artifact of another target is `usage.bad_args`. A pipeline may explain a change (`Pipeline::changed_file`): stapling a notarization ticket changes a macOS app (`Contents/CodeResources`) or DMG (a bigger signature) but not its signature, so a file with a recorded `cdhash` passes when `codesign --verify` still passes, its cdhash is the recorded one and `xcrun stapler validate` passes; otherwise the FAIL says which of the three failed. That is what lets `icm verify macos` run after stage 1's stapling and `--after-notarize` after stage 2's. `--after-notarize` is macOS only, `--url` web only. The target's gates are the pipeline's `verify`.
- **Release builds** (`release/compile.rs`; Appendix C items 4 and 6). `Release::invocation` builds the target's package with `--locked`, `--target-dir target/icm/release-target` (dev and release artifacts never mix) and the target's profile as `--config`: `release` with `lto="thin"` and, for iOS, macOS and Android (which ship symbols), `debug="line-tables-only"` (§11.1's `CARGO_PROFILE_RELEASE_DEBUG`, as a setting); the web's own `icm-web` (`inherits="release"`, `opt-level="z"`, `lto=true`, `codegen-units=1`, `debug=false`). `Release::cargo` puts the deployment target of an Apple triple in cargo's environment, stamps it in `release-target/stamps/deployment-<triple>-<profile>.txt` and, when it changed (or a build exists that icm did not stamp), first runs `cargo clean -p <pkg> --target <t> --release --target-dir <release-target>` (`Ctx::deployment_target_in`); it records `rustc` in the tools. Verified with real cargo on the `checkapp` fixture: the `icm-web` profile builds for wasm32 from `--config` alone, and a macOS build whose `--min-os` went from 12.0 to 13.0 relinked to `minos 13.0`. `icm __test release-build <target> [--min-os X]` runs one such build.
- **THIRD_PARTY_NOTICES** (`release/notices.rs`; Appendix C item 18). `Release::notices(ctx, triple)` runs `cargo metadata --format-version 1 --locked --filter-platform <triple>`, walks the app package's normal dependencies (build and dev dependencies and proc macros never ship; the app itself is left out) and writes `THIRD_PARTY_NOTICES.txt`: Fira Sans and its SIL OFL 1.1 first when it is embedded (iced_graphics with `fira-sans`, or `mobile-fira-sans` on Android and iOS; the `fonts/OFL.txt` beside the framework's iced_graphics, else the copy `build.rs` embeds from `graphics/fonts/OFL.txt`), then every crate with its licence expression and repository, each distinct licence text once with the crates that use it (`license-file`, else `LICENSE*`, `LICENCE*`, `COPYING*`, `NOTICE*`, `UNLICENSE`, `COPYRIGHT*`; a git or path package without one takes the nearest up to its checkout root or the app's workspace root, a registry package never), and the crates that have only an expression. A section of its own lists the Rust standard library every artifact links, which `cargo metadata` does not show: `core`, `alloc`, `std`, `std_detect`, `panic_abort`, `panic_unwind`, `unwind` and `compiler_builtins` (rustc's version), `windows-link` on Windows, and the crates std vendors for the triple (`cfg-if`, `hashbrown`, `rustc-demangle`; `libc`, `addr2line`, `gimli`, `object`, `memchr`, `miniz_oxide`, `adler2` except on MSVC; `dlmalloc` on `wasm32-unknown-unknown`), with their licence expressions. With the toolchain's `rust-src` component (`<sysroot>/lib/rustlib/src/rust/library`) the vendored versions come from its `Cargo.lock` and their texts from `vendor/<name>-<version>/` (`compiler-builtins/LICENSE.txt` for compiler_builtins); otherwise the Rust project's MIT text, Apache-2.0 and the LLVM exception, which icm embeds (`release/licences/`), stand for them. It goes into `gen/<target>/release/` and the dist directory (role `notices`); a crate that declares no licence is WARN `release.licence_unknown` (new). The pipeline puts the file inside its artifacts and records each place with `Release::embed_notices` (`artifacts.json` `notices[]`). The core's gate `release.notices` (new, exit 1): no place recorded is a FAIL; each place is checked inside directories and zip archives (a bounds-checked central-directory reader), and taken as declared (INFO) for containers icm cannot open (`.msi`, `.deb`, `.dmg`). `icm verify` checks the recorded places, or, for an artifact built elsewhere, a `THIRD_PARTY_NOTICES.txt` anywhere inside a zip or directory. The template app's Android notices list 305 crates (about 640 KB of text).
- **`--dry-run` everywhere a device is touched** (§1 principle 1; a bug fix: web and Android ran for real). The web commands (`web/plan.rs`: `build`, `run`, `shot`, `logs`, `input`), the Android ones (`android/plan.rs`: `build`, `run`, `stop`, `shot`, `logs`, `input`, `devices`), ios-sim's `stop`, `shot`, `logs` and `input`, desktop's `logs`, `stop <web|--all>` (a step per session record, read from the records alone), `build --all` (each platform's plan; the result's `plan` now collects every plan a command reports, and `planned` replaces `built`) and `ledger mark-uploaded` report their plan and return before taking a lock, starting adb, a browser or a session, or writing a file. The build steps show the real cargo command (with the NDK environment when the SDK is found; `android::apk::cdylib_cmd` and `web::invocation`/`bindgen_cmd` are shared with the builds); device steps are described, since a dry run picks no device. `check`, `test`, `ui`, `shot --headless` and `verify` still run.
- **`icm upload-commands <target>`** prints `UPLOAD.md` of `dist/latest/<target>` (a content command; the result carries `owner_steps`). **`icm diagnose altool|notarytool|play <file|->`** reads the saved output and hands it to the iOS, macOS or Android pipeline's parser (their sections below).

### iOS release (phase 2; `cli/src/release/ios.rs`, `cli/src/ios/`; §9.1-§9.3, §11.1, §12.2)

The iOS pipeline is no longer a stub: `icm release ios`, `icm verify ios` and `icm diagnose altool` are implemented. Verified on this Mac (Xcode 27.0 27A266a, iOS SDK 27.0) with the template: `--sign none` builds, gates and verifies an ad-hoc `.ipa` (exit 0, owner items as WARNs); a scratch app with a self-signed `Apple Distribution: icm test (ICMTEST001)` identity in a temporary keychain and a CMS-signed fake App Store profile is built, signed with that identity and the distribution entitlements and gated (every gate PASS), then ends with exit 9 because the system does not trust the certificate.

- **Preconditions.** A macOS host (else `env.unsupported_host`); Xcode at or above the policy's `app_store.min_sdk` (`env.xcode_too_old`, an owner item); `ios.xcode.not_beta` is a WARN (Appendix C item 1's rule, `tools::is_beta_xcode`; altool is the real gate); the project toolchain's `aarch64-apple-ios` target (exit 4); the `DT*` keys of that Xcode. Signing: `security find-identity -p codesigning [<keychain>]` (host.toml `signing_keychain` / `ICM_KEYCHAIN`, read only; the "Matching identities" section, so an untrusted identity is listed with its problem) and the profiles in `ICM_PROVISIONING_PROFILES` (`:`-separated, for CI and tests) or Xcode's two directories. `auto` identities are the valid `Apple Distribution`/`iPhone Distribution` ones of `[ios] team_id`; a SHA-1 or a name picks one even when untrusted, and the release then builds and signs with it and ends with exit 9 (`needs_owner_later`, `ios.sign.no_identity` naming `security`'s problem). Profiles are matched on kind (App Store: no device list, not all devices), team, App ID (exact beats wildcard, then the latest expiry), the identity's certificate (SHA-1 of each `DeveloperCertificates` entry) and expiry: under 7 days an owner item (`ios.sign.profile_expired`), under 30 a WARN. Each miss is `ios.sign.no_profile`, `.profile_mismatch` or `.profile_expired` with the reason. A profile is read by taking the XML plist out of its CMS envelope (`ios::plist_xml::embedded`), not with `security cms -D`, so matching runs and is tested on any host; certificate SHA-1 and base64 are icm's own (`ios/sha1.rs`, no new crate).
- **Build and binary gates.** `Release::invocation` + `Release::cargo` with `IPHONEOS_DEPLOYMENT_TARGET` (stamped). The Mach-O gates parse the file directly, like the simulator's (`ios/macho.rs` adds `LC_UUID`, architectures and undefined symbols from `LC_SYMTAB`): `ios.macho.platform` (IOS for every slice; blocking), `.minos` (equal to MinimumOSVersion and at least the policy's `app_store.min_deployment`), `.sdk_floor`, `.arch` (arm64 only), `.sdk_matches_dt`; `store.no_agent_bridge` (the bytes `ICM_AGENT_BRIDGE_V1`). These are non-blocking FAILs: the artifacts are still written, not uploadable, exit 1.
- **Privacy.** `ios.privacy.reasons` scans the undefined symbols and the Objective-C names in the binary's bytes (objc2 looks classes up by name) for FileTimestamp, SystemBootTime, DiskSpace, ActiveKeyboards and UserDefaults (`ios/privacy.rs`). PrivacyInfo.xcprivacy is written from `[ios.privacy]` (icm does not choose reasons for the owner); a used category without a declared reason FAILs, and the fix is the whole `api_reasons = { … }` line with a common reason per missing category (C617.1, 35F9.1, E174.1, 54BD.1, CA92.1) to check against Apple's list. The template's release binary imports `_stat`, `_fstat`, `_fstatat` and `_mach_absolute_time`, which its two declared reasons cover.
- **dSYM** (Appendix C item 23). `xcrun dsymutil <exe> -o gen/ios/release/<Name>.app.dSYM` before the bundled copy is stripped (`xcrun strip -S -x` on a copy). `ios.dsym.uuid` (new) compares the dSYM's DWARF file's `LC_UUID` with the executable's (read directly, not with `dwarfdump --uuid`). `ios.dsym.line_tables` (new) runs `xcrun dwarfdump --debug-line <dSYM> -o <file>` (55 MB of text for the template, 0.3 s) and needs a file entry under the package's `src/`: rustc names a workspace member's files relative to the workspace root, so a relative directory is resolved against `src/`'s ancestors. The zip is `<Name>.app.dSYM.zip` (`zip -qry -X`), role `symbols`, kind `dsym` (the result key §4.4 names).
- **Bundle** (`ios/bundle.rs`). actool for `iphoneos` (`ios_sim::bundle::compile_assets_for`: the same flattened, opaque 1024 px RGB icon and stamp) and the device Info.plist: the simulator's managed keys with `CFBundleSupportedPlatforms = [iPhoneOS]`, `UIDeviceFamily = [1]`, the orientations, the scene manifest, `MinimumOSVersion` = `[ios] min_os`, the `DT*` keys and, for the App Store, `ITSAppUsesNonExemptEncryption` (when answered) and `ITSEncryptionExportComplianceCode`. The `DT*` keys (`ios/dt.rs`) come from one `xcodebuild -version -sdk iphoneos` (`SDKVersion`, `PlatformVersion`, and `ProductBuildVersion` for DTSDKBuild and DTPlatformBuild), DTXcode from the Xcode app's Info.plist, DTXcodeBuild from `xcodebuild -version`, DTCompiler from the iPhoneOS platform's `DefaultProperties.DEFAULT_COMPILER` and BuildMachineOSBuild from `sw_vers -buildVersion`: the values Xcode 27 writes. Then PrivacyInfo, the stripped executable, `[app] resources`, `platform/ios/resources/`, `THIRD_PARTY_NOTICES.txt` and `embedded.mobileprovision` go to the root. Gates: `ios.plist.lint`, `.required_keys` (the device list), `.scene_manifest`, `.ipad_orientations`, `ios.privacy.present`, `ios.plist.dt_keys` (equal to this Xcode's), `ios.plist.export_compliance` (a WARN under `--sign none`, where `config.owner_decision` already warned), `ios.plist.usage_descriptions` (AVCaptureDevice, AVAudioRecorder, LAContext, PHPhotoLibrary and CLLocationManager in the binary's bytes need their NS*UsageDescription), `ios.icon.opaque_1024` (`xcrun assetutil --info`: an `Icon Image` named AppIcon, 1024x1024, `"Opaque": true`).
- **Entitlements and signing.** With `[ios] team_id`, the distribution set (§9.3: `application-identifier`, `com.apple.developer.team-identifier`, `get-task-allow = false`, `beta-reports-active = true`, plus `[ios.entitlements]`) goes into `gen/ios/release/entitlements.plist` and is checked against the profile (`ios.entitlements.not_in_profile`: a `false` boolean needs nothing; strings and arrays match the profile's values with `*` wildcards). Sign last: `xattr -cr`, then `codesign --force --sign <SHA-1> --entitlements <plist> --generate-entitlement-der --timestamp=none [--keychain <kc>] <App.app>` under the watchdog (60 s, or `ICM_CODESIGN_TIMEOUT` seconds; a timeout is `ios.sign.keychain_prompt`, exit 9). `--sign none` signs ad hoc (`--sign -`, with entitlements only when there is a team), so the bundle is sealed and the verify gates still mean something. Then `codesign --verify --strict --deep -vv` (`ios.sign.verify`), `codesign -dvv` for what signed it, and `codesign -d --entitlements - --xml` compared with the plist (`ios.entitlements.get_task_allow`).
- **IPA** (`ios/ipa.rs`). `ditto` into `gen/ios/release/ipa/Payload/`, every file's time set to `SOURCE_DATE_EPOCH` or 1980-01-02 UTC (zip stores local time, and 1980-01-01 UTC is before the zip epoch west of UTC), then `/usr/bin/zip -q -X -y <ipa> <every path, sorted, directories named>` from that directory (`-r Payload` once the names pass 200 KB). An unsigned IPA built twice is byte-identical. `ios.ipa.layout` reads the central directory (only `Payload/<one>.app/…`, no `__MACOSX/`, no `._*`); `ios.ipa.signature` unzips into `gen/ios/release/ipa-check/` and verifies the extracted app. Outputs: `<Name>.ipa` (upload), the dSYM zip (symbols), `Info.plist` and `PrivacyInfo.xcprivacy` (metadata, kinds `info_plist` and `privacy`), THIRD_PARTY_NOTICES (also at `Payload/<Name>.app/`). `artifacts.json` `signing` holds the identity's name and SHA-1, the keychain, what signed it, the profile (`name`, `uuid`, `type`, `team`, `app_id`, `expires`) and the entitlements.
- **`icm verify ios`** unzips into `<target>/icm/gen/ios/verify/` (icm's cache outside a project) and runs the same gates on what it finds: layout, extracted signature, plists, export compliance (a WARN for a `--sign none` release), `ios.version.format` on CFBundleShortVersionString and CFBundleVersion, the `DT*` keys (FAIL when missing; a WARN when they name another Xcode than the host's, which is no fault of the artifact), the Mach-O gates against the plist's MinimumOSVersion and DTSDKName, the bridge, usage descriptions, the bundle's privacy manifest against the binary, the icon, the leaf `Authority` (`ios.sign.no_identity` unless Apple or iPhone Distribution), `get-task-allow`, and `embedded.mobileprovision` (`ios.sign.no_profile` when absent; kind, team from the signature, app id, expiry, signed entitlements ⊆ profile). The release's sign mode applies (§6), so an ad-hoc `--sign none` IPA verifies ok with WARNs.
- **`icm diagnose altool`** reads altool's `--output-format json` (or, failing that, its text). Each `product-errors[]` entry (its `NSLocalizedFailureReason`, else its `message`) maps through its ITMS code to the gate that prevents it (90022/90023/90704/90713/90717 icon, 91053 privacy reasons, 91061 privacy manifest, 90683 usage descriptions, 90474 iPad orientations, 90062/90186/90189 build number, 90060 version format, 90725 SDK floor, 90534 beta toolchain, 90208 minimum OS, 90161 profile, 90045/90046 entitlements, 90034/90035 signature, 90087 architecture), or by its words to the new owner ids `ios.asc.auth` (exit 9: authentication, 401, the private key) and `ios.asc.app_record` (exit 9: no app record for the bundle id), else to `ios.asc.rejected` (new, exit 1) with the message. A success reports `delivery_id` (any `delivery-uuid`) and `build_status`; input that is none of these is `usage.bad_args`.
- **Owner commands.** `owner_plans::ios` keeps validate, upload and `--build-status --apple-id … --wait` (which waits until App Store Connect has processed the build), adds Transporter as the alternative uploader (a comment line in UPLOAD.md; upload.sh skips it), and its listing step names the store screenshot preset.
- **Tests.** Unit tests cover the plist reader, SHA-1 and base64, Mach-O symbols and UUIDs, the privacy classifier, profile decoding and matching (App Store, development, expired, wildcard, another certificate, by UUID and by path), the entitlement subset check, identity parsing, the DT keys, the IPA layout, the dSYM line-table reader, the Mach-O gates and the altool parser. `tests/ios_release.rs` runs the pipeline against fake `cargo`, `xcrun` (actool, dsymutil, dwarfdump, strip, assetutil), `codesign`, `security`, `xcodebuild` and `sw_vers` with the real `plutil`, `ditto`, `zip` and `unzip`: a signed, uploadable release that verifies; an unsigned one that is reproducible; every owner stop (no profile, expired, expiring, a development profile, no identity, an untrusted identity); broken binaries (privacy, bridge, minos, an empty dSYM, a transparent icon, a simulator binary); a beta Xcode; a hanging codesign; a broken IPA; altool output; and the dry run.

### iOS devices (phase 2; `cli/src/platform/ios_device/`; §10.5, §13.1)

`icm build|run|shot|logs|devices ios-device` are implemented through `xcrun devicectl` (devicectl 642, Xcode 27). No physical device or development certificate is available on this Mac, so they are verified with fake tools (`tests/ios_device.rs`) and dry runs; on this host `icm devices ios-device` lists none (devicectl's simulator entries are left out) and `icm run ios-device` stops with `ios.device.not_found` before building. A real `icm build ios-device` of a scratch app with a self-signed `Apple Development` identity in a temporary keychain and a CMS-signed fake development profile built, bundled and signed it (`get-task-allow` true, the profile embedded).

- **Devices.** `devicectl list devices --json-output <tmp>`; physical devices only (`reality` not `simulated`, `visibilityClass` not `simulators`), read from `properties` with the deprecated `hardwareProperties`/`deviceProperties`/`connectionProperties` as fallback. `--device` matches the UDID, the CoreDevice identifier or the name; otherwise the single connected (`connected`, `available` or `tunneled`), paired device. None is `ios.device.not_found` (7, naming the known ones), several `ios.device.ambiguous` (new, 7), `developerModeStatus: disabled` `ios.device.developer_mode_off` (9). `icm devices` lists physical devices on macOS hosts (a WARN when devicectl cannot), and `icm devices ios-device` no longer exits 2.
- **Signing before the build.** `[ios] team_id` (else `config.owner_decision`, 9), the `Apple Development`/`iPhone Developer` identity by `[ios.signing] development.identity` and a development profile by `development.profile` that lists the device (`build` without `--device` takes one for any device), as for releases (`crate::ios`). A named identity the system does not trust is a WARN: the device refuses the app at install. Every miss is an owner error before any cargo step.
- **Build.** `cargo build --target aarch64-apple-ios` (dev profile, or `--release`) with `IPHONEOS_DEPLOYMENT_TARGET` stamped, `ios.macho.platform` (IOS), the device bundle in `target/icm/build/ios-device/<profile>/` (`crate::ios::bundle`: actool for iphoneos, the `DT*` keys, no export-compliance keys, `embedded.mobileprovision`), the plist gates (blocking), the development entitlements (`get-task-allow = true`) checked against the profile (`ios.entitlements.not_in_profile`, 9), `xattr -cr`, codesign under the keychain watchdog, `codesign --verify`.
- **Run.** `devicectl device install app --device <udid> <App.app> --json-output <run>/install.json` (`ios.device.install_failed`, new, 7), then `devicectl device process launch --device <udid> --terminate-existing --console <id>` spawned detached (setsid) with stdout in `target/icm/sessions/ios-device/<run>/console.log`, stderr in `console.stderr`, and `DEVICECTL_CHILD_ICM_EVENTS=1`, `DEVICECTL_CHILD_ICM_RUN_ID`, `DEVICECTL_CHILD_RUST_BACKTRACE=1` plus `--env` values. Ready is `ICM_EVENT ready` in the console; a `panic` event or `panicked at` line is `run.app_panicked` (10, the console line as evidence); devicectl exiting first is `run.app_died`; without a `start` event after 5 s, three consecutive `devicectl device info processes` polls showing `<Name>.app/<bin>` make `ready.source = "probe"`; nothing within `--wait-ready` is `run.not_ready`. Whether `--console` carries stderr (where `ICM_EVENT` goes) is still unverified (§19), which is what the probe is for. After `--settle`, `devicectl device capture screenshot --destination <run>/screen.png`, the preview and blank detection. `app.log` and `logs.ndjson` are the console parsed like desktop stderr (`platform: "ios-device"`).
- **Session.** `target/icm/sessions/ios-device.json` in the generic format (`session.rs`): `pid` is the devicectl console process, `stop` is `xcrun devicectl device process terminate --device <udid> --pid <the app's pid from ICM_EVENT start>`, `device.managed` false; plus `console`, `launched`, `app_pid` and `screen`. `icm stop ios-device` and `icm stop --all` end it through that record (the terminate command, then SIGTERM to devicectl, which forwards it to the app); the next `run` ends the previous console process. `icm shot ios-device [--name] [--out]` captures again; `icm logs ios-device` reads the console (`--level`, `--since`, `--grep`, `--tail`, `--raw`; not the device's unified log). `icm input ios-device` is `input.unsupported` (2). `--dry-run` prints the plan for `build`, `run`, `shot`, `logs` and `stop` without listing a device or reading a keychain.

### App Store screenshots (`cli/src/platform/ios_sim/store.rs`; Appendix C item 19)

- **`icm run ios-sim --store`** runs the app on the managed simulator of the newest `iPhone <n> Pro Max` type the runtime supports (`icm-iphone-18-pro-max-ios-27.0` here: 1320x2868 at 3x), created when missing like the default one; `--sim` and `--device` still win. Without a Pro Max type the run fails `ios.sim.not_found`.
- **`icm shot ios-sim --store [--name <screen>]`** captures as `shot` does, then keeps the capture when App Store Connect takes its size: 6.9-inch (1320x2868, 1290x2796, 1260x2736) or 6.5-inch (1284x2778, 1242x2688), either orientation. It is flattened onto white into an RGB PNG without an alpha channel at `target/icm/store/ios/<name>-<W>x<H>.png` (artifact `store_screenshot`; the result's `store` names the class), so screens taken one by one, with input in between, accumulate in one place for the listing. Any other size is `ios.shot.store_size` (new, exit 7) with `icm run ios-sim --store` as the fix. Verified with the template on this Mac: a 1320x2868 6.9-inch screenshot of its first screen.
- `UPLOAD.md`'s listing step and the template's AGENTS.md name the preset. Google Play's phone-screenshot preset belongs to the Android pipeline.

### Phase 2 acceptance and CI (`cli/tests/accept/phase2.sh`, `.github/workflows/icm-ios.yml`; §17, §18)

- **`phase2.sh`** follows §18's agent acceptance with the template from `icm new … --id com.example.demo --framework path:<fork>`: `icm doctor ios-sim ios-device --fix --yes` and `icm check ios-device` (toolchain, `aarch64-apple-ios`, Cargo.lock); `icm release ios` exits 9 with `fix.by == "owner"`; `--sign none --allow-dirty` is ok with an `.ipa`, `config.owner_decision` among the WARNs, no failed check and PASS events for the dSYM, icon, privacy, DT-key, IPA and notices gates; `zipinfo` shows no `._` or `__MACOSX` entry; `icm verify ios` is ok; the extracted Info.plist's `DTXcodeBuild` equals `xcodebuild -version`'s build, `UIDeviceFamily` is `[1]`, assetutil reports the icon `"Opaque" : true` and PrivacyInfo declares C617.1; a second build is byte-identical; an RGBA icon (`fixtures/icon-rgba-1024.png`) passes `ios.icon.opaque_1024`; dropping SystemBootTime from `api_reasons` fails `ios.privacy.reasons` with the line to add; `[ios] min_os = "12.0"` is refused (exit 3); `diagnose altool` maps ITMS-90717 and reads a delivery id; UPLOAD.md has build-status polling, Transporter and the screenshot preset, and the unsigned release's `upload.sh` exits 9; `run ios-sim --store` and `shot ios-sim --store` give a 6.9-inch RGB screenshot; `devices ios-device`, `run ios-device --dry-run` and `input ios-device` (exit 2). Then the signed path with throwaway material: `test-identity.sh` puts a self-signed `Apple Distribution: icm test (ICMTEST001)` identity in a temporary keychain, and the script writes an App Store profile for `ICMTEST001.dev.accept.ios` holding that certificate (the distribution entitlements, 180 days) in a CMS envelope signed by a throwaway key (`openssl smime`). With the app's id, team, encryption answer and icon set and the identity named by SHA-1, `icm release ios` exits 9 with `ios.sign.no_identity` (`CSSMERR_TP_NOT_TRUSTED`) as its only failed check and every other gate PASS; the IPA's app carries `Authority=Apple Distribution: icm test (ICMTEST001)`, the signed entitlements (`get-task-allow` false, `beta-reports-active` true) and the profile byte for byte, and `icm verify ios` on it is ok. The keychain never joins the search list (checked) and is deleted at the end. The owner's signed release, upload and TestFlight steps and a physical iPhone (`ICM_ACCEPT_DEVICE=1`) are SKIP. Unless they are set, the script points `ICM_KEYCHAIN` at a keychain file that does not exist and `ICM_PROVISIONING_PROFILES` at an empty directory, so icm never lists the user's identities or Xcode's profiles. Run on this Mac on 2026-10-07 (Xcode 27.0 27A266a): 21 steps passed, 3 SKIP (the device run and the two owner steps).
- **`--via-xcode-export`** (§11.1's fallback, §18 step 2.3) exits 2 `usage.not_implemented`, from the plan too: it is built only if App Store Connect refuses an IPA icm built in the owner's first upload, which no agent can run.
- **CI** (not enabled on the fork yet): `icm-ios.yml` runs the iOS modules' unit tests on Ubuntu and macOS (none needs an Apple tool) plus the fake-tool tests on macOS, and `phase2.sh` on a macOS runner after selecting the newest release Xcode (26 or later; older fails the job), with an empty keychain path and profile directory, keeping the dist directory, the store screenshots and the step logs as an artifact. The real-device and signed-upload checks stay the owner's.

### Web release (phase 4; `cli/src/release/web.rs`, `cli/src/web/release_site.rs`, `smoke.rs`, `page.rs`; §9.5, §11.3, §12.4)

- **Preconditions**, before any build: the wasm32 target on the project's toolchain (`env.rust_target_missing`, 4); a `Cargo.lock` naming wasm-bindgen, since a release builds the committed lock (`deps.wasm_bindgen_cli`, 4, `icm doctor web --fix --yes` creates it); the wasm-bindgen CLI of that version; binaryen's `wasm-opt` at its pin through `pinned::require` (downloaded only with `--yes`, else `env.tool_missing`); Chrome or Chromium (`env.chrome_missing`). The web pipeline is implemented: `icm release web` and `icm verify web` no longer exit `usage.not_implemented`.
- **Build.** `cargo build --profile icm-web` from `--config` with `--locked` in `target/icm/release-target` (release core), `[web] rustflags` in `CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS`, getrandom failures mapped as in dev; `wasm-bindgen --target web --no-typescript --out-name app` (no `--debug`) into `gen/web/release/bindgen/`; then `rustc --print cfg --target wasm32-unknown-unknown <[web] rustflags>` in the project directory (so `rust-toolchain.toml` applies), and each `target_feature="x"` becomes `--enable-x` for `wasm-opt -Oz`, renamed where binaryen differs (`nontrapping-fptoint` → `nontrapping-float-to-int`, `simd128` → `simd`, `atomics` → `threads`) and kept only when `wasm-opt --help` lists the flag (a feature without one is left out with a progress line). This settles §11.3's [I]: Rust 1.98's wasm32 defaults (bulk-memory, multivalue, mutable-globals, nontrapping-fptoint, reference-types, sign-ext) map one to one onto binaryen `version_133`, and `artifacts.json` records the flags under `tools.wasm-opt-features`. Only `-Oz`: the bindgen output carries no names or DWARF worth stripping (`--strip-debug` saved 114 bytes on the template).
- **The site** (`dist/<version>+<build>/web/site/`, the first `upload` file, so `verify` and `ledger mark-uploaded` take it): `pkg/app-<h8>.js` and `pkg/app_bg-<h8>.wasm` (`<h8>`: the first 8 hex digits of the file's sha256; wasm-bindgen `snippets/` are copied under `pkg/` unhashed); `index.html` without the dev forwarder, with `<base href>` from `[web] public_url` (a trailing `/` added), `<meta name="description">` from `[app] description`, the icon links (or `data:,`), and `init({ module_or_path })` whose failure is logged as a console error; `404.html`, the same page; `.nojekyll`; `manifest.webmanifest` (`scope` and `start_url` `.`, icons 192, 512 and a maskable 512); icons from `[app] icon`: 32, 192 and 512 px keep alpha, Apple's 180 px touch icon is flattened onto `[app] background`, the maskable icon is the icon at 66 % on the background (the share Android's adaptive icon uses); `[app] resources` (copied first, so a generated file wins a name clash); `THIRD_PARTY_NOTICES.txt` at the root. `_headers` lists exact paths under `public_url`'s path: `X-Content-Type-Options: nosniff` on `*`, `no-cache` on the page, `index.html` and the manifest, `Content-Type: application/wasm` and `public, max-age=31536000, immutable` on the hashed modules; no two rules set the same header on one path, because Netlify and Cloudflare Pages combine such rules differently. Beside the site: `hosting/` (`nginx.conf`, `apache.htaccess`, `Caddyfile`: the same type and caching for servers that do not read `_headers`, role `metadata`), `size.json` (the size report: the `.wasm` before and after wasm-opt, gzip sizes, the JavaScript, the site; also the result's `size`), and `site.zip` (stored, files at the archive root in name order, fixed timestamps; `upload`). A site is not signed: under `--sign auto` the release counts as signed and `signing` is `{kind: "none"}`.
- **Gates** (through `Release::check`): `web.hashed_assets` (the names `index.html` loads exist and carry their content's hash); `web.mime` from `_headers` (a site without one is a WARN), then from the response the page received; `web.size_budget` (the `.wasm`'s `gzip -9 -n` size against `[web] size_budget_kb`; gzip runs as a subprocess, so icm links no deflate crate and §16.1's flate2 is not used); `web.fonts_embedded` (see below); `web.renderer_fallback` (iced's features from `cargo metadata`, as in dev); `web.serve_smoke`. Any FAIL makes the release not uploadable, exit 1.
- **Fonts in the artifact.** Dev keeps its WARN from iced's features; release and verify read the `.wasm`. wasm-opt's memory packing drops runs of zeros from data segments (memory starts zeroed), so a font is contiguous only in the memory the segments initialise: icm lays the active segments out at their offsets (gaps zeroed, passive segments apart) and finds TrueType and OpenType files by their table directory, checked field by field (`searchRange`, `entrySelector`, `rangeShift`, sorted printable tags, `cmap` present, tables in bounds), and reads each family from its `name` table. A text font other than iced's always-embedded `Iced-Icons`, or a `.ttf`/`.otf`/`.woff`/`.woff2` in the site, passes; the template's `.wasm` embeds Fira Sans.
- **The serve check** (`web/smoke.rs`; runs in icm's own process and leaves nothing running). The site is served on `127.0.0.1:0` by the dev server's code at `public_url`'s path: `/` serves the dist site as it is; another path, or an absolute `https://` public_url, serves a copy under `<run>/smoke/root/<path>/` whose `<base href>` points at the local path. Headless Chrome runs with the session's switches, a throwaway profile (removed afterwards) and the Network domain on, loads the page with `?icm_events=1`, and has 60 s (bounded by `--timeout`) for `ICM_EVENT ready`, or after 5 s without `ICM_EVENT start` a canvas with a size (`source: probe`). After a 1 s settle it takes `smoke/screen.png` and its preview. `web.serve_smoke` FAILs on a panic, a load failure, a crash, Chrome exiting, no readiness, a blank screenshot (99.5 % one colour) or any error-level record (console errors, exceptions, failed loads; Chrome's known noise stays at debug as in dev); Chrome that cannot be driven at all is a FAIL with `chrome.log`. The result's `smoke` sums it up (`status`, `ready`, `renderer`, `wasm[]` with status, MIME type and `Cache-Control`, `blank`, `errors`, `screenshot`, `console`). The session host and the serve check share the page recorder (`web/page.rs`), which also keeps the responses.
- **`icm verify web`** takes the site directory (default: the first upload of `dist/latest/web`) or a zip of one (unzipped into the run directory with `unzip`; anything else is `usage.bad_args`) and runs the static gates and the serve check, with the project's size budget (4096 KB outside a project). `--url` loads the deployed site instead; a `.wasm` served with another type or status fails `web.mime` with the host's fix (`_headers`, `hosting/`, the S3 line). The serve check's files are in the run directory: the project's `runs/` (the core attaches it), else icm's cache.
- **Owner plan.** As before per `[web] host`, plus a `once` step on the headers: nothing for Netlify and Cloudflare Pages, `hosting/` for nginx, Apache and Caddy, the S3 notes, and GitHub Pages's defaults.
- **Verified** 2026-10-07 on macOS arm64. The template in the fork (`examples/app`, the root lock's wasm-bindgen 0.2.106): the `icm-web` build (41 s cold), wasm-bindgen's 6,353,989 bytes became 4,240,652 after `wasm-opt -Oz` (1,668,565 gzipped; the JavaScript 137,180 and 21,493), `ICM_EVENT ready` about 2 s after navigation in headless Chrome 154 (WebGL2 through SwiftShader), the counter drawn with Fira Sans, no console error. A new app from `icm new` (`cli/tests/accept/phase4.sh`, whose fresh lock took wasm-bindgen 0.2.129): 6,535,196 bytes to 4,326,622 (1,695,487 gzipped), ready after 2 to 3.5 s, `icm verify web` and `icm verify web --url` against a local static server passing, and failing `web.mime` when that server sends the `.wasm` as `application/octet-stream`. wasm-bindgen 0.2.106 orders some exports differently from run to run, so two releases of one commit can carry different hashed names; each name still matches its content. Real hosts are the owner's acceptance; `.github/workflows/icm-web.yml` runs the release on a Linux runner once Actions is enabled.

### Desktop releases (phase 5; `cli/src/release/{macos,windows,linux,desktop}.rs`; §9.6, §11.4 to §11.6, §12.4, Appendix C items 20 and 23)

- **Hosts.** Each desktop target builds on its own OS, with that OS's toolchain and packaging tools. On any other host `icm release macos|windows|linux` exits 4 `env.unsupported_host` after the core's owner checks and before anything is built. `--dry-run` still prints the plan. `ICM_HOST_OS` (`macos`, `windows`, `linux`) stands in for the host in icm's tests, which run the Windows and Linux pipelines against fake tools on a Mac (`cli/tests/desktop_release.rs`).
- **Windows hosts.** This build of icm does not run on Windows: its process groups, signals, locks and sessions are Unix-only (25 files of `cli/src` use `std::os::unix` or `libc`). The Windows pipeline is complete and tested against fake tools, and `makensis` compiled its generated installer for real on macOS. Its real-host job in `.github/workflows/icm-desktop.yml` runs only when the repository variable `ICM_WINDOWS_HOST` is `true`, once icm supports the host.
- **macOS identity** (`release/macos/sign.rs`). `security find-identity -p codesigning` (certificates only, so it never prompts) searches host.toml `signing_keychain` / `ICM_KEYCHAIN`, else the user's search list.
  - `auto` takes a *valid* `Developer ID Application:` identity. None is `macos.sign.no_developer_id` and identities of several names are `macos.sign.identity_ambiguous` (new). Both are the owner's (exit 9), before the build.
  - A SHA-1 or a name takes that identity even when it is untrusted or not a Developer ID: the app is signed with it, and the release ends with exit 9 `macos.sign.no_developer_id` once its artifacts are written. That is how a self-signed identity in a test keychain exercises the signed path (`cli/tests/accept/test-identity.sh` makes one, in a keychain that never joins the search list).
  - codesign always gets the SHA-1 (a name can match several certificates) and `--keychain` when one is set. A Developer ID signs with `--timestamp`, any other identity with `--timestamp=none`, so icm never asks Apple's timestamp service to vouch for a certificate Apple did not issue. `--sign none` signs ad hoc, with the hardened runtime, because Apple silicon runs only signed code; it still looks the identity up and reports the result as a WARN or INFO.
  - A codesign that takes more than 120 s is `macos.sign.keychain_prompt` (new, the owner's).
  - `--dry-run` shows the same command lines without looking the identity up: `auto`, a SHA-1 or a `Developer ID Application:` name stands for a Developer ID (`--timestamp`), any other name signs with `--timestamp=none`, and `security find-identity` and codesign name the keychain from host.toml `signing_keychain` / `ICM_KEYCHAIN`.
  - `artifacts.json` `signing` records the identity, its SHA-1, `developer_id`, the keychain path, `hardened_runtime`, `team` and the `authority` chain.
- **macOS stage 1.**
  - It builds for the host's Apple triple (`aarch64-apple-darwin` on Apple silicon), not always aarch64. `--universal` or `[desktop.macos] universal` builds both triples and joins them with `lipo -create`; the x86_64 target is checked first (`env.rust_target_missing`).
  - `macos.arch` (new) reads each slice's architecture from the Mach-O, as the design's `lipo -archs` would. `macos.min_os` wants each slice's `LC_BUILD_VERSION` minos to equal `min_os`, except that arm64 slices cannot go below 11.0.
  - The dSYM gate is `macos.dsym` (new, WARN; Appendix C item 23 for macOS): `dwarfdump --debug-line` must name one of the package's own `lib`/`bin` sources, relative to the crate, as DWARF 4 tables do (`"src/lib.rs"`, or `"main.rs"` with an include directory `"src"`). An empty dSYM names none.
  - The executable is `strip -S`'d after `dsymutil`. The Info.plist (`release/macos/bundle.rs`) adds `CFBundleSupportedPlatforms`, `NSSupportsAutomaticGraphicsSwitching` and a `PkgInfo` to §9.6's keys. `LSApplicationCategoryType` maps `[app] category` (bare or `public.app-category.*`) and is left out for a category macOS does not know. Usage descriptions cover the camera, the microphone and location.
  - Entitlements are written only when `[app.permissions]` needs one under the hardened runtime: `device.camera`, `device.audio-input`, `personal-information.location`.
  - The icon keeps its alpha (desktop icons may be transparent; only iOS flattens). It comes from `[app] icon`, else the template's placeholder, resampled with a premultiplied box filter into an iconset for `iconutil`. `[app] resources` go to `Contents/Resources/<path>`.
  - `plutil -lint` is `macos.bundle` (new). spctl's verdict is INFO `macos.gatekeeper`, explained (unnotarized Developer ID, untrusted identity, ad hoc): every app is rejected before notarization.
  - Dist: `<Name>.app` (role `stage`, kind `app`), `<Name>-<version>.app.zip` (`upload`, `app_zip`; ditto `--keepParent --norsrc --noextattr --noqtn --noacl`), `<Name>-<version>.dSYM.zip` (`symbols`, `dsym`). The notices sit at `Contents/Resources/THIRD_PARTY_NOTICES.txt` in the app and the zip. `signed` is true only for a Developer ID under `--sign auto`.
- **macOS stage 2 (`--dmg`).** It reads stage 1's `artifacts.json`; without its `.app` it fails `release.not_found` (exit 2). `macos.not_stapled` (`xcrun stapler validate`) is the owner's before anything is built (a WARN under `--sign none`), and `codesign --verify` on the app runs again (a changed app stops the release). The stage directory gets the app (ditto) and an `Applications` link, then `hdiutil create -fs HFS+ -format UDZO`. macOS 27's hdiutil prints a deprecation warning for `create` and still works. The DMG is signed like the app, without the runtime option. Gates: `macos.sign.verify` and `macos.dmg` (new: `hdiutil verify`). Stage 1's files are recorded again (the stapled app with its new sha256 and stage 1's `cdhash`), and the zip's role becomes `stage`; the DMG's `cdhash` is recorded too. The owner plans note that stapling changes the bytes and not the signature. The notices place in the DMG is declared (INFO).
- **`icm verify macos`** needs macOS. It takes a `.app`, an `.app.zip` (unzipped with ditto) or a `.dmg` (`hdiutil verify`, then attached read-only and `-nobrowse` under `target/icm/tmp`, the app inside checked, then detached, by force if needed). Its gates are the bundle keys, the Mach-O minos against `[desktop.macos] min_os` (else `LSMinimumSystemVersion`), the signature and the hardened runtime. With `--after-notarize`, `macos.not_stapled` and `macos.gatekeeper` must pass; without it, Gatekeeper's verdict is explained.
- **`icm diagnose notarytool`** (`release/macos/notary.rs`) reads the JSON of a submission or of `notarytool log`, or the text output.
  - `Accepted` is PASS `macos.notarization` (new).
  - `Invalid`/`Rejected` is FAIL `macos.notarization` (exit 1), with each issue mapped to its gate: hardened runtime, `macos.sign.verify` (signature, secure timestamp, `get-task-allow`), `macos.sign.no_developer_id`. A bare `Invalid` points at `notarytool log`.
  - Authentication and keychain-profile errors are `macos.notary_credentials` (new, exit 9).
  - `In Progress` is a WARN.
- **Windows** (`release/windows.rs`, `release/windows/files.rs`).
  - Tools: `rc.exe` and `signtool.exe` come from `ICM_TOOL_*`, else the newest `Windows Kits\10\bin\<version>\x64`, else `PATH`; missing is `windows.sdk_missing`, and signtool is needed only when signing. `wix` and `makensis` are found only for the formats asked; missing is `env.tool_missing` by `agent`, naming `dotnet tool install --global wix --version 5.0.2` or NSIS.
  - `windows.msi_version` (exit 3) also requires `[app] build` ≤ 65535, VERSIONINFO's field width.
  - `app.rc` is UTF-8 (`#pragma code_page(65001)`) with no `#include`, so rc.exe needs no include path. Its `.res` is named after a hash of the rc and the ICO, so cargo relinks when the version changes.
  - The build is `cargo rustc --release --locked --target x86_64-pc-windows-msvc` with `--config target.x86_64-pc-windows-msvc.rustflags=["-C", "target-feature=+crt-static"]` (every crate, so C dependencies agree) and `-- -C link-arg=<res>`.
  - Gates: `windows.pe_imports` (new; Appendix C item 20) reads the PE import and delay-import tables (`release/desktop/pe.rs`) and fails on the VC++ redistributable and MinGW runtimes; the Universal CRT is part of Windows 10, so it passes. `windows.subsystem` (new, WARN) flags a console executable.
  - `sign_command` runs through `sh -c` (Git for Windows' sh), with `{file}` quoted and `%VAR%` rewritten to `${VAR}`, so secrets stay in the environment and out of icm's argv and logs. `artifacts.json` `signing` records `sign_program` (the command's program, without its directory) and `sign_env`, never the command line. The executable is signed before it is packaged, then both installers are signed. `windows.signed` runs `signtool verify /pa /v` on all three; under `--sign none` it is one WARN.
  - `app.wxs` (WiX v5 schema) installs per machine in `ProgramFiles64Folder\<Name>`: a component per file, the notices and resources included, a Start-menu shortcut keyed by an `HKMU` value, `MajorUpgrade AllowSameVersionUpgrades`, `ARPPRODUCTICON`, and the UpgradeCode (a version-5 GUID from SHA-256 of `[app] id`; no `upgrade_code` key yet).
  - `installer.nsi` installs per user in `$LOCALAPPDATA\Programs\<Name>` with an uninstaller, a Start-menu shortcut and an HKCU uninstall entry (with `QuietUninstallString`). makensis runs with `-INPUTCHARSET UTF8` and `LC_ALL=C.UTF-8`, because NSIS 3.13 on Unix aborts with `std::bad_alloc` under the C locale.
  - Files: `<Name>-<version>.msi` (kind `msi`) and `<Name>-<version>-setup.exe` (kind `exe`). The ICO (`release/desktop/icons.rs`) holds PNG frames of 16 to 256 px, which makensis accepts.
  - `icm verify windows` reads the PE gates on any host; signtool runs only on Windows (SKIP elsewhere).
- **Linux** (`release/linux.rs`, `release/linux/files.rs`).
  - `[desktop.linux] maintainer` is `config.owner_decision`, the owner's before the build. Under `--sign none` the package says `<publisher> <maintainer-unset@invalid>`.
  - The package name is `deb_package`, else the Cargo package's name made a Debian name (lower case, `_` as `-`); an invalid one is `config.invalid`. dpkg-deb and dpkg-shlibdeps are required for `deb` (`env.tool_missing` naming `apt-get install`), and the pinned appimagetool and runtime for `appimage` (`--yes` downloads them).
  - The executable is built for the host. `linux.glibc_floor` reads `.gnu.version_r` (`release/desktop/elf.rs`).
  - The `.deb` carries `usr/share/doc/<package>/{THIRD_PARTY_NOTICES.txt, copyright}` and resources under `usr/share/<package>/`. `Installed-Size` is computed. Depends comes from `dpkg-shlibdeps -O` against a stub `debian/control`, Recommends from §11.6 (`libvulkan1, libegl1`, not `libvulkan1 | libgl1`: wgpu's GL fallback opens `libEGL.so.1`) plus `deb_recommends`, and Section and `Categories=` map `[app] category`. It is built with `dpkg-deb -Zxz --build --root-owner-group`: xz, which every Debian-based dpkg reads, where Ubuntu's default zstd is not read by older Debian.
  - `dpkg-deb --info` must show the package. `linux.desktop_file` is icm's own checks plus `desktop-file-validate` when installed. `linux.deb.lint` is lintian's E/W tags (WARN) when installed, SKIP otherwise.
  - The AppImage's AppDir bundles libxkbcommon(-x11) and libwayland-cursor from the host's library directories (`ICM_LINUX_LIB_DIRS` for tests), with their Debian copyright files when present. libwayland-client stays the host's: it is on the AppImage project's excludelist (a Mesa built against a newer wayland fails to load against an older bundled copy, and wgpu then finds no adapter), and a unit test keeps every bundled library off that list (`files::EXCLUDELIST`). A missing one is `linux.appimage_libs` (new, WARN). `appimagetool --appimage-extract-and-run --runtime-file <pinned runtime>` runs with `ARCH`.
  - Linux packages carry no signature, so `signed` is true under `--sign auto`. The Xvfb smoke test is the CI job's, not icm's.
  - `icm verify linux` unpacks a `.deb` with its own `ar` reader and `tar` on any host, and an AppImage with `--appimage-extract` on Linux only (SKIP elsewhere). It checks glibc, the `.desktop` entries and the notices.
- **Shared** (`release/desktop.rs`): `[app] resources` (the web's glob rules, made relative to the project), `[app] publisher` (else the name) for installers and packages, and the category maps.
- **Verified 2026-10-07 on this Mac** (macOS 27.0.1, Xcode 27), on copies of `cli/tests/fixtures/release`:
  - An unsigned release: an ad-hoc, hardened, arm64 `.app` with minos 12.0, its zip and dSYM, then the DMG, and `icm verify macos` mounting it.
  - A release signed with a self-signed identity in a temporary keychain (`ICM_KEYCHAIN`): `codesign --verify` passes, `Authority=` names the identity, the flags are `runtime`, and the release ends with exit 9. The user's keychain search list was unchanged.
  - A universal build: `lipo -archs` gives x86_64 and arm64. Changing `min_os` to 13.0 relinked both slices.
  - The real makensis 3.13 (Homebrew) compiled the generated installer script with its PNG-frame icon. The real dpkg-deb 1.23 built the `.deb`, and `icm verify linux` read it.
  - `cli/tests/accept/phase5.sh` with the template app (a release build of iced with thin LTO): `icm release macos` without a Developer ID exits 9 with `macos.sign.no_developer_id`. `--sign none` gives an ad-hoc, hardened arm64 `.app` with minos 12.0 and its DMG, which mounts with `hdiutil attach -nobrowse`; the app inside verifies, `icm verify macos` passes, and the bundled app draws its first frame (Metal). A release signed with a self-signed identity also draws its first frame. Windows and Linux releases are refused with `env.unsupported_host`. The macOS job of `.github/workflows/icm-desktop.yml` runs this script. Each launch writes its own log, removed before the app starts, so a launch never reads the ready line of the one before. The script picks its steps by host: on Linux the `.deb` and AppImage release with `linux.glibc_floor`, `icm verify linux`, `dpkg -i`/`dpkg -r` (as root, or with `ICM_ACCEPT_INSTALL=1` and passwordless sudo) and the AppImage under `xvfb-run`; on Windows (Git Bash) the `.msi` and NSIS release, `icm verify windows` and both installers installed and removed with `ICM_ACCEPT_INSTALL=1`. Those two branches have not run anywhere yet: the workflow's Linux and Windows jobs run the same checks inline, and icm does not build on Windows.
- **Not done.** `icm ci init` (step 5.4) still exits 2 `usage.not_implemented`. The real-host Windows and Linux acceptance (MSI and NSIS install and uninstall, `dpkg -i`, the AppImage under Xvfb) is the CI jobs', pending Actions and, for Windows, icm's Windows host support.
