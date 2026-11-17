#!/bin/sh
# Paper implementer: the report only. A real implementer stages its work and never commits, so the
# worktree holds no commit past the base when the close seat starts. Fixed usage: 2000 millionths
# of a dollar.
set -eu
echo '{"usage":{"input_tokens":200,"output_tokens":20,"cache_read_tokens":50,"cache_write_tokens":5,"cost_micros":2000}}' >> "$PF_PAPER_EVENTS"
mkdir -p "$PAPER_REPORT_DIR"
echo "# Report of $PAPER_UNIT" > "$PAPER_REPORT_DIR/report.md"
echo '{"turn_end":true}' >> "$PF_PAPER_EVENTS"
