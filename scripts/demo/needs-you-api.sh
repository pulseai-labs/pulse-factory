#!/usr/bin/env bash
# Answer the factory from a paired device (d20): the stop-rule question and the spine PR's ack, over
# the API, each with a one-use confirm code. It starts a controller with `--api-listen 127.0.0.1:0` on
# a temp state directory, arms the paper spine `paper-d20` (an implementer that raises a stop-rule
# question, then a spine PR that merges only on the claim's ack) and drives it from two paired
# devices, `D-1` and `D-2`, with a pinned `curl`.
#
# It does not build: `PF_BIN` and `PF_REPLAY_BIN` are built binaries (defaults `pulse-factory` and
# `pf-claude-replay` on PATH; `scripts/install.sh` installs both). It needs `curl`, `jq`, `git`,
# `sha256sum` and coreutils. It spends nothing, starts no real `claude`, calls no `gh` and reaches
# nothing beyond 127.0.0.1 and the bare repo of the paper forge. Everything lives in one temp
# directory, which the script removes on exit after it stops the controller.
#
#   PF_BIN=target/debug/pulse-factory PF_REPLAY_BIN=target/debug/pf-claude-replay \
#     scripts/demo/needs-you-api.sh
#
# The steps (each prints `step <n>: ...`):
#   1. start the controller, pair D-1 and D-2, add a project;
#   2. a process under a chat is refused on `arm`, `claim` and `ack`;
#   3. arm paper-d20; the CLI claims (generation 1);
#   4. the open `stop` request is on the API, with its verbs;
#   5. D-1 asks for a confirm code (the claim does not move) and answers (the claim moves to D-1);
#   6. a used, a foreign and an expired code are each refused with a word;
#   7. the spine PR's head moves after the code is issued: `subject_moved`; a new code merges it;
#   8. `api.ledger`, `controller.ledger` and the run ledger hold each refusal, act and merge;
#   9. no confirm code, claim token or device token is in any ledger or in the controller's stderr.
#
# `PAPER_SRC` names a copy of `scripts/demo/paper-d20` to run instead. It exits non-zero on any failed
# assertion and prints `needs-you-api: ok` at the end.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"
PF_BIN="${PF_BIN:-pulse-factory}"
PF_REPLAY_BIN="${PF_REPLAY_BIN:-pf-claude-replay}"
src="${PAPER_SRC:-$here/paper-d20}"
shipped_scripts="$repo_root/workflows/ossify-spine/scripts"
fixture="$repo_root/crates/pf-adapter-claude/fixtures/init.jsonl"
limit_seconds=100

fail() { echo "needs-you-api: FAIL: $*" >&2; exit 1; }
for tool in curl jq git sha256sum base32 base64 od tail head tr grep sed cut sleep; do
  command -v "$tool" >/dev/null || fail "$tool is not on PATH (this script needs it)"
done
for bin in "$PF_BIN" "$PF_REPLAY_BIN"; do
  command -v "$bin" >/dev/null || [ -x "$bin" ] || fail "$bin is not an executable"
done
[ -f "$shipped_scripts/rule.sh" ] || fail "no rule.sh at $shipped_scripts"
[ -f "$fixture" ] || fail "no fixture at $fixture"
[ -d "$src/workflow" ] || fail "no paper workflow at $src"
# The wrapper runs from another directory, so the binaries need absolute paths.
abs() {
  local p
  p="$(command -v "$1")"
  case "$p" in /*) printf '%s' "$p" ;; *) printf '%s' "$PWD/$p" ;; esac
}
PF_BIN="$(abs "$PF_BIN")"
PF_REPLAY_BIN="$(abs "$PF_REPLAY_BIN")"

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

unset PF_SEAT_TOKEN PULSE_FACTORY_CHAT
export HOME="$tmp/home" PF_STATE_DIR="$tmp/state" PF_SHARE_DIR="$tmp/share"
# Git must not read the host's configuration, hooks or credentials.
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 GIT_TERMINAL_PROMPT=0
mkdir -p "$HOME" "$PF_SHARE_DIR"
stderr_file="$tmp/controller.stderr"
api_ledger="$PF_STATE_DIR/api.ledger"
controller_ledger="$PF_STATE_DIR/controller.ledger"
chat_ledger="$PF_STATE_DIR/chat.ledger"
run_ledger="$tmp/run-ledger.md"
: > "$run_ledger"
started="$(date +%s)"

check() { [ "$1" = "$2" ] || fail "$3: wanted '$2', got '$1'"; }
within_limit() {
  [ $(( $(date +%s) - started )) -lt "$limit_seconds" ] \
    || fail "the demo did not finish within $limit_seconds s (waiting for: $1)"
}

# ------------------------------------------------------------------ the paper spine and its forge
paper="$tmp/paper"
spine_dir="$tmp/spine"
mkdir -p "$paper" "$spine_dir"
cp -R "$src/workflow" "$paper/workflow"
cp -R "$src/seats" "$paper/seats"
chmod +x "$paper"/seats/*.sh
# The answer hook of a stop file is the shipped one: the demo runs the package's own `rule.sh`.
mkdir -p "$paper/workflow/scripts"
cp "$shipped_scripts/rule.sh" "$paper/workflow/scripts/rule.sh"

base="main"
branch="spine-d20"
spine="demo-d20"
forge="$paper/forge.git"
git init -q --bare -b "$base" "$forge"
git init -q -b "$base" "$tmp/seed"
echo "base" > "$tmp/seed/README.md"
git -C "$tmp/seed" add README.md
git -C "$tmp/seed" -c user.name=paper -c user.email=paper@example.invalid commit -q -m "base root"
git -C "$tmp/seed" push -q "$forge" "$base"
git -C "$tmp/seed" push -q "$forge" "$base:refs/heads/$branch"

arm_args=("$spine" --workflow "$paper/workflow" --ledger "$run_ledger" --paper "$paper"
  --param "spine_branch=$branch" --param "spine_dir=$spine_dir" --param "worktree_root=$tmp/worktrees"
  --param "base_branch=$base" --yes)

# --------------------------------------------------------------------------- the chat's wrapper
# The wrapper is the chat process (docs/contracts/chat-adapter.md): it starts one background child,
# then becomes the replay of a recorded session (a `--version` probe gets the replay at once, with no
# child, and the child holds no pipe of the adapter). The child waits for `go`, then runs the operator's
# verbs from below the chat and writes each exit code and stderr. The adapter's environment
# allowlist passes no Pulse Factory variable but the controller's marker, so the child names the
# state directory itself.
chat_dir="$tmp/chat"
mkdir -p "$chat_dir"
fake_token="$chat_dir/fake-token"
printf '%064d' 0 | tr 0 a > "$fake_token"
wrapper="$tmp/claude-wrapper"
cat > "$wrapper" <<EOF
#!/bin/sh
case "\$1" in --version) exec "$PF_REPLAY_BIN" --fixture "$fixture" "\$@" ;; esac
(
  i=0
  while [ ! -e "$chat_dir/go" ] && [ \$i -lt 300 ]; do sleep 0.1; i=\$((i + 1)); done
  export PF_STATE_DIR="$PF_STATE_DIR" PF_SHARE_DIR="$PF_SHARE_DIR"
  "$PF_BIN" arm ${arm_args[*]@Q} > "$chat_dir/arm.out" 2> "$chat_dir/arm.err"; echo \$? > "$chat_dir/arm.rc"
  "$PF_BIN" claim > "$chat_dir/claim.out" 2> "$chat_dir/claim.err"; echo \$? > "$chat_dir/claim.rc"
  "$PF_BIN" ack R-1 --rev 1 --claim-token-file "$fake_token" > "$chat_dir/ack.out" 2> "$chat_dir/ack.err"
  echo \$? > "$chat_dir/ack.rc"
) < /dev/null > /dev/null 2>&1 &
exec "$PF_REPLAY_BIN" --fixture "$fixture" "\$@"
EOF
chmod +x "$wrapper"

# ------------------------------------------------------------------------- the API, pinned
addr="" spki="" pin="" status="" reply=""
start_controller() {
  # `PF_CONFIRM_TTL=3` shortens a confirm code to 3 seconds, so step 6 can wait one out.
  PF_CONFIRM_TTL=3 PF_CHAT_CLAUDE_BIN="$wrapper" PF_SERVER_NAME=demo-server \
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

# Pair one device with a fresh code: `pair <name> <token file>`.
pair() {
  local code body
  code="$("$PF_BIN" device pair --json | jq -r '.code')"
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

codes="$tmp/codes.txt"
: > "$codes"
# `issue <token> <request> <revision> <verb> <command id> [<text sha256 | to>]` sets `code`.
code=""
issue() {
  local subject
  case "$4" in
    answer) subject="$(jq -nc --arg r "$2" --argjson v "$3" --arg h "$6" \
      '{kind: "request", request: $r, revision: $v, verb: "answer", text_sha256: $h}')" ;;
    ack) subject="$(jq -nc --arg r "$2" --argjson v "$3" \
      '{kind: "request", request: $r, revision: $v, verb: "ack"}')" ;;
    *) fail "issue: no such verb $4" ;;
  esac
  verb_as "$1" confirm_code "$(jq -nc --argjson s "$subject" --arg c "$5" '{subject: $s, command_id: $c}')"
  check "$status" 200 "confirm_code for $2 ($reply)"
  code="$(printf '%s' "$reply" | jq -r '.code')"
  printf '%s' "$code" | grep -qE '^pfc1-[0-9a-f]{32}$' || fail "the confirm code has the wrong shape"
  printf '%s\n' "$code" >> "$codes"
}

# The current claim over the API: `claim_of <token>` prints "<generation> <holder>" or "none".
claim_of() {
  verb_as "$1" requests '{}'
  check "$status" 200 "requests ($reply)"
  printf '%s' "$reply" | jq -r '.claim | if . == null then "none" else "\(.generation) \(.holder)" end'
}

# Wait, with a bound, until `requests` (all) over the API holds an open request of a kind:
#   wait_open <token> <kind>; it prints the request ID.
wait_open() {
  local id
  while :; do
    within_limit "an open $2 request"
    verb_as "$1" requests '{"all": true}'
    check "$status" 200 "requests all ($reply)"
    id="$(printf '%s' "$reply" | jq -r --arg k "$2" '[.requests[] | select(.kind == $k and .status == "open")][0].id // empty')"
    [ -z "$id" ] || { printf '%s' "$id"; return 0; }
    sleep 0.2
  done
}

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
session="$(printf '%s' "$reply" | jq -r '.session.id')"
check "$session" S-1 "step 2: the session"
: > "$chat_dir/go"
for _ in $(seq 1 300); do [ -e "$chat_dir/ack.rc" ] && break; sleep 0.1; done
[ -e "$chat_dir/ack.rc" ] || fail "step 2: the process under the chat did not finish its three verbs"
for verb in arm claim ack; do
  check "$(cat "$chat_dir/$verb.rc")" 1 "step 2: the exit of $verb from below the chat"
  grep -q "$verb refused" "$chat_dir/$verb.err" || fail "step 2: $verb was not refused: $(cat "$chat_dir/$verb.err")"
  grep -qE 'chat process|PULSE_FACTORY_CHAT' "$chat_dir/$verb.err" \
    || fail "step 2: the refusal of $verb does not name the chat: $(cat "$chat_dir/$verb.err")"
done
verb_as "$token1" requests '{"all": true}'
check "$status" 200 "step 2: requests ($reply)"
check "$(printf '%s' "$reply" | jq -r '.requests | length')" 0 "step 2: no request exists after the refusals"
check "$(claim_of "$token1")" none "step 2: the claim after the refusals"
if "$PF_BIN" status >/dev/null 2>&1; then fail "step 2: a run exists after the refused arm"; fi
verb_as "$token1" session_end "$(jq -nc '{session_id: "S-1", command_id: "demo-end-1"}')"
check "$status" 200 "step 2: session_end ($reply)"
echo "step 2: arm, claim and ack from below the chat are each refused (exit 1) and no run or claim exists"

# --------------------------------------------------------------------------------- 3
arm_out="$("$PF_BIN" arm "${arm_args[@]}")" || fail "step 3: arm refused"
run="$(printf '%s\n' "$arm_out" | sed -n 's/^run: //p' | head -n 1)"
[ -n "$run" ] || fail "step 3: arm printed no run ID"
claim_json="$("$PF_BIN" claim --run "$run" --holder "demo-cli" --json)" || fail "step 3: claim refused"
check "$(printf '%s' "$claim_json" | jq -r '.generation')" 1 "step 3: the first claim"
echo "step 3: run $run armed from the script, the CLI holds claim 1"

# --------------------------------------------------------------------------------- 4
stop_id="$(wait_open "$token1" stop)"
verb_as "$token1" requests '{}'
check "$(printf '%s' "$reply" | jq -c --arg id "$stop_id" '.requests[] | select(.id == $id) | .verbs')" '["answer","clear"]' \
  "step 4: the verbs of the stop request"
verb_as "$token1" show "$(jq -nc --arg id "$stop_id" '{id: $id}')"
check "$status" 200 "step 4: show ($reply)"
stop_rev="$(printf '%s' "$reply" | jq -r '.revision')"
echo "step 4: the open stop request $stop_id (revision $stop_rev) is on the API with the verbs answer and clear"

# --------------------------------------------------------------------------------- 5
ruling="go"
text_hash="$(printf '%s' "$ruling" | sha256sum | cut -d' ' -f1)"
issue "$token1" "$stop_id" "$stop_rev" answer demo-code-1 "$text_hash"
answer_code="$code"
check "$(claim_of "$token1")" "1 demo-cli" "step 5: the claim after the code was issued"
verb_as "$token1" answer "$(jq -nc --arg r "$stop_id" --argjson v "$stop_rev" --arg t "$ruling" --arg c "$answer_code" \
  '{request: $r, rev: $v, text: $t, code: $c, command_id: "demo-answer-1"}')"
check "$status" 200 "step 5: the answer from D-1 ($reply)"
check "$(printf '%s' "$reply" | jq -r '.generation')" 2 "step 5: the generation the answer ran under"
case "$(claim_of "$token1")" in
  "2 device D-1 ("*) ;;
  *) fail "step 5: the claim did not move to D-1: $(claim_of "$token1")" ;;
esac
if "$PF_BIN" answer "$stop_id" --rev "$stop_rev" --text "late" >/dev/null 2>"$tmp/cli-answer.err"; then
  fail "step 5: the fenced CLI holder answered"
fi
grep -q 'the claim moved to device D-1' "$tmp/cli-answer.err" \
  || fail "step 5: the fenced CLI holder was not told where the claim went: $(cat "$tmp/cli-answer.err")"
echo "step 5: the code moved nothing; the answer from D-1 took the claim (generation 2); the CLI holder is told it moved to D-1"

# --------------------------------------------------------------------------------- 6
ack_id="$(wait_open "$token1" merge_ack)"
verb_as "$token1" show "$(jq -nc --arg id "$ack_id" '{id: $id}')"
check "$status" 200 "step 6: show the merge_ack ($reply)"
ack_rev="$(printf '%s' "$reply" | jq -r '.revision')"
check "$ack_rev" 1 "step 6: the revision of the merge_ack"
tip="$(git -C "$forge" rev-parse "refs/heads/$branch")"
check "$(printf '%s' "$reply" | jq -r '.head_sha')" "$tip" "step 6: the head SHA of the merge_ack"
check "$(printf '%s' "$reply" | jq -r '.base')" "$base" "step 6: the base of the merge_ack"
# A used code: the same code again, with a new command ID.
verb_as "$token1" answer "$(jq -nc --arg r "$stop_id" --argjson v "$stop_rev" --arg t "$ruling" --arg c "$answer_code" \
  '{request: $r, rev: $v, text: $t, code: $c, command_id: "demo-answer-2"}')"
check "$status" 422 "step 6: the used code ($reply)"
check "$(printf '%s' "$reply" | jq -r '.refusal')" code_used "step 6: the word of the used code"
# A foreign code: D-2 presents a code that D-1 holds.
issue "$token1" "$ack_id" "$ack_rev" ack demo-code-2
foreign_code="$code"
verb_as "$token2" ack "$(jq -nc --arg r "$ack_id" --argjson v "$ack_rev" --arg c "$foreign_code" \
  '{request: $r, rev: $v, code: $c, command_id: "demo-ack-foreign"}')"
check "$status" 422 "step 6: the foreign code ($reply)"
check "$(printf '%s' "$reply" | jq -r '.refusal')" code_foreign "step 6: the word of the foreign code"
# An expired code: left for 4 seconds, longer than the 3 s TTL.
issue "$token1" "$ack_id" "$ack_rev" ack demo-code-3
expired_code="$code"
sleep 4
verb_as "$token1" ack "$(jq -nc --arg r "$ack_id" --argjson v "$ack_rev" --arg c "$expired_code" \
  '{request: $r, rev: $v, code: $c, command_id: "demo-ack-expired"}')"
check "$status" 422 "step 6: the expired code ($reply)"
check "$(printf '%s' "$reply" | jq -r '.refusal')" code_expired "step 6: the word of the expired code"
[ "$(git -C "$forge" rev-list --count "$base")" = 1 ] || fail "step 6: $base moved on a refused ack"
echo "step 6: a used code is code_used, D-2 with a code of D-1 is code_foreign, a code left for 4 s is code_expired"

# --------------------------------------------------------------------------------- 7
verb_as "$token1" requests '{"all": true}'
check "$status" 200 "step 7: requests all ($reply)"
check "$(printf '%s' "$reply" | jq -r --arg id "$stop_id" '.requests[] | select(.id == $id) | .status')" answered \
  "step 7: the stop request"
check "$(printf '%s' "$reply" | jq -r --arg id "$ack_id" '.requests[] | select(.id == $id) | "\(.status) \(.pr != null) \(.base) \(.head_sha)"')" \
  "open true $base $tip" "step 7: the merge_ack with its PR, base and head SHA"
# The late commit is ready before the code is issued, so only the push falls inside the 3 s.
git clone -q --branch "$branch" "$forge" "$tmp/late"
echo "late" > "$tmp/late/late.txt"
git -C "$tmp/late" add late.txt
git -C "$tmp/late" -c user.name=operator -c user.email=operator@example.invalid commit -q -m "late fix"
issue "$token1" "$ack_id" "$ack_rev" ack demo-code-4
stale_code="$code"
git -C "$tmp/late" push -q origin "$branch"
new_tip="$(git -C "$forge" rev-parse "refs/heads/$branch")"
[ "$new_tip" != "$tip" ] || fail "step 7: the late commit did not move $branch"
verb_as "$token1" ack "$(jq -nc --arg r "$ack_id" --argjson v "$ack_rev" --arg c "$stale_code" \
  '{request: $r, rev: $v, code: $c, command_id: "demo-ack-stale"}')"
check "$status" 422 "step 7: the ack after the head moved ($reply)"
check "$(printf '%s' "$reply" | jq -r '.refusal')" subject_moved "step 7: the word of the moved head"
verb_as "$token1" show "$(jq -nc --arg id "$ack_id" '{id: $id}')"
check "$(printf '%s' "$reply" | jq -r '.revision')" 2 "step 7: the revision after the head moved"
check "$(printf '%s' "$reply" | jq -r '.head_sha')" "$new_tip" "step 7: the head SHA after the head moved"
[ "$(git -C "$forge" rev-list --count "$base")" = 1 ] || fail "step 7: $base moved on a refused ack"
issue "$token1" "$ack_id" 2 ack demo-code-5
verb_as "$token1" ack "$(jq -nc --arg r "$ack_id" --arg c "$code" '{request: $r, rev: 2, code: $c, command_id: "demo-ack-2"}')"
check "$status" 200 "step 7: the ack at revision 2 ($reply)"
check "$(printf '%s' "$reply" | jq -r '.merged')" true "step 7: the merge"
check "$(printf '%s' "$reply" | jq -r '.head_sha')" "$new_tip" "step 7: the merged head SHA"
git -C "$forge" merge-base --is-ancestor "$new_tip" "$base" || fail "step 7: $base does not hold the new spine head $new_tip"
echo "step 7: a head that moved after the code is subject_moved (revision 2); a new code merged $new_tip into $base"

# --------------------------------------------------------------------------------- 8
refused_line() { # <verb> <device> <word>
  grep -E "verb=$1 device=$2 .*outcome=refused refusal=$3\$" "$api_ledger" >/dev/null \
    || fail "step 8: api.ledger has no refused $1 line of $2 with $3: $(cat "$api_ledger")"
}
refused_line answer D-1 code_used
refused_line ack D-2 code_foreign
refused_line ack D-1 code_expired
refused_line ack D-1 subject_moved
check "$(grep -c 'outcome=refused' "$api_ledger" || true)" 4 "step 8: the refused lines of api.ledger"
for want in " confirm_code D-1 " " device_answer D-1 $stop_id@$stop_rev " " device_ack D-1 $ack_id@2 "; do
  grep -qF -- "$want" "$controller_ledger" || fail "step 8: controller.ledger has no line with '$want'"
done
grep -E "merge of unit spine: .*branch=$branch sha=$new_tip outcome=landed" "$run_ledger" >/dev/null \
  || fail "step 8: the run ledger has no landed merge of $new_tip"
echo "step 8: api.ledger holds the four refusals with their words; controller.ledger and the run ledger hold the acts and the merge"

# --------------------------------------------------------------------------------- 9
secrets=("$token1" "$token2" "$(cat "$PF_STATE_DIR/claim-token")")
while IFS= read -r c; do secrets+=("$c"); done < "$codes"
for secret in "${secrets[@]}"; do
  for file in "$api_ledger" "$controller_ledger" "$run_ledger" "$chat_ledger" "$stderr_file"; do
    if grep -qF -- "$secret" "$file"; then fail "step 9: a confirm code, a claim token or a device token is in $(basename "$file")"; fi
  done
done
echo "step 9: no confirm code, claim token or device token is in any ledger or in the controller's stderr"

echo "needs-you-api: ok"
