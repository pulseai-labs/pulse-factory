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
# in `$PF_STATE_DIR/hosts/server`. A host ends its child's group and exits on `SIGTERM`, but not at
# once, so `stop_hosts` waits for each host it signalled to exit, up to 5 s in total. It then sends
# `SIGKILL` to the process group and to the PID of each host that is still live, and returns only
# when no host it found is live (a zombie is not live). A caller that checks for live hosts right
# after the script exits sees none. It always returns 0, so a `set -e` cleanup trap goes on to
# `rm -rf "$tmp"`: when a host is still live after `SIGKILL` (a process in uninterruptible sleep),
# it prints a warning on stderr that names each such host.
# It does nothing when the state directory has no server name (no seat was started) and never
# touches a host of another server. `host.pid` holds the host's PID and its start time (field 22 of
# `/proc/<pid>/stat`); a PID whose start time differs belongs to another process now and is
# skipped, in the first pass and in every later check.

# Succeed when PID $1 is a live process that started at $2 (a zombie is not live).
_host_is_live() {
  local fields
  # The command name sits in parentheses and may hold spaces: cut after the last ")". Then field 1
  # is the state (field 3 of the file) and field 20 is the start time (field 22 of the file).
  fields="$(sed 's/.*) //' "/proc/$1/stat" 2>/dev/null | cut -d' ' -f1,20)" || return 1
  case "$fields" in
    "Z "* | "X "*) return 1 ;;
  esac
  [ "${fields#* }" = "$2" ]
}

# Wait until no host of the list "$1" is live, or until the epoch time $2 in nanoseconds. The list
# holds words `<pid>:<start time>`, separated by spaces. Succeed when none is live.
_hosts_wait_gone() {
  local host live
  while :; do
    live=0
    for host in $1; do
      _host_is_live "${host%%:*}" "${host#*:}" && live=1
    done
    [ "$live" = 0 ] && return 0
    [ "$(date +%s%N)" -lt "$2" ] || return 1
    sleep 0.05
  done
}

stop_hosts() {
  local server pidfile pid start host hosts="" left=""
  [ -n "${PF_STATE_DIR:-}" ] && [ -n "${XDG_RUNTIME_DIR:-}" ] || return 0
  [ -r "$PF_STATE_DIR/hosts/server" ] || return 0
  server="$(cat "$PF_STATE_DIR/hosts/server")"
  [ -n "$server" ] || return 0
  for pidfile in "$XDG_RUNTIME_DIR/pf-hosts/$server"/*/host.pid; do
    [ -f "$pidfile" ] || continue
    read -r pid start _ < "$pidfile" || continue
    [ -n "$pid" ] && [ -n "$start" ] || continue
    _host_is_live "$pid" "$start" || continue
    kill -TERM "$pid" 2>/dev/null || true
    hosts="$hosts $pid:$start"
  done
  [ -n "$hosts" ] || return 0
  _hosts_wait_gone "$hosts" "$(($(date +%s%N) + 5000000000))" && return 0
  # A host that outlived the wait: kill its group and itself, each only while it is still the
  # process the file named (its start time matches).
  for host in $hosts; do
    _host_is_live "${host%%:*}" "${host#*:}" || continue
    kill -KILL -- "-${host%%:*}" 2>/dev/null || true
    kill -KILL "${host%%:*}" 2>/dev/null || true
    left="$left $host"
  done
  # A killed process is gone at once, or a zombie that its parent has not reaped yet. The short
  # bound stops a process in uninterruptible sleep from holding the script's exit trap.
  _hosts_wait_gone "$left" "$(($(date +%s%N) + 2000000000))" && return 0
  for host in $left; do
    _host_is_live "${host%%:*}" "${host#*:}" \
      && echo "stop_hosts: host pid ${host%%:*} (start time ${host#*:}) is still live after SIGKILL" >&2
  done
  return 0
}
