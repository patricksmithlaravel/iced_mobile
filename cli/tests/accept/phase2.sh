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
# live in $ACCEPT. The store-screenshot steps create icm's managed
# icm-iphone-<n>-pro-max-ios-<version> simulator when it is missing and
# shut it down at the end.
#
# Signing material is throwaway and stays in $ACCEPT. icm only reads
# signing assets, and this script never lets it see the user's: unless
# set, ICM_KEYCHAIN names a keychain file that does not exist (no
# identities) and ICM_PROVISIONING_PROFILES an empty directory, so the
# releases stop for the owner's items as on a fresh host. The signed path
# runs with a self-signed "Apple Distribution: icm test (ICMTEST001)"
# identity in a temporary keychain (test-identity.sh; it never joins the
# user's search list, a step checks the list is unchanged, and it is
# deleted at the end) and a fake App Store profile for ICMTEST001 in a CMS
# envelope signed by a throwaway key. The release signs and gates the app
# with them and ends with exit 9, since no Apple service trusts that
# certificate.
#
# Owner steps (never run here; printed as SKIP):
#   - a signed release with the owner's id, team, App Store profile and
#     Apple Distribution identity: icm release ios
#   - upload.sh, the build processed in TestFlight (internal), then
#     icm ledger mark-uploaded ios
# A run on a physical iPhone is SKIP too; set ICM_ACCEPT_DEVICE=1 with a
# development-provisioned iPhone connected (and ICM_KEYCHAIN and
# ICM_PROVISIONING_PROFILES pointing at the owner's development assets) to
# run on it.
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
# The user's keychains and Xcode's profile directories stay out of reach.
export ICM_KEYCHAIN=${ICM_KEYCHAIN:-$ACCEPT/keys/no-signing.keychain-db}
export ICM_PROVISIONING_PROFILES=${ICM_PROVISIONING_PROFILES:-$ACCEPT/keys/no-profiles}
mkdir -p "$ICM_CACHE_DIR" "$(dirname "$ICM_HOST_CONFIG")" "$ACCEPT/keys"
mkdir -p "$ICM_PROVISIONING_PROFILES" 2>/dev/null || true
DEMO="$ACCEPT/demo"
# The signed path's throwaway material.
TEST_TEAM=ICMTEST001
TEST_IDENTITY="Apple Distribution: icm test ($TEST_TEAM)"
TEST_KC="$ACCEPT/keys/ios-test.keychain-db"
TEST_PROFILES="$ACCEPT/keys/test-profiles"
KC_PASS="phase2-throwaway-$$"
SEARCH_LIST_BEFORE=$(security list-keychains -d user)
cd "$ACCEPT"

echo "fork: $FORK"
echo "outputs: $ACCEPT"
echo "keychain: $ICM_KEYCHAIN; profiles: $ICM_PROVISIONING_PROFILES"

cleanup() {
    if [ -f "$DEMO/icm.toml" ] && command -v icm >/dev/null; then
        (cd "$DEMO" && icm stop --all --shutdown --json -q >"$ACCEPT/cleanup.json" 2>&1) || true
    fi
    if [ -f "$TEST_KC" ]; then
        security delete-keychain "$TEST_KC" 2>/dev/null || rm -f "$TEST_KC"
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

# --- signed with throwaway material ----------------------------------------

# A self-signed Apple Distribution identity in a temporary keychain, and a
# fake App Store profile for ICMTEST001.dev.accept.ios that holds its
# certificate, in a CMS envelope (Apple signs real ones; here a throwaway
# key that is deleted once used).
test_signing_material() {
    rm -rf "$TEST_PROFILES" "$ACCEPT/keys/profile.uuid"
    mkdir -p "$TEST_PROFILES"
    bash "$FORK/cli/tests/accept/test-identity.sh" "$TEST_KC" "$KC_PASS" "$TEST_IDENTITY" >"$ACCEPT/keys/identity.txt"
    test "$(security list-keychains -d user)" = "$SEARCH_LIST_BEFORE"
    local sha1 der uuid now expires
    sha1=$(sed -n 's/^sha1=//p' "$ACCEPT/keys/identity.txt")
    security find-certificate -c "$TEST_IDENTITY" -p "$TEST_KC" >"$ACCEPT/keys/distribution.pem"
    test "$(/usr/bin/openssl x509 -in "$ACCEPT/keys/distribution.pem" -noout -fingerprint -sha1 | sed 's/.*=//; s/://g')" = "$sha1"
    der=$(/usr/bin/openssl x509 -in "$ACCEPT/keys/distribution.pem" -outform DER | base64 | tr -d '\n')
    uuid=$(uuidgen)
    now=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    expires=$(date -u -v+180d +%Y-%m-%dT%H:%M:%SZ)
    cat >"$ACCEPT/keys/profile.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>AppIDName</key>
	<string>icm test</string>
	<key>ApplicationIdentifierPrefix</key>
	<array><string>$TEST_TEAM</string></array>
	<key>CreationDate</key>
	<date>$now</date>
	<key>Platform</key>
	<array><string>iOS</string></array>
	<key>DeveloperCertificates</key>
	<array><data>$der</data></array>
	<key>Entitlements</key>
	<dict>
		<key>application-identifier</key>
		<string>$TEST_TEAM.dev.accept.ios</string>
		<key>keychain-access-groups</key>
		<array><string>$TEST_TEAM.*</string></array>
		<key>get-task-allow</key>
		<false/>
		<key>com.apple.developer.team-identifier</key>
		<string>$TEST_TEAM</string>
		<key>beta-reports-active</key>
		<true/>
	</dict>
	<key>ExpirationDate</key>
	<date>$expires</date>
	<key>Name</key>
	<string>icm test App Store</string>
	<key>TeamIdentifier</key>
	<array><string>$TEST_TEAM</string></array>
	<key>TeamName</key>
	<string>icm test</string>
	<key>TimeToLive</key>
	<integer>180</integer>
	<key>UUID</key>
	<string>$uuid</string>
	<key>Version</key>
	<integer>1</integer>
</dict>
</plist>
EOF
    plutil -lint "$ACCEPT/keys/profile.plist"
    /usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj "/CN=icm test profile signer" \
        -keyout "$ACCEPT/keys/cms.key" -out "$ACCEPT/keys/cms.pem" >/dev/null 2>&1
    /usr/bin/openssl smime -sign -binary -nodetach -outform DER -signer "$ACCEPT/keys/cms.pem" \
        -inkey "$ACCEPT/keys/cms.key" -in "$ACCEPT/keys/profile.plist" -out "$TEST_PROFILES/$uuid.mobileprovision"
    rm -f "$ACCEPT/keys/cms.key"
    # A CMS envelope whose content is the plist.
    /usr/bin/openssl smime -verify -noverify -inform DER -in "$TEST_PROFILES/$uuid.mobileprovision" \
        -out "$ACCEPT/keys/profile.decoded.plist" 2>/dev/null
    cmp -s "$ACCEPT/keys/profile.plist" "$ACCEPT/keys/profile.decoded.plist"
    echo "$uuid" >"$ACCEPT/keys/profile.uuid"
    evidence "\"$TEST_IDENTITY\" ($sha1) in $TEST_KC; App Store profile $uuid for $TEST_TEAM.dev.accept.ios, expires $expires"
}

# The signed path: the identity and the profile are found, the app is
# signed with the distribution entitlements and every gate passes; the
# release still ends with the owner's item, an identity no Apple service
# trusts (exit 9, ios.sign.no_identity).
release_test_signed() {
    cd "$DEMO"
    local sha1
    sha1=$(sed -n 's/^sha1=//p' "$ACCEPT/keys/identity.txt")
    cp icm.toml "$ACCEPT/icm.toml.signed.bak"
    cp assets/icon.png "$ACCEPT/icon.png.signed.bak"
    # Steps run in a subshell: its exit puts the app back.
    trap 'cp "$ACCEPT/icm.toml.signed.bak" "$DEMO/icm.toml"; cp "$ACCEPT/icon.png.signed.bak" "$DEMO/assets/icon.png"' EXIT
    # Not the template's placeholders and unanswered questions: those
    # would stop the release before the build.
    sed -i '' 's/^id = "com.example.demo"/id = "dev.accept.ios"/' icm.toml
    sed -i '' "s/^# team_id = .*/team_id = \"$TEST_TEAM\"/" icm.toml
    sed -i '' 's/^# uses_non_exempt_encryption = .*/uses_non_exempt_encryption = false/' icm.toml
    sed -i '' "s/^distribution = { identity = \"auto\"/distribution = { identity = \"$sha1\"/" icm.toml
    grep -q "^distribution = { identity = \"$sha1\"" icm.toml
    sips --rotate 90 assets/icon.png --out "$ACCEPT/icon-turned.png" >/dev/null
    cp "$ACCEPT/icon-turned.png" assets/icon.png
    ICM_KEYCHAIN=$TEST_KC ICM_PROVISIONING_PROFILES=$TEST_PROFILES \
        icm release ios --allow-dirty --json -q >"$ACCEPT/s.json" || true
    jqe '.exit == 9 and ([.errors[].id] == ["ios.sign.no_identity"]) and (.errors[0].detail | test("CSSMERR_TP_NOT_TRUSTED"))' "$ACCEPT/s.json"
    # The owner's item is the only failed check.
    jqe '.checks.failed == ["ios.sign.no_identity"] and .release.uploadable == false and (.artifacts.ipa | test("\\.ipa$"))' "$ACCEPT/s.json"
    for id in app.id.placeholder ios.sign.no_profile ios.entitlements.not_in_profile ios.sign.verify ios.entitlements.get_task_allow \
        ios.plist.export_compliance ios.plist.dt_keys ios.privacy.reasons ios.icon.opaque_1024 ios.dsym.uuid ios.dsym.line_tables \
        ios.macho.platform ios.macho.minos ios.macho.arch ios.ipa.layout ios.ipa.signature release.notices; do
        check_event "$ACCEPT/s.json" "$id" pass
    done
    evidence "exit 9: $(/usr/bin/jq -r '.errors[0].detail' "$ACCEPT/s.json" | cut -c1-200)"
}

# What the signed IPA holds: the identity's signature, the distribution
# entitlements and the profile, embedded byte for byte.
test_signed_ipa() {
    local ipa app sha1 uuid
    ipa=$(abs "$(/usr/bin/jq -r .artifacts.ipa "$ACCEPT/s.json")")
    sha1=$(sed -n 's/^sha1=//p' "$ACCEPT/keys/identity.txt")
    uuid=$(cat "$ACCEPT/keys/profile.uuid")
    rm -rf "$ACCEPT/signed-ipa"
    unzip -q "$ipa" -d "$ACCEPT/signed-ipa"
    app=$(echo "$ACCEPT/signed-ipa/Payload/"*.app)
    codesign -dvv "$app" >"$ACCEPT/signed-codesign.txt" 2>&1
    grep -qx "Authority=$TEST_IDENTITY" "$ACCEPT/signed-codesign.txt"
    grep -qx 'Identifier=dev.accept.ios' "$ACCEPT/signed-codesign.txt"
    codesign --verify --strict --deep "$app"
    codesign -d --entitlements - --xml "$app" 2>/dev/null >"$ACCEPT/signed-entitlements.plist"
    test "$(plutil -extract application-identifier raw "$ACCEPT/signed-entitlements.plist")" = "$TEST_TEAM.dev.accept.ios"
    test "$(plutil -extract get-task-allow raw "$ACCEPT/signed-entitlements.plist")" = false
    test "$(plutil -extract beta-reports-active raw "$ACCEPT/signed-entitlements.plist")" = true
    cmp -s "$app/embedded.mobileprovision" "$TEST_PROFILES/$uuid.mobileprovision"
    test "$(plutil -extract CFBundleIdentifier raw "$app/Info.plist")" = dev.accept.ios
    test "$(plutil -extract ITSAppUsesNonExemptEncryption raw "$app/Info.plist")" = false
    /usr/bin/jq -e --arg sha1 "$sha1" --arg uuid "$uuid" \
        '.signing.identity_sha1 == $sha1 and .signing.profile.uuid == $uuid and .signing.profile.type == "app-store"' \
        "$(dirname "$ipa")/artifacts.json" >/dev/null
    evidence "$(grep -E '^(Authority|TeamIdentifier|CodeDirectory)' "$ACCEPT/signed-codesign.txt" | tr '\n' ' ' | cut -c1-200)"
    evidence "entitlements: application-identifier $TEST_TEAM.dev.accept.ios, get-task-allow false, beta-reports-active true; embedded.mobileprovision is the test profile"
}

verify_test_signed() {
    local ipa
    ipa=$(abs "$(/usr/bin/jq -r .artifacts.ipa "$ACCEPT/s.json")")
    (cd "$DEMO" && icm verify ios --artifact "$ipa" --json -q) >"$ACCEPT/sv.json" || true
    jqe '.ok and .checks.fail == 0' "$ACCEPT/sv.json"
    check_event "$ACCEPT/sv.json" ios.sign.no_identity pass
    check_event "$ACCEPT/sv.json" ios.sign.no_profile pass
    evidence "$(/usr/bin/jq -r '.summary' "$ACCEPT/sv.json")"
}

keychain_cleaned() {
    if [ -f "$TEST_KC" ]; then
        security delete-keychain "$TEST_KC"
    fi
    test ! -e "$TEST_KC"
    # The default keychain of the other steps was only ever a name.
    if [ "$ICM_KEYCHAIN" = "$ACCEPT/keys/no-signing.keychain-db" ]; then
        test ! -e "$ICM_KEYCHAIN"
    fi
    test "$(security list-keychains -d user)" = "$SEARCH_LIST_BEFORE"
    evidence "the user's keychain search list is as before; the test keychain is deleted"
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
    step test-signing-material test_signing_material
    if [ -f "$ACCEPT/keys/profile.uuid" ]; then
        step release-test-signed release_test_signed
        step test-signed-ipa test_signed_ipa
        step verify-test-signed verify_test_signed
    else
        skip release-test-signed "no throwaway signing material"
    fi
    step keychain-cleaned keychain_cleaned
    skip owner-signed-release "owner: a real id, team, App Store profile and Apple Distribution identity, then icm release ios"
    skip owner-upload "owner: upload.sh, the build processed in TestFlight, icm ledger mark-uploaded ios"
    finish
}

main "$@"
