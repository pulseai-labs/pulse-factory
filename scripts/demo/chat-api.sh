#!/usr/bin/env bash
# Chat with a project's Orchestrator over the API (d18): the real Claude Code adapter against a replay
# of a recorded session, a paired device, the chat verbs over pinned TLS, and the chat stream read by
# `pf-stream-reader`. It starts a controller with `--api-listen 127.0.0.1:0` on a temp state directory
# and `PF_CHAT_CLAUDE_BIN` set to a wrapper of `pf-claude-replay`, so the chat runs an unqualified
# binary that plays `permissions.jsonl`: a `Bash` approval, a deny, a reply and a cost.
#
# It does not build: `PF_BIN`, `PF_REPLAY_BIN` and `PF_STREAM_BIN` are built binaries (defaults
# `pulse-factory`, `pf-claude-replay` and `pf-stream-reader` on PATH). It needs `curl`, `jq`, `git`
# and coreutils. It spends nothing, starts no real `claude` and reaches no address beyond 127.0.0.1.
#
#   PF_BIN=target/debug/pulse-factory PF_REPLAY_BIN=target/debug/pf-claude-replay \
#     PF_STREAM_BIN=target/debug/pf-stream-reader scripts/demo/chat-api.sh
set -euo pipefail

PF_BIN="${PF_BIN:-pulse-factory}"
PF_REPLAY_BIN="${PF_REPLAY_BIN:-pf-claude-replay}"
PF_STREAM_BIN="${PF_STREAM_BIN:-pf-stream-reader}"

fail() { echo "chat-api: FAIL: $*" >&2; exit 1; }
for tool in curl jq git base32 base64 od tail head tr grep sed; do
  command -v "$tool" >/dev/null || fail "$tool is not on PATH (this script needs it)"
done
for bin in "$PF_BIN" "$PF_REPLAY_BIN" "$PF_STREAM_BIN"; do
  command -v "$bin" >/dev/null || [ -x "$bin" ] || fail "$bin is not an executable"
done
# The wrapper runs from another directory, so the replay needs an absolute path.
PF_REPLAY_BIN="$(command -v "$PF_REPLAY_BIN")"
case "$PF_REPLAY_BIN" in /*) ;; *) PF_REPLAY_BIN="$PWD/$PF_REPLAY_BIN" ;; esac

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fixture="$repo_root/crates/pf-adapter-claude/fixtures/permissions.jsonl"
[ -f "$fixture" ] || fail "no fixture at $fixture"

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
  for r in "${readers[@]:-}"; do [ -z "$r" ] || kill "$r" 2>/dev/null || true; done
  stop_controller
  rm -rf "$tmp"
}
trap cleanup EXIT

unset PF_SEAT_TOKEN
export HOME="$tmp/home" PF_STATE_DIR="$tmp/state" PF_SHARE_DIR="$tmp/share"
mkdir -p "$HOME" "$PF_SHARE_DIR"
stderr_file="$tmp/controller.stderr"
api_ledger="$PF_STATE_DIR/api.ledger"
controller_ledger="$PF_STATE_DIR/controller.ledger"
chat_ledger="$PF_STATE_DIR/chat.ledger"

check() { [ "$1" = "$2" ] || fail "$3: wanted '$2', got '$1'"; }

# The two-line wrapper of the replay (docs/contracts/chat-adapter.md): the adapter's environment
# allowlist passes no Pulse Factory variable to the child, so the fixture is named here.
wrapper="$tmp/claude-wrapper"
printf '#!/bin/sh\nexec "%s" --fixture "%s" "$@"\n' "$PF_REPLAY_BIN" "$fixture" > "$wrapper"
chmod +x "$wrapper"

# A repo with one commit.
make_repo() {
  mkdir -p "$1"
  git -C "$1" init -q -b main
  git -C "$1" -c user.name=demo -c user.email=demo@example.invalid commit -q --allow-empty -m demo
}

# Start the controller on a free loopback port, wait until it listens, and set `addr` and `spki`.
start_controller() {
  PF_CHAT_CLAUDE_BIN="$wrapper" PF_SERVER_NAME=demo-server \
    "$PF_BIN" controller --tick-ms 3600000 --api-listen 127.0.0.1:0 >/dev/null 2>>"$stderr_file" &
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
  local text="${1#pf1-}" pad
  text="$(printf '%s' "$text" | tr 'a-z' 'A-Z')"
  pad=$(( (8 - ${#text} % 8) % 8 ))
  while [ "$pad" -gt 0 ]; do text="$text="; pad=$((pad - 1)); done
  printf '%s' "$text" | base32 -d > "$2"
}

# One call, pinned to the server key as a client must:
#   api_call <method> <path> <token or -> <body or ->
# It sets `status`, `reply` and the headers file. A verb call counts in `verb_calls`.
status="" reply="" verb_calls=0 pin=""
api_call() {
  local method="$1" path="$2" token="$3" body="$4"
  local args=(-sS -k --max-time 20 --pinnedpubkey "sha256//$pin" -X "$method" -w '\n%{http_code}' -D "$tmp/headers")
  [ "$token" = - ] || args+=(-H "Authorization: Bearer $token")
  [ "$body" = - ] || args+=(-H 'Content-Type: application/json' --data-binary "$body")
  case "$path" in /v1/verbs/*) verb_calls=$((verb_calls + 1)) ;; esac
  local out rc=0
  out="$(curl "${args[@]}" "https://$addr$path" 2>"$tmp/curl.err")" || rc=$?
  [ "$rc" -eq 0 ] || fail "curl exited $rc for $method $path: $(cat "$tmp/curl.err")"
  status="${out##*$'\n'}"
  reply="${out%$'\n'*}"
}

header() { tr -d '\r' < "$tmp/headers" | grep -i "^$1:" | head -n1 | sed 's/^[^:]*: *//'; }

verb() { api_call POST "/v1/verbs/$1" "$token" "$2"; }

# Wait, with a bound, until a stream output file holds a frame that `jq` accepts.
#   wait_frame <file> <jq filter over one frame> <what>
wait_frame() {
  local file="$1" filter="$2" what="$3"
  for _ in $(seq 1 300); do
    if jq -Rn -e "[inputs | fromjson?] | any(.[]; $filter)" "$file" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.1
  done
  fail "$what: no such frame in $(basename "$file"): $(cat "$file" 2>/dev/null | head -c 2000)"
}

frames() { jq -Rc 'fromjson? | select(.)' "$1"; }

# Start a reader in the background: `reader <name> <query> <token file> <max seconds>`.
reader() {
  "$PF_STREAM_BIN" --url "wss://$addr/v1/stream$2" --pin "$spki" --token-file "$3" --max-seconds "$4" \
    > "$tmp/$1.out" 2> "$tmp/$1.err" &
  readers+=("$!")
  last_reader=$!
}

# --------------------------------------------------------------------------------- 1
start_controller
echo "controller up on $addr"
grep -q 'unqualified Claude Code binary' "$stderr_file" || fail "step 1: the controller did not name the override as unqualified"

code="$("$PF_BIN" device pair --json | jq -r '.code')"
decode_code "$code" "$tmp/code.bin"
check "$(od -An -tx1 -v -j11 -N32 "$tmp/code.bin" | tr -d ' \n')" "$spki" "step 1: the SPKI hash in the code"
pin="$(tail -c +12 "$tmp/code.bin" | head -c 32 | base64 -w0)"
body="$(jq -nc --arg code "$code" '{code: $code, device_name: "demo-macbook"}')"
api_call POST /v1/pair - "$body"
check "$status" 200 "step 1: the pairing ($reply)"
token="$(printf '%s' "$reply" | jq -r '.token')"
printf '%s' "$token" | grep -qE '^[0-9a-f]{64}$' || fail "step 1: the token is not 64 hex digits"
printf '%s' "$token" > "$tmp/token"

make_repo "$tmp/work/alpha"
verb project_add "$(jq -nc --arg root "$tmp/work/alpha" '{root: $root, name: "alpha", command_id: "demo-add-alpha"}')"
check "$status" 200 "step 1: project_add over the API ($reply)"
project="$(printf '%s' "$reply" | jq -r '.project.id')"
echo "step 1: controller started, D-1 paired, project $project (alpha) added over the API"

# --------------------------------------------------------------------------------- 2
verb session_start "$(jq -nc --arg p "$project" '{project_id: $p, command_id: "demo-start-1"}')"
check "$status" 200 "step 2: session_start over the API ($reply)"
check "$(printf '%s' "$reply" | jq -r '.session.id')" S-1 "step 2: the session"
check "$(printf '%s' "$reply" | jq -r '.session.worktree_id')" W-1 "step 2: the worktree"
check "$(printf '%s' "$reply" | jq -r '.worktree.path')" "$tmp/work/alpha" "step 2: the session's root"
check "$(header pf-server-id)" "$spki" "step 2: pf-server-id on the answer"
check "$(header pf-server-name)" demo-server "step 2: pf-server-name on the answer"
echo "step 2: S-1 started in W-1 at alpha's root; the answer names the server"

# --------------------------------------------------------------------------------- 3
reader s1 "?session=S-1&after=0" "$tmp/token" 90
s1_pid=$last_reader
wait_frame "$tmp/s1.out" '.type == "caught"' "step 3: caught"
wait_frame "$tmp/s1.out" '.type == "hello" and .server.id == "'"$spki"'" and .server.name == "demo-server"' "step 3: hello with the server"
wait_frame "$tmp/s1.out" '.type == "hello" and (.sessions | any(.id == "S-1" and .process.qualified == false))' "step 3: S-1 unqualified"
echo "step 3: the stream gave hello (the server, S-1 unqualified) and caught"

# --------------------------------------------------------------------------------- 4
verb session_set '{"session_id":"S-1","permission_mode":"manual","command_id":"demo-set-1"}'
check "$status" 200 "step 4: session_set ($reply)"
question="what does this project hold, apart from this exact demo text 7f3c91"
verb session_send "$(jq -nc --arg t "$question" '{session_id: "S-1", text: $t, command_id: "demo-send-1"}')"
check "$status" 200 "step 4: session_send ($reply)"
echo "step 4: the mode is manual and a question was sent"

# --------------------------------------------------------------------------------- 5
wait_frame "$tmp/s1.out" '.type == "item" and .item.body.kind == "approval_request" and .item.body.tool == "Bash"' "step 5: the approval request"
approval_seq="$(frames "$tmp/s1.out" | jq -r 'select(.type == "item" and .item.body.kind == "approval_request") | .item.seq' | head -n1)"
verb session_answer "$(jq -nc --argjson s "$approval_seq" '{session_id: "S-1", seq: $s, answer: {type: "deny", value: {message: "no"}}, command_id: "demo-answer-1"}')"
check "$status" 200 "step 5: session_answer ($reply)"
wait_frame "$tmp/s1.out" '.type == "delta"' "step 5: a delta"
wait_frame "$tmp/s1.out" '.type == "item" and .item.body.kind == "assistant_text"' "step 5: the reply"
wait_frame "$tmp/s1.out" '.type == "item" and .item.body.kind == "turn_result" and .item.body.usage.cost_micros > 0' "step 5: the turn result"
cost="$(frames "$tmp/s1.out" | jq -r 'select(.type == "item" and .item.body.kind == "turn_result") | .item.body.usage.cost_micros' | head -n1)"
wait_frame "$tmp/s1.out" '.type == "session" and .session.usage.cost_micros == '"$cost" "step 5: a session frame with the cost"
echo "step 5: approval for Bash denied; deltas, the reply and a turn result with cost $cost"

# --------------------------------------------------------------------------------- 6
turns="$(grep -c ' chat turn ' "$chat_ledger" || true)"
check "$turns" 1 "step 6: the turn lines of chat.ledger"
grep ' chat turn ' "$chat_ledger" | grep -q "cost_micros=$cost " || fail "step 6: the turn line does not hold the cost $cost"
for kind in chat_session_start chat_session_set chat_send chat_answer; do
  check "$(grep -c " $kind " "$controller_ledger" || true)" 1 "step 6: the $kind lines of controller.ledger"
done
check "$(grep -c 'route=stream' "$api_ledger" || true)" 1 "step 6: the stream lines of api.ledger"
check "$(grep 'route=stream' "$api_ledger" | grep -c 'outcome=upgraded' || true)" 1 "step 6: the upgraded stream lines"
check "$(grep -c 'route=verb' "$api_ledger" || true)" "$verb_calls" "step 6: the verb lines of api.ledger"
echo "step 6: the ledgers hold the turn, the four commands, one stream and $verb_calls verb calls"

# --------------------------------------------------------------------------------- 7
reader s2 "?session=S-1&after=0" "$tmp/token" 2
wait "$last_reader" 2>/dev/null || true
grep '"type":"item"' "$tmp/s1.out" > "$tmp/s1.items" || true
grep '"type":"item"' "$tmp/s2.out" > "$tmp/s2.items" || true
cmp -s "$tmp/s1.items" "$tmp/s2.items" || fail "step 7: after=0 did not give the same item frames, byte for byte"
[ -s "$tmp/s2.items" ] || fail "step 7: after=0 gave no items"
last_item_line="$(grep -n '"type":"item"' "$tmp/s2.out" | tail -n1 | cut -d: -f1)"
caught_line="$(grep -n '"type":"caught"' "$tmp/s2.out" | head -n1 | cut -d: -f1)"
[ -n "$caught_line" ] && [ "$caught_line" -gt "$last_item_line" ] || fail "step 7: caught does not follow the items"
reader s3 "?session=S-1&after=$approval_seq" "$tmp/token" 2
wait "$last_reader" 2>/dev/null || true
want_later="$(jq -Rr --argjson s "$approval_seq" 'select((fromjson? // {}) | .type == "item" and .item.seq > $s)' "$tmp/s1.out")"
got_later="$(grep '"type":"item"' "$tmp/s3.out" || true)"
check "$got_later" "$want_later" "step 7: after=<the approval> gives only the later items"
echo "step 7: after=0 repeats the items byte for byte, after=$approval_seq gives only the later ones"

# --------------------------------------------------------------------------------- 8
verb session_start "$(jq -nc --arg p "$project" '{project_id: $p, command_id: "demo-start-2"}')"
check "$status" 422 "step 8: a second Orchestrator for alpha ($reply)"
printf '%s' "$reply" | jq -e '.error | contains("S-1")' >/dev/null || fail "step 8: the refusal does not name S-1: $reply"
make_repo "$tmp/work/beta"
verb project_add "$(jq -nc --arg root "$tmp/work/beta" '{root: $root, name: "beta", command_id: "demo-add-beta"}')"
check "$status" 200 "step 8: project_add beta ($reply)"
beta="$(printf '%s' "$reply" | jq -r '.project.id')"
rm -rf "$tmp/work/beta"
verb session_start "$(jq -nc --arg p "$beta" '{project_id: $p, command_id: "demo-start-3"}')"
check "$status" 422 "step 8: a session in a deleted root ($reply)"
printf '%s' "$reply" | jq -e '.error | contains("is gone")' >/dev/null || fail "step 8: the refusal does not say the root is gone: $reply"
: > "$tmp/no-token"
rc=0
"$PF_STREAM_BIN" --url "wss://$addr/v1/stream" --pin "$spki" --token-file "$tmp/no-token" --max-seconds 5 >/dev/null 2>"$tmp/r.err" || rc=$?
check "$rc" 1 "step 8: the exit of a reader with no token"
grep -q 'status 401' "$tmp/r.err" || fail "step 8: a reader with no token did not get 401: $(cat "$tmp/r.err")"
printf 'not-a-token' > "$tmp/bad-token"
rc=0
"$PF_STREAM_BIN" --url "wss://$addr/v1/stream" --pin "$spki" --token-file "$tmp/bad-token" --max-seconds 5 >/dev/null 2>"$tmp/r.err" || rc=$?
check "$rc" 1 "step 8: the exit of a reader with a bad token"
grep -q 'status 401' "$tmp/r.err" || fail "step 8: a reader with a bad token did not get 401: $(cat "$tmp/r.err")"
echo "step 8: a second Orchestrator and a deleted root are 422, a reader with no token or a bad one gets 401"

# --------------------------------------------------------------------------------- 9
reader s9 "?session=S-1" "$tmp/token" 60
s9_pid=$last_reader
wait_frame "$tmp/s9.out" '.type == "caught"' "step 9: the reader is open"
"$PF_BIN" device revoke D-1 >/dev/null || fail "step 9: device revoke D-1 failed"
for _ in $(seq 1 50); do kill -0 "$s9_pid" 2>/dev/null || break; sleep 0.1; done
if kill -0 "$s9_pid" 2>/dev/null; then fail "step 9: the reader is still open 5 seconds after the revoke"; fi
grep -q 'close 4001 revoked' "$tmp/s9.err" || fail "step 9: the reader was not closed with 4001 revoked: $(cat "$tmp/s9.err")"
verb session_send '{"session_id":"S-1","text":"after the revoke","command_id":"demo-send-2"}'
check "$status" 401 "step 9: a session_send with the revoked token"
echo "step 9: the revoke closed the open stream with 4001 revoked, and the token is refused"

# --------------------------------------------------------------------------------- 10
for secret in "$token" "$code" "$question"; do
  for file in "$api_ledger" "$controller_ledger" "$chat_ledger" "$stderr_file"; do
    if grep -qF -- "$secret" "$file"; then fail "step 10: a token, a code or a sent text is in $(basename "$file")"; fi
  done
done
echo "step 10: no token, code or sent text is in any ledger or in the controller's stderr"

echo "chat-api: ok"
