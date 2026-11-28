#!/bin/sh
# Paper reviewer: it reads the head of the item branch in the bare repo and passes it. The file
# starts with `verdict: pass` and `head: <sha>`. Fixed usage: 800 millionths of a dollar.
set -eu
echo '{"usage":{"input_tokens":80,"output_tokens":8,"cache_read_tokens":0,"cache_write_tokens":0,"cost_micros":800}}' >> "$PF_PAPER_EVENTS"
head="$(git -C "$PAPER_FORGE" rev-parse "refs/heads/$PAPER_BRANCH")"
printf 'verdict: pass\nhead: %s\n\nNo finding.\n' "$head" > "$PAPER_REVIEW"
echo '{"turn_end":true}' >> "$PF_PAPER_EVENTS"
