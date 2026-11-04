#!/bin/sh
# Paper verifier: a pass verdict. Fixed usage: 500 millionths of a dollar.
set -eu
echo '{"usage":{"input_tokens":50,"output_tokens":5,"cache_read_tokens":0,"cache_write_tokens":0,"cost_micros":500}}' >> "$PF_PAPER_EVENTS"
echo '{"verdict":"pass"}' > "$PAPER_VERDICT"
echo '{"turn_end":true}' >> "$PF_PAPER_EVENTS"
