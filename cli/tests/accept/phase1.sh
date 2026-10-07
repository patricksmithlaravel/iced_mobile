#!/usr/bin/env bash
# Phase 1 acceptance (docs/icm/DESIGN.md §18, with Appendix C and its
# phase 1 scope cut, item 30): the dev loop on desktop, web (headless
# Chrome), the iOS Simulator and an Android emulator, on this Mac.
#
#   cli/tests/accept/phase1.sh
#
# About ten minutes on an M4 Max with cargo's registry cache warm; slower
# machines take longer. Every icm command that can build for long runs with
# --detach and `icm wait` (nine minutes per call), or after the detached
# prewarm, so no single command outlasts an agent's command timeout; run the
# script itself in the background and read its output.
#
# Outputs go to $ACCEPT (default: a new temporary directory). icm is
# installed into $ICM_ROOT (default $ACCEPT/icm). Unless they are already
# set, icm's cache, its host.toml (and the debug keystore next to it) and
# Android's per-user directories (the AVD, adb and emulator state) live in
# $ACCEPT too, so the run creates no AVD in ~/.android and nothing in
# ~/.config/icm; the emulator still writes ~/.android/modem-nv-ram-<port>.
# The iOS Simulator has no such switch: the managed simulator
# (icm-iphone-*) is created in the user's device set when missing, and shut
# down at the end like the managed emulator.
#
# Tawara: `icm check` on a scratch clone of $TAWARA (default
# ~/Tawara-mobile) with a hand-written icm.toml (design §15); the real
# repository is only read by `git clone`. Skipped when it does not exist.
set -euo pipefail

FORK=${FORK:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)}
F="$FORK/cli/tests/fixtures"
ACCEPT=${ACCEPT:-$(mktemp -d)}
mkdir -p "$ACCEPT"
ACCEPT=$(cd "$ACCEPT" && pwd)
# shellcheck source=lib.sh
. "$FORK/cli/tests/accept/lib.sh"

ICM_ROOT=${ICM_ROOT:-$ACCEPT/icm}
export PATH="$ICM_ROOT/bin:$PATH"
export ICM_CACHE_DIR=${ICM_CACHE_DIR:-$ACCEPT/cache}
export ICM_HOST_CONFIG=${ICM_HOST_CONFIG:-$ACCEPT/host/host.toml}
export ANDROID_USER_HOME=${ANDROID_USER_HOME:-$ACCEPT/android-home}
export ANDROID_AVD_HOME=${ANDROID_AVD_HOME:-$ANDROID_USER_HOME/avd}
mkdir -p "$ICM_CACHE_DIR" "$(dirname "$ICM_HOST_CONFIG")" "$ANDROID_AVD_HOME" "$ACCEPT/previews"
TAWARA=${TAWARA:-$HOME/Tawara-mobile}
DEMO="$ACCEPT/demo"
PANICS="$ACCEPT/panics"
PLATFORMS=(desktop web ios-sim android)
cd "$ACCEPT"

echo "fork: $FORK"
echo "outputs: $ACCEPT"
echo "icm: $ICM_ROOT/bin/icm; cache $ICM_CACHE_DIR; host.toml $ICM_HOST_CONFIG; AVDs $ANDROID_AVD_HOME"

# Whatever happens, leave no app, browser, simulator or emulator running.
cleanup() {
    local dir
    for dir in "$DEMO" "$PANICS"; do
        if [ -f "$dir/icm.toml" ] && command -v icm >/dev/null; then
            (cd "$dir" && icm stop --all --shutdown --json -q >"$ACCEPT/cleanup-$(basename "$dir").json" 2>&1) || true
        fi
    done
}
trap cleanup EXIT

# --- helpers ------------------------------------------------------------

# keep FILE NAME: copy a screenshot or preview into $ACCEPT/previews.
keep() {
    if [ -n "$1" ] && [ "$1" != null ] && [ -f "$1" ]; then
        cp "$1" "$ACCEPT/previews/$2"
    fi
}

# crop_count PNG SCALE TOP OUT: cut "Count: N" out of a screenshot of the
# template (left padding 16 pt, the row starts TOP pt down, 44 pt tall).
# Both offsets are above 0: with `--cropOffset 0 0` sips crops the centre.
crop_count() {
    local scale=$2 top=$3 x y w h
    x=$(awk -v s="$scale" 'BEGIN { printf "%d", 16 * s }')
    y=$(awk -v s="$scale" -v t="$top" 'BEGIN { printf "%d", t * s }')
    w=$(awk -v s="$scale" 'BEGIN { printf "%d", 140 * s }')
    h=$(awk -v s="$scale" 'BEGIN { printf "%d", 44 * s }')
    sips -c "$h" "$w" --cropOffset "$y" "$x" "$1" --out "$4" >/dev/null
}

# fixed_evidence RESULT: one evidence line per fix doctor applied, cut short.
fixed_evidence() {
    local line
    /usr/bin/jq -r '.fixed[]? | tostring' "$1" | while IFS= read -r line; do
        evidence "fixed: $(printf '%s' "$line" | sed "s|$ACCEPT|\$ACCEPT|g" | cut -c1-160)"
    done
}

# check_events RESULT FILTER: the run's events (from its run directory)
# include a check event matching FILTER.
check_events() {
    local dir
    dir=$(/usr/bin/jq -r .run_dir "$1")
    /usr/bin/jq -e -s "[.[] | select(.type == \"check\") | select($2)] | length > 0" "$dir/events.ndjson" >/dev/null || {
        echo "no check event matching $2 in $dir/events.ndjson; the checks were:"
        /usr/bin/jq -r 'select(.type == "check") | "  \(.status // .level) \(.id): \(.detail)"' "$dir/events.ndjson" | cut -c1-160
        return 1
    }
}

# --- install, doctor, new -------------------------------------------------

install_icm() {
    cargo install --locked --path "$FORK/cli" --root "$ICM_ROOT"
    test "$(command -v icm)" = "$ICM_ROOT/bin/icm"
    evidence "$(command -v icm)"
}

version() {
    icm --version | tee "$ACCEPT/version.txt"
    grep -Eq '^icm 0\.14\.1-mobile\.[0-9]+ ' "$ACCEPT/version.txt"
    evidence "$(cat "$ACCEPT/version.txt")"
}

doctor_machine() {
    icmd doctor desktop web ios-sim android --fix --yes >"$ACCEPT/doctor.json" || true
    jqe '.exit == 0' "$ACCEPT/doctor.json"
    evidence "$(/usr/bin/jq -r '.summary' "$ACCEPT/doctor.json")"
    fixed_evidence "$ACCEPT/doctor.json"
}

new_app() {
    icm new "$DEMO" --id com.example.demo --framework path:"$FORK" --json -q >"$ACCEPT/new.json"
    jqe '.ok' "$ACCEPT/new.json"
    test -f "$DEMO/icm.toml"
    test -f "$DEMO/AGENTS.md"
    grep -q '^id = "com.example.demo"' "$DEMO/icm.toml"
    evidence "$(/usr/bin/jq -r '.summary' "$ACCEPT/new.json"); warnings: $(/usr/bin/jq -c '[.warnings[].id]' "$ACCEPT/new.json")"
}

# In the app: the lockfile and the wasm-bindgen CLI matching it, which a
# doctor outside any app cannot know (it SKIPs deps.wasm_bindgen_cli).
doctor_app() {
    cd "$DEMO"
    icmd doctor --fix --yes >"$ACCEPT/doctor-app.json" || true
    jqe '.exit == 0' "$ACCEPT/doctor-app.json"
    test -f Cargo.lock
    jqe '[.requirements[] | select(.id == "deps.wasm_bindgen_cli")] | length == 1 and all(.[]; .status == "pass")' "$ACCEPT/doctor-app.json"
    evidence "$(/usr/bin/jq -r '.summary' "$ACCEPT/doctor-app.json")"
    fixed_evidence "$ACCEPT/doctor-app.json"
}

check_all() {
    cd "$DEMO"
    icmd check --all >"$ACCEPT/check.json" || true
    jqe '.ok and .checks.fail == 0' "$ACCEPT/check.json"
    evidence "checks: $(/usr/bin/jq -c '.checks | {pass, warn, fail, info, skip}' "$ACCEPT/check.json"); warnings: $(/usr/bin/jq -c '[.warnings[].id]' "$ACCEPT/check.json")"
}

# The catalogue explains every id; in human mode the doc is on stdout.
explain() {
    cd "$DEMO"
    icm explain run.app_panicked --json -q >"$ACCEPT/explain.json"
    jqe '.ok and .exit == 0' "$ACCEPT/explain.json"
    icm explain run.app_panicked >"$ACCEPT/explain.txt"
    grep -q '^# run.app_panicked' "$ACCEPT/explain.txt"
    icm explain exit-codes --json -q >"$ACCEPT/explain-exit.json"
    jqe '.ok' "$ACCEPT/explain-exit.json"
    evidence "$(head -n 3 "$ACCEPT/explain.txt" | tr '\n' ' ')"
}

# --timeout ends the command with step.timeout (exit 8) and a result, and
# kills the step's process group: no cargo is left behind. The touch makes
# every platform's cargo check recompile the app, which takes longer than
# the second allowed.
timeout_kills() {
    cd "$DEMO"
    touch src/lib.rs
    icm check --all --timeout 1s --json -q >"$ACCEPT/timeout.json" || true
    jqe '.exit == 8 and .errors[0].id == "step.timeout"' "$ACCEPT/timeout.json"
    sleep 1
    if pgrep -f "cargo .*--manifest-path $DEMO/Cargo.toml" >/dev/null; then
        pgrep -fl "cargo .*--manifest-path $DEMO/Cargo.toml"
        return 1
    fi
    evidence "$(/usr/bin/jq -r '.errors[0].detail' "$ACCEPT/timeout.json" | sed "s|$ACCEPT|\$ACCEPT|g" | cut -c1-200)"
}

# --detach returns at once with the `icm wait` command; the waits then give
# the build's own result (Appendix C item 24). It also prewarms every
# platform, so the runs below build incrementally.
build_detached() {
    cd "$DEMO"
    icm build --all --detach --json -q >"$ACCEPT/build-start.json"
    jqe '.ok and .status == "running" and (.next[0].cmd | startswith("icm wait "))' "$ACCEPT/build-start.json"
    local run rc
    run=$(/usr/bin/jq -r .run "$ACCEPT/build-start.json")
    while :; do
        rc=0
        icm wait "$run" --timeout 9m --json -q >"$ACCEPT/build.json" || rc=$?
        [ "$rc" -eq 8 ] && jqe '.status == "running" and .errors[0].id == "run.still_running"' "$ACCEPT/build.json" && continue
        break
    done
    jqe '.ok and .command == "build" and (.built | length) == 4' "$ACCEPT/build.json"
    evidence "run $run; built $(/usr/bin/jq -c '.built' "$ACCEPT/build.json") in $(/usr/bin/jq -r '.ms' "$ACCEPT/build.json") ms"
}

# --- the dev loop ------------------------------------------------------------

run_platform() {
    local p=$1 out="$ACCEPT/run-$1.json"
    cd "$DEMO"
    icm run "$p" --timeout 9m --json -q >"$out" || true
    keep "$(/usr/bin/jq -r '.artifacts.screenshot // empty' "$out")" "run-$p.png"
    keep "$(/usr/bin/jq -r '.artifacts.preview // empty' "$out")" "run-$p.preview.png"
    jqe '.ok and .exit == 0 and .process.ready.source == "icm_event"' "$out"
    jqe '(.warnings | map(.id) | index("run.screen_blank")) == null' "$out"
    test -s "$(/usr/bin/jq -r .artifacts.preview "$out")"
    # Without Screen Recording the desktop run passes with WARN
    # desktop.shot.permission and a headless preview (§18 note).
    if [ "$p" = desktop ] && [ "$(/usr/bin/jq -r '.screen.source' "$out")" = headless ]; then
        jqe '(.warnings | map(.id) | index("desktop.shot.permission")) != null' "$out"
    fi
    evidence "ready $(/usr/bin/jq -c '.process.ready | {source, ms}' "$out"); screen $(/usr/bin/jq -c '.screen' "$out"); warnings $(/usr/bin/jq -c '[.warnings[].id]' "$out"); preview $ACCEPT/previews/run-$p.preview.png"
}

# The last line of `--json` is the result; a second web run replaces the
# first one's session (Appendix C item 13).
run_web_again() {
    cd "$DEMO"
    icm run web --timeout 9m --json >"$ACCEPT/run-web-2.ndjson" || true
    tail -n1 "$ACCEPT/run-web-2.ndjson" >"$ACCEPT/run-web-2.json"
    jqe '.type == "result"' "$ACCEPT/run-web-2.json"
    jqe '.ok' "$ACCEPT/run-web-2.json"
    /usr/bin/jq -e -s 'all(.[]; .v == 1)' "$ACCEPT/run-web-2.ndjson" >/dev/null
    evidence "$(wc -l <"$ACCEPT/run-web-2.ndjson" | tr -d ' ') lines, last: type=$(/usr/bin/jq -r .type "$ACCEPT/run-web-2.json") ok=$(/usr/bin/jq -r .ok "$ACCEPT/run-web-2.json")"
}

# Every --json line is a v1 object, and the last one is an ok result.
run_ios_stream() {
    cd "$DEMO"
    icm run ios-sim --timeout 9m --json >"$ACCEPT/run-ios-sim-2.ndjson" || true
    local l
    while IFS= read -r l; do
        printf '%s\n' "$l" | /usr/bin/jq -e '.v == 1' >/dev/null
    done <"$ACCEPT/run-ios-sim-2.ndjson"
    tail -n1 "$ACCEPT/run-ios-sim-2.ndjson" >"$ACCEPT/run-ios-sim-2.json"
    jqe '.type == "result" and .ok' "$ACCEPT/run-ios-sim-2.json"
    evidence "$(wc -l <"$ACCEPT/run-ios-sim-2.ndjson" | tr -d ' ') lines, all v1; types $(/usr/bin/jq -r .type "$ACCEPT/run-ios-sim-2.ndjson" | sort | uniq -c | awk '{printf "%s=%s ", $2, $1}')"
}

logs_ios() {
    cd "$DEMO"
    icm logs ios-sim --level info --json -q >"$ACCEPT/logs-ios-sim.json" || true
    jqe '.ok' "$ACCEPT/logs-ios-sim.json"
    evidence "$(/usr/bin/jq -r '.summary' "$ACCEPT/logs-ios-sim.json"); count $(/usr/bin/jq -c '.count // .counts' "$ACCEPT/logs-ios-sim.json"); sources $(/usr/bin/jq -c '[.records[]?.source] | group_by(.) | map({(.[0]): length}) | add' "$ACCEPT/logs-ios-sim.json")"
}

input_tap() {
    local p=$1 x=$2 y=$3 out="$ACCEPT/input-$1.json"
    cd "$DEMO"
    icm input "$p" tap "$x" "$y" --json -q >"$out" || true
    jqe '.ok' "$out"
    evidence "tap $x $y (preview px) -> $(/usr/bin/jq -c '{screen, summary}' "$out")"
}

# One coordinate space (Appendix C item 25): tap "Increment" at its place
# in points, read off the template's layout (right-aligned, about 106 pt
# wide, its row TOP pt down), and see "Count: 0" become "Count: 1" in the
# running app's screenshot.
tap_increment() {
    local p=$1 top=$2 before after x y w scale
    cd "$DEMO"
    icm shot "$p" --name before-tap --json -q >"$ACCEPT/shot-$p-before.json"
    jqe '.ok' "$ACCEPT/shot-$p-before.json"
    before=$(/usr/bin/jq -r '.artifacts.screenshot' "$ACCEPT/shot-$p-before.json")
    w=$(/usr/bin/jq -r '.screen.pt[0]' "$ACCEPT/shot-$p-before.json")
    scale=$(/usr/bin/jq -r '.screen.scale' "$ACCEPT/shot-$p-before.json")
    x=$(awk -v w="$w" 'BEGIN { printf "%d", w - 69 }')
    y=$((top + 22))
    icm input "$p" tap "$x" "$y" --space pt --json -q >"$ACCEPT/input-$p-increment.json"
    jqe '.ok' "$ACCEPT/input-$p-increment.json"
    sleep 1
    icm shot "$p" --name after-tap --json -q >"$ACCEPT/shot-$p-after.json"
    jqe '.ok' "$ACCEPT/shot-$p-after.json"
    after=$(/usr/bin/jq -r '.artifacts.screenshot' "$ACCEPT/shot-$p-after.json")
    keep "$before" "$p-before-tap.png"
    keep "$after" "$p-after-tap.png"
    keep "$(/usr/bin/jq -r '.artifacts.preview // empty' "$ACCEPT/shot-$p-after.json")" "$p-after-tap.preview.png"
    crop_count "$before" "$scale" "$top" "$ACCEPT/previews/$p-count-before.png"
    crop_count "$after" "$scale" "$top" "$ACCEPT/previews/$p-count-after.png"
    if cmp -s "$ACCEPT/previews/$p-count-before.png" "$ACCEPT/previews/$p-count-after.png"; then
        echo "the count did not change: compare $ACCEPT/previews/$p-count-{before,after}.png"
        return 1
    fi
    evidence "tap $x $y pt on a ${w}pt-wide screen at ${scale}x; count crops differ: $ACCEPT/previews/$p-count-{before,after}.png"
}

host_test() {
    cd "$DEMO"
    icmd test >"$ACCEPT/test.json" || true
    jqe '.ok and .checks.pass > 0 and .checks.fail == 0' "$ACCEPT/test.json"
    evidence "$(/usr/bin/jq -r '.summary' "$ACCEPT/test.json"); checks $(/usr/bin/jq -c '.checks | {pass, fail}' "$ACCEPT/test.json")"
}

headless_shots() {
    cd "$DEMO"
    icmd shot --headless --all-viewports >"$ACCEPT/shots.json" || true
    jqe '.ok and (.shots | length) == 4 and all(.shots[]; .blank == false)' "$ACCEPT/shots.json"
    local label
    for label in $(/usr/bin/jq -r '.shots[].label' "$ACCEPT/shots.json"); do
        keep "$(/usr/bin/jq -r --arg l "$label" '.shots[] | select(.label == $l) | .preview' "$ACCEPT/shots.json")" "headless-$label.preview.png"
    done
    evidence "$(/usr/bin/jq -c '[.shots[] | {label, size}]' "$ACCEPT/shots.json")"
}

ui_tree() {
    cd "$DEMO"
    icm ui --headless tree --json -q >"$ACCEPT/tree.json" || true
    jqe '.ok and .artifacts.tree != null' "$ACCEPT/tree.json"
    local tree
    tree=$(/usr/bin/jq -r .artifacts.tree "$ACCEPT/tree.json")
    grep -q 'Increment' "$tree"
    grep -q 'Count: 0' "$tree"
    evidence "tree $tree ($(wc -c <"$tree" | tr -d ' ') bytes) names Increment and Count: 0"
}

# No app, browser, booted icm simulator or icm emulator is left.
nothing_left() {
    icm ps --json -q >"$ACCEPT/ps.json"
    jqe '[.sessions[]? | select(.running and ((.alive | length) > 0))] | length == 0' "$ACCEPT/ps.json"
    if xcrun simctl list devices booted | grep -E '^\s+icm-' | grep -v 'icm-test-'; then
        echo "an icm simulator is still booted"
        return 1
    fi
    local adb serial name
    adb="$(icm print env android | sed -n 's/^export ANDROID_HOME=//p')/platform-tools/adb"
    if [ -x "$adb" ]; then
        for serial in $("$adb" devices | awk '/^emulator-/ {print $1}'); do
            name=$("$adb" -s "$serial" emu avd name 2>/dev/null | head -n1 | tr -d '\r')
            case "$name" in
            icm-test-*) ;;
            icm-*)
                echo "the managed emulator $name ($serial) still runs"
                return 1
                ;;
            esac
        done
    fi
}

stop_all() {
    cd "$DEMO"
    icm stop --all --shutdown --json -q >"$ACCEPT/stop.json" || true
    jqe '.ok' "$ACCEPT/stop.json"
    nothing_left
    evidence "$(/usr/bin/jq -r '.summary' "$ACCEPT/stop.json"); ps after: $(/usr/bin/jq -r '.summary' "$ACCEPT/ps.json")"
}

# --- negative cases ----------------------------------------------------------

# An app that panics in its first view: the template, made by `icm new`, with
# one line added. It shares the demo's target directory and lockfile, so
# only the app crate compiles.
make_panics() {
    icm new "$PANICS" --id com.example.panics --framework path:"$FORK" --no-git --json -q >"$ACCEPT/new-panics.json"
    jqe '.ok' "$ACCEPT/new-panics.json"
    cp "$DEMO/Cargo.lock" "$PANICS/Cargo.lock"
    mkdir -p "$PANICS/.cargo"
    printf '[build]\ntarget-dir = "%s"\n' "$DEMO/target" >"$PANICS/.cargo/config.toml"
    /usr/bin/perl -0pi -e 's/(\n    fn view\(&self\) -> Element<[^\n]*\{\n)/$1        panic!("acceptance: this app panics in its first view");\n/' "$PANICS/src/lib.rs"
    grep -q 'panic!("acceptance: this app panics' "$PANICS/src/lib.rs"
    cd "$DEMO"
    icmd build ios-sim --config "$PANICS/icm.toml" >"$ACCEPT/build-panics-ios-sim.json" || true
    jqe '.ok' "$ACCEPT/build-panics-ios-sim.json"
    icmd build android --config "$PANICS/icm.toml" >"$ACCEPT/build-panics-android.json" || true
    jqe '.ok' "$ACCEPT/build-panics-android.json"
    evidence "$PANICS/src/lib.rs: $(grep -n 'panic!("acceptance' "$PANICS/src/lib.rs" | tr -s ' ')"
}

panic_ios() {
    cd "$DEMO"
    icm run ios-sim --config "$PANICS/icm.toml" --timeout 9m --json -q >"$ACCEPT/p1.json" || true
    jqe '.exit == 10 and .errors[0].id == "run.app_panicked" and (.errors[0].evidence | length) > 0' "$ACCEPT/p1.json"
    evidence "$(/usr/bin/jq -c '.errors[0] | {id, detail, evidence: [.evidence[]? | (.text // .path // .)]}' "$ACCEPT/p1.json" | cut -c1-400)"
}

panic_android() {
    cd "$DEMO"
    icm run android --config "$PANICS/icm.toml" --timeout 9m --json -q >"$ACCEPT/p2.json" || true
    jqe '.exit == 10 and .errors[0].id == "run.app_panicked"' "$ACCEPT/p2.json"
    evidence "$(/usr/bin/jq -c '.errors[0] | {id, detail, evidence: [.evidence[]? | (.text // .path // .)]}' "$ACCEPT/p2.json" | cut -c1-400)"
}

# Two copies of iced in one lockfile. The fixture is copied, so icm's run
# directory is not written into the checkout.
two_copies() {
    rm -rf "$ACCEPT/twocopies"
    cp -R "$F/twocopies" "$ACCEPT/twocopies"
    cd "$DEMO"
    icm check --config "$ACCEPT/twocopies/icm.toml" --json -q >"$ACCEPT/p3.json" || true
    jqe '.exit == 3 and .errors[0].id == "deps.single_iced"' "$ACCEPT/p3.json"
    evidence "$(/usr/bin/jq -c '.errors[0] | {id, detail}' "$ACCEPT/p3.json" | cut -c1-300)"
}

bad_platform() {
    cd "$DEMO"
    icm run nowhere --json -q >"$ACCEPT/p4.json" || true
    jqe '.exit == 2' "$ACCEPT/p4.json"
    evidence "$(/usr/bin/jq -c '.errors[0] | {id, detail}' "$ACCEPT/p4.json")"
}

# --- Tawara --------------------------------------------------------------------

# A scratch clone of Tawara-mobile with icm.toml written by hand from design
# §15 (`init --adopt-*` is cut from phase 1), then `icm check`: the config
# is valid, the lockfile checks pass (one iced, one Android activity
# backend) and both platforms compile.
tawara_check() {
    rm -rf "$ACCEPT/tawara"
    git clone --quiet "$TAWARA" "$ACCEPT/tawara"
    cd "$ACCEPT/tawara"
    cat >icm.toml <<'EOF'
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
EOF
    icmd check --all >"$ACCEPT/tawara-check.json" || true
    jqe '.ok and .exit == 0 and .checks.fail == 0' "$ACCEPT/tawara-check.json"
    check_events "$ACCEPT/tawara-check.json" '.id == "build.compile_error" and .status == "pass" and (.detail | startswith("cargo check ios-sim"))'
    check_events "$ACCEPT/tawara-check.json" '.id == "build.compile_error" and .status == "pass" and (.detail | startswith("cargo check android"))'
    evidence "checks $(/usr/bin/jq -c '.checks | {pass, warn, fail, info}' "$ACCEPT/tawara-check.json"); warnings $(/usr/bin/jq -c '[.warnings[].id]' "$ACCEPT/tawara-check.json")"
    evidence "real repository untouched: $(git -C "$TAWARA" status --short | wc -l | tr -d ' ') changed files in $TAWARA"
}

# What design §15 says `icm check` finds in Tawara: deps.cli_framework_skew
# WARNs (it pins the fork at 71f00e8); deps.single_iced and
# deps.android_activity_backend pass.
tawara_deps() {
    cd "$ACCEPT/tawara"
    jqe '(.warnings | map(.id) | index("deps.cli_framework_skew")) != null' "$ACCEPT/tawara-check.json"
    check_events "$ACCEPT/tawara-check.json" '.id == "deps.single_iced" and .status == "pass"'
    check_events "$ACCEPT/tawara-check.json" '.id == "deps.android_activity_backend" and .status == "pass"'
    evidence "deps.cli_framework_skew WARN, deps.single_iced PASS, deps.android_activity_backend PASS"
}

final_cleanup() {
    cd "$PANICS" 2>/dev/null || cd "$DEMO"
    icm stop --all --shutdown --json -q >"$ACCEPT/stop-panics.json" || true
    jqe '.ok' "$ACCEPT/stop-panics.json"
    cd "$DEMO"
    icm stop --all --shutdown --json -q >"$ACCEPT/stop-final.json" || true
    jqe '.ok' "$ACCEPT/stop-final.json"
    nothing_left
    evidence "$(/usr/bin/jq -r '.summary' "$ACCEPT/stop-final.json")"
}

# --- the run ---------------------------------------------------------------------

# In a function, so bash has read all of it before the first step: an edit
# to this file during a run cannot change what the run does.
main() {
    must install install_icm
    step version version
    must doctor doctor_machine
    must new new_app
    must doctor-app doctor_app
    step check-all check_all
    step explain explain
    step timeout-kills-group timeout_kills
    must build-detached build_detached
    for p in "${PLATFORMS[@]}"; do
        step "run-$p" run_platform "$p"
    done
    step run-web-again-last-line run_web_again
    step run-ios-sim-json-lines run_ios_stream
    step logs-ios-sim logs_ios
    step input-android input_tap android 200 400
    step input-android-increment tap_increment android 64
    step input-web input_tap web 100 100
    step input-web-increment tap_increment web 16
    step test host_test
    step shot-headless-all-viewports headless_shots
    step ui-headless-tree ui_tree
    step stop-all-shutdown stop_all
    must make-panics-app make_panics
    step panic-ios-sim panic_ios
    step panic-android panic_android
    step deps-single-iced two_copies
    step usage-bad-platform bad_platform
    if [ -d "$TAWARA/.git" ]; then
        step tawara-check tawara_check
        step tawara-deps tawara_deps
    else
        skip tawara-check "no git checkout at $TAWARA"
        skip tawara-deps "no git checkout at $TAWARA"
    fi
    step final-cleanup final_cleanup
    finish
}

main "$@"
