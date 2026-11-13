#!/usr/bin/env bash
# d12: the UI journey in headless Chromium, against a built binary and the built bundle.
#
#   PF_BIN=target/debug/pulse-factory PF_UI_DIR=~/.local/share/pulse-factory/ui scripts/demo/ui-pair.sh
#
# `PF_BIN` is a built binary (default `pulse-factory` on PATH). `PF_UI_DIR` is the built bundle
# (default `${PF_SHARE_DIR:-$HOME/.local/share/pulse-factory}/ui`, where scripts/install-ui.sh puts it).
# `PF_BRIDGE_BIN` is the built `pf-client-bridge` (default: `pf-client-bridge` next to `PF_BIN`, else
# `pf-client-bridge` on PATH).
# This script builds nothing. It runs `npm ci` in apps/ only when apps/node_modules is missing, and it
# downloads no browser: Chromium 1248 must be in the Playwright cache.
#
# It starts a controller with the API on 127.0.0.1 in a temp state directory, gets a pairing code, and
# starts the test bridge, `pf-client-bridge` (the `desktop-bridge` fake with the real pinned client
# behind it, crates/pf-client). The journey is
# apps/ui/e2e/pair.spec.ts. It reaches no address beyond 127.0.0.1, prints no code and no token, stops
# both processes on exit and removes its temp directory. `PF_SHOTS_DIR` names a directory for one
# screenshot each of 3a, 2c, 3b and 3c.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
PF_BIN="${PF_BIN:-pulse-factory}"
PF_UI_DIR="${PF_UI_DIR:-${PF_SHARE_DIR:-$HOME/.local/share/pulse-factory}/ui}"
browsers="${PLAYWRIGHT_BROWSERS_PATH:-$HOME/.cache/ms-playwright}"
started=$(date +%s)

fail() { echo "ui-pair: FAIL: $*" >&2; exit 1; }

for tool in jq node npm; do command -v "$tool" >/dev/null || fail "$tool is not on PATH"; done
[ "$(node -p 'process.versions.node.split(".")[0]')" -ge 22 ] || fail "Node 22 or later is needed"
pf_bin="$(command -v "$PF_BIN" || true)"
[ -n "$pf_bin" ] && [ -x "$pf_bin" ] || fail "PF_BIN=$PF_BIN is not an executable"
if [ -n "${PF_BRIDGE_BIN:-}" ]; then
  bridge_bin="$(command -v "$PF_BRIDGE_BIN" || true)"
  [ -n "$bridge_bin" ] && [ -x "$bridge_bin" ] || fail "PF_BRIDGE_BIN=$PF_BRIDGE_BIN is not an executable (the pf-client-bridge binary)"
elif [ -x "$(dirname "$pf_bin")/pf-client-bridge" ]; then
  bridge_bin="$(dirname "$pf_bin")/pf-client-bridge"
else
  bridge_bin="$(command -v pf-client-bridge || true)"
  [ -n "$bridge_bin" ] || fail "no pf-client-bridge binary: set PF_BRIDGE_BIN, or put pf-client-bridge next to PF_BIN or on PATH"
fi
[ -f "$PF_UI_DIR/index.html" ] || fail "PF_UI_DIR=$PF_UI_DIR has no index.html: install the UI first with scripts/install-ui.sh (this script builds nothing)"
[ -d "$browsers/chromium-1248" ] || fail "Chromium 1248 is not in the Playwright cache ($browsers/chromium-1248): it is the browser of @playwright/test 1.64.1, and this script downloads none"

if [ ! -d "$root/apps/node_modules" ]; then
  PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 npm --prefix "$root/apps" ci --no-audit --no-fund
fi

tmp="$(mktemp -d)"
controller_pid=""
bridge_pid=""
cleanup() {
  for pid in "$bridge_pid" "$controller_pid"; do
    if [ -n "$pid" ]; then
      kill "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
    fi
  done
  rm -rf "$tmp"
}
trap cleanup EXIT

# The controller: the API on a free loopback port, its state in the temp directory. Its home folder is a
# directory of the temp directory, so the Add a project picker lists that and never the operator's own
# (r1.s5.w5). The spec gets the path in PF_D12_HOME and makes its folder there.
home="$tmp/home"
mkdir -p "$home"
export PF_D12_HOME="$home"
export PF_STATE_DIR="$tmp/state"
HOME="$home" "$pf_bin" controller --api-listen 127.0.0.1:0 > "$tmp/controller.log" 2>&1 &
controller_pid=$!
waited=0
until [ "$("$pf_bin" api status --json 2>/dev/null | jq -r '.state // empty' 2>/dev/null || true)" = "listening" ]; do
  kill -0 "$controller_pid" 2>/dev/null || fail "the controller exited: $(head -c 400 "$tmp/controller.log")"
  waited=$((waited + 1))
  [ "$waited" -lt 200 ] || fail "the API was not listening after 20 s"
  sleep 0.1
done
listen="$("$pf_bin" api status --json | jq -r '.listen')"
case "$listen" in 127.0.0.1:*) ;; *) fail "the API listens on $listen, not on 127.0.0.1" ;; esac

# The pairing code is a secret: it goes to the test through the environment and is never printed.
code="$("$pf_bin" device pair --json | jq -r '.code')"
[ -n "$code" ] && [ "$code" != "null" ] || fail "device pair made no code"

# The bridge, with its state in the temp directory.
"$bridge_bin" --ui "$PF_UI_DIR" --state "$tmp/bridge" > "$tmp/bridge.log" 2>&1 &
bridge_pid=$!
bridge_url=""
waited=0
until [ -n "$bridge_url" ]; do
  kill -0 "$bridge_pid" 2>/dev/null || fail "the bridge exited: $(head -c 400 "$tmp/bridge.log")"
  bridge_url="$(sed -n 's/^listening \(http:\/\/127\.0\.0\.1:[0-9]*\)$/\1/p' "$tmp/bridge.log" | head -n 1)"
  waited=$((waited + 1))
  [ "$waited" -lt 100 ] || fail "the bridge printed no listening line in 10 s"
  sleep 0.1
done

if [ -n "${PF_SHOTS_DIR:-}" ]; then mkdir -p "$PF_SHOTS_DIR"; fi
export PF_BRIDGE_URL="$bridge_url" PF_CODE="$code" PF_BRIDGE_STATE="$tmp/bridge"
export PF_BIN="$pf_bin" PF_E2E_OUT="$tmp/playwright-output"
npm --prefix "$root/apps" run e2e --workspace ui -- e2e/pair.spec.ts || fail "the Playwright journey failed"

echo "ui-pair: ok ($(( $(date +%s) - started )) s)"
