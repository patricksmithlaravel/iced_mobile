> Design for `icm`, the iced_mobile app tool, written 2026-10-06 before implementation. Appendix C (the review's corrections) overrides earlier sections. Where the code and this document disagree, the code and its tests win; update this file in the same commit.

# icm: the iced_mobile app tool and template (final design)

Design only, 2026-10-06. Nothing in `/Users/patrickzweil/iced_mobile`, `~/Tawara-mobile` or any other repo was changed.

**Evidence tags.** **[V]** means verified on this Mac, by the research brief, by a judge or during this synthesis. **[S]** means it comes from a source cited in the research brief. **[I]** means inference that has not been tested yet. Every [I] has a spike or CI check in §18.

**What this is.** This is the synthesis of three candidate designs, cargo-icm, icepack and icm. cargo-icm and icm tied on the judges' totals (22 each); icepack scored 16.

- **Core:** cargo-icm's minimal-dependency core and release pipelines, the base two judges recommended.
- **From icm:** the agent contract, i.e. the result object with `errors[]`, the exit code for "owner needed", the run directory, the check catalogue, policy kept as data, a CLI that links no iced crate, and 16 KB AAB alignment.
- **From icepack:** `object`-based binary checks, the `--sign none` path, the session TTL, and wasm-opt feature flags derived from the compiler.

Every error the judges found is fixed. Appendix A lists each one and where it is resolved.

**State of the fork today [V].** Local `main` at `4ed24aa73` already contains:
- the P0 floors: winit 0.30.13, softbuffer 0.4.7, and the display-server exemption;
- NativeActivity by default, plus `iced::mobile::{init_logger, set_android_app, on_lifecycle, AndroidApp}` and `iced::android_main!` (`33a00d56c`);
- Fira Sans as the default font on Android and iOS (`4ba8c1edd`), with a fallback to system fonts (`5802255ff`);
- the keep-alive guards (`476dff628`) and iOS Return/Tab mapping (`4ed24aa73`).

None of this is pushed. The remote has only `tawara/0.14-mobile`, and no `v0.14.1-mobile.*` tag exists yet [V]. Phase 0 pushes it.

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
2. **"You decide."** The fork may carry winit patches only for fixes that cannot live in iced_winit: the A3 recreation freeze, C2 composition and C1 insets. iced_winit takes such a winit as a direct git dependency, never through `[patch]`, because `[patch]` does not reach apps. `icm check` enforces one winit (`deps.single_winit`), so either state of the fork is safe to detect.
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
| android | `android.device.none`, `.ambiguous` (7); `android.emulator.boot_timeout` (8), `.ports_busy` (7); `android.install.failed`, `.signature_mismatch` (7); `android.so.export` (6), `.align16k` (1), `.abis` (1); `android.manifest.target_sdk`, `.config_changes`, `.has_code`, `.debuggable`, `.lib_name`, `.version` (1), `.back_optout` (WARN); `android.bundle.alignment` (1); `android.aab.validate`, `.signed` (1), `.unsigned` (WARN, `--sign none`); `android.apk.zipalign`, `.signature` (1); `android.keystore.missing`, `.password_env_unset` (9); `android.permissions.review` (WARN); `android.screen.secure` (INFO); `android.aapt2_failed`, `.bundletool_failed` (6) |
| web | `web.port_busy` (7), `web.chrome_failed` (7), `web.size_budget` (1), `web.mime` (1), `web.fonts_embedded` (1), `web.renderer_fallback` (WARN), `web.hashed_assets` (1), `web.serve_smoke` (1) |
| desktop | `desktop.shot.permission` (WARN; falls back to a headless render), `macos.sign.no_developer_id` (9), `macos.hardened_runtime`, `macos.sign.verify`, `macos.min_os`, `macos.gatekeeper` (1), `macos.not_stapled` (9), `windows.sign.not_configured` (9), `windows.signed` (1), `windows.msi_version` (3), `windows.sdk_missing` (4), `linux.glibc_floor` (1), `linux.desktop_file` (1), `linux.deb.lint` (WARN) |
| run | `run.ready` / `run.alive` (PASS checks), `run.app_died`, `run.app_panicked`, `run.anr`, `run.not_ready` (10), `run.activity_recreated` (1), `run.screen_blank` (WARN; 1 with `--expect-content`), `run.font_missing` (WARN), `run.lock_busy` (7), `run.no_session` (7) |
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
    - `cargo install wasm-bindgen-cli --version =<lock> --locked --root <cache>`
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
back = "key"                           # enableOnBackInvokedCallback="false" (temporary opt-out; WARN in release)
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
- **Android:** `package`, `versionCode`, `versionName`, `<uses-sdk>`, `android:hasCode`, `android:extractNativeLibs`, `android:debuggable`, the activity name, `android.app.lib_name`, the launcher intent filter, `android:icon`, `android:roundIcon`, and `android:configChanges`. Users may *add* configChanges values but never remove any.

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

        // No safe-area API yet: pad for status bar, notch and home indicator (targetSdk 36 is edge-to-edge).
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
- Keep the root padding (no safe-area API yet; Android targetSdk 36 is edge-to-edge).
- .ice `click` and host tests use a mouse; phones use touch. Confirm UI changes with `icm run` on
  ios-sim and android and look at the screenshot.
- Keep `features = ["fira-sans"]`; text with no font renders as nothing.

## Releases belong to the owner
- `icm version bump build`, then `icm release <ios|android|web|macos|windows|linux> --json -q`.
  Artifacts, UPLOAD.md and upload.sh land in target/icm/dist/<version>+<build>/<target>/.
- NEVER run upload.sh or any command in UPLOAD.md (altool, notarytool, fastlane, wrangler, …).
- No signing assets? `icm release <target> --sign none` checks everything else.

## Known limitations of iced_mobile {{framework_tag}}
{{docs/agents/limitations.md of that tag: safe area (pad the root); clipboard stub on mobile; Android IME
is key events only (no composition, accents and CJK input unreliable); a drag starting on a button does not
scroll; text_editor and rich-text links ignore touch; dark mode not detected on mobile; Lifecycle::Suspended
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
      android:enableOnBackInvokedCallback="false" android:theme="@style/IcmTheme">
    <activity android:name="android.app.NativeActivity" android:exported="true" android:launchMode="singleTask"
        android:windowSoftInputMode="adjustResize|stateHidden"
        android:configChanges="mcc|mnc|locale|touchscreen|keyboard|keyboardHidden|navigation|screenLayout|fontScale|uiMode|orientation|density|screenSize|smallestScreenSize|layoutDirection|colorMode|fontWeightAdjustment|grammaticalGender">
      <meta-data android:name="android.app.lib_name" android:value="app"/>
      <intent-filter>
        <action android:name="android.intent.action.MAIN"/>
        <category android:name="android.intent.category.LAUNCHER"/>
      </intent-filter>
    </activity>
  </application>
</manifest>
```
- The configChanges list is the review §6.6 list: Tawara's values plus `mcc|mnc|grammaticalGender`. It is policy data keyed by API level.
- Generated resources:
  - `values/themes.xml`: `IcmTheme`, parent `@android:style/Theme.Material.NoActionBar`, with `windowBackground` set from `background`
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
   - `deps.wasm_bindgen_cli`: the cached wasm-bindgen equals the app lock's version (`doctor web --fix --yes` runs `cargo install wasm-bindgen-cli --version =<v> --locked --root <cache>/wasm-bindgen/<v>`)
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
   - Recommends = `libxkbcommon0, libxkbcommon-x11-0, libwayland-client0, libvulkan1 | libgl1` (dlopened by winit and wgpu, so `ldd` cannot see them) + `deb_recommends`.
   - `[desktop.linux] maintainer` is required.
   - `dpkg-deb --build --root-owner-group gen/linux/deb <dist>/linux/<pkg>_<version>-<build>_amd64.deb`.
   - Gates: `dpkg-deb --info`; `desktop-file-validate` (`linux.desktop_file`) if installed; `lintian` as WARN only.
3. AppImage:
   - Lay out `gen/linux/AppDir/{AppRun, <id>.desktop, <id>.png, usr/bin/<bin>, usr/lib/}`.
   - `usr/lib` bundles `libxkbcommon.so.0`, `libxkbcommon-x11.so.0`, `libwayland-client.so.0` and `libwayland-cursor.so.0`, copied from the same 22.04 image, so they match the glibc floor. GPU drivers come from the host. `AppRun` sets `LD_LIBRARY_PATH`.
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
| `ready` | after the first presented frame: `ms`, `window{size,scale}`, `backend` |
| `lifecycle` | `state` |
| `panic` | `message`, `location`, `thread` |
| `warning` | `code` (e.g. `font.default_missing`, `compositor.fallback`) |
| `exit` | `code` |

As built: `ready` also carries `window.physical`, `adapter` and `api` (`Metal`, `Vulkan`, `BrowserWebGpu` or `tiny-skia`), for example `{"v":1,"kind":"ready","ms":652,"window":{"size":[1024,768],"physical":[2048,1536],"scale":2},"backend":"wgpu","adapter":"Apple M4 Max","api":"Metal"}`. `start` has `pid: null` on the web and `bridge: null` until phase 6. `exit` is never sent on iOS or the web, where winit's run does not return. A panic inside winit's callbacks on macOS cascades into more panics and an abort (134), so several `panic` events can arrive: report the first. On the web, the first `ready` gives the canvas attribute size, which can differ from the page's. On Android, the sysprop `debug.iced.backend` chooses the renderer when `ICED_BACKEND` is unset.

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
| `Call set_android_app` / `android_main` ran twice / `RecreationAttempt` | duplicate iced copies (`deps.single_iced`) or Activity recreation (configChanges); never `iced::exit` |
| `No Unix display server backend` | framework floor not met |
| `dlopen failed: library "lib…so" not found` | `android.manifest.lib_name` |
| `INSTALL_FAILED_UPDATE_INCOMPATIBLE` / `INSTALL_FAILED_NO_MATCHING_ABIS` | signature changed (`--reinstall --yes`); ABI not built |
| `ANR in <id>` / `am_destroy_activity` | blocked main thread or recreation |
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
| F6 | `iced::mobile::safe_area` (fixed fallback insets first, then real ones); new types in `iced::mobile`, not new enum variants (review §6.8) | to do | 6 |
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
   activity = { "android:windowSoftInputMode" = "adjustResize|stateHidden" }
   [checks]                                   # added by hand in step 4, once the scripts exist
   # ios-sim = ["platform/ios/checks.sh"]     # TAWARA lines, xattr, 0700 mode, background lock
   # android = ["platform/android/checks.sh"] # FLAG_SECURE, no_backup mode, Home/Back
   ```
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
4. **Run directories.** Commands that do work keep `runs/<id>/` under `<target>/icm` once the project resolves, otherwise under the cache dir (`~/Library/Caches/icm`, `ICM_CACHE_DIR`). `explain`, `print` and `wait` are views and keep none; usage errors (exit 2) keep none. Paths in results are relative to the current directory when inside it.
5. **Content commands.** `print` and `explain` write their content to stdout in human mode (so `eval "$(icm print env android)"` works) and protocol lines to stderr, only on failure. With `--json` the content is a result field.
6. **`--detach` / `icm wait`** (item 24). The parent creates the run directory, re-executes icm in a new session with `ICM_RUN_ID`/`ICM_RUN_DIR`/`ICM_RUN_ROOT`/`ICM_DETACHED=1`, writes `detached.json`, and returns `status: "running"` with a `next` of `icm wait <run> --timeout 9m --json -q`. `icm wait` (default 9 minutes) replays the run's events and exits with its exit code; on timeout it returns `run.still_running` (exit 8, `status: "running"`).
7. **Signals.** SIGINT, SIGTERM and SIGHUP are recorded; the runner kills its child's process group (SIGTERM, then SIGKILL after 2 s) and the command fails `run.interrupted` (130) with a written result. A watchdog thread does the same after 5 s if the main thread does not notice.
8. **Runner.** Child output goes to files, never pipes (a daemon such as the adb server that inherits stdout cannot hang icm). Children get `RUSTUP_AUTO_INSTALL=0`, `CARGO_TERM_COLOR=never`, `GIT_TERMINAL_PROMPT=0`, `GIT_SSH_COMMAND="ssh -oBatchMode=yes"` (unless set) and `LC_ALL=C` (opt-out per step). Secrets are redacted in argv (`pass:…`, values after password flags, `NAME=value` for secret names), in the env delta, and in the step logs when a tool echoes a secret it was given.
9. **host.toml** also accepts `java_home` and `android_ndk`. `emulator_ports` must be even ports in 5554–5682 (item 10); the default stays `[5580, 5582, 5584]`.
10. **JDK discovery** reads every candidate's version (`release` file or `java -version`); `/usr/libexec/java_home -v 17+` returns the Java 8 applet plugin on this host and is rejected. Order: host.toml, `$JAVA_HOME`, `java_home -v 17+`, Homebrew `openjdk@21`/`@17`/`openjdk`, `/Library/Java/JavaVirtualMachines/*`, Android Studio's JBR, `/usr/lib/jvm/*`.
11. **Version embedding** (item 3). `build.rs` embeds the rev and the default framework pin: in a cargo git checkout (`.cargo-ok`), `tag:v<version>` when cargo's database has `refs/remotes/origin/tags/v<version>` at HEAD, else `rev:<sha>`; in a local checkout, `rev:<sha>` only when a configured non-local remote's branches contain HEAD and the tree is clean, else `path:<checkout>`. `ICM_BUILD_FRAMEWORK` forces it.
12. **Deployment targets** (item 6). Stamps live in `target/icm/stamps/deployment-<triple|host>-<profile>.txt`; a changed value, or an existing build icm did not stamp, runs `cargo clean -p <pkg> --target <t>` first.
13. **Input coordinates** (item 25). `--space preview|px|pt` (default `preview`); `screen.rs` converts and renders `screen{px, pt, preview, scale}`.
14. **Later-phase commands** (`release`, `init`, `version`, …) parse and exit 2 `usage.not_implemented`, as do phase-1 commands a build does not implement yet. `icm print tools` (discovery report) and `icm print commands` (the clap surface as JSON) exist from the core on.