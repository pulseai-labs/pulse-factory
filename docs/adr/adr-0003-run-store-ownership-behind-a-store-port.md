# ADR-0003 — The controller owns the run store behind a store port

- **Status:** Accepted, 2026-11-05, at the Release 0 close
- **Date:** 2026-10-25
- **Amended:** 2026-10-26, at the planning of spine r0.s1, with text from its review round 1 on
  2026-10-31: the spine-plan row and the run queue row.
- **Amended:** 2026-11-07, at `r1.s2.w1`: projects, the `@controller` admission scope and the
  controller ledger. The revisit trigger changes. Nothing else above changes.

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
