#!/bin/sh
# Paper implementer: one commit on the item's branch, then the report. Fixed usage: 2000
# millionths of a dollar.
set -eu
echo '{"usage":{"input_tokens":200,"output_tokens":20,"cache_read_tokens":50,"cache_write_tokens":5,"cost_micros":2000}}' >> "$PF_PAPER_EVENTS"
echo "work of $PAPER_UNIT" > "$PAPER_UNIT.txt"
git add "$PAPER_UNIT.txt"
git -c user.name=paper -c user.email=paper@example.invalid commit -q -m "$PAPER_UNIT: paper work"
mkdir -p "$PAPER_REPORT_DIR"
echo "# Report of $PAPER_UNIT" > "$PAPER_REPORT_DIR/report.md"
echo '{"turn_end":true}' >> "$PF_PAPER_EVENTS"
