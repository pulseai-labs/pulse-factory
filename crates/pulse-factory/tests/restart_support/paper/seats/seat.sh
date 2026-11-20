#!/bin/sh
# The paper seat of the restart tests. It stays alive until the test writes `$PAPER_OUT/go`.
#
#   $PAPER_OUT/seat.pid   the seat's own PID, written once
#   $PAPER_OUT/hook.in    a hook call the test wants made: the seat runs `$PF_HOOK_CMD` on it, with
#                         its own environment (so the controller sees a call from below the seat),
#                         writes the answer to `hook.out` and removes `hook.in`
#   $PAPER_OUT/go         the seat finishes: `work.done`, then the end of the turn
#
# Fixed usage: 1000 millionths of a dollar.
set -eu
echo '{"usage":{"input_tokens":100,"output_tokens":10,"cache_read_tokens":0,"cache_write_tokens":0,"cost_micros":1000}}' >> "$PF_PAPER_EVENTS"
echo "$$" > "$PAPER_OUT/seat.pid"
while [ ! -e "$PAPER_OUT/go" ]; do
  if [ -s "$PAPER_OUT/hook.in" ]; then
    /bin/sh -c "$PF_HOOK_CMD" < "$PAPER_OUT/hook.in" > "$PAPER_OUT/hook.tmp" 2>&1 || true
    rm -f "$PAPER_OUT/hook.out"
    mv "$PAPER_OUT/hook.tmp" "$PAPER_OUT/hook.out"
    rm -f "$PAPER_OUT/hook.in"
  fi
  sleep 0.05
done
: > "$PAPER_OUT/work.done"
echo '{"turn_end":true}' >> "$PF_PAPER_EVENTS"
