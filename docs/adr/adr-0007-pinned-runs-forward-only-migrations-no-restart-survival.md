# ADR-0007 — Pinned runs, forward-only migrations, and no restart survival

- **Status:** Proposed
- **Date:** 2026-10-25

## Context
- Runs stay pinned to their contracts, and a changed contract needs a new run.
- Start-up is reconciliation: it does not replay, and it does not start every recorded executor by
  default.
- A binary rolls back only while schema and protocol stay compatible.
- The operator decided on 2026-10-25 that Release 0 gives no restart guarantee.

## Decision
- At `arm`, each run pins its workflow file's version, and a content-hash manifest of the workflow file
  and every file it references: role briefs and rule sets. A seat is launched only from the pinned
  content. A change to any pinned file needs a new run.
- Store migrations are forward-only (ADR-0003).
- A binary rolls back only to one with the same store schema version. Otherwise the fix goes forward.
- On start-up, the controller enters reconciliation:
  - every unit whose seat process is gone is parked, and a request is opened for it;
  - nothing relaunches by default;
  - the claim decides each relaunch.
- To back out a bad release, reinstall the earlier binary while no run is active.

Rejected alternatives:
- **Restart survival now** (one owner process per seat). It is on the feature map: "seats survive
  controller restart".
- **Event-sourced replay.** The store design rules it out.

## Consequences
- A controller crash costs each in-flight seat its work since that seat last committed or staged.
- The operator decides each relaunch through the claim.
