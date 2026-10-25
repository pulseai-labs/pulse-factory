# ADR-0004 — A versioned CLI with JSON output, and a strict versioned workflow file

- **Status:** Proposed
- **Date:** 2026-10-25

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
- **Breaking-change policy** (the project is pre-1.0):
  - A breaking change to the CLI's JSON or to the workflow format bumps that schema version and adds
    a changelog line.
  - An old version is refused with a message that names the supported one.
- The skill in `skill/` is part of the contract.

Rejected alternatives:
- **The full recipe format.** Its field inventory is a proposed structure, not a finished executable
  schema.
- **A workflow built into the Rust code.** It breaks "a workflow is data".

## Consequences
- Every workflow file declares its version.
- Agents learn the CLI from the skill, so the skill changes with the verbs.
