#!/usr/bin/env bash
# d23: answer the factory in the app. The Needs you page and the Settings rows in headless Chromium,
# through the test bridge, against the real controller, API and engine on the paper env (`paper-d20`):
# the stop-rule question is answered with a typed answer and two clicks, the spine PR is merged with two
# clicks, the run shows what it waits for and then that it is done, and a chat policy is set and unset.
#
#   PF_BIN=target/debug/pulse-factory PF_UI_DIR=apps/ui/dist PF_D23_START=cli \
#     scripts/demo/ui-needs-you.sh
#
# `PF_D23_START` is how the run starts (the default is `card`):
#   card  the start card of the chat starts the run (w2's `e2e/start-card.spec.ts`, which runs first),
#         on the env that `scripts/demo/start-env.sh` builds; the controller's chat is the replay
#         wrapper `<env>/claude-wrapper`. It needs `PF_REPLAY_BIN`. This is the ledger's command, on the
#         merged tree.
#   cli   this script arms `paper-d20` itself with the CLI and takes no claim: the device's first act
#         takes it. No chat starts and no replay binary is needed.
# Both modes run the same Needs you and Settings journey, `e2e/needs-you.spec.ts`.
#
# `PF_BIN` is a built binary (default `pulse-factory` on PATH). `PF_BRIDGE_BIN` is the built
# `pf-client-bridge` (default: next to `PF_BIN`, else on PATH). `PF_REPLAY_BIN` is the built
# `pf-claude-replay` (default: next to `PF_BIN`, else on PATH; card mode only). `PF_UI_DIR` is the built
# bundle (default `${PF_SHARE_DIR:-$HOME/.local/share/pulse-factory}/ui`). This script builds nothing.
# It runs `npm ci` in apps/ only when apps/node_modules is missing, and it downloads no browser:
# Chromium 1248 must be in the Playwright cache. It fails, never skips, when a tool, a binary, the bundle,
# the paper package, `rule.sh`, a fixture or that browser is missing.
#
# It starts a controller with the API on 127.0.0.1 in a temp state directory, with its HOME and
# PF_SHARE_DIR in the temp directory, and the bridge. It calls no GitHub, starts no real `claude`, spends
# nothing, and reaches nothing beyond 127.0.0.1 and the bare repo of the paper forge. The pairing code
# goes to the tests through the environment and is never printed. It keeps everything in one temp
# directory, and removes it on exit after it stops the bridge and the controller.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
PF_BIN="${PF_BIN:-pulse-factory}"
PF_UI_DIR="${PF_UI_DIR:-${PF_SHARE_DIR:-$HOME/.local/share/pulse-factory}/ui}"
mode="${PF_D23_START:-card}"
browsers="${PLAYWRIGHT_BROWSERS_PATH:-$HOME/.cache/ms-playwright}"
shipped_scripts="$root/workflows/ossify-spine/scripts"
src="$here/paper-d20"
limit_seconds=330
started=$(date +%s)

fail() { echo "ui-needs-you: FAIL: $*" >&2; exit 1; }
check() { [ "$1" = "$2" ] || fail "$3: wanted '$2', got '$1'"; }

case "$mode" in
  card | cli) ;;
  *) fail "PF_D23_START=$mode: it is 'card' or 'cli'" ;;
esac

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

[ -f "$PF_UI_DIR/index.html" ] || fail "PF_UI_DIR=$PF_UI_DIR has no index.html: the bundle must be built first (this script builds nothing)"
[ -d "$src/workflow" ] && [ -d "$src/seats" ] || fail "no paper-d20 package at $src"
[ -f "$shipped_scripts/rule.sh" ] || fail "no rule.sh at $shipped_scripts"
[ -f "$root/apps/ui/e2e/needs-you.spec.ts" ] || fail "no journey at apps/ui/e2e/needs-you.spec.ts"
[ -d "$browsers/chromium-1248" ] || fail "Chromium 1248 is not in the Playwright cache ($browsers/chromium-1248): it is the browser of @playwright/test 1.64.1, and this script downloads none"

replay_bin=""
if [ "$mode" = card ]; then
  # The card journey, the recorded proposal and the env builder are w2's (the round-2 merge).
  for needed in scripts/demo/start-env.sh apps/ui/e2e/start-card.spec.ts crates/pf-adapter-claude/fixtures/start_proposal.jsonl; do
    [ -e "$root/$needed" ] || fail "the start card journey is w2's (round-2 merge): $needed is missing; run PF_D23_START=cli on this tree"
  done
  replay_bin="$(sibling PF_REPLAY_BIN pf-claude-replay "${PF_REPLAY_BIN:-}")"
fi

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
      for _ in $(seq 1 100); do kill -0 "$pid" 2>/dev/null || break; sleep 0.1; done
      kill -9 "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
    fi
  done
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
spine="demo-d23"
base="main"
branch="spine-d23"
forge="$env/paper/forge.git"
if [ "$mode" = cli ]; then
  # A copy of paper-d20 with the shipped `rule.sh`, as d20 copies it, its bare forge and its seed.
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
else
  # w2's builder (K3): the paper package under PF_SHARE_DIR and the replay wrapper at <env>/claude-wrapper.
  export PF_REPLAY_BIN="$replay_bin"
  "$root/scripts/demo/start-env.sh" "$env" > "$tmp/start-env.log" 2>&1 || fail "start-env.sh failed: $(head -c 400 "$tmp/start-env.log")"
  [ -x "$env/claude-wrapper" ] || fail "start-env.sh left no executable $env/claude-wrapper"
fi

# ------------------------------------------------------------------------------- the controller
if [ "$mode" = card ]; then
  HOME="$tmp/home" PF_CHAT_CLAUDE_BIN="$env/claude-wrapper" PF_SERVER_NAME=d23-server \
    "$pf_bin" controller --tick-ms 200 --api-listen 127.0.0.1:0 > "$tmp/controller.log" 2> "$stderr_file" &
else
  HOME="$tmp/home" PF_SERVER_NAME=d23-server \
    "$pf_bin" controller --tick-ms 200 --api-listen 127.0.0.1:0 > "$tmp/controller.log" 2> "$stderr_file" &
fi
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

# One project, named d23: the card journey starts the run in it, and the Settings rows name it.
mkdir -p "$tmp/work/d23"
git -C "$tmp/work/d23" init -q -b main
git -C "$tmp/work/d23" -c user.name=d23 -c user.email=d23@example.invalid commit -q --allow-empty -m d23
project_json="$("$pf_bin" project add "$tmp/work/d23" --name d23 --json)" || fail "project add d23 failed"
project_id="$(printf '%s' "$project_json" | jq -r '.project.id // .id')"
printf '%s' "$project_id" | grep -qE '^P-[0-9]+$' || fail "project add gave no P-<n> ID: $project_json"

# The pairing code is a secret: it goes to the tests through the environment and is never printed.
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

# The ruling the journey types: the ledgers must not hold it.
answer_text="go with the d23 ruling 5e2b7c"

# What both journeys read (the card journey reads the first six and the project's name).
export PF_BRIDGE_URL="$bridge_url" PF_CODE="$code" PF_STATE_DIR="$tmp/state" PF_BRIDGE_STATE="$tmp/bridge"
export PF_E2E_OUT="$tmp/playwright-output" PF_D23_PROJECT="d23" PF_D23_PROJECT_ID="$project_id"
export PF_D23_ANSWER="$answer_text"
export PF_D23_CLOSE_FILE="$env/spine/closed.flag"

journey() { # <spec> <name>
  local out="$tmp/playwright-$2.out"
  if ! npm --prefix "$root/apps" run e2e --workspace ui -- "$1" 2>&1 | tee "$out"; then
    fail "the Playwright journey $1 failed"
  fi
  grep -q '1 passed' "$out" || fail "the journey $1 did not report '1 passed'"
  if grep -qi 'skipped' "$out"; then fail "the journey $1 skipped a test"; fi
}

# --------------------------------------------------------------------------------- the start
if [ "$mode" = cli ]; then
  arm_args=("$spine" --workflow "$env/paper/workflow" --ledger "$run_ledger" --paper "$env/paper"
    --param "spine_branch=$branch" --param "spine_dir=$spine_dir" --param "worktree_root=$env/worktrees"
    --param "base_branch=$base" --yes)
  "$pf_bin" arm "${arm_args[@]}" > "$tmp/arm.out" || fail "arm refused: $(cat "$tmp/arm.out")"
  # No CLI claim: the device's first act takes it (from=none).
else
  journey e2e/start-card.spec.ts start-card
fi
run_id="$("$pf_bin" status --json | jq -r '.run // empty')"
[ -n "$run_id" ] || fail "status --json gave no run ID"
export PF_D23_RUN="$run_id"

# ------------------------------------------------------------------------------- the journey
journey e2e/needs-you.spec.ts needs-you

# ----------------------------------------------------------------------- what the ledgers hold
[ -f "$api_ledger" ] || fail "no api.ledger"
[ -f "$controller_ledger" ] || fail "no controller.ledger"
grep -qE ' device_answer D-1 [^ ]+ .*admitted' "$controller_ledger" || fail "controller.ledger has no admitted device_answer of D-1"
grep -qE ' device_ack D-1 [^ ]+ .*admitted' "$controller_ledger" || fail "controller.ledger has no admitted device_ack of D-1"
grep -qE "setting_set chat\.self_turn_cap admitted scope=project:$project_id value=5 by=D-1" "$controller_ledger" \
  || fail "controller.ledger has no setting_set of chat.self_turn_cap at project $project_id by D-1"
for verb in confirm_code answer ack run_list settings_set; do
  grep -qE "verb=$verb device=D-1 .*outcome=ok" "$api_ledger" || fail "api.ledger has no ok line of D-1 for $verb"
done
if [ "$mode" = cli ]; then
  git -C "$forge" merge-base --is-ancestor "$(git -C "$forge" rev-parse "refs/heads/$branch")" "$base" \
    || fail "the bare forge's $base does not hold the head of $branch"
  grep -qE "outcome=landed" "$run_ledger" || fail "the run ledger has no landed merge"
fi

# No token, pairing code, confirm code or answer text in a ledger, the controller's stderr or the bridge's log.
token="$(jq -r '.token' "$tmp/bridge/device.json")"
printf '%s' "$token" | grep -qE '^[0-9a-f]{64}$' || fail "the bridge's device file holds no 64-hex token"
for file in "$api_ledger" "$controller_ledger" "$chat_ledger" "$stderr_file" "$tmp/bridge.log"; do
  [ -f "$file" ] || continue
  for secret in "$token" "$code" "$answer_text"; do
    if grep -qF -- "$secret" "$file"; then fail "a token, a pairing code or the answer text is in $(basename "$file")"; fi
  done
  if grep -qE 'pfc1-[0-9a-f]{32}' "$file"; then fail "a confirm code is in $(basename "$file")"; fi
done

echo "ui-needs-you: ok (start: $mode, $(( $(date +%s) - started )) s)"
