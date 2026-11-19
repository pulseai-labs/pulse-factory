# shellcheck shell=bash
# The host helper of the demo scripts (r2.s2.w3). Source it, and call `stop_hosts` in the exit trap,
# after the controller is stopped and before the temp directory goes:
#
#   . "$here/lib/hosts.sh"
#   cleanup() { ...stop the controller...; stop_hosts; rm -rf "$tmp"; }
#
# A seat lives in a session host that outlives its controller (ADR-0001), so stopping the controller
# ends no seat. `stop_hosts` sends `SIGTERM` to each detached host of this script's own server:
# `$XDG_RUNTIME_DIR/pf-hosts/<server>/<unit>/host.pid` for each `<unit>`, where `<server>` is the name
# in `$PF_STATE_DIR/hosts/server`. A host ends its child's group and exits on `SIGTERM`. It does
# nothing when the state directory has no server name (no seat was started) and never touches a
# host of another server. `host.pid` holds the host's PID and its start time (field 22 of
# `/proc/<pid>/stat`); a PID whose start time differs belongs to another process now and is skipped.

stop_hosts() {
  local server pidfile pid start now
  [ -n "${PF_STATE_DIR:-}" ] && [ -n "${XDG_RUNTIME_DIR:-}" ] || return 0
  [ -r "$PF_STATE_DIR/hosts/server" ] || return 0
  server="$(cat "$PF_STATE_DIR/hosts/server")"
  [ -n "$server" ] || return 0
  for pidfile in "$XDG_RUNTIME_DIR/pf-hosts/$server"/*/host.pid; do
    [ -f "$pidfile" ] || continue
    read -r pid start _ < "$pidfile" || continue
    [ -n "$pid" ] && [ -n "$start" ] || continue
    # The command name sits in parentheses and may hold spaces: cut after the last ")".
    now="$(sed 's/.*) //' "/proc/$pid/stat" 2>/dev/null | cut -d' ' -f20)" || now=""
    [ "$now" = "$start" ] || continue
    kill -TERM "$pid" 2>/dev/null || true
  done
}
