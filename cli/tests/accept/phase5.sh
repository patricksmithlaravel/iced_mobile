#!/usr/bin/env bash
# Phase 5 acceptance (docs/icm/DESIGN.md §18, phase 5): desktop releases of
# the template app on the host this runs on. Each desktop target builds on
# its own OS, so the steps depend on the host:
#
# - macOS: the app released unsigned (an ad-hoc signed, hardened .app, its
#   zip for the notary service and a DMG, mounted and verified), then
#   signed with a throwaway self-signed identity; both launch.
# - Linux: the .deb and the AppImage (`--sign none`), `linux.glibc_floor`,
#   `icm verify linux`, the .deb installed with dpkg and removed, and the
#   AppImage's first frame under Xvfb (or the session's display).
# - Windows (Git Bash): the .msi and the NSIS installer (`--sign none`),
#   `icm verify windows`, and both installed silently and uninstalled.
#   This build of icm does not run on Windows yet; the steps are ready for
#   when it does.
#
# On every host the other two targets are refused (exit 4).
#
#   cli/tests/accept/phase5.sh
#
# A few minutes with cargo's registry cache warm (the template's release
# build uses thin LTO). It needs jq (/usr/bin/jq, else the one on PATH),
# and the network the first time cargo resolves the template; on macOS
# Xcode's command-line tools; on Linux dpkg-dev (and desktop-file-utils,
# xvfb); on Windows WiX v5, NSIS and pwsh.
#
# Outputs go to $ACCEPT (default: a new temporary directory). icm is
# installed into $ICM_ROOT (default $ACCEPT/icm); its cache and host.toml
# live in $ACCEPT. The built app opens a window for a few seconds.
#
# Installing touches the system, so on Linux the .deb is installed only as
# root (a CI container) or with ICM_ACCEPT_INSTALL=1 and passwordless sudo,
# and on Windows the installers run only with ICM_ACCEPT_INSTALL=1; each
# removes what it installed. Otherwise those steps are SKIP.
#
# Signing material (macOS) is throwaway and stays in $ACCEPT: two temporary
# keychains (one empty, one holding a self-signed code-signing identity
# made with /usr/bin/openssl), handed to icm through ICM_KEYCHAIN. They
# never join the user's keychain search list (a step checks it is
# unchanged) and are deleted at the end. The user's own keychains are
# never searched. Windows and Linux releases here are `--sign none`.
#
# Owner steps (never run here; printed as SKIP):
#   - macOS: notarization and stapling (UPLOAD.md), `icm release macos
#     --dmg` of the stapled app, and `icm verify macos --after-notarize`.
#   - Windows: a release signed with the owner's [desktop.windows]
#     sign_command.
#   - Linux: publishing the .deb and the AppImage.
set -euo pipefail

FORK=${FORK:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)}
ACCEPT=${ACCEPT:-$(mktemp -d)}
mkdir -p "$ACCEPT"
ACCEPT=$(cd "$ACCEPT" && pwd)
# shellcheck source=lib.sh
. "$FORK/cli/tests/accept/lib.sh"

case "$(uname -s)" in
Darwin) HOST=macos ;;
Linux) HOST=linux ;;
MINGW* | MSYS* | CYGWIN*) HOST=windows ;;
*) HOST=other ;;
esac

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
SEARCH_LIST_BEFORE=
if [ "$HOST" = macos ]; then
    SEARCH_LIST_BEFORE=$(security list-keychains -d user)
fi
cd "$ACCEPT"

echo "fork: $FORK"
echo "outputs: $ACCEPT"
echo "host: $HOST"

cleanup() {
    if [ "$HOST" = macos ]; then
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
    fi
}
trap cleanup EXIT

# --- helpers --------------------------------------------------------------

# native PATH: the path as icm (a native program) takes it: C:/... under
# Git Bash, unchanged elsewhere.
native() {
    if command -v cygpath >/dev/null 2>&1; then
        cygpath -m "$1"
    else
        printf '%s\n' "$1"
    fi
}

# abs RESULT KEY: a result path, absolute, with forward slashes (icm prints
# paths inside the app relative to it).
abs() {
    local path
    path=$("$JQ" -r "$2" "$1")
    path=${path//\\//}
    case "$path" in
    /* | [A-Za-z]:/*) printf '%s\n' "$path" ;;
    *) printf '%s/%s\n' "$DEMO" "$path" ;;
    esac
}

# check_event RESULT FILTER: the run's events include a check matching FILTER.
check_event() {
    local dir
    dir=$(abs "$1" .run_dir)
    "$JQ" -e -s "[.[] | select(.type == \"check\") | select($2)] | length > 0" "$dir/events.ndjson" >/dev/null || {
        echo "no check event matching $2; the checks were:"
        "$JQ" -r 'select(.type == "check") | "  \(.status) \(.id): \(.detail)"' "$dir/events.ndjson" | cut -c1-200
        return 1
    }
}

# app_bin: the template's binary, as icm.toml names it ([app] bin).
app_bin() {
    sed -n 's/^bin *= *"\([^"]*\)".*/\1/p' "$DEMO/icm.toml" | head -n1
}

# app_name: [app] name, the product name installers use.
app_name() {
    sed -n 's/^name *= *"\([^"]*\)".*/\1/p' "$DEMO/icm.toml" | head -n1
}

# executable APP: the path of a macOS bundle's executable.
executable() {
    printf '%s/Contents/MacOS/%s\n' "$1" "$(/usr/bin/plutil -extract CFBundleExecutable raw "$1/Contents/Info.plist")"
}

# await_ready PID ERR: the process PID writes an ICM_EVENT line of kind
# ready to ERR (a new file: the caller removes any earlier one first) within
# 60 s; then its process group (or it) is stopped.
await_ready() {
    local pid=$1 err=$2
    for _ in $(seq 1 120); do
        if grep -q '^ICM_EVENT .*"kind":"ready"' "$err" 2>/dev/null; then
            kill -- "-$pid" 2>/dev/null || kill "$pid" 2>/dev/null || true
            wait "$pid" 2>/dev/null || true
            evidence "$(grep -m1 '"kind":"ready"' "$err" | cut -c1-160)"
            return 0
        fi
        if ! kill -0 "$pid" 2>/dev/null; then
            echo "the app exited before its first frame:"
            tail -20 "$err"
            return 1
        fi
        sleep 0.5
    done
    kill -- "-$pid" 2>/dev/null || kill "$pid" 2>/dev/null || true
    echo "no ICM_EVENT ready event after 60 s"
    tail -20 "$err"
    return 1
}

# launch APP NAME: the macOS bundle's executable draws its first frame. Its
# output goes to $ACCEPT/launch-NAME.{out,err}, removed first: the shell
# truncates them only once the child runs, so an earlier launch's ready
# line must not be there to be read.
launch() {
    rm -f "$ACCEPT/launch-$2.out" "$ACCEPT/launch-$2.err"
    ICM_EVENTS=1 "$(executable "$1")" >"$ACCEPT/launch-$2.out" 2>"$ACCEPT/launch-$2.err" &
    await_ready $! "$ACCEPT/launch-$2.err"
}

# as_root COMMAND...: COMMAND as root (directly when this is root).
as_root() {
    if [ "$(id -u)" -eq 0 ]; then
        "$@"
    else
        sudo -n "$@"
    fi
}

# --- every host -----------------------------------------------------------

install_icm() {
    cargo install --locked --path "$(native "$FORK/cli")" --root "$(native "$ICM_ROOT")"
    case "$(command -v icm)" in
    "$ICM_ROOT/bin/icm"*) ;;
    *)
        echo "icm on PATH is $(command -v icm), not $ICM_ROOT/bin/icm"
        return 1
        ;;
    esac
    evidence "$(icm --version)"
}

new_app() {
    icm new "$(native "$DEMO")" --id com.example.demo --framework "path:$(native "$FORK")" --json -q >"$ACCEPT/new.json"
    jqe '.ok' "$ACCEPT/new.json"
    cd "$DEMO"
    if [ "$HOST" = linux ]; then
        # The .deb's Maintainer (the owner's decision; a WARN under --sign none).
        perl -pi -e 's/^# maintainer = .*/maintainer = "icm acceptance <accept\@icm.invalid>"/' icm.toml
        grep -q '^maintainer = ' icm.toml
    fi
    # Resolves Cargo.lock (releases build --locked) and compiles the desktop.
    icm check desktop --json -q >"$ACCEPT/check.json" || true
    jqe '.ok' "$ACCEPT/check.json"
    test -f "$DEMO/Cargo.lock"
    evidence "$("$JQ" -r '.summary' "$ACCEPT/new.json")"
}

# The two targets that are not this host's are refused before any build.
other_hosts() {
    cd "$DEMO"
    local target
    for target in macos windows linux; do
        [ "$target" != "$HOST" ] || continue
        icm release "$target" --sign none --json -q >"$ACCEPT/release-$target.json" || true
        jqe '.exit == 4 and .errors[0].id == "env.unsupported_host"' "$ACCEPT/release-$target.json"
        evidence "$target: $("$JQ" -r '.errors[0].detail' "$ACCEPT/release-$target.json" | cut -c1-160)"
    done
}

# --- macOS ----------------------------------------------------------------

keychains() {
    mkdir -p "$ACCEPT/keys"
    security create-keychain -p "$KC_PASS" "$EMPTY_KC"
    bash "$FORK/cli/tests/accept/test-identity.sh" "$TEST_KC" "$KC_PASS" "icm-test Phase5 Signing" >"$ACCEPT/keys/identity.txt"
    test "$(security list-keychains -d user)" = "$SEARCH_LIST_BEFORE"
    evidence "$(tr '\n' ' ' <"$ACCEPT/keys/identity.txt")"
}

# design §18 phase 5: without a Developer ID, a signed release is the owner's.
needs_developer_id() {
    cd "$DEMO"
    ICM_KEYCHAIN=$EMPTY_KC icm release macos --json -q >"$ACCEPT/release-auto.json" || true
    jqe '.exit == 9 and ([.errors[].id] | index("macos.sign.no_developer_id") != null)' "$ACCEPT/release-auto.json"
    test ! -d "$DEMO/target/icm/dist/0.1.0+1/macos"
    evidence "$("$JQ" -r '.summary' "$ACCEPT/release-auto.json" | cut -c1-200)"
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
    evidence "$("$JQ" -r '.summary' "$ACCEPT/release-none.json" | cut -c1-200)"
    evidence "lipo -archs: $(cat "$ACCEPT/archs.txt"); minos 12.0; ad hoc with the hardened runtime"
}

launch_unsigned() {
    launch "$(abs "$ACCEPT/release-none.json" .artifacts.app)" unsigned
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
    evidence "$("$JQ" -r '.summary' "$ACCEPT/verify.json")"
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
    launch "$(abs "$ACCEPT/release-test-id.json" .artifacts.app)" signed
}

search_list_unchanged() {
    cleanup
    test "$(security list-keychains -d user)" = "$SEARCH_LIST_BEFORE"
    test ! -f "$TEST_KC" && test ! -f "$EMPTY_KC"
    evidence "the user's keychain search list is as before; the test keychains are deleted"
}

macos_steps() {
    must keychains keychains
    step needs-developer-id needs_developer_id
    must release-unsigned unsigned_release
    step launch-unsigned launch_unsigned
    step dmg-unsigned unsigned_dmg
    step verify-dmg verify_dmg
    step release-test-identity test_identity_release
    step launch-signed launch_signed
    step other-hosts other_hosts
    step keychains-cleaned search_list_unchanged
    skip notarize-and-staple "the owner's (UPLOAD.md); icm never notarizes"
    skip dmg-of-stapled-app "the owner's: icm release macos --dmg after stapling"
    skip verify-after-notarize "the owner's: icm verify macos --after-notarize"
}

# --- Linux ----------------------------------------------------------------

release_linux() {
    cd "$DEMO"
    icm release linux --sign none --yes --allow-dirty --json -q >"$ACCEPT/release-linux.json" || true
    jqe '.ok' "$ACCEPT/release-linux.json"
    for id in linux.glibc_floor linux.desktop_file release.notices; do
        check_event "$ACCEPT/release-linux.json" ".id == \"$id\" and .status == \"pass\""
    done
    local deb appimage
    deb=$(abs "$ACCEPT/release-linux.json" .artifacts.deb)
    appimage=$(abs "$ACCEPT/release-linux.json" .artifacts.appimage)
    dpkg-deb --info "$deb" >"$ACCEPT/deb-info.txt"
    dpkg-deb --contents "$deb" >"$ACCEPT/deb-contents.txt"
    grep -q "usr/bin/$(app_bin)\$" "$ACCEPT/deb-contents.txt"
    grep -q 'usr/share/doc/.*/THIRD_PARTY_NOTICES.txt$' "$ACCEPT/deb-contents.txt"
    grep -q 'usr/share/applications/com.example.demo.desktop$' "$ACCEPT/deb-contents.txt"
    test -s "$appimage"
    evidence "$("$JQ" -r '.summary' "$ACCEPT/release-linux.json" | cut -c1-200)"
    evidence "$(basename "$deb"): $(dpkg-deb --field "$deb" Package) $(dpkg-deb --field "$deb" Version), Depends: $(dpkg-deb --field "$deb" Depends | cut -c1-100)"
}

verify_linux() {
    cd "$DEMO"
    icm verify linux --json -q >"$ACCEPT/verify-linux.json" || true
    jqe '.ok' "$ACCEPT/verify-linux.json"
    evidence "$("$JQ" -r '.summary' "$ACCEPT/verify-linux.json")"
}

# The .deb installs with dpkg, puts the executable and the .desktop entry
# in place, and removes cleanly.
deb_installs() {
    local deb package bin
    deb=$(abs "$ACCEPT/release-linux.json" .artifacts.deb)
    package=$(dpkg-deb --field "$deb" Package)
    bin=$(app_bin)
    as_root dpkg -i "$deb"
    # Steps run in a subshell: its exit removes the package if a check fails.
    trap 'as_root dpkg -r "$package" >/dev/null 2>&1 || true' EXIT
    dpkg -s "$package" >/dev/null
    test -x "/usr/bin/$bin"
    test -f /usr/share/applications/com.example.demo.desktop
    if command -v desktop-file-validate >/dev/null; then
        desktop-file-validate /usr/share/applications/com.example.demo.desktop
    fi
    as_root dpkg -r "$package"
    test ! -e "/usr/bin/$bin"
    evidence "dpkg -i installed $package (/usr/bin/$bin, com.example.demo.desktop); dpkg -r removed it"
}

# The AppImage draws its first frame: under Xvfb when xvfb-run is there,
# else on the session's display. No FUSE is needed: the runtime extracts
# itself. setsid gives the app its own process group, stopped as a whole.
appimage_ready() {
    local appimage
    appimage=$(abs "$ACCEPT/release-linux.json" .artifacts.appimage)
    chmod +x "$appimage"
    rm -f "$ACCEPT/appimage.out" "$ACCEPT/appimage.err"
    if command -v xvfb-run >/dev/null; then
        APPIMAGE_EXTRACT_AND_RUN=1 ICM_EVENTS=1 setsid --wait xvfb-run -a "$appimage" \
            >"$ACCEPT/appimage.out" 2>"$ACCEPT/appimage.err" &
    else
        APPIMAGE_EXTRACT_AND_RUN=1 ICM_EVENTS=1 setsid --wait "$appimage" \
            >"$ACCEPT/appimage.out" 2>"$ACCEPT/appimage.err" &
    fi
    await_ready $! "$ACCEPT/appimage.err"
}

linux_steps() {
    must release-linux release_linux
    step verify-linux verify_linux
    if [ "$(id -u)" -eq 0 ] || { [ "${ICM_ACCEPT_INSTALL:-}" = 1 ] && sudo -n true 2>/dev/null; }; then
        step deb-installs deb_installs
    else
        skip deb-installs "installs into /usr: run as root, or with ICM_ACCEPT_INSTALL=1 and passwordless sudo"
    fi
    if command -v xvfb-run >/dev/null || [ -n "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ]; then
        step appimage-ready appimage_ready
    else
        skip appimage-ready "no xvfb-run and no display"
    fi
    step other-hosts other_hosts
    skip publish-linux "the owner's: publishing the .deb and the AppImage"
}

# --- Windows (Git Bash) ---------------------------------------------------

release_windows() {
    cd "$DEMO"
    icm release windows --sign none --allow-dirty --json -q >"$ACCEPT/release-windows.json" || true
    jqe '.ok' "$ACCEPT/release-windows.json"
    for id in windows.pe_imports release.notices; do
        check_event "$ACCEPT/release-windows.json" ".id == \"$id\" and .status == \"pass\""
    done
    test -s "$(abs "$ACCEPT/release-windows.json" .artifacts.msi)"
    test -s "$(abs "$ACCEPT/release-windows.json" .artifacts.exe)"
    evidence "$("$JQ" -r '.summary' "$ACCEPT/release-windows.json" | cut -c1-200)"
}

verify_windows() {
    cd "$DEMO"
    icm verify windows --json -q >"$ACCEPT/verify-windows.json" || true
    jqe '.ok' "$ACCEPT/verify-windows.json"
    evidence "$("$JQ" -r '.summary' "$ACCEPT/verify-windows.json")"
}

# The .msi installs per machine and uninstalls (CI's smoke script).
msi_installs() {
    local msi
    msi=$(abs "$ACCEPT/release-windows.json" .artifacts.msi)
    pwsh -NoProfile -NonInteractive -File "$(native "$FORK/.github/ci/msi-smoke.ps1")" \
        -Msi "$(native "$msi")" -Exe "$(app_bin).exe" -LogDir "$(native "$ACCEPT")"
    evidence "$(basename "$msi") installed with msiexec /qn and uninstalled"
}

# The NSIS installer installs per user and its uninstaller removes it.
nsis_installs() {
    local setup dir
    setup=$(abs "$ACCEPT/release-windows.json" .artifacts.exe)
    dir="$(cygpath -u "$LOCALAPPDATA")/Programs/$(app_name)"
    "$setup" /S
    trap '"$dir/uninstall.exe" /S 2>/dev/null || true' EXIT
    test -f "$dir/$(app_bin).exe"
    "$dir/uninstall.exe" /S
    for _ in $(seq 1 20); do
        [ -e "$dir/$(app_bin).exe" ] || break
        sleep 1
    done
    test ! -e "$dir/$(app_bin).exe"
    evidence "$(basename "$setup") /S installed into $dir; uninstall.exe /S removed it"
}

windows_steps() {
    must release-windows release_windows
    step verify-windows verify_windows
    if [ "${ICM_ACCEPT_INSTALL:-}" = 1 ]; then
        step msi-installs msi_installs
        step nsis-installs nsis_installs
    else
        skip msi-installs "installs per machine: set ICM_ACCEPT_INSTALL=1"
        skip nsis-installs "installs per user: set ICM_ACCEPT_INSTALL=1"
    fi
    step other-hosts other_hosts
    skip signed-windows "the owner's: a release signed with [desktop.windows] sign_command"
}

main() {
    if [ "$HOST" = other ]; then
        echo "phase5.sh runs on macOS, Linux or Windows (Git Bash), not $(uname -s)"
        exit 1
    fi
    must install install_icm
    must new new_app
    case "$HOST" in
    macos) macos_steps ;;
    linux) linux_steps ;;
    windows) windows_steps ;;
    esac
    finish
}

main "$@"
