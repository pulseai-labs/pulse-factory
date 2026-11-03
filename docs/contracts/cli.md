# CLI contract

`pulse-factory` is one binary (ADR-0001, ADR-0004). The CLI verbs talk to the controller over one
Unix socket. No verb waits or watches: a verb sends one request, prints one answer and exits. A run
has ten verbs: `arm`, `status`, `halt`, `hook` and `controller` (`r0.s1`), and `requests`, `show`,
`claim`, `answer` and `clear` (`r0.s2`). `requests.md` holds the records and the rules.

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
- **A seat cannot spend.** `arm`, `halt`, `claim`, `answer` and `clear` refuse when `PF_SEAT_TOKEN`
  is in the environment. The controller checks the caller of `claim`, `answer` and `clear` again
  (see `claim`).
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

## `requests`

```
pulse-factory requests [--run <run>] [--all] [--json]
```

Lists the open requests of every run, newest last. Each line holds the ID, revision, kind, run,
unit, stage, what it blocks (the unit, and each later unit the merge order holds back), the time it
opened and the reason. `--run` narrows to one run; an unknown run exits 1 and names it. `--all` adds
answered and withdrawn requests, each marked with its status. With nothing open it prints
`no open requests`. The JSON holds `schema_version`, `open` (the count of open requests) and
`requests` (the objects, with `id`, `revision`, `kind`, `status`, `run`, `unit`, `stage`, `blocks`,
`opened_at`, `answered_at` and `reason`). It needs no claim.

## `show`

```
pulse-factory show <R-n> [--json]
```

Prints every field of one request: the body, the answer if there is one, and the verbs that answer
it (`answer`, and `clear` for a request that has a unit). The JSON adds `body`, `answer`,
`answer_generation` and `verbs` to the fields of `requests`. An unknown ID exits 1 and names it. It
needs no claim.

## `claim`

```
pulse-factory claim [--holder <label>] [--run <run>] [--json]
```

Makes this session the one that may answer. The controller makes a token, stores its hash as
generation `n+1`, fences the old claim and writes the ledger line. The client writes the token to
`<state dir>/claim-token` (mode 0600, replacing an older file) and prints the generation, the holder
and the path, never the token. The default holder is `<user>@<host>:<process>`. The default run is
the latest that has not ended.

- **Refusals** (exit 1, nothing recorded): `PF_SEAT_TOKEN` set; a caller that is, or descends from, a
  seat; a `/proc` read that fails; no random bytes (`no token can be made`); no run that takes a
  claim.
- The JSON holds `schema_version`, `generation`, `holder`, `run` and `token_file`.

## `answer`

```
pulse-factory answer <R-n> --rev <k> --text <ruling> [--claim-token-file <path>] [--json]
```

Records the ruling in the request and writes the ledger line. A request that has a unit resumes the
unit at the stage it parked in, as `clear` does. The text is untrusted data: it is stored and shown,
never run. `--rev` must be the request's current revision.

- **Refusals** (exit 1, nothing recorded): the refusals of `claim`; a token file that is missing or
  empty (the message names the path); a token that matches no claim; a fenced token
  (`claim <old> was fenced by claim <new>`); an unknown request; a request that is not open; another
  revision (the message names the current one); a changed pinned file (the message names it).
- The JSON holds `schema_version`, `request`, `revision`, `run`, `unit`, `generation` and `resumed`.

## `clear`

```
pulse-factory clear <run> <item> [--to <stage>] [--claim-token-file <path>] [--json]
```

Resumes a parked unit in the same run. It answers the unit's open request with
`{"verb":"clear","to_stage":…}`, sets the unit `running` at the stage with no session and writes the
ledger line `answer <R-n>@<rev> by claim <generation>: clear <unit> --to <stage>`. The default stage
is the one the unit parked in. The next tick launches it. A halted run accepts the clear and
launches nothing.

- **Refusals** (exit 1, nothing recorded): those of `answer`; a unit that is not parked or has no
  open request; a `--to` that is not a stage of the unit's track.
- The JSON holds `schema_version`, `run`, `unit`, `request`, `revision`, `generation` and
  `to_stage`.

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
