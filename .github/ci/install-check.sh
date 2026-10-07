#!/usr/bin/env bash
# The install check (design §2.3 and §17 item 10): `cargo install --locked
# --git` of this checkout finds icm in its nested workspace (cli/ has its own
# Cargo.lock, which --locked must accept as it is), and the icm it installs
# pins this commit for `icm new`, or this release's tag when one is given
# (design Appendix C item 3: the tag comes from icm's version and is
# confirmed against cargo's git database).
#
#   .github/ci/install-check.sh <install root> [<tag>]
#
# Needs git, cargo and jq. Installs into <install root>/bin.
set -euo pipefail

fork=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
# shellcheck source-path=SCRIPTDIR source=lib.sh
. "$fork/.github/ci/lib.sh"

root=${1:-}
tag=${2:-}
if [ -z "$root" ]; then
    echo "usage: $0 <install root> [<tag>]" >&2
    exit 2
fi

# The commit cargo installs: the tag's, or this checkout's.
rev=$(git -C "$fork" rev-parse --verify "${tag:-HEAD}^{commit}")
version=$(git -C "$fork" show "$rev:cli/Cargo.toml" | sed -n 's/^version *= *"\([^"]*\)".*/\1/p' | head -n1)
url="file://$(native "$fork")"

if [ -n "$tag" ]; then
    group "cargo install --locked --git $url --tag $tag icm"
    cargo install --locked --git "$url" --tag "$tag" --root "$root" icm
    want="tag:$tag"
else
    group "cargo install --locked --git $url --rev $rev icm"
    cargo install --locked --git "$url" --rev "$rev" --root "$root" icm
    want="rev:$rev"
fi

group "the installed icm"
icm="$root/bin/icm"
line=$("$icm" --version)
echo "$line"
case "$line" in
"icm $version (rev ${rev:0:12})") ;;
*) fail "icm --version printed '$line', not 'icm $version (rev ${rev:0:12})'" ;;
esac

# The start event of any --json command carries the build's facts.
"$icm" explain exit-codes --json >"$root/explain.ndjson"
framework=$(head -n1 "$root/explain.ndjson" | jq -r '.icm.framework')
echo "icm new pins: $framework"
[ "$framework" = "$want" ] || fail "icm new would pin '$framework', not '$want'"
echo "install check: ok"
