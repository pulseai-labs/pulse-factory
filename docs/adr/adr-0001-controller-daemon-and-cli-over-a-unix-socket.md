# ADR-0001 — One controller daemon as a systemd user unit, and one CLI over an OS-user Unix socket

- **Status:** Accepted, 2026-11-05, at the Release 0 close, after `scripts/install.sh` ran from a
  clean checkout (see "Verified claims").
- **Date:** 2026-10-25
- **Amended:** 2026-11-07, at `r1.s2.w2`: the controller also serves a network API, as a listener
  beside the socket. The unit gains an optional environment file. Nothing above changes.

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
- The systemd user bus is reachable from an agent's shell when `XDG_RUNTIME_DIR` is set to
  `/run/user/$(id -u)`, which `scripts/install.sh` does when it is unset. The unit was installed and
  ran demo line d3 on 2026-11-02 (`systemctl --user is-active pulse-factory` printed `active`).
- `scripts/install.sh` ran from a clean clone of `main` at `c92a31a` on draco-desk (2026-11-05, Release
  0 close). It built the release binary in 49 s, installed it, the workflows, the skill and the unit,
  and restarted the unit. `systemctl --user is-active pulse-factory` printed `active`, `pulse-factory
  status --json` answered over the socket, and the three `auto:` demo lines passed on the installed
  binary.

### Unverified claims
None.

## Amendment, 2026-11-07 (`r1.s2.w2`)

The second listener that "Consequences" allowed has come (ADR-0012).

- **The API listener lives in the controller.** `pulse-factory controller` serves the network API on
  its own Tokio runtime, beside the Unix socket. It is the same process, so there is still one
  control authority. The listener calls the controller's verb handler, the code that the socket
  runs. The socket stays the CLI's transport, and it stays mode 0600 for the operator's OS user.
- **Off by default.** The listener binds only the address in `--api-listen` or `PF_API_LISTEN`.
  With neither, nothing listens. A bind that cannot happen yet does not stop the controller: it keeps
  serving the socket and tries again.
- **The unit gains `EnvironmentFile=-%h/.config/pulse-factory/controller.env`.** The leading `-`
  makes the file optional. The operator turns the API on by writing `PF_API_LISTEN=<address>:<port>`
  there and restarting the unit (`docs/install.md`).
- **The rejected alternative "an HTTPS API now" is superseded**, for the reason in ADR-0012: the
  run view and the MacBook app need it, and the login, the pinned key and the revocable devices that
  the Release 0 decision waited for exist now.
