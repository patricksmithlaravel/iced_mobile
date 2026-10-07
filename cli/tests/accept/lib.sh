# Helpers for the acceptance scripts (design §18). Sourced by phase<N>.sh,
# never run on its own.
#
# Every step runs in a subshell with `set -euo pipefail`, so its first
# failing command fails it. Its output goes to $ACCEPT/log/NN-<name>.log;
# lines it prints starting with `evidence:` are echoed under its PASS or
# FAIL line, the report an agent or the owner reads. A failed `step` is
# counted and the script goes on, so one run reports every step; a failed
# `must` (a step later steps cannot do without) ends the script. `finish`
# prints the summary and exits 1 when any step failed.

# jq: /usr/bin/jq (design §18), else the one on PATH (Git Bash has none
# in /usr/bin).
JQ=/usr/bin/jq
if [ ! -x "$JQ" ]; then
    JQ=$(command -v jq 2>/dev/null || echo /usr/bin/jq)
fi

ACCEPT_N=0
ACCEPT_PASSED=()
ACCEPT_FAILED=()
ACCEPT_SKIPPED=()

_accept_step() {
    local mode=$1 name=$2
    shift 2
    ACCEPT_N=$((ACCEPT_N + 1))
    mkdir -p "$ACCEPT/log"
    local log
    log=$(printf '%s/log/%02d-%s.log' "$ACCEPT" "$ACCEPT_N" "$name")
    local start=$SECONDS rc=0
    printf -- '-- %s\n' "$name"
    # Not inside `if`: bash ignores errexit in a condition, subshell included.
    set +e
    (
        set -euo pipefail
        "$@"
    ) >"$log" 2>&1
    rc=$?
    set -e
    local took=$((SECONDS - start))
    if [ "$rc" -eq 0 ]; then
        printf 'PASS %s (%ss)\n' "$name" "$took"
        ACCEPT_PASSED+=("$name")
        grep '^evidence:' "$log" | sed 's/^/    /' || true
    else
        printf 'FAIL %s (exit %s, %ss; log: %s)\n' "$name" "$rc" "$took" "$log"
        ACCEPT_FAILED+=("$name")
        grep '^evidence:' "$log" | sed 's/^/    /' || true
        tail -n 20 "$log" | grep -v '^evidence:' | sed 's/^/    | /' || true
        if [ "$mode" = must ]; then
            printf 'STOP: later steps need %s\n' "$name"
            finish
        fi
    fi
}

# step NAME COMMAND...: run COMMAND (usually a function of the script).
step() { _accept_step step "$@"; }

# must NAME COMMAND...: the same, but a failure ends the script.
must() { _accept_step must "$@"; }

# skip NAME WHY: a step this run does not perform (an owner step, say).
skip() {
    ACCEPT_N=$((ACCEPT_N + 1))
    printf 'SKIP %s: %s\n' "$1" "$2"
    ACCEPT_SKIPPED+=("$1")
}

finish() {
    printf '\n%s passed, %s failed, %s skipped (logs and outputs: %s)\n' \
        "${#ACCEPT_PASSED[@]}" "${#ACCEPT_FAILED[@]}" "${#ACCEPT_SKIPPED[@]}" "$ACCEPT"
    if [ "${#ACCEPT_FAILED[@]}" -gt 0 ]; then
        printf 'failed: %s\n' "${ACCEPT_FAILED[*]}"
        exit 1
    fi
    exit 0
}

# evidence TEXT...: one line for the report.
evidence() { printf 'evidence: %s\n' "$*"; }

# jqe FILTER FILE: `jq -e` that shows the value it judged when it fails.
jqe() {
    if ! "$JQ" -e "$1" "$2" >/dev/null; then
        printf 'assertion failed: %s\n  on %s:\n' "$1" "$2"
        "$JQ" -c '{ok, exit, summary, errors: [.errors[]? | {id, detail, evidence}], warnings: [.warnings[]? | .id], failed: .checks.failed}' "$2" 2>/dev/null ||
            head -c 2000 "$2"
        return 1
    fi
}

# icmd ARGS...: `icm ARGS... --detach --json -q`, then `icm wait` (at most
# nine minutes per call, under an agent's command timeout) until the run
# ends. Prints the run's result line and returns its exit code.
icmd() {
    local start run out rc=0
    start=$(icm "$@" --detach --json -q) || rc=$?
    if [ "$rc" -ne 0 ]; then
        printf '%s\n' "$start"
        return "$rc"
    fi
    if [ "$("$JQ" -r '.status' <<<"$start")" != running ]; then
        printf '%s\n' "$start"
        return "$("$JQ" -r '.exit' <<<"$start")"
    fi
    run=$("$JQ" -r '.run' <<<"$start")
    echo "detached: icm $* -> $run" >&2
    while :; do
        rc=0
        out=$(icm wait "$run" --timeout 9m --json -q) || rc=$?
        if [ "$rc" -eq 8 ] && [ "$("$JQ" -r '.status' <<<"$out")" = running ]; then
            echo "still running: $run" >&2
            continue
        fi
        printf '%s\n' "$out"
        return "$rc"
    done
}

# icm_env NAME [PLATFORM]: one variable of `icm print env PLATFORM`
# (default android), read from the JSON result. The shell form quotes a
# value with a space, so cutting `export NAME=` off its lines kept the
# quotes and broke on such paths; the JSON value is the path itself. Fails
# when icm reports no such variable.
icm_env() {
    local out
    if ! out=$(icm print env "${2:-android}" --json -q); then
        printf 'icm print env %s failed: %s\n' "${2:-android}" "$out" >&2
        return 1
    fi
    "$JQ" -er --arg name "$1" '.env[$name] // empty' <<<"$out" || {
        printf 'icm print env %s has no %s\n' "${2:-android}" "$1" >&2
        return 1
    }
}

# sdk_adb: the SDK's adb (icm's own Android environment). Fails when there
# is none, so a caller cannot read "no adb" as "no device online".
sdk_adb() {
    local home adb
    home=$(icm_env ANDROID_HOME)
    adb="$home/platform-tools/adb"
    if [ ! -x "$adb" ]; then
        printf 'no adb at %s: install platform-tools (icm doctor android --fix --yes)\n' "$adb" >&2
        return 1
    fi
    printf '%s\n' "$adb"
}

# java_home: the JDK icm hands Android's tools.
java_home() { icm_env JAVA_HOME; }

# no_foreign_android: fails when an Android device other than an icm-
# emulator is online. icm runs on the single online device when its own
# emulator is not up, so the owner's phone or AVDs, or another test run's
# icm-test- emulator, would get the demo installed; the script stops
# instead. Without an adb to ask, it fails too (fail closed).
no_foreign_android() {
    local adb devices serial name foreign=0
    adb=$(sdk_adb)
    devices=$("$adb" devices)
    for serial in $(awk 'NR > 1 && $2 == "device" {print $1}' <<<"$devices"); do
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
    evidence "online: $(awk 'NR > 1 && NF {printf "%s(%s) ", $1, $2}' <<<"$devices")"
}

# png_size FILE: "W H" of a PNG, from its IHDR.
png_size() {
    local w h
    w=$(od -An -tu1 -j16 -N4 "$1" | awk 'NF {print $1*16777216 + $2*65536 + $3*256 + $4}')
    h=$(od -An -tu1 -j20 -N4 "$1" | awk 'NF {print $1*16777216 + $2*65536 + $3*256 + $4}')
    printf '%s %s\n' "$w" "$h"
}

# is_png FILE: a non-empty file with the PNG signature.
is_png() {
    [ -s "$1" ] && [ "$(od -An -tx1 -N8 "$1" | tr -d ' \n')" = 89504e470d0a1a0a ]
}
