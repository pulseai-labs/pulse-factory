#!/bin/sh
# Paper close of d32. Its first dispatch writes the correction packet and ends its turn: the unit
# parks on the packet (a red gate). Each later dispatch pushes the item's branch to the bare repo,
# writes the PR file beside it, and writes the PR reference. The PR number is the item's number.
# Fixed usage: 1500 millionths of a dollar.
set -eu
echo '{"usage":{"input_tokens":150,"output_tokens":15,"cache_read_tokens":0,"cache_write_tokens":0,"cost_micros":1500}}' >> "$PF_PAPER_EVENTS"
seen="$(dirname "$PAPER_PR_URL")/close.dispatched"
if [ ! -e "$seen" ]; then
  : > "$seen"
  printf 'The close gate is red: AC-2 exits 1.\n' > "$PAPER_CORRECTION"
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
