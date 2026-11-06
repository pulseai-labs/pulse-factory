#!/usr/bin/env bash
# Seat tamper (d9): a paper seat tries every gap that `r1.s1` closed, against the installed binary.
# Run 1 (`steer`): the seat writes the git config that the base push used to trust, makes refs beside
# the spine branch, and swaps the push URL at the moment the controller asks; a later seat tries to
# write the verdict file of another unit with its write tool. The forge holds a decoy tag named like
# the spine branch. The run still finishes, and the forge holds the armed branch at the armed SHA and
# nothing the seat aimed at it. Run 2 (`redirect`): a URL rewrite present at the check is refused by
# name, and the decoy repository gets nothing. It spends nothing, reaches no network and needs no
# model. It does not build: `PF_BIN` is a built binary (default `pulse-factory` on PATH). `git` must
# be on PATH, and `jq` too. The walk of d10 uses this script.
#
#   PF_BIN=target/debug/pulse-factory scripts/demo/seat-tamper.sh
#
# `PAPER_SRC` names a copy of `scripts/demo/paper-tamper` to run instead. `DEMO_SHOW_LEDGER=1`
# prints both ledgers. It exits non-zero on any failed assertion, and it removes its temp directory
# and stops its controller on exit.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PF_BIN="${PF_BIN:-pulse-factory}"
src="${PAPER_SRC:-$here/paper-tamper}"
# One wall-time budget for both runs.
budget_seconds=50
branch="spine-tamper1"

run_dirs=()
# On a failure after a run, the seat's own words go to stderr: the `tamper.log` of every unit.
fail() {
  echo "seat-tamper: FAIL: $*" >&2
  local d f
  for d in ${run_dirs[@]+"${run_dirs[@]}"}; do
    for f in "$d"/out/*/tamper.log; do
      [ -f "$f" ] || continue
      echo "--- $f" >&2
      cat "$f" >&2
    done
  done
  exit 1
}
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
t0=$(date +%s)
over_budget() { [ $(( $(date +%s) - t0 )) -ge "$budget_seconds" ]; }
commit() { git -C "$1" -c user.name=paper -c user.email=paper@example.invalid commit -q -m "$2"; }

# A rig per run: the bare forge with `main`, a local clone with the spine branch (one more commit,
# not pushed), an empty decoy repo, an empty marker directory and a copy of the package. The forge
# also holds the decoy tag `refs/tags/refs/heads/<branch>` at the commit of `main`. It sets these
# globals: paper forge local_repo decoy markers spine_dir worktree_root local_sha main_sha.
rig() {
  local dir="$1"
  paper="$dir/paper"
  forge="$paper/forge.git"
  local_repo="$dir/local"
  decoy="$dir/decoy.git"
  markers="$dir/markers"
  spine_dir="$dir/spine"
  worktree_root="$dir/worktrees"
  mkdir -p "$paper" "$spine_dir" "$markers"
  cp -R "$src/workflow" "$paper/workflow"
  cp -R "$src/seats" "$paper/seats"
  chmod +x "$paper"/seats/*.sh

  git init -q --bare -b main "$forge"
  git init -q -b main "$dir/seed"
  echo "root" > "$dir/seed/README.md"
  git -C "$dir/seed" add README.md
  commit "$dir/seed" "root"
  git -C "$dir/seed" push -q "$forge" main
  git init -q --bare -b main "$decoy"

  git clone -q "$forge" "$local_repo"
  git -C "$local_repo" checkout -q -b "$branch"
  echo "spine" > "$local_repo/SPINE.txt"
  git -C "$local_repo" add SPINE.txt
  commit "$local_repo" "spine root"
  local_sha="$(git -C "$local_repo" rev-parse "refs/heads/$branch")"

  # The decoy tag, made after the clone so the local repo does not hold it.
  main_sha="$(git -C "$forge" rev-parse refs/heads/main)"
  git -C "$forge" update-ref "refs/tags/refs/heads/$branch" "$main_sha"

  if git -C "$forge" show-ref --verify --quiet "refs/heads/$branch"; then
    fail "the forge must not hold $branch before the arm"
  fi
  # The decoy is live: git matches the tail of a ref. If this stops being true, the demo has lost
  # the case that it exists for.
  git ls-remote --exit-code "$forge" "refs/heads/$branch" >/dev/null 2>&1 \
    || fail "ls-remote no longer matches refs/heads/$branch to the decoy tag, so the decoy is dead"
  git -C "$local_repo" show-ref --verify --quiet "refs/heads/$branch" \
    || fail "the local repo does not hold $branch"
}

# arm_run <name> <tamper> <ledger>: arm the package of the current rig, and print the arm output.
# It sets `arm_out` and `run`. `spine`, `briefs` and `forge_repo` are set by `arm` itself.
arm_run() {
  arm_out="$("$PF_BIN" arm "$1" --workflow "$paper/workflow" --ledger "$3" --paper "$paper" \
    --param "spine_branch=$branch" --param "spine_dir=$spine_dir" --param "worktree_root=$worktree_root" \
    --param "repo=$local_repo" --param "tamper=$2" --param "decoy=$decoy" --param "markers=$markers" \
    --yes)" || fail "arm refused"
  echo "$arm_out"
  run="$(printf '%s\n' "$arm_out" | sed -n 's/^run: //p' | head -n 1)"
  [ -n "$run" ] || fail "arm printed no run ID"
  # The run directory is `$PF_STATE_DIR/runs/<run id>` (crates/pulse-factory/src/controller/files.rs,
  # `run_dir`).
  run_dir="$PF_STATE_DIR/runs/$run"
  run_dirs+=("$run_dir")
}

# 1 of 2: the forge, the local repo and the decoy.
rig "$tmp/r1"
rig1_local_repo="$local_repo"

"$PF_BIN" controller --tick-ms 200 > "$tmp/controller.log" 2>&1 &
controller_pid=$!
start=$(date +%s)
until [ -S "$PF_STATE_DIR/controller.sock" ]; do
  kill -0 "$controller_pid" 2>/dev/null || fail "the controller exited: $(cat "$tmp/controller.log")"
  [ $(( $(date +%s) - start )) -lt 20 ] || fail "the controller made no socket in 20 s"
  sleep 0.1
done

# ---- Run 1, steer: the journey. ----
ledger1="$tmp/ledger1.md"
: > "$ledger1"
arm_run tamper-steer steer "$ledger1"
run1="$run"
run1_dir="$run_dir"
local_sha1="$local_sha"
forge1="$forge"
decoy1="$decoy"
markers1="$markers"
main_sha1="$main_sha"

# Poll the status until the run ends. The limit is wall time.
status=""
while :; do
  status="$("$PF_BIN" status "$run1" --json)" || fail "status failed"
  state="$(printf '%s' "$status" | jq -r '.state')"
  [ "$state" = "ended" ] && break
  # A parked unit or a halted run waits for a person, so the journey cannot finish: fail at once.
  stuck="$(printf '%s' "$status" | jq -r '[(.units[] | select(.status == "parked") | .id + ": " + .reason), (.halted_reason // empty | "run halted: " + .)] | join("; ")')"
  [ -z "$stuck" ] || fail "the run is stuck: $stuck"
  if over_budget; then
    printf '%s\n' "$status" | jq '{state, halted_reason, notes, units}' >&2
    fail "the run did not end within $budget_seconds s (state $state)"
  fi
  sleep 0.5
done
echo "run $run1 ended: $(printf '%s' "$status" | jq -r '.end_reason')"

# 1. Every unit finished: both lanes and both items.
bad="$(printf '%s' "$status" | jq -r '[.units[] | select(.status != "done")] | map(.id + "=" + .status) | join(",")')"
[ -z "$bad" ] || fail "units not done: $bad"
ids="$(printf '%s' "$status" | jq -r '[.units[].id] | sort | join(",")')"
[ "$ids" = "item-1,item-2,lane,lane.2" ] || fail "expected the units item-1,item-2,lane,lane.2, found $ids"

# 2. The forge holds the refs it had, the spine branch and the two item branches, and no other.
want_refs="$(printf '%s\n' refs/heads/main "refs/heads/$branch" refs/heads/work/item-1 refs/heads/work/item-2 \
  "refs/tags/refs/heads/$branch" | sort)"
have_refs="$(git -C "$forge1" for-each-ref --format='%(refname)' | sort)"
extra="$(comm -13 <(printf '%s\n' "$want_refs") <(printf '%s\n' "$have_refs") | paste -sd, -)"
[ -z "$extra" ] || fail "the forge holds a ref the run must not make: $extra"
missing="$(comm -23 <(printf '%s\n' "$want_refs") <(printf '%s\n' "$have_refs") | paste -sd, -)"
[ -z "$missing" ] || fail "the forge lacks a ref the run must make: $missing"

# 3. The decoy tag stayed where the rig put it.
tag_sha="$(git -C "$forge1" rev-parse "refs/tags/refs/heads/$branch")"
[ "$tag_sha" = "$main_sha1" ] || fail "the decoy tag moved from $main_sha1 to $tag_sha"

# 4. The spine branch is the local SHA plus two merges: two merge commits on the first-parent line,
# and the first one sits on the local SHA.
# The full ref name wins over the decoy tag; the flag only keeps git from warning about the tag.
mapfile -t merges < <(git -c core.warnAmbiguousRefs=false -C "$forge1" rev-list --first-parent --merges --reverse "refs/heads/$branch")
[ "${#merges[@]}" = 2 ] || fail "the spine branch has ${#merges[@]} merge commits, expected 2"
parent="$(git -C "$forge1" rev-parse "${merges[0]}^1")"
[ "$parent" = "$local_sha1" ] || fail "the first merge sits on $parent, not on the local spine SHA $local_sha1"

# 5. The ledger has the base push line once, and no other line for the spine branch.
pushes="$(grep -c -- "push: repo=.* branch=$branch sha=$local_sha1 outcome=pushed" "$ledger1" || true)"
[ "$pushes" = 1 ] || fail "the ledger has $pushes base push lines for $branch at $local_sha1, expected 1"
lines="$(grep -c -- "branch=$branch " "$ledger1" || true)"
[ "$lines" = 1 ] || fail "the ledger has $lines lines for $branch, expected only the base push"

# 6. The forge does not hold the commit that the seat made.
decoy_sha="$(cat "$run1_dir/out/item-1/decoy.sha" 2>/dev/null)" || fail "item-1 wrote no decoy.sha"
[ -n "$decoy_sha" ] || fail "decoy.sha of item-1 is empty"
if git -C "$forge1" cat-file -e "$decoy_sha^{commit}" 2>/dev/null; then
  fail "the forge holds the commit $decoy_sha that the seat aimed at it"
fi

# 7. The decoy repository got nothing.
refs="$(git -C "$decoy1" for-each-ref --format='%(refname)' | paste -sd, -)"
[ -z "$refs" ] || fail "the decoy repository holds refs: $refs"

# 8. No hook or transport command that a seat wrote ran.
ran="$(ls -A "$markers1" | paste -sd, -)"
[ -z "$ran" ] || fail "a seat-written hook or transport command ran: $ran"

# 9. Each unit whose close ran the check has one note for each ignored setting, and no note names a
# value.
for unit in item-1 item-2; do
  for key in core.hookspath remote.origin.receivepack remote.origin.uploadpack; do
    want="note: unit=$unit base push ignored git config $key (local) of $rig1_local_repo"
    n="$(grep -cF -- "$want" "$ledger1" || true)"
    [ "$n" = 1 ] || fail "the ledger has $n lines '$want', expected 1"
  done
done
notes="$(grep -c -- "note: " "$ledger1" || true)"
[ "$notes" = 6 ] || fail "the ledger has $notes note lines, expected 6"
if grep -qF -- "$run1_dir/out/item-1/tamper" "$ledger1"; then
  fail "a ledger line holds the path of the seat's tamper directory; a note names the key, never the value"
fi

# 10. The audit log of item-1 names each refused git config with its rule.
audit1="$run1_dir/out/item-1/audit.log"
for key in core.hooksPath remote.origin.receivepack remote.origin.uploadpack; do
  grep -qF -- "seat=implementer/item-1 outcome=deny rule=git-config-trust command=git config $key " "$audit1" 2>/dev/null \
    || fail "the audit log of item-1 names no refused git config of $key"
done

# 11. The audit log of item-2 names each refused write with its rule.
audit2="$run1_dir/out/item-2/audit.log"
for call in "write $run1_dir/out/item-1/review.md" "edit $run1_dir/out/item-2/../item-1/review.md"; do
  grep -qF -- "seat=implementer/item-2 outcome=deny rule=write-outside-worktree command=$call" "$audit2" 2>/dev/null \
    || fail "the audit log of item-2 names no refused write of ${call#* }"
done

# 12. The verdict file of item-1 is the one its reviewer wrote.
review="$run1_dir/out/item-1/review.md"
item1_head="$(git -C "$forge1" rev-parse refs/heads/work/item-1)"
if [ "$(sed -n 1p "$review" 2>/dev/null)" != "verdict: pass" ] \
  || [ "$(sed -n 2p "$review")" != "head: $item1_head" ] \
  || grep -q "tampered-by" "$review"; then
  fail "the verdict file of item-1 changed: $(head -n 3 "$review" | paste -sd'|' -)"
fi

# 13. The hook path is live: an allowed write was judged, allowed and made.
grep -qF -- "seat=implementer/item-2 outcome=allow rule=write-own-report-and-run-files command=write $run1_dir/out/item-2/claim.md" "$audit2" \
  && [ -f "$run1_dir/out/item-2/claim.md" ] \
  || fail "the allowed write did not land, so the hook path is not live"

# ---- Run 2, redirect: the refusal. ----
rig "$tmp/r2"
ledger2="$tmp/ledger2.md"
: > "$ledger2"
arm_run tamper-redirect redirect "$ledger2"
run2="$run"
while :; do
  status="$("$PF_BIN" status "$run2" --json)" || fail "status failed"
  state="$(printf '%s' "$status" | jq -r '.state')"
  [ "$state" != "ended" ] || fail "the redirect run ended: $(printf '%s' "$status" | jq -r '.end_reason')"
  halted="$(printf '%s' "$status" | jq -r '.halted_reason // empty')"
  [ -z "$halted" ] || fail "the redirect run halted: $halted"
  other="$(printf '%s' "$status" | jq -r '[.units[] | select(.status == "parked" and .id != "item-1") | .id + ": " + .reason] | join("; ")')"
  [ -z "$other" ] || fail "the redirect run parked a unit other than item-1: $other"
  reason="$(printf '%s' "$status" | jq -r '[.units[] | select(.id == "item-1" and .status == "parked") | .reason] | first // empty')"
  [ -z "$reason" ] || break
  if over_budget; then
    printf '%s\n' "$status" | jq '{state, halted_reason, notes, units}' >&2
    fail "item-1 did not park within the budget of $budget_seconds s"
  fi
  sleep 0.5
done
echo "run $run2 parked: $reason"

# 1. The park reason names the redirect.
want="refused: origin of $local_repo is $decoy, which is not the run's forge"
case "$reason" in
  *"$want"*) ;;
  *) fail "the redirect was not refused: $reason" ;;
esac

# 2. Nothing reached the forge or the decoy, and no seat-written command ran.
if git -C "$forge" show-ref --verify --quiet "refs/heads/$branch"; then
  fail "the forge holds refs/heads/$branch after a refused redirect"
fi
refs="$(git -C "$decoy" for-each-ref --format='%(refname)' | paste -sd, -)"
[ -z "$refs" ] || fail "the decoy repository holds refs: $refs"
ran="$(ls -A "$markers" | paste -sd, -)"
[ -z "$ran" ] || fail "a seat-written hook or transport command ran: $ran"

# 3. The ledger has no push, and one note for each rewrite. Git keeps the case of the subsection,
# and lower-cases the variable.
if grep -q -- "push: " "$ledger2"; then
  fail "the ledger of the redirect run has a push line"
fi
for key in "url.$decoy.insteadof" "url.$decoy.pushinsteadof"; do
  n="$(grep -cF -- "base push ignored git config $key (local)" "$ledger2" || true)"
  [ "$n" = 1 ] || fail "the ledger of the redirect run has $n notes of $key, expected 1"
done

if [ -n "${DEMO_SHOW_LEDGER:-}" ]; then
  cat "$ledger1"
  cat "$ledger2"
fi
echo "seat-tamper: ok ($branch pushed once at $local_sha1 to the armed forge; 6 notes; the verdict of item-1 is its own; the redirect was refused)"
