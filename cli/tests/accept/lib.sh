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
