# Helpers for the CI scripts in this directory. Sourced, never run on its
# own. The scripts run under bash 3.2 (macOS), bash 5 (Linux) and Git Bash
# (Windows).

# The path as native tools take it: C:/... under Git Bash, unchanged
# elsewhere. icm, cargo and msiexec are native Windows programs there.
native() {
    if command -v cygpath >/dev/null 2>&1; then
        cygpath -m "$1"
    else
        printf '%s\n' "$1"
    fi
}

# A collapsible section in the job log, closing the one before.
group() {
    if [ "${GITHUB_ACTIONS:-}" = true ]; then
        if [ -n "${ci_group_open:-}" ]; then
            echo "::endgroup::"
        fi
        ci_group_open=1
        echo "::group::$*"
    else
        echo "== $*"
    fi
}

# fail MESSAGE: an error annotation (on stderr, so it survives a command
# substitution), then exit 1.
fail() {
    echo "::error::$*" >&2
    exit 1
}

# icm_json NAME ARGS...: runs `icm ARGS... --json -q`, keeps the result
# object in $results/NAME.json (the caller sets `results`), prints its exit
# and summary, and on failure its errors (as annotations) and their fixes.
# Returns icm's exit code.
# shellcheck disable=SC2154 # `results` is the caller's.
icm_json() {
    local name=$1
    shift
    local out="$results/$name.json" rc=0
    echo "+ icm $* --json -q"
    icm "$@" --json -q >"$out" || rc=$?
    if ! jq -e '.type == "result"' "$out" >/dev/null 2>&1; then
        echo "::error::icm $1 printed no result object (exit $rc)"
        cat "$out"
        [ "$rc" -ne 0 ] || rc=1
        return "$rc"
    fi
    # What is shown never fails the step; the exit code below does.
    jq -r '"exit \(.exit): \(.summary)"' "$out" || true
    jq -r '.warnings[]? | "WARN \(.id? // ""): \(.detail? // .)"' "$out" || true
    if [ "$rc" -ne 0 ]; then
        jq -r '.errors[]? | "::error::\(.id? // ""): \(.detail? // .)"' "$out" || true
        jq -r '.errors[]? | .fix?.commands[]? | "  fix: \(.)"' "$out" || true
        echo "run directory: $(jq -r '.run_dir // "none"' "$out" || true)"
    fi
    return "$rc"
}

# result_path NAME FILTER: a path from $results/NAME.json (icm prints them
# relative to the directory it ran in, here the app), made absolute with
# forward slashes; empty when the filter finds nothing.
result_path() {
    local path
    path=$(jq -r "$2 // empty" "$results/$1.json")
    path=${path//\\//}
    case "$path" in
    "" | /* | [A-Za-z]:/*) ;;
    *) path="$PWD/$path" ;;
    esac
    printf '%s\n' "$path"
}

# wait_ready LOG PID SECONDS: waits for the app's `ICM_EVENT` ready line in
# LOG (it prints them with ICM_EVENTS=1). Fails when the app exits first or
# the time runs out.
wait_ready() {
    local log=$1 pid=$2 limit=$3 waited=0
    while [ "$waited" -lt "$limit" ]; do
        if grep -q '^ICM_EVENT {"v":1,"kind":"ready"' "$log" 2>/dev/null; then
            echo "ready after ~${waited}s: $(grep -m1 '^ICM_EVENT {"v":1,"kind":"ready"' "$log" | cut -c1-160)"
            return 0
        fi
        if ! kill -0 "$pid" 2>/dev/null; then
            echo "the app exited before it was ready; its stderr:"
            tail -n 40 "$log"
            return 1
        fi
        sleep 1
        waited=$((waited + 1))
    done
    echo "no ready event in ${limit}s; the app's stderr:"
    tail -n 40 "$log"
    return 1
}
