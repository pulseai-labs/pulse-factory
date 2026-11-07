# ADR-0009 — One operator on one host

- **Status:** Accepted, 2026-11-05, at the Release 0 close
- **Date:** 2026-10-25
- **Amended:** 2026-11-07, at `r1.s2.w2`: the operator's paired devices act as the operator, and "no
  headless or API execution" now means no execution beyond the API allowlist. Nothing else above
  changes.

## Context
- The first build supports one operator, one Linux host and a Mac client.
- One logical control authority serializes admissions. Authority transfer is fenced.
- Execution units are interactive terminal sessions of existing coding agents, with no headless or
  API substitute.
- The default concurrency is 2 model sessions plus 1 cold build, because of the host's NVMe write
  budget.

## Decision
- **Trust:** one operator. Trust is the operator's OS user, which ADR-0001's socket enforces.
- **The claim:**
  - one active claim per project;
  - a new claim fences out the old one with a fresh generation;
  - an answer binds to an exact request ID and revision.
- **A deterministic control core:**
  - the same command ID with the same content returns the same record;
  - changed content under a used command ID is a conflict;
  - admissions serialize at commit.
- **Capacity:** at most 2 model sessions plus 1 cold build at once by default, set in the workflow
  settings.
- **No headless or API execution.**

## Consequences
- An operator who opens a second session must claim again there. The new claim fences out the old
  one.
- More throughput needs a raised cap and a measured write budget, not code changes.

## Amendment, 2026-11-07 (`r1.s2.w2`)

ADR-0012 adds paired devices.

- **Trust.** The operator is still the one principal. A device that the operator paired (a one-use
  code, a pinned key and a revocable token) acts as the operator. It has no scope of its own and no
  identity beyond its row in the store (ID, name, last-seen). The operator's OS user remains the
  trust for the socket (ADR-0001), and the operator's authority reaches a device only through
  `pulse-factory device pair`, which a seat cannot run (ADR-0005, amended).
- **No headless or API execution.** The line "No headless or API execution" stays, and its meaning
  is now exact: the API starts no execution. A device calls only the verbs on the allowlist
  (`project_add`, `project_list`, `project_remove`). Those verbs launch no seat and change no run.
  There is no `arm`, `claim`, `answer`, `clear`, `ack` or `halt` for a device. A later verb on the
  allowlist amends ADR-0012, and one that launches a seat or merges a PR also needs the human
  confirm that `r1.s4` plans.
- **The control core is unchanged.** A device command is an admission under `@controller` with a
  command ID. The same ID with the same content returns the same record, and changed content is a
  conflict (ADR-0003, amended).
