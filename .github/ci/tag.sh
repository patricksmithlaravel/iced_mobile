#!/usr/bin/env bash
# A release tag names icm's version (AGENTS.md "Rules"): one tag,
# v0.14.x-mobile.N, releases the framework and icm together, and only
# cli/Cargo.toml carries the -mobile.N version, so the tag must be "v" plus
# exactly that version. `icm new` derives the tag it pins from the same
# version (design Appendix C item 3), so a mismatch would make every new app
# pin a tag that does not exist.
#
#   .github/ci/tag.sh <tag>
#
# Exits 0 when the tag matches, 1 (with an ::error:: line) when it does not,
# and 2 without a tag.
set -euo pipefail

fork=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
tag=${1:-}
if [ -z "$tag" ]; then
    echo "usage: $0 <tag>" >&2
    exit 2
fi

# The first `version = "…"` line of cli/Cargo.toml is [package]'s.
version=$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' "$fork/cli/Cargo.toml" | head -n1)

if ! printf '%s\n' "$tag" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+-mobile\.[0-9]+$'; then
    echo "::error::tag $tag is not of the form v<major>.<minor>.<patch>-mobile.<n>"
    exit 1
fi
if [ "$tag" != "v$version" ]; then
    echo "::error::tag $tag does not match icm's version $version (cli/Cargo.toml); the tag must be v$version"
    exit 1
fi
echo "tag $tag matches icm $version (cli/Cargo.toml)"
