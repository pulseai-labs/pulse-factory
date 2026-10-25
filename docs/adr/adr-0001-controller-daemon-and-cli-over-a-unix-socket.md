# ADR-0001 — One controller daemon as a systemd user unit, and one CLI over an OS-user Unix socket

- **Status:** Proposed
- **Date:** 2026-10-25

## Context
A Pulse Factory run lasts hours after the operator closes the session that armed it. The project
supports one operator on one Linux host, draco-desk, at first. One logical control authority owns the
store and the seats. Release 0 has no network API and no run view. Its surface is the CLI, which any
agent session can run.

## Decision
- One `pulse-factory` binary.
- `pulse-factory controller` runs as a systemd user service (`deploy/systemd/`), with lingering on.
  It outlives the operator's session. It is the one control authority over the store and the seats.
- Every other verb is a CLI client. It talks to the controller over a Unix domain socket in the
  operator's own state directory. Only the operator's OS user can open the socket (mode 0600).
- A session on another machine reaches the CLI over ssh.
- Seats run as child processes of the controller, each in its own PTY.

Rejected alternatives:
- **A detached controller started by `arm`.** No service manager restarts it or shows its state.
- **An HTTPS API now.** Release 0 has no network surface. The HTTPS edge comes with the run view.
- **One supervised owner process per seat.** This comes later, with restart survival.

## Consequences
- One place to look: `systemctl --user status pulse-factory`.
- Seats stop when the controller stops. ADR-0007 covers what start-up does then.
- The HTTPS edge can come later as a second listener. This decision does not block it.

### Verified claims
- `loginctl show-user dev -p Linger` printed `Linger=yes` on draco-desk (2026-10-25).

### Unverified claims
- The systemd user bus was not reachable from the onboarding agent's shell: `systemctl --user` failed
  because `$XDG_RUNTIME_DIR` was not set. Revisit when the skeleton spine installs the unit.
