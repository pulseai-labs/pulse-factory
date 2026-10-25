# ADR-0002 — A generic engine core behind ports

- **Status:** Proposed
- **Date:** 2026-10-25

## Context
Pulse Factory is a registry and driver of workflows. The engine is generic. A workflow is data that
names its stages, roles, models, briefs, done signals and merge rules. The ossify spine is the first
workflow, not the engine's model of work. Pulse Factory must call ossify, never re-implement an ossify
contract. Each coding agent gets its own adapter.

## Decision
- `crates/pf-engine` holds units, stages, sessions, signals, merge by rule, requests, the claim, the
  ledger, capacity and reconciliation.
- It defines its ports in `src/port/`: store, agent adapter, forge and unit source.
- It depends on no other `pf-` crate. It knows nothing about spines, rounds, work items, or any one agent.
- Store, adapter, forge, workflow-format and ossify crates implement ports. They depend on
  `pf-engine`, never the reverse.
- `crates/pf-ossify` is the only crate that calls `oss` or reads ossify's files.
- `crates/pulse-factory`, the binary, is the composition root.

Rejected alternatives:
- **One crate.** ossify knowledge would leak into the engine, and the second workflow would need a
  rewrite.
- **Dynamically loaded plugins.** No measured need.

## Consequences
- A second workflow or a second agent is a new crate.
- Engine tests use fake ports.
- The ports must be right before the second implementation of each exists. A port that leaks shows up
  as a change to the engine's own manifest or ports, which this bone's touch surface catches.
