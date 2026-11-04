#!/usr/bin/env bash
# Spine base push (d7): the spine branch exists only in a local repo, and an armed run opens its
# first item PR with no hand push. The item `close` stage names the branch with `base_push`, and
# the controller puts it on the bare forge before the close seat launches. It spends nothing,
# reaches no network and needs no model. It does not build: `PF_BIN` is a built binary (default
# `pulse-factory` on PATH). `git` must be on PATH, and `jq` too.
#
#   PF_BIN=target/debug/pulse-factory scripts/demo/spine-base.sh
#
# `PAPER_SRC` names a copy of `scripts/demo/paper-base` to run instead. It exits non-zero on any
# failed assertion, and it removes its temp directory and stops its controller on exit.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PF_BIN="${PF_BIN:-pulse-factory}"
src="${PAPER_SRC:-$here/paper-base}"
limit_seconds=60

fail() { echo "spine-base: FAIL: $*" >&2; exit 1; }
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

# The paper forge: a bare repo whose HEAD is `main`, with one commit on it. It holds no spine branch.
branch="spine-base1"
git init -q --bare -b main "$paper/forge.git"
git init -q -b main "$tmp/seed"
echo "root" > "$tmp/seed/README.md"
git -C "$tmp/seed" add README.md
git -C "$tmp/seed" -c user.name=paper -c user.email=paper@example.invalid commit -q -m "root"
git -C "$tmp/seed" push -q "$paper/forge.git" main

# The local repo: cloned from the forge, with the spine branch (one more commit) that is NOT pushed.
local_repo="$tmp/local"
git clone -q "$paper/forge.git" "$local_repo"
git -C "$local_repo" checkout -q -b "$branch"
echo "spine" > "$local_repo/SPINE.txt"
git -C "$local_repo" add SPINE.txt
git -C "$local_repo" -c user.name=paper -c user.email=paper@example.invalid commit -q -m "spine root"
local_sha="$(git -C "$local_repo" rev-parse "refs/heads/$branch")"
rc=0
git ls-remote --exit-code "$paper/forge.git" "refs/heads/$branch" >/dev/null 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "the forge must not hold $branch before the arm (ls-remote rc $rc)"

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
arm_out="$("$PF_BIN" arm demo-base --workflow "$paper/workflow" --ledger "$ledger" --paper "$paper" \
  --param "spine_branch=$branch" --param "spine_dir=$spine_dir" --param "worktree_root=$tmp/worktrees" \
  --param "repo=$local_repo" --yes)" || fail "arm refused"
echo "$arm_out"
run="$(printf '%s\n' "$arm_out" | sed -n 's/^run: //p' | head -n 1)"
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

# 1. Every unit finished: the lane and the item.
bad="$(printf '%s' "$status" | jq -r '[.units[] | select(.status != "done")] | map(.id + "=" + .status) | join(",")')"
[ -z "$bad" ] || fail "units not done: $bad"
[ "$(printf '%s' "$status" | jq '[.units[] | select(.track == "item")] | length')" = 1 ] || fail "expected one item"

# 2. The forge holds the spine branch at the local SHA plus the item's merge: one merge commit on
# the first-parent line, whose first parent is the local SHA and whose second parent brings the
# item's file.
mapfile -t merges < <(git -C "$paper/forge.git" rev-list --first-parent --merges --reverse "$branch")
[ "${#merges[@]}" = 1 ] || fail "the spine branch has ${#merges[@]} merge commits, expected 1"
parent="$(git -C "$paper/forge.git" rev-parse "${merges[0]}^1")"
[ "$parent" = "$local_sha" ] || fail "the merge sits on $parent, not on the local spine SHA $local_sha"
git -C "$paper/forge.git" show "${merges[0]}^2:item-1.txt" >/dev/null 2>&1 \
  || fail "the merge on $branch does not bring item-1"

# 3. The ledger has the base push line, and it comes before the item's own push line.
line_of() { grep -n -- "$1" "$ledger" | head -n 1 | cut -d: -f1; }
base_line="$(line_of "push: repo=.* branch=$branch sha=$local_sha outcome=pushed")"
[ -n "$base_line" ] || fail "no ledger line for the base push of $branch at $local_sha"
item_line="$(line_of "push: repo=.* branch=work/item-1 .*outcome=pushed")"
[ -n "$item_line" ] || fail "no ledger line for the push of item-1"
[ "$base_line" -lt "$item_line" ] || fail "the ledger shows the item's push before the base push"
[ "$(grep -c -- "branch=$branch " "$ledger")" = 1 ] \
  || fail "the ledger holds more than one line for $branch"

[ -z "${DEMO_SHOW_LEDGER:-}" ] || cat "$ledger"
echo "spine-base: ok ($branch pushed once by the controller at $local_sha, then the item merged)"
