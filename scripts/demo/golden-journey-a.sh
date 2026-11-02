#!/usr/bin/env bash
# Golden journey A (d1): the controller runs a one-round, two-item spine in the paper env, and each
# item's PR merges into the spine branch with no operator present. It spends nothing, reaches no
# network and needs no model. It does not build: `PF_BIN` is a built binary (default
# `pulse-factory` on PATH). `jq` and `git` must be on PATH.
#
#   PF_BIN=target/debug/pulse-factory scripts/demo/golden-journey-a.sh
#
# `PAPER_SRC` names a copy of `scripts/demo/paper` to run instead. It exits non-zero on any failed
# assertion, and it removes its temp directory and stops its controller on exit.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PF_BIN="${PF_BIN:-pulse-factory}"
src="${PAPER_SRC:-$here/paper}"
limit_seconds=120

fail() { echo "golden-journey-a: FAIL: $*" >&2; exit 1; }
for tool in jq git; do command -v "$tool" >/dev/null || fail "$tool is not on PATH"; done
command -v "$PF_BIN" >/dev/null || [ -x "$PF_BIN" ] || fail "PF_BIN=$PF_BIN is not an executable"

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

# The paper forge: a bare repo whose HEAD is the spine branch, with one commit on it.
branch="spine-g1"
git init -q --bare -b "$branch" "$paper/forge.git"
git init -q -b "$branch" "$tmp/seed"
echo "spine" > "$tmp/seed/README.md"
git -C "$tmp/seed" add README.md
git -C "$tmp/seed" -c user.name=paper -c user.email=paper@example.invalid commit -q -m "spine root"
git -C "$tmp/seed" push -q "$paper/forge.git" "$branch"

"$PF_BIN" controller --tick-ms 200 > "$tmp/controller.log" 2>&1 &
controller_pid=$!
start=$(date +%s)
until [ -S "$PF_STATE_DIR/controller.sock" ]; do
  kill -0 "$controller_pid" 2>/dev/null || fail "the controller exited: $(cat "$tmp/controller.log")"
  [ $(( $(date +%s) - start )) -lt 20 ] || fail "the controller made no socket in 20 s"
  sleep 0.1
done

ledger="$tmp/ledger.md"
: > "$ledger"
run_line="$("$PF_BIN" arm demo-g1 --workflow "$paper/workflow" --ledger "$ledger" --paper "$paper" \
  --param "spine_branch=$branch" --param "spine_dir=$spine_dir" --param "worktree_root=$tmp/worktrees" \
  --yes)" || fail "arm refused"
echo "$run_line"
run="$(printf '%s\n' "$run_line" | sed -n 's/^run: //p' | head -n 1)"
[ -n "$run" ] || fail "arm printed no run ID"

# Poll the status until the run ends. The limit is wall time.
start=$(date +%s)
status=""
while :; do
  status="$("$PF_BIN" status "$run" --json)" || fail "status failed"
  state="$(printf '%s' "$status" | jq -r '.state')"
  [ "$state" = "ended" ] && break
  # A parked unit or a halted run waits for a person, so the journey cannot finish: fail at once.
  stuck="$(printf '%s' "$status" | jq -r '[(.units[] | select(.status == "parked") | .id + ": " + .reason), (.halted_reason // empty | "run halted: " + .)] | join("; ")')"
  [ -z "$stuck" ] || fail "the run is stuck: $stuck"
  [ $(( $(date +%s) - start )) -lt "$limit_seconds" ] || {
    printf '%s\n' "$status" | jq '{state, halted_reason, notes, units}' >&2
    fail "the run did not end within $limit_seconds s (state $state)"
  }
  sleep 0.5
done
echo "run $run ended: $(printf '%s' "$status" | jq -r '.end_reason')"

# 1. Every unit finished: the lane and both items.
bad="$(printf '%s' "$status" | jq -r '[.units[] | select(.status != "done")] | map(.id + "=" + .status) | join(",")')"
[ -z "$bad" ] || fail "units not done: $bad"
[ "$(printf '%s' "$status" | jq '[.units[] | select(.track == "item")] | length')" = 2 ] || fail "expected two items"

# 2. Each item's PR merged into the spine branch, in declared order, as a merge commit in the bare
# repo. The first merge on the first-parent line holds item-1's commit, and the second item-2's.
mapfile -t merges < <(git -C "$paper/forge.git" rev-list --first-parent --merges --reverse "$branch")
[ "${#merges[@]}" = 2 ] || fail "the spine branch has ${#merges[@]} merge commits, expected 2"
for k in 0 1; do
  item="item-$((k + 1))"
  git -C "$paper/forge.git" show "${merges[$k]}^2:$item.txt" >/dev/null 2>&1 \
    || fail "merge $((k + 1)) on $branch does not bring $item"
done

# 3. A ledger line for each push and each merge, and the merges in declared order.
line_of() { grep -n -- "$1" "$ledger" | head -n 1 | cut -d: -f1; }
for item in item-1 item-2; do
  [ -n "$(line_of "push: repo=.*branch=work/$item .*outcome=pushed")" ] \
    || fail "no ledger line for the push of $item"
  [ -n "$(line_of "merge of unit $item: .*outcome=landed")" ] \
    || fail "no ledger line for the merge of $item"
done
[ "$(line_of "merge of unit item-1: .*outcome=landed")" -lt "$(line_of "merge of unit item-2: .*outcome=landed")" ] \
  || fail "the ledger shows item-2 merged before item-1"

# 4. Each session's cost is the fixed number, and the total is the fixed sum.
check_cost() { # role expected_micros expected_count
  local got
  got="$(printf '%s' "$status" | jq -c --arg r "$1" '[.sessions[] | select(.role == $r) | .cost_micros]')"
  local want
  want="$(jq -nc --argjson m "$2" --argjson c "$3" '[range($c)] | map($m)')"
  [ "$got" = "$want" ] || fail "role $1 cost $got, expected $want"
}
check_cost lane 1000 1
check_cost implementer 2000 2
check_cost verifier 500 2
check_cost close 1500 2
total="$(printf '%s' "$status" | jq '.total.cost_micros')"
[ "$total" = 9000 ] || fail "the run total was $total, expected 9000"

[ -z "${DEMO_SHOW_LEDGER:-}" ] || cat "$ledger"
echo "golden-journey-a: ok (2 items merged in order, total cost 9000 micro-dollars)"
