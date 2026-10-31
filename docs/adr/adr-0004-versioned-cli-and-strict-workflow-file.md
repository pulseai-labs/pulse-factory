# ADR-0004 — A versioned CLI with JSON output, and a strict versioned workflow file

- **Status:** Proposed
- **Date:** 2026-10-25
- **Amended:** 2026-10-26, at the planning of spine r0.s1: declared plugins, unit sources and seed
  units.

## Context
- Every session acts on a run through the Pulse Factory CLI and its skill.
- The CLI has no wait or watch verb. A session that could wait would sit and poll, and polling was the
  r3.s2 pilot's cost failure.
- The operator decided on 2026-10-25 that the first workflow format is a strict small subset that
  grows toward the full recipe format.

## Decision
- **CLI verbs:** `arm`, `status`, `requests`, `show`, `claim`, `answer` and `halt`, plus the internal
  `controller` and `hook`.
- Every verb takes `--json`, and its JSON carries a `schema_version`.
- `status` returns a snapshot. No verb waits or watches.
- **The workflow file:** YAML with a top-level `version:`.
  - The parser rejects unknown fields and unknown versions. There is no silent conversion.
  - It holds only what the ossify spine workflow needs.
  - It declares the plugins its seats need, each with a minimum version (for example the ossify
    plugin). `arm` refuses a run when a seat cannot load a declared plugin at or above its minimum
    version, and pins the versions it resolved with the run (ADR-0007).
  - It names its unit sources (the queue directory; the record format is ADR-0002's) and the seed
    units that start a run, such as the seat that fills the queue.
- **Breaking-change policy** (the project is pre-1.0):
  - A breaking change to the CLI's JSON, the workflow format or the unit record bumps that schema
    version and adds a changelog line.
  - An old version is refused with a message that names the supported one.
- The skill in `skill/` is part of the contract.

Rejected alternatives:
- **The full recipe format.** Its field inventory is a proposed structure, not a finished executable
  schema.
- **A workflow built into the Rust code.** It breaks "a workflow is data".

## Consequences
- Every workflow file declares its version.
- Agents learn the CLI from the skill, so the skill changes with the verbs.
