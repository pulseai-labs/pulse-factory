#!/bin/sh
# The paper seat of d30 (`scripts/demo/host-restart.sh`). It runs inside a session host.
#
#   $PAPER_OUT/pids    one line per start of this script: its own PID. A seat that a restart
#                      launched again would write a second line
#   $PAPER_OUT/lines   the numbers 1, 2, 3 …, one line every 100 ms, until the gate exists
#   $RESTART_GATE_DIR/$PAPER_UNIT
#                      the gate: when the file exists the seat finishes (`work.done`, then the end
#                      of its turn)
#
# Fixed usage: 1000 millionths of a dollar.
set -eu
echo '{"usage":{"input_tokens":100,"output_tokens":10,"cache_read_tokens":0,"cache_write_tokens":0,"cost_micros":1000}}' >> "$PF_PAPER_EVENTS"
echo "$$" >> "$PAPER_OUT/pids"
n=0
while [ ! -e "$RESTART_GATE_DIR/$PAPER_UNIT" ]; do
  n=$((n + 1))
  echo "$n" >> "$PAPER_OUT/lines"
  sleep 0.1
done
: > "$PAPER_OUT/work.done"
echo '{"turn_end":true}' >> "$PF_PAPER_EVENTS"
