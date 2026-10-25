# PUBLIC_BOUNDARY.md

This repository is public. Its posture is `fully-open`: all product code is published, and nothing in
it is held back as a moat. Planning material lives elsewhere, in a private AI workspace, by routing
rule.

## Machine-checkable rules
<!-- Executed by the release-close boundary audit
     (close/references/boundary-audit.md) at every release close. That file
     is authoritative for which repos run these rules, which trees they are
     matched against, and what each arm does with a finding — do not restate
     any of it here. Authored at onboarding so the file exists from day
     one. -->
never-tracked: **/.env, **/.env.*, **/*.pem, **/*.key, **/id_rsa*, **/id_ed25519*
never-tracked: **/secrets/**, **/credentials.json, **/*.sqlite-wal, **/*.sqlite-shm
never-tracked: **/MASTER-SPEC.md, **/memory-bank/**, docs/superpowers/**, **/handoffs/**
never-tracked: **/.ossify/**, **/.workspace/**
fixtures-must-be: synthetic

## Working-tree hygiene allowlist
<!-- Classes of untracked sensitive files known to exist in local clones,
     named by PATTERN only — never by content description. -->
- `.env*` (untracked)
- `target/` (untracked build output)
- `*.sqlite`, `*.sqlite-wal`, `*.sqlite-shm` (untracked local run stores)

## Never here (prose rules)
- No secrets, tokens, API keys or credentials of any kind.
- No downstream strategy, roadmap or competitive material.
- No non-synthetic fixtures: no real user data, no real session transcripts.
- No AI-workspace material: specs in progress, planning documents, memory bank, handoffs, agent
  transcripts or run ledgers.
- No local run stores or their write-ahead logs.
