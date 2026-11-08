# ADR-0003 — The controller owns the run store behind a store port

- **Status:** Accepted, 2026-11-05, at the Release 0 close
- **Date:** 2026-10-25
- **Amended:** 2026-10-26, at the planning of spine r0.s1, with text from its review round 1 on
  2026-10-31: the spine-plan row and the run queue row.
- **Amended:** 2026-11-07, at `r1.s2.w1`: projects, the `@controller` admission scope and the
  controller ledger. The revisit trigger changes. Nothing else above changes.
- **Amended:** 2026-11-07, at `r1.s2.w2`: devices (migration 0004), the device commands under the
  `@controller` scope, and the writes outside admission. Nothing else above changes.
- **Amended:** 2026-11-08, at `r1.s3.w2`: the project hierarchy (migration 0005), the chat sessions'
  display log, the new `@controller` commands, a second ID placeholder and four more writes outside
  admission. The revisit trigger changes. Nothing else above changes.

## Context
The controller is one control authority over a local SQLite store. ossify's state is the source of
truth for a spine. A seat reads it through `oss` and writes unit records into the run's queue; Pulse
Factory takes its units from those records (ADR-0002). The ledger is one file per spine in the
AI workspace. The operator decided on 2026-10-25:
- answer data ownership now;
- plan the store schema properly in the release after Release 0;
- put the store behind an adapter, so Pulse Base (SQLite plus PulseDB vectors) can replace the
  direct SQLite store later.

## Decision
| State | Owner | How Pulse Factory uses it |
|---|---|---|
| Runs, nodes, sessions, requests, claims, admissions, and each session's token usage and cost | The controller, through the store port | Reads and writes. The first adapter is `crates/pf-store-sqlite`: rusqlite, one serialized writer, WAL, `synchronous=FULL`. |
| Spine plan, rounds, items, spine state | ossify | A seat reads it through `oss` and writes one unit record per item into the run's queue. Pulse Factory's code never calls `oss`: a command such as an `oss` status write is data in the workflow file, which the engine runs (ADR-0002). It never copies the plan: a record holds only what launches a unit. |
| Run queue (unit records) | The controller owns the directory, under the run's state. The seats the workflow names write to it. | A seat publishes a record by atomic rename. The controller admits each record once by its unit ID. The same ID with changed content, or a malformed record, parks the run (ADR-0006). The queue stays with the run's state. |
| Ledger | One append-only file per spine in the AI workspace. The controller is its only writer. | Seats write their decisions to their own run files. The controller appends each one to the ledger as a whole line, in admission order, and never rewrites a line. The controller never commits. Spine close's second pass commits the ledger with the spine's other AI-workspace records, through that workspace's PR flow. |
| Worktrees, handoffs, reports, stop files | The seats and ossify | Reads them as done signals. |
| Transcripts and token usage | Each agent's own logs | Reads them. |

**Migration posture:**
- Store migrations are numbered, forward-only, and recorded with a schema version.
- The controller refuses to migrate while a run is active. There is no active-run migration.
- The release after Release 0 plans the schema in detail (feature map: "store schema design").

Rejected alternatives:
- **Design the schema now.** The operator wants it planned properly, and that belongs to its own
  release. It is due in the release after Release 0.
- **Copy the ossify plan into the store.** That makes two sources of truth.
- **Call SQLite directly from the engine.** That blocks Pulse Base.

## Consequences
- The store can be swapped without engine changes.
- The store port must express one atomic commit: the state transition, its command and receipt, and
  its outbox, together. It must do this without leaking SQLite.

### Unverified claims
- Whether one store port can carry that atomic commit on both SQLite and Pulse Base. Revisit when
  the project hierarchy is planned, or Pulse Base offers a store API.

## Amendment, 2026-11-07 (`r1.s2.w1`)

The controller's store gains projects, a second admission scope and a second ledger.

- **Projects.** Migration 0003 adds the table `projects`: an ID (`P-<n>`, never reused), a name, one
  root, `created_at` and `removed_at`. A removed row stays. Two partial unique indexes keep a live
  root and a live name unique. The project schema beyond the root waits for the project hierarchy
  brainstorm (operator, 2026-11-07). The store port reads and writes projects (`ProjectRow`,
  `Effect::AddProject`, `Effect::RemoveProject`, `Store::projects`, `Store::project`).
- **The admission scope.** `admissions.run_id` names the admission scope: a run's ID, or
  `@controller` for the controller's own commands (projects now, devices in `r1.s2.w2`). The scope
  keeps `UNIQUE (scope, command ID)`, so a replay returns the first record and changed content is a
  conflict, as it is for a run (ADR-0009). Migration 0003 adds a trigger that refuses a `runs.id`
  that holds `@`, so no run can take the scope. The scope is one constant of the store port.
- **The controller ledger.** `<state dir>/controller.ledger` takes one line for each admitted or
  refused controller command. It is not a run's ledger and it is not in the AI workspace. It uses
  the outbox, the one writer and the start-up recovery that the run ledgers use.
- **The revisit trigger.** The claim under Unverified claims is revisited when the project
  hierarchy is planned, or Pulse Base offers a store API.

Known limits:
- The controller ledger is operator-local in the state directory, is not committed, and has no
  rotation yet (feature map: "controller ledger rotation").
- Migration 0003, like every migration, refuses under an active run. The controller does not start
  until no run is active, so install with no active run.

## Amendment, 2026-11-07 (`r1.s2.w2`)

The store gains devices, and the `@controller` scope holds device commands too (ADR-0012).

- **Devices.** Migration 0004 adds the table `devices`: an ID (`D-<n>`, never reused), a name, the
  token's SHA-256 (unique), `paired_at`, `last_seen_at`, `last_peer` and `revoked_at`. The store
  never holds a token, only its hash. A revoked row stays. The store port reads and writes devices
  (`DeviceRow`, `NewDevice`, `Effect::AddDevice`, `Effect::RevokeDevice`, `Store::devices`,
  `Store::device_by_token`). `RevokeDevice` must name a live device, and a refusal writes nothing.
- **Device commands are controller commands.** `device_pair` (the redeem of a pairing code) and
  `device_revoke` are admissions under `@controller`, with a command ID and one controller-ledger
  line each, as the project commands are. The same ID with the same content returns the first
  record, and changed content is a conflict. The command ID of a redeem is the SHA-256 of the code's
  secret, so the store sees one secret at most once. A line names the device, its name and the peer,
  and never the code or the token.
- **`touch_device` is a store write outside admission.** `Store::touch_device(id, at, peer)`
  writes `last_seen_at` and `last_peer` with no admission row and no ledger line, at most once a
  minute for a device, and only for a live device. The reason: the API authenticates every call by a
  read of the store, and a read must not cost an admission row or a ledger line per request. Last
  seen is telemetry, not a state transition. Nothing decides on it in this item. The idle expiry of
  `r1.s2.w5` reads it.
- **Migration 0004** follows the posture above: forward-only, and it refuses under an active run.
  Install with no active run.

## Amendment, 2026-11-08 (`r1.s3.w2`)

The store gains the project hierarchy and the display log of the chat sessions (ADR-0015).

- **The hierarchy.** Migration 0005 gives `projects` a `kind` (`git` or `folder`) and a `remote_key`
  (the host and path of the git remote, never a URL), and adds `worktrees`, `agents` (with the
  built-in Orchestrator row), `chat_sessions` and `chat_items`. The hierarchy is server → project →
  worktree → chat session. The kind never changes, a `worktrees` row exists only when a session
  starts there, and a project has at most one live Orchestrator session. Triggers and partial unique
  indexes hold each rule, and the store port checks them first. ADR-0015 records the rules.
- **New `@controller` commands.** `chat_session_start`, `chat_session_end`, `chat_item_redact` and
  `project_kind` are admissions under `@controller`, each with a command ID and one controller ledger
  line, as the project and device commands are. `project_kind` is the kind backfill that runs at
  controller start for a project added before migration 0005. A refusal of any of them is an
  admission of its own. They launch nothing and change no run, so `halt` does not apply to them.
- **A second ID placeholder.** A commit's ledger line and the admission's `unit_id` take the ID that
  the store assigns in place of `NEW_ID`. A `chat_session_start` commit may also record a worktree,
  so its line carries `{worktree-id}` as well. The store replaces it with the `W-<n>` that the start
  used. One commit may therefore assign two IDs: `S-<n>` through `NEW_ID`, and `W-<n>`.
- **The writes outside admission.** There are five, with `touch_device` (above). Each records what
  the file system, git or the runtime reported, and none is an operator command:
  - `touch_device`: telemetry from an authenticated API call;
  - `mark_worktree_gone`: a mark, for display, of what the file system or git showed. The admission
    checks the worktree live and never decides on the mark;
  - `set_chat_session_state`: the runtime's turn state, which changes on every turn. Nothing decides
    on it;
  - `set_chat_runtime_ref`: the id that the runtime reports for its own session;
  - `append_chat_item`: the display log of what the runtime did. A row for each text block must not
    cost an admission and a ledger line.
- **A read never costs an admission.** `project list` reads the remote key live and writes nothing.
  The stored key is set only inside an admitted command: `project_add`, `project_kind` and
  `chat_session_start`.
- **The display log against the CLI transcript.** The store keeps the display log: one row for each
  completed item, for the app. The CLI's own transcript is the model's context, and only `--resume`
  reads it. It is never copied into the store (Q4). Text deltas are never stored.
- **The cap.** A tool call's stored input and output are each capped at 16 KB, with a truncation mark
  and the original size. The store enforces the cap in `append_chat_item`, and a `CHECK` is the
  backstop. The transcript keeps everything whole.
- **Append-only, except one admitted redaction per item.** `chat_item_redact` (C12) is the one
  change to an appended item. It has a command ID and one ledger line that holds no item text, the
  text columns become fixed marks, and a trigger allows that update shape and no other. An item is
  never deleted.
- **Live chat sessions at start.** A restart ends every chat process (ADR-0007). At controller start,
  each live chat session goes to `idle` and keeps `ended_at` empty, and its pending requests are
  closed as lost (Q2 rule 4, C3). These are writes outside admission.
- **The revisit trigger.** The claim under Unverified claims is revisited when the project hierarchy
  is planned, or Pulse Base offers a store API, or custom agents, worktree creation or the
  multi-server app is planned.

Known limit: migration 0005, like every migration, refuses under an active run. Install with no
active run.
