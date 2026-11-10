#!/usr/bin/env bash
# The start card in headless Chromium, through the test bridge, against the real controller, the real
# Claude Code adapter and a replay of a recorded chat that proposes a start (the card's journey of
# r1.s4.w2; w3's d23 runs the same spec file).
#
#   PF_BIN=target/debug/pulse-factory PF_UI_DIR=apps/ui/dist PF_REPLAY_BIN=target/debug/pf-claude-replay \
#     scripts/demo/ui-start.sh
#
# `PF_BIN` is a built binary (default `pulse-factory` on PATH). `PF_BRIDGE_BIN` is the built
# `pf-client-bridge` (default: next to `PF_BIN`, else on PATH). `PF_REPLAY_BIN` is the built
# `pf-claude-replay` (default: next to `PF_BIN`, else on PATH). `PF_UI_DIR` is the built bundle (default
# `${PF_SHARE_DIR:-$HOME/.local/share/pulse-factory}/ui`). This script builds nothing. It runs `npm ci` in
# apps/ only when apps/node_modules is missing, and it downloads no browser: Chromium 1248 must be in the
# Playwright cache. It fails, never skips, when a tool, a binary, the bundle, a fixture or that browser is
# missing.
#
# It runs `start-env.sh` (the paper env and the chat's wrapper), starts a controller with the API on
# 127.0.0.1 in a temp state directory with `PF_CHAT_CLAUDE_BIN` set to that wrapper, adds the project
# `start-demo`, makes a pairing code and starts the bridge. Then it runs `apps/ui/e2e/start-card.spec.ts`
# with the environment that file reads (K1): PF_BRIDGE_URL, PF_CODE, PF_BIN, PF_STATE_DIR, PF_E2E_OUT and
# PF_D23_PROJECT. It checks `1 passed` and that nothing was skipped, then checks that no token, code or
# pairing code is in a ledger, the controller's stderr or the bridge's stream. The controller's `HOME` is in
# the temp directory. The script reaches no address beyond 127.0.0.1, stops everything on exit and removes
# its temp directory.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
PF_BIN="${PF_BIN:-pulse-factory}"
PF_UI_DIR="${PF_UI_DIR:-${PF_SHARE_DIR:-$HOME/.local/share/pulse-factory}/ui}"
browsers="${PLAYWRIGHT_BROWSERS_PATH:-$HOME/.cache/ms-playwright}"
started=$(date +%s)

fail() { echo "ui-start: FAIL: $*" >&2; exit 1; }

for tool in jq node npm git curl tee grep sed; do command -v "$tool" >/dev/null || fail "$tool is not on PATH"; done
[ "$(node -p 'process.versions.node.split(".")[0]')" -ge 22 ] || fail "Node 22 or later is needed"
pf_bin="$(command -v "$PF_BIN" || true)"
[ -n "$pf_bin" ] && [ -x "$pf_bin" ] || fail "PF_BIN=$PF_BIN is not an executable"
case "$pf_bin" in /*) ;; *) pf_bin="$PWD/$pf_bin" ;; esac
pf_dir="$(cd "$(dirname "$pf_bin")" && pwd)"

# A sibling binary: the named override, else the one next to PF_BIN, else the one on PATH.
sibling() {
  local var="$1" name="$2" override="$3" found=""
  if [ -n "$override" ]; then
    found="$(command -v "$override" || true)"
    [ -n "$found" ] && [ -x "$found" ] || fail "$var=$override is not an executable (the $name binary)"
  elif [ -x "$pf_dir/$name" ]; then
    found="$pf_dir/$name"
  else
    found="$(command -v "$name" || true)"
    [ -n "$found" ] || fail "no $name binary: set $var, or put $name next to PF_BIN or on PATH"
  fi
  case "$found" in /*) ;; *) found="$PWD/$found" ;; esac
  printf '%s' "$found"
}
bridge_bin="$(sibling PF_BRIDGE_BIN pf-client-bridge "${PF_BRIDGE_BIN:-}")"
replay_bin="$(sibling PF_REPLAY_BIN pf-claude-replay "${PF_REPLAY_BIN:-}")"

[ -f "$PF_UI_DIR/index.html" ] || fail "PF_UI_DIR=$PF_UI_DIR has no index.html: the bundle must be built first (this script builds nothing)"
[ -x "$here/start-env.sh" ] || fail "no start-env.sh next to this script"
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

unset PF_SEAT_TOKEN PULSE_FACTORY_CHAT
# The browser and npm keep the operator's own HOME; only the controller's HOME is in the temp directory.
export PF_STATE_DIR="$tmp/state" PF_SHARE_DIR="$tmp/share"
# Git must not read the host's configuration, hooks or credentials.
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 GIT_TERMINAL_PROMPT=0
mkdir -p "$tmp/home" "$PF_SHARE_DIR"
stderr_file="$tmp/controller.stderr"
chat_ledger="$PF_STATE_DIR/chat.ledger"
controller_ledger="$PF_STATE_DIR/controller.ledger"
api_ledger="$PF_STATE_DIR/api.ledger"

# The paper env and the chat's wrapper (K3).
PF_BIN="$pf_bin" PF_REPLAY_BIN="$replay_bin" "$here/start-env.sh" "$tmp/start" > "$tmp/start-env.out" 2>&1 \
  || fail "start-env.sh failed: $(cat "$tmp/start-env.out")"
run_ledger="$tmp/start/ledger.md"

# The project the card proposes for: one repo with one commit, on `main`.
mkdir -p "$tmp/work/start-demo"
git -C "$tmp/work/start-demo" init -q -b main
git -C "$tmp/work/start-demo" -c user.name=demo -c user.email=demo@example.invalid commit -q --allow-empty -m demo

# The controller: the API on a free loopback port. The tick is slow: the armed paper run launches nothing
# while the journey runs.
HOME="$tmp/home" PF_CHAT_CLAUDE_BIN="$tmp/start/claude-wrapper" PF_SERVER_NAME=start-server \
  "$pf_bin" controller --tick-ms 3600000 --api-listen 127.0.0.1:0 > "$tmp/controller.log" 2> "$stderr_file" &
controller_pid=$!
waited=0
until [ "$("$pf_bin" api status --json 2>/dev/null | jq -r '.state // empty' 2>/dev/null || true)" = "listening" ]; do
  kill -0 "$controller_pid" 2>/dev/null || fail "the controller exited: $(head -c 400 "$stderr_file")"
  waited=$((waited + 1))
  [ "$waited" -lt 200 ] || fail "the API was not listening after 20 s"
  sleep 0.1
done
listen="$("$pf_bin" api status --json | jq -r '.listen')"
case "$listen" in 127.0.0.1:*) ;; *) fail "the API listens on $listen, not on 127.0.0.1" ;; esac

"$pf_bin" project add "$tmp/work/start-demo" --name start-demo > /dev/null || fail "project add start-demo failed"

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

export PF_BRIDGE_URL="$bridge_url" PF_CODE="$code" PF_BIN="$pf_bin" PF_E2E_OUT="$tmp/playwright-output" PF_D23_PROJECT="start-demo"
if ! npm --prefix "$root/apps" run e2e --workspace ui -- e2e/start-card.spec.ts 2>&1 | tee "$tmp/playwright.out"; then
  for f in "$tmp"/start/chat/*; do [ -f "$f" ] && echo "--- $(basename "$f"): $(head -c 600 "$f")" >&2; done
  echo "--- card: $("$pf_bin" session items S-1 --json 2>&1 | jq -c '.items[] | select(.body.kind == "start_card") | .body' 2>&1 | head -c 3000)" >&2
  echo "--- controller stderr: $(tail -c 800 "$stderr_file")" >&2
  fail "the Playwright journey failed"
fi
grep -q '1 passed' "$tmp/playwright.out" || fail "the journey did not report '1 passed'"
if grep -qi 'skipped' "$tmp/playwright.out"; then fail "the journey skipped a test"; fi

# The token is in no bridge answer, the stream's too: read the stream once more, as the page does.
token="$(jq -r '.token' "$tmp/bridge/device.json")"
printf '%s' "$token" | grep -qE '^[0-9a-f]{64}$' || fail "the bridge's device file holds no 64-hex token"
curl -sS -N --max-time 3 -H "Origin: $bridge_url" "$bridge_url/bridge/stream?session=S-1&after=0" > "$tmp/stream.out" 2>/dev/null || true
grep -q '"event":"frame"' "$tmp/stream.out" || fail "the bridge's stream gave no frame"
grep -q '"kind":"start_result"' "$tmp/stream.out" || fail "the bridge's stream holds no start_result"

# No token, pairing code or confirm code in a ledger, in the controller's stderr or in the stream.
for secret in "$token" "$code"; do
  for file in "$chat_ledger" "$controller_ledger" "$api_ledger" "$run_ledger" "$stderr_file" "$tmp/stream.out"; do
    [ -f "$file" ] || continue
    if grep -qF -- "$secret" "$file"; then fail "a token or a pairing code is in $(basename "$file")"; fi
  done
done
for file in "$chat_ledger" "$controller_ledger" "$api_ledger" "$run_ledger" "$stderr_file" "$tmp/stream.out"; do
  [ -f "$file" ] || continue
  if grep -qE 'pfc1-[0-9a-f]{32}' "$file"; then fail "a confirm code is in $(basename "$file")"; fi
done

echo "ui-start: ok ($(( $(date +%s) - started )) s)"
