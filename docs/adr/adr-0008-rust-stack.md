# ADR-0008 — The stack: Rust with Tokio, rusqlite, portable-pty, clap, serde and yaml-rust2

- **Status:** Proposed
- **Date:** 2026-10-25

## Context
- The architecture spec chose Rust, Tokio, SQLite through rusqlite, portable-pty as the first PTY
  adapter, clap, serde_json and yaml-rust2.
- draco-desk has an NVMe write budget. `~/.cargo/config.toml` limits debug info, and builds share one
  target directory.

## Decision
- Rust stable, in one cargo workspace, with these crates:
  - Tokio;
  - rusqlite;
  - portable-pty;
  - clap;
  - serde and serde_json;
  - yaml-rust2 for parsing the strict workflow file. `crates/pf-workflow` validates the parsed tree
    against its own typed schema. It rejects unknown keys and unknown versions, and it reports each
    error at the parser's marker position.
- Builds follow the host's build rules: no `CARGO_PROFILE_*` overrides, one shared target directory,
  and one cold build at a time.
- Not in Release 0. These come with the run view or terminal attach:
  - Axum and Caddy;
  - React, TypeScript and Vite;
  - jsonschema;
  - libghostty-vt.

Reversibility:
- yaml-rust2 and clap are easy to replace.
- portable-pty sits behind the agent adapter port.
- rusqlite sits behind the store port (ADR-0003).
- Tokio is the hardest to replace.

## Consequences
- One language across the controller, CLI and hook.
- The Tokio runtime shapes every async boundary. Blocking database, file and validation work stays off
  the async control path.

### Verified claims
These were read from crates.io and the host on 2026-10-25, with no build:

| Crate or tool | Version |
|---|---|
| `tokio` | 1.54.1 |
| `rusqlite` | 0.41.0 |
| `portable-pty` | 0.9.0 (last release 2025-02-11) |
| `clap` | 4.6.7 |
| `serde` | 1.0.229 |
| `serde_json` | 1.0.154 |
| `yaml-rust2` | 0.13.0 |
| `rustc` and `cargo` on draco-desk | 1.98.1 |

Plus, verified on 2026-11-01 by `r0.s1.w3`: `portable-pty` can spawn, read, write and resize a PTY from
Tokio. `crates/pf-adapter-omp/tests/pty_driver.rs` proves it.

### Unverified claims
The skeleton spine's first build verifies each of these:
- rusqlite runs WAL with `synchronous=FULL` behind one writer thread.
- yaml-rust2 rejects duplicate keys, and its markers give `pf-workflow` a position for each
  unknown-field error.

portable-pty has had no release since 2025-02. Revisit this ADR if it fails its first-use check.
