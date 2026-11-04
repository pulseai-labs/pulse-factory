#!/bin/sh
# Paper implementer. On its first dispatch the handoff has no clarification: it writes `gaps.json`
# and ends its turn with no report. On the next dispatch it reads the answer in the handoff's
# `## Clarifications`, writes it into the item's file, commits, and writes the report. Fixed usage
# for each dispatch: 2000 millionths of a dollar.
set -eu
echo '{"usage":{"input_tokens":200,"output_tokens":20,"cache_read_tokens":50,"cache_write_tokens":5,"cost_micros":2000}}' >> "$PF_PAPER_EVENTS"
# The answer lines are the quoted lines `> → <text>` that `clarify.sh` appends.
answer="$(sed -n '/^## Clarifications$/,$p' "$PAPER_HANDOFF" | sed -n 's/^> → //p' | tail -n 1)"
if [ -z "$answer" ]; then
  printf '{"gaps":[{"section":"greeting","question":"Which word should %s write?","severity":"blocking"}]}\n' \
    "$PAPER_UNIT" > "$PAPER_GAPS"
  echo '{"turn_end":true}' >> "$PF_PAPER_EVENTS"
  exit 0
fi
echo "$answer" > "$PAPER_UNIT.txt"
git add "$PAPER_UNIT.txt"
git -c user.name=paper -c user.email=paper@example.invalid commit -q -m "$PAPER_UNIT: paper work ($answer)"
mkdir -p "$PAPER_REPORT_DIR"
echo "# Report of $PAPER_UNIT" > "$PAPER_REPORT_DIR/report.md"
echo '{"turn_end":true}' >> "$PF_PAPER_EVENTS"
