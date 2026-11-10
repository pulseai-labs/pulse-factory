#!/usr/bin/env bash
# Start a spine from the Orchestrator chat and arm it from a paired device (d22): the chat proposes, a
# start card lands in the chat, and a paired device confirms it with a one-use code and arms the run,
# over the API with a pinned `curl` and `pf-stream-reader`. It starts a controller with
# `--api-listen 127.0.0.1:0` on a temp state directory. The chat is a wrapper of `pf-claude-replay`
# (`start-env.sh`): a process below the chat runs `propose`, and an `arm` that the controller refuses.
#
# It does not build: `PF_BIN`, `PF_REPLAY_BIN` and `PF_STREAM_BIN` are built binaries (defaults
# `pulse-factory`, `pf-claude-replay` and `pf-stream-reader` on PATH). It needs `curl`, `jq`, `git`,
# `sha256sum` and coreutils. It spends nothing, starts no real `claude`, calls no `gh` and reaches
# nothing beyond 127.0.0.1 and the bare repo of the paper forge. Everything lives in one temp directory
# (`HOME`, `PF_STATE_DIR`, `PF_SHARE_DIR`), which the script removes on exit after it stops the
# controller.
#
#   PF_BIN=target/debug/pulse-factory PF_REPLAY_BIN=target/debug/pf-claude-replay \
#     PF_STREAM_BIN=target/debug/pf-stream-reader scripts/demo/start-api.sh
#
# The steps (each prints `step <n>: ...`):
#   1. start the controller, pair D-1 and D-2, add a project;
#   2. start the Orchestrator session over the API, follow it as D-1 and send one message;
#   3. the stream shows the chat's Bash call and a start card; `arm` from below the chat is refused and no
#      run exists; `propose` from the script itself is refused;
#   4. a wrong plan hash, a foreign code and an expired code are each refused with a word;
#   5. a fresh code arms the run; the card gets its result; a replay changes nothing;
#   6. `halt` halts the run;
#   7. the ledgers hold each act and refusal, and no secret is in any of them.
#
# It exits non-zero on any failed assertion and prints `start-api: ok` at the end.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PF_BIN="${PF_BIN:-pulse-factory}"
PF_REPLAY_BIN="${PF_REPLAY_BIN:-pf-claude-replay}"
PF_STREAM_BIN="${PF_STREAM_BIN:-pf-stream-reader}"
limit_seconds=150

fail() { echo "start-api: FAIL: $*" >&2; exit 1; }
for tool in curl jq git sha256sum base32 base64 od tail head tr grep sed cut sleep; do
  command -v "$tool" >/dev/null || fail "$tool is not on PATH (this script needs it)"
done
for bin in "$PF_BIN" "$PF_REPLAY_BIN" "$PF_STREAM_BIN"; do
  command -v "$bin" >/dev/null || [ -x "$bin" ] || fail "$bin is not an executable"
done
[ -x "$here/start-env.sh" ] || fail "no start-env.sh next to this script"
# The wrapper and the readers run from other directories, so the binaries need absolute paths.
abs() {
  local p
  p="$(command -v "$1")"
  case "$p" in /*) printf '%s' "$p" ;; *) printf '%s' "$PWD/$p" ;; esac
}
PF_BIN="$(abs "$PF_BIN")"
PF_REPLAY_BIN="$(abs "$PF_REPLAY_BIN")"
PF_STREAM_BIN="$(abs "$PF_STREAM_BIN")"

tmp="$(mktemp -d)"
pid=""
readers=()
stop_controller() {
  [ -n "$pid" ] || return 0
  kill "$pid" 2>/dev/null || true
  for _ in $(seq 1 100); do kill -0 "$pid" 2>/dev/null || break; sleep 0.1; done
  kill -9 "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  pid=""
}
cleanup() {
  for r in ${readers[@]+"${readers[@]}"}; do kill "$r" 2>/dev/null || true; done
  stop_controller
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

# The paper env and the chat's wrapper (K3).
"$here/start-env.sh" "$tmp/start" > "$tmp/start-env.out" 2>&1 || fail "start-env.sh failed: $(cat "$tmp/start-env.out")"
start="$tmp/start"
chat_dir="$start/chat"
run_ledger="$start/ledger.md"

# ------------------------------------------------------------------------- the API, pinned
addr="" spki="" pin="" status="" reply=""
start_controller() {
  # `PF_CONFIRM_TTL=3` shortens a confirm code to 3 seconds, so step 4 can wait one out.
  PF_CONFIRM_TTL=3 PF_CHAT_CLAUDE_BIN="$start/claude-wrapper" PF_SERVER_NAME=demo-server \
    "$PF_BIN" controller --tick-ms 200 --api-listen 127.0.0.1:0 >/dev/null 2>>"$stderr_file" &
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

# One call, pinned to the server key as a client must:
#   api_call <method> <path> <token or -> <body or ->
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

# A verb call as one device: `verb_as <device token> <verb> <body>`.
verb_as() { api_call POST "/v1/verbs/$2" "$1" "$3"; }

pairing_codes="$tmp/pairing-codes.txt"
: > "$pairing_codes"
# Pair one device with a fresh code: `pair <name> <token file>`; it sets `paired_id`.
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

# Start a stream reader in the background: `reader <name> <query> <token file> <max seconds>`.
reader() {
  "$PF_STREAM_BIN" --url "wss://$addr/v1/stream$2" --pin "$spki" --token-file "$3" --max-seconds "$4" \
    > "$tmp/$1.out" 2> "$tmp/$1.err" &
  readers+=("$!")
}

# Wait, with a bound, until a stream output file holds a frame that `jq` accepts.
#   wait_frame <file> <jq filter over one frame> <what>
wait_frame() {
  local file="$1" filter="$2" what="$3"
  for _ in $(seq 1 400); do
    if jq -Rn -e "[inputs | fromjson?] | any(.[]; $filter)" "$file" >/dev/null 2>&1; then
      return 0
    fi
    within_limit "$what"
    sleep 0.1
  done
  fail "$what: no such frame in $(basename "$file"): $(head -c 2000 "$file" 2>/dev/null)"
}

frames() { jq -Rc 'fromjson? | select(.)' "$1"; }

confirm_codes="$tmp/confirm-codes.txt"
: > "$confirm_codes"
# `issue <token> <proposal> <plan sha256> <command id>` asks for an arm code and sets `code`.
code=""
issue() {
  verb_as "$1" confirm_code "$(jq -nc --arg p "$2" --arg h "$3" --arg c "$4" \
    '{subject: {kind: "arm", proposal: $p, plan_sha256: $h}, command_id: $c}')"
}
issue_ok() {
  issue "$@"
  check "$status" 200 "confirm_code for $2 ($reply)"
  code="$(printf '%s' "$reply" | jq -r '.code')"
  printf '%s' "$code" | grep -qE '^pfc1-[0-9a-f]{32}$' || fail "the confirm code has the wrong shape"
  printf '%s\n' "$code" >> "$confirm_codes"
}

# `arm_as <token> <proposal> <plan sha256> <code> <command id>`.
arm_as() {
  verb_as "$1" arm "$(jq -nc --arg p "$2" --arg h "$3" --arg k "$4" --arg c "$5" \
    '{proposal: $p, plan_sha256: $h, code: $k, command_id: $c}')"
}

refusal() { printf '%s' "$reply" | jq -r '.refusal // empty'; }

# --------------------------------------------------------------------------------- 1
start_controller
pair demo-macbook "$tmp/token-1"
check "$paired_id" D-1 "step 1: the first device"
pair demo-ipad "$tmp/token-2"
check "$paired_id" D-2 "step 1: the second device"
token1="$(cat "$tmp/token-1")"
token2="$(cat "$tmp/token-2")"
mkdir -p "$tmp/work/alpha"
git -C "$tmp/work/alpha" init -q -b main
git -C "$tmp/work/alpha" -c user.name=demo -c user.email=demo@example.invalid commit -q --allow-empty -m demo
verb_as "$token1" project_add "$(jq -nc --arg root "$tmp/work/alpha" '{root: $root, name: "alpha", command_id: "demo-add-alpha"}')"
check "$status" 200 "step 1: project_add ($reply)"
project="$(printf '%s' "$reply" | jq -r '.project.id')"
echo "step 1: controller up on $addr, D-1 and D-2 paired, project $project added, confirm codes last 3 s"

# --------------------------------------------------------------------------------- 2
verb_as "$token1" session_start "$(jq -nc --arg p "$project" '{project_id: $p, command_id: "demo-start-1"}')"
check "$status" 200 "step 2: session_start ($reply)"
check "$(printf '%s' "$reply" | jq -r '.session.id')" S-1 "step 2: the session"
reader s1 "?session=S-1&after=0" "$tmp/token-1" 120
wait_frame "$tmp/s1.out" '.type == "caught"' "step 2: caught"
verb_as "$token1" session_send "$(jq -nc '{session_id: "S-1", text: "start the spine s1 of this project", command_id: "demo-send-1"}')"
check "$status" 200 "step 2: session_send ($reply)"
echo "step 2: S-1 started over the API, followed as D-1, one message sent"

# --------------------------------------------------------------------------------- 3
wait_frame "$tmp/s1.out" '.type == "item" and .item.body.kind == "tool_call" and .item.body.tool == "Bash" and ((.item.body.input.text // "") | contains("pulse-factory propose"))' \
  "step 3: the chat's Bash call"
wait_frame "$tmp/s1.out" '.type == "item" and .item.body.kind == "start_card"' "step 3: the start card"
card="$(frames "$tmp/s1.out" | jq -c 'select(.type == "item" and .item.body.kind == "start_card") | .item' | head -n 1)"
card_seq="$(printf '%s' "$card" | jq -r '.seq')"
proposal="$(printf '%s' "$card" | jq -r '.body.proposal')"
plan="$(printf '%s' "$card" | jq -r '.body.plan_sha256')"
printf '%s' "$proposal" | grep -qE '^sp-[0-9a-f]{16}$' || fail "step 3: the proposal ID '$proposal' has the wrong shape"
printf '%s' "$plan" | grep -qE '^[0-9a-f]{64}$' || fail "step 3: the plan hash '$plan' has the wrong shape"
printf '%s' "$card" | jq -e '.body.summary
  | .spine == "s1" and .items == 1
    and (.roles | length > 0) and all(.roles[]; (.name | length > 0) and (.model | length > 0))
    and (.caps.model_sessions | type == "number") and (.most_launches | type == "number")
    and (.merges | length == 2) and all(.merges[]; .sentence | length > 0)' >/dev/null \
  || fail "step 3: the card's summary is not the one of the paper spine: $(printf '%s' "$card" | jq -c '.body.summary')"
for _ in $(seq 1 600); do [ -e "$chat_dir/arm.rc" ] && break; within_limit "the process under the chat"; sleep 0.1; done
[ -e "$chat_dir/arm.rc" ] || fail "step 3: the process under the chat did not finish its two verbs"
check "$(cat "$chat_dir/propose.rc")" 0 "step 3: the exit of propose from below the chat ($(cat "$chat_dir/propose.err"))"
check "$(cat "$chat_dir/arm.rc")" 1 "step 3: the exit of arm from below the chat"
grep -qE 'chat process|PULSE_FACTORY_CHAT' "$chat_dir/arm.err" \
  || fail "step 3: the refusal of arm does not name the chat: $(cat "$chat_dir/arm.err")"
if "$PF_BIN" status >/dev/null 2>&1; then fail "step 3: a run exists after the refused arm"; fi
call_args=(s1 --workflow paper-spine-d20 --ledger "$run_ledger" --paper "$start/paper"
  --param spine_branch=spine-s1 --param "spine_dir=$start/spine" --param "worktree_root=$start/worktrees"
  --param base_branch=main)
rc=0
"$PF_BIN" propose "${call_args[@]}" > /dev/null 2> "$tmp/script-propose.err" || rc=$?
check "$rc" 1 "step 3: propose from the script itself"
grep -q 'runs under no chat session' "$tmp/script-propose.err" \
  || fail "step 3: propose from the script was not refused for want of a chat: $(cat "$tmp/script-propose.err")"
echo "step 3: the stream shows the Bash call and the card $proposal; arm from below the chat is refused (exit 1), no run exists, propose from the script is refused"

# --------------------------------------------------------------------------------- 4
issue "$token1" "$proposal" "$(printf '%064d' 0)" demo-code-wrong
check "$status" 422 "step 4: a wrong plan hash ($reply)"
check "$(refusal)" subject_moved "step 4: the word of the wrong plan hash"
issue_ok "$token1" "$proposal" "$plan" demo-code-1
printf '%s' "$reply" | jq -e --arg p "$proposal" '.generation == null and .start.proposal == $p' >/dev/null \
  || fail "step 4: the code's answer does not bind the card: $(printf '%s' "$reply" | jq -c 'del(.code)')"
foreign_code="$code"
arm_as "$token2" "$proposal" "$plan" "$foreign_code" demo-arm-foreign
check "$status" 422 "step 4: the foreign code ($reply)"
check "$(refusal)" code_foreign "step 4: the word of the foreign code"
issue_ok "$token1" "$proposal" "$plan" demo-code-2
expired_code="$code"
sleep 4
arm_as "$token1" "$proposal" "$plan" "$expired_code" demo-arm-expired
check "$status" 422 "step 4: the expired code ($reply)"
check "$(refusal)" code_expired "step 4: the word of the expired code"
if "$PF_BIN" status >/dev/null 2>&1; then fail "step 4: a run exists after three refusals"; fi
echo "step 4: a wrong plan hash is subject_moved, D-2 with a code of D-1 is code_foreign, a code left for 4 s is code_expired; no run exists"

# --------------------------------------------------------------------------------- 5
issue_ok "$token1" "$proposal" "$plan" demo-code-3
arm_code="$code"
arm_as "$token1" "$proposal" "$plan" "$arm_code" demo-arm-1
check "$status" 200 "step 5: the arm ($reply)"
run="$(printf '%s' "$reply" | jq -r '.run_id')"
[ -n "$run" ] && [ "$run" != null ] || fail "step 5: the arm named no run: $reply"
check "$(printf '%s' "$reply" | jq -r '.device')" D-1 "step 5: the device of the arm"
status_json="$("$PF_BIN" status --json)" || fail "step 5: status exits non-zero after the arm"
check "$(printf '%s' "$status_json" | jq -r '.run')" "$run" "step 5: the run in status"
check "$(printf '%s' "$status_json" | jq -r '.project_id')" "$project" "step 5: the project of the run"
check "$(printf '%s' "$status_json" | jq -r '.env')" paper "step 5: the env of the run"
wait_frame "$tmp/s1.out" '.type == "item" and .item.body.kind == "start_result" and .item.body.result == "armed" and .item.body.outcome == "answered" and .item.body.run == "'"$run"'" and .item.body.device == "D-1" and .item.answers_seq == '"$card_seq" \
  "step 5: the card's result"
arm_as "$token1" "$proposal" "$plan" "$arm_code" demo-arm-1
check "$status" 200 "step 5: the replay of the arm ($reply)"
check "$(printf '%s' "$reply" | jq -r '.replayed')" true "step 5: the replay flag"
check "$(printf '%s' "$reply" | jq -r '.run_id')" "$run" "step 5: the run of the replay"
check "$("$PF_BIN" status --json | jq -r '.run')" "$run" "step 5: the run in status after the replay"
check "$(grep -c ' run_project ' "$controller_ledger" || true)" 1 "step 5: the run_project lines after the replay"
check "$(grep -cE ' device_arm D-1 proposal=.* admitted' "$controller_ledger" || true)" 1 "step 5: the admitted device_arm lines after the replay"
issue "$token1" "$proposal" "$plan" demo-code-4
check "$status" 422 "step 5: a new code for an armed proposal ($reply)"
check "$(refusal)" subject_moved "step 5: the word of the new code"
echo "step 5: the arm from D-1 made run $run in project $project on the paper env; the card shows it; a replay made no second run"

# --------------------------------------------------------------------------------- 6
halt_json="$("$PF_BIN" halt --reason "d22 demo" --json)" || fail "step 6: halt failed"
check "$(printf '%s' "$halt_json" | jq -r '.halted')" true "step 6: halted"
echo "step 6: halt halted run $run"

# --------------------------------------------------------------------------------- 7
for want in " propose S-1 proposal=$proposal " " confirm_code D-1 proposal=$proposal verb=arm" \
  " device_arm D-1 proposal=$proposal " " run_project $run project=$project"; do
  grep -qF -- "$want" "$controller_ledger" || fail "step 7: controller.ledger has no line with '$want'"
done
grep -E "device_arm D-1 proposal=$proposal .*admitted" "$controller_ledger" >/dev/null \
  || fail "step 7: controller.ledger has no admitted device_arm line"
grep -E 'verb=arm device=D-1 .*status=200 outcome=ok' "$api_ledger" >/dev/null \
  || fail "step 7: api.ledger has no ok arm line of D-1: $(cat "$api_ledger")"
for word in subject_moved code_foreign code_expired; do
  grep -E "outcome=refused refusal=$word\$" "$api_ledger" >/dev/null \
    || fail "step 7: api.ledger has no refused line with $word: $(cat "$api_ledger")"
done
secrets=("$token1" "$token2")
while IFS= read -r c; do secrets+=("$c"); done < "$pairing_codes"
while IFS= read -r c; do secrets+=("$c"); done < "$confirm_codes"
for secret in "${secrets[@]}"; do
  for file in "$api_ledger" "$controller_ledger" "$run_ledger" "$chat_ledger" "$stderr_file"; do
    [ -f "$file" ] || continue
    if grep -qF -- "$secret" "$file"; then fail "step 7: a confirm code, a pairing code or a device token is in $(basename "$file")"; fi
  done
done
echo "step 7: the ledgers hold the propose, the confirm, the arm and the run's project; no code or token is in any of them"

echo "start-api: ok ($(( $(date +%s) - started )) s)"
