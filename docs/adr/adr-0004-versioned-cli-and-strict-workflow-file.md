# ADR-0004 — A versioned CLI with JSON output, and a strict versioned workflow file

- **Status:** Accepted, 2026-11-05, at the Release 0 close
- **Date:** 2026-10-25
- **Amended:** 2026-10-26, at the planning of spine r0.s1, with text from its review round 1 on
  2026-10-31: declared plugins, unit sources and seed
  units.
- **Amended:** 2026-11-01, at `r0.s1.w2`: `roots` and the role rule files. Nothing above changes.
- **Amended:** 2026-11-07, at `r1.s2.w2`: the network API mirrors the CLI's verbs under `/v1`, and the
  CLI gains the verbs `device` and `api`. Nothing above changes.

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
  - It declares `roots`: the directories that a working directory from a unit record may name. A
    launch parks the unit when such a directory is relative, does not exist, or lies outside every
    root after the path is made canonical.
  - It names one rule file per role. A rule file is `{version: 1, role, allow: [...]}`, read as
    strictly as the workflow file, and `allow` takes a closed set of values. A role gets exactly what
    it lists. The adapter adds the deny list.
  - Its field set, its placeholders and the launch settings are in `docs/contracts/workflow-file.md`,
    `docs/contracts/launch-settings.md` and `docs/contracts/role-rules.md`.
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

## Amendment, 2026-11-07 (`r1.s2.w2`)

The API and the CLI share one set of verbs (ADR-0012).

- **The API mirrors the CLI under `/v1`.** Each verb that a device may call is a controller verb
  with JSON arguments and a JSON answer, and `POST /v1/verbs/<verb>` returns the same JSON that the
  CLI prints with `--json`. The CLI is a client of the controller's verb handler, and so is the
  API, so a verb has one implementation. An allowlist sets which verbs a device may call
  (`docs/contracts/api.md`). In this spine it holds `project_add`, `project_list` and
  `project_remove`.
- **The wire types are shared.** The project and device shapes live in `crates/pf-api` and derive
  `ts_rs::TS`. The CLI, the controller and the generated TypeScript types use one definition. A
  change that removes or renames a field raises `schema_version`, as the breaking-change policy
  says, and a test fails when the generated types drift.
- **New verbs.** `device` has the forms `pair`, `list` and `revoke`. `api` has the form `status`.
  Each takes `--json` and carries `schema_version`. `controller` gains `--api-listen <addr>`
  (default: the variable `PF_API_LISTEN`). The verbs and flags are in `docs/contracts/cli.md`, and
  the skill names them.
- **The decision list names the verbs again:** `arm`, `status`, `halt`, `requests`, `show`, `claim`,
  `answer`, `clear`, `ack`, `project`, `device` and `api`, plus the internal `controller` and
  `hook`. No verb waits or watches.
