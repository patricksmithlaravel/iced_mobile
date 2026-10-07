#!/usr/bin/env bash
# Phase 4 acceptance (docs/icm/DESIGN.md §18, §11.3, §12.4): the web
# release of the template on this machine, then the checks the owner runs
# after deploying it, against a local stand-in for the host.
#
#   cli/tests/accept/phase4.sh
#
# A few minutes with cargo's registry cache warm: icm is installed from the
# checkout, `icm new` makes the template app (its framework is this
# checkout), `icm doctor web --fix --yes` creates its Cargo.lock and installs
# the wasm-bindgen CLI of that lock and the pinned wasm-opt (downloads, into
# icm's cache), and `icm release web` builds the size-optimized site, runs
# the gates and loads the site once in headless Chrome.
#
# Nothing is deployed: the "host" is a Python static server on 127.0.0.1
# serving the release's site (`.wasm` as application/wasm, then as
# application/octet-stream for the negative case). Deploying to a real host
# and `icm verify web --url <https url>` are the owner's acceptance.
#
# Outputs go to $ACCEPT (default: a new temporary directory); icm goes into
# $ICM_ROOT (default $ACCEPT/icm), its host.toml into $ACCEPT. icm's cache is
# $ICM_CACHE_DIR (default $ACCEPT/cache; point it at an existing cache to
# reuse its wasm-bindgen and wasm-opt). Needs Chrome, python3, /usr/bin/jq
# and the network for doctor's installs.
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
DEMO="$ACCEPT/webdemo"
cd "$ACCEPT"

echo "fork: $FORK"
echo "outputs: $ACCEPT"
echo "icm: $ICM_ROOT/bin/icm; cache $ICM_CACHE_DIR"

cleanup() {
    if [ -f "$DEMO/icm.toml" ] && command -v icm >/dev/null; then
        (cd "$DEMO" && icm stop --all --json -q >"$ACCEPT/cleanup.json" 2>&1) || true
    fi
}
trap cleanup EXIT

# host NAME SITE WASM_TYPE: a static server for SITE on a free port of
# 127.0.0.1, serving .wasm as WASM_TYPE; sets HOST_PORT and HOST_PID and
# stops the server when the calling step ends.
host() {
    local portfile="$ACCEPT/port-$1"
    rm -f "$portfile"
    python3 - "$2" "$portfile" "$3" <<'PY' >"$ACCEPT/host-$1.log" 2>&1 &
import functools, http.server, socketserver, sys
site, portfile, wasm_type = sys.argv[1:4]
class Handler(http.server.SimpleHTTPRequestHandler):
    extensions_map = {**http.server.SimpleHTTPRequestHandler.extensions_map, ".wasm": wasm_type}
server = socketserver.ThreadingTCPServer(("127.0.0.1", 0), functools.partial(Handler, directory=site))
with open(portfile + ".tmp", "w") as f:
    f.write(str(server.server_address[1]))
import os
os.rename(portfile + ".tmp", portfile)
server.serve_forever()
PY
    HOST_PID=$!
    # Steps run in a subshell: its exit ends the server.
    trap 'kill "$HOST_PID" 2>/dev/null || true' EXIT
    local tries=0
    while [ ! -s "$portfile" ] && [ "$tries" -lt 100 ]; do
        sleep 0.1
        tries=$((tries + 1))
    done
    HOST_PORT=$(cat "$portfile")
}

# in_demo PATH: a path from a result (relative to the app when inside it).
in_demo() {
    case "$1" in
    /*) printf '%s\n' "$1" ;;
    *) printf '%s/%s\n' "$DEMO" "$1" ;;
    esac
}

# --- steps -------------------------------------------------------------------

install_icm() {
    cargo install --locked --path "$FORK/cli" --root "$ICM_ROOT"
    test "$(command -v icm)" = "$ICM_ROOT/bin/icm"
    evidence "$(icm --version)"
}

new_app() {
    icm new "$DEMO" --id com.example.webdemo --framework path:"$FORK" --json -q >"$ACCEPT/new.json"
    jqe '.ok' "$ACCEPT/new.json"
    evidence "$(/usr/bin/jq -r '.summary' "$ACCEPT/new.json")"
}

# The lock, the wasm32 target, the wasm-bindgen CLI of the lock, and the
# pinned wasm-opt the release needs (doctor reports it as a WARN until then).
doctor_web() {
    cd "$DEMO"
    icmd doctor web --fix --yes >"$ACCEPT/doctor.json" || true
    jqe '.exit == 0' "$ACCEPT/doctor.json"
    test -f Cargo.lock
    icm print tools --json -q >"$ACCEPT/tools.json"
    evidence "$(/usr/bin/jq -r '.summary' "$ACCEPT/doctor.json")"
}

release_plan() {
    cd "$DEMO"
    icm release web --allow-dirty --dry-run --json -q >"$ACCEPT/plan.json"
    jqe '.ok and ([.plan[].name] | index("wasm-opt") and index("web.serve_smoke"))' "$ACCEPT/plan.json"
    test ! -e target/icm/dist
    evidence "$(/usr/bin/jq -r '[.plan[].name] | join(", ")' "$ACCEPT/plan.json")"
}

# §18 phase 4: the release is ok, the hashed module is loaded explicitly,
# _headers types the .wasm, and no check failed (web.serve_smoke included).
release_web() {
    cd "$DEMO"
    icmd release web --allow-dirty >"$ACCEPT/w.json" || true
    jqe '.ok' "$ACCEPT/w.json"
    SITE=$(in_demo "$(/usr/bin/jq -r .artifacts.site "$ACCEPT/w.json")")
    echo "$SITE" >"$ACCEPT/site.path"
    grep -q 'module_or_path: "./pkg/app_bg-' "$SITE/index.html"
    grep -q 'application/wasm' "$SITE/_headers"
    jqe '[.checks.failed[]] | length == 0' "$ACCEPT/w.json"
    jqe '.release.uploadable and .smoke.status == "ready" and .smoke.blank == false and .smoke.errors == 0' "$ACCEPT/w.json"
    jqe '.smoke.wasm[0].mime == "application/wasm"' "$ACCEPT/w.json"
    for file in 404.html .nojekyll manifest.webmanifest icon-192.png icon-maskable-512.png THIRD_PARTY_NOTICES.txt; do
        test -f "$SITE/$file"
    done
    cp "$(in_demo "$(/usr/bin/jq -r .artifacts.screenshot "$ACCEPT/w.json")")" "$ACCEPT/release-screen.png"
    evidence "$(/usr/bin/jq -r '.summary' "$ACCEPT/w.json")"
    evidence "wasm $(/usr/bin/jq -r '.size.wasm | "\(.before_wasm_opt) -> \(.bytes) bytes, \(.gzip) gzipped (budget \(.budget_kb) KB)"' "$ACCEPT/w.json"); ready after $(/usr/bin/jq -r .smoke.ready.ms "$ACCEPT/w.json") ms in $(/usr/bin/jq -r .smoke.browser "$ACCEPT/w.json")"
    evidence "wasm-opt features: $(/usr/bin/jq -r '.tools["wasm-opt-features"]' "$ACCEPT/w.json")"
}

upload_md() {
    cd "$DEMO"
    icm upload-commands web >"$ACCEPT/UPLOAD.md"
    grep -q 'icm verify web --url' "$ACCEPT/UPLOAD.md"
    grep -q 'icm ledger mark-uploaded web' "$ACCEPT/UPLOAD.md"
    # upload.sh refuses without the owner's variables, and so uploads nothing.
    local rc=0
    env -u WEB_DEPLOY_TARGET -u WEB_URL bash "$(in_demo "$(/usr/bin/jq -r .artifacts.upload_sh "$ACCEPT/w.json")")" \
        >"$ACCEPT/upload-sh.out" 2>&1 || rc=$?
    test "$rc" -eq 9
    evidence "upload.sh without WEB_DEPLOY_TARGET: exit $rc"
}

verify_files() {
    cd "$DEMO"
    icm verify web --json -q >"$ACCEPT/verify.json" || true
    jqe '.ok and .smoke.status == "ready" and ([.checks.failed[]] | length == 0)' "$ACCEPT/verify.json"
    evidence "$(/usr/bin/jq -r '.summary' "$ACCEPT/verify.json")"
}

# The owner's check after deploying, against a local stand-in host.
verify_host() {
    cd "$DEMO"
    host good "$(cat "$ACCEPT/site.path")" application/wasm
    icm verify web --url "http://127.0.0.1:$HOST_PORT/" --json -q >"$ACCEPT/verify-url.json" || true
    jqe '.ok and .smoke.status == "ready" and .smoke.wasm[0].mime == "application/wasm"' "$ACCEPT/verify-url.json"
    evidence "$(/usr/bin/jq -r '.summary' "$ACCEPT/verify-url.json")"
}

verify_host_wrong_mime() {
    cd "$DEMO"
    host bad "$(cat "$ACCEPT/site.path")" application/octet-stream
    icm verify web --url "http://127.0.0.1:$HOST_PORT/" --json -q >"$ACCEPT/verify-bad.json" || true
    jqe '.exit == 1 and ([.checks.failed[]] == ["web.mime"])' "$ACCEPT/verify-bad.json"
    evidence "$(/usr/bin/jq -r '.errors[0].detail' "$ACCEPT/verify-bad.json")"
}

main() {
    must install install_icm
    must new new_app
    must doctor-web doctor_web
    step release-plan release_plan
    must release-web release_web
    step upload-commands upload_md
    step verify-files verify_files
    step verify-host verify_host
    step verify-host-wrong-mime verify_host_wrong_mime
    skip owner-deploy "the owner deploys the site and runs icm verify web --url <https url>"
    finish
}

main "$@"
