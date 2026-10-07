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
# down at the end like the managed emulator. adb's server is shared, so the
# run stops early when an Android device other than an icm- emulator is
# online: icm would otherwise install the demo on it.
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
# adb's own device pick would win over icm's managed emulator.
unset ANDROID_SERIAL
TAWARA=${TAWARA:-$HOME/Tawara-mobile}
DEMO="$ACCEPT/demo"
PANICS="$ACCEPT/panics"
PLATFORMS=(desktop web ios-sim android)
# A made-up secret for the redaction steps (never a real credential).
HOOK_SECRET="acc3pt-$$-$(date +%s)-hush"
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

# The SDK's adb, from icm's own Android environment.
sdk_adb() {
    printf '%s/platform-tools/adb\n' "$(icm print env android | sed -n 's/^export ANDROID_HOME=//p')"
}

# icm runs on the single online Android device when its own emulator is not
# up. Any device other than an icm- emulator (the owner's phone or AVDs, or
# another test run's icm-test- emulator) would then get the demo installed
# on it, so the run stops here instead.
no_foreign_android() {
    local adb serial name foreign=0
    adb=$(sdk_adb)
    if [ ! -x "$adb" ]; then
        evidence "no adb yet: no device can be online"
        return 0
    fi
    for serial in $("$adb" devices | awk 'NR > 1 && $2 == "device" {print $1}'); do
        name=
        case "$serial" in
        emulator-*) name=$("$adb" -s "$serial" emu avd name 2>/dev/null | head -n1 | tr -d '\r') ;;
        esac
        case "$name" in
        icm-test-*) ;;
        icm-*)
            echo "$serial runs $name"
            continue
            ;;
        esac
        echo "$serial (${name:-not an emulator}) is not icm's managed emulator: stop it, or run this script when it is off"
        foreign=1
    done
    [ "$foreign" -eq 0 ]
    evidence "online: $("$adb" devices | awk 'NR > 1 && NF {printf "%s(%s) ", $1, $2}')"
}

# Android's tools need JDK 17+, and the host's java is Java 8: icm finds a
# JDK and hands it to children through JAVA_HOME and PATH (Appendix C
# item 2), and `icm print env android` shows both.
android_env() {
    icm print env android >"$ACCEPT/env-android.sh"
    local java_home first major
    java_home=$(sed -n 's/^export JAVA_HOME=//p' "$ACCEPT/env-android.sh")
    test -n "$java_home"
    major=$("$java_home/bin/java" -version 2>&1 | sed -nE '1s/.*version "([0-9]+).*/\1/p')
    test "$major" -ge 17
    first=$(sed -n 's/^export PATH=//p' "$ACCEPT/env-android.sh" | cut -d: -f1)
    test "$first" = "$java_home/bin"
    grep -q '^export ANDROID_HOME=' "$ACCEPT/env-android.sh"
    grep -q '^export ANDROID_NDK_HOME=' "$ACCEPT/env-android.sh"
    icm print env android --json -q >"$ACCEPT/env-android.json"
    export ACCEPT_JAVA_HOME=$java_home
    jqe '.ok and .env.JAVA_HOME == $ENV.ACCEPT_JAVA_HOME and (.env.PATH | startswith($ENV.ACCEPT_JAVA_HOME + "/bin:"))' "$ACCEPT/env-android.json"
    evidence "JAVA_HOME=$java_home (java $major), first on PATH; $(grep -c '^export ' "$ACCEPT/env-android.sh") exports"
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

# The output contract (§4.1 to §4.4, Appendix D item 3). Human mode: stdout
# holds only protocol lines, and with -q only CHECK FAIL/WARN and RESULT.
# --json: every line is a v1 event, the first is `start`, the last and only
# result is the result. The run directory keeps events.ndjson, ending with
# that result, and result.json; target/icm/last.json copies the newest.
output_contract() {
    cd "$DEMO"
    icm check desktop >"$ACCEPT/contract-human.txt" 2>"$ACCEPT/contract-human.err"
    if grep -vE '^(STEP|CHECK|ARTIFACT|READY|PLAN|LOG|NEXT|RESULT) |^  ' "$ACCEPT/contract-human.txt"; then
        echo "human stdout lines without a protocol keyword (above)"
        return 1
    fi
    grep -q '^STEP ' "$ACCEPT/contract-human.txt"
    grep -q '^CHECK PASS ' "$ACCEPT/contract-human.txt"
    tail -n1 "$ACCEPT/contract-human.txt" | grep -q '^RESULT ok '
    icm check desktop -q >"$ACCEPT/contract-quiet.txt" 2>/dev/null
    if grep -vE '^CHECK (FAIL|WARN) |^RESULT |^  ' "$ACCEPT/contract-quiet.txt"; then
        echo "-q printed lines other than CHECK FAIL/WARN and RESULT (above)"
        return 1
    fi
    tail -n1 "$ACCEPT/contract-quiet.txt" | grep -q '^RESULT ok '

    icm check desktop --json >"$ACCEPT/contract.ndjson"
    /usr/bin/jq -e -s 'all(.[]; .v == 1 and (.type | type) == "string" and (.run | type) == "string" and (.t | type) == "number")' "$ACCEPT/contract.ndjson" >/dev/null
    /usr/bin/jq -e -s '.[0].type == "start" and .[-1].type == "result" and ([.[] | select(.type == "result")] | length) == 1' "$ACCEPT/contract.ndjson" >/dev/null
    /usr/bin/jq -e -s '[.[] | select(.type == "step" and .phase == "begin")] | length > 0 and all(.[]; has("name") and has("argv") and has("env") and has("cwd"))' "$ACCEPT/contract.ndjson" >/dev/null
    /usr/bin/jq -e -s '[.[] | select(.type == "step" and .phase == "end")] | length > 0 and all(.[]; has("name") and has("ok") and has("ms") and has("log"))' "$ACCEPT/contract.ndjson" >/dev/null
    tail -n1 "$ACCEPT/contract.ndjson" >"$ACCEPT/contract-result.json"
    jqe '.ok and .exit == 0 and .ok == (.exit == 0) and .schema == "icm.result/1" and .command == "check"' "$ACCEPT/contract-result.json"
    # The files hold the result without the event's `t`.
    local dir want
    dir=$(/usr/bin/jq -r .run_dir "$ACCEPT/contract-result.json")
    test -f "$dir/events.ndjson"
    test -f "$dir/result.json"
    test -f target/icm/last.json
    want=$(/usr/bin/jq -S -c 'del(.t)' "$ACCEPT/contract-result.json")
    test "$(tail -n1 "$dir/events.ndjson" | /usr/bin/jq -S -c 'del(.t)')" = "$want"
    test "$(/usr/bin/jq -S -c 'del(.t)' "$dir/result.json")" = "$want"
    test "$(/usr/bin/jq -S -c 'del(.t)' target/icm/last.json)" = "$want"
    ls "$dir/steps/" | grep -q '\.log$'
    evidence "human: $(wc -l <"$ACCEPT/contract-human.txt" | tr -d ' ') protocol lines, last: $(tail -n1 "$ACCEPT/contract-human.txt" | cut -c1-120)"
    evidence "-q: $(wc -l <"$ACCEPT/contract-quiet.txt" | tr -d ' ') lines; --json: $(wc -l <"$ACCEPT/contract.ndjson" | tr -d ' ') v1 events, start first, result last"
    evidence "$dir: events.ndjson ends with result.json; target/icm/last.json is the same; steps: $(ls "$dir/steps" | tr '\n' ' ')"
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
    jqe '.ok and (.records | length) > 0' "$ACCEPT/logs-ios-sim.json"
    evidence "$(/usr/bin/jq -r '.summary' "$ACCEPT/logs-ios-sim.json"); count $(/usr/bin/jq -c '.count // .counts' "$ACCEPT/logs-ios-sim.json"); sources $(/usr/bin/jq -c '[.records[]?.source] | group_by(.) | map({(.[0]): length}) | add' "$ACCEPT/logs-ios-sim.json")"
}

# `icm logs` on the other platforms: the running app's records since the
# launch, among them its ICM_EVENT lines.
logs_platform() {
    local p=$1 out="$ACCEPT/logs-$1.json"
    cd "$DEMO"
    icm logs "$p" --json -q >"$out" || true
    jqe '.ok and (.records | length) > 0' "$out"
    jqe '[.records[] | tostring | select(test("ICM_EVENT"))] | length > 0' "$out"
    evidence "$(/usr/bin/jq -r '.summary' "$out"); sources $(/usr/bin/jq -c '[.records[]?.source] | group_by(.) | map({(.[0]): length}) | add' "$out")"
}

# The OS screenshot of the running app (§13.1): a PNG at the screen's pixel
# size and a preview no longer than 1024 px, neither blank. Without Screen
# Recording the desktop falls back to the headless render, with its WARN.
shot_platform() {
    local p=$1 out="$ACCEPT/shot-$1.json" png preview long
    cd "$DEMO"
    icm shot "$p" --json -q >"$out" || true
    png=$(/usr/bin/jq -r '.artifacts.screenshot // empty' "$out")
    preview=$(/usr/bin/jq -r '.artifacts.preview // empty' "$out")
    keep "$png" "shot-$p.png"
    keep "$preview" "shot-$p.preview.png"
    jqe '.ok and (.warnings | map(.id) | index("run.screen_blank")) == null' "$out"
    is_png "$png"
    is_png "$preview"
    test "$(png_size "$png")" = "$(/usr/bin/jq -r '.screen.px | map(tostring) | join(" ")' "$out")"
    test "$(png_size "$preview")" = "$(/usr/bin/jq -r '.screen.preview | map(tostring) | join(" ")' "$out")"
    long=$(png_size "$preview" | awk '{print ($1 > $2) ? $1 : $2}')
    test "$long" -le 1024
    if [ "$p" = desktop ] && [ "$(/usr/bin/jq -r '.screen.source' "$out")" = headless ]; then
        jqe '(.warnings | map(.id) | index("desktop.shot.permission")) != null' "$out"
    fi
    evidence "$(/usr/bin/jq -r '.summary' "$out"); $(png_size "$png" | tr ' ' x) png, $(png_size "$preview" | tr ' ' x) preview; screen $(/usr/bin/jq -c '.screen' "$out"); $ACCEPT/previews/shot-$p.preview.png"
}

# One coordinate space (Appendix C item 25): every input result reports
# the screen in px, pt and preview pixels with its scale.
input_tap() {
    local p=$1 x=$2 y=$3 out="$ACCEPT/input-$1.json"
    cd "$DEMO"
    icm input "$p" tap "$x" "$y" --json -q >"$out" || true
    jqe '.ok' "$out"
    jqe '.screen | (.px | length) == 2 and (.pt | length) == 2 and (.preview | length) == 2 and .scale > 0' "$out"
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

# --- hooks: the children's environment and redaction -------------------------

# The demo gets a desktop and an Android hook (§13.6) for two steps, then
# its own icm.toml back.
hooks_on() {
    cd "$DEMO"
    cp icm.toml "$ACCEPT/demo-icm.toml"
    mkdir -p accept-hooks
    cat >accept-hooks/desktop.sh <<'EOF'
# acceptance: echoes a secret-named variable it inherits from icm
echo "stdout: the hook was given $ACCEPT_API_TOKEN"
echo "stderr: the hook was given $ACCEPT_API_TOKEN" >&2
if kill -0 "$ICM_PID" 2>/dev/null; then
    echo "CHECK PASS accept_desktop_pid: the app (pid $ICM_PID) is alive"
else
    echo "CHECK FAIL accept_desktop_pid: no live app at ICM_PID=$ICM_PID"
fi
echo "CHECK WARN accept_token: the hook was given $ACCEPT_API_TOKEN"
EOF
    cat >accept-hooks/android.sh <<'EOF'
# acceptance: Android children get JDK 17+ through JAVA_HOME and PATH
java_bin=$(command -v java)
version=$(java -version 2>&1 | head -n1)
major=$(printf '%s\n' "$version" | sed -E 's/.*version "([0-9]+).*/\1/')
if [ "$java_bin" = "$JAVA_HOME/bin/java" ] && [ "$major" -ge 17 ] 2>/dev/null; then
    echo "CHECK PASS accept_java: $version from JAVA_HOME=$JAVA_HOME"
else
    echo "CHECK FAIL accept_java: java is $java_bin ($version), JAVA_HOME=$JAVA_HOME"
fi
sdk=$($ICM_ADB shell getprop ro.build.version.sdk | tr -d '\r')
if [ -n "$sdk" ]; then
    echo "CHECK PASS accept_adb: ICM_ADB reaches the device (API $sdk)"
else
    echo "CHECK FAIL accept_adb: ICM_ADB=$ICM_ADB got no answer"
fi
EOF
    /usr/bin/perl -0pi -e 's/^(\[checks\][^\n]*\n)/$1desktop = ["accept-hooks\/desktop.sh"]\nandroid = ["accept-hooks\/android.sh"]\n/m' icm.toml
    grep -q '^desktop = \["accept-hooks/desktop.sh"\]' icm.toml
}

hooks_off() {
    cp "$ACCEPT/demo-icm.toml" "$DEMO/icm.toml"
    rm -rf "$DEMO/accept-hooks"
}

# hook_details RESULT: the hook checks' lines from the run's events.
hook_details() {
    /usr/bin/jq -r 'select(.type == "check" and (.id | startswith("hook."))) | "\(.status) \(.id): \(.detail)"' \
        "$(/usr/bin/jq -r .run_dir "$1")/events.ndjson" | sed "s|$HOOK_SECRET|<the secret>|g" | cut -c1-200
}

# A desktop hook runs once the app is up, with ICM_PID. A secret-named
# variable in icm's environment that the hook echoes is redacted in the
# hook's step logs, its stderr (`.log`) and stdout (`.stdout`) (§1
# principle 5, Appendix D item 8).
hooks_desktop() {
    cd "$DEMO"
    hooks_on
    ACCEPT_API_TOKEN=$HOOK_SECRET icm run desktop --no-build --timeout 9m --json -q >"$ACCEPT/hooks-desktop.json" || true
    jqe '.ok and (.hooks | length) == 1 and .hooks[0].ok' "$ACCEPT/hooks-desktop.json"
    jqe 'any(.hooks[0].checks[]; .id == "hook.accept_desktop_pid" and .status == "pass")' "$ACCEPT/hooks-desktop.json"
    local log out
    log=$(/usr/bin/jq -r '.hooks[0].log' "$ACCEPT/hooks-desktop.json")
    out="${log%.log}.stdout"
    test -f "$log"
    test -f "$out"
    if grep -nF "$HOOK_SECRET" "$log" "$out" | sed "s|$HOOK_SECRET|<the secret>|g" | grep .; then
        echo "the secret is in the hook's step logs (above)"
        return 1
    fi
    grep -q '^stderr: the hook was given <redacted>$' "$log"
    grep -q '^stdout: the hook was given <redacted>$' "$out"
    while IFS= read -r line; do evidence "$line"; done < <(hook_details "$ACCEPT/hooks-desktop.json")
    evidence "step logs $log and .stdout: the echoed secret reads <redacted>"
}

# ... and appears nowhere else in what icm writes: the run directory (its
# events.ndjson and result.json too), last.json and the printed result.
# The hook also put the secret in a `CHECK WARN` line, whose detail lands
# in the check event and the result's warnings.
redaction_everywhere() {
    cd "$DEMO"
    local dir
    dir=$(/usr/bin/jq -r .run_dir "$ACCEPT/hooks-desktop.json")
    test -d "$dir"
    if grep -rnF "$HOOK_SECRET" "$dir" target/icm/last.json "$ACCEPT/hooks-desktop.json" | sed "s|$HOOK_SECRET|<the secret>|g" | cut -c1-300 | grep .; then
        echo "the secret value given to icm in ACCEPT_API_TOKEN is written in the files above"
        return 1
    fi
    evidence "no file under $dir, nor last.json or the result, holds the secret"
}

# An Android hook gets the JDK and the device: `java` on its PATH is
# JAVA_HOME's JDK 17+ (the host's java is Java 8; Appendix C item 2) and
# ICM_ADB reaches the emulator.
hooks_android() {
    cd "$DEMO"
    icm run android --no-build --timeout 9m --json -q >"$ACCEPT/hooks-android.json" || true
    keep "$(/usr/bin/jq -r '.artifacts.preview // empty' "$ACCEPT/hooks-android.json")" "hooks-android.preview.png"
    jqe '.ok and (.hooks | length) == 1 and .hooks[0].ok' "$ACCEPT/hooks-android.json"
    jqe 'any(.hooks[0].checks[]; .id == "hook.accept_java" and .status == "pass") and any(.hooks[0].checks[]; .id == "hook.accept_adb" and .status == "pass")' "$ACCEPT/hooks-android.json"
    while IFS= read -r line; do evidence "$line"; done < <(hook_details "$ACCEPT/hooks-android.json")
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
    adb=$(sdk_adb)
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
    # --no-hardlinks: the clone shares no object file with the real one.
    git clone --quiet --no-hardlinks "$TAWARA" "$ACCEPT/tawara"
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
    step android-env android_env
    must no-foreign-android-device no_foreign_android
    must new new_app
    must doctor-app doctor_app
    step check-all check_all
    step explain explain
    step output-contract output_contract
    step timeout-kills-group timeout_kills
    must build-detached build_detached
    for p in "${PLATFORMS[@]}"; do
        step "run-$p" run_platform "$p"
    done
    step run-web-again-last-line run_web_again
    step run-ios-sim-json-lines run_ios_stream
    step logs-ios-sim logs_ios
    for p in desktop web android; do
        step "logs-$p" logs_platform "$p"
    done
    step shot-ios-sim shot_platform ios-sim
    step shot-desktop shot_platform desktop
    step input-android input_tap android 200 400
    step input-android-increment tap_increment android 64
    step input-web input_tap web 100 100
    step input-web-increment tap_increment web 16
    step test host_test
    step shot-headless-all-viewports headless_shots
    step ui-headless-tree ui_tree
    step hooks-desktop-redacted hooks_desktop
    step redaction-everywhere redaction_everywhere
    step hooks-android-env hooks_android
    step hooks-off hooks_off
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
