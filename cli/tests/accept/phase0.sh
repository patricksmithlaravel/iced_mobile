#!/usr/bin/env bash
# Phase 0 acceptance (docs/icm/DESIGN.md §18, with Appendix C): the
# framework prerequisites icm relies on. Run from anywhere in a fork
# checkout on a Mac with the iOS, Android and wasm targets installed:
#
#   cli/tests/accept/phase0.sh
#
# It builds in the fork's own target/, starts examples/app's desktop window
# twice for a few seconds, and keeps every other output in $ACCEPT (default:
# a new temporary directory). The tag check is an owner step: it runs only
# with ICM_ACCEPT_OWNER_PUSHED=1, after the owner has pushed the tag.
set -euo pipefail

FORK=${FORK:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)}
ACCEPT=${ACCEPT:-$(mktemp -d)}
mkdir -p "$ACCEPT"
cd "$FORK"
# shellcheck source=lib.sh
. "$FORK/cli/tests/accept/lib.sh"
echo "fork: $FORK"
echo "outputs: $ACCEPT"

# The framework check matrix: iced alone must compile for every target the
# template ships to. On Android iced turns on NativeActivity by default
# (iced_winit alone enables no activity backend).
check_matrix() {
    cargo check -p iced
    cargo check -p iced --target aarch64-apple-ios-sim
    cargo check -p iced --target aarch64-linux-android
    cargo check -p iced --target wasm32-unknown-unknown
    evidence "cargo check -p iced: host, aarch64-apple-ios-sim, aarch64-linux-android, wasm32-unknown-unknown"
}

# iced must also build on the rust-version the workspace declares (Tawara
# pins iced_winit directly and keeps a floor of its own). Runs when that
# toolchain is installed (`rustup toolchain install <version>`), in its own
# target directory.
msrv_version() {
    sed -n 's/^rust-version = "\(.*\)"$/\1/p' Cargo.toml | head -n1
}

msrv_toolchain() {
    rustup toolchain list | awk '{print $1}' | grep -E "^$(msrv_version)(\.0)?-" | head -n1 || true
}

msrv() {
    local toolchain
    toolchain=$(msrv_toolchain)
    CARGO_TARGET_DIR="$ACCEPT/msrv-target" cargo "+$toolchain" check -p iced
    evidence "cargo +$toolchain check -p iced (workspace rust-version $(msrv_version))"
}

# What phase 0 adds besides code: the template, the agents' limitations
# page, the Fira Sans licence, the empty `agent` feature (F7 stub) and the
# repository metadata.
deliverables() {
    test -f examples/app/Cargo.toml
    test -f examples/app/icm.toml
    test -f examples/app/tests/icm.rs
    test -s docs/agents/limitations.md
    test -s graphics/fonts/OFL.txt
    cargo metadata --no-deps --format-version 1 >"$ACCEPT/metadata.json"
    jqe '.packages[] | select(.name == "iced") | .features | has("agent") and (.agent == [])' "$ACCEPT/metadata.json"
    jqe '.packages[] | select(.name == "iced") | .repository | test("github.com/.*/iced_mobile")' "$ACCEPT/metadata.json"
    evidence "examples/app, docs/agents/limitations.md, graphics/fonts/OFL.txt present; iced features.agent = []; repository $(/usr/bin/jq -r '.packages[] | select(.name == "iced") | .repository' "$ACCEPT/metadata.json")"
}

build_app() {
    cargo build -p app
    test -x target/debug/app
    evidence "target/debug/app built"
}

# Start the desktop app with stderr in $2 for at most $1 seconds or until
# the ready event; leaves the elapsed seconds in $ACCEPT/$3.
run_app() {
    local limit=$1 log=$2 took=$3 pid i
    ./target/debug/app 2>"$log" &
    pid=$!
    for i in $(seq "$limit"); do
        grep -q '^ICM_EVENT {"v":1,"kind":"ready"' "$log" && break
        kill -0 "$pid" 2>/dev/null || break
        sleep 1
    done
    echo "$i" >"$ACCEPT/$took"
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
}

# ICM_EVENT lines are opt-in (Appendix C item 27): with ICM_EVENTS=1 the
# desktop app reports ready on stderr.
events_on() {
    export ICM_EVENTS=1
    run_app 60 "$ACCEPT/ev.log" ready-secs
    grep -q '^ICM_EVENT {"v":1,"kind":"ready"' "$ACCEPT/ev.log"
    evidence "ready after ~$(cat "$ACCEPT/ready-secs")s: $(grep -m1 '^ICM_EVENT {"v":1,"kind":"ready"' "$ACCEPT/ev.log" | cut -c1-160)"
    evidence "events seen: $(grep -o '^ICM_EVENT {"v":1,"kind":"[a-z_]*"' "$ACCEPT/ev.log" | sed 's/.*"kind":"//; s/"$//' | sort -u | tr '\n' ' ')"
}

# ... and without it, the same app prints none, for longer than it took to
# get ready above.
events_off() {
    local wait_for
    wait_for=$(($(cat "$ACCEPT/ready-secs" 2>/dev/null || echo 5) * 2 + 3))
    env -u ICM_EVENTS ./target/debug/app 2>"$ACCEPT/noev.log" &
    local pid=$!
    sleep "$wait_for"
    kill -0 "$pid"   # still running: it was not silent because it died
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    if grep -q 'ICM_EVENT' "$ACCEPT/noev.log"; then
        grep 'ICM_EVENT' "$ACCEPT/noev.log" | head -n 5
        return 1
    fi
    evidence "no ICM_EVENT line in ${wait_for}s without ICM_EVENTS ($(wc -l <"$ACCEPT/noev.log" | tr -d ' ') stderr lines)"
}

# The headless harness (F4): `cargo test` runs tests/flows/*.ice.
flows() {
    ICED_TEST_BACKEND=tiny-skia cargo test -p app --test icm 2>&1 | tee "$ACCEPT/flows.log"
    evidence "$(grep -E '^(flow |test result)' "$ACCEPT/flows.log" | tr '\n' ' ')"
}

# ... and renders the real view: iPhone 17 points at 3x.
headless_shot() {
    rm -f "$ACCEPT/h.png"
    ICED_TEST_BACKEND=tiny-skia cargo test -p app --test icm -- icm-shot --viewport 402x874 --scale 3 --theme light --wait-ms 500 --out "$ACCEPT/h.png"
    is_png "$ACCEPT/h.png"
    test "$(png_size "$ACCEPT/h.png")" = "1206 2622"
    evidence "$ACCEPT/h.png: $(png_size "$ACCEPT/h.png" | tr ' ' x), $(wc -c <"$ACCEPT/h.png" | tr -d ' ') bytes"
}

owner_tag() {
    git ls-remote --tags origin 'v0.14.1-mobile.1' | grep -q mobile.1
    evidence "origin has v0.14.1-mobile.1"
}

# In a function, so bash has read all of it before the first step: an edit
# to this file during a run cannot change what the run does.
main() {
    step check-matrix check_matrix
    if [ -n "$(msrv_toolchain)" ]; then
        step msrv msrv
    else
        skip msrv "Rust $(msrv_version), the workspace's rust-version, is not installed (rustup toolchain install $(msrv_version))"
    fi
    step deliverables deliverables
    must build-app build_app
    step events-opt-in events_on
    step events-off-by-default events_off
    step flows flows
    step headless-shot headless_shot
    if [ "${ICM_ACCEPT_OWNER_PUSHED:-}" = 1 ]; then
        step owner-tag owner_tag
    else
        skip owner-tag "owner step: set ICM_ACCEPT_OWNER_PUSHED=1 once v0.14.1-mobile.1 is pushed"
    fi
    finish
}

main "$@"
