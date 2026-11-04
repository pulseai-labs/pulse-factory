#!/bin/sh
# Paper close, as ossify's PR-mode item close is under the seat's rules: it needs the spine branch
# on `origin` and may push only its own branch. When `origin` lacks the spine branch it writes its
# stop file and ends. Otherwise it pushes the item's branch, writes the PR file with the spine
# branch as base, and writes the PR reference. Fixed usage: 1500 millionths of a dollar.
set -eu
echo '{"usage":{"input_tokens":150,"output_tokens":15,"cache_read_tokens":0,"cache_write_tokens":0,"cost_micros":1500}}' >> "$PF_PAPER_EVENTS"
if ! git ls-remote --exit-code origin "refs/heads/$PAPER_BASE" >/dev/null 2>&1; then
  echo "spine branch not on origin" > "$PAPER_STOP"
  echo '{"turn_end":true}' >> "$PF_PAPER_EVENTS"
  exit 0
fi
forge="$(cd "$PAPER_FORGE" && pwd -P)"
n="${PAPER_UNIT#item-}"
git push -q origin "$PAPER_BRANCH"
sha="$(git rev-parse "$PAPER_BRANCH")"
echo "{\"push\":{\"branch\":\"$PAPER_BRANCH\",\"sha\":\"$sha\"}}" >> "$PF_PAPER_EVENTS"
mkdir -p "$(dirname "$forge")/prs"
printf '{"head_branch":"%s","base":"%s","checks":"green"}\n' "$PAPER_BRANCH" "$PAPER_BASE" \
  > "$(dirname "$forge")/prs/$n.json"
printf 'file://%s#%s\n' "$forge" "$n" > "$PAPER_PR_URL"
echo '{"turn_end":true}' >> "$PF_PAPER_EVENTS"
