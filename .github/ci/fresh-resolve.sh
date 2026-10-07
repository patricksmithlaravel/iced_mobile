#!/usr/bin/env bash
# Fresh resolve (design Appendix C item 29): what a new app resolves today.
# The fork's Cargo.lock pins what the framework's own CI builds, but a new
# app has no lock, so its first cargo call resolves every crate afresh from
# crates.io. This makes an app from the template with icm, pinned to this
# checkout by path, resolves its lock from nothing, and compiles it for
# every platform in [app] platforms without a device (`icm check --all`).
#
#   .github/ci/fresh-resolve.sh <work dir>
#
# Needs icm (built from this checkout) and jq on PATH; on macOS, so the iOS
# Simulator target is checked too. Keeps icm's result objects in
# <work dir>/results, with the fresh Cargo.lock and the crates it has that
# the fork's lock does not (fresh-only.txt), and appends `results=<that
# directory>` to $GITHUB_OUTPUT when it is set.
set -euo pipefail

fork=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
# shellcheck source-path=SCRIPTDIR source=lib.sh
. "$fork/.github/ci/lib.sh"

work=${1:-}
if [ -z "$work" ]; then
    echo "usage: $0 <work dir>" >&2
    exit 2
fi
mkdir -p "$work/results"
work=$(cd "$work" && pwd)
results="$work/results"
app="$work/fresh"
# For the upload step, in the job's own form of the path.
if [ -n "${GITHUB_OUTPUT:-}" ]; then
    echo "results=$(native "$results")" >>"$GITHUB_OUTPUT"
fi

# `name version` of every package in a Cargo.lock, sorted.
lock_packages() {
    awk '
        /^\[\[package\]\]/ { name = ""; next }
        /^name = / { name = $3; gsub(/"/, "", name); next }
        /^version = / && name != "" { version = $3; gsub(/"/, "", version); print name " " version }
    ' "$1" | sort -u
}

group "icm new"
icm_json new new "$(native "$app")" --framework "path:$(native "$fork")"
cd "$app"

group "icm doctor desktop --fix --yes"
# The template's toolchain; installed here, it brings every target its
# rust-toolchain.toml lists. Already installed, it keeps its own targets, so
# CI installs the ones `check --all` needs first (.github/ci/toolchain.sh).
icm_json doctor doctor desktop --fix --yes

group "cargo generate-lockfile (from nothing)"
rm -f Cargo.lock
cargo generate-lockfile
cp Cargo.lock "$results/Cargo.lock"
lock_packages "$fork/Cargo.lock" >"$results/fork-packages.txt"
lock_packages Cargo.lock >"$results/fresh-packages.txt"
comm -13 "$results/fork-packages.txt" "$results/fresh-packages.txt" >"$results/fresh-only.txt"
echo "$(wc -l <"$results/fresh-packages.txt" | tr -d ' ') packages; $(wc -l <"$results/fresh-only.txt" | tr -d ' ') not in the fork's Cargo.lock:"
cat "$results/fresh-only.txt"

group "icm check --all"
icm_json check check --all
jq -r '.targets[]? | "\(.platform) \(.triple): \(if .ok then "ok" else "FAILED" end), \(.warnings // 0) warning(s), \(.ms) ms"' "$results/check.json"

group "done"
echo "fresh resolve: icm check --all is ok with $(wc -l <"$results/fresh-only.txt" | tr -d ' ') crate versions the fork's CI does not build"
