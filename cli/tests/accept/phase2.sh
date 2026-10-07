#!/usr/bin/env bash
# Phase 2 acceptance (docs/icm/DESIGN.md §18 "Phase 2", with Appendix C
# items 1, 19 and 23): the App Store release of the template without the
# owner's signing assets, the gates on good and broken inputs, App Store
# screenshots, and the ios-device commands that need no device.
#
#   cli/tests/accept/phase2.sh
#
# A few minutes on an M4 Max with cargo's registry cache warm. Needs macOS
# with Xcode 26 or newer, /usr/bin/jq and the network for the
# first `icm doctor --fix --yes`.
#
# Outputs go to $ACCEPT (default: a new temporary directory); icm is
# installed into $ICM_ROOT (default $ACCEPT/icm), its cache and host.toml
# live in $ACCEPT. icm only reads signing assets: set ICM_KEYCHAIN to a
# keychain file and ICM_PROVISIONING_PROFILES to a directory to keep it
# away from the user's keychain search list and Xcode's profiles (the
# agent acceptance needs none of them; it expects the owner's items to be
# missing). The store-screenshot steps create icm's managed
# icm-iphone-<n>-pro-max-ios-<version> simulator when it is missing and
# shut it down at the end.
#
# Owner steps (a signed release with real assets, upload.sh, TestFlight,
# the ledger) and a run on a physical iPhone are printed as SKIP; set
# ICM_ACCEPT_DEVICE=1 with a development-provisioned iPhone connected to
# run on it too.
set -euo pipefail

FORK=${FORK:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)}
F="$FORK/cli/tests/fixtures"
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
DEMO="$ACCEPT/demo"
cd "$ACCEPT"

echo "fork: $FORK"
echo "outputs: $ACCEPT"
echo "keychain: ${ICM_KEYCHAIN:-the keychain search list, read only}; profiles: ${ICM_PROVISIONING_PROFILES:-the Xcode profile directories, read only}"

cleanup() {
    if [ -f "$DEMO/icm.toml" ] && command -v icm >/dev/null; then
        (cd "$DEMO" && icm stop --all --shutdown --json -q >"$ACCEPT/cleanup.json" 2>&1) || true
    fi
}
trap cleanup EXIT

# check_event RESULT ID STATUS: the run's events hold a check ID with STATUS.
check_event() {
    local dir
    dir=$(/usr/bin/jq -r .run_dir "$1")
    case "$dir" in /*) ;; *) dir="$DEMO/$dir" ;; esac
    /usr/bin/jq -e -s --arg id "$2" --arg status "$3" \
        '[.[] | select(.type == "check" and .id == $id and .status == $status)] | length > 0' \
        "$dir/events.ndjson" >/dev/null || {
        echo "no $3 check $2 in $dir/events.ndjson; its checks:"
        /usr/bin/jq -r 'select(.type == "check") | "  \(.status) \(.id): \(.detail)"' "$dir/events.ndjson" | cut -c1-160
        return 1
    }
}

# abs PATH: a result path, absolute (results name paths inside the app
# relative to it).
abs() {
    case "$1" in /*) printf '%s\n' "$1" ;; *) printf '%s/%s\n' "$DEMO" "$1" ;; esac
}

# --- setup ----------------------------------------------------------------

install_icm() {
    cargo install --locked --path "$FORK/cli" --root "$ICM_ROOT"
    test "$(command -v icm)" = "$ICM_ROOT/bin/icm"
    evidence "$(icm --version)"
}

new_app() {
    icm new "$DEMO" --id com.example.demo --framework path:"$FORK" --json -q >"$ACCEPT/new.json"
    jqe '.ok' "$ACCEPT/new.json"
    evidence "$(/usr/bin/jq -r '.summary' "$ACCEPT/new.json")"
}

# The app's toolchain, its aarch64-apple-ios target and Cargo.lock.
doctor_app() {
    cd "$DEMO"
    icm doctor ios-sim ios-device --fix --yes --json -q >"$ACCEPT/doctor.json" || true
    jqe '.exit == 0 or .exit == 9' "$ACCEPT/doctor.json"
    icm check ios-device --json -q >"$ACCEPT/check.json"
    jqe '.ok' "$ACCEPT/check.json"
    test -f Cargo.lock
    evidence "$(/usr/bin/jq -r '.summary' "$ACCEPT/check.json")"
}

# --- the release ------------------------------------------------------------

# Signed: the owner's items stop it before the build (exit 9, by: owner).
release_signed_stops() {
    cd "$DEMO"
    icm release ios --json -q >"$ACCEPT/r.json" || true
    jqe '.exit == 9 and .errors[0].fix.by == "owner"' "$ACCEPT/r.json"
    jqe '(.owner_steps | length) > 0' "$ACCEPT/r.json"
    evidence "errors: $(/usr/bin/jq -c '[.errors[].id]' "$ACCEPT/r.json")"
}

release_unsigned() {
    cd "$DEMO"
    icm release ios --sign none --allow-dirty --json -q >"$ACCEPT/u.json"
    jqe '.ok and (.artifacts.ipa | test("\\.ipa$")) and ([.warnings[].id] | index("config.owner_decision") != null)' "$ACCEPT/u.json"
    jqe '.release.uploadable == false and (.checks.failed | length) == 0' "$ACCEPT/u.json"
    for id in ios.dsym.uuid ios.dsym.line_tables ios.icon.opaque_1024 ios.privacy.reasons ios.plist.dt_keys ios.ipa.layout ios.ipa.signature release.notices; do
        check_event "$ACCEPT/u.json" "$id" pass
    done
    evidence "$(/usr/bin/jq -r '.summary' "$ACCEPT/u.json")"
    evidence "warnings: $(/usr/bin/jq -c '[.warnings[].id]' "$ACCEPT/u.json")"
}

ipa_layout() {
    local ipa
    ipa=$(abs "$(/usr/bin/jq -r .artifacts.ipa "$ACCEPT/u.json")")
    if zipinfo -1 "$ipa" | grep -E '(^|/)\._|__MACOSX'; then
        return 1
    fi
    zipinfo -1 "$ipa" | grep -q '^Payload/[^/]*\.app/THIRD_PARTY_NOTICES\.txt$'
    evidence "$(zipinfo -1 "$ipa" | wc -l | tr -d ' ') entries, all under Payload/"
}

verify_ipa() {
    local ipa
    ipa=$(abs "$(/usr/bin/jq -r .artifacts.ipa "$ACCEPT/u.json")")
    (cd "$DEMO" && icm verify ios --artifact "$ipa" --json -q) >"$ACCEPT/v.json"
    jqe '.ok' "$ACCEPT/v.json"
    evidence "$(/usr/bin/jq -r '.summary' "$ACCEPT/v.json")"
}

ipa_contents() {
    local ipa build
    ipa=$(abs "$(/usr/bin/jq -r .artifacts.ipa "$ACCEPT/u.json")")
    rm -rf "$ACCEPT/ipa"
    unzip -q -o "$ipa" -d "$ACCEPT/ipa"
    build=$(xcodebuild -version | awk '/Build version/{print $3}')
    plutil -extract DTXcodeBuild raw "$ACCEPT/ipa/Payload/"*.app/Info.plist | grep -qx "$build"
    plutil -extract UIDeviceFamily.0 raw "$ACCEPT/ipa/Payload/"*.app/Info.plist | grep -qx 1
    xcrun assetutil --info "$ACCEPT/ipa/Payload/"*.app/Assets.car | grep -q '"Opaque" : true'
    grep -q C617.1 "$ACCEPT/ipa/Payload/"*.app/PrivacyInfo.xcprivacy
    evidence "DTXcodeBuild $build, UIDeviceFamily [1], AppIcon opaque, C617.1 declared"
}

# A second unsigned build of the same tree is the same IPA.
reproducible() {
    local ipa first second
    ipa=$(abs "$(/usr/bin/jq -r .artifacts.ipa "$ACCEPT/u.json")")
    first=$(shasum -a 256 "$ipa" | cut -d' ' -f1)
    (cd "$DEMO" && icm release ios --sign none --allow-dirty --json -q) >"$ACCEPT/u2.json"
    jqe '.ok' "$ACCEPT/u2.json"
    second=$(shasum -a 256 "$ipa" | cut -d' ' -f1)
    test "$first" = "$second"
    evidence "sha256 $first both times"
}

# An RGBA icon is flattened: the opaque gate passes and the placeholder
# WARN is gone.
rgba_icon() {
    cd "$DEMO"
    cp "$F/icon-rgba-1024.png" assets/icon.png
    icm release ios --sign none --allow-dirty --json -q >"$ACCEPT/rgba.json"
    jqe '.ok and ([.warnings[].id] | index("app.icon.placeholder") == null)' "$ACCEPT/rgba.json"
    check_event "$ACCEPT/rgba.json" ios.icon.opaque_1024 pass
    git -C "$DEMO" checkout -- assets/icon.png 2>/dev/null || cp "$FORK/examples/app/assets/icon.png" assets/icon.png
    evidence "RGBA source icon: ios.icon.opaque_1024 PASS"
}

# Broken inputs fail their gates: an undeclared required-reason category
# (the template imports _stat and _mach_absolute_time).
privacy_gate() {
    cd "$DEMO"
    cp icm.toml "$ACCEPT/icm.toml.bak"
    sed -i '' 's/^api_reasons = .*/api_reasons = { FileTimestamp = ["C617.1"] }/' icm.toml
    icm release ios --sign none --allow-dirty --json -q >"$ACCEPT/privacy.json" || true
    cp "$ACCEPT/icm.toml.bak" icm.toml
    jqe '.exit == 1 and (.checks.failed | index("ios.privacy.reasons") != null)' "$ACCEPT/privacy.json"
    jqe '[.errors[] | select(.id == "ios.privacy.reasons") | .fix.summary | test("SystemBootTime = \\[\"35F9.1\"\\]")] | any' "$ACCEPT/privacy.json"
    evidence "SystemBootTime undeclared: ios.privacy.reasons FAIL with the api_reasons line"
}

# A min_os below the App Store floor is refused before the build.
min_os_floor() {
    cd "$DEMO"
    cp icm.toml "$ACCEPT/icm.toml.bak"
    sed -i '' 's/^min_os = "16.0"/min_os = "12.0"/' icm.toml
    icm release ios --sign none --allow-dirty --json -q >"$ACCEPT/minos.json" || true
    cp "$ACCEPT/icm.toml.bak" icm.toml
    jqe '.exit == 3' "$ACCEPT/minos.json"
    evidence "min_os 12.0: $(/usr/bin/jq -r '.errors[0].id' "$ACCEPT/minos.json")"
}

diagnose_altool() {
    cd "$DEMO"
    printf '%s\n' '{"product-errors":[{"code":-19208,"message":"Validation failed","userInfo":{"NSLocalizedFailureReason":"Invalid large app icon. (ITMS-90717)"}}]}' >"$ACCEPT/validate.json"
    icm diagnose altool "$ACCEPT/validate.json" --json -q >"$ACCEPT/diag.json" || true
    jqe '.exit == 1 and .errors[0].id == "ios.icon.opaque_1024"' "$ACCEPT/diag.json"
    printf '%s\n' '{"success-message":"No errors uploading","details":{"delivery-uuid":"00000000-0000-4000-8000-000000000000"}}' >"$ACCEPT/upload.json"
    icm diagnose altool "$ACCEPT/upload.json" --json -q >"$ACCEPT/diag2.json"
    jqe '.ok and .delivery_id == "00000000-0000-4000-8000-000000000000"' "$ACCEPT/diag2.json"
    evidence "ITMS-90717 -> ios.icon.opaque_1024; the delivery id is read"
}

upload_md() {
    local dir
    dir=$(abs "$(/usr/bin/jq -r .artifacts.dist "$ACCEPT/u.json")")
    grep -q -- '--build-status --apple-id' "$dir/UPLOAD.md"
    grep -q 'Transporter' "$dir/UPLOAD.md"
    grep -q 'icm shot ios-sim --store' "$dir/UPLOAD.md"
    bash -n "$dir/upload.sh"
    # Not uploadable: upload.sh refuses before it reads any variable.
    rc=0
    bash "$dir/upload.sh" >/dev/null 2>&1 || rc=$?
    test "$rc" -eq 9
    evidence "UPLOAD.md has altool, build-status polling, Transporter; upload.sh exits 9"
}

# --- screenshots and devices ------------------------------------------------

store_screenshots() {
    cd "$DEMO"
    icmd run ios-sim --store >"$ACCEPT/store-run.json"
    jqe '.ok and (.device.type | test("Pro Max"))' "$ACCEPT/store-run.json"
    icm shot ios-sim --store --name home --json -q >"$ACCEPT/store-shot.json"
    jqe '.ok and (.store.class == "6.9-inch" or .store.class == "6.5-inch")' "$ACCEPT/store-shot.json"
    local png
    png=$(abs "$(/usr/bin/jq -r '.artifacts.store_screenshot' "$ACCEPT/store-shot.json")")
    is_png "$png"
    test "$(od -An -tu1 -j25 -N1 "$png" | tr -d ' ')" = 2
    icm stop ios-sim --shutdown --json -q >"$ACCEPT/store-stop.json"
    evidence "$(/usr/bin/jq -r '.device.name' "$ACCEPT/store-run.json"): $(png_size "$png" | tr ' ' x) RGB at $png"
}

device_commands() {
    cd "$DEMO"
    icm devices ios-device --json -q >"$ACCEPT/devices.json"
    jqe '.ok' "$ACCEPT/devices.json"
    icm run ios-device --dry-run --json -q >"$ACCEPT/device-plan.json"
    jqe '.ok and ([.plan[].name] | index("devicectl.launch") != null)' "$ACCEPT/device-plan.json"
    icm input ios-device tap 1 1 --json -q >"$ACCEPT/device-input.json" || true
    jqe '.exit == 2 and .errors[0].id == "input.unsupported"' "$ACCEPT/device-input.json"
    evidence "$(/usr/bin/jq -r '.summary' "$ACCEPT/devices.json")"
}

device_run() {
    cd "$DEMO"
    icmd run ios-device >"$ACCEPT/device-run.json"
    jqe '.ok' "$ACCEPT/device-run.json"
    icm stop ios-device --json -q >"$ACCEPT/device-stop.json"
    evidence "$(/usr/bin/jq -r '.summary' "$ACCEPT/device-run.json")"
}

main() {
    must install install_icm
    must new new_app
    must doctor-app doctor_app
    step release-signed-stops-for-owner release_signed_stops
    must release-unsigned release_unsigned
    step ipa-layout ipa_layout
    step verify-ipa verify_ipa
    step ipa-contents ipa_contents
    step reproducible reproducible
    step rgba-icon rgba_icon
    step privacy-gate privacy_gate
    step min-os-floor min_os_floor
    step diagnose-altool diagnose_altool
    step upload-md upload_md
    step store-screenshots store_screenshots
    step device-commands device_commands
    if [ "${ICM_ACCEPT_DEVICE:-}" = 1 ]; then
        step device-run device_run
    else
        skip device-run "no ICM_ACCEPT_DEVICE=1 (needs a provisioned iPhone and the owner's development certificate)"
    fi
    skip owner-signed-release "owner: a real id, team, App Store profile and Apple Distribution identity, then icm release ios"
    skip owner-upload "owner: upload.sh, the build processed in TestFlight, icm ledger mark-uploaded ios"
    finish
}

main "$@"
