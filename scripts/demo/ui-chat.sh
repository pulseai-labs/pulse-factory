#!/usr/bin/env bash
# d19: the Orchestrator chat in headless Chromium, through the test bridge, against the real controller,
# the real Claude Code adapter and a replay of recorded sessions.
#
#   PF_BIN=target/debug/pulse-factory PF_UI_DIR=apps/ui/dist PF_REPLAY_BIN=target/debug/pf-claude-replay \
#     scripts/demo/ui-chat.sh
#
# `PF_BIN` is a built binary (default `pulse-factory` on PATH). `PF_BRIDGE_BIN` is the built
# `pf-client-bridge` (default: next to `PF_BIN`, else on PATH). `PF_REPLAY_BIN` is the built
# `pf-claude-replay` (default: next to `PF_BIN`, else on PATH; scripts/install.sh installs it there).
# `PF_UI_DIR` is the built bundle (default `${PF_SHARE_DIR:-$HOME/.local/share/pulse-factory}/ui`).
# This script builds nothing. It runs `npm ci` in apps/ only when apps/node_modules is missing, and it
# downloads no browser: Chromium 1248 must be in the Playwright cache. It fails, never skips, when a
# tool, a binary, the bundle, a fixture or that browser is missing.
#
# It starts a controller with the API on 127.0.0.1 in a temp state directory, with `PF_CHAT_CLAUDE_BIN`
# set to a wrapper of `pf-claude-replay` (the `claude-binary` fake), so the chat runs an unqualified
# binary and spends nothing. The wrapper picks the fixture by the chat's working directory, which the
# adapter sets to the session's worktree: `*/alpha` plays `permissions.jsonl` (a `Bash` approval and a
# deny) and any other directory plays `artifacts_on.jsonl` (two turns, an effort change between them).
# The controller's `HOME` is in the temp directory, so the chat touches no real `~/.claude`; the browser
# and npm keep the operator's own. The UI runs through `pf-client-bridge` (the `desktop-bridge` fake),
# not through the Tauri app. The journey is apps/ui/e2e/chat.spec.ts. The script reaches no address
# beyond 127.0.0.1, prints no code and no token, stops everything on exit and removes its temp directory.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
PF_BIN="${PF_BIN:-pulse-factory}"
PF_UI_DIR="${PF_UI_DIR:-${PF_SHARE_DIR:-$HOME/.local/share/pulse-factory}/ui}"
browsers="${PLAYWRIGHT_BROWSERS_PATH:-$HOME/.cache/ms-playwright}"
fixtures="$root/crates/pf-adapter-claude/fixtures"
started=$(date +%s)

fail() { echo "ui-chat: FAIL: $*" >&2; exit 1; }

for tool in jq node npm git curl tee grep sed; do command -v "$tool" >/dev/null || fail "$tool is not on PATH"; done
[ "$(node -p 'process.versions.node.split(".")[0]')" -ge 22 ] || fail "Node 22 or later is needed"
pf_bin="$(command -v "$PF_BIN" || true)"
[ -n "$pf_bin" ] && [ -x "$pf_bin" ] || fail "PF_BIN=$PF_BIN is not an executable"
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
for fixture in permissions.jsonl artifacts_on.jsonl; do
  [ -f "$fixtures/$fixture" ] || fail "no fixture at $fixtures/$fixture"
done
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

unset PF_SEAT_TOKEN
export PF_STATE_DIR="$tmp/state"
stderr_file="$tmp/controller.stderr"
chat_ledger="$PF_STATE_DIR/chat.ledger"
controller_ledger="$PF_STATE_DIR/controller.ledger"

# Two repos with one commit each, on `main`.
make_repo() {
  mkdir -p "$1"
  git -C "$1" init -q -b main
  git -C "$1" -c user.name=d19 -c user.email=d19@example.invalid commit -q --allow-empty -m d19
}
make_repo "$tmp/work/alpha"
make_repo "$tmp/work/beta"

# The replay wrapper. The adapter's environment allowlist passes no Pulse Factory variable to the child,
# so the fixture is chosen here, by the working directory.
wrapper="$tmp/claude-wrapper"
cat > "$wrapper" <<WRAP
#!/bin/sh
case "\$(pwd)" in
  */alpha) fixture="$fixtures/permissions.jsonl" ;;
  *) fixture="$fixtures/artifacts_on.jsonl" ;;
esac
exec "$replay_bin" --fixture "\$fixture" "\$@"
WRAP
chmod +x "$wrapper"

# The controller: the API on a free loopback port, its state and its HOME in the temp directory.
mkdir -p "$tmp/home" "$tmp/share"
HOME="$tmp/home" PF_SHARE_DIR="$tmp/share" PF_CHAT_CLAUDE_BIN="$wrapper" PF_SERVER_NAME=d19-server \
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
grep -q 'unqualified Claude Code binary' "$stderr_file" || fail "the controller did not name the override as unqualified"

"$pf_bin" project add "$tmp/work/alpha" --name alpha > /dev/null || fail "project add alpha failed"
"$pf_bin" project add "$tmp/work/beta" --name beta > /dev/null || fail "project add beta failed"

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

# The texts the journey sends: no ledger may hold them.
ask_alpha="what does alpha hold, apart from this exact d19 text 7f3c91"
ask_beta_1="a first question for beta, with this exact d19 text 2b8e40"
ask_beta_2="a second question for beta, with this exact d19 text c5d1a7"

export PF_BRIDGE_URL="$bridge_url" PF_CODE="$code" PF_BRIDGE_STATE="$tmp/bridge"
export PF_BIN="$pf_bin" PF_E2E_OUT="$tmp/playwright-output" PF_ALPHA_DIR="$tmp/work/alpha"
export PF_D19_ASK_ALPHA="$ask_alpha" PF_D19_ASK_BETA_1="$ask_beta_1" PF_D19_ASK_BETA_2="$ask_beta_2"
if ! npm --prefix "$root/apps" run e2e --workspace ui -- e2e/chat.spec.ts 2>&1 | tee "$tmp/playwright.out"; then
  fail "the Playwright journey failed"
fi
grep -q '1 passed' "$tmp/playwright.out" || fail "the journey did not report '1 passed'"
if grep -qi 'skipped' "$tmp/playwright.out"; then fail "the journey skipped a test"; fi

# The token is in no bridge answer, the stream's too: read the stream once more, as the page does.
token="$(jq -r '.token' "$tmp/bridge/device.json")"
printf '%s' "$token" | grep -qE '^[0-9a-f]{64}$' || fail "the bridge's device file holds no 64-hex token"
curl -sS -N --max-time 3 -H "Origin: $bridge_url" "$bridge_url/bridge/stream?session=S-1&after=0" > "$tmp/stream.out" 2>/dev/null || true
grep -q '"event":"frame"' "$tmp/stream.out" || fail "the bridge's stream gave no frame"

# No token, no code and no sent text in a ledger, in the controller's stderr or in the stream.
for secret in "$token" "$code" "$ask_alpha" "$ask_beta_1" "$ask_beta_2"; do
  for file in "$chat_ledger" "$controller_ledger" "$stderr_file"; do
    [ -f "$file" ] || fail "no $(basename "$file")"
    if grep -qF -- "$secret" "$file"; then fail "a token, a code or a sent text is in $(basename "$file")"; fi
  done
done
for secret in "$token" "$code"; do
  if grep -qF -- "$secret" "$tmp/stream.out"; then fail "a token or a code is in the bridge's stream"; fi
done

echo "ui-chat: ok ($(( $(date +%s) - started )) s)"
