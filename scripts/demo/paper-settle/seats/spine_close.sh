#!/bin/sh
# Paper spine close: the first pass of the spine close. It clones the spine branch, pushes it (the
# bare repo already holds every item), and writes the spine PR into the base branch. It merges
# nothing. The PR number is 100. Fixed usage: 1200 millionths of a dollar.
set -eu
echo '{"usage":{"input_tokens":120,"output_tokens":12,"cache_read_tokens":0,"cache_write_tokens":0,"cost_micros":1200}}' >> "$PF_PAPER_EVENTS"
forge="$(cd "$PAPER_FORGE" && pwd -P)"
git clone -q --branch "$PAPER_BRANCH" "$forge" "$PAPER_CLONE"
git -C "$PAPER_CLONE" push -q origin "$PAPER_BRANCH"
sha="$(git -C "$PAPER_CLONE" rev-parse "$PAPER_BRANCH")"
echo "{\"push\":{\"branch\":\"$PAPER_BRANCH\",\"sha\":\"$sha\"}}" >> "$PF_PAPER_EVENTS"
mkdir -p "$(dirname "$forge")/prs"
printf '{"head_branch":"%s","base":"%s","checks":"green"}\n' "$PAPER_BRANCH" "$PAPER_BASE" \
  > "$(dirname "$forge")/prs/100.json"
printf 'file://%s#100\n' "$forge" > "$PAPER_PR_URL"
echo '{"turn_end":true}' >> "$PF_PAPER_EVENTS"
