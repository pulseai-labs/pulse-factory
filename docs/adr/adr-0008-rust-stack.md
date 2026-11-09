# ADR-0008 — The stack: Rust with Tokio, rusqlite, portable-pty, clap, serde and yaml-rust2

- **Status:** Accepted, 2026-11-05, at the Release 0 close
- **Date:** 2026-10-25
- **Amended:** 2026-11-07, at `r1.s2.w2`: the network API's crates, and two claims move from
  "Unverified" to "Verified". Nothing above changes except where the amendment says.
- **Amended:** 2026-11-08, at `r1.s2.w4`: the pinned client's crates and features, and the Mac app's
  separate cargo workspace. Nothing above changes except where the amendment says.
- **Amended:** 2026-11-08, at `r1.s3.w1`: the new member `crates/pf-adapter-claude` and its
  `pf-claude-replay` binary, and the line references of two verified claims. Nothing above changes
  except where the amendment says.
- **Amended:** 2026-11-09, at `r1.s3.w3`: `tokio-tungstenite`, axum's `ws` feature, the new dependencies of
  `pulse-factory` and the `pf-stream-reader` binary.

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
  (`crates/pf-store-sqlite/src/lib.rs:216-225`, tested in `crates/pf-store-sqlite/tests/store_settings.rs`).
- yaml-rust2 rejects duplicate keys, and its markers give `pf-workflow` a position for each
  unknown-field and duplicate-key error (`crates/pf-workflow/tests/parse_strict.rs:180`: "a duplicate
  key is refused at the second key", and the unknown-field tests).

## Amendment, 2026-11-08 (`r1.s2.w4`)

The pinned client (`crates/pf-client`, ADR-0012) is a workspace member. It adds one crate and three
feature choices to the workspace, and the Mac app brings Tauri in a cargo workspace of its own.

**New crate.** `security-framework` **3.7.0** is pinned with `=` in the workspace `Cargo.toml`.
`pf-client` takes it under `[target.'cfg(target_os = "macos")'.dependencies]` only, for the Keychain
(`KeychainStore`). The Linux build and its tests never compile it. `Cargo.lock` gains it and three
crates under it: `security-framework-sys` 2.17.0, `core-foundation` 0.10.1 and `core-foundation-sys`
0.8.7. It was created 2026-02-20, is the newest release and is not yanked (crates.io, 2026-11-08).

**New features, on crates already in the workspace.**
- `hyper`: `client`, in `pf-client` only. It adds `try-lock` 0.2.5 and `want` 0.3.2 to `Cargo.lock`.
  The client uses `hyper::client::conn::http1` over the pinned TLS stream, so `hyper-util`'s client
  features stay off.
- `tokio`: `rt`, `net`, `time`, `sync`, `io-util` and `macros` in `pf-client`. The crate uses no
  multi-thread runtime: the bridge binary runs on a current-thread runtime.
- `rustls`: `tls12`, in `pf-client`'s `[dev-dependencies]` only. A test server offers TLS 1.2 so a test
  can show that the client refuses it. Cargo unifies features in a test build, so `rustls` also has
  `tls12` there. The client and the listener each name TLS 1.3 only in code, so no production path
  offers 1.2.
- `axum` serves the bridge with the features the workspace already has (`http1`, `json`, `tokio`).
  `http-body-util`, `bytes`, `tokio-rustls` and `rcgen` (dev only) are reused as pinned.

**The Mac app.** `apps/desktop/src-tauri` (`pulse-factory-desktop`) is its own cargo workspace with a
committed `Cargo.lock`, and the root `Cargo.toml` excludes `apps/desktop`. No build on draco-desk
compiles it. It pins `tauri` **2.11.5** and `tauri-build` **2.6.3** with `=` (ADR-0013 holds the check),
and depends on `pf-client` and `pf-api` by path, so it uses the versions that this workspace pins for
every shared crate. Its lock file was resolved from the local registry cache, which holds some
dependencies of Tauri at older versions than the newest.

**First-use check.** The workspace builds and its tests pass with these versions on draco-desk
(`cargo test --workspace --locked`, AC-13 of `r1.s2.w4`). The Keychain code and the Tauri app build only
on GitHub's `macos-latest` runner, in the `macos-app` job of the item PR. **Unverified until that run is
green:** `KeychainStore` compiles against `security-framework` 3.7.0 and its tests pass on the runner.

**Reversibility.** `security-framework` sits behind the `DeviceStore` trait; a file store replaces it.
Tauri sits behind the same six-operation `Transport`, and the UI does not change if it is replaced.

## Amendment, 2026-11-08 (`r1.s3.w1`)

The chat adapter is a workspace member: `crates/pf-adapter-claude` (ADR-0014). It holds the library
`pf_adapter_claude` and one binary, `pf-claude-replay`, which stands in for the `claude` CLI and
plays recorded fixtures back in tests and demos.

**No new third-party crate.** The crate depends on `pf-engine`, `serde`, `serde_json` and `tokio`
with the `sync` feature, all already in the workspace. `tempfile` is a dev dependency, as elsewhere.
It does not take `sha2`: the crate has its own streaming SHA-256 (`src/sha256.rs`), checked against
the FIPS 180-4 vectors and against `pf_engine::claim::sha256_hex`, so the managed CLI binary
(about 340 MB) is hashed in chunks and never read whole into memory. `crates/pf-engine/src/claim/**`
is the `claim-authority` gate's surface and is not changed.

**`tokio` `sync`** is now also a feature of `pf-adapter-fake` and `pf-adapter-claude`, for the event
channel of the chat port (an unbounded `mpsc`, as in `pf-engine`).

**`Cargo.lock`** gains the one member and the `tokio` edge of `pf-adapter-fake`, and no other crate.

## Amendment, 2026-11-09 (`r1.s3.w3`)

The chat stream and the chat controller add one crate, one feature and three dependencies.

- **`tokio-tungstenite`**, pinned with `=0.29.0`, with `default-features = false` and the `handshake`
  feature, is a workspace dependency. axum's `ws` feature needs `tokio-tungstenite` 0.29 and
  `tungstenite` 0.29, so `pf-api` serves the WebSocket upgrade with it. The first build fetches the two
  crates from crates.io once (draco-desk's offline cache held 0.30 only), and `Cargo.lock` then holds
  them. The sync `tungstenite` client that `tokio-tungstenite` re-exports serves the readers and the
  tests, so the item adds no `futures-util`.
- **axum's `ws` feature** is on, in the `pf-api` dependency.
- **`pf-adapter-claude` and `pf-client`** are normal dependencies of `pulse-factory`: the controller
  launches the chat through the adapter, and `pf-stream-reader` builds its TLS config with
  `pf_client::pin::client_config`. The `pulse-factory` binary links the adapter. `rustls` is a direct
  dependency of the `pulse-factory` package for the reader, and `proptest` a dev-dependency for the chat
  property test (its failure persistence stays off).
- **`pf-stream-reader`** is a second binary of the `pulse-factory` package: a small client of the chat
  stream for the demo (`scripts/demo/chat-api.sh`) and for tests. It prints each frame as one line of
  JSON, reads the token from a file, and is no product surface; `pf-client`'s own stream may replace
  it. `scripts/install.sh` also installs `pf-claude-replay` for the same demo.

