# ADR-0003 — The controller owns the run store behind a store port

- **Status:** Proposed
- **Date:** 2026-10-25

## Context
The controller is one control authority over a local SQLite store. ossify's state is the source of
truth for a spine, and Pulse Factory reads it through `oss`. The ledger is one file per spine in the
AI workspace. The operator decided on 2026-10-25:
- answer data ownership now;
- plan the store schema properly in the release after Release 0;
- put the store behind an adapter, so Pulse Base (SQLite plus PulseDB vectors) can replace the
  direct SQLite store later.

## Decision
| State | Owner | How Pulse Factory uses it |
|---|---|---|
| Runs, nodes, sessions, requests, claims, admissions, and each session's token usage and cost | The controller, through the store port | Reads and writes. The first adapter is `crates/pf-store-sqlite`: rusqlite, one serialized writer, WAL, `synchronous=FULL`. |
| Spine plan, rounds, items, spine state | ossify | Reads through `oss` at each lane step. Never copies it. |
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
  Pulse Base offers a store API.
