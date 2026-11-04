#!/bin/sh
# Paper lane: clone one item worktree from the bare repo, write the item's handoff, publish one queue
# record, and write the done files. Fixed usage: 1000 millionths of a dollar.
set -eu
echo '{"usage":{"input_tokens":100,"output_tokens":10,"cache_read_tokens":0,"cache_write_tokens":0,"cost_micros":1000}}' >> "$PF_PAPER_EVENTS"
mkdir -p "$PAPER_QUEUE" "$PAPER_WORKTREE_ROOT"
id="item-1"
wt="$PAPER_WORKTREE_ROOT/$id"
git clone -q --branch "$PAPER_SPINE_BRANCH" "$PAPER_FORGE" "$wt"
git -C "$wt" checkout -q -b "work/$id"
handoff="$PAPER_SPINE_DIR/work-$id/handoff.md"
mkdir -p "$(dirname "$handoff")"
printf '# Handoff of %s\n\nWrite the greeting file of the item.\n\n## Clarifications\n' "$id" > "$handoff"
tmp="$PAPER_QUEUE/.$id.json.tmp"
printf '{"version":1,"id":"%s","group":"g1","order":1,"vars":{"worktree_path":"%s","branch":"work/%s","handoff_path":"%s"}}\n' \
  "$id" "$wt" "$id" "$handoff" > "$tmp"
mv "$tmp" "$PAPER_QUEUE/$id.json"
: > "$PAPER_RUN_DIR/rounds.done"
: > "$PAPER_LANE_DONE"
echo '{"turn_end":true}' >> "$PF_PAPER_EVENTS"
