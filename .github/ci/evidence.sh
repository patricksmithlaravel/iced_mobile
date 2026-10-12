#!/usr/bin/env bash
# Prints the end of every file an icm result names as evidence for its
# errors (the step log of a failed fix or build, the emulator's log), so a
# failed CI step shows why in its own log. A result names paths relative to
# the directory icm ran in: run this from there.
#
#   .github/ci/evidence.sh <result.json> [<lines>]
set -euo pipefail

result=${1:-}
lines=${2:-60}
if [ -z "$result" ]; then
    echo "usage: $0 <result.json> [<lines>]" >&2
    exit 2
fi

jq -r '[.errors[]?.evidence[]?.path | strings] | unique | .[]' "$result" |
    while IFS= read -r path; do
        if [ -f "$path" ]; then
            echo "::group::$path"
            tail -n "$lines" "$path"
            echo "::endgroup::"
        else
            echo "evidence $path is not a file here"
        fi
    done
