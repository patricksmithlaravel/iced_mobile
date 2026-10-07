#!/usr/bin/env bash
# Phase 5 acceptance (docs/icm/DESIGN.md §18, phase 5): desktop releases on
# this Mac. The template app is released for macOS unsigned (an ad-hoc
# signed, hardened .app, its zip for the notary service and a DMG), then
# signed with a throwaway self-signed identity; Windows and Linux releases
# are refused here (they run on their own hosts, in CI).
#
#   cli/tests/accept/phase5.sh
#
# A few minutes with cargo's registry cache warm (the template's release
# build uses thin LTO). It needs macOS with Xcode's command-line tools,
# /usr/bin/jq, and the network the first time cargo resolves the template.
#
# Outputs go to $ACCEPT (default: a new temporary directory). icm is
# installed into $ICM_ROOT (default $ACCEPT/icm); its cache and host.toml
# live in $ACCEPT.
#
# Signing material is throwaway and stays in $ACCEPT: two temporary
# keychains (one empty, one holding a self-signed code-signing identity
# made with /usr/bin/openssl), handed to icm through ICM_KEYCHAIN. They
# never join the user's keychain search list (a step checks it is
# unchanged) and are deleted at the end. The user's own keychains are
# never searched. The built app opens a window for a few seconds.
#
# Owner steps (never run here): notarization and stapling, `icm release
# macos --dmg` of the stapled app, and `icm verify macos --after-notarize`.
set -euo pipefail

FORK=${FORK:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)}
ACCEPT=${ACCEPT:-$(mktemp -d)}
mkdir -p "$ACCEPT"
ACCEPT=$(cd "$ACCEPT" && pwd)
# shellcheck source=lib.sh
. "$FORK/cli/tests/accept/lib.sh"

ICM_ROOT=${ICM_ROOT:-$ACCEPT/icm}
export PATH="$ICM_ROOT/bin:$PATH"
export ICM_CACHE_DIR=${ICM_CACHE_DIR:-$ACCEPT/cache}
export ICM_HOST_CONFIG=${ICM_HOST_CONFIG:-$ACCEPT/host/host.toml}
mkdir -p "$ICM_CACHE_DIR" "$(dirname "$ICM_HOST_CONFIG")"
unset ICM_KEYCHAIN ICM_HOST_OS
DEMO="$ACCEPT/demo"
EMPTY_KC="$ACCEPT/keys/empty.keychain-db"
TEST_KC="$ACCEPT/keys/test.keychain-db"
KC_PASS="phase5-throwaway-$$"
SEARCH_LIST_BEFORE=$(security list-keychains -d user)
cd "$ACCEPT"

echo "fork: $FORK"
echo "outputs: $ACCEPT"

cleanup() {
    local kc
    for kc in "$EMPTY_KC" "$TEST_KC"; do
        if [ -f "$kc" ]; then
            security delete-keychain "$kc" 2>/dev/null || rm -f "$kc"
        fi
    done
    if [ -d "$ACCEPT/mnt" ] && mount | grep " on $ACCEPT/mnt " >/dev/null; then
        hdiutil detach "$ACCEPT/mnt" -force >/dev/null 2>&1 || true
    fi
    pkill -f "$DEMO/target/icm/dist/.*/Contents/MacOS/" 2>/dev/null || true
}
trap cleanup EXIT

# --- helpers --------------------------------------------------------------

# check_event RESULT FILTER: the run's events include a check matching FILTER.
check_event() {
    local dir
    dir=$(/usr/bin/jq -r .run_dir "$1")
    case "$dir" in /*) ;; *) dir="$DEMO/$dir" ;; esac
    /usr/bin/jq -e -s "[.[] | select(.type == \"check\") | select($2)] | length > 0" "$dir/events.ndjson" >/dev/null || {
        echo "no check event matching $2; the checks were:"
        /usr/bin/jq -r 'select(.type == "check") | "  \(.status) \(.id): \(.detail)"' "$dir/events.ndjson" | cut -c1-200
        return 1
    }
}

# abs RESULT KEY: a result path, absolute.
abs() {
    local path
    path=$(/usr/bin/jq -r "$2" "$1")
    case "$path" in /*) printf '%s\n' "$path" ;; *) printf '%s/%s\n' "$DEMO" "$path" ;; esac
}

# executable APP: the path of the bundle's executable.
executable() {
    printf '%s/Contents/MacOS/%s\n' "$1" "$(/usr/bin/plutil -extract CFBundleExecutable raw "$1/Contents/Info.plist")"
}

# launch APP: the bundle's executable draws its first frame (an ICM_EVENT
# line of kind ready) within 60 s; then it is stopped.
launch() {
    local app=$1 exe pid
    exe=$(executable "$app")
    ICM_EVENTS=1 "$exe" >"$ACCEPT/launch.out" 2>"$ACCEPT/launch.err" &
    pid=$!
    for _ in $(seq 1 120); do
        if grep -q '^ICM_EVENT .*"kind":"ready"' "$ACCEPT/launch.err"; then
            kill "$pid" 2>/dev/null || true
            wait "$pid" 2>/dev/null || true
            evidence "$(grep -m1 '"kind":"ready"' "$ACCEPT/launch.err" | cut -c1-160)"
            return 0
        fi
        if ! kill -0 "$pid" 2>/dev/null; then
            echo "the app exited before its first frame:"
            tail -20 "$ACCEPT/launch.err"
            return 1
        fi
        sleep 0.5
    done
    kill "$pid" 2>/dev/null || true
    echo "no ICM_EVENT ready event after 60 s"
    tail -20 "$ACCEPT/launch.err"
    return 1
}

# --- steps ----------------------------------------------------------------

install_icm() {
    cargo install --locked --path "$FORK/cli" --root "$ICM_ROOT"
    test "$(command -v icm)" = "$ICM_ROOT/bin/icm"
    evidence "$(icm --version)"
}

new_app() {
    icm new "$DEMO" --id com.example.demo --framework path:"$FORK" --json -q >"$ACCEPT/new.json"
    jqe '.ok' "$ACCEPT/new.json"
    cd "$DEMO"
    # Resolves Cargo.lock (releases build --locked) and compiles the desktop.
    icm check desktop --json -q >"$ACCEPT/check.json" || true
    jqe '.ok' "$ACCEPT/check.json"
    test -f "$DEMO/Cargo.lock"
    evidence "$(/usr/bin/jq -r '.summary' "$ACCEPT/new.json")"
}

keychains() {
    mkdir -p "$ACCEPT/keys"
    security create-keychain -p "$KC_PASS" "$EMPTY_KC"
    bash "$FORK/cli/tests/accept/test-identity.sh" "$TEST_KC" "$KC_PASS" "icm-test Phase5 Signing" >"$ACCEPT/keys/identity.txt"
    test "$(security list-keychains -d user)" = "$SEARCH_LIST_BEFORE"
    evidence "$(cat "$ACCEPT/keys/identity.txt" | tr '\n' ' ')"
}

# design §18 phase 5: without a Developer ID, a signed release is the owner's.
needs_developer_id() {
    cd "$DEMO"
    ICM_KEYCHAIN=$EMPTY_KC icm release macos --json -q >"$ACCEPT/release-auto.json" || true
    jqe '.exit == 9 and ([.errors[].id] | index("macos.sign.no_developer_id") != null)' "$ACCEPT/release-auto.json"
    test ! -d "$DEMO/target/icm/dist/0.1.0+1/macos"
    evidence "$(/usr/bin/jq -r '.summary' "$ACCEPT/release-auto.json" | cut -c1-200)"
}

unsigned_release() {
    cd "$DEMO"
    ICM_KEYCHAIN=$EMPTY_KC icm release macos --sign none --json -q >"$ACCEPT/release-none.json" || true
    jqe '.ok and .release.uploadable == false' "$ACCEPT/release-none.json"
    for id in macos.arch macos.min_os macos.bundle macos.sign.verify macos.hardened_runtime release.notices; do
        check_event "$ACCEPT/release-none.json" ".id == \"$id\" and .status == \"pass\""
    done
    check_event "$ACCEPT/release-none.json" '.id == "macos.gatekeeper" and .status == "info"'
    local app exe
    app=$(abs "$ACCEPT/release-none.json" .artifacts.app)
    exe=$(executable "$app")
    lipo -archs "$exe" | tee "$ACCEPT/archs.txt"
    grep -q arm64 "$ACCEPT/archs.txt"
    # Into files first: `grep -q` stops reading early, and a SIGPIPE in the
    # writer fails the step under pipefail.
    otool -l "$exe" >"$ACCEPT/otool.txt"
    grep -A3 LC_BUILD_VERSION "$ACCEPT/otool.txt" | grep 'minos 12.0'
    codesign -d -vv "$app" >"$ACCEPT/codesign-none.txt" 2>&1
    grep 'flags=0x10002(adhoc,runtime)' "$ACCEPT/codesign-none.txt"
    test -f "$app/Contents/Resources/AppIcon.icns"
    test -f "$app/Contents/Resources/THIRD_PARTY_NOTICES.txt"
    test -f "$(abs "$ACCEPT/release-none.json" .artifacts.app_zip)"
    evidence "$(/usr/bin/jq -r '.summary' "$ACCEPT/release-none.json" | cut -c1-200)"
    evidence "lipo -archs: $(cat "$ACCEPT/archs.txt"); minos 12.0; ad hoc with the hardened runtime"
}

launch_unsigned() {
    launch "$(abs "$ACCEPT/release-none.json" .artifacts.app)"
}

unsigned_dmg() {
    cd "$DEMO"
    ICM_KEYCHAIN=$EMPTY_KC icm release macos --dmg --sign none --json -q >"$ACCEPT/release-dmg.json" || true
    jqe '.ok and ([.warnings[].id] | index("macos.not_stapled") != null)' "$ACCEPT/release-dmg.json"
    check_event "$ACCEPT/release-dmg.json" '.id == "macos.dmg" and .status == "pass"'
    local dmg app
    dmg=$(abs "$ACCEPT/release-dmg.json" .artifacts.dmg)
    app=$(basename "$(abs "$ACCEPT/release-none.json" .artifacts.app)")
    mkdir -p "$ACCEPT/mnt"
    hdiutil attach -nobrowse -readonly -mountpoint "$ACCEPT/mnt" "$dmg"
    test -d "$ACCEPT/mnt/$app"
    test -L "$ACCEPT/mnt/Applications"
    codesign --verify --strict --deep "$ACCEPT/mnt/$app"
    hdiutil detach "$ACCEPT/mnt"
    evidence "$(basename "$dmg"): mounts with hdiutil attach -nobrowse; $app verifies inside"
}

verify_dmg() {
    cd "$DEMO"
    icm verify macos --json -q >"$ACCEPT/verify.json" || true
    jqe '.ok and .verify.sign == "none"' "$ACCEPT/verify.json"
    check_event "$ACCEPT/verify.json" '.id == "macos.min_os" and .status == "pass"'
    evidence "$(/usr/bin/jq -r '.summary' "$ACCEPT/verify.json")"
}

# The signed path with a throwaway identity: icm signs (no timestamp: it is
# not Apple's), every gate passes, and the release ends with the owner's
# item, since only a Developer ID can be notarized.
test_identity_release() {
    cd "$DEMO"
    local sha1
    sha1=$(sed -n 's/^sha1=//p' "$ACCEPT/keys/identity.txt")
    # Not the template's placeholders: those would stop the release first.
    sed -i '' 's/^id = "com.example.demo"/id = "dev.icm.phase5"/' icm.toml
    sed -i '' "s/^identity = \"auto\"/identity = \"$sha1\"/" icm.toml
    sips --rotate 90 "$DEMO/assets/icon.png" --out "$ACCEPT/icon.png" >/dev/null
    cp "$ACCEPT/icon.png" "$DEMO/assets/icon.png"
    ICM_KEYCHAIN=$TEST_KC icm release macos --json -q >"$ACCEPT/release-test-id.json" || true
    jqe '.exit == 9 and .errors[0].id == "macos.sign.no_developer_id" and .release.uploadable == false' "$ACCEPT/release-test-id.json"
    for id in macos.sign.verify macos.hardened_runtime macos.min_os macos.bundle; do
        check_event "$ACCEPT/release-test-id.json" ".id == \"$id\" and .status == \"pass\""
    done
    local app
    app=$(abs "$ACCEPT/release-test-id.json" .artifacts.app)
    codesign -d -vv "$app" >"$ACCEPT/codesign-d.txt" 2>&1
    grep -q 'Authority=icm-test Phase5 Signing' "$ACCEPT/codesign-d.txt"
    grep -q 'flags=0x10000(runtime)' "$ACCEPT/codesign-d.txt"
    codesign --verify --strict --deep "$app"
    test "$(security list-keychains -d user)" = "$SEARCH_LIST_BEFORE"
    evidence "$(grep -E '^(Authority|CodeDirectory)' "$ACCEPT/codesign-d.txt" | tr '\n' ' ' | cut -c1-200)"
}

launch_signed() {
    launch "$(abs "$ACCEPT/release-test-id.json" .artifacts.app)"
}

other_hosts() {
    cd "$DEMO"
    local target
    for target in windows linux; do
        icm release "$target" --sign none --json -q >"$ACCEPT/release-$target.json" || true
        jqe '.exit == 4 and .errors[0].id == "env.unsupported_host"' "$ACCEPT/release-$target.json"
        evidence "$target: $(/usr/bin/jq -r '.errors[0].detail' "$ACCEPT/release-$target.json" | cut -c1-160)"
    done
}

search_list_unchanged() {
    cleanup
    test "$(security list-keychains -d user)" = "$SEARCH_LIST_BEFORE"
    test ! -f "$TEST_KC" && test ! -f "$EMPTY_KC"
    evidence "the user's keychain search list is as before; the test keychains are deleted"
}

main() {
    must install install_icm
    must new new_app
    must keychains keychains
    step needs-developer-id needs_developer_id
    must release-unsigned unsigned_release
    step launch-unsigned launch_unsigned
    step dmg-unsigned unsigned_dmg
    step verify-dmg verify_dmg
    step release-test-identity test_identity_release
    step launch-signed launch_signed
    step windows-linux-other-hosts other_hosts
    step keychains-cleaned search_list_unchanged
    skip notarize-and-staple "the owner's (UPLOAD.md); icm never notarizes"
    skip dmg-of-stapled-app "the owner's: icm release macos --dmg after stapling"
    skip verify-after-notarize "the owner's: icm verify macos --after-notarize"
    finish
}

main "$@"
