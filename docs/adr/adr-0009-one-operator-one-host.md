# ADR-0009 — One operator on one host

- **Status:** Accepted, 2026-11-05, at the Release 0 close
- **Date:** 2026-10-25
- **Amended:** 2026-11-07, at `r1.s2.w2`: the operator's paired devices act as the operator, and "no
  headless or API execution" now means no execution beyond the API allowlist. Nothing else above
  changes.
- **Amended:** 2026-11-09, at `r1.s4.w1`: a device reads requests and answers, clears and acks with a
  confirm code, taking the claim as it acts; the claim is one for the whole store.

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

## Amendment, 2026-11-09 (`r1.s4.w1`): the device verbs, and the claim's scope

- **Devices answer.** A device now reads `requests` and `show`, and it calls `confirm_code`,
  `answer`, `clear` and `ack` with a confirm code (ADR-0005, amended; ADR-0012, amended). It takes
  the claim as it acts: `confirm_code` moves nothing, and the spend fences the holder its code saw.
  There is still no `claim` verb, no `halt` and no `arm` for a device on the API (`arm` comes with
  `r1.s4.w2`). The device still acts as the operator and has no scope of its own.
- **One current claim for the whole store.** The Decision's "one active claim per project" is
  reconciled with what the code does: one current claim for the whole store, across every run and
  project. The `claims` table has no run or project column, and `claims_one_current` (migration 0002)
  allows at most one row with no `fenced_at`. A claim taken for any run fences the holder of every
  other. This amendment states it as the rule. The store does not change.

## Amendment, 2026-11-10 (`r1.s4.w2`): a device arms from a start card

- **A device arms a run from a start card.** The Orchestrator's `propose` posts a start card into its
  chat. The device asks `confirm_code` for an `arm` code, which binds the proposal and the hash of the
  plan, and sends `arm` with the code. `arm` joins the device allowlist for this and nothing else. The
  device still acts as the operator and has no scope of its own.
- **There is still no `claim` and no `halt` on the API.** `halt` withdraws every open proposal, so a
  stale card cannot start a run after the kill switch.
- **Known limit, unchanged:** a code proves the device, the freshness and the plan, not that a person
  pressed the button.
