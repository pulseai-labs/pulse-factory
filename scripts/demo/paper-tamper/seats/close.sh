#!/bin/sh
# Paper close, as ossify's PR-mode item close is under the seat's rules: it needs the spine branch
# on the forge and may push only its own branch. It checks the spine branch by its full name. A forge
# that holds only the decoy tag `refs/tags/refs/heads/<branch>` answers `ls-remote refs/heads/<branch>`
# with that tag, because git matches the tail of a ref, so the branch counts only when the second
# field of a line is exactly `refs/heads/<branch>`. When it is not there the seat writes its stop file
# and ends. Otherwise it pushes the item's branch by the forge's path with the hooks off, writes the PR
# file with the spine branch as base, and writes the PR reference. A path names no remote, so
# `remote.origin.*` does not apply, and the hooks are off: a marker file can come only from the
# controller's base push. This push is the seat's own, and it is not under test. Fixed usage: 1500
# millionths of a dollar.
set -eu
echo '{"usage":{"input_tokens":150,"output_tokens":15,"cache_read_tokens":0,"cache_write_tokens":0,"cost_micros":1500}}' >> "$PF_PAPER_EVENTS"
forge="$(cd "$PAPER_FORGE" && pwd -P)"
found="$(git ls-remote "$forge" "refs/heads/$PAPER_BASE" 2>/dev/null | awk -v ref="refs/heads/$PAPER_BASE" '$2 == ref { print $1 }')"
if [ -z "$found" ]; then
  echo "spine branch not on origin" > "$PAPER_STOP"
  echo '{"turn_end":true}' >> "$PF_PAPER_EVENTS"
  exit 0
fi
n="${PAPER_UNIT#item-}"
git -c core.hooksPath=/dev/null push -q "$forge" "$PAPER_BRANCH"
sha="$(git rev-parse "$PAPER_BRANCH")"
echo "{\"push\":{\"branch\":\"$PAPER_BRANCH\",\"sha\":\"$sha\"}}" >> "$PF_PAPER_EVENTS"
mkdir -p "$(dirname "$forge")/prs"
printf '{"head_branch":"%s","base":"%s","checks":"green"}\n' "$PAPER_BRANCH" "$PAPER_BASE" \
  > "$(dirname "$forge")/prs/$n.json"
printf 'file://%s#%s\n' "$forge" "$n" > "$PAPER_PR_URL"
echo '{"turn_end":true}' >> "$PF_PAPER_EVENTS"
