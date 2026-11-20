# shellcheck shell=bash
# The status helper of the demo scripts (r2.s2.w3). Source it, with `PF_BIN` set:
#
#   . "$here/lib/status.sh"
#   status="$(status_json "$run")" || fail "status failed 4 times"
#
# `status_json <run>` runs `"$PF_BIN" status <run> --json` and prints the answer. A call that exits
# non-zero is tried again, up to three more times, 0.5 s apart. The fourth failure prints "status
# failed 4 times: <the last stderr>" to stderr and returns 1. The output of a failed call is never
# read as data: a status that fails while the controller starts or restarts can print half an answer,
# and a loop that read it would take an empty unit list for a run with no units.

status_json() {
  local run="$1" attempt out err_file last_err=""
  err_file="$(mktemp)"
  for attempt in 1 2 3 4; do
    if out="$("$PF_BIN" status "$run" --json 2>"$err_file")"; then
      rm -f "$err_file"
      printf '%s\n' "$out"
      return 0
    fi
    last_err="$(cat "$err_file")"
    [ "$attempt" -eq 4 ] || sleep 0.5
  done
  rm -f "$err_file"
  echo "status failed 4 times: $last_err" >&2
  return 1
}
