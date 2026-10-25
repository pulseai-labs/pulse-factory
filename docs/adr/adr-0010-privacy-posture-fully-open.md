# ADR-0010 — Privacy posture: fully-open via none

- **Status:** Proposed
- **Date:** 2026-10-25

## Context
- The operator decided on 2026-10-25 that Pulse Factory is fully open, with no revenue intent.
- The product repo `pulseai-labs/pulse-factory` is public. Its AI workspace
  `pulseai-labs/pulse-factory-internal` is private, as the organisation's `<name>` / `<name>-internal`
  convention sets.

## Decision
- **Posture:** `fully-open`.
- **Moat:** the moat inventory is empty, and the channel is `none`.
- **Routing:** specs, planning documents, the memory bank and process records stay in the private AI
  workspace. That is document routing, not a moat.
- **`PUBLIC_BOUNDARY.md`** at this repo's root carries the secrets rules and the planning-document
  rules.
- **Composition root:** this repo's root, where the cargo workspace sits.

## Consequences
- Every file in this repo is public from its first push. Secrets and AI-workspace material never
  enter it.
- A revenue intent would reopen this decision through a posture-supersede.
