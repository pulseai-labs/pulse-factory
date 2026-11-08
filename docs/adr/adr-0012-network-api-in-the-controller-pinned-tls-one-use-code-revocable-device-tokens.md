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
- The rate limit on failed codes and failed token calls is `r1.s2.w5`'s (see "Guards (r1.s2.w5)").

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
- Out of this item, and done in `r1.s2.w5`: the rate limit, one request-ledger line per API request, a
  bind guard for addresses outside loopback and the tailnet, idle expiry (30 days) and the "server key
  changed" message. The decisions and their limits are in "Guards (r1.s2.w5)" below.

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

- A peer with 5 bad codes, or 5 bad tokens, is blocked on every route for 10 minutes while a second
  address still pairs, the window slides, the map holds at most 4096 peers, and a blocked peer's
  429s are counted in one record a minute (`crates/pf-api/tests/guard.rs`,
  `crates/pf-api/tests/audit.rs`, 2026-11-07).
- `listen_allowed` accepts the first and last address of each range and refuses the addresses next to
  them (`crates/pf-api/tests/listen_range.rs`, 2026-11-07).
- On a real controller, an address outside loopback and the tailnet leaves the API `failed` with no key
  and the socket still served, `api.ledger` has one line for each request with no secret in it, and a
  device that is idle for the configured time is refused and still listed
  (`crates/pulse-factory/tests/api_guards.rs`, 2026-11-07).
- `scripts/demo/pair-api.sh` (d11) passes against the built binary on `127.0.0.1`, including the
  restart on a new key (`crates/pulse-factory/tests/demo_scripts.rs`, 2026-11-07).

### Unverified claims
- The bind retry when Tailscale starts after the controller. The test uses a tailnet address
  (`100.100.1.1`) that is never on the host; the walk on draco-desk with a real tailnet address (demo
  line d13) verifies the retry.
- The Mac app itself (the Tauri shell and the Keychain store) keeps and uses the pin. `pf-client` is
  verified on draco-desk (see "The pinned client (r1.s2.w4)"). The shell's glue and the Keychain run on
  GitHub's macOS runner (the `macos-app` job) and in the d13 walk after the spine merges.

## Guards (r1.s2.w5)

*Added 2026-11-07, at `r1.s2.w5`.* This section adds the five guards that "Out of this item" left to
this item. It adds no verb, no route and no wire type, and it changes no decision above. The
planning audit items C2, C4, C7, C11 and C13 and the guards rulings of `SPINE.md` are its source.

**The rate limit (audit C4).**
- Two counters per peer address, each over a sliding 10 minutes: failed codes (a `/v1/pair` answer of
  400 or 401) and failed token calls (a 401 on `/v1/status` or `/v1/verbs/<verb>`).
- The 5th failure of either kind inside the window blocks that peer for 10 minutes on every route. A
  success does not clear a count. The block answers 429 with `Retry-After` before the token, the body
  or the path is read, and it reaches no host method but `audit`.
- Each peer is counted alone, so a second address with a valid code still pairs while the first is
  blocked. The state is in memory: a restart clears every count and block (and voids the live code).
  The map holds at most 4096 peers and drops the oldest entry when full.
- Limit: an address behind which several clients sit shares one count. A per-device limit, a ban list
  that survives a restart and a limit on successful calls are out of scope.

**One request-ledger line per API request (audit C7).**
- Ruling at the `r1.s2.w5` barrier: the request ledger is its own file, `<state dir>/api.ledger`.
  `controller.ledger` holds one line per store admission, keyed by its sequence number, and recovery
  rewrites missing outbox lines. A per-request line there would need a store row per request, and
  the amendment of ADR-0003 rules that a read must not cost one (`touch_device`). The admission lines
  of w1 and w2 stay in `controller.ledger`, so a state-changing call has both lines.
- The router calls `ApiHost::audit` once for every request it answers. The controller appends one line
  under one writer: `<UTC time> api route=<route> verb=<verb|-> device=<D-n|-> peer=<ip>
  status=<code> outcome=<outcome>`. Replays and auth refusals are included. A verb name is written
  only when it is a verb of the controller's dispatch table, else as `verb=? verb_len=<n>`, so a token
  or a code sent as a verb name never reaches the file. No line holds a token, a code, its secret or a
  key. Only the device ID names the caller.
- A blocked peer's 429s are counted in one line a minute: the first writes `count=1`, and a later one
  writes a line only 60 seconds after that peer's last `blocked` line, with `count=<n>` for every 429
  since it, itself included. A peer that floods while blocked cannot fill the disk (the host's write
  budget; audit C15's unbounded-ledger limit). The 429s of the last minute of a flood that then stops
  stay uncounted.
- The file is append-only, with no `fsync` per line, and it is not in the store. A crash can lose the
  last lines. A failed append prints one stderr line, and the request still gets its answer. A TLS
  failure before HTTP is no request and writes no line. Rotation and a size cap of both ledgers are
  the feature map's "controller ledger rotation" (audit C15).

**The bind guard (audit C11).**
- Before it makes or loads a key, the controller checks the API address. Allowed: `127.0.0.0/8`,
  `::1`, `100.64.0.0/10` and `fd7a:115c:a1e0::/48`, on any port. Any other address leaves the API
  `failed` with `the API address <ip> is outside loopback and the tailnet (127.0.0.0/8, ::1,
  100.64.0.0/10, fd7a:115c:a1e0::/48)`. No key is made and nothing listens. The tailnet is the fence
  (Q3), and this makes the fence a check, not a habit.

**Idle device expiry (audit C2).**
- A device with no call for 30 days (its `last_seen_at`, or its `paired_at` when it never called) is
  refused with the one 401 text, and the refusal does not touch the device. It stays in `device list`,
  unrevoked. The operator pairs it again with a new code, and may revoke the old row.
- The refusal counts as a failed token call. The router cannot tell an idle token from a guessed one,
  so a client that keeps calling with an idle token blocks its own address, `/v1/pair` included, for
  10 minutes. The UI of `r1.s2.w3` stops at the first 401. A blocked operator waits 10 minutes or
  restarts the controller.
- `last_seen_at` lags by up to a minute (the touch throttle), which does not matter at 30 days.

**A changed server key (audit C11).**
- The server cannot see a client's pin, so this is a client rule (`docs/contracts/api.md`, "The pin
  check"): a client that holds a pin and sees another key closes the connection before it sends
  anything and tells its user `server key changed: re-pair`. The server writes no line for it.

## The pinned client (r1.s2.w4)

*Added 2026-11-08, at `r1.s2.w4`.* This section records the client side of the pin check. It adds no
verb, no route and no wire type to the API, and it changes no decision above.

- **`crates/pf-client` is the pinned client.** One crate serves the Mac app's Rust side and the test
  bridge `pf-client-bridge` (`docs/contracts/ui-transport.md`). Both use the same `Transport` and the
  same verifier, so the app and d12 make the same call.
- **The verifier.** `PinVerifier` is a rustls `ServerCertVerifier`. It ignores the chain, the host name
  and the clock. It reads the end-entity certificate's DER SubjectPublicKeyInfo, hashes it with SHA-256
  and fails the handshake unless the hash is the pin. It still checks the TLS 1.3 handshake signature
  against the certificate's key, so the peer must hold the pinned key. The client config offers TLS 1.3
  only. The handshake fails on the client before the client writes any HTTP byte, and the error is
  `key_changed` with the message `server key changed: re-pair`. A failure before the pin check (a
  refused connection, a timeout, a server that offers TLS 1.2 only) is `unreachable`.
- **The admission rule on the client.** The one function that builds a `/v1/verbs/<verb>` request
  refuses a verb that is not in `pf_api::ALLOWLIST` as `bad_request`, before it connects.
- **`pf-client` reuses `pf-api`** for the wire types and the code decoder (`PairingCode::decode`, the
  allowlist). This pulls the server's crates (`pf-api` and, through it, `pf-engine`) into the Mac app's
  build. The code the client trusts is the code the server uses, so the two cannot drift. Limit: the app
  carries server code it never runs. A later spine may split a wire-only crate out of `pf-api`.
- **Tokens.** `pair` stores the token from `POST /v1/pair` in the device store and does not return it.
  No error message holds the token or the code: a server text is cut to 300 characters and replaced by
  `the server answered <status>` when it holds a secret or any run of 64 hex digits. On the Mac the token
  is a generic password in the login Keychain (ADR-0013); for the bridge and the tests it is one file of
  mode 0600, written by a rename.
- **Limits.** One request per connection, with a 10-second connect timeout and a 30-second request
  timeout. A pin is trust on first use: the code carries the hash, so a code that reaches the app through
  a hostile channel pins that channel's key. The operator carries the code over ssh.

### Verified claims (r1.s2.w4)
- The verifier accepts a self-signed certificate whose SPKI hash is the pin, with any server name and any
  time, and refuses one whose hash is not. A TLS 1.2 server is refused. A wrong pin fails the handshake
  with zero HTTP bytes read by the server (`crates/pf-client/tests/pinned_tls.rs`, 2026-11-08).
- Each API status maps to its kind, a message is cut to 300 characters and never holds a 64-hex token,
  `FileStore` writes mode 0600 by a rename and `forget` removes the file, and `halt`, `ack` and an
  unknown verb are `bad_request` with no connection (`crates/pf-client/tests/`, 2026-11-08).
- Against a real controller: `pair` returns `D-1` and keeps the token in the store, `status`,
  `project_add` and `project_list` work, a used code is `code_refused`, a code for another key is
  `key_changed`, `device revoke D-1` makes the next call `unauthorized`, a controller restarted on a new
  key makes the next call `key_changed` and `api.ledger` gains no line for it, a blocked peer gets
  `rate_limited`, a closed port is `unreachable`, `pair`'s return holds no token, and the token is in no
  error, stderr line or ledger line (`crates/pulse-factory/tests/client_pinned.rs`, 2026-11-08).
- `pf-client-bridge` serves the bridge protocol with the same checks, and d12 passes with it
  (`crates/pulse-factory/tests/client_bridge.rs`, `scripts/demo/ui-pair.sh`, 2026-11-08).

Revisit this ADR when a second operator, a device outside the operator's tailnet, or a browser
client served by the server is planned.
