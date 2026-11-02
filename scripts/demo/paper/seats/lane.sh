#!/bin/sh
# Paper lane: clone two item worktrees from the bare repo, publish one queue record for each,
# and write the done files. Fixed usage: 1000 millionths of a dollar.
set -eu
echo '{"usage":{"input_tokens":100,"output_tokens":10,"cache_read_tokens":0,"cache_write_tokens":0,"cost_micros":1000}}' >> "$PF_PAPER_EVENTS"
mkdir -p "$PAPER_QUEUE" "$PAPER_WORKTREE_ROOT"
for n in 1 2; do
  id="item-$n"
  wt="$PAPER_WORKTREE_ROOT/$id"
  git clone -q --branch "$PAPER_SPINE_BRANCH" "$PAPER_FORGE" "$wt"
  git -C "$wt" checkout -q -b "work/$id"
  tmp="$PAPER_QUEUE/.$id.json.tmp"
  printf '{"version":1,"id":"%s","group":"g1","order":%s,"vars":{"worktree_path":"%s","branch":"work/%s"}}\n' \
    "$id" "$n" "$wt" "$id" > "$tmp"
  mv "$tmp" "$PAPER_QUEUE/$id.json"
done
: > "$PAPER_RUN_DIR/rounds.done"
: > "$PAPER_LANE_DONE"
echo '{"turn_end":true}' >> "$PF_PAPER_EVENTS"
