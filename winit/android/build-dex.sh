#!/usr/bin/env bash
# Builds winit/android/classes.dex, the dex iced_winit embeds for the Android
# keyboard (winit/src/ime/dex.rs), from the Java sources in
# winit/android/java, and writes winit/android/classes.dex.inputs: the
# SHA-256 of each source (what `cargo test -p iced_winit ime::dex` checks),
# then the javac and d8 versions.
#
#   winit/android/build-dex.sh           rebuild both files
#   winit/android/build-dex.sh --check   rebuild in a temporary directory and
#                                        exit 1 if either file differs
#
# Needs JDK 21 (JAVA_HOME, else /usr/libexec/java_home -v 21, Homebrew's
# openjdk@21, or javac on PATH) and, in the Android SDK ($ANDROID_HOME, else
# $ANDROID_SDK_ROOT, else /opt/homebrew/share/android-commandlinetools), the
# packages "platforms;android-36" and "build-tools;36.0.0". The output is
# reproducible: the same sources and tools give the same bytes.
set -euo pipefail

platform=android-36
build_tools=36.0.0
jdk_major=21

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"

check=false
case "${1:-}" in
    "") ;;
    --check) check=true ;;
    *)
        echo "usage: $0 [--check]" >&2
        exit 2
        ;;
esac

fail() {
    echo "build-dex.sh: $*" >&2
    exit 1
}

# JDK 21's javac: the first candidate that runs and reports 21.
find_javac() {
    local candidates=()
    local home

    if [[ -n "${JAVA_HOME:-}" ]]; then
        candidates+=("$JAVA_HOME/bin/javac")
    fi
    if [[ -x /usr/libexec/java_home ]] &&
        home="$(/usr/libexec/java_home -v "$jdk_major" 2>/dev/null)"; then
        candidates+=("$home/bin/javac")
    fi
    for prefix in /opt/homebrew/opt /usr/local/opt; do
        candidates+=("$prefix/openjdk@$jdk_major/libexec/openjdk.jdk/Contents/Home/bin/javac")
    done
    if command -v javac >/dev/null 2>&1; then
        candidates+=("$(command -v javac)")
    fi

    local candidate version
    for candidate in "${candidates[@]}"; do
        [[ -x "$candidate" ]] || continue
        version="$("$candidate" -version 2>&1)" || continue
        if [[ "$version" == "javac $jdk_major."* || "$version" == "javac $jdk_major" ]]; then
            echo "$candidate"
            return 0
        fi
    done

    return 1
}

javac="$(find_javac)" ||
    fail "no JDK $jdk_major found: set JAVA_HOME to a JDK $jdk_major home"
jdk_bin="$(dirname "$javac")"
javac_version="$("$javac" -version 2>&1)"
javac_version="${javac_version#javac }"

sdk="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-/opt/homebrew/share/android-commandlinetools}}"
android_jar="$sdk/platforms/$platform/android.jar"
d8="$sdk/build-tools/$build_tools/d8"

[[ -f "$android_jar" ]] ||
    fail "no $android_jar: install it with sdkmanager \"platforms;$platform\""
[[ -x "$d8" ]] ||
    fail "no $d8: install it with sdkmanager \"build-tools;$build_tools\""

# d8 runs the `java` on PATH.
export PATH="$jdk_bin:$PATH"
d8_version="$("$d8" --version 2>&1 | head -n 1)"
d8_version="${d8_version#D8 }"
d8_version="${d8_version%% *}"

sources=()
while IFS= read -r source; do
    sources+=("$source")
done < <(cd "$root" && find winit/android/java -type f -name '*.java' | LC_ALL=C sort)

[[ ${#sources[@]} -gt 0 ]] || fail "no Java sources in $here/java"

if command -v shasum >/dev/null 2>&1; then
    sha256() { shasum -a 256 "$1" | cut -d ' ' -f 1; }
else
    sha256() { sha256sum "$1" | cut -d ' ' -f 1; }
fi

work="$(mktemp -d "${TMPDIR:-/tmp}/build-dex.XXXXXX")"
trap 'rm -rf "$work"' EXIT
mkdir "$work/classes" "$work/dex" "$work/out"

absolute=()
for source in "${sources[@]}"; do
    absolute+=("$root/$source")
done

"$javac" --release 8 -encoding UTF-8 -Xlint:all,-options -Werror \
    -implicit:none -classpath "$android_jar" -d "$work/classes" \
    "${absolute[@]}"

classes=()
while IFS= read -r class; do
    classes+=("$class")
done < <(find "$work/classes" -type f -name '*.class' | LC_ALL=C sort)

"$d8" --release --min-api 26 --lib "$android_jar" --output "$work/dex" \
    "${classes[@]}"

[[ -f "$work/dex/classes.dex" ]] || fail "d8 wrote no classes.dex"
cp "$work/dex/classes.dex" "$work/out/classes.dex"

{
    for source in "${sources[@]}"; do
        printf '%s  %s\n' "$(sha256 "$root/$source")" "$source"
    done
    printf 'javac %s\n' "$javac_version"
    printf 'd8 %s (build-tools %s)\n' "$d8_version" "$build_tools"
} >"$work/out/classes.dex.inputs"

if $check; then
    stale=()
    for file in classes.dex classes.dex.inputs; do
        if ! cmp -s "$work/out/$file" "$here/$file"; then
            stale+=("winit/android/$file")
        fi
    done

    if [[ ${#stale[@]} -gt 0 ]]; then
        echo "build-dex.sh: ${stale[*]} differ from a rebuild:" >&2
        diff "$here/classes.dex.inputs" "$work/out/classes.dex.inputs" >&2 || true
        echo "run winit/android/build-dex.sh and commit both files" \
            "(a different javac or d8 version also shows here)" >&2
        exit 1
    fi

    echo "winit/android/classes.dex is up to date"
    exit 0
fi

cp "$work/out/classes.dex" "$here/classes.dex"
cp "$work/out/classes.dex.inputs" "$here/classes.dex.inputs"
echo "wrote winit/android/classes.dex ($(wc -c <"$here/classes.dex" | tr -d ' ') bytes) and classes.dex.inputs"
