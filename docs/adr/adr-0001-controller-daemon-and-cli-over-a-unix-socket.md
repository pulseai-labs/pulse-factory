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

## Amendment, 2026-11-16 (Proposed): every session runs in its own session host

**Status: Proposed; ruled by the operator on 2026-11-16.** It takes effect when the code that builds
it lands, and that work's close marks it Accepted. Until then the Decision above holds.

**Why.** A controller restart stops every seat (ADR-0007), so an upgrade or a crash costs running
work. More kinds of session are coming, a terminal among them, and each one launches a process. If each
launches it as a child of the controller, each must later be moved out again.

**Decision.**
- Every session runs in its own **session host**: a run seat, a chat and a terminal alike. A host is
  one small process per session. It starts the session's process with the argv, the working directory
  and the environment it is given. It owns the session's PTY or pipes. It keeps a bounded replay buffer
  of the output that the controller has not acknowledged, and, for a PTY session, the screen model. It
  holds the child's exit status until the controller reads it.
- Each host is its own service-manager job. On Linux it is a transient systemd user unit in a slice of
  its own, outside the controller's cgroup, with `Type=notify` (ready only after its socket is bound)
  and `KillMode=mixed`. An `ExecStopPost=` step writes the exit file, so a host that is killed still
  leaves one. On macOS it is a launchd job in the operator's GUI domain. A restart of the controller's
  unit stops no host. A secret never goes into the unit's properties, which systemd writes to a
  world-readable file: the host reads it from a 0600 file and deletes the file before it starts the
  child.
- The controller is still the one control authority over the store and the sessions. It attaches to
  each host over a Unix socket in the unit's runtime directory (`RuntimeDirectory=`, mode 0700), which
  systemd removes when the host stops. The host accepts a connection only from the controller's
  cgroup, and the controller checks the host's cgroup and the unit's invocation ID. These checks stop
  mistakes, not a hostile process of the same user (ADR-0005, amendment of 2026-11-16). The host
  protocol is versioned. A connection starts with `hello`: the protocol version, the session, the
  incarnation, the unit's invocation ID, the child's PID and the hash of the launch record the host
  started from (ADR-0003). The controller
  supports the current protocol and the one before it. An install that would drop a protocol a
  running host still speaks is refused until those hosts end.
- The host knows no runtime protocol, no workflow and no rule. The adapters stay in the controller and
  read their process through the host (ADR-0011, ADR-0014). An adapter change ships with the
  controller, and a host upgrade stays rare.
- A host whose child ended keeps the exit status and the unread output for a bounded time (a setting).
  Then it writes them to an exit file in the persistent state directory and ends.

**What it replaces.** "Seats run as child processes of the controller, each in its own PTY." The
rejected alternative "one supervised owner process per seat" is now chosen, and widened to every
session.

**Consequences.**
- One more process per session. The existing caps on live sessions bound the number of hosts.
- Each session gets its own cgroup on Linux, which the caller check uses (ADR-0005, amendment of
  2026-11-16).
- The exact launchd form on macOS is open until a test run verifies it.
- ADR-0007 (amendment of 2026-11-16) says what a restart does now.
