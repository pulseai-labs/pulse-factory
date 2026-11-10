#!/bin/sh
# Paper implementer of d20. On its first dispatch the unit has no ruling: it writes its stop file
# with one question (a stop-rule question) and ends its turn with no report. On the next dispatch it
# reads the ruling that `rule.sh` appended to the unit's rulings file, writes it into the item's
# file, commits, and writes the report. Fixed usage for each dispatch: 2000 millionths of a dollar.
set -eu
echo '{"usage":{"input_tokens":200,"output_tokens":20,"cache_read_tokens":50,"cache_write_tokens":5,"cost_micros":2000}}' >> "$PF_PAPER_EVENTS"
# `rule.sh` appends a heading, the quoted stop text and the ruling text: the ruling is the last line.
ruling=""
if [ -s "$PAPER_RULINGS" ]; then
  ruling="$(grep -v '^$' "$PAPER_RULINGS" | tail -n 1)"
fi
if [ -z "$ruling" ]; then
  printf 'Which word should %s write: stop or go?\n' "$PAPER_UNIT" > "$PAPER_STOP"
  echo '{"turn_end":true}' >> "$PF_PAPER_EVENTS"
  exit 0
fi
echo "$ruling" > "$PAPER_UNIT.txt"
git add "$PAPER_UNIT.txt"
git -c user.name=paper -c user.email=paper@example.invalid commit -q -m "$PAPER_UNIT: paper work ($ruling)"
mkdir -p "$PAPER_REPORT_DIR"
echo "# Report of $PAPER_UNIT" > "$PAPER_REPORT_DIR/report.md"
echo '{"turn_end":true}' >> "$PF_PAPER_EVENTS"
