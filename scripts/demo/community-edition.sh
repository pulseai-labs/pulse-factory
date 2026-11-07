#!/usr/bin/env bash
# d14: the public repo's CI passed on the checked-out HEAD, and its `Smoke` step ran the built binary.
#
#   scripts/demo/community-edition.sh
#
# It reads HEAD (`git rev-parse HEAD` in the repo that holds this script, or in `PF_REPO_DIR`) and asks
# GitHub once, with `gh run list`, for the `ci` runs of that commit in `PF_CI_REPO` (default
# `pulseai-labs/pulse-factory`). It never waits and never re-runs anything: with no run for HEAD it says
# "push", and with a run that has not finished it says so. A run counts only when it succeeded and its
# job list has a step named `Smoke` with the conclusion `success`.
#
# This is the one demo that needs the network (planning audit C10): `gh` must reach GitHub, and when it
# cannot, `gh` fails and so does this script. It builds nothing, spends nothing and writes nothing.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_dir="${PF_REPO_DIR:-$(cd "$here/../.." && pwd)}"
repo="${PF_CI_REPO:-pulseai-labs/pulse-factory}"

fail() { echo "community-edition: $*" >&2; exit 1; }

for tool in gh jq git; do command -v "$tool" >/dev/null || fail "$tool is not on PATH"; done

sha="$(git -C "$repo_dir" rev-parse HEAD 2>/dev/null)" || fail "cannot read HEAD of $repo_dir"

# One question, no wait. gh lists the newest run first.
list="$(gh run list -R "$repo" --workflow ci --commit "$sha" --json databaseId,status,conclusion 2>&1)" \
  || fail "gh run list failed: $(printf '%s' "$list" | head -c 300)"
runs="$(printf '%s' "$list" | jq 'length' 2>/dev/null)" || fail "gh run list answered with something that is not JSON"

[ "$runs" -gt 0 ] || fail "no ci run for $sha: push"

id="$(printf '%s' "$list" | jq -r '.[0].databaseId')"
status="$(printf '%s' "$list" | jq -r '.[0].status')"
conclusion="$(printf '%s' "$list" | jq -r '.[0].conclusion // ""')"

[ "$status" = "completed" ] || fail "ci run $id for $sha is still running"
[ "$conclusion" = "success" ] || fail "ci run $id for $sha concluded $conclusion"

jobs="$(gh run view "$id" -R "$repo" --json jobs 2>&1)" \
  || fail "gh run view failed: $(printf '%s' "$jobs" | head -c 300)"
green="$(printf '%s' "$jobs" | jq '[.jobs[]?.steps[]? | select(.name == "Smoke" and .conclusion == "success")] | length' 2>/dev/null)" \
  || fail "gh run view answered with something that is not JSON"
[ "$green" -gt 0 ] || fail "ci run $id has no green Smoke step"

echo "community-edition: ok ($sha, run $id)"
