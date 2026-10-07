#!/usr/bin/env bash
# Phase 3 acceptance (docs/icm/DESIGN.md §18, phase 3): the Google Play
# release and the lifecycle suite, on this Mac with the managed Android
# emulator.
#
#   cli/tests/accept/phase3.sh
#
# Test values only: the upload keystore is a throwaway PKCS12 file in
# $ACCEPT whose password is random and lives only in this script's
# environment (ICM_TEST_STOREPASS). Nothing is uploaded: the owner's Play
# Console steps (the first manual upload, a later `fastlane supply` draft)
# are reported as SKIP.
#
# Like phase1.sh, icm is installed into $ICM_ROOT (default $ACCEPT/icm), and
# icm's cache (bundletool is downloaded there through the pinned-tool
# table), its host.toml and debug keystore, and Android's per-user
# directories (the managed AVD) live in $ACCEPT unless already set. The
# emulator still writes ~/.android/modem-nv-ram-<port>. The run stops early
# when an Android device other than an icm- emulator is online. It builds
# the template for arm64-v8a and x86_64 in release: allow ten minutes and
# more on a cold cache; run it in the background.
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
export ANDROID_USER_HOME=${ANDROID_USER_HOME:-$ACCEPT/android-home}
export ANDROID_AVD_HOME=${ANDROID_AVD_HOME:-$ANDROID_USER_HOME/avd}
mkdir -p "$ICM_CACHE_DIR" "$(dirname "$ICM_HOST_CONFIG")" "$ANDROID_AVD_HOME" "$ACCEPT/previews"
unset ANDROID_SERIAL
DEMO="$ACCEPT/demo"
ICM_TEST_STOREPASS="$(openssl rand -hex 16)"
export ICM_TEST_STOREPASS
cd "$ACCEPT"

echo "fork: $FORK"
echo "outputs: $ACCEPT"

cleanup() {
    if [ -f "$DEMO/icm.toml" ] && command -v icm >/dev/null; then
        (cd "$DEMO" && icm stop --all --shutdown --json -q >"$ACCEPT/cleanup.json" 2>&1) || true
    fi
}
trap cleanup EXIT

keep() {
    if [ -n "$1" ] && [ "$1" != null ] && [ -f "$1" ]; then
        cp "$1" "$ACCEPT/previews/$2"
    fi
}

# sdk_adb, java_home and no_foreign_android are lib.sh's: they read icm's
# Android environment from `icm print env android --json`.

# bundletool, as icm runs it: the pinned jar in icm's cache.
bundletool() {
    local java
    java="$(java_home)/bin/java"
    "$java" -jar "$ICM_CACHE_DIR/tools/bundletool/1.18.3/bundletool-all-1.18.3.jar" "$@"
}

# --- setup -----------------------------------------------------------------------

install_icm() {
    cargo install --locked --path "$FORK/cli" --root "$ICM_ROOT"
    test "$(command -v icm)" = "$ICM_ROOT/bin/icm"
    evidence "$(icm --version)"
}

doctor_android() {
    icmd doctor android --fix --yes >"$ACCEPT/doctor.json" || true
    jqe '.exit == 0' "$ACCEPT/doctor.json"
    test -f "$ICM_CACHE_DIR/tools/bundletool/1.18.3/bundletool-all-1.18.3.jar"
    evidence "$(/usr/bin/jq -r '.summary' "$ACCEPT/doctor.json")"
}


# The app: a real id (not the placeholder) and an icon that is not the
# template's (the template's, turned), with a throwaway upload key.
new_app() {
    icm new "$DEMO" --id dev.accept.demo --framework path:"$FORK" --json -q >"$ACCEPT/new.json"
    jqe '.ok' "$ACCEPT/new.json"
    cd "$DEMO"
    sips -r 90 assets/icon.png >/dev/null
    "$(java_home)/bin/keytool" -genkeypair -keystore "$ACCEPT/up.jks" -storetype PKCS12 -alias upload \
        -keyalg RSA -keysize 2048 -validity 365 -dname CN=test \
        -storepass:env ICM_TEST_STOREPASS -keypass:env ICM_TEST_STOREPASS </dev/null
    cat >>icm.toml <<EOF

[android.signing]
upload = { keystore = "$ACCEPT/up.jks", alias = "upload", store_pass_env = "ICM_TEST_STOREPASS" }
EOF
    icmd check android >"$ACCEPT/check.json" || true
    jqe '.ok' "$ACCEPT/check.json"
    evidence "dev.accept.demo with a throwaway upload key in $ACCEPT/up.jks"
}

# The managed emulator, booted by a dev run (the release's smoke install
# then uses it).
run_android() {
    cd "$DEMO"
    icmd run android >"$ACCEPT/run.json" || true
    keep "$(/usr/bin/jq -r '.artifacts.preview // empty' "$ACCEPT/run.json")" run.preview.png
    jqe '.ok and .process.ready.source == "icm_event"' "$ACCEPT/run.json"
    evidence "$(/usr/bin/jq -r '.summary' "$ACCEPT/run.json")"
}

# --- the release -------------------------------------------------------------------

release_signed() {
    cd "$DEMO"
    icmd release android --allow-dirty >"$ACCEPT/a.json" || true
    keep "$(/usr/bin/jq -r '.artifacts.screenshot // empty' "$ACCEPT/a.json")" smoke.png
    jqe '.ok and ((.checks.failed | length) == 0) and .release.signed and .release.uploadable' "$ACCEPT/a.json"
    check_smoke "$ACCEPT/a.json"
    local aab
    aab=$(/usr/bin/jq -r .artifacts.aab "$ACCEPT/a.json")
    bundletool dump config --bundle="$aab" | grep -q PAGE_ALIGNMENT_16K
    bundletool dump manifest --bundle="$aab" | grep -q 'targetSdkVersion="36"'
    if bundletool dump manifest --bundle="$aab" | grep -q debuggable; then
        echo "the release manifest is debuggable"
        return 1
    fi
    "$(java_home)/bin/jarsigner" -verify "$aab" | grep -q 'jar verified.'
    evidence "$(/usr/bin/jq -r '.summary' "$ACCEPT/a.json" | sed "s|$ACCEPT|\$ACCEPT|g")"
    evidence "$aab: $(stat -f %z "$aab") bytes, PAGE_ALIGNMENT_16K, targetSdk 36, jar verified"
}

# The smoke install ran on the emulator and drew.
check_smoke() {
    local dir
    dir=$(/usr/bin/jq -r .run_dir "$1")
    /usr/bin/jq -e -s '[.[] | select(.type == "check" and .id == "android.smoke" and .status == "pass")] | length == 1' "$dir/events.ndjson" >/dev/null
    evidence "smoke: $(/usr/bin/jq -r -s '[.[] | select(.type == "check" and .id == "android.smoke")][0].detail' "$dir/events.ndjson" | cut -c1-180)"
}

password_never_written() {
    cd "$DEMO"
    if grep -rqF "$ICM_TEST_STOREPASS" target/icm "$ACCEPT"/*.json 2>/dev/null; then
        grep -rlF "$ICM_TEST_STOREPASS" target/icm "$ACCEPT"/*.json | head
        return 1
    fi
    evidence "the store password appears in no file under target/icm or in the results"
}

password_env_unset() {
    cd "$DEMO"
    env -u ICM_TEST_STOREPASS icm release android --allow-dirty --no-smoke --json -q >"$ACCEPT/a2.json" || true
    jqe '.exit == 9 and .errors[0].id == "android.keystore.password_env_unset"' "$ACCEPT/a2.json"
    jqe '.artifacts.aab | endswith("-unsigned.aab")' "$ACCEPT/a2.json"
    evidence "$(/usr/bin/jq -r '.errors[0].detail' "$ACCEPT/a2.json")"
}

release_unsigned_apk() {
    cd "$DEMO"
    icm release android --sign none --apk --allow-dirty --no-smoke --json -q >"$ACCEPT/a3.json" || true
    jqe '.ok and (.artifacts.apk | endswith("-universal-debugkey.apk"))' "$ACCEPT/a3.json"
    evidence "$(/usr/bin/jq -r '.artifacts.apk' "$ACCEPT/a3.json" | sed "s|$ACCEPT|\$ACCEPT|g")"
}

# The universal APK signed with the upload key installs on the emulator
# (after the debug-signed app is removed) and draws.
universal_apk() {
    cd "$DEMO"
    icm release android --apk --allow-dirty --no-smoke --json -q >"$ACCEPT/a4.json" || true
    jqe '.ok and (.artifacts.apk | endswith("-universal.apk"))' "$ACCEPT/a4.json"
    local apk adb serial mark
    apk=$(/usr/bin/jq -r .artifacts.apk "$ACCEPT/a4.json")
    adb=$(sdk_adb)
    serial=$(/usr/bin/jq -r .device.serial "$ACCEPT/run.json")
    "$adb" -s "$serial" uninstall dev.accept.demo || true
    "$adb" -s "$serial" install "$apk" | grep -q Success
    "$adb" -s "$serial" shell setprop debug.icm.events 1
    mark=$("$adb" -s "$serial" shell date +%s.%N | tr -d '\r')
    "$adb" -s "$serial" shell am start -W -S -n dev.accept.demo/android.app.NativeActivity
    sleep 5
    "$adb" -s "$serial" logcat -d -v threadtime,epoch -T "$mark" -s ICM_EVENT:I | grep -q '"kind":"ready"'
    "$adb" -s "$serial" exec-out screencap -p >"$ACCEPT/previews/universal.png"
    "$adb" -s "$serial" uninstall dev.accept.demo >/dev/null
    evidence "$(basename "$apk") installed with adb on $serial and sent ICM_EVENT ready"
}

verify_release() {
    cd "$DEMO"
    icm release android --allow-dirty --no-smoke --json -q >"$ACCEPT/a5.json" || true
    jqe '.ok' "$ACCEPT/a5.json"
    icm verify android --json -q >"$ACCEPT/verify.json" || true
    jqe '.ok and .checks.fail == 0' "$ACCEPT/verify.json"
    evidence "$(/usr/bin/jq -r '.summary' "$ACCEPT/verify.json" | sed "s|$ACCEPT|\$ACCEPT|g")"
}

run_from_aab() {
    cd "$DEMO"
    icm run android --from-aab --timeout 9m --json -q >"$ACCEPT/from-aab.json" || true
    keep "$(/usr/bin/jq -r '.artifacts.preview // empty' "$ACCEPT/from-aab.json")" from-aab.preview.png
    jqe '.ok and (.artifacts.aab | endswith(".aab")) and .process.ready.source == "icm_event"' "$ACCEPT/from-aab.json"
    evidence "$(/usr/bin/jq -r '.summary' "$ACCEPT/from-aab.json")"
}

upload_md() {
    cd "$DEMO"
    local md
    md=$(/usr/bin/jq -r .artifacts.upload_md "$ACCEPT/a5.json")
    grep -q 'first Android release' "$md"
    grep -q '12 testers' "$md"
    grep -q 'fastlane supply' "$md"
    local rc=0
    bash "$(dirname "$md")/upload.sh" >"$ACCEPT/upload-sh.out" 2>&1 || rc=$?
    test "$rc" -eq 9
    evidence "UPLOAD.md: the Play Console first; upload.sh exits 9 for the first release"
}

diagnose_play() {
    printf '[!] Google Api Error: Invalid request - APK specifies a version code that has already been used.\n' >"$ACCEPT/supply.log"
    icm diagnose play "$ACCEPT/supply.log" --json -q >"$ACCEPT/diagnose.json" || true
    jqe '.exit == 1 and .errors[0].id == "version.build_not_increased"' "$ACCEPT/diagnose.json"
    evidence "$(/usr/bin/jq -r '.errors[0].detail' "$ACCEPT/diagnose.json")"
}

# --- the lifecycle suite --------------------------------------------------------------

lifecycle_android() {
    cd "$DEMO"
    icm test --on android --lifecycle --timeout 9m --json -q >"$ACCEPT/lifecycle-android.json" || true
    jqe '.ok and .lifecycle.failed == 0' "$ACCEPT/lifecycle-android.json"
    evidence "$(/usr/bin/jq -r '.summary' "$ACCEPT/lifecycle-android.json")"
    /usr/bin/jq -r '.lifecycle.steps[] | "\(.status) \(.step): \(.detail)"' "$ACCEPT/lifecycle-android.json" | cut -c1-160 | while IFS= read -r line; do
        evidence "$line"
    done
}

lifecycle_ios() {
    cd "$DEMO"
    icm test --on ios-sim --lifecycle --timeout 9m --json -q >"$ACCEPT/lifecycle-ios.json" || true
    jqe '.ok' "$ACCEPT/lifecycle-ios.json"
    evidence "$(/usr/bin/jq -r '.summary' "$ACCEPT/lifecycle-ios.json")"
}

final_cleanup() {
    cd "$DEMO"
    icm stop --all --shutdown --json -q >"$ACCEPT/stop.json" || true
    jqe '.ok' "$ACCEPT/stop.json"
    evidence "$(/usr/bin/jq -r '.summary' "$ACCEPT/stop.json")"
}

main() {
    must install install_icm
    must doctor-android doctor_android
    must no-foreign-android-device no_foreign_android
    must new new_app
    must run-android run_android
    step release-signed release_signed
    step password-never-written password_never_written
    step password-env-unset password_env_unset
    step release-unsigned-apk release_unsigned_apk
    step universal-apk-installs universal_apk
    step verify verify_release
    step run-from-aab run_from_aab
    step upload-md upload_md
    step diagnose-play diagnose_play
    step lifecycle-android lifecycle_android
    if icm test --on ios-sim --lifecycle --dry-run --json -q >"$ACCEPT/lifecycle-ios-plan.json" 2>/dev/null; then
        step lifecycle-ios-sim lifecycle_ios
    else
        skip lifecycle-ios-sim "this icm has no ios-sim lifecycle suite ($(/usr/bin/jq -r '.errors[0].id' "$ACCEPT/lifecycle-ios-plan.json"))"
    fi
    skip play-first-upload "owner: the first upload to Internal testing in the Play Console"
    skip play-later-upload "owner: a later fastlane supply draft upload"
    step final-cleanup final_cleanup
    finish
}

main
