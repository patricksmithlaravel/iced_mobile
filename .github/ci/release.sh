#!/usr/bin/env bash
# CI's release job for one desktop target (design §17 item 11 and §18 phase
# 5): a new app from the template, pinned to this checkout by path, released
# with `--sign none`, verified, and installed or started the way a user
# would. Nothing is signed, notarized or sent anywhere: icm never does that,
# and the owner runs the release's UPLOAD.md.
#
#   .github/ci/release.sh <macos|windows|linux> <work dir>
#
# - macos: stage 1 (the .app) and stage 2 (`--dmg`), `icm verify` after
#   each; `lipo -archs` and the Mach-O minos; the DMG mounts read-only and
#   holds the app and an Applications link; the app reaches its first frame.
# - windows: the .msi and the NSIS installer; the .msi installs and
#   uninstalls silently (msi-smoke.ps1).
# - linux: the .deb and the AppImage (in the ubuntu:22.04 container, so
#   `linux.glibc_floor` holds); the .deb installs with dpkg and removes; the
#   AppImage reaches its first frame under Xvfb.
#
# Needs icm (built from this checkout) and jq on PATH, plus the packaging
# tools the workflow installs. Keeps icm's result objects in
# <work dir>/results and the app in <work dir>/app (the release under its
# target/icm/dist), and first of all appends `work=<work dir>` to
# $GITHUB_OUTPUT when it is set, so the upload step finds whatever exists
# when a later step fails. (In a container job `runner.temp` names the
# host's directory, not the container's, so the workflow cannot name it.)
set -euo pipefail

fork=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
# shellcheck source-path=SCRIPTDIR source=lib.sh
. "$fork/.github/ci/lib.sh"

usage="usage: $0 <macos|windows|linux> <work dir>"
target=${1:-}
work=${2:-}
case "$target" in
macos | windows | linux) ;;
*)
    echo "$usage" >&2
    exit 2
    ;;
esac
if [ -z "$work" ]; then
    echo "$usage" >&2
    exit 2
fi

mkdir -p "$work/results"
work=$(cd "$work" && pwd)
results="$work/results"
app="$work/app"
if [ -n "${GITHUB_OUTPUT:-}" ]; then
    echo "work=$(native "$work")" >>"$GITHUB_OUTPUT"
fi

# The template's binary, as icm.toml names it ([app] bin).
app_bin() {
    sed -n 's/^bin *= *"\([^"]*\)".*/\1/p' "$app/icm.toml" | head -n1
}

# A key of one icm.toml table, e.g. `toml_key desktop.macos min_os`.
toml_key() {
    awk -v table="[$1]" -v key="$2" '
        $1 == table { inside = 1; next }
        /^\[/ { inside = 0 }
        inside && $1 == key { gsub(/"/, "", $3); print $3; exit }
    ' "$app/icm.toml"
}

# The one file in $dist whose name matches a glob, or a failure.
one_file() {
    local found
    found=$(find "$dist" -maxdepth 1 -name "$1" | head -n1)
    [ -n "$found" ] || fail "no $1 in $dist: $(find "$dist" -mindepth 1 -maxdepth 1 -exec basename {} \; | tr '\n' ' ')"
    printf '%s\n' "$found"
}

release() {
    local name=$1
    shift
    icm_json "$name" release "$target" --sign none --allow-dirty --yes "$@"
    dist=$(result_path "$name" '.release.dist')
    [ -d "$dist" ] || fail "the release reported no dist directory"
    echo "dist: $dist"
    ls -la "$dist"
    # Unsigned, so never uploadable; the reasons are worth a look.
    jq -c '{uploadable: .release.uploadable, not_uploadable: .release.not_uploadable}' "$results/$name.json" || true
}

smoke_macos() {
    local bundle exe archs minos want mount pid
    bundle=$(one_file '*.app')
    exe="$bundle/Contents/MacOS/$(plutil -extract CFBundleExecutable raw -o - "$bundle/Contents/Info.plist")"

    group "lipo -archs and minos"
    archs=$(lipo -archs "$exe")
    echo "lipo -archs: $archs"
    case " $archs " in
    *" $(uname -m) "*) ;;
    *) fail "$exe has no $(uname -m) slice ($archs)" ;;
    esac
    want=$(toml_key desktop.macos min_os)
    minos=$(vtool -show-build "$exe" | awk '$1 == "minos" { print $2; exit }')
    echo "minos: $minos ([desktop.macos] min_os = $want)"
    [ "$minos" = "$want" ] || fail "minos $minos is not [desktop.macos] min_os $want"

    group "the DMG mounts"
    mount="$work/dmg"
    mkdir -p "$mount"
    hdiutil attach -nobrowse -readonly -noautoopen -mountpoint "$mount" "$(one_file '*.dmg')"
    ls -la "$mount"
    local ok=1
    [ -d "$mount/$(basename "$bundle")" ] || ok=0
    [ -L "$mount/Applications" ] || ok=0
    hdiutil detach "$mount"
    [ "$ok" = 1 ] || fail "the DMG lacks $(basename "$bundle") or the Applications link"

    group "the app reaches its first frame"
    # Both streams to files: a child holding the step's output open would
    # keep the step from ending.
    ICM_EVENTS=1 "$exe" >"$results/app.stdout" 2>"$results/app.stderr" &
    pid=$!
    local rc=0
    wait_ready "$results/app.stderr" "$pid" 60 || rc=1
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    [ "$rc" = 0 ] || fail "the released app did not reach its first frame"
}

smoke_windows() {
    local msi
    msi=$(one_file '*.msi')
    one_file '*setup*.exe' >/dev/null

    group "the .msi installs and uninstalls"
    pwsh -NoProfile -NonInteractive -File "$(native "$fork/.github/ci/msi-smoke.ps1")" \
        -Msi "$(native "$msi")" -Exe "$(app_bin).exe" -LogDir "$(native "$results")"
}

smoke_linux() {
    local deb appimage package bin pid xvfb

    group "the .deb installs and removes"
    deb=$(one_file '*.deb')
    dpkg-deb --info "$deb"
    package=$(dpkg-deb --field "$deb" Package)
    bin=$(app_bin)
    dpkg -i "$deb"
    dpkg -s "$package"
    [ -x "/usr/bin/$bin" ] || fail "/usr/bin/$bin is not installed by $package"
    dpkg -r "$package"
    [ ! -e "/usr/bin/$bin" ] || fail "/usr/bin/$bin is still there after dpkg -r"

    group "the AppImage reaches its first frame under Xvfb"
    appimage=$(one_file '*.AppImage')
    chmod +x "$appimage"
    Xvfb :99 -screen 0 1280x800x24 -nolisten tcp >"$results/xvfb.log" 2>&1 &
    xvfb=$!
    # No FUSE in a container: the runtime extracts itself and runs AppRun.
    # setsid gives the app its own process group, ended as a whole below
    # (--wait keeps $! alive as long as the app, should setsid fork); both
    # streams go to files so no child holds the step's output open.
    DISPLAY=:99 APPIMAGE_EXTRACT_AND_RUN=1 ICM_EVENTS=1 \
        setsid --wait "$appimage" >"$results/app.stdout" 2>"$results/app.stderr" &
    pid=$!
    local rc=0
    wait_ready "$results/app.stderr" "$pid" 60 || rc=1
    kill -- "-$pid" 2>/dev/null || kill "$pid" 2>/dev/null || true
    kill "$xvfb" 2>/dev/null || true
    wait 2>/dev/null || true
    [ "$rc" = 0 ] || fail "the AppImage did not reach its first frame"
}

group "icm new"
icm_json new new "$(native "$app")" --framework "path:$(native "$fork")"
cd "$app"

group "icm doctor desktop --fix --yes"
# The template's toolchain, and on Linux the pinned appimagetool.
icm_json doctor doctor desktop --fix --yes

case "$target" in
macos)
    group "icm release macos --sign none (stage 1: the .app)"
    release release
    group "icm verify macos (the .app)"
    icm_json verify verify macos
    group "icm release macos --dmg --sign none (stage 2: the DMG)"
    release release-dmg --dmg
    group "icm verify macos (the DMG)"
    icm_json verify-dmg verify macos
    smoke_macos
    ;;
windows)
    group "icm release windows --sign none"
    release release
    group "icm verify windows"
    icm_json verify verify windows
    smoke_windows
    ;;
linux)
    group "icm release linux --sign none"
    release release
    group "icm verify linux"
    icm_json verify verify linux
    smoke_linux
    ;;
esac

group "done"
echo "release $target: ok ($(native "$dist"))"
