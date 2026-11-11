#!/usr/bin/env bash
# Chat policies as scoped settings (d21): the self-started turn cap and the deny list at launch.
# The real Claude Code adapter runs against a replay of `deny_rules.jsonl`, the recording of
# qualification case 19: an operator turn and two `Task` turns, then the deny-rule prompts. It starts
# a controller on a temporary state directory with `PF_CHAT_CLAUDE_BIN` set to a wrapper that records
# its argument list and then runs `pf-claude-replay`. Five steps:
#
#   1. a project value of `chat.self_turn_cap` overrides the server's, and `settings get` names the
#      scope and the row it masks;
#   2. the first launch carries the deny rules;
#   3. a process that descends from the chat is refused on `settings set`, and the refusal is audited;
#   4. the chat is stopped as it starts its second turn on its own, and shows `needs_you` with the
#      reason, one `chat.ledger` line;
#   5. the operator unsets the project value, and the next message resumes the session.
#
# It does not build: `PF_BIN` and `PF_REPLAY_BIN` are built binaries (defaults `pulse-factory` and
# `pf-claude-replay` on PATH). It needs `jq`, `git` and coreutils. It spends nothing, starts no real
# `claude` and reaches no address. Every file is under one temporary directory that a trap removes.
#
# The replay does not answer the adapter's context-usage request, so each turn's end holds the chat's
# event pump for the adapter's 30 s control timeout (docs/contracts/chat-adapter.md). Step 4 waits for
# two such holds, and the waits below are sized for it. A run takes about one minute.
#
#   PF_BIN=target/debug/pulse-factory PF_REPLAY_BIN=target/debug/pf-claude-replay \
#     scripts/demo/chat-policy.sh
set -euo pipefail

PF_BIN="${PF_BIN:-pulse-factory}"
PF_REPLAY_BIN="${PF_REPLAY_BIN:-pf-claude-replay}"

fail() { echo "chat-policy: FAIL: $*" >&2; exit 1; }
for tool in jq git tr grep sed awk head sleep seq mktemp; do
  command -v "$tool" >/dev/null || fail "$tool is not on PATH (this script needs it)"
done
for bin in "$PF_BIN" "$PF_REPLAY_BIN"; do
  command -v "$bin" >/dev/null || [ -x "$bin" ] || fail "$bin is not an executable"
done
# The wrapper and the controller run from other directories, so each binary needs an absolute path.
PF_BIN="$(command -v "$PF_BIN")"
case "$PF_BIN" in /*) ;; *) PF_BIN="$PWD/$PF_BIN" ;; esac
PF_REPLAY_BIN="$(command -v "$PF_REPLAY_BIN")"
case "$PF_REPLAY_BIN" in /*) ;; *) PF_REPLAY_BIN="$PWD/$PF_REPLAY_BIN" ;; esac
JQ_BIN="$(command -v jq)"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fixture="$repo_root/crates/pf-adapter-claude/fixtures/deny_rules.jsonl"
[ -f "$fixture" ] || fail "no fixture at $fixture"

tmp="$(mktemp -d)"
pid=""
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
  rm -rf "$tmp"
}
trap cleanup EXIT

unset PF_SEAT_TOKEN
export HOME="$tmp/home" PF_STATE_DIR="$tmp/state" PF_SHARE_DIR="$tmp/share"
mkdir -p "$HOME" "$PF_SHARE_DIR" "$tmp/wrapper"
stderr_file="$tmp/controller.stderr"
controller_ledger="$PF_STATE_DIR/controller.ledger"
chat_ledger="$PF_STATE_DIR/chat.ledger"

check() { [ "$1" = "$2" ] || fail "$3: wanted '$2', got '$1'"; }

# A repo with one commit.
mkdir -p "$tmp/work/alpha"
git -C "$tmp/work/alpha" init -q -b main
git -C "$tmp/work/alpha" -c user.name=demo -c user.email=demo@example.invalid commit -q --allow-empty -m demo

# The wrapper of the replay (docs/contracts/chat-adapter.md). The adapter asks `--version` before each
# launch: that is no launch, so it records nothing. Each launch records its argument list in
# `argv.<n>`. On the first launch only, it also starts one background child that waits until the
# session shows a live process, then tries `settings set` from inside the chat. The child descends
# from the chat process (`exec` gives the replay the wrapper's PID). The adapter's environment
# allowlist passes no Pulse Factory name to the chat, so the wrapper names the state directory
# itself. The child closes its own standard streams, so it holds none of the chat's pipes.
wrapper="$tmp/claude-wrapper"
cat > "$wrapper" <<WRAP
#!/bin/sh
d='$tmp/wrapper'
if [ "\$1" != "--version" ]; then
  n=\$(ls "\$d" | grep -c '^argv\\.')
  printf '%s\\0' "\$@" > "\$d/argv.\$n"
  if [ ! -e "\$d/first-launch" ]; then
    : > "\$d/first-launch"
    (
      exec >/dev/null 2>&1 </dev/null
      for _ in \$(seq 1 300); do
        if PF_STATE_DIR='$PF_STATE_DIR' '$PF_BIN' session list --json | '$JQ_BIN' -e '[.sessions[] | select(.process != null)] | length > 0' >/dev/null; then
          rc=0
          PF_STATE_DIR='$PF_STATE_DIR' '$PF_BIN' settings set chat.self_turn_cap 50 --project P-1 || rc=\$?
          echo "\$rc" > "\$d/child.rc"
          exit 0
        fi
        sleep 0.1
      done
      echo timeout > "\$d/child.rc"
    ) &
  fi
fi
exec '$PF_REPLAY_BIN' --fixture '$fixture' "\$@"
WRAP
chmod +x "$wrapper"

PF_CHAT_CLAUDE_BIN="$wrapper" PF_SERVER_NAME=demo-server \
  "$PF_BIN" controller --tick-ms 3600000 >/dev/null 2>"$stderr_file" &
pid=$!
ready=""
for _ in $(seq 1 200); do
  if "$PF_BIN" settings get --json >/dev/null 2>&1; then ready=yes; break; fi
  kill -0 "$pid" 2>/dev/null || fail "the controller exited before it answered: $(head -c 400 "$stderr_file")"
  sleep 0.05
done
[ -n "$ready" ] || fail "the controller did not answer within 10 s"

project="$("$PF_BIN" project add "$tmp/work/alpha" --json | jq -r '.project.id')"
check "$project" P-1 "the project's ID"

# Wait, with a bound, until `$1` (a shell command) succeeds. `wait_until <seconds> <what> <command…>`.
wait_until() {
  local seconds="$1" what="$2"; shift 2
  local tries=$((seconds * 10))
  for _ in $(seq 1 "$tries"); do
    if "$@" >/dev/null 2>&1; then return 0; fi
    kill -0 "$pid" 2>/dev/null || fail "$what: the controller exited: $(head -c 400 "$stderr_file")"
    sleep 0.1
  done
  fail "$what: not within $seconds s"
}

session_field() { "$PF_BIN" session list --all --json | jq -r --arg id "$1" ".sessions[] | select(.id == \$id) | $2"; }
state_is() { [ "$(session_field "$1" '.state')" = "$2" ]; }
cap_lines() { grep -c 'reason=self_turn_cap' "$chat_ledger" || true; }
argv_after() { tr '\0' '\n' < "$tmp/wrapper/argv.$1" | awk -v flag="$2" 'found { print; exit } $0 == flag { found = 1 }'; }

# --------------------------------------------------------------------------------- 1
"$PF_BIN" settings set chat.self_turn_cap 5 >/dev/null || fail "step 1: settings set (server) failed"
"$PF_BIN" settings set chat.self_turn_cap 1 --project P-1 >/dev/null || fail "step 1: settings set (project) failed"
got="$("$PF_BIN" settings get chat.self_turn_cap --project P-1 --json)"
check "$(jq -r '.settings[0].value' <<<"$got")" 1 "step 1: the resolved cap"
check "$(jq -r '.settings[0].from.kind' <<<"$got")" project "step 1: the scope of the cap"
check "$(jq -r '.settings[0].masked | length' <<<"$got")" 1 "step 1: the rows the project value masks"
check "$(jq -r '.settings[0].masked[0].scope.kind' <<<"$got")" server "step 1: the scope it masks"
check "$(jq -r '.settings[0].masked[0].value' <<<"$got")" 5 "step 1: the value it masks"
echo "chat-policy: ok step 1: the project's cap of 1 overrides the server's 5, and get names the scope and the masked row"

# --------------------------------------------------------------------------------- 2
started="$("$PF_BIN" session start --project P-1 --json)"
session="$(jq -r '.session.id' <<<"$started")"
check "$session" S-1 "step 2: the session's ID"
"$PF_BIN" session send "$session" "start the background tasks" >/dev/null || fail "step 2: session send failed"
[ -f "$tmp/wrapper/argv.0" ] || fail "step 2: the wrapper recorded no launch"
rules="$(argv_after 0 --disallowedTools)"
case "$rules" in *'Bash(gh pr merge *)'*) ;; *) fail "step 2: the launch has no rule for gh pr merge: $rules" ;; esac
case "$rules" in *'Bash(git push * --force*)'*) ;; *) fail "step 2: the launch has no rule for a forced push: $rules" ;; esac
echo "chat-policy: ok step 2: the first launch carries the deny rules (gh pr merge, git push --force)"

# --------------------------------------------------------------------------------- 3
wait_until 30 "step 3: the chat's child process finished" test -f "$tmp/wrapper/child.rc"
check "$(cat "$tmp/wrapper/child.rc")" 1 "step 3: the exit of settings set from inside the chat"
grep -q 'setting_set refused caller=chat:S-1' "$controller_ledger" || fail "step 3: no refused line for the chat in controller.ledger: $(cat "$controller_ledger")"
check "$("$PF_BIN" settings get chat.self_turn_cap --project P-1 --json | jq -r '.settings[0].value')" 1 "step 3: the cap after the chat's try"
echo "chat-policy: ok step 3: a process below the chat was refused on settings set, the refusal is in controller.ledger, and the cap is still 1"

# --------------------------------------------------------------------------------- 4
wait_until 240 "step 4: the session shows needs_you" state_is "$session" needs_you
check "$(session_field "$session" '.process')" null "step 4: the session's process after the stop"
"$PF_BIN" session items "$session" --json | jq -e '[.items[] | select(.body.kind == "notice" and .body.code == "self_turn_cap")] | length == 1' >/dev/null \
  || fail "step 4: no self_turn_cap notice in the session's items"
check "$(cap_lines)" 1 "step 4: the self_turn_cap lines in chat.ledger"
grep -q 'reason=self_turn_cap self_turns=2 cap=1 from=project:P-1' "$chat_ledger" \
  || fail "step 4: the stop line is not the one expected: $(grep 'reason=self_turn_cap' "$chat_ledger")"
echo "chat-policy: ok step 4: the chat was stopped as it started its second turn on its own, shows needs_you, and chat.ledger holds one reason=self_turn_cap line"

# --------------------------------------------------------------------------------- 5
# The replay plays `deny_rules.jsonl` from its first line on every launch, so the resumed process
# plays the same operator turn and two `Task` turns again. The project value of 1 would stop it
# again, so the operator unsets it first and the server's 5 applies.
"$PF_BIN" settings unset chat.self_turn_cap --project P-1 >/dev/null || fail "step 5: settings unset failed"
# A resume needs the runtime's transcript, which the replay does not write: the adapter looks for
# `<config dir>/projects/*/<session id>.jsonl` (the config dir is `$HOME/.claude`). The demo makes one.
runtime_session="$(argv_after 0 --session-id)"
[ -n "$runtime_session" ] || fail "step 5: the first launch has no --session-id"
mkdir -p "$HOME/.claude/projects/-demo"
echo '{}' > "$HOME/.claude/projects/-demo/$runtime_session.jsonl"
"$PF_BIN" session send "$session" "carry on" >/dev/null || fail "step 5: the second session send failed"
wait_until 30 "step 5: the wrapper recorded the second launch" test -f "$tmp/wrapper/argv.1"
tr '\0' '\n' < "$tmp/wrapper/argv.1" | grep -qx -- '--resume' || fail "step 5: the second launch is not a resume: $(tr '\0' ' ' < "$tmp/wrapper/argv.1")"
if state_is "$session" needs_you; then fail "step 5: the session is still needs_you after the operator's message"; fi
sleep 2
if state_is "$session" needs_you; then fail "step 5: the session went back to needs_you"; fi
check "$(cap_lines)" 1 "step 5: the self_turn_cap lines in chat.ledger after the resume"
echo "chat-policy: ok step 5: the operator's next message resumed the session, and chat.ledger still holds one reason=self_turn_cap line"

for file in "$controller_ledger" "$chat_ledger"; do
  if grep -qF -- 'start the background tasks' "$file" || grep -qF -- 'carry on' "$file"; then
    fail "a sent text is in $(basename "$file")"
  fi
done

echo "chat-policy: ok"
