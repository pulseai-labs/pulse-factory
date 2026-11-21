#!/bin/sh
# Paper verifier of d32. Its first dispatch ends its turn with no done signal, so the unit parks (the
# cause is `no_done_signal`). Each later dispatch passes only when the rulings file holds the note that
# the operator typed ("with care"), so a note that never reached the seat fails the demo. Fixed usage:
# 500 millionths of a dollar.
set -eu
echo '{"usage":{"input_tokens":50,"output_tokens":5,"cache_read_tokens":0,"cache_write_tokens":0,"cost_micros":500}}' >> "$PF_PAPER_EVENTS"
seen="$(dirname "$PAPER_VERDICT")/verify.dispatched"
if [ ! -e "$seen" ]; then
  : > "$seen"
  echo '{"turn_end":true}' >> "$PF_PAPER_EVENTS"
  exit 0
fi
if ! grep -q 'with care' "$PAPER_RULINGS" 2>/dev/null; then
  echo "paper verifier: the rulings file holds no note" >&2
  exit 1
fi
echo '{"verdict":"pass"}' > "$PAPER_VERDICT"
echo '{"turn_end":true}' >> "$PF_PAPER_EVENTS"
