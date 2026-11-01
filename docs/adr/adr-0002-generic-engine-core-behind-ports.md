# ADR-0002 — A generic engine core behind ports

- **Status:** Proposed
- **Date:** 2026-10-25
- **Amended:** 2026-10-26, at the planning of spine r0.s1: no planning-method code, a queue unit
  source, and the vocabulary below.
- **Amended:** 2026-11-01, at `r0.s1.w2`: the vocabulary gains *track* and a seed's *repeat*. Nothing
  above changes.

## Context
Pulse Factory is a registry and driver of workflows. The engine is generic. A workflow is data that
names its stages, roles, models, briefs, done signals and merge rules. The ossify spine is the first
workflow, not the engine's model of work. Pulse Factory must never re-implement an ossify contract:
seats use ossify through the plugin the workflow declares. Each coding agent gets its own adapter.

**Vocabulary** (operator, 2026-10-26):

| Term | Meaning |
|---|---|
| Agent runtime | The program a session runs in: Claude Code, Codex or OMP. Each has one adapter. |
| Plugin | An installable bundle for agent runtimes: skills, hooks, commands and tools. Seats use plugins; Pulse Factory never runs them itself. ossify, crew and merge-bar are plugins. |
| Skill | An instruction folder that a session loads. It is usually part of a plugin. |
| App | An MCP connector and the skills that drive it. |
| Agent | A persona: a name, instructions, a brain (runtime, model, effort) and the plugins it carries. The operator chats with it. |
| Workflow | Pulse Factory's own recipe: stages, roles, models, briefs, done signals, unit sources, seed units, the merge rule and capacity settings. It is data, not a plugin. It declares the plugins its seats need. |
| Unit source | Where a run's units come from. Each unit source names the track its units join. |
| Track | A named list of stages with an optional merge definition (rule, repo, base, head check). A unit moves through the stages of the track it came in on, and it merges only when its track has a merge definition. A workflow with one track behaves as a plain stage list. |
| Seed unit | A unit that the workflow file declares and the engine admits at the start of a run, with no queue record. It names its track. |
| Repeat | An optional rule of a seed unit. It admits the seed again under a new ID (`<id>.2`, `<id>.3`) when the previous seed is `done`, a unit on a named track came in after it, every unit on that track is `done`, a named file does not exist, and fewer than a stated maximum of seeds have run. It lets a workflow re-seed a group-by-group step with no engine code that names the step. |
| Run | One armed instance of a workflow. |

## Decision
- `crates/pf-engine` holds units, stages, sessions, signals, merge by rule, requests, the claim, the
  ledger, capacity and reconciliation.
- It defines its ports in `src/port/`: store, agent adapter, forge and unit source.
- It depends on no other `pf-` crate. It knows nothing about spines, rounds, work items, or any one agent.
- Store, adapter, forge and workflow-format crates implement ports. They depend on `pf-engine`, never
  the reverse.
- **No crate knows a planning method.** Pulse Factory has no ossify code. A workflow's seats use a
  method through the plugins the workflow declares (ADR-0004), and the workflow's briefs name the
  method's commands. A workflow file may also name a command that the engine runs as data, such as
  an `oss` status write.
- **The unit source reads a queue that a seat fills.** A seat writes one versioned JSON record per
  unit into the run's queue directory: its ID, its group, its order and its variables. That record
  format is the port's contract, so any method's seat can produce it.
- `crates/pulse-factory`, the binary, is the composition root.

Rejected alternatives:
- **One crate.** Planning-method knowledge would leak into the engine, and the second workflow would need a
  rewrite.
- **Dynamically loaded plugins.** No measured need.
- **One crate per planning method (`pf-ossify`).** A workflow for another method, such as
  Superpowers, would need a new crate and a rebuild. The baton pilot runs the ossify spine through a
  generic queue, with no ossify code in its engine and the `oss` commands as recipe data, so the crate
  buys nothing.

## Consequences
- A second workflow is a new workflow file and its briefs, with no rebuild, as long as the current
  workflow format and rule sets can express it. A change to either is a product change (ADR-0004). A
  second agent runtime is a new adapter crate.
- The real method contract (for ossify, `oss` and its skills) is exercised by real seats, not by
  Pulse Factory's own tests.
- Engine tests use fake ports.
- The ports must be right before the second implementation of each exists. A port that leaks shows up
  as a change to the engine's own manifest or ports, which this bone's touch surface catches.
