#!/usr/bin/env bash
# Installs the Rust toolchain a CI job builds with and makes it the default.
#
#   .github/ci/toolchain.sh pinned [<target>...]   the toolchain examples/app/rust-toolchain.toml
#                                                  pins, with clippy and rustfmt
#   .github/ci/toolchain.sh msrv [<target>...]     the workspace's rust-version (Cargo.toml)
#
# The pinned toolchain is the one this release of icm and the template were
# tested with, so a new stable Rust cannot turn CI red overnight; raising the
# template's pin moves CI along with it. Every runner (macOS, Linux, Windows
# under Git Bash) has rustup; the Linux release container installs it first.
# The toolchain becomes rustup's default only on a runner (GITHUB_ACTIONS).
set -euo pipefail

fork=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
usage="usage: $0 pinned|msrv [<target>...]"

case "${1:-}" in
pinned)
    toolchain=$(sed -n 's/^channel *= *"\([^"]*\)".*/\1/p' "$fork/examples/app/rust-toolchain.toml" | head -n1)
    components="clippy,rustfmt"
    ;;
msrv)
    toolchain=$(sed -n 's/^rust-version *= *"\([^"]*\)".*/\1/p' "$fork/Cargo.toml" | head -n1)
    components=""
    ;;
*)
    echo "$usage" >&2
    exit 2
    ;;
esac
shift

if [ -z "$toolchain" ]; then
    echo "::error::no toolchain found for '$usage'" >&2
    exit 1
fi

args=(toolchain install "$toolchain" --profile minimal --no-self-update)
if [ -n "$components" ]; then
    args+=(--component "$components")
fi
for target in "$@"; do
    args+=(--target "$target")
done

rustup "${args[@]}"
# Only on a runner: on a workstation the default toolchain is its owner's.
if [ "${GITHUB_ACTIONS:-}" = true ]; then
    rustup default "$toolchain"
fi
rustc "+$toolchain" --version
cargo "+$toolchain" --version
