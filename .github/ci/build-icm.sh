#!/usr/bin/env bash
# Builds icm from this checkout, as AGENTS.md's install line does, into
# <root>/bin, and puts that directory on the job's PATH ($GITHUB_PATH).
#
#   .github/ci/build-icm.sh <root>
set -euo pipefail

fork=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
# shellcheck source-path=SCRIPTDIR source=lib.sh
. "$fork/.github/ci/lib.sh"

root=${1:-}
if [ -z "$root" ]; then
    echo "usage: $0 <root>" >&2
    exit 2
fi
mkdir -p "$root"
root=$(cd "$root" && pwd)

cargo install --locked --path "$(native "$fork/cli")" --root "$(native "$root")"
"$root/bin/icm" --version
if [ -n "${GITHUB_PATH:-}" ]; then
    # PATH entries in the runner's own form (C:\... on Windows).
    if command -v cygpath >/dev/null 2>&1; then
        cygpath -w "$root/bin" >>"$GITHUB_PATH"
    else
        echo "$root/bin" >>"$GITHUB_PATH"
    fi
fi
