#!/usr/bin/env bash
# The Orchestrator settles a halt below the line (d35): a stall opens a request below the line in a
# project with a live clear grant, the controller wakes the project's Orchestrator chat with one marked
# block, and the chat settles the stall with `clear` from below the chat. A question above the line is
# not in the block, and the same chat is refused when it tries to settle it. It starts a controller with
# `--api-listen 127.0.0.1:0` on a temp state directory, arms the paper spine `paper-settle` from a start
# card (so the run has a project) and drives it from the paired device `D-1`, with a pinned `curl`.
#
# It does not build: `PF_BIN`, `PF_REPLAY_BIN` and `PF_STREAM_BIN` are built binaries (defaults
# `pulse-factory`, `pf-claude-replay` and `pf-stream-reader` on PATH; `scripts/install.sh` installs
# them). It needs `curl`, `jq`, `git`, `sha256sum` and coreutils. It spends nothing, starts no real
# `claude`, calls no `gh` and reaches nothing beyond 127.0.0.1 and the bare repo of the paper forge.
# Everything lives in one temp directory, which the script removes on exit after it stops the controller
# and its hosts. No model runs: the chat is `pf-claude-replay` on a recorded turn, and the settles are
# made by a shell that `start-env.sh` (with `PF_WAKE_SETTLE=1`) starts below the chat.
#
#   PF_BIN=target/debug/pulse-factory PF_REPLAY_BIN=target/debug/pf-claude-replay \
#     PF_STREAM_BIN=target/debug/pf-stream-reader scripts/demo/orchestrator-settles.sh
#
# The stall takes about two minutes: the paper implementer of `item-1` gives no turn on its first two
# launches, and the stage's stall limit is one minute (the floor). So the limit of the script is 200 s.
#
# The steps (each prints `step <n>: ...`):
#   1. start the controller, pair D-1, add the project P-1, and give it the clear grant from the CLI;
#   2. start the Orchestrator session S-1 (wrapper launch 1): it proposes the start, D-1 confirms the
#      card with a fresh code over the API, and the run arms with the project P-1;
#   3. end S-1 and start S-2 (wrapper launch 2): the newest live session takes the wake;
#   4. the run parks `item-2` on its question (above the line) and, after two stalls, `item-1` on a
#      stall (below it). The stall's notice wakes S-2: one `wake` item, one `Notice` send and one turn
#      with the cause `notice`. The block lists the stall alone, with `acts=clear,redispatch`. The
#      question is not in it, and reaches the chat as its `request_opened` item only;
#   5. the settle of the stall is admitted: the run ledger holds the settle, `requests` shows the stall
#      answered with who settled it and why, the controller ledger holds the `woken` record, and the
#      item launches again and reaches its PR;
#   6. the settle of the question is refused (`above the line`), and the question stays open with the
#      verbs `answer` and `clear` for the claim;
#   7. `grant revoke --project P-1`, and the second settle of the stall is refused (`holds no live
#      clear grant`);
#   8. no confirm code, claim token or device token is in any ledger or in the controller's stderr.
#
# `PAPER_SRC` names a copy of `scripts/demo/paper-settle` to run instead. It exits non-zero on any
# failed check and prints `orchestrator-settles: ok` at the end.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"
PF_BIN="${PF_BIN:-pulse-factory}"
PF_REPLAY_BIN="${PF_REPLAY_BIN:-pf-claude-replay}"
PF_STREAM_BIN="${PF_STREAM_BIN:-pf-stream-reader}"
PAPER_SRC="${PAPER_SRC:-$here/paper-settle}"
limit_seconds=200

fail() { echo "orchestrator-settles: FAIL: $*" >&2; exit 1; }
for tool in curl jq git sha256sum base32 base64 od tail head tr grep sed cut sleep wc; do
  command -v "$tool" >/dev/null || fail "$tool is not on PATH (this script needs it)"
done
for bin in "$PF_BIN" "$PF_REPLAY_BIN" "$PF_STREAM_BIN"; do
  command -v "$bin" >/dev/null || [ -x "$bin" ] || fail "$bin is not an executable"
done
[ -x "$here/start-env.sh" ] || fail "no start-env.sh next to this script"
[ -d "$PAPER_SRC/workflow" ] || fail "no paper workflow at $PAPER_SRC"
# The wrapper and the readers run from other directories, so the binaries need absolute paths.
abs() {
  local p
  p="$(command -v "$1")"
  case "$p" in /*) printf '%s' "$p" ;; *) printf '%s' "$PWD/$p" ;; esac
}
PF_BIN="$(abs "$PF_BIN")"
PF_REPLAY_BIN="$(abs "$PF_REPLAY_BIN")"
PF_STREAM_BIN="$(abs "$PF_STREAM_BIN")"
# `start-env.sh` is a child process and reads these from its environment.
export PF_BIN PF_REPLAY_BIN PF_STREAM_BIN PAPER_SRC
export PF_WAKE_SETTLE=1

tmp="$(mktemp -d)"
pid=""
. "$here/lib/hosts.sh"
stop_controller() {
  [ -n "$pid" ] || return 0
  kill "$pid" 2>/dev/null || true
  for _ in $(seq 1 100); do kill -0 "$pid" 2>/dev/null || break; sleep 0.1; done
  kill -9 "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  pid=""
}
cleanup() {
  stop_controller
  stop_hosts
  rm -rf "$tmp"
}
trap cleanup EXIT

unset PF_SEAT_TOKEN PULSE_FACTORY_CHAT
export HOME="$tmp/home" PF_STATE_DIR="$tmp/state" PF_SHARE_DIR="$tmp/share"
# Git must not read the host's configuration, hooks or credentials.
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 GIT_TERMINAL_PROMPT=0
mkdir -p "$HOME" "$PF_SHARE_DIR"
stderr_file="$tmp/controller.stderr"
api_ledger="$PF_STATE_DIR/api.ledger"
controller_ledger="$PF_STATE_DIR/controller.ledger"
chat_ledger="$PF_STATE_DIR/chat.ledger"
started="$(date +%s)"

check() { [ "$1" = "$2" ] || fail "$3: wanted '$2', got '$1'"; }
within_limit() {
  [ $(( $(date +%s) - started )) -lt "$limit_seconds" ] \
    || fail "the demo did not finish within $limit_seconds s (waiting for: $1)"
}

# The paper env and the chat's wrapper: launch 1 proposes the start, launch 2 and later wait for the wake
# and settle from below the chat.
"$here/start-env.sh" "$tmp/start" > "$tmp/start-env.out" 2>&1 || fail "start-env.sh failed: $(cat "$tmp/start-env.out")"
start="$tmp/start"
run_ledger="$start/ledger.md"
chat_dir="$start/chat"

# ------------------------------------------------------------------------- the API, pinned
addr="" spki="" pin="" status="" reply=""
start_controller() {
  # `--tick-ms 100` keeps the paper seats' hand-offs short.
  PF_CHAT_CLAUDE_BIN="$start/claude-wrapper" PF_SERVER_NAME=demo-server \
    "$PF_BIN" controller --tick-ms 100 --api-listen 127.0.0.1:0 >/dev/null 2>>"$stderr_file" &
  pid=$!
  local state=""
  for _ in $(seq 1 200); do
    state="$("$PF_BIN" api status --json 2>/dev/null | jq -r '.state' 2>/dev/null || true)"
    [ "$state" = listening ] && break
    kill -0 "$pid" 2>/dev/null || fail "the controller exited before it listened: $(cat "$stderr_file")"
    sleep 0.05
  done
  check "$state" listening "the API state after the controller started"
  addr="$("$PF_BIN" api status --json | jq -r '.listen')"
  spki="$("$PF_BIN" api status --json | jq -r '.key_spki_sha256')"
  case "$addr" in 127.0.0.1:*) ;; *) fail "the API listens on '$addr', not on 127.0.0.1" ;; esac
}

# The raw bytes of a `pf1-` code, in a file (docs/contracts/api.md, "Pairing codes").
decode_code() {
  local text="${1#pf1-}" padding
  text="$(printf '%s' "$text" | tr 'a-z' 'A-Z')"
  padding=$(( (8 - ${#text} % 8) % 8 ))
  while [ "$padding" -gt 0 ]; do text="$text="; padding=$((padding - 1)); done
  printf '%s' "$text" | base32 -d > "$2"
}

# One call, pinned to the server key as a client must: `api_call <method> <path> <token or -> <body or ->`.
# It sets `status` and `reply`.
api_call() {
  local method="$1" path="$2" token="$3" body="$4"
  local args=(-sS -k --max-time 20 --pinnedpubkey "sha256//$pin" -X "$method" -w '\n%{http_code}')
  [ "$token" = - ] || args+=(-H "Authorization: Bearer $token")
  [ "$body" = - ] || args+=(-H 'Content-Type: application/json' --data-binary "$body")
  local out rc=0
  out="$(curl "${args[@]}" "https://$addr$path" 2>"$tmp/curl.err")" || rc=$?
  [ "$rc" -eq 0 ] || fail "curl exited $rc for $method $path: $(cat "$tmp/curl.err")"
  status="${out##*$'\n'}"
  reply="${out%$'\n'*}"
}

# A verb call as the device: `verb <verb> <body>`.
verb() { api_call POST "/v1/verbs/$1" "$token1" "$2"; }

pairing_codes="$tmp/pairing-codes.txt"
confirm_codes="$tmp/confirm-codes.txt"
: > "$pairing_codes"
: > "$confirm_codes"
paired_id=""
pair() {
  local code body
  code="$("$PF_BIN" device pair --json | jq -r '.code')"
  printf '%s\n' "$code" >> "$pairing_codes"
  decode_code "$code" "$tmp/code.bin"
  check "$(od -An -tx1 -v -j11 -N32 "$tmp/code.bin" | tr -d ' \n')" "$spki" "the SPKI hash in the pairing code"
  pin="$(tail -c +12 "$tmp/code.bin" | head -c 32 | base64 -w0)"
  body="$(jq -nc --arg code "$code" --arg name "$1" '{code: $code, device_name: $name}')"
  api_call POST /v1/pair - "$body"
  check "$status" 200 "pairing $1 ($reply)"
  printf '%s' "$reply" | jq -r '.token' > "$2"
  grep -qE '^[0-9a-f]{64}$' "$2" || fail "the token of $1 is not 64 hex digits"
  paired_id="$(printf '%s' "$reply" | jq -r '.device.id')"
}

# A confirm code for a subject: `issue <subject JSON> <command id>`. It sets `code`.
code=""
issue() {
  verb confirm_code "$(jq -nc --argjson s "$1" --arg c "$2" '{subject: $s, command_id: $c}')"
  check "$status" 200 "confirm_code ($reply)"
  code="$(printf '%s' "$reply" | jq -r '.code')"
  printf '%s' "$code" | grep -qE '^pfc1-[0-9a-f]{32}$' || fail "the confirm code has the wrong shape"
  printf '%s\n' "$code" >> "$confirm_codes"
}

# Wait, with a bound, until a ledger holds a line: `wait_line <file> <fixed text> <what>`.
wait_line() {
  for _ in $(seq 1 3000); do
    if grep -qF -- "$2" "$1" 2>/dev/null; then return 0; fi
    within_limit "$3"
    sleep 0.1
  done
  fail "$3: no line with '$2' in $(basename "$1")"
}

# Wait, with a bound, until a file exists: `wait_file <path> <what>`.
wait_file() {
  for _ in $(seq 1 3000); do
    [ -e "$1" ] && return 0
    within_limit "$2"
    sleep 0.1
  done
  fail "$2: no file $1"
}

# The request of a unit in `requests --all --json`, as one JSON object, or nothing: `request_of <unit>`.
request_of() {
  "$PF_BIN" requests --all --json 2>/dev/null \
    | jq -c --arg u "$1" '[.requests[] | select(.unit == $u)] | sort_by(.opened_at) | last // empty' 2>/dev/null || true
}

# Wait, with a bound, until a unit has a request that satisfies a jq filter; it prints the request.
wait_request() {
  local unit="$1" filter="$2" what="$3" r
  while :; do
    r="$(request_of "$unit")"
    if [ -n "$r" ] && printf '%s' "$r" | jq -e "$filter" >/dev/null 2>&1; then printf '%s' "$r"; return 0; fi
    within_limit "$what"
    sleep 0.2
  done
}

# The text of the wake item(s) of a session: one JSON string for each, one per line.
wake_blocks() {
  "$PF_BIN" session items "$1" --json 2>/dev/null \
    | jq -c '[.. | objects | select(.body? and .body.kind? == "notice" and .body.code? == "wake")] | .[] | .body.text' 2>/dev/null || true
}

# ----------------------------------------------------------------------------------- 1
start_controller
pair demo-macbook "$tmp/token-1"
check "$paired_id" D-1 "step 1: the device"
token1="$(cat "$tmp/token-1")"
mkdir -p "$tmp/work/alpha"
git -C "$tmp/work/alpha" init -q -b main
git -C "$tmp/work/alpha" -c user.name=demo -c user.email=demo@example.invalid commit -q --allow-empty -m demo
verb project_add "$(jq -nc --arg root "$tmp/work/alpha" '{root: $root, name: "alpha", command_id: "demo-add-alpha"}')"
check "$status" 200 "step 1: project_add ($reply)"
project="$(printf '%s' "$reply" | jq -r '.project.id')"
check "$project" P-1 "step 1: the project"
"$PF_BIN" grant clear --project "$project" >/dev/null || fail "step 1: the clear grant was refused"
echo "step 1: controller up on $addr, D-1 paired, project $project added with a live clear grant"

# ----------------------------------------------------------------------------------- 2
verb session_start "$(jq -nc --arg p "$project" '{project_id: $p, command_id: "demo-start-1"}')"
check "$status" 200 "step 2: session_start ($reply)"
check "$(printf '%s' "$reply" | jq -r '.session.id')" S-1 "step 2: the first session"
verb session_send "$(jq -nc '{session_id: "S-1", text: "start the spine s1 of this project", command_id: "demo-send-1"}')"
check "$status" 200 "step 2: session_send ($reply)"
card=""
while :; do
  within_limit "the start card"
  card="$("$PF_BIN" session items S-1 --json | jq -c '[.. | objects | select(.body? and .body.kind? == "start_card")][0] // empty')"
  [ -z "$card" ] || break
  sleep 0.1
done
proposal="$(printf '%s' "$card" | jq -r '.body.proposal')"
plan="$(printf '%s' "$card" | jq -r '.body.plan_sha256')"
issue "$(jq -nc --arg p "$proposal" --arg h "$plan" '{kind: "arm", proposal: $p, plan_sha256: $h}')" demo-arm-code
verb arm "$(jq -nc --arg p "$proposal" --arg h "$plan" --arg k "$code" '{proposal: $p, plan_sha256: $h, code: $k, command_id: "demo-arm-1"}')"
check "$status" 200 "step 2: the arm ($reply)"
run="$(printf '%s' "$reply" | jq -r '.run_id')"
[ -n "$run" ] && [ "$run" != null ] || fail "step 2: the arm named no run: $reply"
check "$("$PF_BIN" status --json | jq -r '.project_id')" "$project" "step 2: the project of the run"
check "$(cat "$chat_dir/launches")" 1 "step 2: the launches of the chat wrapper"
echo "step 2: S-1 proposed the start, D-1 confirmed the card with a fresh code, run $run armed with project $project"

# ----------------------------------------------------------------------------------- 3
# A project holds one live Orchestrator session, so the first one ends and a second starts. The second
# is the newest, so it takes the wake (a wake goes to the newest live session). Its wrapper is launch 2:
# it replays a plain turn, which the wake's turn uses, and settles from below the chat.
verb session_end "$(jq -nc '{session_id: "S-1", command_id: "demo-end-1"}')"
check "$status" 200 "step 3: session_end ($reply)"
verb session_start "$(jq -nc --arg p "$project" '{project_id: $p, command_id: "demo-start-2"}')"
check "$status" 200 "step 3: the second session_start ($reply)"
check "$(printf '%s' "$reply" | jq -r '.session.id')" S-2 "step 3: the second session"
wait_file "$chat_dir/launches" "step 3: the launch counter"
check "$(cat "$chat_dir/launches")" 2 "step 3: the launches of the chat wrapper"
echo "step 3: S-1 ended and S-2 started (launch 2 of the chat wrapper): the newest live session takes the wake"

# ----------------------------------------------------------------------------------- 4
ask="$(wait_request item-2 '.status == "open"' "step 4: the question's request")"
ask_id="$(printf '%s' "$ask" | jq -r '.id')"
printf '%s' "$ask" | jq -e '.cause == "stop_file" and .line == "above"' >/dev/null \
  || fail "step 4: the question is not a stop file above the line: $ask"
# The stall is two stalls of one minute each. An item that goes on to its verify stage with no stall
# request never stalled: fail at once, naming it.
stall=""
while :; do
  stall="$(request_of item-1)"
  if [ -n "$stall" ] && printf '%s' "$stall" | jq -e '.status == "open" and .cause == "stalled"' >/dev/null 2>&1; then break; fi
  if grep -qF "launch of stage verify for unit item-1 admitted" "$run_ledger" 2>/dev/null; then
    fail "step 4: the stall never parked: item-1 went on to verify with no request below the line"
  fi
  within_limit "step 4: the stall's request (two stalls of one minute each)"
  sleep 0.2
done
stall_id="$(printf '%s' "$stall" | jq -r '.id')"
stall_rev="$(printf '%s' "$stall" | jq -r '.revision')"
printf '%s' "$stall" | jq -e '.line == "below"' >/dev/null || fail "step 4: the stall is not below the line: $stall"
# The wake: one item in S-2, one `Notice` send, one turn with the cause `notice`.
blocks=""
while :; do
  blocks="$(wake_blocks S-2)"
  [ -z "$blocks" ] || break
  within_limit "the wake item of S-2"
  sleep 0.1
done
check "$(printf '%s\n' "$blocks" | wc -l | tr -d ' ')" 1 "step 4: the wake items of S-2"
block="$(printf '%s' "$blocks" | jq -r '.')"
request_lines="$(printf '%s\n' "$block" | grep '^request=' || true)"
check "$(printf '%s\n' "$request_lines" | wc -l | tr -d ' ')" 1 "step 4: the request lines of the block"
case "$request_lines" in
  "request=$stall_id revision=$stall_rev cause=stalled line=below run=$run unit=item-1 stage=implement acts=clear,redispatch") ;;
  *) fail "step 4: the block's request line is not the stall's: $request_lines" ;;
esac
case "$block" in *"$ask_id"*) fail "step 4: the block names the question $ask_id" ;; esac
wait_line "$chat_ledger" "turn session=S-2 cause=notice" "step 4: the woken turn"
check "$(grep -cF " wake S-2 project=$project requests=$stall_id@$stall_rev " "$controller_ledger" || true)" 1 "step 4: the wake lines of controller.ledger"
# The question reaches the chat as its own `request_opened` item, and no other item names it.
items="$("$PF_BIN" session items S-2 --json)"
check "$(printf '%s' "$items" | jq -r --arg r "request $ask_id," '[.. | objects | select(.body? and .body.kind? == "notice" and .body.code? == "request_opened" and (.body.text | contains($r)))] | length')" 1 \
  "step 4: the request_opened items of the question"
check "$(printf '%s' "$items" | jq -r --arg r "$ask_id" '[.. | objects | select(.body? and .body.kind? == "notice" and .body.code? == "wake" and (.body.text | contains($r)))] | length')" 0 \
  "step 4: the wake items that name the question"
echo "step 4: the question $ask_id parked above the line; after two stalls $stall_id opened below it, and one wake of S-2 listed it alone with acts=clear,redispatch (one Notice send, one turn with the cause notice)"

# ----------------------------------------------------------------------------------- 5
wait_file "$chat_dir/settle-$stall_id.rc" "step 5: the settle of the stall"
check "$(cat "$chat_dir/settle-$stall_id.rc")" 0 "step 5: the exit code of the stall's settle ($(cat "$chat_dir/settle-$stall_id.err" 2>/dev/null))"
wait_line "$run_ledger" "settle $stall_id@$stall_rev by grant $project#1 session S-2: act=clear reason=" "step 5: the settle in the run ledger"
settled="$(wait_request item-1 '.id == "'"$stall_id"'" and .status == "answered"' "step 5: the stall answered")"
printf '%s' "$settled" | jq -e --arg p "$project" '.settled.session == "S-2" and .settled.generation == 1 and .settled.act == "clear"
  and .settled.project == $p and (.settled.reason | length > 0)' >/dev/null \
  || fail "step 5: the stall's record of the settle is wrong: $settled"
wait_line "$controller_ledger" "woken $stall_id@$stall_rev session=S-2" "step 5: the woken record"
# The unit launches again (launch 3 of the implementer) and reaches its PR on the paper forge.
wait_line "$run_ledger" "launch of stage verify for unit item-1 admitted" "step 5: the item runs verify after the settle"
wait_file "$start/paper/prs/1.json" "step 5: the item's PR on the paper forge"
echo "step 5: the settle of $stall_id is admitted (act clear, grant $project#1, session S-2); the run ledger and requests say so, the stall is marked woken, and item-1 launched again and reached its PR"

# ----------------------------------------------------------------------------------- 6
wait_file "$chat_dir/settle-$ask_id.rc" "step 6: the settle of the question"
[ "$(cat "$chat_dir/settle-$ask_id.rc")" != 0 ] || fail "step 6: the settle of the question was admitted"
grep -qF "above the line" "$chat_dir/settle-$ask_id.err" \
  || fail "step 6: the settle of the question was not refused for its line: $(cat "$chat_dir/settle-$ask_id.err")"
open="$(request_of item-2)"
printf '%s' "$open" | jq -e --arg id "$ask_id" '.id == $id and .status == "open" and ([.verbs[]] | (index("answer") != null and index("clear") != null))
  and (has("settled") | not)' >/dev/null \
  || fail "step 6: the question is not open with the verbs answer and clear: $open"
echo "step 6: the settle of the question $ask_id was refused (above the line), and it stays open with its verbs answer and clear for the claim"

# ----------------------------------------------------------------------------------- 7
"$PF_BIN" grant revoke --project "$project" >/dev/null || fail "step 7: the grant was not revoked"
: > "$chat_dir/after-revoke"
wait_file "$chat_dir/settle-again.rc" "step 7: the second settle of the stall"
[ "$(cat "$chat_dir/settle-again.rc")" != 0 ] || fail "step 7: the settle after the revoke was admitted"
grep -qF "holds no live clear grant" "$chat_dir/settle-again.err" \
  || fail "step 7: the settle after the revoke was not refused for the grant: $(cat "$chat_dir/settle-again.err")"
echo "step 7: after grant revoke the second settle of $stall_id was refused (the project holds no live clear grant)"

# ----------------------------------------------------------------------------------- 8
secrets=("$token1")
# The claim token exists only when a CLI claim made one; none was made here.
[ ! -f "$PF_STATE_DIR/claim-token" ] || secrets+=("$(cat "$PF_STATE_DIR/claim-token")")
while IFS= read -r c; do secrets+=("$c"); done < "$confirm_codes"
while IFS= read -r c; do secrets+=("$c"); done < "$pairing_codes"
for secret in "${secrets[@]}"; do
  for file in "$api_ledger" "$controller_ledger" "$run_ledger" "$chat_ledger" "$stderr_file"; do
    [ -f "$file" ] || continue
    if grep -qF -- "$secret" "$file"; then fail "step 8: a confirm code, a pairing code, the claim token or the device token is in $(basename "$file")"; fi
  done
done
echo "step 8: no confirm code, pairing code, claim token or device token is in any ledger or in the controller's stderr"

echo "orchestrator-settles: ok ($(( $(date +%s) - started )) s)"
