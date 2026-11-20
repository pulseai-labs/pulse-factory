#!/usr/bin/env bash
# The barrier check of the session host's real launcher (r2.s2.w1, r2.s2.w3, r2.s2.w4; SPINE.md Q7, audit 13).
#
# Run it OUTSIDE the seat fence, where a systemd user manager answers, with XDG_RUNTIME_DIR set.
# A baton seat inside `seat-fence` reaches no user bus, so the seat skips the `systemd_live_*` tests
# by name and the orchestrator runs this at each round barrier:
#
#   nice -n 10 ionice -c3 cargo build -p pf-host && scripts/check-hosts-systemd.sh
#
# It builds no `pf-host` itself. PF_HOST_BIN names the `pf-host` binary the live tests start as a unit
# (default `target/debug/pf-host`, under CARGO_TARGET_DIR when that is set; pass PF_HOST_BIN when a
# cargo config puts the target elsewhere). It checks that a user manager answers, then runs three sets
# of tests with `--ignored`: the host's own (`pf-host --test systemd_live`), the controller's
# (`pulse-factory --test systemd_live_hosts`, where `PF_HOST_LAUNCHER=systemd` and a paper seat run in
# a unit of their own) and the OMP adapter's (`pf-adapter-omp --test systemd_live_omp`, a stub `omp`
# on a terminal in a unit of its own, rebound by a second adapter). Cargo builds the `pulse-factory`
# binary the second set starts. It then lists the units of the test servers (`pf-host-pfcheck*` for
# the first set, `pf-host-c0ffe*` for the second, `pf-host-pfompcheck*` for the third, and the
# controller units `pfc-c0ffe*`) and fails if any is left. It prints
# `check-hosts-systemd: ok` and exits 0 only when all of that held.
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

# The units of the test servers: the host tests' `pfcheck`, the controller tests' `c0ffee` and
# `c0ffed` (a second server in one test), the OMP adapter tests' `pfompcheck`, and the controller
# units `pfc-c0ffee*`.
leftover_units() {
  local pattern
  for pattern in 'pf-host-pfcheck*' 'pf-host-c0ffe*' 'pf-host-pfompcheck*' 'pfc-c0ffe*'; do
    systemctl --user list-units --all --plain --no-legend --no-pager "$pattern" 2>/dev/null | awk 'NF { print $1 }'
  done
}

left_before="$(leftover_units)"
[ -z "$left_before" ] || fail "units from an earlier run are still there: $(echo "$left_before" | tr '\n' ' ')(stop them: systemctl --user stop <unit>)"

nice -n 10 ionice -c3 cargo test -p pf-host --test systemd_live -- --ignored ||
  fail "the pf-host systemd_live tests failed"

nice -n 10 ionice -c3 cargo test -p pulse-factory --test systemd_live_hosts -- --ignored ||
  fail "the systemd_live_hosts tests failed"

nice -n 10 ionice -c3 cargo test -p pf-adapter-omp --test systemd_live_omp -- --ignored ||
  fail "the systemd_live_omp tests failed"

left="$(leftover_units)"
[ -z "$left" ] || fail "units left behind: $(echo "$left" | tr '\n' ' ')"

echo "check-hosts-systemd: ok"
