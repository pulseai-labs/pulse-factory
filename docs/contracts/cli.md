# CLI contract

`pulse-factory` is one binary (ADR-0001, ADR-0004). The CLI verbs talk to the controller over one
Unix socket. No verb waits or watches: a verb sends one request, prints one answer and exits. This
spine has five verbs. `r0.s2` adds the rest (`requests`, `show`, `claim`, `answer`, and a clear verb).

## Common rules

- **State directory.** `$PF_STATE_DIR`, else `~/.local/state/pulse-factory`, mode 0700. The socket
  is `<state dir>/controller.sock`, mode 0600. The controller owns the store, the queue and the
  ledger (ADR-0003). A client never writes there.
- **Share directory.** `$PF_SHARE_DIR`, else `~/.local/share/pulse-factory`. It holds `workflows/`
  and `skill/`. `scripts/install.sh` fills it.
- **No controller.** A verb that needs the controller exits 1 and prints a message that names
  `systemctl --user status pulse-factory`.
- **`--json`.** Each verb except `hook` and `controller` takes `--json`. Its JSON holds
  `schema_version` (now `1`). A change that removes or renames a field raises the version.
- **Exit codes.** 0 is success. 1 is a refusal or an error. 2 is `arm` with neither `--dry-run` nor
  `--yes`, and a command line that clap refuses.
- **A seat cannot spend.** `arm` and `halt` refuse when `PF_SEAT_TOKEN` is in the environment.
- **The wire.** One request and one answer for each connection, each one line of JSON, at most
  1 MiB. Each carries `protocol_version`; a different version is refused with a message that names
  both.

## `arm`

```
pulse-factory arm <spine> --workflow <name-or-path> --ledger <file> [--param k=v]...
                  [--plugin-root <dir>]... [--paper <dir>] [--replace <run>] [--dry-run | --yes] [--json]
```

- `<spine>` fills `param.spine` and matches `[A-Za-z0-9._-]+`.
- A workflow name finds `<share dir>/workflows/<name>/`. A value with a `/` is a path.
- `--param` gives a declared parameter. `arm` itself sets `spine`, `briefs` (the run's own copy of
  the briefs) and `forge_repo` (read from the forge); a value for any of them is refused. A value
  that is `.` or starts with `./` or `../` is refused, because the controller runs in another
  directory.
- `--paper <dir>` runs the seats as paper scripts against the bare repo `<dir>/forge.git`: no
  model, no spend, no network. `status` shows `env: paper`.
- **Refusals.** Each prints `pulse-factory: arm refused: <cause>` on stderr, exits 1 and records
  nothing: `PF_SEAT_TOKEN` set; a workflow that does not load; a missing, undeclared, duplicate or
  arm-set parameter; a failed `plan_check`; a plugin not found at its minimum version; a
  `--plugin-root` that does not exist; `--dry-run` with `--yes`; a run with a running session; an
  earlier run not at an end state that `--replace` does not name; and, without `--paper`, a `repo`
  whose `origin` is not on GitHub or a failed `gh` read.
- **The summary.** Spine, item count, workflow name and file version, each role with runtime and
  model, the caps (`model_sessions`, `cold_builds`, each seed's `max`, one relaunch per stage), the
  pinned plugins, the most launches, `item PRs merge into <base> on green checks with no ack`, the
  run `--replace` names, and `paper env` when set. The most launches is
  `2 x (seed max x seed-track stages + items x item-track stages)`, summed over every seed and
  track. It is what the plan shows. It is not a limit the run enforces.
- **Flags.** `--dry-run` prints the summary, exits 0 and records nothing. It needs no controller;
  with none it skips the checks on earlier runs and says so. With neither flag, `arm` prints the
  summary, says `re-run with --yes to start` and exits 2. `--yes` sends the plan to the controller,
  which pins and records the run and prints `run: <run id>`.
- **`--replace <run>`** replaces an earlier run that is not at an end state and has no running
  session. After the new run's pins succeed, the old run ends with `replaced by <new run>`.

## `status`

```
pulse-factory status [<run>] [--json]
```

The default run is the latest. It prints the run, `env`, state, any halt or degraded reason, the
notes of the last tick, each unit with its stage, status and park reason, each session with unit,
stage, role, model, runtime, status, tokens by kind and cost, and the run's total. Cost is whole
micro-dollars, shown as dollars. An unknown run exits 1 and names it.

## `halt`

```
pulse-factory halt [<run>] --reason <text> [--json]
```

Running seats finish their stage. Nothing new launches or merges. A repeat is safe, and the first
reason stays. A caller with `PF_SEAT_TOKEN` is refused.

## `hook`

```
pulse-factory hook
```

The OMP extension runs this for each tool call of a seat (`/bin/sh -c "$PF_HOOK_CMD"`). Stdin holds
`{"role":..,"tool":..,"input":{..}}`. For `bash` the hook reads `input.command`. For a write tool it
reads `input.path` or `input.file_path`; a write tool with neither is denied. It sends only the
session key (`PF_SESSION_KEY`), the tool and the command or path to the controller. It prints
`{"decision":"allow"|"deny","reason":..}` and exits 0. Every failure is a deny: no key, an unknown
key, no controller, unreadable input. This is the extension's protocol, not `--json`.

## `controller`

```
pulse-factory controller [--tick-ms <n>]
```

Runs in the foreground. The systemd user unit runs it. One tick runs every `--tick-ms`
milliseconds (default 2000). A second controller on one socket refuses to start. A socket file that
no one answers is stale, and the controller removes it.
