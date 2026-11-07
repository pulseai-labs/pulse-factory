# ADR-0012 — A network API in the controller: in-process TLS pinned at pairing, a one-use code login, and revocable device tokens

- **Status:** Accepted, 2026-11-07, at `r1.s2.w2`
- **Date:** 2026-11-07

## Context
- Release 1's first exit criterion: the operator installs the MacBook app, pairs it with draco-desk
  over TLS with a login, adds a project and sees it in the app. An unpaired client is refused, and
  after the operator revokes the MacBook from the CLI, its next call is refused too.
- ADR-0001 left a network surface for later ("the HTTPS edge can come later as a second listener").
  ADR-0005 used local rails only and left login and TLS for the run view. This ADR is that decision.
- The host is one Linux machine on the operator's tailnet. The operator is the only user (ADR-0009).
  The tailnet is the fence. Reachability is not authentication (design call D-156).
- The grill of spine `r1.s2` (2026-11-06 and 2026-11-07) fixed Q1 to Q3 and A1 to A8. The planning
  audit challenged them, and the operator accepted its dispositions.

## Decision

**Q1, the login is a host-local one-use code (D-193).**
- `pulse-factory device pair` runs on the host, over the socket, and prints one code. It works once,
  for 10 minutes. A new code replaces the live one, and a restart voids it.
- The code holds an 80-bit secret, the SPKI hash of the server key and the listen address, as the one
  copyable string `pf1-…` (`docs/contracts/api.md`).
- The app sends the code and a device name to `POST /v1/pair`. The server uses the code up and issues
  a device token: 32 random bytes, shown as 64 hex digits. The server keeps only the SHA-256, as for
  the claim token (A5).
- There is no passphrase. The operator's access to the host (ssh) is what proves the operator.
- The rate limit on failed codes and failed token calls belongs to `r1.s2.w5`, not to this item.

**Q2, TLS is in-process rustls in the controller.**
- On the first start with the API on, the controller makes an ECDSA P-256 key and a self-signed
  certificate (`rcgen`) in `<state dir>/api/`. `key.pem` is mode 0600.
- The client pins the SHA-256 of the key's SubjectPublicKeyInfo, which the code carries. It checks
  the pin before it sends the code. It ignores the certificate chain.
- A changed key fails closed. Only a new pairing accepts a new key. Key loss is therefore "revoke
  every device and pair again".
- TLS 1.3 only, with the `ring` provider (no `aws-lc-rs`, so no cmake build).
- Caddy is dropped. D-186 named it "subject to private certificate qualification", and an
  in-process listener needs no second process.

**Q3, the bind.**
- The API listens only on the address in `--api-listen` or `PF_API_LISTEN`. It is off until the
  operator sets one: no key is made and nothing listens. The unit reads
  `~/.config/pulse-factory/controller.env` as an optional environment file.
- A bind that fails with "address not available" retries every 5 seconds, so the controller can
  start before Tailscale does. Any other bind error is shown and not retried. In both cases the
  controller keeps serving the socket. `api status` shows `off`, `waiting`, `listening` or `failed`.

**The self-answered decisions (A1 to A8).**
- A1: the HTTP framework is Axum (hyper 1 underneath).
- A2: the transport is HTTPS request and response with JSON. WebSocket comes with `r1.s3`.
- A3: the API has one versioned route per CLI verb, under `/v1/verbs/<verb>`, through the
  controller's verb handler, with an allowlist of the verbs open to devices.
- A4: the API runs inside the controller daemon (ADR-0001), as a listener beside the socket.
- A5: the device token is kept by the client (the macOS Keychain, for the app). The server keeps the
  SHA-256. No log, ledger line or error holds a token, a code or a key.
- A6: every route except pairing needs a live device token. A revoke refuses the next call at once,
  because the API reads the store on each call.
- A7: a state-changing call carries a command ID. The same ID and content returns the same record.
  A property test covers the device commands (`device_admission_property.rs`).
- A8: the client's Rust side makes the pinned TLS call. A webview opens no TLS connection itself.

**The allowlist rule.**
- A device may call exactly `project_add`, `project_list` and `project_remove` in this spine.
- There is no `ack`, `answer`, `claim`, `clear`, `arm` or `halt` on the list, and no device verb
  (`device_pair`, `device_list`, `device_revoke`, `api_status`). A device cannot let another device
  in, and it cannot stop the run state.
- **Each later spine that adds a verb to the allowlist amends this ADR** (audit C2). The router test
  names every verb that must stay off the list, so a change to the list fails a test.

**The device model.**
- A device is a row in the store: an ID (`D-<n>`, never reused), a name, the token's SHA-256, the
  pairing time, last-seen time, last peer address and revoke time (migration 0004).
- Pairing and revoking are admissions under `@controller`, each with one controller-ledger line that
  names the device and never the code or the token (ADR-0003, amended).
- A device acts as the operator and has no scope of its own (ADR-0009, amended). Per-device scopes
  and the human confirm of a merge-ack come with `r1.s4`.

Rejected alternatives:
- **Caddy as the TLS edge.** It adds a second process and a certificate qualification (D-186), and
  the pin must be the key of the process that the client talks to. An in-process listener has one
  key and no second process to start, restart or lose.
- **A passphrase login.** It adds a secret that the operator must keep and that a guesser can try.
  The host-local code needs no stored secret, and the operator's ssh access is already the proof.
- **Tailscale identity in place of pairing (audit C12).** The gate requires a pinned key and a device
  list that the CLI can revoke. Tailscale identity is per node, so draco-hub and the Mac mini would
  each be the operator. Reachability is not authentication (D-156). The tailnet stays the fence
  (Q3), not the login.

## Consequences
- A device can be revoked from the CLI, and the next call is refused. A paired device is as
  powerful as the operator within the allowlist, so a stolen unlocked device is the operator's
  problem until the operator revokes it.
- `last_seen` is the one store write outside admission (ADR-0003, amended): a read must not cost an
  admission row, and the idle expiry of `r1.s2.w5` reads it.
- The server key is a single point of identity. Losing it costs a re-pair of every device.
- The API's types are the clients' types: `ts-rs` renders them, and a test fails when the committed
  files drift (`apps/api-client/src/gen/`).
- Out of this item, and done in `r1.s2.w5`: the rate limit, one ledger line per API request, a bind
  guard for addresses outside loopback and the tailnet, idle expiry (30 days) and the "server key
  changed" message.

### Verified claims
- A pinned `curl` completes `POST /v1/pair` and the verb calls on `127.0.0.1`, refuses a changed key
  before it sends anything, and refuses a revoked device on the next call
  (`crates/pulse-factory/tests/api_pairing.rs`, 2026-11-07).
- The router answers 401 without reaching the host for a missing, malformed or unknown token, 403 for
  every verb off the allowlist (including `halt` and `ack`), 413 over 1 MiB and 404 for `/v2`
  (`crates/pf-api/tests/router.rs`, 2026-11-07).
- The device commands keep the admission rules: a replay returns the first record, changed content
  is a conflict, no ID is issued twice, and one ledger line is written per new command
  (`crates/pf-store-sqlite/tests/device_admission_property.rs`, 2026-11-07).
- The committed TypeScript types equal what `ts-rs` renders now
  (`crates/pf-api/tests/ts_types.rs`, 2026-11-07).
- The dependency set has no `aws-lc-rs` (`cargo tree`, 2026-11-07). Versions are in ADR-0008.

### Unverified claims
- The bind retry when Tailscale starts after the controller. The test uses a TEST-NET address that
  is never on the host; the walk on draco-desk with a real tailnet address (demo line d13) verifies
  the retry.
- The Tauri 2 macOS client keeps and uses the pin. `r1.s2.w4` verifies it with the `pf-client`
  bridge and its Rust tests.

Revisit this ADR when a second operator, a device outside the operator's tailnet, or a browser
client served by the server is planned.
