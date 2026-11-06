#!/bin/sh
# Paper lane: make one item worktree as a linked worktree of the local repo, publish the queue
# record and write the done files. The first lane queues `item-1`. The second (`lane.2`) queues
# `item-2` and writes `rounds.done`. A linked worktree shares the repo's `.git/config` and refs, so a
# setting that an item's seat writes lands where the base push reads. The start point is named by its
# full ref: the tamper seat makes a local tag named like the spine branch, and a short name would
# resolve to the tag. Fixed usage: 1000 millionths of a dollar.
set -eu
echo '{"usage":{"input_tokens":100,"output_tokens":10,"cache_read_tokens":0,"cache_write_tokens":0,"cost_micros":1000}}' >> "$PF_PAPER_EVENTS"
case "$PAPER_UNIT" in
  lane) id=item-1; group=g1; order=1 ;;
  lane.2) id=item-2; group=g2; order=2 ;;
  *) echo "paper lane: unexpected unit $PAPER_UNIT" >&2; exit 1 ;;
esac
mkdir -p "$PAPER_QUEUE" "$PAPER_WORKTREE_ROOT"
wt="$PAPER_WORKTREE_ROOT/$id"
git -C "$PAPER_REPO" worktree add -q -b "work/$id" "$wt" "refs/heads/$PAPER_SPINE_BRANCH"
tmp="$PAPER_QUEUE/.$id.json.tmp"
printf '{"version":1,"id":"%s","group":"%s","order":%s,"vars":{"worktree_path":"%s","branch":"work/%s"}}\n' \
  "$id" "$group" "$order" "$wt" "$id" > "$tmp"
mv "$tmp" "$PAPER_QUEUE/$id.json"
if [ "$id" = item-2 ]; then
  : > "$PAPER_ROUNDS_DONE"
fi
: > "$PAPER_LANE_DONE"
echo '{"turn_end":true}' >> "$PF_PAPER_EVENTS"
