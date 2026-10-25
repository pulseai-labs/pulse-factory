# ADR-0009 — One operator on one host

- **Status:** Proposed
- **Date:** 2026-10-25

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
