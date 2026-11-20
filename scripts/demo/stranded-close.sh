#!/usr/bin/env bash
# Stranded close (r2.s1): a close seat on any model either opens the item PR or leaves a stop
# request that says why. A paper close seat plays the two ways a model ends early, against the
# installed binary and the shipped close stage of `workflows/ossify-spine` (its `no_signal` key and
# its `scripts/stranded.sh`, copied into the paper package).
#
# Run 1 (`nudged`): the seat ends its first turn with no PR, reads the nudge from `PF_PAPER_INBOX`
# and opens the PR. The item is done, the PR merges into the spine branch, and the ledger holds one
# nudge line for the unit.
# Run 2 (`stranded`): the seat stages two files and ends, and ends again after the nudge. The run
# holds one `stop` request for the unit whose text names the item, the branch and both staged files,
# and `stranded.sh` changed nothing in the worktree. An answer resumes the unit: `rulings.md` holds
# the ruling, the close stage runs again, the seat opens the PR and the item is done.
#
# It spends nothing, reaches no network and needs no model. It does not build: `PF_BIN` is a built
# binary (default `pulse-factory` on PATH). `git` must be on PATH, and `jq` too.
#
#   PF_BIN=target/debug/pulse-factory scripts/demo/stranded-close.sh
#
# `PAPER_SRC` names a copy of `scripts/demo/paper-stranded` to run instead, and `STRANDED_SH` a
# script to use as `stranded.sh`. `DEMO_SHOW_LEDGER=1` prints both ledgers. It exits non-zero on any
# failed assertion, and it prints the seat logs of the failing run to stderr. It removes its temp
# directory and stops its controller on exit. One wall-time budget of 60 s covers both runs.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$here/lib/status.sh"
PF_BIN="${PF_BIN:-pulse-factory}"
src="${PAPER_SRC:-$here/paper-stranded}"
package="$here/../../workflows/ossify-spine"
stranded_sh="${STRANDED_SH:-$package/scripts/stranded.sh}"
budget_seconds=60
branch="spine-stranded1"

# The run that a failure names. `phase` is set before each run starts.
phase="setup"
run_dirs=()
# On a failure the seat's own words go to stderr: the `close.log` of every unit, and the ledgers.
fail() {
  echo "stranded-close: FAIL: $phase: $*" >&2
  local d f
  for d in ${run_dirs[@]+"${run_dirs[@]}"}; do
    for f in "$d"/out/*/close.log "$d"/out/*/stop "$d"/out/*/rulings.md "$d"/out/*/paper-notes.log; do
      [ -f "$f" ] || continue
      echo "--- $f" >&2
      cat "$f" >&2
    done
  done
  exit 1
}
for tool in jq git; do command -v "$tool" >/dev/null || fail "$tool is not on PATH"; done
command -v "$PF_BIN" >/dev/null || [ -x "$PF_BIN" ] || fail "PF_BIN=$PF_BIN is not an executable"
[ -f "$stranded_sh" ] || fail "no stranded.sh at $stranded_sh"
[ -f "$package/scripts/rule.sh" ] || fail "no rule.sh in $package/scripts"

tmp="$(mktemp -d)"
controller_pid=""
. "$here/lib/hosts.sh"
cleanup() {
  if [ -n "$controller_pid" ]; then
    kill "$controller_pid" 2>/dev/null || true
    wait "$controller_pid" 2>/dev/null || true
  fi
  stop_hosts
  rm -rf "$tmp"
}
trap cleanup EXIT

export PF_STATE_DIR="$tmp/state" PF_SHARE_DIR="$tmp/share"
# Git must not read the host's configuration, hooks or credentials.
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 GIT_TERMINAL_PROMPT=0
mkdir -p "$PF_SHARE_DIR"
t0=$(date +%s)
over_budget() { [ $(( $(date +%s) - t0 )) -ge "$budget_seconds" ]; }
commit() { git -C "$1" -c user.name=paper -c user.email=paper@example.invalid commit -q -m "$2"; }

# A rig per run: the bare forge with `main`, a local clone with the spine branch (one more commit,
# not pushed: the controller's base push puts it on the forge) and a copy of the paper package with
# the shipped `stranded.sh` and `rule.sh`. It sets: paper forge local_repo spine_dir worktree_root.
rig() {
  local dir="$1"
  paper="$dir/paper"
  forge="$paper/forge.git"
  local_repo="$dir/local"
  spine_dir="$dir/spine"
  worktree_root="$dir/worktrees"
  mkdir -p "$paper" "$spine_dir"
  cp -R "$src/workflow" "$paper/workflow"
  cp -R "$src/seats" "$paper/seats"
  chmod +x "$paper"/seats/*.sh
  mkdir -p "$paper/workflow/scripts"
  cp "$stranded_sh" "$paper/workflow/scripts/stranded.sh"
  cp "$package/scripts/rule.sh" "$paper/workflow/scripts/rule.sh"

  git init -q --bare -b main "$forge"
  git init -q -b main "$dir/seed"
  echo "root" > "$dir/seed/README.md"
  git -C "$dir/seed" add README.md
  commit "$dir/seed" "root"
  git -C "$dir/seed" push -q "$forge" main

  git clone -q "$forge" "$local_repo"
  git -C "$local_repo" checkout -q -b "$branch"
  echo "spine" > "$local_repo/SPINE.txt"
  git -C "$local_repo" add SPINE.txt
  commit "$local_repo" "spine root"
  local_sha="$(git -C "$local_repo" rev-parse "refs/heads/$branch")"
  if git -C "$forge" show-ref --verify --quiet "refs/heads/$branch"; then
    fail "the forge must not hold $branch before the arm"
  fi
}

# arm_run <name> <mode> <ledger>: arm the package of the current rig. It sets `run` and `run_dir`.
arm_run() {
  local arm_out
  arm_out="$("$PF_BIN" arm "$1" --workflow "$paper/workflow" --ledger "$3" --paper "$paper" \
    --param "spine_branch=$branch" --param "spine_dir=$spine_dir" --param "worktree_root=$worktree_root" \
    --param "repo=$local_repo" --param "mode=$2" --yes)" || fail "arm refused"
  echo "$arm_out"
  run="$(printf '%s\n' "$arm_out" | sed -n 's/^run: //p' | head -n 1)"
  [ -n "$run" ] || fail "arm printed no run ID"
  # The run directory is `$PF_STATE_DIR/runs/<run id>` (crates/pulse-factory/src/controller/files.rs,
  # `run_dir`).
  run_dir="$PF_STATE_DIR/runs/$run"
  run_dirs+=("$run_dir")
}

# wait_end <run>: poll the status until the run ends. A parked unit or a halted run waits for a
# person, so the journey cannot finish: fail at once. It sets `status`.
wait_end() {
  while :; do
    status="$(status_json "$1")" || fail "status failed 4 times"
    state="$(printf '%s' "$status" | jq -r '.state')"
    [ "$state" = "ended" ] && break
    local stuck
    stuck="$(printf '%s' "$status" | jq -r '[(.units[] | select(.status == "parked") | .id + ": " + .reason), (.halted_reason // empty | "run halted: " + .)] | join("; ")')"
    [ -z "$stuck" ] || fail "the run is stuck: $stuck"
    if over_budget; then
      printf '%s\n' "$status" | jq '{state, halted_reason, notes, units}' >&2
      fail "the run did not end within $budget_seconds s (state $state)"
    fi
    sleep 0.5
  done
}

# assert_one_merge_with <file>...: the spine branch of the forge is the local SHA plus one merge
# commit on its first-parent line, and the second parent of that merge brings each file.
assert_one_merge_with() {
  local f merges
  mapfile -t merges < <(git -C "$forge" rev-list --first-parent --merges --reverse "$branch")
  [ "${#merges[@]}" = 1 ] || fail "the spine branch has ${#merges[@]} merge commits, expected 1"
  local parent
  parent="$(git -C "$forge" rev-parse "${merges[0]}^1")"
  [ "$parent" = "$local_sha" ] || fail "the merge sits on $parent, not on the local spine SHA $local_sha"
  for f in "$@"; do
    git -C "$forge" show "${merges[0]}^2:$f" >/dev/null 2>&1 || fail "the merge on $branch does not bring $f"
  done
}

nudge_lines() { grep -c -- "item-1.*stage close role .* nudge 1 of 1" "$1" || true; }

"$PF_BIN" controller --tick-ms 200 > "$tmp/controller.log" 2>&1 &
controller_pid=$!
start=$(date +%s)
until [ -S "$PF_STATE_DIR/controller.sock" ]; do
  kill -0 "$controller_pid" 2>/dev/null || fail "the controller exited: $(cat "$tmp/controller.log")"
  [ $(( $(date +%s) - start )) -lt 20 ] || fail "the controller made no socket in 20 s"
  sleep 0.1
done

# ---- Run 1, nudged: the seat finishes after one nudge. ----
phase="run 1 (nudged)"
rig "$tmp/r1"
ledger1="$tmp/ledger1.md"
: > "$ledger1"
arm_run demo-nudged nudged "$ledger1"
run1_dir="$run_dir"
wait_end "$run"
echo "run $run ended: $(printf '%s' "$status" | jq -r '.end_reason')"

# 1. Every unit finished: the lane and the one item.
bad="$(printf '%s' "$status" | jq -r '[.units[] | select(.status != "done")] | map(.id + "=" + .status) | join(",")')"
[ -z "$bad" ] || fail "units not done: $bad"
[ "$(printf '%s' "$status" | jq '[.units[] | select(.track == "item")] | length')" = 1 ] || fail "expected one item"

# 2. The PR merged into the spine branch: one merge commit on the local SHA, and it brings the
# file that the seat committed.
assert_one_merge_with item-1.txt

# 3. The ledger holds exactly one nudge line for the unit, and the seat read the nudge.
n="$(nudge_lines "$ledger1")"
[ "$n" = 1 ] || fail "the ledger has $n nudge lines for item-1, expected 1"
grep -q '^nudge read: .*item-1' "$run1_dir/out/item-1/close.log" || fail "the seat did not read a nudge that names item-1"
[ "$(grep -c '^launch ' "$run1_dir/out/item-1/close.log")" = 1 ] || fail "the close seat launched more than once: a nudge is a turn, not a launch"

# 4. No stop request: the nudge was enough.
open="$("$PF_BIN" requests --run "$run" --json | jq -r '.open')"
[ "$open" = 0 ] || fail "the run holds $open open requests, expected none"
echo "nudged: the seat finished after 1 nudge; the PR merged into $branch"

# ---- Run 2, stranded: the seat ends twice; a stop request says why; an answer resumes the unit. ----
phase="run 2 (stranded)"
rig "$tmp/r2"
ledger2="$tmp/ledger2.md"
: > "$ledger2"
arm_run demo-stranded stranded "$ledger2"
run2="$run"
run2_dir="$run_dir"
wt="$worktree_root/item-1"

# Wait for the unit to park. A run that ends, or a park of another unit, is a failure.
while :; do
  status="$(status_json "$run2")" || fail "status failed 4 times"
  state="$(printf '%s' "$status" | jq -r '.state')"
  [ "$state" != "ended" ] || fail "the run ended: $(printf '%s' "$status" | jq -r '.end_reason')"
  halted="$(printf '%s' "$status" | jq -r '.halted_reason // empty')"
  [ -z "$halted" ] || fail "the run halted: $halted"
  other="$(printf '%s' "$status" | jq -r '[.units[] | select(.status == "parked" and .id != "item-1") | .id + ": " + .reason] | join("; ")')"
  [ -z "$other" ] || fail "a unit other than item-1 parked: $other"
  reason="$(printf '%s' "$status" | jq -r '[.units[] | select(.id == "item-1" and .status == "parked") | .reason] | first // empty')"
  [ -z "$reason" ] || break
  if over_budget; then
    printf '%s\n' "$status" | jq '{state, halted_reason, notes, units}' >&2
    fail "item-1 did not park within the budget of $budget_seconds s"
  fi
  sleep 0.5
done

# 1. One open request, of kind `stop`, for the unit at the close stage.
reqs="$("$PF_BIN" requests --run "$run2" --json)" || fail "requests failed"
[ "$(printf '%s' "$reqs" | jq '.open')" = 1 ] || fail "expected one open request, found: $(printf '%s' "$reqs" | jq -c '[.requests[] | {id, kind, unit, stage, reason}]')"
kind="$(printf '%s' "$reqs" | jq -r '.requests[0].kind')"
[ "$kind" = stop ] || fail "the open request is a $kind (reason: $reason), expected a stop"
[ "$(printf '%s' "$reqs" | jq -r '.requests[0] | .unit + "/" + .stage')" = "item-1/close" ] \
  || fail "the stop request is not for item-1 at the close stage"
req_id="$(printf '%s' "$reqs" | jq -r '.requests[0].id')"
rev="$(printf '%s' "$reqs" | jq -r '.requests[0].revision')"

# 2. Its text names the item, the branch and both staged files, and the next step.
body="$("$PF_BIN" show "$req_id" --json | jq -r '.body.stop_text // empty')" || fail "show failed"
[ -n "$body" ] || fail "the stop request holds no stop_text"
for want in "item-1" "work/item-1" "stranded-a.txt" "stranded-b.txt" "2 files" \
  "the close seat ended before the item PR opened" "answer to run close again"; do
  case "$body" in
    *"$want"*) ;;
    *) fail "the stop text lacks '$want': $body" ;;
  esac
done
echo "stop request $req_id for item-1: $(printf '%s' "$body" | head -n 1)"

# 3. One nudge went out before the stop.
n="$(nudge_lines "$ledger2")"
[ "$n" = 1 ] || fail "the ledger has $n nudge lines for item-1, expected 1"

# 4. `stranded.sh` changed nothing but the stop file: both files are still staged, nothing is
# committed past the base and the forge holds no branch of the item.
staged="$(git -C "$wt" diff --cached --name-only | paste -sd, -)"
[ "$staged" = "stranded-a.txt,stranded-b.txt" ] || fail "the staged files changed: $staged"
[ "$(git -C "$wt" rev-list --count "$local_sha..HEAD")" = 0 ] || fail "a commit exists past the base"
if git -C "$forge" show-ref --verify --quiet "refs/heads/work/item-1"; then
  fail "the forge holds work/item-1 before the answer"
fi

# 5. An answer resumes the unit: the hook writes the ruling, and the close stage runs again.
ruling="open the PR with the two staged files"
"$PF_BIN" claim --run "$run2" --holder "stranded-close" --json >/dev/null || fail "claim refused"
answered="$("$PF_BIN" answer "$req_id" --rev "$rev" --text "$ruling" --json)" || fail "answer $req_id refused"
[ "$(printf '%s' "$answered" | jq -r '.hook')" = passed ] || fail "the answer hook did not pass: $answered"
[ "$(printf '%s' "$answered" | jq -r '.resumed')" = true ] || fail "the answer did not resume the unit: $answered"
wait_end "$run2"
echo "run $run2 ended: $(printf '%s' "$status" | jq -r '.end_reason')"

# 6. The ruling is in `rulings.md`, quoted under the stop text, and the stop file was retired.
rulings="$run2_dir/out/item-1/rulings.md"
grep -qF -- "$ruling" "$rulings" 2>/dev/null || fail "rulings.md does not hold the ruling"
grep -qF -- "> close of item-1 on branch work/item-1" "$rulings" || fail "rulings.md does not quote the stop text"
[ ! -e "$run2_dir/out/item-1/stop" ] || fail "the stop file is still there after the answer"

# 7. The close stage ran twice, the second time with the ruling, and the item is done and merged.
log="$run2_dir/out/item-1/close.log"
[ "$(grep -c '^launch ' "$log")" = 2 ] || fail "the close seat launched $(grep -c '^launch ' "$log") times, expected 2"
grep -q '^launch 2: mode=stranded rulings=yes' "$log" || fail "the second launch did not find the ruling"
bad="$(printf '%s' "$status" | jq -r '[.units[] | select(.status != "done")] | map(.id + "=" + .status) | join(",")')"
[ -z "$bad" ] || fail "units not done: $bad"
assert_one_merge_with stranded-a.txt stranded-b.txt
[ "$("$PF_BIN" requests --run "$run2" --json | jq -r '.open')" = 0 ] || fail "a request is still open after the item merged"
echo "stranded: the stop named item-1, work/item-1 and both staged files; the answer resumed it; the PR merged"

if [ -n "${DEMO_SHOW_LEDGER:-}" ]; then
  cat "$ledger1"
  cat "$ledger2"
fi
echo "stranded-close: ok (nudged: 1 nudge, PR merged; stranded: 1 stop request, answered, PR merged)"
