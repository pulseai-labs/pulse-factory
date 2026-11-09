# ADR-0015 — Projects, worktrees, agents and chat sessions: the hierarchy in the controller's store

- **Status:** Proposed, 2026-11-08, at `r1.s3.w2`. It becomes Accepted at the `r1.s3` close.
- **Date:** 2026-11-08
- **Amended:** 2026-11-09, at `r1.s3.w3`: the controller's half of the chat: verbs, processes, pump,
  `chat.ledger`, lifetime, `halt` and the caller rule.

## Context
- Release 1's second exit criterion: the operator chats with a project's Orchestrator from the
  MacBook. A chat session needs a durable home in the store: the project it belongs to, the worktree
  it runs in, the agent it talks to, and the log the app shows.
- ADR-0003 (amended at `r1.s2.w1`) gave the store projects with one root and left the rest of the
  project schema to the hierarchy design. This ADR is that decision for the store. The controller's
  entities stay outside the engine (ADR-0002), and the controller owns them (ADR-0001, ADR-0003).
- The grill of spine `r1.s3` (Q2, Q4, Q5 and Q7) and its planning audit (C2, C3, C4 and C12) fixed
  the rules below. The operator accepted the dispositions. The design source is the project
  hierarchy design of 2026-11-08, §3.0 to §3.3.
- ADR-0014 is `r1.s3.w1`'s chat runtime. This ADR stores `runtime` and `runtime_session_ref` as
  opaque text and does not depend on the chat port.

## Decision

**The hierarchy.** The model is server → project → worktree → chat session.
- **No host column, and the server id on every API response.** Each store belongs to one server. No
  table has a `host` column. The API puts the server id on every response (`r1.s3.w3`), so a client
  that holds several servers keeps them apart without a key in the store.
- **A project has a kind, and the kind never changes.** The kind is `git` or `folder`. A `git`
  project is the main worktree of a repository. A `folder` project is a plain folder, or a
  subdirectory of a repository (its git operations would reach outside it, so it is a folder). A new
  project always has a kind. A trigger holds both rules: no insert without a kind, and no change of a
  kind once set. After `git init` in a folder project, the operator removes the project and adds it
  again.
- **The refusals.** `project add` refuses a root that is neither kind, with the cause: a linked
  worktree (the reason names its main worktree), a bare repository, the inside of a git directory, a
  repository that git refuses for dubious ownership, and any other git failure. A failed detection is
  never `folder`. Git runs as a subprocess with a 10-second timeout, with the controller's own `GIT_*`
  variables removed.
- **The remote key.** A `git` project has a `remote_key`: the host and path of its remote, such as
  `github.com/pulseai-labs/pulse-factory`. The remote is `origin`, else the one remote, else none.
  The key drops the user, the password and the port, lowercases the host, and drops one trailing `.git`.
  The remote URL is never stored, printed, written to a ledger line or put in a generated type,
  because it can hold a token.
- **Where the stored remote key is set.** There are three places, each inside a command that is
  already admitted: `project add`, the kind backfill at controller start, and each chat session
  start. A `project list` reads the key live and writes nothing, because a read never costs an
  admission (`r1.s2.w2`'s rule).
- **Git is the truth for worktrees.** A `worktrees` row exists only when a session starts there. The
  main worktree row (`created_by = root`) is recorded at the project's first Orchestrator start,
  never at `project add`. Each start checks the worktree live, from the file system and from
  `git worktree list --porcelain -z`. An entry marked `prunable` counts as not listed. A git failure
  that is not "not a repository" refuses the start and is no gone mark. The first read that sees a
  recorded row gone sets `gone_at`, for display. The admission never decides on the mark. A worktree
  that comes back at a gone path gets a new row.
- **A project has one live Orchestrator per server.** A partial unique index holds it. A second
  start is refused and names the live session. Two clones of one repository on one server are two
  projects (open item A).
- **Two session tables.** Run sessions and chat sessions are separate tables. One table with a
  nullable `run_id` is rejected below.
- **Live means `ended_at` is empty.** The end of a session is the only record of an end.
- **After an end, a new chat starts with a fresh CLI context** (open item B). The new row has no
  `runtime_session_ref`. Nothing copies the old one.

**"Live" does not depend on a process (Q2 rule 4, C3).**
- A controller restart ends every chat process (ADR-0007: no restart survival).
- At controller start, each live chat session goes to `idle` with no process. Its `ended_at`,
  `runtime_session_ref`, model, effort and permission mode stay.
- Each pending request of such a session gets an answer item with the outcome `lost`. The process
  that asked is gone, so the app must not show a card that can never be answered.
- The next message reopens the session with `--resume <runtime_session_ref>` (`r1.s3.w3`), when the
  session has had a message. A session with none opens fresh (C15, operator ruling on PR #41).
- So a live row after a restart is a session that waits to resume, not a zombie. The
  one-live-Orchestrator index holds by design.

**Agents (P1, P5).**
- The `agents` table holds the fields of the 6a design. The Orchestrator is a seeded built-in row:
  runtime `claude-code`, model `claude-opus-5-5`, effort `high`, permission mode `bypassPermissions`
  and the runtime option `{"artifacts":"on"}`.
- **The NULL-instructions rule.** `instructions` is NULL for a built-in agent. The persona text
  comes from the binary (`r1.s3.w3`), so a new persona needs no migration.
- **The launch-env rule.** The `launch_env` map holds names and values that are not secrets. They
  reach the store, the paired devices and any ledger line that names them. A secret, or a value
  that belongs to the host, goes in a server setting (P4: the Orchestrator's `SEAT_MODS_ROLE`). The
  built-in row ships `{}`. A later release that writes agents applies this rule to each write.
- There is no create or edit screen and no other agent in `r1.s3`.

**The session's runtime overrides (P3).** A chat session row keeps its current model, effort and
permission mode. A new session starts from the agent's defaults. `SetChatSessionRuntime` is the store
side. `r1.s3.w3` owns the command that commits it, because the values must be checked against the chat
port's options.

**The commands.** Each is one admission under `@controller`, with a command ID and one controller
ledger line. A refusal is an admission of its own. They launch nothing and change no run, so `halt`
does not apply to them (`r1.s3.w3` extends `halt` over the live chat processes).
- `chat_session_start`: checks the worktree live, records the worktree when the project has none
  live at its root, refreshes the stored remote key, and starts the session `idle`.
- `chat_session_end`.
- `chat_item_redact` (C12).
- `project_kind`: the backfill of a project added before migration 0005, at controller start. A
  project it cannot detect keeps no kind and is reported in four places: a refused `project_kind`
  admission, a line on stderr at each start, `project list`, and the refusal of a session start. It
  never guesses, and each start tries again.

**The second ID placeholder `{worktree-id}`.** A session start may record a worktree and a session
in one commit. The store replaces `{new-id}` with `S-<n>`, and `{worktree-id}` with the `W-<n>` that
the start used. This is a port-contract change beside `NEW_ID`. ADR-0003's amendment records it too.

**The writes outside admission.** There are five. Each records what the file system, git or the
runtime reported, and no operator command is among them.
- `touch_device`: telemetry from an API call that has been authenticated. A read must not cost an
  admission (`r1.s2.w2`).
- `mark_worktree_gone`: a mark for display of what the file system or git showed. The admission
  checks the worktree live and never decides on the mark.
- `set_chat_session_state`: the runtime's turn state, which changes on every turn. Nothing decides
  on it.
- `set_chat_runtime_ref`: the `runtime_session_ref`, the id that the runtime reports for its own
  session. It is a fact of the runtime, not an operator's command.
- `append_chat_item`: the display log of what the runtime did. A row for each text block must not
  cost an admission and a ledger line. Its usage sums are the runtime's own report.

The one change to an appended item is a command, so it is not among the five: `chat_item_redact`.

**The display log against the CLI transcript (Q4).**
- The store keeps the chat's display log: one row for each completed item, for the app. The CLI's
  own transcript is the model's context. Only `--resume` reads it, and it is never copied into the
  store. Text deltas are never stored.
- A tool call's stored input and output are each capped at 16 KB (`TOOL_TEXT_MAX_BYTES`), with a
  truncation mark and the original size. The store enforces the cap, so no caller can skip it. The
  transcript keeps everything whole.
- An approval or a question is stored when it is requested. Its answer is a second item that names
  the request's `seq`. A reconnect then shows the card as pending (C2). An answer's outcome is
  `answered`, `lost` or `withdrawn`.
- **Append-only, except one admitted redaction per item (C12).** The command is `chat_item_redact`.
  It has a command ID and one ledger line that holds no item text. The update has one shape, which a
  trigger allows and nothing else: `redacted_at` goes from NULL to a value, `payload` becomes
  `{"redacted":true}`, each text that was not NULL becomes `[redacted]`, and every other column stays.
  A redacted request stays a request, so its answer still pairs.
- The redaction is CLI-only: `r1.s3.w3` exposes it as a CLI verb and not on the API allowlist, so a
  device cannot rewrite the record. It covers the store's display log. It leaves the CLI's own
  transcript under `~/.claude`, and the copies that paired devices already received, as they are.
- Known limits: a turn run in a terminal is not in the app's history, and a secret that a tool
  prints reaches the store and the paired devices.

**Known limit: submodule and `--separate-git-dir` checkouts.** `project add` refuses both, and the
refusal names the cause. A submodule checkout is not a main worktree. Treat them as `git` when a
project needs one.

## Rejected alternatives
- **One session table with a nullable `run_id`.** A run session and a chat session differ in
  lifetime, owner and state. One table would hold constraints that fit neither.
- **A `host` column.** Each store belongs to one server, and the server id on every response tells
  clients apart. A column would copy a fact the store cannot check.
- **Pulse Factory's own worktree list.** Git is the truth for worktrees. A second list drifts from
  it.
- **One Orchestrator across servers.** The Orchestrator runs on the server that holds the project.
- **Storing the CLI transcript.** It is the model's context, and `--resume` reads it already. A copy
  is a second source of truth and a second place for a secret.
- **Storing text deltas.** The app shows completed items. Deltas are the stream's.
- **A `CHECK` on the chat-item kind.** The kind is a closed list in the store port. A later kind then
  needs a port change and no table rebuild.
- **A `project list` that commits a changed remote key.** A read never costs an admission. The list
  answers the live key, and the next session start stores it.
- **A fully immutable display log.** A leaked secret could never be removed.
- **Deleting an item to redact it.** A gap in `seq` would break the reconnect.

## Consequences
- The store holds the hierarchy, and a project's chat has a home that survives a restart without a
  process.
- The controller's ledger gains four commands (and `r1.s3.w3` adds four more). `project list` costs no admission.
- The port has one more ID placeholder and five writes outside admission. Each write names the
  reason it is not a command, so a later write can be judged against them.
- A leaked secret in the display log can be removed with one admitted command, and the removal
  leaves a record that holds no secret.
- The project JSON gains `kind`, `remote_key` and `kind_error`. `schema_version` stays 1, because a
  new field does not raise it (ADR-0004).

### Unverified claims
- That the Orchestrator's model id `claude-opus-5-5` is accepted by the Claude Code CLI. The planning
  audit grounded it from the ids of baton's seats and the headless probe. `r1.s3.w1`'s probe confirms
  it, and `r1.s3.w3` checks the pair before the first launch.
- That `git worktree list --porcelain -z` marks every deleted linked worktree `prunable` on the git
  versions of the operator's hosts. It was probed on git 2.53 on draco-desk, 2026-11-08. The live
  check also asks the file system, so a deleted directory is gone either way.
- That a restart leaves no chat process alive. ADR-0007 states it. `r1.s3.w3` stops each chat process
  at `halt` and at an end, and this ADR relies on the restart.


## Amendment, 2026-11-09 (`r1.s3.w3`): the controller's half of the chat

`r1.s3.w3` adds the verbs, the processes and the stream that sit on the hierarchy.

- **The verbs and their admissions.** `session send`, `answer`, `interrupt` and `set` are the commands
  `chat_send`, `chat_answer`, `chat_interrupt` and `chat_session_set`, beside `chat_session_start`,
  `chat_session_end` and `chat_item_redact`. Each is one admission under `@controller` with a command
  ID and one `controller.ledger` line, admitted or refused. A replay returns the first record and
  makes no port call. A `chat_send` stores the SHA-256 of its text, so the admission row and the line
  hold no text. A start, a send and a restart are the only things that open a process: a turn begins
  only on an admitted send or when a background task the chat started finishes.
- **The runtime overrides on the row (P3).** A session keeps its own model, effort, permission mode and
  runtime options. They start as the agent's, change with `session set`, survive a release and
  `--resume`, and never change the agent. Migration 0006 adds `chat_sessions.runtime_options`, a JSON
  object of text values, default `{}`; a session that was live before it launches with its agent's
  options.
- **The persona is in the binary.** The built-in Orchestrator's `instructions` stays NULL in the row.
  The controller passes the persona as the open's instructions. It names the project, the root, the
  server and the session, holds no time, and names no plugin, hook or role. The server's own
  environment setting `PF_AGENT_ENV_<AGENT>` is the interim for an agent's environment (P4).
- **The launch check and the degrade (N5).** Before the first launch and before each open, the agent's
  runtime must be on the server, and its model, effort, permission mode and each runtime option must
  pass the runtime's declared options. A runtime option whose stored value the runtime no longer
  offers is not a refusal: the controller passes it, the adapter launches with the tools off, and the
  controller prints one line. The degrade wins over the seed.
- **The event pump and the item bodies.** One pump for each live process turns the adapter's events
  into display-log items in event order: `assistant_text`, one `tool_call` for each finished call (the
  capped texts byte for byte, with their size and flag), `approval_request` and `question_request`
  (stored when asked, so a reconnect shows the card as pending), an answer item (`answered`, `lost` or
  `withdrawn`), `background_task`, `turn_result` with the turn's own usage, and `notice`. Deltas go to
  the stream only. The session's cost is the exact sum of its turns. A tool open at an exit is stored
  with no output and `finished: false`. The store's cut calls the adapter's `cap`, so one truncation
  mark serves the adapter, the store and the stream.
- **`chat.ledger`.** `<state dir>/chat.ledger` records one line for each process open, each turn (with
  its usage and cost, self-started turns included) and each stop (`end`, `idle`, `halt`, `exited`,
  `shutdown` or `option`). It is append-only, like `api.ledger`, and holds no message, tool text,
  token or key. The store's `turn_result` items are the durable record.
- **The idle release and the cap (Q2 rules 1 to 3, C2, C7).** A process with no running turn, no
  background task and no pending request is released after 30 idle minutes. A pending card keeps its
  process and counts against the cap. The cap is `PF_CHAT_MAX_LIVE` (default 4). A refused open names
  every live chat and why it holds its process, with since when, and no process is released early to
  make room. A released process is reopened only by the operator's next `send`.
- **`halt`, a stop and `KillMode=control-group` (C3).** `halt` stops every live chat process and its
  group, closes each pending request as `lost`, adds a notice and leaves each session live and idle.
  With no run it still stops the chats. A controller stop (SIGTERM or SIGINT) stops every process, and
  the unit states `KillMode=control-group`, so systemd also ends any child left in its cgroup, such as
  a background command in its own session. A controller start settles each live session to `idle`.
- **Context lost (C15).** `ResumeFailed` marks the session "context lost" with the adapter's text, adds
  a notice and refuses `send`. Nothing opens a fresh context for a session that has had a message.
  A session with no message has no conversation to lose: the runtime writes no transcript before the
  first message, so the controller opens it fresh and stores the new runtime ID (operator ruling on
  PR #41, class C1). The mark is in memory, and the next resume attempt after a restart sets it again.
- **The caller rule over chat processes.** The controller refuses a state-changing chat verb from a
  caller that is, or descends from, a seat or a live chat process (`ChatPort::process_id`), so an
  Orchestrator cannot drive its own chat or another one (Q2 rule 2).
- **The stream.** The paired device reads the chat on `/v1/stream` (ADR-0012, amended). A revoke closes
  its streams.

Known limits: a turn run in a terminal stays out of the app's history; a secret that a tool prints
reaches the store, the stream and the paired devices; `chat.ledger` can lose its last lines in a crash.

Revisit this ADR when custom agents, worktree creation or the multi-server app is planned.
