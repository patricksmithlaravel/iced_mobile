#!/usr/bin/env bash
# Prints the developer directory of the newest release Xcode, for
# `sudo xcode-select -s` in a job that builds for the App Store, which takes
# no build made with a beta Xcode.
#
#   .github/ci/xcode.sh [<applications dir>]
#
# It looks at the Xcode_<version>.app bundles in <applications dir>
# (default /Applications, where GitHub's macOS images keep them). Their
# names cannot tell a beta: the images install one as
# Xcode_<v>_beta_<n>.app and also link it as Xcode_<v>.app. So each bundle
# is followed to its real path and judged by what it says of itself:
# Contents/Resources/LicenseInfo.plist's licenseType ("GM" for a release or
# a release candidate, which the App Store takes too; "Beta" for a beta) and
# Contents/version.plist's CFBundleShortVersionString. A bundle whose real
# name says beta, or whose licence type or version cannot be read, is left
# out. Versions compare by number (26.10 is newer than 26.9).
#
# Says on stderr what it looked at. Exits 1 with an ::error:: line when it
# finds no release Xcode. It runs under macOS's bash 3.2, which cannot
# parse a `case` inside `$(...)`, so none is written there.
set -euo pipefail

apps=${1:-/Applications}
version_re='^[0-9]+(\.[0-9]+)*$'
best=""
best_version=""
best_key=-1

for app in "$apps"/Xcode_*.app; do
    # The pattern itself when nothing matches, or a dangling link.
    [ -d "$app" ] || continue
    real=$(cd -P "$app" && pwd) || continue
    case "$real" in
    *[Bb]eta*)
        echo "skip $app: $real is a beta" >&2
        continue
        ;;
    esac
    license=$(plutil -extract licenseType raw -o - "$real/Contents/Resources/LicenseInfo.plist" 2>/dev/null) || license=""
    case "$license" in
    "")
        echo "skip $app: no licence type in $real/Contents/Resources/LicenseInfo.plist" >&2
        continue
        ;;
    *[Bb]eta*)
        echo "skip $app: $real is a beta (licence type $license)" >&2
        continue
        ;;
    esac
    version=$(plutil -extract CFBundleShortVersionString raw -o - "$real/Contents/version.plist" 2>/dev/null) || version=""
    if ! [[ $version =~ $version_re ]]; then
        echo "skip $app: no version in $real/Contents/version.plist" >&2
        continue
    fi
    IFS=. read -r major minor patch _ <<<"$version"
    key=$((10#$major * 1000000 + 10#${minor:-0} * 1000 + 10#${patch:-0}))
    echo "found Xcode $version ($license) at $real" >&2
    # A version's links are the same Xcode: the first one found stays.
    if [ "$key" -gt "$best_key" ]; then
        best=$real
        best_version=$version
        best_key=$key
    fi
done

if [ -z "$best" ]; then
    echo "::error::no release Xcode in $apps (an Xcode_*.app whose LicenseInfo.plist says GM, with a version)" >&2
    exit 1
fi
echo "newest release Xcode: $best_version at $best" >&2
printf '%s/Contents/Developer\n' "$best"
