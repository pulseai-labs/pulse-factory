#!/usr/bin/env bash
# Golden journey B (d4): the controller runs a one-round, one-item spine in the paper env, and the
# operator's claim answers a gap and acks the spine PR. It spends nothing, reaches no network and
# needs no model. It does not build: `PF_BIN` is a built binary (default `pulse-factory` on PATH).
# `jq` and `git` must be on PATH.
#
#   PF_BIN=target/debug/pulse-factory scripts/demo/golden-journey-b.sh
#
# The journey:
#   1. arm with `--param base_branch=main`; the paper implementer writes `gaps.json`;
#   2. claim, show and answer the `gaps` request; the item runs again and reads the answer in the
#      handoff's `## Clarifications`;
#   3. the paper reviewer passes; the item merges into the spine branch;
#   4. the paper spine-close seat writes the spine PR into `main`; a `merge_ack` request opens;
#   5. a new commit lands on the spine branch; `ack` at the old revision is refused and names
#      revision 2 and the new SHA; `show`, then `ack` at revision 2 merges the PR into `main`;
#   6. the ledger holds the answer line, the ack line and the merge line; the run waits for its
#      `end_when` file, then ends with the paper reason; each session's cost is fixed.
#
# `PAPER_SRC` names a copy of `scripts/demo/paper-b` to run instead. It exits non-zero on any failed
# assertion, and it removes its temp directory and stops its controller on exit.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PF_BIN="${PF_BIN:-pulse-factory}"
src="${PAPER_SRC:-$here/paper-b}"
shipped_scripts="$here/../../workflows/ossify-spine/scripts"
limit_seconds=120

fail() { echo "golden-journey-b: FAIL: $*" >&2; exit 1; }
for tool in jq git; do command -v "$tool" >/dev/null || fail "$tool is not on PATH"; done
command -v "$PF_BIN" >/dev/null || [ -x "$PF_BIN" ] || fail "PF_BIN=$PF_BIN is not an executable"
[ -f "$shipped_scripts/clarify.sh" ] || fail "no clarify.sh at $shipped_scripts"

tmp="$(mktemp -d)"
controller_pid=""
cleanup() {
  if [ -n "$controller_pid" ]; then
    kill "$controller_pid" 2>/dev/null || true
    wait "$controller_pid" 2>/dev/null || true
  fi
  rm -rf "$tmp"
}
trap cleanup EXIT

export PF_STATE_DIR="$tmp/state" PF_SHARE_DIR="$tmp/share"
# Git must not read the host's configuration, hooks or credentials.
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 GIT_TERMINAL_PROMPT=0
mkdir -p "$PF_SHARE_DIR"
paper="$tmp/paper"
spine_dir="$tmp/spine"
mkdir -p "$paper" "$spine_dir"
cp -R "$src/workflow" "$paper/workflow"
cp -R "$src/seats" "$paper/seats"
chmod +x "$paper"/seats/*.sh
# The answer hook is the shipped one: the journey runs the package's own `clarify.sh`.
mkdir -p "$paper/workflow/scripts"
cp "$shipped_scripts/clarify.sh" "$paper/workflow/scripts/clarify.sh"

# The paper forge: a bare repo with `main` and the spine branch, which starts at `main`.
base="main"
branch="spine-g2"
spine="demo-g2"
forge="$paper/forge.git"
git init -q --bare -b "$base" "$forge"
git init -q -b "$base" "$tmp/seed"
echo "base" > "$tmp/seed/README.md"
git -C "$tmp/seed" add README.md
git -C "$tmp/seed" -c user.name=paper -c user.email=paper@example.invalid commit -q -m "base root"
git -C "$tmp/seed" push -q "$forge" "$base"
git -C "$tmp/seed" push -q "$forge" "$base:refs/heads/$branch"

"$PF_BIN" controller --tick-ms 200 > "$tmp/controller.log" 2>&1 &
controller_pid=$!
journey_start=$(date +%s)
until [ -S "$PF_STATE_DIR/controller.sock" ]; do
  kill -0 "$controller_pid" 2>/dev/null || fail "the controller exited: $(cat "$tmp/controller.log")"
  [ $(( $(date +%s) - journey_start )) -lt 20 ] || fail "the controller made no socket in 20 s"
  sleep 0.1
done

ledger="$tmp/ledger.md"
: > "$ledger"
# 1. Arm. The summary names the spine PR's merge.
arm_out="$("$PF_BIN" arm "$spine" --workflow "$paper/workflow" --ledger "$ledger" --paper "$paper" \
  --param "spine_branch=$branch" --param "spine_dir=$spine_dir" --param "worktree_root=$tmp/worktrees" \
  --param "base_branch=$base" --yes)" || fail "arm refused"
echo "$arm_out"
run="$(printf '%s\n' "$arm_out" | sed -n 's/^run: //p' | head -n 1)"
[ -n "$run" ] || fail "arm printed no run ID"

# The whole journey has one wall-time limit. A halted run, or a parked unit that waits for anything
# but the claim in this journey, cannot finish: fail at once with the status.
status=""
waiting_for=""
refresh() {
  status="$("$PF_BIN" status "$run" --json)" || fail "status failed"
  local stuck
  stuck="$(printf '%s' "$status" | jq -r '[(.units[] | select(.status == "parked" and (.reason | test("gaps surfaced for item-1|waits on the claim.s ack") | not)) | .id + ": " + .reason), (.halted_reason // empty | "run halted: " + .)] | join("; ")')"
  [ -z "$stuck" ] || fail "the run is stuck: $stuck"
  [ $(( $(date +%s) - journey_start )) -lt "$limit_seconds" ] || {
    printf '%s\n' "$status" | jq '{state, halted_reason, notes, units}' >&2
    fail "the journey did not finish within $limit_seconds s (waiting for: $waiting_for)"
  }
}
# wait_for <what> <jq filter of the status that prints true>
wait_for() {
  waiting_for="$1"
  while :; do
    refresh
    [ "$(printf '%s' "$status" | jq -r "$2")" = true ] && return 0
    sleep 0.3
  done
}
# open_request <kind> <unit>: wait for an open request, print its ID.
open_request() {
  waiting_for="an open $1 request of $2"
  local id
  while :; do
    refresh
    id="$("$PF_BIN" requests --run "$run" --json \
      | jq -r --arg k "$1" --arg u "$2" '[.requests[] | select(.kind == $k and .unit == $u and .status == "open")][0].id // empty')"
    [ -z "$id" ] || { printf '%s' "$id"; return 0; }
    sleep 0.3
  done
}

# 2. The paper implementer wrote `gaps.json`; the item parks with a `gaps` request.
gaps_id="$(open_request gaps item-1)"
echo "gaps request: $gaps_id"
claim_json="$("$PF_BIN" claim --run "$run" --holder "journey-b" --json)" || fail "claim refused"
gen="$(printf '%s' "$claim_json" | jq -r '.generation')"
[ "$gen" = 1 ] || fail "the first claim has generation $gen, expected 1"
gaps_show="$("$PF_BIN" show "$gaps_id" --json)" || fail "show $gaps_id failed"
printf '%s' "$gaps_show" | jq -e '.body.gaps[0].question | contains("Which word")' >/dev/null \
  || fail "show $gaps_id does not hold the gap the seat wrote: $gaps_show"
rev="$(printf '%s' "$gaps_show" | jq -r '.revision')"
answer_text="bonjour"
"$PF_BIN" answer "$gaps_id" --rev "$rev" --text "$answer_text" >/dev/null || fail "answer $gaps_id refused"

# The item runs again, and its implementer reads the answer in the handoff.
handoff="$spine_dir/work-item-1/handoff.md"
wait_for "the item to merge into the spine branch" \
  '[.units[] | select(.id == "item-1" and .status == "done")] | length == 1'
grep -q "^> → $answer_text\$" "$handoff" || fail "the handoff holds no answer under ## Clarifications"
[ "$(git -C "$forge" show "$branch:item-1.txt")" = "$answer_text" ] \
  || fail "the item's file on $branch does not hold the answer"
merges="$(git -C "$forge" rev-list --first-parent --merges "$branch" | wc -l)"
[ "$merges" = 1 ] || fail "the spine branch has $merges merge commits, expected 1"

# 4. The spine-close seat wrote the PR into the base branch; the merge waits on the claim.
ack_id="$(open_request merge_ack spine)"
echo "merge_ack request: $ack_id"
tip="$(git -C "$forge" rev-parse "refs/heads/$branch")"
ack_show="$("$PF_BIN" show "$ack_id" --json)" || fail "show $ack_id failed"
[ "$(printf '%s' "$ack_show" | jq -r '.revision')" = 1 ] || fail "the merge_ack request is not at revision 1"
for field in "head_sha=$tip" "base=$base" "head_branch=$branch"; do
  [ "$(printf '%s' "$ack_show" | jq -r --arg k "${field%%=*}" '.body[$k]')" = "${field#*=}" ] \
    || fail "the merge_ack body lacks $field: $ack_show"
done
printf '%s' "$ack_show" | jq -e '.body.pr | endswith("#100")' >/dev/null || fail "the merge_ack body names no PR #100"
printf '%s' "$ack_show" | jq -e '.verbs == ["ack"]' >/dev/null || fail "show lists verbs other than ack: $ack_show"
ack_text="$("$PF_BIN" show "$ack_id")" || fail "show $ack_id (text) failed"
for want in "$tip" "$base" "$branch"; do
  printf '%s\n' "$ack_text" | grep -qF -- "$want" || fail "show names no $want: $ack_text"
done
# Nothing has merged yet: the base branch holds only its root.
[ "$(git -C "$forge" rev-list --count "$base")" = 1 ] || fail "$base moved before the ack"
# The kill switch of the claim: `answer` does not take a merge_ack request.
if "$PF_BIN" answer "$ack_id" --rev 1 --text nope >/dev/null 2>"$tmp/answer.err"; then
  fail "answer took a merge_ack request"
fi
grep -q "ack" "$tmp/answer.err" || fail "the answer refusal does not name ack: $(cat "$tmp/answer.err")"

# 5. The spine branch moves. The old revision is refused with the new revision and SHA.
git clone -q --branch "$branch" "$forge" "$tmp/late"
echo "late" > "$tmp/late/late.txt"
git -C "$tmp/late" add late.txt
git -C "$tmp/late" -c user.name=operator -c user.email=operator@example.invalid commit -q -m "late fix"
git -C "$tmp/late" push -q origin "$branch"
new_tip="$(git -C "$forge" rev-parse "refs/heads/$branch")"
[ "$new_tip" != "$tip" ] || fail "the late commit did not move $branch"
if "$PF_BIN" ack "$ack_id" --rev 1 >"$tmp/ack.out" 2>"$tmp/ack.err"; then
  fail "ack at the old revision was accepted"
fi
grep -q "revision 2" "$tmp/ack.err" && grep -qF "$new_tip" "$tmp/ack.err" \
  || fail "the refusal does not name revision 2 and $new_tip: $(cat "$tmp/ack.err")"
[ "$(git -C "$forge" rev-list --count "$base")" = 1 ] || fail "$base moved on a refused ack"
ack_show="$("$PF_BIN" show "$ack_id" --json)" || fail "show $ack_id failed"
[ "$(printf '%s' "$ack_show" | jq -r '.revision')" = 2 ] || fail "the request is not at revision 2"
[ "$(printf '%s' "$ack_show" | jq -r '.body.head_sha')" = "$new_tip" ] || fail "the request does not hold the new SHA"
"$PF_BIN" ack "$ack_id" --rev 2 >"$tmp/ack2.out" || fail "ack at revision 2 refused"
cat "$tmp/ack2.out"

# The PR merged into `main`: it holds the item's file and the late commit.
[ "$(git -C "$forge" show "$base:item-1.txt")" = "$answer_text" ] || fail "$base lacks the item's file"
[ "$(git -C "$forge" show "$base:late.txt")" = "late" ] || fail "$base lacks the late commit"
[ "$(git -C "$forge" rev-parse "$base^2")" = "$new_tip" ] || fail "$base did not merge the acked SHA $new_tip"

# Ledger: the answer line, the ack line, and the merge line.
line_of() { grep -n -- "$1" "$ledger" | head -n 1 | cut -d: -f1; }
[ -n "$(line_of "answer $gaps_id@$rev by claim $gen: $answer_text")" ] \
  || fail "no ledger line for the answer under claim $gen"
[ -n "$(line_of "ack $ack_id@2 by claim $gen: verb=ack unit=spine pr=.*#100 base=$base branch=$branch sha=$new_tip")" ] \
  || fail "no ledger line for the ack of $ack_id@2 with $new_tip"
[ -n "$(line_of "ack $ack_id@1 by claim $gen refused: ")" ] || fail "no ledger line for the refused ack"
[ -n "$(line_of "merge of unit spine: .*branch=$branch sha=$new_tip outcome=landed")" ] \
  || fail "no ledger line for the merge of the spine PR"
[ "$(line_of "ack $ack_id@2 by claim")" -lt "$(line_of "merge of unit spine: .*outcome=landed")" ] \
  || fail "the ledger shows the merge before the ack"

# The claim token reaches no ledger line, no status and no request.
token="$(cat "$PF_STATE_DIR/claim-token")"
[ -n "$token" ] || fail "the claim token file is empty"
! grep -qF -- "$token" "$ledger" || fail "the claim token is in the ledger"
refresh
! printf '%s' "$status" | grep -qF -- "$token" || fail "the claim token is in the status"
! "$PF_BIN" requests --run "$run" --all --json | grep -qF -- "$token" || fail "the claim token is in a request"

# The run stays active with the waiting note until the end_when file exists.
wait_for "the waiting note" '(.notes | tostring | contains("the spine PR is merged"))'
[ "$(printf '%s' "$status" | jq -r '.state')" != ended ] || fail "the run ended before its end_when file"
[ ! -e "$spine_dir/closed.flag" ] || fail "closed.flag exists before the journey wrote it"
grep -q '^spine$' "$(find "$PF_STATE_DIR" -name after_merge.log | head -n 1)" \
  || fail "the after_merge command did not run for the spine unit"
: > "$spine_dir/closed.flag"
wait_for "the run to end" '.state == "ended"'
reason="$(printf '%s' "$status" | jq -r '.end_reason')"
[ "$reason" = "paper spine $spine closed" ] || fail "the run ended with '$reason'"
echo "run $run ended: $reason"

# Every unit is done, and no request is open.
bad="$(printf '%s' "$status" | jq -r '[.units[] | select(.status != "done")] | map(.id + "=" + .status) | join(",")')"
[ -z "$bad" ] || fail "units not done: $bad"
[ "$(printf '%s' "$status" | jq '[.units[] | select(.track == "item")] | length')" = 1 ] || fail "expected one item"
[ "$("$PF_BIN" requests --run "$run" --json | jq '.open')" = 0 ] || fail "a request is still open"

# Each session's cost is the fixed number, and the total is the fixed sum.
check_cost() { # role expected_micros expected_count
  local got want
  got="$(printf '%s' "$status" | jq -c --arg r "$1" '[.sessions[] | select(.role == $r) | .cost_micros]')"
  want="$(jq -nc --argjson m "$2" --argjson c "$3" '[range($c)] | map($m)')"
  [ "$got" = "$want" ] || fail "role $1 cost $got, expected $want"
}
check_cost lane 1000 1
check_cost implementer 2000 2
check_cost verifier 500 1
check_cost close 1500 1
check_cost reviewer 800 1
check_cost spine_close 1200 1
total="$(printf '%s' "$status" | jq '.total.cost_micros')"
[ "$total" = 9000 ] || fail "the run total was $total, expected 9000"

[ -z "${DEMO_SHOW_LEDGER:-}" ] || cat "$ledger"
echo "golden-journey-b: ok (gap answered, spine PR acked at revision 2 and merged into $base, total cost 9000 micro-dollars)"
