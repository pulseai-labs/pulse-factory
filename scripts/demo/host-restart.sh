#!/usr/bin/env bash
# Sessions survive a controller restart (d30): two paper seats and one chat run in session hosts that
# are systemd user units, and the controller, itself a user unit, is restarted and then killed
# mid-turn. The seats and the chat keep their processes, the chat's turn is stored once, and the
# stream a client reads has no gap and no repeat. Then every host is stopped, and the chat resumes
# from its launch record. It needs a real systemd user manager, so it runs outside a seat's fence:
# in CI's `systemd live` step, and at the round barrier and the spine close.
#
#   PF_BIN=target/debug/pulse-factory PF_HOST_BIN=target/debug/pf-host \
#     PF_REPLAY_BIN=target/debug/pf-claude-replay PF_STREAM_BIN=target/debug/pf-stream-reader \
#     scripts/demo/host-restart.sh
#
# It builds nothing: the four variables name built binaries (defaults `pulse-factory`, `pf-host`,
# `pf-claude-replay` and `pf-stream-reader` on PATH). It needs `jq`, `git`, `curl`, `base32`,
# `base64`, `od`, `sha256sum`, `systemd-run` and `systemctl`. It spends nothing, starts no real
# `claude` and reaches no address beyond 127.0.0.1. One wall-time budget of 60 s covers every phase.
#
# Phase 0 is the guard: with no systemd user manager it prints one line on stderr and exits 1.
# Phase 1 is the set-up: a temp directory, a host server name of its own (`d30` and nine hex digits,
# so its units are `pfc-<server>` and `pf-host-<server>-*` and the installed controller's units are
# never touched), the controller as a transient unit, a device, a project, a two-unit paper run and
# the chat session S-1. Phase 2 restarts the controller in the middle of a turn. Phase 3 kills it
# with SIGKILL in the middle of the same turn. Phase 4 stops every host and checks that the chat is
# lost, and that its next message resumes it from the launch record. The exit trap stops every unit
# and checks that none is left.
set -euo pipefail

# Phase 0: the guard, before any other check.
if ! systemctl --user show-environment >/dev/null 2>&1; then
  echo "host-restart: no systemd user manager (run outside the seat fence)" >&2
  exit 1
fi

T0="$(date +%s)"
BUDGET=60
PF_BIN="${PF_BIN:-pulse-factory}"
PF_HOST_BIN="${PF_HOST_BIN:-pf-host}"
PF_REPLAY_BIN="${PF_REPLAY_BIN:-pf-claude-replay}"
PF_STREAM_BIN="${PF_STREAM_BIN:-pf-stream-reader}"

fail() { echo "host-restart: FAIL: $*" >&2; exit 1; }
over_budget() { [ $(( $(date +%s) - T0 )) -le "$BUDGET" ] || fail "over the ${BUDGET} s budget"; }

for tool in jq git curl base32 base64 od sha256sum systemd-run systemctl awk grep sed seq mktemp tr head tail cut cmp sort; do
  command -v "$tool" >/dev/null || fail "$tool is not on PATH (this script needs it)"
done
abs_bin() {
  local found
  found="$(command -v "$1")" && [ -x "$found" ] || fail "$1 is not an executable"
  case "$found" in /*) printf '%s' "$found" ;; *) printf '%s/%s' "$PWD" "$found" ;; esac
}
PF_BIN="$(abs_bin "$PF_BIN")"
PF_HOST_BIN="$(abs_bin "$PF_HOST_BIN")"
PF_REPLAY_BIN="$(abs_bin "$PF_REPLAY_BIN")"
PF_STREAM_BIN="$(abs_bin "$PF_STREAM_BIN")"
[ -n "${XDG_RUNTIME_DIR:-}" ] || fail "XDG_RUNTIME_DIR is not set"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"
fixtures="$repo_root/crates/pf-adapter-claude/fixtures"
plain="$fixtures/plain_turn.jsonl"
resume_fixture="$fixtures/resume.jsonl"
[ -f "$plain" ] && [ -f "$resume_fixture" ] || fail "no fixtures under $fixtures"

tmp="$(mktemp -d)"
server="d30$(od -An -N5 -tx1 /dev/urandom | tr -d ' \n' | cut -c1-9)"
readers=()
finished=""
. "$here/lib/hosts.sh"
. "$here/lib/status.sh"

units() { systemctl --user list-units --all --plain --no-legend --no-pager "$1" 2>/dev/null | awk 'NF { print $1 }'; }

# The exit trap runs in pass and in fail: stop the readers, the controller unit and every host unit
# of this server, stop the detached hosts the helper knows, check that no unit is left, and remove the
# temp directory. A unit that is left fails a run that would otherwise pass.
cleanup() {
  local rc=$? left="" u
  trap - EXIT
  set +e
  for r in "${readers[@]:-}"; do [ -z "$r" ] || kill "$r" 2>/dev/null; done
  systemctl --user stop "pfc-$server" >/dev/null 2>&1
  for u in $(units "pf-host-$server-*"); do systemctl --user stop "$u" >/dev/null 2>&1; done
  stop_hosts
  left="$(units "pf-host-$server-*"; units "pfc-$server*")"
  if [ -n "$left" ]; then
    echo "host-restart: FAIL: units left behind: $(echo "$left" | tr '\n' ' ')" >&2
    [ "$rc" -ne 0 ] || rc=1
  fi
  rm -rf "$tmp"
  if [ "$rc" -eq 0 ] && [ -n "$finished" ]; then echo "host-restart: ok"; fi
  exit "$rc"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# --------------------------------------------------------------------------------- phase 1
unset PF_SEAT_TOKEN
export HOME="$tmp/home" PF_STATE_DIR="$tmp/state" PF_SHARE_DIR="$tmp/share"
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 GIT_TERMINAL_PROMPT=0
mkdir -p "$HOME" "$PF_SHARE_DIR" "$tmp/chat" "$tmp/gates" "$tmp/wt"
# The host server name is written before the controller starts, so the units carry this script's own
# server (G9) and never the installed controller's.
mkdir -p -m 700 "$PF_STATE_DIR" "$PF_STATE_DIR/hosts"
( umask 077; printf '%s\n' "$server" > "$PF_STATE_DIR/hosts/server" )
controller_ledger="$PF_STATE_DIR/controller.ledger"
chat_ledger="$PF_STATE_DIR/chat.ledger"
log="$tmp/controller.log"

check() { [ "$1" = "$2" ] || fail "$3: wanted '$2', got '$1'"; }

# Wait, with a bound and the script's budget, until a command succeeds: `wait_until <seconds> <what> <command…>`.
wait_until() {
  local seconds="$1" what="$2" tries
  shift 2
  tries=$((seconds * 20))
  for _ in $(seq 1 "$tries"); do
    if "$@" >/dev/null 2>&1; then return 0; fi
    over_budget
    sleep 0.05
  done
  fail "$what: not within $seconds s${log:+ ($(tail -n 5 "$log" 2>/dev/null | tr '\n' ' '))}"
}

# The wrapper of the replay (docs/contracts/chat-adapter.md). `--version` is no launch. Each launch
# records its argument list in `argv.<n>`, its PID in `pid.<n>` (the `exec` gives the replay the
# wrapper's PID) and its progress in `progress.<n>`. A fresh launch plays `plain_turn.jsonl`, held in
# the middle of the turn by two gates: before its first `assistant` line (g1) and before its `result`
# line (g2). A `--resume` launch plays `resume.jsonl` with no gate.
wrapper="$tmp/claude-wrapper"
{
  printf '#!/bin/sh\nd=%q\nreplay=%q\nplain=%q\nresume=%q\n' "$tmp/chat" "$PF_REPLAY_BIN" "$plain" "$resume_fixture"
  cat <<'WRAP'
if [ "$1" = "--version" ]; then exec "$replay" --fixture "$plain" "$@"; fi
n=$(ls "$d" | grep -c '^argv\.')
printf '%s\0' "$@" > "$d/argv.$n"
echo $$ > "$d/pid.$n"
case " $* " in
*" --resume "*) exec "$replay" --fixture "$resume" --progress "$d/progress.$n" "$@" ;;
esac
exec "$replay" --fixture "$plain" --gate "assistant:$d/g1" --gate "result:$d/g2" --progress "$d/progress.$n" "$@"
WRAP
} > "$wrapper"
chmod +x "$wrapper"
wrapper_sha="$(sha256sum "$wrapper" | cut -d' ' -f1)"

# The paper run: a copy of `scripts/demo/paper-restart`, and a bare forge that holds `main`.
paper="$tmp/paper"
mkdir -p "$paper" "$tmp/spine"
cp -R "$here/paper-restart/workflow" "$paper/workflow"
cp -R "$here/paper-restart/seats" "$paper/seats"
chmod +x "$paper"/seats/*.sh
git init -q --bare -b main "$paper/forge.git"

# The chat's project: a git repo with one commit.
mkdir -p "$tmp/work/alpha"
git -C "$tmp/work/alpha" init -q -b main
git -C "$tmp/work/alpha" -c user.name=demo -c user.email=demo@example.invalid commit -q --allow-empty -m demo

# Start the controller as a transient user unit, with the deploy unit's kill mode, and wait until it
# answers and the API listens. It sets `addr` and `spki`. A unit that was killed is collected, not at
# once, so a start waits until the old name is gone.
start_controller() {
  wait_until 10 "the old controller unit to go" no_controller_unit
  systemd-run --user --quiet --collect --unit="pfc-$server" -p KillMode=control-group \
    -p "StandardOutput=append:$log" -p "StandardError=append:$log" \
    -E "PATH=$PATH" -E "HOME=$HOME" -E "PF_STATE_DIR=$PF_STATE_DIR" -E "PF_SHARE_DIR=$PF_SHARE_DIR" \
    -E "XDG_RUNTIME_DIR=$XDG_RUNTIME_DIR" -E "PF_HOST_LAUNCHER=systemd" -E "PF_HOST_BIN=$PF_HOST_BIN" \
    -E "PF_CHAT_CLAUDE_BIN=$wrapper" -E "GIT_CONFIG_GLOBAL=/dev/null" -E "GIT_CONFIG_NOSYSTEM=1" \
    -E "GIT_TERMINAL_PROMPT=0" \
    -- "$PF_BIN" controller --tick-ms 200 --api-listen 127.0.0.1:0 \
    || fail "systemd-run of the controller failed"
  wait_until 30 "the controller socket" "$PF_BIN" settings get --json
  wait_until 30 "the API to listen" api_listening
  addr="$("$PF_BIN" api status --json | jq -r '.listen')"
  spki="$("$PF_BIN" api status --json | jq -r '.key_spki_sha256')"
  case "$addr" in 127.0.0.1:*) ;; *) fail "the API listens on '$addr', not on 127.0.0.1" ;; esac
}
api_listening() { [ "$("$PF_BIN" api status --json | jq -r '.state')" = listening ]; }

# The raw bytes of a `pf1-` code, in a file (docs/contracts/api.md, "Pairing codes").
decode_code() {
  local text="${1#pf1-}" pad
  text="$(printf '%s' "$text" | tr 'a-z' 'A-Z')"
  pad=$(( (8 - ${#text} % 8) % 8 ))
  while [ "$pad" -gt 0 ]; do text="$text="; pad=$((pad - 1)); done
  printf '%s' "$text" | base32 -d > "$2"
}

# A stream reader in the background: `reader <name> <after> [max seconds]`. It ends when the controller
# stops or dies, or after 50 s. `last_reader` is its PID.
last_reader=""
reader() {
  "$PF_STREAM_BIN" --url "wss://$addr/v1/stream?session=S-1&after=$2" --pin "$spki" --token-file "$tmp/token" --max-seconds "${3:-50}" \
    > "$tmp/$1.out" 2> "$tmp/$1.err" &
  readers+=("$!")
  last_reader=$!
}
ended() { ! kill -0 "$1" 2>/dev/null; }
frames() { jq -Rc 'fromjson? | select(.)' "$1"; }
has_frame() { jq -Rn -e "[inputs | fromjson?] | any(.[]; $2)" "$1" >/dev/null 2>&1; }
wait_frame() { wait_until 20 "$3" has_frame "$1" "$2"; }
last_seq() { frames "$1" | jq -r 'select(.type == "item") | .item.seq' | tail -n1; }
items_of() { grep '"type":"item"' "$1" || true; }

item_kinds() { "$PF_BIN" session items S-1 --json | jq -r '[.items[].body.kind] | join(" ")'; }
item_count() { "$PF_BIN" session items S-1 --json | jq -r --arg k "$1" '[.items[] | select(.body.kind == $k)] | length'; }
session_field() { "$PF_BIN" session list --all --json | jq -r ".sessions[] | select(.id == \"S-1\") | $1"; }

# A process's start time (field 22 of /proc/<pid>/stat), empty when it is gone.
starttime() { sed 's/.*) //' "/proc/$1/stat" 2>/dev/null | cut -d' ' -f20; }

no_controller_unit() { [ -z "$(units "pfc-$server*")" ]; }
no_host_units() { [ -z "$(units "pf-host-$server-*")" ]; }
three_host_units() { [ "$(host_units | wc -l)" -eq 3 ]; }
three_attach_lines() { [ "$(grep -c ' host_attach .* attached' "$controller_ledger" || true)" -eq 3 ]; }
two_turn_results() { [ "$(item_count turn_result)" -eq 2 ]; }
progress_at_least() { [ "$(cat "$tmp/chat/progress.$1" 2>/dev/null || echo 0)" -ge "$2" ]; }
# How many host lines the fixture holds before its first line of top-level type $1.
lines_before() {
  jq -c 'select(.to == "host") | .msg.type' "$plain" | awk -v t="\"$1\"" '$0 == t { print NR - 1; exit }'
}
# The units of the hosts of this server, without `.service`.
host_units() { units "pf-host-$server-*" | sed 's/\.service$//'; }

# --------------------------------------------------------------------------------- 1
start_controller
code="$("$PF_BIN" device pair --json | jq -r '.code')"
decode_code "$code" "$tmp/code.bin"
check "$(od -An -tx1 -v -j11 -N32 "$tmp/code.bin" | tr -d ' \n')" "$spki" "phase 1: the SPKI hash in the code"
pin="$(tail -c +12 "$tmp/code.bin" | head -c 32 | base64 -w0)"
paired="$(curl -sS -k --max-time 20 --pinnedpubkey "sha256//$pin" -X POST -H 'Content-Type: application/json' \
  --data-binary "$(jq -nc --arg code "$code" '{code: $code, device_name: "demo-d30"}')" "https://$addr/v1/pair")" \
  || fail "phase 1: the pairing call failed"
token="$(printf '%s' "$paired" | jq -r '.token')"
printf '%s' "$token" | grep -qE '^[0-9a-f]{64}$' || fail "phase 1: the token is not 64 hex digits"
printf '%s' "$token" > "$tmp/token"

project="$("$PF_BIN" project add "$tmp/work/alpha" --json | jq -r '.project.id')"
check "$project" P-1 "phase 1: the project's ID"
run_line="$("$PF_BIN" arm restart --workflow "$paper/workflow" --ledger "$tmp/ledger.md" --paper "$paper" \
  --param "spine_dir=$tmp/spine" --param "worktree_root=$tmp/wt" --param "gate_dir=$tmp/gates" --yes)" \
  || fail "phase 1: arm refused"
run="$(printf '%s\n' "$run_line" | sed -n 's/^run: //p' | head -n 1)"
[ -n "$run" ] || fail "phase 1: arm printed no run ID"
run_dir="$PF_STATE_DIR/runs/$run"
mkdir -p "$run_dir/queue"
order=0
for u in u1 u2; do
  order=$((order + 1))
  mkdir -p "$tmp/wt/$u"
  printf '{"version":1,"id":"%s","group":"g1","order":%d,"vars":{"worktree_path":"%s","branch":"work/%s"}}\n' \
    "$u" "$order" "$tmp/wt/$u" "$u" > "$run_dir/queue/.$u.json.tmp"
  mv "$run_dir/queue/.$u.json.tmp" "$run_dir/queue/$u.json"
done
session="$("$PF_BIN" session start --project P-1 --json | jq -r '.session.id')"
check "$session" S-1 "phase 1: the chat session's ID"
# A seat writes its PID once it runs; its session names its host once the controller committed it.
for u in u1 u2; do
  wait_until 20 "the seat of $u" test -s "$run_dir/out/$u/pids"
done
wait_until 20 "three host units" three_host_units
wait_until 20 "three host_attach lines" three_attach_lines
reader r1 0
r1_pid=$last_reader
wait_frame "$tmp/r1.out" '.type == "caught"' "phase 1: the first reader's caught frame"
echo "host-restart: ok phase 1: controller unit pfc-$server up on $addr, two paper seats and S-1 run in three host units"

seat_pid() { tail -n1 "$run_dir/out/$1/pids"; }
pid_u1="$(seat_pid u1)" pid_u2="$(seat_pid u2)"
pid_chat="$(cat "$tmp/chat/pid.0")"
start_u1="$(starttime "$pid_u1")" start_u2="$(starttime "$pid_u2")" start_chat="$(starttime "$pid_chat")"
[ -n "$start_u1" ] && [ -n "$start_u2" ] && [ -n "$start_chat" ] || fail "phase 1: a seat or the chat has no process"

# After `restarts` controller restarts: the same three processes, no second launch, and per unit one
# attach line for the launch and one for each rebind, one `survived` line for each rebind, no refusal.
survived_checks() {
  local restarts="$1" u n
  check "$(starttime "$pid_u1")" "$start_u1" "restart $restarts: seat u1's process"
  check "$(starttime "$pid_u2")" "$start_u2" "restart $restarts: seat u2's process"
  check "$(starttime "$pid_chat")" "$start_chat" "restart $restarts: the chat's process"
  check "$(ls "$tmp/chat" | grep -c '^argv\.' || true)" 1 "restart $restarts: the chat's launches"
  check "$(wc -l < "$run_dir/out/u1/pids")" 1 "restart $restarts: seat u1's starts"
  check "$(wc -l < "$run_dir/out/u2/pids")" 1 "restart $restarts: seat u2's starts"
  n="$(host_units | wc -l)"
  check "$n" 3 "restart $restarts: the host units"
  for u in $(host_units); do
    check "$(grep -c " host_attach $u attached" "$controller_ledger" || true)" "$((restarts + 1))" "restart $restarts: the attach lines of $u"
    check "$(grep -c " host_tier1 $u survived" "$controller_ledger" || true)" "$restarts" "restart $restarts: the survived lines of $u"
  done
  if grep -q ' refused ' "$controller_ledger"; then fail "restart $restarts: a refused line: $(grep ' refused ' "$controller_ledger")"; fi
  check "$(grep -c ' chat rebind .*outcome=survived' "$chat_ledger" || true)" "$restarts" "restart $restarts: the chat's rebind lines"
}

# --------------------------------------------------------------------------------- 2
"$PF_BIN" session send S-1 "hello d30" >/dev/null || fail "phase 2: session send failed"
before_assistant="$(lines_before assistant)"
wait_until 20 "the replay to hold before its first assistant line" progress_at_least 0 "$before_assistant"
wait_frame "$tmp/r1.out" '.type == "item" and .item.body.kind == "operator_message"' "phase 2: the operator message"
sleep 0.3
check "$(item_count assistant_text)" 0 "phase 2: the reply items before the restart"
systemctl --user restart "pfc-$server" || fail "phase 2: systemctl restart failed"
wait_until 20 "the first reader to end with the old controller" ended "$r1_pid"
wait_until 30 "the controller socket after the restart" "$PF_BIN" settings get --json
wait_until 30 "the API to listen after the restart" api_listening
addr="$("$PF_BIN" api status --json | jq -r '.listen')"
check "$("$PF_BIN" api status --json | jq -r '.key_spki_sha256')" "$spki" "phase 2: the server key after the restart"
survived_checks 1
seq1="$(last_seq "$tmp/r1.out")"
reader r2 "${seq1:-0}"
r2_pid=$last_reader
wait_frame "$tmp/r2.out" '.type == "caught"' "phase 2: the second reader's caught frame"
touch "$tmp/chat/g1"
wait_frame "$tmp/r2.out" '.type == "item" and .item.body.kind == "assistant_text"' "phase 2: the reply item"
echo "host-restart: ok phase 2: a restart in the middle of the turn kept every process, launched nothing again and attached each host again"
over_budget

# --------------------------------------------------------------------------------- 3
before_result="$(lines_before result)"
wait_until 20 "the replay to hold before its result line" progress_at_least 0 "$before_result"
sleep 0.3
check "$(item_count turn_result)" 0 "phase 3: the turn results before the kill"
systemctl --user kill -s KILL "pfc-$server" || fail "phase 3: systemctl kill failed"
wait_until 20 "the second reader to end with the killed controller" ended "$r2_pid"
start_controller
survived_checks 2
seq2="$(last_seq "$tmp/r2.out")"
reader r3 "${seq2:-0}"
wait_frame "$tmp/r3.out" '.type == "caught"' "phase 3: the third reader's caught frame"
touch "$tmp/chat/g2"
wait_frame "$tmp/r3.out" '.type == "item" and .item.body.kind == "turn_result"' "phase 3: the turn result"

# The item frames of the three readers run from 1 with no gap and no repeat, and equal a read from 0.
reader r4 0 2
wait "$last_reader" 2>/dev/null || true
{ items_of "$tmp/r1.out"; items_of "$tmp/r2.out"; items_of "$tmp/r3.out"; } > "$tmp/readers.items"
items_of "$tmp/r4.out" > "$tmp/r4.items"
[ -s "$tmp/r4.items" ] || fail "phase 3: a read from 0 gave no items"
cmp -s "$tmp/readers.items" "$tmp/r4.items" || fail "phase 3: the readers' item frames are not an after=0 read, byte for byte"
seqs="$(jq -Rr 'fromjson? | select(.type == "item") | .item.seq' "$tmp/readers.items" | tr '\n' ' ')"
want_seqs="$(seq 1 "$(jq -Rr 'fromjson? | select(.type == "item") | .item.seq' "$tmp/readers.items" | wc -l)" | tr '\n' ' ')"
check "$seqs" "$want_seqs" "phase 3: the item seqs, from 1 with no gap and no repeat"

want_text="$(jq -r 'select(.to == "host" and .msg.type == "assistant") | .msg.message.content[]? | select(.text) | .text' "$plain" | head -n1)"
got_text="$("$PF_BIN" session items S-1 --json | jq -r '[.items[] | select(.body.kind == "assistant_text")][0].body.text')"
check "$got_text" "$want_text" "phase 3: the reply item's text"
want_cost="$(jq -r 'select(.to == "host" and .msg.type == "result") | .msg.total_cost_usd' "$plain" | head -n1 | awk '{ printf "%d", $1 * 1000000 + 0.5 }')"
check "$(item_count turn_result)" 1 "phase 3: the turn results"
check "$("$PF_BIN" session items S-1 --json | jq -r '[.items[] | select(.body.kind == "turn_result")][0].body.usage.cost_micros')" "$want_cost" "phase 3: the turn's cost"
check "$(grep -c ' chat turn ' "$chat_ledger" || true)" 1 "phase 3: the turn lines of chat.ledger"
grep ' chat turn ' "$chat_ledger" | grep -q "cost_micros=$want_cost " || fail "phase 3: the turn line does not hold the cost $want_cost"
check "$(session_field '.usage.cost_micros')" "$want_cost" "phase 3: the session's cost"
check "$("$PF_BIN" session items S-1 --json | jq -r '[.items[] | select(.body.kind == "notice" and .body.code == "output_lost")] | length')" 0 "phase 3: the output_lost notices"

# The first seat was never launched again: it finishes done, and its numbered lines run from 1.
touch "$tmp/gates/u1"
unit_done() { status_json "$run" | jq -e '.units[] | select(.id == "u1" and .status == "done")' >/dev/null; }
wait_until 20 "seat u1 to finish done" unit_done
check "$(cat "$run_dir/out/u1/lines" | tr '\n' ' ')" "$(seq 1 "$(wc -l < "$run_dir/out/u1/lines")" | tr '\n' ' ')" "phase 3: seat u1's numbered lines"
check "$(grep -cx 1 "$run_dir/out/u1/lines" || true)" 1 "phase 3: the number 1 in seat u1's lines"
check "$(wc -l < "$run_dir/out/u1/pids")" 1 "phase 3: seat u1's starts"
echo "host-restart: ok phase 3: a SIGKILL in the middle of the turn lost nothing: the items run from 1 with no gap and no repeat, and the turn's cost is $want_cost, counted once"
over_budget

# --------------------------------------------------------------------------------- 4
systemctl --user stop "pfc-$server" || fail "phase 4: stopping the controller unit failed"
stopped="$(host_units)"
[ -n "$stopped" ] || fail "phase 4: no host unit was left to stop"
for u in $(units "pf-host-$server-*"); do systemctl --user stop "$u" || fail "phase 4: stopping $u failed"; done
for u in $stopped; do
  [ -f "$PF_STATE_DIR/hosts/exits/$u.json" ] || fail "phase 4: $u left no exit file"
done
wait_until 20 "the host units to go" no_host_units
start_controller
parked() { status_json "$run" | jq -e '.units[] | select(.id == "u2" and .status == "parked" and (.reason | contains("seat lost")))' >/dev/null; }
wait_until 20 "seat u2 to park with seat_lost" parked
check "$(session_field '.state')" idle "phase 4: the chat's state"
check "$("$PF_BIN" session items S-1 --json | jq -r '[.items[] | select(.body.kind == "notice" and .body.code == "host_lost")] | length')" 1 "phase 4: the host_lost notices"
ref="$(session_field '.runtime_session_ref')"
[ -n "$ref" ] && [ "$ref" != null ] || fail "phase 4: the session has no runtime session ID"
# A resume needs the runtime's transcript, which the replay does not write: the demo makes the stand-in.
mkdir -p "$HOME/.claude/projects/-demo"
echo '{}' > "$HOME/.claude/projects/-demo/$ref.jsonl"
"$PF_BIN" session send S-1 "carry on" >/dev/null || fail "phase 4: the second session send failed"
wait_until 20 "the second launch" test -f "$tmp/chat/argv.1"
resumed="$(tr '\0' '\n' < "$tmp/chat/argv.1" | awk '$0 == "--resume" { found = 1; next } found { print; exit }')"
check "$resumed" "$ref" "phase 4: the ID after --resume in the second launch"
second_open="$(grep ' chat open ' "$chat_ledger" | sed -n 2p)"
case "$second_open" in *"resume=yes"*"unit="*"incarnation=2 "*) ;; *) fail "phase 4: the second open line is not a resume of incarnation 2: $second_open" ;; esac
case "$second_open" in *"binary_sha256=$wrapper_sha"*) ;; *) fail "phase 4: the second open line does not hold the wrapper's SHA-256 $wrapper_sha: $second_open" ;; esac
case "$(grep ' chat open ' "$chat_ledger" | sed -n 1p)" in *"binary_sha256=$wrapper_sha"*) ;; *) fail "phase 4: the first open line does not hold the wrapper's SHA-256" ;; esac
wait_until 20 "the resumed turn's result" two_turn_results
echo "host-restart: ok phase 4: every host stopped, S-1 idle with a host_lost notice, u2 parked with seat_lost, and the next message resumed S-1 from its launch record (incarnation 2)"
over_budget

finished=yes
