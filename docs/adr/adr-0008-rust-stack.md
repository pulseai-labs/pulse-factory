# ADR-0008 — The stack: Rust with Tokio, rusqlite, portable-pty, clap, serde and yaml-rust2

- **Status:** Accepted, 2026-11-05, at the Release 0 close
- **Date:** 2026-10-25
- **Amended:** 2026-11-07, at `r1.s2.w2`: the network API's crates, and two claims move from
  "Unverified" to "Verified". Nothing above changes except where the amendment says.

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
None of the claims that the skeleton spine's first build was to verify is open now (see the amendment
of 2026-11-07).

portable-pty has had no release since 2025-02. Revisit this ADR if it fails its first-use check.

## Amendment, 2026-11-07 (`r1.s2.w2`)

The network API (ADR-0012) adds crates, and the claims that waited for a first build are verified.

**New crates.** Each is pinned with `=` in the workspace `Cargo.toml`, with default features off. The
versions were checked at the first use, on 2026-11-07. `crates/pf-api` is the one new workspace
member. `pulse-factory` depends on it.

| Crate or tool | Version | Used for |
|---|---|---|
| `axum` | 0.8.9 | the router (`http1`, `json`, `tokio`) |
| `hyper` | 1.12.0 | the HTTP/1.1 server under axum |
| `hyper-util` | 0.1.21 | Tokio glue for hyper |
| `http-body-util` | 0.1.5 | the 1 MiB body limit |
| `bytes` | 1.12.1 | request bodies |
| `rustls` | 0.23.46 | TLS 1.3, with the `ring` provider only |
| `tokio-rustls` | 0.26.6 | the TLS acceptor on Tokio |
| `rcgen` | 0.14.10 | the self-signed certificate and the ECDSA P-256 key |
| `ts-rs` | 12.0.1 | the TypeScript types for the clients |
| `tower` | 0.5.3 | dev only: drives the router in tests without TLS |

- **First-use check.** The workspace built and its tests passed with these versions. `cargo tree`
  and `Cargo.lock` hold no `aws-lc-rs`, so no cmake build is needed: `rustls` and `rcgen` use the
  `ring` provider. A pinned `curl` (8.18.0, with OpenSSL) completed the pairing and the pin check
  against the controller's `rustls` listener (`crates/pulse-factory/tests/api_pairing.rs`).
- **Axum is in.** The "Not in Release 0" list kept Axum and Caddy for the run view. Axum comes in
  here (A1 of ADR-0012). Caddy does not: the controller terminates TLS itself (ADR-0012). The other
  items of that list (React, TypeScript and Vite, jsonschema, libghostty-vt) stay out of the
  cargo workspace.
- **Reversibility.** `axum` and `ts-rs` are easy to replace. The `rustls` choice sits behind the
  listener in `crates/pf-api` and is checked by a pinned client in a test.

**Verified claims, moved from "Unverified claims".** These were open since 2026-10-25 and are
verified now:
- rusqlite runs WAL with `synchronous=FULL` behind one writer thread. The store opens with
  `journal_mode=wal` and `synchronous=FULL` and reads them back through `PRAGMA`
  (`crates/pf-store-sqlite/src/lib.rs`, tested in `crates/pf-store-sqlite/tests/store_settings.rs`).
- yaml-rust2 rejects duplicate keys, and its markers give `pf-workflow` a position for each
  unknown-field and duplicate-key error (`crates/pf-workflow/tests/parse_strict.rs`: "a duplicate
  key is refused at the second key", and the unknown-field tests).
