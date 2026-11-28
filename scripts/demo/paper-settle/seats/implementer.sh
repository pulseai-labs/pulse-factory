#!/bin/sh
# Paper implementer of d35. It acts by the unit it runs for:
#   item-1 (the stall)   writes nothing and gives no turn on its first two launches: it counts its
#                        launches in the unit's out directory, and on launch 1 and 2 it waits, so the
#                        controller sees a stall (a relaunch, then a park below the line). On launch 3,
#                        after the Orchestrator settled the park with `clear`, it writes the item's
#                        file, commits, and writes the report.
#   item-2 (the question) writes its stop file with one question and ends its turn: the unit parks
#                        above the line, and nothing answers it.
# Fixed usage for each dispatch that gives a turn: 2000 millionths of a dollar.
set -eu
case "$PAPER_UNIT" in
  item-1)
    count_file="$(dirname "$PAPER_STOP")/launches"
    launches=0
    if [ -s "$count_file" ]; then launches="$(cat "$count_file")"; fi
    launches=$((launches + 1))
    echo "$launches" > "$count_file"
    if [ "$launches" -le 2 ]; then
      exec sleep 3600
    fi
    ruling="go"
    ;;
  *)
    ruling=""
    if [ -s "$PAPER_RULINGS" ]; then
      ruling="$(grep -v '^$' "$PAPER_RULINGS" | tail -n 1)"
    fi
    if [ -z "$ruling" ]; then
      echo '{"usage":{"input_tokens":200,"output_tokens":20,"cache_read_tokens":50,"cache_write_tokens":5,"cost_micros":2000}}' >> "$PF_PAPER_EVENTS"
      printf 'Which word should %s write: stop or go?\n' "$PAPER_UNIT" > "$PAPER_STOP"
      echo '{"turn_end":true}' >> "$PF_PAPER_EVENTS"
      exit 0
    fi
    ;;
esac
echo '{"usage":{"input_tokens":200,"output_tokens":20,"cache_read_tokens":50,"cache_write_tokens":5,"cost_micros":2000}}' >> "$PF_PAPER_EVENTS"
echo "$ruling" > "$PAPER_UNIT.txt"
git add "$PAPER_UNIT.txt"
git -c user.name=paper -c user.email=paper@example.invalid commit -q -m "$PAPER_UNIT: paper work ($ruling)"
mkdir -p "$PAPER_REPORT_DIR"
echo "# Report of $PAPER_UNIT" > "$PAPER_REPORT_DIR/report.md"
echo '{"turn_end":true}' >> "$PF_PAPER_EVENTS"
