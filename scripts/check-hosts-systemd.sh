#!/usr/bin/env bash
# The barrier check of the session host's real launcher (r2.s2.w1, SPINE.md Q7, audit 13).
#
# Run it OUTSIDE the seat fence, where a systemd user manager answers, with XDG_RUNTIME_DIR set.
# A baton seat inside `seat-fence` reaches no user bus, so the seat skips the `systemd_live_*` tests
# by name and the orchestrator runs this at each round barrier:
#
#   nice -n 10 ionice -c3 cargo build -p pf-host && scripts/check-hosts-systemd.sh
#
# It builds nothing itself. PF_HOST_BIN names the `pf-host` binary the live tests start as a unit
# (default `target/debug/pf-host`, under CARGO_TARGET_DIR when that is set; pass PF_HOST_BIN when a
# cargo config puts the target elsewhere). It checks that a user manager answers, runs the
# `systemd_live` tests with `--ignored`, then lists the `pf-host-pfcheck*` units and fails if any is
# left. It prints `check-hosts-systemd: ok` and exits 0 only when all of that held.
#
# Exit 1: no user manager, no binary, a failed test or a unit left behind. The message says which.

set -u

fail() {
  echo "check-hosts-systemd: $*" >&2
  exit 1
}

PF_HOST_BIN="${PF_HOST_BIN:-${CARGO_TARGET_DIR:-target}/debug/pf-host}"
[ -x "$PF_HOST_BIN" ] || fail "no pf-host binary at $PF_HOST_BIN (build it: cargo build -p pf-host, or set PF_HOST_BIN)"
# An absolute path: cargo runs a test with the crate's directory as its working directory.
PF_HOST_BIN="$(cd "$(dirname "$PF_HOST_BIN")" && pwd)/$(basename "$PF_HOST_BIN")"
export PF_HOST_BIN

[ -n "${XDG_RUNTIME_DIR:-}" ] || fail "XDG_RUNTIME_DIR is not set (run outside the seat fence, as the user)"
command -v systemctl >/dev/null 2>&1 || fail "no systemctl on PATH"
manager_err="$(systemctl --user show-environment 2>&1 >/dev/null)" ||
  fail "no systemd user manager answers: ${manager_err:-systemctl --user failed} (run outside the seat fence)"

left_before="$(systemctl --user list-units --all --plain --no-legend --no-pager 'pf-host-pfcheck*' 2>/dev/null | awk 'NF { print $1 }')"
[ -z "$left_before" ] || fail "units from an earlier run are still there: $(echo "$left_before" | tr '\n' ' ')(stop them: systemctl --user stop <unit>)"

nice -n 10 ionice -c3 cargo test -p pf-host --test systemd_live -- --ignored ||
  fail "the systemd_live tests failed"

left="$(systemctl --user list-units --all --plain --no-legend --no-pager 'pf-host-pfcheck*' 2>/dev/null | awk 'NF { print $1 }')"
[ -z "$left" ] || fail "units left behind: $(echo "$left" | tr '\n' ' ')"

echo "check-hosts-systemd: ok"
