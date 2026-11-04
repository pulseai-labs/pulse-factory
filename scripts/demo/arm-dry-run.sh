#!/usr/bin/env bash
# Arm dry run (d2): `arm --dry-run` on the shipped `workflows/ossify-spine`, against a temp state
# file, a stub plugin root, a stub git repo with a GitHub `origin` and a stub `gh` first on PATH.
# It asserts what the summary promises, that the state directory did not change and that nothing
# launched. It needs no controller, spends nothing and reaches no network. It does not build:
# `PF_BIN` is a built binary (default `pulse-factory` on PATH). `jq` and `git` must be on PATH.
#
#   PF_BIN=target/debug/pulse-factory scripts/demo/arm-dry-run.sh
#
# `GH_STUB` names a script to use as `gh` instead of the built-in stub (a test uses a failing one).
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PF_BIN="${PF_BIN:-pulse-factory}"
workflow="${WORKFLOW_DIR:-$here/../../workflows/ossify-spine}"

fail() { echo "arm-dry-run: FAIL: $*" >&2; exit 1; }
for tool in jq git; do command -v "$tool" >/dev/null || fail "$tool is not on PATH"; done
command -v "$PF_BIN" >/dev/null || [ -x "$PF_BIN" ] || fail "PF_BIN=$PF_BIN is not an executable"
[ -f "$workflow/workflow.yaml" ] || fail "no workflow at $workflow"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

export PF_STATE_DIR="$tmp/state" PF_SHARE_DIR="$tmp/share"
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 GIT_TERMINAL_PROMPT=0
mkdir -p "$PF_SHARE_DIR" "$tmp/bin" "$tmp/plugins" "$tmp/spine" "$tmp/method" "$tmp/wt"

# A stub `gh`: it records each call and answers `repo view`. A test can replace it.
calls="$tmp/gh.calls"
if [ -n "${GH_STUB:-}" ]; then
  cp "$GH_STUB" "$tmp/bin/gh"
else
  cat > "$tmp/bin/gh" <<STUB
#!/bin/sh
echo "\$@" >> '$calls'
echo '{"nameWithOwner":"acme/widgets","defaultBranchRef":{"name":"main"}}'
STUB
fi
chmod +x "$tmp/bin/gh"
export PATH="$tmp/bin:$PATH"

# A stub plugin in the layout of a plugin cache, at a version above the workflow's minimum.
plugin="$tmp/plugins/market/ossify/1.30.1"
mkdir -p "$plugin/.claude-plugin"
echo '{"version":"1.30.1"}' > "$plugin/.claude-plugin/plugin.json"
echo "stub" > "$plugin/SKILL.md"

# A stub git repo whose origin is on GitHub, and a state file with three items for the spine.
git init -q -b main "$tmp/repo"
git -C "$tmp/repo" remote add origin https://github.com/acme/widgets.git
cat > "$tmp/state.json" <<JSON
{"work_items":[
  {"spine":"s1","id":"w1"},{"spine":"s1","id":"w2"},{"spine":"s1","id":"w3"},
  {"spine":"other","id":"x1"}
]}
JSON

before="$(find "$tmp" -path "$tmp/bin" -prune -o -path "$tmp/gh.calls" -prune -o -print | sort | md5sum)"
out="$("$PF_BIN" arm s1 --workflow "$workflow" --ledger "$tmp/ledger.md" --dry-run \
  --plugin-root "$tmp/plugins" \
  --param spine_branch=spine/s1 --param "spine_dir=$tmp/spine" --param "method_root=$tmp/method" \
  --param "repo=$tmp/repo" --param "worktree_root=$tmp/wt" --param "state_file=$tmp/state.json" \
  --param oss=oss --param base_branch=main 2>&1)" || { echo "$out" >&2; fail "arm --dry-run exited non-zero"; }
echo "$out"

want() { printf '%s\n' "$out" | grep -qxF -- "$1" || fail "the summary lacks the line: $1"; }
want "spine: s1"
want "items: 3"
want "workflow: ossify-spine (file version 1)"
for role in lane implementer verifier close; do
  want "role $role: runtime omp, model zai/glm-5.3-flash"
done
want "role reviewer: runtime omp, model zai/glm-5.3"
want "caps: model_sessions 2, cold_builds 1"
want "caps: seed lane runs at most 12 times"
want "caps: seed spine runs at most 1 times"
want "caps: loop review -> fix at most 2 times per item unit"
want "caps: loop review -> review at most 2 times per item unit"
want "caps: loop fix -> review at most 2 times per item unit"
want "caps: one relaunch per stage"
# Per item: implement, verify, close and review once (4), `fix` counts 0, and the three loop caps
# 2 + 2 + 2 (6): 10. The spine track has one stage, and its seed runs once: 1.
# 2 x (12 lane launches + 3 items x 10 + 1 spine close)
want "most launches: 86"
want "item PRs merge into spine/s1 on green checks with no ack"
want "the spine PR merges into main only on the claim's ack"
want "base push: before item.close, push spine/s1 to origin when origin lacks it"
printf '%s\n' "$out" | grep -q '^plugin.*ossify.*1\.30\.1' || fail "the summary lacks the pinned plugin"

# Nothing changed: no state directory, no socket, no ledger, and no `gh` call but `repo view`.
[ ! -e "$PF_STATE_DIR" ] || fail "the dry run made the state directory"
[ ! -e "$tmp/ledger.md" ] || fail "the dry run wrote the ledger"
after="$(find "$tmp" -path "$tmp/bin" -prune -o -path "$tmp/gh.calls" -prune -o -print | sort | md5sum)"
[ "$before" = "$after" ] || fail "the dry run changed files under the temp directory"
if [ -e "$calls" ]; then
  bad="$(grep -v '^repo view ' "$calls" || true)"
  [ -z "$bad" ] || fail "gh was called for more than repo view: $bad"
fi

echo "arm-dry-run: ok"
