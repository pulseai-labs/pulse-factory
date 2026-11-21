#!/usr/bin/env bash
# Take the options of every decision from a paired device (d32): a seat's options, a note, a workflow's
# options and a merge ack, each under a one-use confirm code, with a notice of each request on the app
# channel and in the project's Orchestrator chat. It starts a controller with `--api-listen 127.0.0.1:0`
# on a temp state directory, arms the paper spine `paper-options` from a start card (so the run has a
# project) and drives it from the paired device `D-1`, with a pinned `curl` and `pf-stream-reader`.
#
# It does not build: `PF_BIN`, `PF_REPLAY_BIN` and `PF_STREAM_BIN` are built binaries (defaults
# `pulse-factory`, `pf-claude-replay` and `pf-stream-reader` on PATH; `scripts/install.sh` installs
# them). It needs `curl`, `jq`, `git`, `sha256sum` and coreutils. It spends nothing, starts no real
# `claude`, calls no `gh` and reaches nothing beyond 127.0.0.1 and the bare repo of the paper forge.
# Everything lives in one temp directory, which the script removes on exit after it stops the controller
# and its hosts.
#
#   PF_BIN=target/debug/pulse-factory PF_REPLAY_BIN=target/debug/pf-claude-replay \
#     PF_STREAM_BIN=target/debug/pf-stream-reader scripts/demo/needs-you-options.sh
#
# The steps (each prints `step <n>: ...`):
#   1. start the controller, pair D-1, add a project, cap the notice words, start the Orchestrator
#      session and send one message, start a second Orchestrator session, open D-1's stream (it
#      acknowledges each notice but the first frame) and arm the proposal under an `arm` code;
#   2. the stream of D-1 is open and has joined at the newest notice;
#   3. the stop: its block names the seat's two options and the recommended one; the first notice frame
#      is not acknowledged, comes again, and is acknowledged before D-1 takes `s2` under a code that
#      binds the option;
#   4. the park: a seat that ended its turn with no done signal; D-1 answers with a typed note, and the
#      next dispatch reads it;
#   5. the red gate: the workflow's two options, `w1` recommended; D-1 takes `w1`, the packet is
#      retired, and the unit runs `verify` next;
#   6. the merge ack and the carry: while it is open, one message to the second Orchestrator session
#      carries its notice with no model turn; then D-1 approves and the spine PR merges;
#   7. the notices: one `opened` frame for each request, at most 8 words, and one `notice` item for each
#      in the second session, at most 10 words before its IDs; the first notice was sent twice and
#      acknowledged once;
#   8. the ledgers hold each option act, and no code, token or marker word is where it must not be.
#
# `PAPER_SRC` names a copy of `scripts/demo/paper-options` to run instead. It exits non-zero on any
# failed check and prints `needs-you-options: ok` at the end.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"
PF_BIN="${PF_BIN:-pulse-factory}"
PF_REPLAY_BIN="${PF_REPLAY_BIN:-pf-claude-replay}"
PF_STREAM_BIN="${PF_STREAM_BIN:-pf-stream-reader}"
PAPER_SRC="${PAPER_SRC:-$here/paper-options}"
plain_fixture="$repo_root/crates/pf-adapter-claude/fixtures/plain_turn.jsonl"
limit_seconds=60
marker="kestrel"

fail() { echo "needs-you-options: FAIL: $*" >&2; exit 1; }
for tool in curl jq git sha256sum base32 base64 od tail head tr grep sed cut sleep wc; do
  command -v "$tool" >/dev/null || fail "$tool is not on PATH (this script needs it)"
done
for bin in "$PF_BIN" "$PF_REPLAY_BIN" "$PF_STREAM_BIN"; do
  command -v "$bin" >/dev/null || [ -x "$bin" ] || fail "$bin is not an executable"
done
[ -x "$here/start-env.sh" ] || fail "no start-env.sh next to this script"
[ -f "$plain_fixture" ] || fail "no fixture at $plain_fixture"
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

tmp="$(mktemp -d)"
pid=""
readers=()
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
  for r in ${readers[@]+"${readers[@]}"}; do kill "$r" 2>/dev/null || true; done
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

# The paper env and the first chat's wrapper (the one that proposes). The second session's wrapper
# is below: it replays a plain turn, so its first turn makes no proposal.
"$here/start-env.sh" "$tmp/start" > "$tmp/start-env.out" 2>&1 || fail "start-env.sh failed: $(cat "$tmp/start-env.out")"
start="$tmp/start"
run_ledger="$start/ledger.md"
wrapper="$tmp/claude-wrapper"
cat > "$wrapper" <<EOF
#!/bin/sh
case "\$1" in --version) exec "$start/claude-wrapper" "\$@" ;; esac
if [ "\$PULSE_FACTORY_CHAT" = S-1 ]; then exec "$start/claude-wrapper" "\$@"; fi
exec "$PF_REPLAY_BIN" --fixture "$plain_fixture" "\$@"
EOF
chmod +x "$wrapper"

# ------------------------------------------------------------------------- the API, pinned
addr="" spki="" pin="" status="" reply=""
start_controller() {
  # `--tick-ms 100` keeps the paper seats' hand-offs short. A notice is sent again after 10 s.
  PF_CHAT_CLAUDE_BIN="$wrapper" PF_SERVER_NAME=demo-server \
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

# The subject of an act on a request: `subject <request> <revision> <verb> [<option>]`.
subject() {
  if [ -n "${4:-}" ]; then
    jq -nc --arg r "$1" --argjson v "$2" --arg b "$3" --arg o "$4" \
      '{kind: "request", request: $r, revision: $v, verb: $b, option: $o}'
  else
    jq -nc --arg r "$1" --argjson v "$2" --arg b "$3" '{kind: "request", request: $r, revision: $v, verb: $b}'
  fi
}

# Take an option under a code that binds it: `take <verb> <request> <revision> <option> <command id>`.
take() {
  issue "$(subject "$2" "$3" "$1" "$4")" "$5-code"
  local body
  case "$1" in
    answer) body="$(jq -nc --arg r "$2" --argjson v "$3" --arg o "$4" --arg k "$code" --arg c "$5" \
      '{request: $r, rev: $v, text: "", option: $o, code: $k, command_id: $c}')" ;;
    ack) body="$(jq -nc --arg r "$2" --argjson v "$3" --arg o "$4" --arg k "$code" --arg c "$5" \
      '{request: $r, rev: $v, option: $o, code: $k, command_id: $c}')" ;;
    *) fail "take: no such verb $1" ;;
  esac
  verb "$1" "$body"
  check "$status" 200 "$1 with option $4 ($reply)"
  check "$(printf '%s' "$reply" | jq -r '.option')" "$4" "the option the $1 took"
}

# Wait, with a bound, until the device's `requests` (all) hold an open request of a kind; it prints the ID.
wait_open() {
  local id
  while :; do
    within_limit "an open $1 request"
    verb requests '{"all": true}'
    check "$status" 200 "requests all ($reply)"
    id="$(printf '%s' "$reply" | jq -r --arg k "$1" '[.requests[] | select(.kind == $k and .status == "open")][0].id // empty')"
    [ -z "$id" ] || { printf '%s' "$id"; return 0; }
    sleep 0.1
  done
}

# The request's decision block over the API: `block <request>`. It sets `shown` (the whole answer).
shown=""
block() {
  verb show "$(jq -nc --arg id "$1" '{id: $id}')"
  check "$status" 200 "show $1 ($reply)"
  shown="$reply"
}

frames() { jq -Rc 'fromjson? | select(.)' "$1"; }
wait_frame() {
  local file="$1" filter="$2" what="$3"
  for _ in $(seq 1 600); do
    if jq -Rn -e "[inputs | fromjson?] | any(.[]; $filter)" "$file" >/dev/null 2>&1; then return 0; fi
    within_limit "$what"
    sleep 0.1
  done
  fail "$what: no such frame in $(basename "$file")"
}

# Wait, with a bound, until a ledger holds a line: `wait_line <file> <fixed text> <what>`.
wait_line() {
  for _ in $(seq 1 600); do
    if grep -qF -- "$2" "$1" 2>/dev/null; then return 0; fi
    within_limit "$3"
    sleep 0.1
  done
  fail "$3: no line with '$2' in $(basename "$1")"
}

words() { printf '%s' "$1" | wc -w | tr -d ' '; }

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
"$PF_BIN" settings set notify.app.max_words 8 --project "$project" >/dev/null || fail "step 1: the app word cap was refused"
"$PF_BIN" settings set notify.orchestrator.max_words 10 --project "$project" >/dev/null || fail "step 1: the orchestrator word cap was refused"
"$PF_BIN" settings set notify.resend_seconds 10 --project "$project" >/dev/null || fail "step 1: the re-send wait was refused"
verb session_start "$(jq -nc --arg p "$project" '{project_id: $p, command_id: "demo-start-1"}')"
check "$status" 200 "step 1: session_start ($reply)"
check "$(printf '%s' "$reply" | jq -r '.session.id')" S-1 "step 1: the first session"
verb session_send "$(jq -nc '{session_id: "S-1", text: "start the spine s1 of this project", command_id: "demo-send-1"}')"
check "$status" 200 "step 1: session_send ($reply)"
card=""
while :; do
  within_limit "the start card"
  card="$("$PF_BIN" session items S-1 --json | jq -c '[.. | objects | select(.body? and .body.kind? == "start_card")][0] // empty')"
  [ -z "$card" ] || break
  sleep 0.1
done
proposal="$(printf '%s' "$card" | jq -r '.body.proposal')"
plan="$(printf '%s' "$card" | jq -r '.body.plan_sha256')"
# D-1's stream opens before the arm, so it joins at the newest notice and gets every later one.
"$PF_STREAM_BIN" --url "wss://$addr/v1/stream" --pin "$spki" --token-file "$tmp/token-1" --max-seconds 90 \
  --ack-notices --withhold-acks 1 > "$tmp/d1.out" 2> "$tmp/d1.err" &
readers+=("$!")
wait_frame "$tmp/d1.out" '.type == "hello"' "step 1: the stream's hello"
issue "$(jq -nc --arg p "$proposal" --arg h "$plan" '{kind: "arm", proposal: $p, plan_sha256: $h}')" demo-arm-code
verb arm "$(jq -nc --arg p "$proposal" --arg h "$plan" --arg k "$code" '{proposal: $p, plan_sha256: $h, code: $k, command_id: "demo-arm-1"}')"
check "$status" 200 "step 1: the arm ($reply)"
run="$(printf '%s' "$reply" | jq -r '.run_id')"
[ -n "$run" ] && [ "$run" != null ] || fail "step 1: the arm named no run: $reply"
check "$("$PF_BIN" status --json | jq -r '.project_id')" "$project" "step 1: the project of the run"
run_dir="$PF_STATE_DIR/runs/$run"
# A project holds one live Orchestrator session, so the first one ends and a second starts. The second
# is the newest, so each notice item lands in it (an item waits until a session exists). It has had no
# turn. The first request opens a few ticks after the arm, after this.
verb session_end "$(jq -nc '{session_id: "S-1", command_id: "demo-end-1"}')"
check "$status" 200 "step 1: session_end ($reply)"
verb session_start "$(jq -nc --arg p "$project" '{project_id: $p, command_id: "demo-start-2"}')"
check "$status" 200 "step 1: the second session_start ($reply)"
check "$(printf '%s' "$reply" | jq -r '.session.id')" S-2 "step 1: the second session"
echo "step 1: controller up on $addr, D-1 paired, project $project with word caps 8 and 10, S-1 and S-2 started, run $run armed from the card"

# ----------------------------------------------------------------------------------- 2
echo "step 2: D-1's stream is open, and it acknowledges each notice but the first frame"

# ----------------------------------------------------------------------------------- 3
stop_id="$(wait_open stop)"
block "$stop_id"
stop_rev="$(printf '%s' "$shown" | jq -r '.revision')"
printf '%s' "$shown" | jq -e --arg p "$project" --arg r "$run" '.decision
  | .cause == "stop_file" and (.summary | length > 0)
    and .refs.project == $p and .refs.run == $r and .refs.unit == "item-1" and .refs.stage == "implement"
    and .asked_by.kind == "seat" and .asked_by.role == "implementer"
    and ([.options[].id] == ["s1", "s2"]) and all(.options[]; .by == "seat")
    and .recommended.option == "s2" and .recommended.by == "seat"' >/dev/null \
  || fail "step 3: the stop's block is not the seat's: $(printf '%s' "$shown" | jq -c '.decision')"
# The seat's words carry the marker word, so the check of step 8 is not vacuous.
printf '%s' "$shown" | grep -qF "$marker" || fail "step 3: the marker word is not in the stop's options"
# The first `opened` notice frame arrives and is not acknowledged. It comes again, as the same position
# with a second send, and the reader acknowledges that one. The acknowledgement is that notice's own.
wait_frame "$tmp/d1.out" '.type == "notice" and .notice.request == "'"$stop_id"'" and .notice.event == "opened" and .notice.sends == 1' \
  "step 3: the first notice frame"
first_position="$(frames "$tmp/d1.out" | jq -r --arg r "$stop_id" 'select(.type == "notice" and .notice.request == $r and .notice.event == "opened") | .notice.position' | head -n 1)"
wait_frame "$tmp/d1.out" '.type == "notice" and .notice.request == "'"$stop_id"'" and .notice.event == "opened" and .notice.sends == 2' \
  "step 3: the same notice again"
wait_line "$controller_ledger" "notice_ack device:D-1 position=$first_position outcome=shown" "step 3: the acknowledgement of the first notice"
take answer "$stop_id" "$stop_rev" s2 demo-answer-s2
echo "step 3: the stop $stop_id names s1 and s2 by the seat, s2 recommended; its notice came twice and was acknowledged once before D-1 took s2"

# ----------------------------------------------------------------------------------- 4
park_id="$(wait_open park)"
block "$park_id"
park_rev="$(printf '%s' "$shown" | jq -r '.revision')"
printf '%s' "$shown" | jq -e '.decision
  | .cause == "no_done_signal" and .asked_by.kind == "controller" and .about.role == "verifier"
    and ([.options[].id] == ["resume"]) and .options[0].by == "controller" and .takes_text == true' >/dev/null \
  || fail "step 4: the park's block is not the controller's: $(printf '%s' "$shown" | jq -c '.decision')"
note="Run the verifier again with care."
issue "$(subject "$park_id" "$park_rev" answer | jq -c --arg h "$(printf '%s' "$note" | sha256sum | cut -d' ' -f1)" '. + {text_sha256: $h}')" demo-note-code
verb answer "$(jq -nc --arg r "$park_id" --argjson v "$park_rev" --arg t "$note" --arg k "$code" \
  '{request: $r, rev: $v, text: $t, code: $k, command_id: "demo-note"}')"
check "$status" 200 "step 4: the typed note ($reply)"
echo "step 4: the park $park_id has the cause no_done_signal and one resume by the controller; D-1 answered with a typed note"

# ----------------------------------------------------------------------------------- 5
red_id="$(wait_open red_gate)"
block "$red_id"
red_rev="$(printf '%s' "$shown" | jq -r '.revision')"
grep -qF "with care" "$run_dir/out/item-1/rulings.md" || fail "step 5: the verifier's note never reached the rulings file"
printf '%s' "$shown" | jq -e '.decision
  | .cause == "park_file" and .asked_by.kind == "seat" and .asked_by.role == "close"
    and ([.options[].id] == ["w1", "w2"]) and all(.options[]; .by == "workflow")
    and .recommended == {option: "w1", by: "workflow", why: "The workflow marked this option."}
    and .takes_text == true' >/dev/null \
  || fail "step 5: the red gate's block is not the workflow's: $(printf '%s' "$shown" | jq -c '.decision')"
take answer "$red_id" "$red_rev" w1 demo-answer-w1
[ ! -e "$run_dir/out/item-1/correction.txt" ] || fail "step 5: the correction packet is still there"
[ -e "$run_dir/out/item-1/correction.txt.answered-$red_id" ] || fail "step 5: the packet was not retired"
last_heading="$(grep '^## ' "$run_dir/out/item-1/rulings.md" | tail -n 1)"
case "$last_heading" in
  *"option w1, written by the workflow") ;;
  *) fail "step 5: the last heading of the rulings file is '$last_heading'" ;;
esac
wait_line "$run_ledger" "launch of stage verify for unit item-1 admitted: role verifier, model paper/verifier, attempt 3" \
  "step 5: the unit runs verify next"
echo "step 5: the red gate $red_id names w1 and w2 by the workflow, w1 recommended; D-1 took w1, the packet is retired"

# ----------------------------------------------------------------------------------- 6
ack_id="$(wait_open merge_ack)"
block "$ack_id"
ack_rev="$(printf '%s' "$shown" | jq -r '.revision')"
printf '%s' "$shown" | jq -e '.decision | ([.options[].id] == ["approve"]) and (has("recommended") | not)' >/dev/null \
  || fail "step 6: the merge ack's block is not a bare approve: $(printf '%s' "$shown" | jq -c '.decision')"
ack_position="$(grep -F "request=$ack_id@" "$controller_ledger" | grep -F 'event=opened' | head -n 1 | sed -n 's/.* notice \([0-9][0-9]*\) request=.*/\1/p')"
[ -n "$ack_position" ] || fail "step 6: no notice line for $ack_id in controller.ledger"
orch="agent:$project:orchestrator"
sends_before="$(grep -cF "notice_send $ack_position target=$orch" "$controller_ledger" || true)"
typed="what is waiting for me?"
verb session_send "$(jq -nc --arg t "$typed" '{session_id: "S-2", text: $t, command_id: "demo-send-2"}')"
check "$status" 200 "step 6: the message to the second session ($reply)"
wait_line "$controller_ledger" "notice_send $ack_position target=$orch attempt=$((sends_before + 1))" "step 6: the carry of the merge ack's notice"
wait_line "$controller_ledger" "notice_ack $orch position=$ack_position outcome=carried" "step 6: the acknowledgement of the carry"
stored="$("$PF_BIN" session items S-2 --json | jq -r '[.. | objects | select(.body? and .body.kind? == "operator_message")][0].body.text')"
check "$stored" "$typed" "step 6: the stored operator message of the second session"
take ack "$ack_id" "$ack_rev" approve demo-ack-approve
check "$(printf '%s' "$reply" | jq -r '.merged')" true "step 6: the merge"
echo "step 6: the merge ack $ack_id holds approve and no recommendation; a message to S-2 carried its notice with no turn of a model; D-1 approved and the spine PR merged"

# ----------------------------------------------------------------------------------- 7
cut=0
for id in "$stop_id" "$park_id" "$red_id" "$ack_id"; do
  text="$(frames "$tmp/d1.out" | jq -r --arg r "$id" 'select(.type == "notice" and .notice.request == $r and .notice.event == "opened") | .notice.text' | head -n 1)"
  [ -n "$text" ] || fail "step 7: no opened notice frame for $id"
  [ "$(words "$text")" -le 8 ] || fail "step 7: the app text of $id holds more than 8 words: $text"
  case "$text" in *…) cut=1 ;; esac
done
items="$("$PF_BIN" session items S-2 --json)"
for id in "$stop_id" "$park_id" "$red_id" "$ack_id"; do
  line="$(printf '%s' "$items" | jq -r --arg r "request $id," '[.. | objects | select(.body? and .body.kind? == "notice" and .body.code? == "request_opened" and (.body.text | contains($r)))][0].body.text // empty')"
  [ -n "$line" ] || fail "step 7: no notice item for $id in S-2"
  head_text="${line%% (request *}"
  [ "$(words "$head_text")" -le 10 ] || fail "step 7: the chat text of $id holds more than 10 words: $head_text"
  case "$head_text" in *…) cut=1 ;; esac
done
check "$cut" 1 "step 7: some notice text is cut and ends with an ellipsis"
check "$(grep -cF "notice_send $first_position target=device:D-1" "$controller_ledger" || true)" 2 "step 7: the sends of the first notice"
check "$(grep -cF "notice_ack device:D-1 position=$first_position " "$controller_ledger" || true)" 1 "step 7: the acknowledgements of the first notice"
echo "step 7: each of the four requests has an app notice of at most 8 words and a chat notice of at most 10; the first was sent twice and acknowledged once"

# ----------------------------------------------------------------------------------- 8
grep -F 'option s2 by seat, recommended' "$run_ledger" >/dev/null || fail "step 8: the run ledger holds no 'option s2 by seat, recommended'"
grep -F 'option w1 by workflow, recommended' "$run_ledger" >/dev/null || fail "step 8: the run ledger holds no 'option w1 by workflow, recommended'"
for want in " option=s2" " option=w1" " option=approve"; do
  grep -qF -- "$want" "$controller_ledger" || fail "step 8: controller.ledger has no line with '$want'"
done
secrets=("$token1")
# The claim token exists only when a CLI claim made one; the device acts here made none.
[ ! -f "$PF_STATE_DIR/claim-token" ] || secrets+=("$(cat "$PF_STATE_DIR/claim-token")")
while IFS= read -r c; do secrets+=("$c"); done < "$confirm_codes"
while IFS= read -r c; do secrets+=("$c"); done < "$pairing_codes"
for secret in "${secrets[@]}"; do
  for file in "$api_ledger" "$controller_ledger" "$run_ledger" "$chat_ledger" "$stderr_file"; do
    [ -f "$file" ] || continue
    if grep -qF -- "$secret" "$file"; then fail "step 8: a confirm code, a pairing code, the claim token or the device token is in $(basename "$file")"; fi
  done
done
# The marker word is in the seat's stop and its options, and in no notice.
if frames "$tmp/d1.out" | jq -r 'select(.type == "notice") | .notice.text' | grep -qF "$marker"; then
  fail "step 8: the marker word is in a notice frame"
fi
if printf '%s' "$items" | jq -r '.. | objects | select(.body? and .body.kind? == "notice") | .body.text' | grep -qF "$marker"; then
  fail "step 8: the marker word is in a notice item"
fi
if grep -hE '(^| )notice(_send|_ack|_superseded)? ' "$controller_ledger" | grep -qF "$marker"; then
  fail "step 8: the marker word is in a ledger line of a notice"
fi
echo "step 8: the ledgers hold each option act; no code or token is in any ledger; the marker word is in no notice"

echo "needs-you-options: ok ($(( $(date +%s) - started )) s)"
