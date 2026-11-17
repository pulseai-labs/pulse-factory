#!/bin/sh
# Paper close seat of the stranded-close demo. It plays a model that ends its turn before the item
# PR opens. Each launch writes one line to `close.log`: the launch number, the mode and whether the
# unit's rulings file exists. Fixed usage: 1500 millionths of a dollar.
#
#   mode=nudged    the first turn ends with no PR. The seat then waits for the controller's nudge in
#                  `PF_PAPER_INBOX`, reads it, and finishes: commit, push, PR.
#   mode=stranded  the seat stages two files and ends its turn, waits for the nudge, and ends its
#                  turn again with no PR. After an answer the stage runs again; that launch finds
#                  the rulings file and finishes.
set -eu
echo '{"usage":{"input_tokens":150,"output_tokens":15,"cache_read_tokens":0,"cache_write_tokens":0,"cost_micros":1500}}' >> "$PF_PAPER_EVENTS"
mkdir -p "$(dirname "$PAPER_LOG")"
launch=$(( $(grep -c '^launch ' "$PAPER_LOG" 2>/dev/null || true) + 1 ))
rulings=no
[ ! -s "$PAPER_RULINGS" ] || rulings=yes
echo "launch $launch: mode=$PAPER_MODE rulings=$rulings" >> "$PAPER_LOG"

end_turn() { echo '{"turn_end":true}' >> "$PF_PAPER_EVENTS"; }

# Wait for the message that the controller delivers, and keep its first line in the log.
read_nudge() {
  while [ ! -s "$PF_PAPER_INBOX" ]; do sleep 0.05; done
  echo "nudge read: $(head -n 1 "$PF_PAPER_INBOX" | cut -c 1-120)" >> "$PAPER_LOG"
}

# What a real close does at its end: commit what is staged, push the item's branch, open the PR.
finish() {
  if [ -z "$(git diff --cached --name-only)" ]; then
    echo "work of $PAPER_UNIT" > "$PAPER_UNIT.txt"
    git add "$PAPER_UNIT.txt"
  fi
  git -c user.name=paper -c user.email=paper@example.invalid commit -q -m "$PAPER_UNIT: paper work"
  forge="$(cd "$PAPER_FORGE" && pwd -P)"
  n="${PAPER_UNIT#item-}"
  git push -q origin "$PAPER_BRANCH"
  sha="$(git rev-parse "$PAPER_BRANCH")"
  echo "{\"push\":{\"branch\":\"$PAPER_BRANCH\",\"sha\":\"$sha\"}}" >> "$PF_PAPER_EVENTS"
  mkdir -p "$(dirname "$forge")/prs"
  printf '{"head_branch":"%s","base":"%s","checks":"green"}\n' "$PAPER_BRANCH" "$PAPER_BASE" \
    > "$(dirname "$forge")/prs/$n.json"
  printf 'file://%s#%s\n' "$forge" "$n" > "$PAPER_PR_URL"
  end_turn
}

case "$PAPER_MODE" in
nudged)
  end_turn
  read_nudge
  finish # after the nudge
  ;;
stranded)
  if [ "$rulings" = yes ]; then
    finish # after the ruling
    exit 0
  fi
  echo "alpha" > stranded-a.txt
  echo "beta" > stranded-b.txt
  git add stranded-a.txt stranded-b.txt
  end_turn
  read_nudge
  end_turn # ends again, with no PR
  # The controller stops this session when it parks the unit.
  sleep 60
  ;;
*)
  echo "paper close: unknown mode $PAPER_MODE" >&2
  exit 1
  ;;
esac
