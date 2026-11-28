# Session host contract

A **session host** is one small process per session. It starts the session's child process on a PTY
or on pipes, keeps the output the controller has not acknowledged, relays input, resize and signals
to the child, holds the child's exit status, and answers one attached controller on a Unix socket.
The `pf-host` crate (`crates/pf-host`) builds it: the `pf-host` binary, the client library the
controller uses, and the two launchers that start a host. ADR-0001's amendment of 2026-11-16 is the
decision behind it. Protocol version 1 is frozen by the golden transcript
(`crates/pf-host/tests/fixtures/v1-transcript.jsonl`, see "Versions").

## What a host is and is not

- A host **is** a supervisor of one child. It starts the child from the argv, the working directory
  and the environment in its launch record file, in its own process group. It reads the child's
  output into a bounded buffer, so a slow reader never blocks the child.
- A host **is not** a controller. It knows no runtime, no workflow and no rule. What the child is,
  what its output means and when it should end are the controller's business. The crate depends on
  no engine, no planning crate and no adapter crate, and its source names no runtime. A test holds
  that (`host_is_dumb.rs`).
- A host **is not** a security boundary against its own user. See "The limits of the checks".
- One host serves one session incarnation. A new incarnation is a new host.

## The launch record file

The host reads one JSON file, given by `--record`. It takes its SHA-256 as the record hash, which
`hello` reports. The controller writes the file from the launch record it holds in the store.

| Field | Type | Meaning |
|---|---|---|
| `argv` | list of strings | The command. At least one entry. `argv[0]` is run as given, with the `PATH` of `env`. |
| `cwd` | string | The working directory. `/` when absent. |
| `env` | object of strings | The whole environment of the child, apart from its secret variables. The host adds nothing else: the child does not inherit the host's environment. |
| `transport` | object | `{"kind":"pipes"}`, or `{"kind":"pty","rows":R,"cols":C}`. The strings `"pipes"` and `"pty"` (24 by 80) are also read. |

On pipes the child's standard input and output are pipes, and its error output goes to the host's
own error output (the unit's journal under systemd, nowhere under the detached launcher). Only the
standard output is the session's output. On a PTY the output is the terminal's, with its own line
endings.

## The arguments of `pf-host run`

`--record`, `--session`, `--incarnation`, `--unit` (names the exit file), `--state-dir`, `--dir` (the
host's directory), `--secrets` (the secrets file path), `--replay-bytes`, `--attach-wait-ms`,
`--exit-hold-ms`, `--launcher` (`systemd` or `detached`), and optionally `--controller-cgroup`.
The host does not check the ranges of the settings (`session.replay_bytes` 64 KiB to 64 MiB,
`session.attach_wait_ms` 100 to 30000, `session.exit_hold_minutes` 1 to 10080); the controller does.

## Files

| File | Where | Mode | Holds |
|---|---|---|---|
| The host's directory | `$XDG_RUNTIME_DIR/<unit>/` under systemd (`RuntimeDirectory=`); `$XDG_RUNTIME_DIR/pf-hosts/<server>/<session>-<incarnation>/` under the detached launcher | 0700 | the files below |
| `host.sock` | the host's directory | 0600 | the listening socket |
| `host.pid` | the host's directory | 0600 | the host's PID and its start time in clock ticks, as `<pid> <start>` |
| `refusals.log` | the host's directory, detached launcher only | 0600 | one line per refused attach |
| The secrets file | `$XDG_RUNTIME_DIR/pulse-factory/secrets/<unit>.json` (directory 0700) | 0600 | `{"attach_key":"<hex>","env":{"NAME":"value"}}` |
| The exit file | `<state>/hosts/exits/<unit>.json` (directory 0700) | 0600 | what is left of the host; see below |

**The secrets file.** The launcher writes it before the start and passes its path. The host reads it,
deletes it, and only then starts the child. It keeps the **attach key** (32 random bytes as hex, made
by the controller for this incarnation) in memory, and gives the child its `env` variables. So the
child never reads the key, and a secret value never reaches an argument, a unit property, a log or the
exit file. A launch that fails deletes the file too. A host that cannot read or delete it does not
start.

**The host's directory** is made at mode 0700 by the launcher, and the host sets the mode again. The
detached host removes its directory when it ends. A stale detached directory is removed by the
controller's orphan sweep (`r2.s2.w3`).

**The exit file** is written when the host ends, by an atomic rename. Fields: `written_by` (`host`
or `stop-post`), `unit`, `session`, `incarnation`, `child_pid`, `exit` (`code`, `signal`, `at`, or
`null`), `reason`, `end` (the cursor after the last output byte), `acked_through` (the cursor below
which the controller acknowledged), `unread_bytes`, `gaps` (each as `[from, to]`), `record_sha256`,
`refusals` (the count of refused attaches), `written_at` (Unix seconds). `reason` is `exit_acked`,
`hold_expired`, `terminated` (the host got `SIGTERM`) or `start_failed` (with an `error` text). The file
holds no output bytes. `pf-host stop-post` writes a file with `written_by: stop-post`, `reason:
stop_post` and the `service_result`, `exit_code` and `exit_status` systemd gave it, only when none
exists.

## The protocol, version 1

The socket carries two kinds of frame, each starting with a header line of one JSON object ended by
a newline. The key `t` names the message. A control line is at most 64 KiB. Two messages carry raw
bytes after the header line: `out` (host to client) and `input` (client to host). Their header's `n`
is the number of bytes that follow, at most 262144 (256 KiB). The **cursor** of an output byte is its
offset since the child started.

## Before an attach

On a new connection the host sends only `version`:

```json
{"t":"version","proto_min":1,"proto_max":1}
```

Nothing else about the session leaves the host before the checks pass (see "The attach checks").

## Messages

**Client to host.**

| Message | Example | Meaning |
|---|---|---|
| `attach` | `{"t":"attach","proto":1,"key":"<64 hex>"}` | The first message. Answered by `hello`, or by `error` and a closed connection. |
| `read` | `{"t":"read","from":0}` | Stream output from this cursor. A cursor past the end is `error` `bad_cursor`. |
| `ack` | `{"t":"ack","to":14}` | The output below this cursor is consumed: the host drops it. Send it only after the commit that consumed the output. |
| `input` | `{"t":"input","n":3}` then 3 raw bytes | Input for the child. |
| `resize` | `{"t":"resize","rows":30,"cols":100}` | A new size for a PTY child. On pipes: `error` `not_pty`. |
| `signal` | `{"t":"signal","sig":"interrupt"}` | `interrupt` (`SIGINT`), `terminate` (`SIGTERM`) or `kill` (`SIGKILL`), sent to the child's process group. |
| `snapshot` | `{"t":"snapshot"}` | A screen snapshot. Version 1 answers `error` `unsupported`. |
| `exit_ack` | `{"t":"exit_ack"}` | The exit was consumed. |

**Host to client.**

| Message | Example | Meaning |
|---|---|---|
| `version` | see above | Sent first, on every connection. |
| `hello` | `{"t":"hello","proto":1,"session":"S-1","incarnation":1,"invocation_id":"…","child_pid":4242,"record_sha256":"…"}` | After a valid `attach`. `invocation_id` is the unit's `INVOCATION_ID` under systemd and empty under the detached launcher. `exit` (`{"code":0,"signal":null,"at":20}`) is present when the child already ended. |
| `out` | `{"t":"out","at":0,"n":6}` then 6 raw bytes | Output that starts at cursor `at`. |
| `gap` | `{"t":"gap","from":0,"to":12}` | Output from `from` to `to` was dropped by an overflow before it was acknowledged. The next `out` starts at `to`. |
| `exit` | `{"t":"exit","code":null,"signal":2,"at":14}` | The child ended and every output byte was sent. `code` or `signal` is set, the other is `null`. `at` is the cursor after the last output byte. |
| `error` | `{"t":"error","code":"key","text":"…"}` | A refusal or a failed request. `code` is stable; `text` is for people. |

Error codes: `version`, `key`, `peer`, `attach`, `attach_timeout` (a refused attach, and the
connection closes); `superseded` (the connection lost its place, and closes); `bad_message`,
`line_too_long`, `frame_too_large` (a frame that cannot be read, and the connection closes);
`bad_cursor`, `not_pty`, `unsupported`, `exited`, `input_full`, `no_exit`, `resize_failed` (a failed
request; the connection stays open).

## The single attachment

One controller is attached at a time. A new valid `attach` takes over: the host sends the old
connection `error` with `code: "superseded"` and closes it. Only the current holder's `read`, `ack`,
`input`, `resize`, `signal` and `exit_ack` count. Anything a superseded connection sends is dropped.

## The cursor and the gap rules

- The host keeps a ring of the replay size (`session.replay_bytes`, built-in 4 MiB). The buffer holds
  the output from its base cursor to the end.
- A `read` from a cursor below the base answers `gap` (`from` the asked cursor, `to` the base) and
  then the kept bytes. A stream that falls behind the base gets one `gap` the same way.
- An overflow drops the oldest unacknowledged bytes and records the stretch. The child is never
  blocked.
- `ack` drops the output below its cursor. A cursor past the end counts as the end. One below the
  base changes nothing.
- A client that reconnects with `read` from its last acknowledged cursor gets every later byte once.
- The host sends `out` frames of at most 256 KiB. After the last byte it sends `exit`, if the child
  ended.

## The exit rule

When the child ends, the host keeps its exit status and the unacknowledged output. A `hello` after
that carries `exit`.

- **A consumed exit ends the host at once.** When the attached controller has acknowledged the last
  output byte (`ack` to the end) and sent `exit_ack`, in either order, the host writes the exit file
  and ends.
- **An unconsumed exit is held.** With no consumer, the host ends after the exit hold
  (`session.exit_hold_minutes`, built-in 1440) counted from the child's end, and writes the exit file.
- On `SIGTERM` the host sends the child's process group `SIGTERM`, waits up to 5 seconds, sends
  `SIGKILL` to the group, writes the exit file (`reason: terminated`) and ends.

## The attach checks

The host checks every connection, and writes each refusal as one line: to stderr (the unit's
journal) under systemd, and to `refusals.log` in the host's directory under the detached launcher,
whose stderr has no reader. The line holds the peer PID and the check, and no key text:

```text
pf-host: refused session=S-1 incarnation=1 peer_pid=4242 check=key at=1789000000
```

Each refusal also adds to `refusals` in the exit file. The checks, in order:

1. **User.** The peer's UID (`SO_PEERCRED`) must be the host's own.
2. **Cgroup.** When the host got `--controller-cgroup`, the peer's cgroup (the `0::` line of
   `/proc/<pid>/cgroup`) must be that one. Check name: `peer`.
3. **Protocol.** `attach` must name a version the host speaks. Check name: `version`.
4. **Key.** `attach` must carry the attach key, compared in constant time. A missing key is a wrong
   one. Check name: `key`.
5. **Time.** A connection that sends no valid `attach` within the attach wait
   (`session.attach_wait_ms`, built-in 3000) is closed and counted. Check name: `attach_timeout`.
   A first message that is not `attach` is refused with check name `attach`.

The controller checks the host too: `peer_check` in the client library reads the host's PID through
`SO_PEERPIDFD` (falling back to `SO_PEERCRED` on a kernel before 6.5) and its cgroup from `/proc`,
and the controller compares `hello`'s `invocation_id` with the registry row. The controller's half
is `r2.s2.w3`.

A re-attach under the clear grant runs the same checks. It is the effect of the Orchestrator's `settle`
with `resume` on a request with the cause `host_unreachable` (`requests.md`, "An act with an effect").
The controller marks the seat's host for the call, and a rebind of a marked host differs from a
start-up rebind in two ways: its `host_attach` line is mandatory, so a re-attach whose attach line
cannot be written is refused as `unreachable` (`the attach line cannot be written: <why>`) and writes
nothing else; and its outcome line is `host_reattach <unit> <survived|lost|unreachable> session=…
incarnation=… settle=<R-n>@<k> reason=… command=…`, in place of `host_tier1`. A host that is gone is
`lost`: its row ends and its files go, as at start-up. The hello, its checks and the rebuild of the
seat's key table are the start-up path, unchanged, and the start-up rebind keeps its best-effort
lines.

## The limits of the checks

These checks stop mistakes and stale connections, such as a second controller on the same state
directory or a leftover client. They do not stop a hostile process of the same user: any process of
the user can move into another cgroup (Spike A, F8), read the controller's key file, or read the
secrets file before the host deletes it (ADR-0005). The real boundary is a sandbox or another OS
user, which this release does not build.

**Under the detached launcher the cgroup check is vacuous:** the host, the controller and the seats
share one cgroup, so the attach key is the only peer check there.

## Versions

A host speaks a range of protocol versions (`proto_min` to `proto_max` in `version`). A controller
speaks the current protocol version and the one before it, so a host started before an upgrade is
still served. An install that would drop a protocol a live host speaks is refused until those hosts
end (`store check-upgrade`, `r2.s2.w2` and `r2.s2.w3`).

**The golden transcript** `crates/pf-host/tests/fixtures/v1-transcript.jsonl` holds protocol version
1: both directions, every message kind once, and the refusal cases (a wrong key, an empty key, a
version the host does not speak, a superseded connection). It has scenario lines (the child and the
host's settings) and frame lines (`c`: the connection, `d`: `c` for client to host and `h` for host to
client, `head`: the header line, `data_hex`: the raw bytes). Two tests hold it (`v1_transcript.rs`):
the host, given the client half, sends the host half; and the client library sends the client half
byte for byte. The host half is compared apart from the child's PID in `hello` and the text of an
`error`; its `code` is compared. A later protocol version keeps this test green.

## The launchers

Both write the secrets file first. The `Launcher` trait has `start(spec)` (returns the unit, the
socket path, the host PID and the invocation ID), `state(unit)` (`Live`, `Gone` or `Unknown`),
`stop(unit)` and `list(server)` (every host of a server that exists, for the orphan sweep).

The names follow SPINE.md G9: the unit is `pf-host-<server>-<session>-<incarnation>` and the slice
`pf-hosts-<server>.slice`. Server and session ids hold 1 to 64 characters of `A-Z a-z 0-9 - _ . :`
and do not start with a dot; a name that would need escaping is refused. The incarnation is the
digits after the last hyphen, so a session id may hold hyphens. A Unix socket path holds at most 107
bytes, so the caller picks a server name short enough for `<dir>/host.sock` to fit; the launcher
refuses a longer path and says so.

**`Systemd`** runs `systemd-run --user --unit=<unit> --slice=<slice> --collect --quiet` with these
unit properties (Spike A, F1 to F5):

| Property | Why |
|---|---|
| `Type=notify` | The unit is ready only after the socket is bound: the host sends `READY=1`. |
| `KillMode=mixed` | A stop sends `SIGTERM` to the host, and `SIGKILL` to what remains. |
| `TimeoutStopSec=10` | Longer than the host's 5 second grace. |
| `RuntimeDirectory=<unit>` and `RuntimeDirectoryMode=0700` | The socket's directory, removed when the unit stops. |
| `ExecStopPost=<pf-host stop-post …>` | A host killed with `SIGKILL` still leaves an exit file. |

It never passes `--setenv` or `-E`: systemd writes a property to a world-readable unit file. It reads
`systemctl --user show -p ActiveState,InvocationID,MainPID` for the state and the invocation ID, and
`systemctl --user list-units --all 'pf-host-<server>-*'` for the list. A failed `systemd-run` is an
error that names its stderr. There is **no fallback** to the detached launcher: a fallback would hide
the failure.

**`Detached`** starts `pf-host run` with a double fork and `setsid`, so the host leaves the caller's
process group and session and a killed caller leaves it running. It clears `INVOCATION_ID` and
`NOTIFY_SOCKET` from the host's environment, since a shell can inherit another unit's. `state` reads
`host.pid` and compares the process's start time from `/proc`. `list` reads the server's directory.
`stop` sends the host `SIGTERM`, and `SIGKILL` after 10 seconds. It is a **fake**
(`host-launcher-detached`) for tests, fenced seats and a controller that runs as a plain process. It
lacks:

- **a cgroup of its own.** A controller held by a unit with `KillMode=control-group` takes its detached
  hosts down with it, and the cgroup peer check is vacuous.
- **a stop-post step.** A killed host leaves no exit file, so liveness is its PID and start time.

## Fences and live tests

A seat inside `seat-fence` reaches no systemd user manager, so the real launcher is tested by
`crates/pf-host/tests/systemd_live.rs` (every test `#[ignore]`d, named `systemd_live_*`). CI runs
them in its `systemd live` step, after it starts a user manager for the runner. The orchestrator runs
`scripts/check-hosts-systemd.sh` outside the fence at each round barrier.
