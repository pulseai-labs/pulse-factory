# CLI contract

`pulse-factory` is one binary (ADR-0001, ADR-0004). The CLI verbs talk to the controller over one
Unix socket. No verb waits or watches: a verb sends one request, prints one answer and exits. A run
has fourteen verbs: `arm`, `status`, `halt`, `hook` and `controller` (`r0.s1`), `requests`, `show`,
`claim`, `answer`, `clear` and `ack` (`r0.s2`), `project` (`r1.s2`), which has the three forms
`add`, `list` and `remove`, and `device` and `api` (`r1.s2`), which manage the network API.
`api.md` is the API's own contract. `requests.md` holds the records and the rules.

## Common rules

- **State directory.** `$PF_STATE_DIR`, else `~/.local/state/pulse-factory`, mode 0700. The socket
  is `<state dir>/controller.sock`, mode 0600. The controller owns the store, the queue and the
  ledgers (ADR-0003). A client never writes there.
- **The controller ledger and the `@controller` scope.** `<state dir>/controller.ledger` is the
  ledger of the controller's own commands (`project`, and the device commands: the redeem of a pairing code and `device revoke`). It is
  not a run's ledger. Each command is admitted once under the admission scope `@controller`, where
  a run's commands are admitted under the run's ID, and each admitted or refused new command is one
  line, `- #<n> <command> …`, written by the controller as the only writer. A replay writes no line.
  A start recovers the file as a run's ledger is recovered: a line that was committed and not
  appended is appended once, and no line appears twice. A run ID can never hold `@`, so no run can
  share the scope. The file is operator-local: it is not committed and it has no rotation yet. A
  path and a reason in a line pass the redaction of every ledger, and a line names no secret.
- **`halt` and the projects.** `halt` applies to runs only. A `project` command launches nothing and
  changes no run, so a halt neither stops it nor needs it. The same holds for `device` commands.
  `halt` is not open to a device over the API either (`api.md`).
- **Share directory.** `$PF_SHARE_DIR`, else `~/.local/share/pulse-factory`. It holds `workflows/`
  and `skill/`. `scripts/install.sh` fills it.
- **No controller.** A verb that needs the controller exits 1 and prints a message that names
  `systemctl --user status pulse-factory`.
- **`--json`.** Each verb except `hook` and `controller` takes `--json`. Its JSON holds
  `schema_version` (now `1`). A change that removes or renames a field raises the version.
- **Exit codes.** 0 is success. 1 is a refusal or an error. 2 is `arm` with neither `--dry-run` nor
  `--yes`, and a command line that clap refuses.
- **A seat cannot spend.** `arm`, `halt`, `claim`, `answer`, `clear`, `ack`, `project add`,
  `project remove`, `device pair` and `device revoke` refuse when `PF_SEAT_TOKEN` is in the
  environment (a seat launch is an irreversible spend, ADR-0005, and a seat has no need to change
  the list of projects or to let a device in).
  `project list`, `device list` and `api status` are reads and stay open. The controller checks the
  caller of `claim`, `answer`, `clear`, `ack`, `device pair` and `device revoke` again (see `claim`).
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
  model, the caps (`model_sessions`, `cold_builds`, each seed's `max`, each stage-outcome loop, one
  relaunch per stage), the pinned plugins, the most launches,
  one merge line for each track that merges, one base push line for each stage with `base_push`,
  the run `--replace` names, and `paper env` when set.
  A track with the rule `green_checks_in_declared_order` prints
  `item PRs merge into <base> on green checks with no ack`. A track with the rule `claim_ack`
  prints `the <track> PR merges into <base> only on the claim's ack`. A loop is one `outcomes` entry of a stage. Its text line is
  `caps: loop <stage> -> <to> at most <max> times per <track> unit`. The JSON form holds them as
  `caps.loops`, an array of `{track, from, to, max}`. A stage with `base_push` prints
  `base push: before <track>.<stage>, push <branch> to origin when origin lacks it`. The branch is
  rendered with the run's values and no unit, and a branch that needs a unit shows as written, as the
  base of a merge line does. The JSON form holds them as `base_pushes`, an array of
  `{track, stage, dir, branch}`. It is additive: `schema_version` stays, and a package with no
  `base_push` has an empty array and prints no such line. A base push does not change the most
  launches.
  The most launches is `2 x (seed max x seed-track launches + items x item-track launches)`, summed
  over every seed and track. The launches of one unit of a track are its stages, each counted once
  except a stage with `entered_by: outcome`, which counts 0, plus the `max` of every `outcomes`
  entry of the track: each time an entry fires, it launches its `to` stage once. The 2 is the one
  relaunch after a stall. The number is what the plan shows. It is not a limit the run enforces.
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
it (`answer`, and `clear` for a request that has a unit). It also prints the gaps a `gaps` request
holds and the text of a `stop` request. The JSON adds `body`, `answer`,
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
- When the workflow declares an `on_answer` hook for the request, it runs first (`workflow-file.md`,
  "Answer hooks"). A hook that fails does not refuse the answer: the request is answered, the unit
  parks again with a new request, and `resumed` is `false`.
- The JSON holds `schema_version`, `request`, `revision`, `run`, `unit`, `generation`, `resumed`,
  `hook` (`none`, `passed` or `failed`) and `hook_failure` (the cause, or null).

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
  open request; a `--to` that is not a stage of the unit's track; a `merge_ack` request (the
  message names `ack`).
- The JSON holds `schema_version`, `run`, `unit`, `request`, `revision`, `generation` and
  `to_stage`.

## `ack`

```
pulse-factory ack <R-n> --rev <k> [--claim-token-file <path>] [--json]
```

Merges the spine PR that an open `merge_ack` request names, with the head SHA of the request
pinned. It takes the claim as `answer` does: the same token file, the same caller check and the same
refusals. `--rev` must be the request's current revision. The controller, in order:

1. refuses a halted or ended run, and names the state;
2. reads the PR again. If its head SHA is not the request's `head_sha`, it revises the request (the
   body holds the new SHA, the revision is `k+1`) and refuses; the message names the new revision
   and SHA. If the checks are not green, the PR has a conflict, or its repo, base or head branch
   changed, it refuses with the cause and the request stays open at its revision;
3. otherwise records the answer (`{"verb":"ack","head_sha":…}` under the claim generation), merges
   with the SHA pinned, checks that the merge landed and runs the track's `after_merge` commands.
   If the merge did not land, the unit parks with a `merge_failed` request, and `clear` sends it
   back to the merge, which opens a new `merge_ack`.

- **Refusals** (exit 1): those of `claim`; a token file that is missing or empty; a token that
  matches no claim; a fenced token; an unknown request; a request of another kind (the message
  names the kind: `ack` answers only a `merge_ack` request); a request that is not open; another
  revision (the message names the current one); a halted or ended run; a moved head or any cause in
  step 2; a changed pinned file. A refusal after the request is known writes the ledger line
  `ack <R-n>@<rev> by claim <generation> refused: <cause>`, except a refusal of the token or the
  caller, which the controller makes before the engine runs. A moved head is the one refusal that
  changes state: it revises the request.
- The ledger line of a landed ack is `ack <R-n>@<rev> by claim <generation>: verb=ack unit=<unit>
  pr=<repo>#<n> base=<base> branch=<head branch> sha=<head SHA>`. The merge writes its own line
  (`merge of unit <unit>: repo=… branch=… sha=… outcome=landed`).
- The JSON holds `schema_version`, `request`, `revision`, `run`, `unit`, `generation`, `verb`
  (`ack`), `pr`, `base`, `head_sha`, `merged` (true when the merge landed), `outcome` (`landed`, or
  the cause of the park) and `after_merge_failure` (the cause, or null).

## `project`

```
pulse-factory project add <root> [--name <name>] [--command-id <id>] [--json]
pulse-factory project list [--json]
pulse-factory project remove <id> [--command-id <id>] [--json]
```

A project is an ID, a name and one root. Nothing else is decided: the project has no repo, pairing,
worktree or kind field, and the rest of its schema waits for the project hierarchy brainstorm
(operator, 2026-11-07). The verbs launch nothing and change no run, so `halt` does not apply to
them. Each change is a controller command with a command ID and one line of the controller ledger
(see Common rules).

- **The root.** `<root>` is an existing directory, a git repo or a plain folder. The CLI resolves a
  relative `<root>` against its current directory and sends an absolute path. The controller makes
  it canonical (`std::fs::canonicalize`): two spellings of one directory, such as a symlink or a
  `..`, are one root. It refuses a root that does not exist, is not a directory, or is `/`. It does
  not look inside the root: a git repo and a plain folder are the same to it. A live project holds
  its root alone: a second `add` of the same root is refused until the first is removed.
- **The name.** `--name` defaults to the last path component of the root. A name is 1 to 64
  characters, has no control character and no line break, and is unique among live projects. The
  check is exact: `Alpha` and `alpha` are two names.
- **The ID.** A project ID is `P-<n>`, in the order of creation. `remove` marks the project removed
  and the row stays, so an ID is never used twice, and a removed root or name can be added again
  under a new ID.
- **The command ID.** `--command-id` is 1 to 128 characters of letters, digits and `. _ - :`. With
  none, the CLI makes a random one (128 bits, as 32 hex digits) for each call. A caller that retries
  sends the same one. The same ID with the same arguments returns the first record's project:
  `replayed` is true in the JSON, and the ledger gets no line. The same ID with other arguments
  exits 1 and names the command ID. A refused command is stored too: its replay is the same
  refusal.
- **`add`** prints `project P-1 added: <name> (<root>)`.
- **`list`** prints one line per live project in creation order, `P-1  <name>  <root>`, or
  `no projects`. It needs no claim and is open to a seat.
- **`remove`** prints `project P-1 removed: <name> (<root>)`.
- **JSON.** `add` and `remove` print `{"schema_version": 1, "replayed": <bool>, "project": {"id",
  "name", "root", "created_at", "removed_at"}}`, where `removed_at` is null for a live project.
  `list` prints `{"schema_version": 1, "projects": [...]}` with the live projects only, in creation
  order. The controller verbs are `project_add`, `project_list` and `project_remove`.
- **Refusals** (exit 1, nothing on stdout, the reason on stderr, one admission and one ledger
  line): a root that does not exist, is not a directory, is `/`, or is held by a live project; a name
  that is empty, longer than 64 characters, holds a control character or a line break, or is held by
  a live project; an unknown or removed ID on `remove`. A command ID that is not valid, and
  `PF_SEAT_TOKEN` set on `add` or `remove`, are refused before any command is admitted: nothing is
  stored.
- **Ledger lines.** `project_add P-1 admitted name=<name> root=<root> command=<id>`,
  `project_remove P-1 admitted command=<id>`, and `project_add refused reason="<reason>"
  command=<id>`. A value with a space is quoted.

## `device`

```
pulse-factory device pair [--json]
pulse-factory device list [--json]
pulse-factory device revoke <id> [--command-id <id>] [--json]
```

A device is a client, such as the MacBook app, that the operator let in over the network API
(ADR-0012, `api.md`). A device acts as the operator and has no scope of its own (ADR-0009). The
verbs run on the host over the socket. The API itself offers none of them (`api.md`, "The
allowlist"). They launch nothing and change no run, so `halt` does not apply to them.

- **`pair`** prints one pairing code and when it stops working. The code is the one copyable string
  that `api.md` describes. It holds the API address, a single-use secret and the SPKI hash of the
  server key. It works once and for 10 minutes, a restart voids it, and a new code replaces the live
  one, so at most one code is live. The text form is two lines: the code, then
  `expires <time> (one use)`. The JSON is `{"schema_version": 1, "code", "expires_at"}`. This output
  is the only place the code is ever shown. The command writes no ledger line: the redeem does.
- **`list`** shows every device, live ones first and each group in pairing order. A line is
  `<id>  <name>  paired <time>  last seen <time> from <peer>`, with `never seen` in place of the last
  two parts for a device that has made no call, and `  revoked <time>` at the end of a revoked one.
  With none it prints `no devices`. It needs no claim and is open to a seat. The JSON is
  `{"schema_version": 1, "devices": [{"id", "name", "paired_at", "last_seen_at", "last_peer",
  "revoked_at"}]}`, where the last three are null when unset. A record holds no token and no hash of
  one. `last_seen_at` and `last_peer` are telemetry: the store writes them at most once a minute for
  a device (ADR-0003, amended). A device that has made no call for 30 days is refused by the API
  (401) and must pair again. It stays in the list, unrevoked: read its age from `last seen`
  (`api.md`, "Idle expiry").
- **`revoke`** marks the device revoked. Its next call over the API is refused with 401, because the
  API reads the store on each call. The row stays, so an ID (`D-<n>`) is never used again, and the
  device pairs again as a new device with a new code. The text form is
  `device D-1 revoked: <name>`. The JSON is `{"schema_version": 1, "replayed": <bool>, "device":
  {...}}`. `--command-id` follows the rules of `project`: the same ID with the same arguments returns
  the first record with `replayed` true and writes no line, and the same ID with other arguments
  exits 1.
- **Refusals** (exit 1, nothing on stdout, the reason on stderr as `pulse-factory: device <verb>:
  <reason>`): `pair` while the API is off (the message names `PF_API_LISTEN`) or while it failed to
  start (the message gives the cause); `revoke` of an unknown device or one that is already revoked,
  and of an ID of more than 64 characters; `pair` and `revoke` with `PF_SEAT_TOKEN` set, and a caller
  that the controller finds to be, or to descend from, a seat. An invalid command ID is refused
  before any command is admitted.
- **Ledger lines.** The redeem of a code is the command `device_pair`:
  `device_pair D-1 admitted name=<name> peer=<address>`. A revoke is
  `device_revoke D-1 admitted command=<id>`, or `device_revoke refused reason="<reason>"
  command=<id>`. A line names the device, its name and the peer, and never the code or the token.

## `api`

```
pulse-factory api status [--json]
```

Shows the state of the network API listener. It is a read: it needs no claim and is open to a seat.

- The text form is one of `off`, `waiting for <addr>`, `listening on <addr>` or `failed: <why>`.
  When the key exists, a second line `key spki sha256: <64 hex digits>` follows.
  - `off`: no address is set, and no key was made.
  - `waiting for <addr>`: the bind failed with "address not available", for example before Tailscale
    starts. The controller tries again every 5 seconds and keeps serving the socket.
  - `listening on <addr>`: the listener is bound.
  - `failed: <why>`: another bind error, or the key or certificate could not be made. The controller
    does not try again, and it keeps serving the socket. An address outside loopback and the tailnet
    (`127.0.0.0/8`, `::1`, `100.64.0.0/10`, `fd7a:115c:a1e0::/48`) is refused before any key is made:
    `failed: the API address <ip> is outside loopback and the tailnet (127.0.0.0/8, ::1,
    100.64.0.0/10, fd7a:115c:a1e0::/48)`. Nothing listens, and `device pair` refuses with the same
    text.
- The JSON is `{"schema_version": 1, "state": "off" | "listening" | "waiting" | "failed", "listen":
  <addr or null>, "key_spki_sha256": <hex or null>, "error": <text or null>}`.
- The hash is the one a pairing code carries. A client pins it. A new key (the key files were
  removed) shows a new hash, and every paired client then fails closed (`api.md`, "Key loss").

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
pulse-factory controller [--tick-ms <n>] [--api-listen <addr>]
```

Runs in the foreground. The systemd user unit runs it. One tick runs every `--tick-ms`
milliseconds (default 2000). A second controller on one socket refuses to start. A socket file that
no one answers is stale, and the controller removes it.

`--api-listen <addr>` serves the network API on `<addr>` (`host:port`, such as
`100.90.203.21:7443`). Without the flag, the environment variable `PF_API_LISTEN` gives the address.
With neither, the API is off, no key is made and nothing listens. The unit reads
`~/.config/pulse-factory/controller.env` as an optional environment file, so the operator sets
`PF_API_LISTEN` there (`docs/install.md`).
