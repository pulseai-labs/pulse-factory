#!/usr/bin/env bash
# d33: take the options of every decision in the app. The Needs you page in headless Chromium, through the
# test bridge, against the real controller, API and engine on the paper package `paper-options` (w4's, the
# package of d32): the seat's stop with two options (the second recommended), the park of a seat that ended
# its turn with no done signal (a Resume with a typed note), the red gate with the workflow's two options, and
# the merge approval, each answered from its card in one press. The page's `Notification` is a recorder, so the
# run also shows that one notification came for each request that opened and that each notice was acknowledged.
#
#   PF_BIN=target/debug/pulse-factory PF_UI_DIR=apps/ui/dist scripts/demo/ui-needs-you-options.sh
#
# `PF_BIN` is a built binary (default `pulse-factory` on PATH). `PF_BRIDGE_BIN` is the built
# `pf-client-bridge` and `PF_HOST_BIN` the built `pf-host` (each defaults to the one next to `PF_BIN`, else on
# PATH). `PF_UI_DIR` is the built bundle (default `${PF_SHARE_DIR:-$HOME/.local/share/pulse-factory}/ui`).
# `PF_D33_PAPER` names another copy of the package `scripts/demo/paper-options` (the test of AC-9 runs a copy
# whose seat marks no option). This script builds nothing. It runs `npm ci` in apps/ only when apps/node_modules
# is missing, and it downloads no browser: Chromium 1248 must be in the Playwright cache. It fails, never skips,
# when a tool, a binary, the bundle, the package, `rule.sh` or that browser is missing.
#
# Like d23's `cli` mode, it arms the package itself with the CLI and takes no claim: the device's first act takes
# it. It starts a controller with the API on 127.0.0.1 in a temp state directory, with its HOME and PF_SHARE_DIR in
# the temp directory, and the bridge. It calls no GitHub, starts no real `claude`, spends nothing, and reaches
# nothing beyond 127.0.0.1 and the bare repo of the paper forge. The pairing code goes to the test through the
# environment and is never printed. Everything lives in one temp directory, which the script removes on exit after it
# stops the bridge, the controller and its hosts. Its own budget is 60 s, measured after `npm ci`.
#
# The steps (each prints `step <n>: ...`):
#   1. the stop: the card, its two options and the recommended one, the notification, one press;
#   2. the park: the card, the one Resume of the controller, a typed note, one press;
#   3. the red gate: the card, the workflow's two options, one press;
#   4. the merge approval: the card, no recommended mark, one press;
#   5. the server's side: the run ledger, the verifier's rulings file, controller.ledger and api.ledger;
#   6. what must not be there: no token, pairing code or confirm code in a ledger, the controller's stderr or the
#      bridge's log (the test checks the page's storage, console, URLs and origins).
# It exits non-zero on any failed check and prints `ui-needs-you-options: ok` at the end.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
PF_BIN="${PF_BIN:-pulse-factory}"
PF_UI_DIR="${PF_UI_DIR:-${PF_SHARE_DIR:-$HOME/.local/share/pulse-factory}/ui}"
browsers="${PLAYWRIGHT_BROWSERS_PATH:-$HOME/.cache/ms-playwright}"
shipped_scripts="$root/workflows/ossify-spine/scripts"
src="${PF_D33_PAPER:-$here/paper-options}"
limit_seconds=60
started=$(date +%s)

fail() { echo "ui-needs-you-options: FAIL: $*" >&2; exit 1; }
check() { [ "$1" = "$2" ] || fail "$3: wanted '$2', got '$1'"; }

for tool in jq node npm git curl tee grep sed cp mktemp sleep seq; do command -v "$tool" >/dev/null || fail "$tool is not on PATH"; done
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
host_bin="$(sibling PF_HOST_BIN pf-host "${PF_HOST_BIN:-}")"
# The controller starts a seat's host from the directory of its own binary, else from PATH.
[ "$(dirname "$host_bin")" = "$pf_dir" ] || export PATH="$(dirname "$host_bin"):$PATH"

[ -f "$PF_UI_DIR/index.html" ] || fail "PF_UI_DIR=$PF_UI_DIR has no index.html: the bundle must be built first (this script builds nothing)"
[ -d "$src/workflow" ] && [ -d "$src/seats" ] || fail "no paper package at $src (PF_D33_PAPER or scripts/demo/paper-options)"
[ -f "$shipped_scripts/rule.sh" ] || fail "no rule.sh at $shipped_scripts"
[ -f "$root/apps/ui/e2e/needs-you-options.spec.ts" ] || fail "no journey at apps/ui/e2e/needs-you-options.spec.ts"
[ -d "$browsers/chromium-1248" ] || fail "Chromium 1248 is not in the Playwright cache ($browsers/chromium-1248): it is the browser of @playwright/test 1.64.1, and this script downloads none"

if [ ! -d "$root/apps/node_modules" ]; then
  PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 npm --prefix "$root/apps" ci --no-audit --no-fund
fi
# The budget is measured after `npm ci`.
started=$(date +%s)

tmp="$(mktemp -d)"
controller_pid=""
bridge_pid=""
. "$here/lib/hosts.sh"
cleanup() {
  for pid in "$bridge_pid" "$controller_pid"; do
    if [ -n "$pid" ]; then
      kill "$pid" 2>/dev/null || true
      for _ in $(seq 1 100); do kill -0 "$pid" 2>/dev/null || break; sleep 0.1; done
      kill -9 "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
    fi
  done
  stop_hosts
  rm -rf "$tmp"
}
trap cleanup EXIT

within_limit() {
  [ $(( $(date +%s) - started )) -lt "$limit_seconds" ] \
    || fail "the demo did not finish within $limit_seconds s (waiting for: $1)"
}

unset PF_SEAT_TOKEN PULSE_FACTORY_CHAT
# Git must not read the host's configuration, hooks or credentials.
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 GIT_TERMINAL_PROMPT=0
export PF_STATE_DIR="$tmp/state" PF_SHARE_DIR="$tmp/share"
export PF_BIN="$pf_bin"
mkdir -p "$tmp/home" "$PF_SHARE_DIR"
stderr_file="$tmp/controller.stderr"
api_ledger="$PF_STATE_DIR/api.ledger"
controller_ledger="$PF_STATE_DIR/controller.ledger"
chat_ledger="$PF_STATE_DIR/chat.ledger"
env="$tmp/env"
spine_dir="$env/spine"
run_ledger="$env/run-ledger.md"
mkdir -p "$env"

# ----------------------------------------------------------------------------- the paper env
# A copy of the package with the shipped `rule.sh`, as d20 and d23 copy it, its bare forge and its seed.
spine="demo-d33"
base="main"
branch="spine-d33"
forge="$env/paper/forge.git"
mkdir -p "$env/paper" "$spine_dir" "$env/worktrees"
cp -R "$src/workflow" "$env/paper/workflow"
cp -R "$src/seats" "$env/paper/seats"
chmod +x "$env"/paper/seats/*.sh
mkdir -p "$env/paper/workflow/scripts"
cp "$shipped_scripts/rule.sh" "$env/paper/workflow/scripts/rule.sh"
: > "$run_ledger"
git init -q --bare -b "$base" "$forge"
git init -q -b "$base" "$tmp/seed"
echo "base" > "$tmp/seed/README.md"
git -C "$tmp/seed" add README.md
git -C "$tmp/seed" -c user.name=paper -c user.email=paper@example.invalid commit -q -m "base root"
git -C "$tmp/seed" push -q "$forge" "$base"
git -C "$tmp/seed" push -q "$forge" "$base:refs/heads/$branch"

# ------------------------------------------------------------------------------- the controller
# `--tick-ms 100` keeps the paper seats' hand-offs short.
HOME="$tmp/home" PF_SERVER_NAME=d33-server \
  "$pf_bin" controller --tick-ms 100 --api-listen 127.0.0.1:0 > "$tmp/controller.log" 2> "$stderr_file" &
controller_pid=$!
waited=0
until [ "$("$pf_bin" api status --json 2>/dev/null | jq -r '.state // empty' 2>/dev/null || true)" = "listening" ]; do
  kill -0 "$controller_pid" 2>/dev/null || fail "the controller exited: $(head -c 400 "$stderr_file")"
  waited=$((waited + 1))
  [ "$waited" -lt 200 ] || fail "the API was not listening after 10 s"
  sleep 0.05
done
listen="$("$pf_bin" api status --json | jq -r '.listen')"
case "$listen" in 127.0.0.1:*) ;; *) fail "the API listens on $listen, not on 127.0.0.1" ;; esac

# The pairing code is a secret: it goes to the test through the environment and is never printed.
code="$("$pf_bin" device pair --json | jq -r '.code')"
[ -n "$code" ] && [ "$code" != "null" ] || fail "device pair made no code"

# ----------------------------------------------------------------------------------- the bridge
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

# ------------------------------------------------------------------------------------- the arm
# The run starts from the CLI and takes no claim: the device's first act takes it (from=none). A device is told
# only of a request that opens after its stream joined, so the journey pairs the browser, opens Needs you and then
# runs this arm (`PF_D33_ARM`): the first request opens a few ticks after it.
arm_args=("$spine" --workflow "$env/paper/workflow" --ledger "$run_ledger" --paper "$env/paper"
  --param "spine_branch=$branch" --param "spine_dir=$spine_dir" --param "worktree_root=$env/worktrees"
  --param "base_branch=$base" --yes)
arm_script="$tmp/arm.sh"
{
  echo '#!/bin/sh'
  echo 'set -eu'
  printf 'exec %q' "$pf_bin"
  printf ' arm'
  for a in "${arm_args[@]}"; do printf ' %q' "$a"; done
  printf ' > %q\n' "$tmp/arm.out"
} > "$arm_script"
chmod +x "$arm_script"
run_dir=""

# What the journey reads. The note holds the words the paper verifier needs (`with care`).
note="Run the verifier again with care."
export PF_BRIDGE_URL="$bridge_url" PF_CODE="$code" PF_BRIDGE_STATE="$tmp/bridge"
export PF_E2E_OUT="$tmp/playwright-output" PF_D33_ARM="$arm_script" PF_D33_NOTE="$note"

# --------------------------------------------------------------------------------- the journey
# The spec prints the `step <n>:` lines 1 to 4.
out="$tmp/playwright.out"
if ! timeout "$limit_seconds" npm --prefix "$root/apps" run e2e --workspace ui -- e2e/needs-you-options.spec.ts 2>&1 | tee "$out"; then
  fail "the Playwright journey e2e/needs-you-options.spec.ts failed"
fi
grep -q '1 passed' "$out" || fail "the journey did not report '1 passed'"
if grep -qi 'skipped' "$out"; then fail "the journey skipped a test"; fi
within_limit "the journey"
run_id="$("$pf_bin" status --json | jq -r '.run // empty')"
[ -n "$run_id" ] || fail "status --json gave no run ID"
run_dir="$PF_STATE_DIR/runs/$run_id"

# ----------------------------------------------------------------------------------- 5 the server
[ -f "$api_ledger" ] || fail "no api.ledger"
[ -f "$controller_ledger" ] || fail "no controller.ledger"
grep -qF 'option s2 by seat, recommended' "$run_ledger" || fail "step 5: the run ledger holds no 'option s2 by seat, recommended'"
grep -qF 'option w1 by workflow, recommended' "$run_ledger" || fail "step 5: the run ledger holds no 'option w1 by workflow, recommended'"
grep -qF 'with care' "$run_dir/out/item-1/rulings.md" || fail "step 5: the verifier's rulings file holds no note"
for want in " option=s2" " option=w1" " option=approve"; do
  grep -qF -- "$want" "$controller_ledger" || fail "step 5: controller.ledger has no line with '$want'"
done
# One `opened` notice for each request that opened; the device acknowledged each, at its position or above it,
# with the outcome `shown`.
opened="$(grep -F 'event=opened' "$controller_ledger" | sed -n 's/.* notice \([0-9][0-9]*\) request=.*/\1/p')"
[ "$(printf '%s\n' "$opened" | grep -c .)" -ge 4 ] || fail "step 5: controller.ledger holds fewer than four opened notices"
highest_ack="$(grep -E 'notice_ack device:D-1 position=[0-9]+ outcome=shown' "$controller_ledger" | sed -n 's/.*position=\([0-9][0-9]*\) outcome=shown.*/\1/p' | sort -n | tail -n 1)"
[ -n "$highest_ack" ] || fail "step 5: controller.ledger holds no notice_ack of D-1 with outcome=shown"
for position in $opened; do
  [ "$highest_ack" -ge "$position" ] || fail "step 5: the notice at position $position was not acknowledged as shown (the highest acknowledgement is $highest_ack)"
done
if grep -qE 'notice_ack device:D-1 .*outcome=blocked' "$controller_ledger"; then fail "step 5: D-1 acknowledged a notice as blocked"; fi
for verb in confirm_code answer ack notice_ack; do
  grep -qE "verb=$verb device=D-1 .*outcome=ok" "$api_ledger" || fail "step 5: api.ledger has no ok line of D-1 for $verb"
done
git -C "$forge" merge-base --is-ancestor "$(git -C "$forge" rev-parse "refs/heads/$branch")" "$base" \
  || fail "step 5: the bare forge's $base does not hold the head of $branch"
grep -qE "outcome=landed" "$run_ledger" || fail "step 5: the run ledger has no landed merge"
echo "step 5: the run ledger, the rulings file, controller.ledger and api.ledger hold each option act and a notice_ack of D-1 for each notice"

# ----------------------------------------------------------------------------------- 6 not there
token="$(jq -r '.token' "$tmp/bridge/device.json")"
printf '%s' "$token" | grep -qE '^[0-9a-f]{64}$' || fail "the bridge's device file holds no 64-hex token"
for file in "$api_ledger" "$controller_ledger" "$chat_ledger" "$run_ledger" "$stderr_file" "$tmp/bridge.log"; do
  [ -f "$file" ] || continue
  for secret in "$token" "$code"; do
    if grep -qF -- "$secret" "$file"; then fail "step 6: a token or a pairing code is in $(basename "$file")"; fi
  done
  if grep -qE 'pfc1-[0-9a-f]{32}' "$file"; then fail "step 6: a confirm code is in $(basename "$file")"; fi
done
echo "step 6: no token, pairing code or confirm code is in a ledger, the controller's stderr or the bridge's log"

echo "ui-needs-you-options: ok ($(( $(date +%s) - started )) s)"
