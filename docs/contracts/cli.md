# CLI contract

`pulse-factory` is one binary (ADR-0001, ADR-0004). The CLI verbs talk to the controller over one
Unix socket. No verb waits or watches: a verb sends one request, prints one answer and exits. A run
has eighteen verbs: `arm`, `status`, `halt`, `hook` and `controller` (`r0.s1`), `requests`, `show`,
`claim`, `answer`, `clear` and `ack` (`r0.s2`), `project` (`r1.s2`), which has the three forms
`add`, `list` and `remove`, `device` and `api` (`r1.s2`), which manage the network API, and
`session` and `chat-runtime` (`r1.s3`), which chat with a project's Orchestrator and install the
runtime it needs, `settings` (`r1.s4`), which reads and sets how a chat behaves, per server,
project or agent, and `propose` (`r1.s4`), which posts a start card into the calling chat.
`api.md` is the API's own contract. `requests.md` holds the records and the rules.

## Common rules

- **State directory.** `$PF_STATE_DIR`, else `~/.local/state/pulse-factory`, mode 0700. The socket
  is `<state dir>/controller.sock`, mode 0600. The controller owns the store, the queue and the
  ledgers (ADR-0003). A client never writes there.
- **The controller ledger and the `@controller` scope.** `<state dir>/controller.ledger` is the
  ledger of the controller's own commands (`project`, the device commands, which are the redeem of a
  pairing code and `device revoke`, and the chat commands `chat_session_start`, `chat_session_end`,
  `chat_item_redact`, `chat_send`, `chat_answer`, `chat_interrupt` and `chat_session_set`, the settings
  commands `setting_set` and `setting_unset` (`r1.s4`), with the kind
  backfill `project_kind` that runs at controller start). A `chat_send` line holds the byte count of
  the text and never the text. It is
  not a run's ledger. Each command is admitted once under the admission scope `@controller`, where
  a run's commands are admitted under the run's ID, and each admitted or refused new command is one
  line, `- #<n> <command> …`, written by the controller as the only writer. A replay writes no line.
  A start recovers the file as a run's ledger is recovered: a line that was committed and not
  appended is appended once, and no line appears twice. A run ID can never hold `@`, so no run can
  share the scope. The file is operator-local: it is not committed and it has no rotation yet. A
  path and a reason in a line pass the redaction of every ledger, and a line names no secret.
  When the file cannot be recovered at start or a line cannot be appended, the controller's
  commands stop until it restarts with a good file: a `project` command, and the redeem of a
  pairing code, are refused with the reason (the redeem leaves the live code unused). `device
  revoke` is the one exception: it still commits to the store, so the device is refused at once,
  and its line waits in the outbox for the next start to append. The reads still work.
- **`halt` and the projects.** `halt` applies to runs and to live chat processes. A `project` command
  launches nothing and changes no run, so a halt neither stops it nor needs it. The same holds for
  `device` commands and for the commands `project_kind`, `chat_session_start`, `chat_session_end`
  and `chat_item_redact`. The commands `setting_set` and `setting_unset` launch nothing and change
  no run either, so `halt` neither stops them nor needs them, and they still commit after a halt.
  `halt` stops every live chat process (see `halt` and `session`). `halt` is
  not open to a device over the API either (`api.md`).
- **`chat.ledger`.** `<state dir>/chat.ledger` is the audit trail of the chat processes, beside
  `api.ledger`. It is append-only, has one writer, writes no `fsync` per line, is not in the store
  and has no rotation. A crash can lose the last lines: the store's `turn_result` items keep the
  usage. One line for each process open, each turn and each process stop; every value passes
  `field()`, and no line holds a message, a tool text, a token or a key:

  ```
  <UTC> chat open session=S-1 runtime=claude-code version=2.1.319 qualified=true resume=no pid=<n|-> deny=<count> deny_from=<scope|built_in>
  <UTC> chat turn session=S-1 cause=operator input_tokens=9 output_tokens=12 cache_read_tokens=0 cache_write_tokens=13006 cost_micros=3465 duration_ms=1042 stop=end_turn is_error=false
  <UTC> chat stop session=S-1 reason=end|idle|halt|exited|shutdown|option [code=<n>]
  <UTC> chat stop session=S-1 reason=self_turn_cap self_turns=<n> cap=<n> from=<scope|built_in>
  ```

  `deny=` is the number of shell command patterns the launch passed to the runtime, and `deny_from=`
  is the scope of the list in force (`built_in`, `server`, `project:P-1`, `agent:orchestrator` or
  `project:P-1/agent:orchestrator`; see `settings`). A stop with `reason=self_turn_cap` is the cap on
  the turns a chat starts on its own: `self_turns` counts the turn that was cut, so it is the cap plus
  one, and `from` is the scope the cap came from.

  `qualified=false` marks an override binary. A turn that the CLI starts on its own (a `task` turn)
  has its line too, with `cause=task`.
- **The chat settings.** The controller reads these from its environment, which is the unit's
  `controller.env` (`docs/install.md`): `PF_SHARE_DIR`, `PF_CHAT_CLAUDE_BIN` (an override binary,
  run as `unqualified`; the controller prints `pulse-factory: the chat runs an unqualified Claude
  Code binary: <path>` at start), `PF_CHAT_MAX_LIVE` (the cap on live chat processes, 1 to 64,
  default 4; a bad value prints one line on stderr and keeps the default), `PF_AGENT_ENV_<AGENT>`
  (the agent's server-side environment, `NAME=value` pairs separated by spaces; the agent ID is
  upper-cased and each other character is `_`), `PF_SERVER_NAME` (the name every API answer carries)
  and `PF_CHAT_REGISTRY` (default `https://registry.npmjs.org`). No `PF_*` name reaches a chat
  process. These are "the chat settings": the controller's environment, and a restart changes them.
  The values that `settings` reads and sets are "settings": scoped values in the store, and no restart
  changes them. The two share no name and no storage. The controller marks each chat with
  `PULSE_FACTORY_CHAT=<session ID>`, which is no `PF_` name (`requests.md`, "The caller check").
- **`PF_CONFIRM_TTL`.** How long a device's confirm code works, in whole seconds. The controller reads
  it at start. It can only shorten the default of 120 seconds: a value from 1 to 120 is used, and a
  bad or larger value prints one line on stderr and keeps 120. The tombstone window stays 10 minutes
  (`requests.md`, "Confirm codes").
- **Share directory.** `$PF_SHARE_DIR`, else `~/.local/share/pulse-factory`. It holds `workflows/`
  and `skill/`. `scripts/install.sh` fills it.
- **No controller.** A verb that needs the controller exits 1 and prints a message that names
  `systemctl --user status pulse-factory`.
- **`--json`.** Each verb except `hook` and `controller` takes `--json`. Its JSON holds
  `schema_version` (now `1`). A change that removes or renames a field raises the version.
- **Exit codes.** 0 is success. 1 is a refusal or an error. 2 is `arm` with neither `--dry-run` nor
  `--yes`, and a command line that clap refuses.
- **A seat cannot spend.** `arm`, `propose`, `halt`, `claim`, `answer`, `clear`, `ack`, `project add`,
  `project remove`, `device pair`, `device revoke` and the state-changing `session` verbs (`start`,
  `send`, `answer`, `interrupt`, `set`, `end` and `redact`) refuse when `PF_SEAT_TOKEN` is in the
  environment (a seat launch is an irreversible spend, ADR-0005, and a seat has no need to change
  the list of projects, to let a device in or to start a chat).
  `project list`, `device list`, `api status`, `session list`, `session items` and `settings get` are
  reads and stay open. `settings set` and `settings unset` are the named exception: the CLI does not
  refuse them under `PF_SEAT_TOKEN`. They always reach the controller, whose caller check refuses the
  seat and audits the refusal (see `settings`). The controller checks the caller of `arm`, `propose`, `claim`,
  `answer`, `clear`, `ack`, `device pair`, `device revoke`, the state-changing `session` verbs and
  `settings set` and `settings unset` again: a caller that is, or descends from, a seat or a chat
  process, or that holds `PULSE_FACTORY_CHAT` in its environment, is refused (see `claim`, `session`
  and `settings`).
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
  whose `origin` is not on GitHub or a failed `gh` read; and a caller that is, or descends from, a
  chat process, or that holds `PULSE_FACTORY_CHAT` (`arm refused: the caller is, or descends from,
  chat process <pid>`). `arm` runs only through the peer-aware path of the socket, so a request with
  no peer is refused with `arm refused: the controller cannot tell which process called`.
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
- **The device path** (`r1.s4.w2`). A paired device arms a run from a start card that a chat posted
  with `propose`. It sends the `arm` verb over the API with a confirm code for that card and that
  plan (`api.md`, "Confirm codes"). The controller then builds the plan again from the snapshot of
  the package that it took at `propose`, and arms only when the plan, the summary and the package
  are the same. The CLI's `arm` and the device path share one plan builder and one summary, so the
  card shows what `arm --dry-run` prints. The plan has two optional fields that the CLI never sets,
  `project_id` (the project of the chat that proposed) and `expect_package` (the content hash of the
  snapshot). A request on the socket that carries a confirm code and no plan is refused with `arm
  refused: confirm codes are for paired devices; …`.
- **The run's project.** A run armed from a start card belongs to the chat's project. `status` and
  `runs` show it as `project_id`. A run that the CLI armed has `project_id` null.

## `propose`

```
pulse-factory propose <spine> --workflow <name> --ledger <file> [--param k=v]...
                      [--plugin-root <dir>]... [--paper <dir>] [--command-id <id>] [--json]
```

`propose` posts a **start card** into the chat that called it, and starts nothing. A paired device
confirms the card, and only then does the run arm (`arm`, "The device path"). It is the Orchestrator
chat's way to start a spine: a chat is refused on `arm`.

- **The inputs** are `arm`'s, with these narrowings (W1, W2). `--workflow` takes the name of an
  installed workflow and never a path: a value with a `/` is refused by the CLI and again by the
  controller (`propose refused: --workflow takes the name of an installed workflow, not a path`).
  `--ledger`, `--plugin-root` and `--paper` are made absolute from the caller's working directory,
  and the controller refuses a relative path again. `propose` has no `--replace`, no `--yes` and no
  `--dry-run`: given `--replace` the CLI exits 2 (clap's unexpected-argument error). A request on the
  socket whose inputs carry `replace` is refused with `propose refused:
  replacing a run is the operator's; use pulse-factory arm --replace`.
- **The caller.** The controller finds the calling chat by the ancestry of the caller, never by the
  marker. In this order:
  1. a caller with `PF_SEAT_TOKEN`, or a tracked seat on its chain, is refused (it is a seat), and so
     is a failed `/proc` read;
  2. a live chat process on the chain gives the session. A `PULSE_FACTORY_CHAT` that names another
     session is refused;
  3. no chat process on the chain, and the caller holds `PULSE_FACTORY_CHAT`: refused, `propose
     refused: the caller holds PULSE_FACTORY_CHAT but runs under no live chat process; the marker
     never names the caller`. This covers a child that the chat detached;
  4. neither: refused, `propose refused: the caller runs under no chat session; propose posts a
     start card into the calling chat, and an operator on this host starts a run with pulse-factory
     arm`.

  `propose` runs only through the peer-aware path of the socket: with no peer it is refused, and it
  is not on the device allowlist.
- **The snapshot and the plan.** The controller draws the proposal ID (`sp-` and 16 hex digits),
  copies the pinned files of `<share dir>/workflows/<name>` to `<state dir>/proposals/<sp-…>/package`
  and pins the copy. It then builds the plan in its own process from the snapshot, with the check
  that `arm --dry-run` makes (`plan_check`, plugins, the forge facts, an earlier run). A refusal from
  any step is `propose refused: <cause>`, with `arm`'s cause; it posts no card and leaves no
  snapshot. A removed project is refused (`propose refused: project P-n is removed`).
- **The bound** (W19). The controller counts the session's calls in the last minute before it builds
  anything. Over `chat.propose_per_min` (see `settings`) the call is refused: `propose refused:
  propose_rate: S-1 made <n> proposals in the last minute (chat.propose_per_min = <k>, from
  <scope>)`.
- **The card.** `plan_sha256` is the SHA-256 of the plan, its summary and the snapshot's content
  hash. A card is a `start_card` item of the chat, with the summary, the parameters, the ledger and
  the plugin roots. A second `propose` from the same chat posts a new card and withdraws the first
  (`start_result` `withdrawn`, "superseded by sp-…"). A card is also withdrawn when the session ends,
  on `halt` (`proposals_withdrawn`) and at a controller start. Proposals live in memory: a restart
  keeps no proposal, and a card whose proposal already has a `run_project` admission is answered
  `armed` with its run.
- **The answer.** JSON: `{"schema_version": 1, "replayed", "proposal", "session", "project_id",
  "plan_sha256", "summary"}`. The text form prints the summary lines that `arm` prints, then
  `proposal: sp-… posted to chat session S-1; nothing is armed until the operator confirms it on a
  paired device`. A retry sends the same `--command-id`: the controller answers the first proposal
  with `replayed: true` and posts no second card. After a restart a replay answers `proposal: null`
  with a note.
- **The ledger.** Every call writes one line of `<state dir>/controller.ledger`. An admitted call:
  `propose S-1 proposal=sp-… project=P-1 spine=<spine> workflow=<name> items=<n|unknown>
  most_launches=<n|unknown> plan=<plan_sha256> command=<id>`. A refusal, the caller's included, is
  one refused admission under a command ID that the controller makes, `propose-refused-<16 hex>`,
  so the caller's own ID stays unused: `propose refused caller=<seat-token|seat:<pid>|chat:<S-n>|
  chat:invalid|unknown> reason="…" command=propose-refused-<hex>`. A `propose_rate` refusal
  coalesces as `settings` refusals do: the first writes its line, and one more line at the close of
  the window carries `refused=<n>`.
- **Exit codes.** 0 is a posted card. A refusal prints `pulse-factory: propose refused: <cause>` on
  stderr and exits 1. `PF_SEAT_TOKEN` set is refused by the CLI.

## `status`

```
pulse-factory status [<run>] [--json]
```

The default run is the latest. It prints the run, `env`, state, any halt or degraded reason, the
notes of the last tick, each unit with its stage, status and park reason, each session with unit,
stage, role, model, runtime, status, tokens by kind and cost, and the run's total. Cost is whole
micro-dollars, shown as dollars. An unknown run exits 1 and names it. `status --json` and the
controller's `runs` list carry `project_id` (`r1.s4.w2`): the project of the chat that proposed the
run, or null for a run that the CLI armed.

## `halt`

```
pulse-factory halt [<run>] --reason <text> [--json]
```

Running seats finish their stage. Nothing new launches or merges. A repeat is safe, and the first
reason stays. A caller with `PF_SEAT_TOKEN` is refused.

`halt` also stops every live chat process and its process group, closes each pending request of
those chats as `lost` (`halted`), adds a `notice` item to each chat, and writes `chat stop
reason=halt` for each. The chat sessions stay live and `idle`: the operator's next `session send`
reopens one, with a new confirm. With no run at all `halt` still stops the chats and answers ok with
`"run": null`. The JSON gains `"chats_stopped": ["S-1", …]`, and the text form names the stopped
chats.

`halt` first withdraws every open proposal (`start_result` `withdrawn`, "halted"), and only then
stops the chats, so the end of a stopped chat finds no proposal to withdraw. The JSON gains
`"proposals_withdrawn": ["sp-…", …]`.

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
`opened_at`, `answered_at`, `reason` and `verbs`, plus `pr`, `base` and `head_sha` on a
`merge_ack`), and `claim` (the current claim as `generation`, `holder`, `device` (the device ID, or
null), `revoked` and `claimed_at`; null when none). The new fields join the old ones: none is
removed and `schema_version` stays 1. The API serves the same JSON (`api.md`). It needs no claim.

## `show`

```
pulse-factory show <R-n> [--json]
```

Prints every field of one request: the body, the answer if there is one, and the verbs that answer
it (`answer`, and `clear` for a request that has a unit). It also prints the gaps a `gaps` request
holds and the text of a `stop` request. The JSON adds `body`, `answer`,
`answer_generation` to the fields of `requests` (`verbs` and a `merge_ack`'s `pr`, `base` and
`head_sha` are in them already). An unknown ID exits 1 and names it. It needs no claim.

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
  seat or a chat process (`claim refused: the caller is, or descends from, chat process <pid>`), or
  that holds `PULSE_FACTORY_CHAT` in its environment; a `/proc` read that fails; no random bytes
  (`no token can be made`); no run that takes a claim; a holder label that starts with `device `
  (`the holder label "device …" is kept for paired devices`).
- A paired device takes the claim only by acting with a confirm code (`requests.md`, "The claim").
  There is no `claim` verb on the API.
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
  (`claim <old> was fenced by claim <new>`); a token that a paired device fenced (the message ends
  `the claim moved to device D-n (<name>) at <time>; claim again with pulse-factory claim`); a call
  with `code` and no token (`confirm codes are for paired devices; on this host use pulse-factory
  claim`); an unknown request; a request that is not open; another
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

- **Refusals** (exit 1, nothing recorded): those of `answer`, the refusal that names `the claim
  moved to device D-n` included; a unit that is not parked or has no
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
  matches no claim; a fenced token, or one that a device fenced (`the claim moved to device D-n`); an unknown request; a request of another kind (the message
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

A project is an ID, a name, one root, a kind and a remote key (ADR-0015). The verbs launch nothing
and change no run, so `halt` does not apply to them. Each change is a controller command with a
command ID and one line of the controller ledger (see Common rules).

- **The root.** `<root>` is an existing directory, a git repo or a plain folder. The CLI resolves a
  relative `<root>` against its current directory and sends an absolute path. The controller makes
  it canonical (`std::fs::canonicalize`): two spellings of one directory, such as a symlink or a
  `..`, are one root. It refuses a root that does not exist, is not a directory, or is `/`. A live
  project holds its root alone: a second `add` of the same root is refused until the first is
  removed.
- **The kind.** `add` looks inside the root once and stores its kind. The kind never changes. After
  a `git init` in a `folder` project, remove the project and add it again.
  - `git`: the root is the main worktree of a git repository.
  - `folder`: the root is a plain folder, or a subdirectory of a repository. Its git operations
    would reach outside it, so it is a folder.
  - Git runs as a subprocess with a 10-second timeout and with the controller's own `GIT_*`
    variables removed. A failed detection is never `folder`: it is a refusal that names the cause.
  - `add` refuses a linked worktree (`the root is a linked worktree; add its main worktree instead:
    <main>`), a bare repository, the inside of a git directory, a repository that git refuses for
    dubious ownership, a submodule checkout or a `--separate-git-dir` checkout (`the root is not a
    main worktree: its git directory is <dir>`), and any other git failure, with git's first stderr
    line. The last two are a known limit of `r1.s3`.
- **The remote key.** A `git` project has a `remote_key` and a `folder` project has none. The key is
  the host and path of the project's remote, for example `github.com/pulseai-labs/pulse-factory`.
  The remote is `origin`, else the one remote when there is exactly one, else there is no key. The
  key drops the user, the password and the port, lowercases the host, and drops one trailing `/` and
  one trailing `.git`. A local path, a `file://` URL or a URL with no path gives no key.
  - `list` reads the key again from git for each live `git` project, and writes nothing: a read
    never costs an admission. When the read fails, or the root is not a directory, it answers the
    stored key.
  - The stored key is set by `add`, by the kind backfill and by each chat session start, each inside
    a command that is already admitted.
  - The remote URL is never stored, printed, written to a ledger line or put in a JSON field,
    because it can hold a token.
- **The kind backfill.** A project that was added before the kind existed has no kind. At each
  controller start, the controller runs the detection of `add` for each live project with no kind.
  It never guesses.
  - A project it detects gets one admitted `project_kind` command (command ID
    `backfill-kind:<P-id>`).
  - A project it cannot detect keeps no kind, and the cause is reported in four places: a refused
    `project_kind` admission (command ID `backfill-kind:<P-id>:refused`, replayed at the next start,
    so its line appears once); a stderr line at each start, `pulse-factory: project P-1 has no kind:
    <cause>`; `list`, with `"kind": null`, the cause in `kind_error` and the text marker below; and
    the refusal of a chat session start, `project P-1 has no kind (<cause>); remove it and add it
    again`.
  - Each start tries again, so a cause that the operator fixes gives the kind at the next start.
  - A controller ledger that is stopped skips the backfill and prints why. A removed project is never
    backfilled.
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
  `no projects`. A project with no kind ends its line with `  (no kind: <cause>)`. It needs no claim
  and is open to a seat.
- **`remove`** prints `project P-1 removed: <name> (<root>)`. It is refused while the project has a
  live chat session (`project P-1 has a live chat session S-2; end it first`). It marks the project's
  live worktree rows removed in the same command.
- **JSON.** `add` and `remove` print `{"schema_version": 1, "replayed": <bool>, "project": {"id",
  "name", "root", "created_at", "removed_at", "kind", "remote_key", "kind_error"}}`, where
  `removed_at` is null for a live project, `kind` is `"git"` or `"folder"`, `remote_key` is a string
  or null, and `kind_error` is always null in the answer of `add` and `remove`. `kind` is null only
  for a project that the backfill could not detect. `list` gives such a project's cause from a fresh
  detection in `kind_error`, and `kind_error` is null in `list` for any other project. A new field
  does not raise `schema_version`.
  `list` prints `{"schema_version": 1, "projects": [...]}` with the live projects only, in creation
  order. The controller verbs are `project_add`, `project_list` and `project_remove`.
- **Refusals** (exit 1, nothing on stdout, the reason on stderr, one admission and one ledger
  line): a root that does not exist, is not a directory, is `/`, or is held by a live project; a root
  that has no kind (a linked worktree, a bare repository, the inside of a git directory, a repository
  git refuses for dubious ownership, a submodule or `--separate-git-dir` checkout, or a git failure,
  as under "The kind"); a name
  that is empty, longer than 64 characters, holds a control character or a line break, or is held by
  a live project; an unknown or removed ID on `remove`, and a `remove` of a project with a live chat
  session. A command ID that is not valid, and
  `PF_SEAT_TOKEN` set on `add` or `remove`, are refused before any command is admitted: nothing is
  stored.
- **Ledger lines.** `project_add P-1 admitted name=<name> root=<root> kind=<kind> command=<id>`,
  `project_remove P-1 admitted command=<id>`, and `project_add refused reason="<reason>"
  command=<id>`. A value with a space is quoted. The commands that the chat sessions add:
  - `project_kind P-1 admitted kind=<kind> remote_key=<key or none> command=backfill-kind:P-1`, and
    `project_kind refused reason="project P-1: <cause>" command=backfill-kind:P-1:refused`;
  - `chat_session_start S-1 admitted project=P-1 worktree=W-1 path=<root> agent=orchestrator
    command=<id>`. When the start changed the stored remote key, the line ends
    `remote_key=<key or none>` before `command=`. A refused start is `chat_session_start refused
    reason="<reason>" command=<id>`;
  - `chat_session_end S-1 admitted command=<id>`;
  - `chat_item_redact S-1 seq=4 admitted command=<id>`. The line never holds the item's text.

  The verbs that reach the chat commands are the `session` verbs. A chat session start checks the
  root live (a git failure or a gone root is a refusal), records the main worktree at the project's
  first Orchestrator start, allows one live Orchestrator for each project, and starts a new chat
  after an end with a fresh CLI context. A `chat_session_start` that is replayed answers the first
  record and writes no line.

## `session`

```
pulse-factory session start --project <P-n> [--agent orchestrator] [--command-id <id>] [--json]
pulse-factory session send <S-n> <text> [--command-id <id>] [--json]
pulse-factory session answer <S-n> <seq> (--allow | --deny <message> | --answer <question>=<label>...)
                             [--command-id <id>] [--json]
pulse-factory session interrupt <S-n> [--command-id <id>] [--json]
pulse-factory session set <S-n> [--model <m>] [--effort <e>] [--mode <p>] [--option <id>=<value>...]
                           [--command-id <id>] [--json]
pulse-factory session end <S-n> [--command-id <id>] [--json]
pulse-factory session redact <S-n> <seq> [--command-id <id>] [--json]
pulse-factory session list [--project <P-n>] [--all] [--json]
pulse-factory session items <S-n> [--after <seq>] [--json]
```

A chat session is one conversation with a project's Orchestrator, in the project's main worktree
(ADR-0015). The controller runs the chat through the chat port: the Claude Code adapter on the
managed, pinned CLI copy, with the agent's defaults and the chat's own runtime overrides. No verb
waits for a reply: the operator reads it with `session items`, and the app reads it on the API
stream (`api.md`).

- **Commands.** Each state-changing verb is one controller command under `@controller`, with a
  command ID (`--command-id`, else random, as `project add`) and one `controller.ledger` line,
  admitted or refused. The same ID with the same content returns the first record with `replayed`
  true and **makes no port call**: no second send, open or stop. The same ID with other content is a
  conflict that names the ID and changes nothing. The lines, after the kind:
  - `chat_session_start` and `chat_session_end`: as in `project`;
  - `chat_send S-1 admitted bytes=<n> command=<id>`: the text's SHA-256 is the content, and no line
    holds the text;
  - `chat_answer S-1 seq=<n> admitted answer=allow|deny|answers command=<id>`;
  - `chat_interrupt S-1 admitted command=<id>`;
  - `chat_session_set S-1 admitted model=… effort=… mode=… options=<id>=<value>,… command=<id>`;
  - `chat_item_redact S-1 seq=<n> admitted command=<id>`.
- **`start`.** Starts the built-in Orchestrator in the project's main worktree and opens its process,
  so the runtime session ID is stored from the first moment and the live controls work before the
  first message. The launch check refuses an agent whose runtime is not on this server (`no chat
  runtime <id> on this server`), a model, effort or permission mode the runtime does not declare, an
  unknown runtime option, a gone worktree and a cap that is full. A second live Orchestrator for the
  project is the store's refusal, which names the live `S-<n>`: follow that one. A failed open after
  the admission leaves the session live with no process, adds a `notice` item and answers an error
  with the adapter's text, which names the fix; the next `session send` tries again.
- **`send`.** The text is 1 byte to 256 KiB. Refused, each as a refused admission: an unknown or ended
  session, a session marked "context lost", no room under the cap when the session has no process,
  and a gone worktree at the reopen. Admitted: the controller opens the process when there is none
  (`--resume` of the stored runtime session ID when the session has had a message, else a fresh open
  that stores the new ID), stores an `operator_message` item and sends the
  text. During a turn the send steers it (`mid_turn` `steer`, shown in the runtime record). A failure
  after the admission is the answer's `note` and a `notice` item; the command stays admitted, so a
  retry needs a new command ID.
- **`answer`.** `<seq>` names a pending `approval_request` or `question_request` of a live session
  with a process. `--allow` and `--deny <message>` answer an approval (the message has no line break
  and at most 1000 characters). `--answer <question>=<label>` answers a question; repeat it for each
  question. The last `=` with no backslash before it separates the two. Write `\=` for an `=` inside
  the question or the label, and `\\` for a backslash before one (a backslash before any other
  character stays as it is). A `send` text, a `--deny` message and an `--option` value may start with
  `-`. Refused: an unknown, answered or closed request, the wrong kind of answer, and a session
  with no process.
- **`interrupt`.** Ends the running turn. With no turn it is admitted and does nothing.
- **`set`.** At least one field. Each value is checked against the runtime's declared options, and a
  bad value or an undeclared option ID is refused with the declared values. The values are stored on
  the session row, so they survive a release and a resume, and the agent's defaults never change.
  Two overlapping `set` calls each keep their own options: the store merges the `--option` values
  into the session's map key by key at the commit.
  With a live process, a changed option that the runtime declares `live` (model, effort and permission
  mode for Claude Code 2.1.319) applies at once. An option that is not live (`artifacts`) shows in
  `pending_options`, and the next `send` to an idle process (no turn, no task, no pending request)
  stops it (`chat stop reason=option`) and reopens it with the new value first. A live change that
  fails stops the process and adds a notice; the next send reopens it.
- **`end`.** Stops the process and its group, closes each pending request as `lost` (`the session
  ended`) and ends the session. A replay stops nothing. A later `send` is refused.
- **`redact`.** Redacts one stored item. It is CLI-only: it is not on the API allowlist. An `item`
  frame with the redaction marks goes to open streams. A device that read the item before keeps its
  copy.
- **`list`.** The live sessions of every project, or of one, in creation order. `--all` adds the
  ended ones. One line each:
  `S-1  P-1 alpha  W-1 main  idle  claude-opus-5-5/high/bypassPermissions  $0.003465  process since <t>, releases at <t>`,
  with `unqualified` for an override binary, `context lost: <why>`, and the holders when they apply:
  `waiting for an answer since <t>`, `background task <id> since <t>` and `turn since <t>`. The
  branch is read live from the worktree. The JSON is `SessionList`: `{"schema_version": 1, "server":
  {"id", "name"}, "runtimes": […], "agents": […], "sessions": […]}`, where a runtime record holds the
  declared options, `mid_turn`, `ready` and `error`, an agent holds no env value, and a session record
  is the `ChatSessionRecord` of `api.md` (usage, `last_turn_at`, `cache_ttl_seconds`, the context
  gauge, `pending_options`, `process` with its holders).
- **`items`.** The stored items after `--after` (default 0), at most 500. The text form prints one
  line each: the `seq`, the kind and a one-line summary. A `start_card` prints `start <spine>
  proposal=<id> items=<n> most_launches=<n>`, and a `start_result` prints `<outcome> answers <seq>
  run=<run>`.
- **The JSON of the others.** `send`, `answer`, `interrupt` and `set` answer `{"schema_version": 1,
  "replayed": …, "session": <ChatSessionRecord>, "note": …}`, where `note` appears only when an
  admitted command could not act. `start` and `end` answer the session shape of `project`'s chat
  commands, with the same optional `note`. The text forms are one line each (`session S-1 started in
  /path (worktree W-1)`, `sent to S-1`, …).
- **The lifetime.** One live process for each chat. A process is *not idle* while a turn runs, a
  background task runs or a request is pending. A process that is none of these for 30 minutes is
  released: it is stopped, a `notice` item says so, and `chat stop reason=idle` is written. Nothing
  reopens a released process except the operator's next `send`: no timer, tick or notice does. The
  cap is `PF_CHAT_MAX_LIVE` (default 4). An open that would pass it is refused, no process is
  released early to make room, and the refusal names every live chat and why it holds its process:
  `the chat process cap (4) is reached: S-3 waits for an answer since <t>; S-5 holds background task
  <id> since <t>; S-7 runs a turn since <t>; S-8 is idle until <t>; end one with pulse-factory
  session end`. A pending card keeps its process live, so it counts against the cap. `halt`, `end` and
  a controller stop (`chat stop reason=shutdown`) stop the processes, and the unit states
  `KillMode=control-group` so that systemd ends any child left. A controller start sets each live
  session `idle` and closes its pending requests as `lost`; the next `send` resumes it.
- **Context lost.** When the runtime cannot resume a session (its transcript is missing), the
  session is marked `context lost: <why>`, a `notice` item says so, and `send` is refused at once.
  Nothing opens a fresh context for a session that has had a message (an `operator_message` item).
  A session with no message opens fresh at its first `send` and stores the new runtime session ID,
  because the runtime has no transcript to resume (operator ruling on PR #41, class C1). End the
  session and start a new one. The mark is in memory: after a controller restart the next `send` tries the
  resume again and marks it again.
- **The cap on self-started turns.** A turn the chat starts on its own (a background task finished,
  cause `task`, or a controller notice, cause `notice`) counts. The operator's next message, and any
  operator turn, sets the count to 0. With the cap `chat.self_turn_cap` of N (see `settings`; default
  3, `0` is off), N such turns run whole after each operator message, and the controller cuts the
  next one as it starts: it interrupts the turn, stops the process and its background tasks, closes
  each pending request as `lost`, writes one `chat.ledger` line `stop session=S-1
  reason=self_turn_cap self_turns=<cap+1> cap=<cap> from=<scope>`, adds a `notice` item with the code
  `self_turn_cap` and sets the session `needs_you`. The interrupted turn's late `turn_result` is still
  stored and keeps the state. The operator's next `send` finds no process, opens one with `--resume`
  and starts the count at 0; the state becomes `working`. `halt` still stops a live process
  mid-turn (`reason=halt`, the session `idle`) and leaves a session that the cap holds `needs_you`.
  A controller restart settles every live session to `idle`, so the `needs_you` of a cap stop does
  not survive a restart (its notice item does).
- **Who may call.** A caller with `PF_SEAT_TOKEN` is refused every state-changing `session` verb. The
  controller checks the caller again: it refuses a caller that is, or descends from, a seat **or a
  live chat process**, so an Orchestrator cannot drive its own chat or another one. A paired device
  acts as the operator over the API (`api.md`), on the allowlist.

## `settings`

```
pulse-factory settings get [<key>] [--project <P-n>] [--agent <id>] [--json]
pulse-factory settings set <key> <value> [--project <P-n>] [--agent <id>] [--command-id <id>] [--json]
pulse-factory settings unset <key> [--project <P-n>] [--agent <id>] [--command-id <id>] [--json]
```

A setting is one value of one key at one scope, in the store's `settings` table (ADR-0003,
ADR-0016). The operator sets how a chat behaves with no restart. A key is read for a context: a
project, an agent, both or neither. The settings are not the controller's environment (see "The chat
settings" in the common rules).

- **The scopes.** The server default, one project, one agent, or one agent in one project. A context
  reads them most specific first, in the words
  "project and agent, project, agent, server, then the built-in": the pair `(project, agent)`, then the project for every agent, then the agent for every
  project, then the server default, then the binary's own value. A project value is the operator's
  choice for that project and every agent in it, so it sits above an agent value. A context with no
  project skips the two project scopes, and one with no agent skips the two agent scopes.
- **The keys.** Exactly three in this release. Any other key is refused: `unknown setting <key>; the
  settings are chat.deny_commands, chat.self_turn_cap, chat.propose_per_min`. The confirm code, the caller checks and the
  claim are authority floors, not settings, and never become keys.

  | Key | Value | Built-in | Merge | A change applies |
  |---|---|---|---|---|
  | `chat.self_turn_cap` | a JSON integer, 0 to 100; `0` is off | `3` | `override` | at the next turn the chat starts on its own, with no relaunch |
  | `chat.deny_commands` | a JSON array of 0 to 32 command patterns, no duplicates | the fifteen patterns below | `override` | at the chat's next launch (`chat.ledger` `open`, `deny=`, `deny_from=`) |
  | `chat.propose_per_min` | a JSON integer, 0 to 60; `0` is off | `4` | `override` | at the next `propose`, with no relaunch |

  `chat.propose_per_min` bounds the plans one chat session builds in a minute (see `propose`). It
  is read for the session's project and agent, and every `propose` call counts as a build, a refused
  one too.

  A pattern is words, and `*` stands for any text. It is 1 to 100 characters of letters, digits,
  space, `.`, `_`, `/`, `:`, `=`, `@`, `+`, `-` and `*`, starts with a letter or a digit, has no
  trailing space and no two spaces in a row, and holds `*` only as a whole word or as the last
  character of a word. It holds no `:*` anywhere: Claude Code reads a rule that ends `:*` as its
  prefix form, which would deny every call. The built-in list, in order: `git push -f*`,
  `git push * -f*`, `git push --force*`, `git push * --force*`, `git push * +*`,
  `git push --delete`, `git push * --delete`, `git push -d`, `git push * -d`, `git branch -D`,
  `git branch * -D`, `gh pr merge`, `gh -R * pr merge`, `gh --repo * pr merge` and
  `gh --repo=* pr merge`. The list is a speed bump, not a boundary (`chat-adapter.md`).
- **The merge mode.** Each key declares how the rows of its scope chain combine. Both keys are
  `override` now: the first row on the chain wins whole, and every other stored row on the chain is
  masked. `union` (for lists) and `min` (for numbers) are reserved for later keys, and need no
  migration, because the store keeps every scope's row as it is. A stored value that this binary
  cannot read (a hand edit) is skipped with one stderr line, and the next scope decides. A stored key
  that this binary does not know is ignored. There is no cache: a change applies at the next read.
- **`get`.** With no key, one record for each key; with a key, that one. `--project` and `--agent`
  name the context, and must be a live project and a known agent. The text form is one line for each
  key, `chat.self_turn_cap 3 (built-in)` or `chat.deny_commands ["cargo publish"] (project P-1)`,
  then one indented line for each value the winner masked, such as `  masks agent:orchestrator
  ["npm publish"]`. The JSON is `SettingsView`: `{"schema_version": 1, "project_id", "agent_id",
  "settings": [SettingRecord…]}`. A `SettingRecord` is `{key, value, from, built_in, merge, applies,
  layers, masked}`: `from` is `{kind, project_id, agent_id}` with `kind` one of `server`, `project`,
  `agent`, `project_agent` or `built_in`; `merge` is `override`; `applies` is `next_turn` or
  `next_launch`; `layers` is every stored row on the chain, most specific first (`scope`, `value`,
  `set_by`, `set_at`); `masked` is the layers the resolved value hides. `get` is a read and is open to
  every caller, a chat included.
- **`set` and `unset`.** `<value>` is JSON text (`5`, `'["gh pr merge"]'`); text that is not JSON is
  refused by the CLI before any call. A value that starts with `-` is taken as it is, so the
  controller refuses `-1` with the rule. With neither `--project` nor `--agent` the scope is the
  server. `set` replaces the row at that scope and key. `unset` deletes it, so the next scope or
  the built-in applies. The text answer names the scope and when it applies:
  `chat.deny_commands set at project P-1; it applies at the chat's next launch`. The JSON is
  `SettingChanged`: `{"schema_version": 1, "replayed", "key", "scope", "value", "resolved":
  SettingRecord}`, with `value` null after an unset.
- **Commands.** Each `set` and `unset` is one controller command under `@controller`, kind
  `setting_set` or `setting_unset`, with a command ID (`--command-id`, else random) and one
  `controller.ledger` line, admitted or refused. The content is the canonical JSON of the key, the
  scope and the value: who called is not content, so the same ID and content from the CLI and from a
  device replays. A replay returns the first record with `replayed` true and writes no line; the same
  ID with other content is a conflict that names the ID. The lines:
  - `setting_set <key> admitted scope=<scope> value=<json> by=<cli|D-n> command=<id>`;
  - `setting_unset <key> admitted scope=<scope> by=<cli|D-n> command=<id>`;
  - the usual `setting_set refused reason=… command=<id>` for a refusal.

  `<scope>` is `server`, `project:P-1`, `agent:orchestrator` or `project:P-1/agent:orchestrator`. The
  stored row's `set_by` is `cli` or the device ID. A refusal is one refused admission and one line,
  and changes no row: an unknown key; a value that fails the rule of its key (`-1`, `101`, `"3"` and
  `3.5` for the cap; a pattern with `:*`, more than 32 entries or a duplicate for the list); an
  unknown or removed project; an unknown agent; an `unset` of a value that is not set (`no value of
  <key> is set at <scope>`). A stopped controller ledger refuses both writes, and the reads still
  work.
- **Who may call.** `get` is open. `set` and `unset` are the operator's: the CLI, and a paired device
  over the API (`api.md`, `by=D-<n>`). The CLI does not refuse them under `PF_SEAT_TOKEN`: they always
  reach the controller, whose caller check refuses a caller that is, or descends from, a seat or a
  live chat process. A refused caller is audited, with a bound, so it gets no unbounded durable
  writes. The controller reads the caller's environment once and names the caller: `seat-token`
  for any caller with `PF_SEAT_TOKEN`; else, for a caller that holds `PULSE_FACTORY_CHAT`, `chat:<S-n>`
  when the value is `S-` and digits (at most 20 characters) and `chat:invalid` for any other value
  (a marker is audited even when the caller left its chat by a double fork, and the raw value is never
  written to a ledger or to a refusal text); with no marker, the walk of the ancestors gives
  `chat:<S-n>`, `seat:<pid>` or `unknown`. The bucket is the key the arguments name
  (`chat.self_turn_cap`, `chat.deny_commands`, `chat.propose_per_min`, or `other` for an unknown key
  or arguments that do not parse). A refused write is answered at once. For each caller and bucket, one window of a
  minute holds the audit:
  - the window's first refusal commits one refused admission under a command ID that the controller
    makes, `caller-refused-<32 hex>`, so the caller's own ID is never used up, and writes its line at
    once: `setting_set refused caller=chat:S-1 bucket=chat.self_turn_cap refused=1 reason="…"
    command=caller-refused-<hex>`;
  - later refusals in the window write nothing, and the controller counts them;
  - at the tick after the window, or at the caller's next refusal after it, a window that counted any
    commits one more refused admission and line: `… refused=<n> window=<start UTC>..<end UTC>
    reason="<the last refusal's text>" command=caller-refused-<hex>`, where `refused` counts the
    refusals after the first.

  So a caller gets at most two refused admissions and two lines for each bucket in a minute, eight
  across the four buckets, whatever it sends. A crash or a restart loses the count of an open
  window. A stopped ledger, or a random source that cannot be read, still refuses the caller and
  prints one stderr line for each window, in place of the ledger line.
- **What the check guarantees, and what it does not.** It refuses a caller that is honest about where
  it runs. The chat runs as the operator's user, so a process it starts can evade the check:
  `env -u PF_SEAT_TOKEN`, a double fork that leaves the ancestor chain, a rewrite of its own `/proc`
  environment, or a direct write to the store file. "A chat cannot raise its own cap" holds only for a
  chat that does not evade the check. This is a known limit; its owner is the Release 2
  authority-rules spine.

## `chat-runtime`

```
pulse-factory chat-runtime install [--json]
```

Fetches the managed Claude Code copy, checks it against the pin and places it, with no controller
running. The binary is Anthropic's official per-platform package at the pinned version: the
controller never ships it, and it never runs a `claude` found on `PATH`. The fetch uses `curl` from
the registry at `$PF_CHAT_REGISTRY` (default `https://registry.npmjs.org`), unpacks `package/claude`,
checks its size and SHA-256 against the pin and places it, mode 0755, at
`$PF_SHARE_DIR/claude/<version>/claude`. A copy that matches the pin is not fetched again. A failed
download, a tarball with no `package/claude`, and a size or SHA-256 that differs from the pin each
place nothing, leave an existing file as it was and print the cause and the URL; the exit is 1.
Success prints `the chat runtime Claude Code <version> is installed at <path>`. The JSON is
`{"schema_version": 1, "installed": true, "version": "<version>", "path": "<path>"}`, or, with exit 1,
`{"schema_version": 1, "installed": false, "error": "<cause and URL>"}` on stdout. A controller with a registry set checks the copy at its start on a background thread
and runs the same fetch when it is missing or altered; until a good copy is in place, a chat open is
refused with the adapter's text, which names this verb.

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
  that the controller finds to be, or to descend from, a seat or a chat process, or to hold
  `PULSE_FACTORY_CHAT`. An invalid command ID is refused
  before any command is admitted.
- **Ledger lines.** The redeem of a code is the command `device_pair`:
  `device_pair D-1 admitted name=<name> peer=<address>`. A revoke also voids the device's live confirm codes. A revoke is
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
