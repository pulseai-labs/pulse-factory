# ADR-0012 — A network API in the controller: in-process TLS pinned at pairing, a one-use code login, and revocable device tokens

- **Status:** Accepted, 2026-11-07, at `r1.s2.w2`
- **Date:** 2026-11-07
- **Amended:** 2026-11-09, at `r1.s3.w3`: the chat verbs on the allowlist, the stream route, the server
  headers and `upgraded` in the request ledger.
- **Amended:** 2026-11-09, at `r1.s3.w4`: the client half of the stream in `pf-client`, the server
  memory, and the reconnect rule. No route, verb or wire type is added.
- **Amended:** 2026-11-09, at `r1.s4.w1`: six verbs on the allowlist (`requests`, `show`,
  `confirm_code`, `answer`, `clear`, `ack`), the per-device confirm code, the request frames on the
  stream, `refusal` in the error body and `refusal=` in the request ledger, and the revoke window as a
  known limit.
- **Amended:** 2026-11-10, at `r1.s4.w3`: `run_list` on the allowlist, the client's `request_frame`
  event and its `refusal` word, and the `verb` command of the Mac app.
- **Amended:** 2026-11-13, at `r1.s5.w5`: `folder_list` on the allowlist, its confinement to the server's
  home folder, and its one `api.ledger` line. Two verbs that exist gain an optional argument each.

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
- A device may call exactly `project_add`, `project_list` and `project_remove` in this spine. The chat
  verbs of `r1.s3.w3` extend the list (see "The chat stream and the chat verbs (r1.s3.w3)" below).
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
- A device acts as the operator and has no scope of its own (ADR-0009, amended). There are still no
  per-device scopes. The per-device confirm of an act is the confirm code (see the amendment of
  `r1.s4.w1` below).

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
- `last_seen` is one of the store's writes outside admission (ADR-0003, amended; ADR-0015 lists the
  others): a read must not cost an admission row, and the idle expiry of `r1.s2.w5` reads it.
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


## The chat stream and the chat verbs (r1.s3.w3)

Amendment, 2026-11-09. `r1.s3.w3` adds the chat verbs to the allowlist, a WebSocket stream on the same
listener and the server's name on every answer. Per the allowlist rule above, this amendment records
the change.

- **The allowlist** (`pf_api::host::ALLOWLIST`) is now ten verbs: `project_add`, `project_list`,
  `project_remove`, `session_list`, `session_start`, `session_send`, `session_answer`,
  `session_interrupt`, `session_set` and `session_end`. Off it, and named by the router test:
  `session_redact` (CLI-only, so a redaction stays the operator's act on the host), `session_items`
  (the stream's `after=0` gives the history), `halt`, `ack`, `answer`, `claim`, `clear`, `arm`, the
  `device_*` verbs and `api_status`. Each chat verb goes through the CLI's handler path, with its
  command ID, so it is one admission and one `controller.ledger` line. A device acts as the operator
  (ADR-0009), and `ApiBridge::call` reaches the verbs through `handle`.
- **The stream route** is `GET /v1/stream[?session=<S-n>&after=<seq>]`, a WebSocket upgrade on the same
  TLS 1.3 listener with the same pinned key. The token is read from `Authorization: Bearer` **at the
  upgrade**: a bad token is a 401 with no upgrade and counts as a token failure for the peer guard.
  After the token come the method (405), the upgrade headers (400), the query (400), the open (404 for
  an unknown session) and the `101`. A device holds at most 8 streams: the ninth upgrade is 429 `too
  many open streams`, which is not a guard failure. A stream holds one of the listener's 256
  connection permits, so the listener serves with `with_upgrades()`. The `ApiHost` trait gains an
  `open_stream(device, session, after)` call with a default that answers 404, so the router never
  reaches the store and the test hosts compile unchanged.
- **The reconnect order** belongs to the host: it subscribes the connection to the live frames, sends
  `hello`, then the stored items after `after` in pages of 500, then `caught`, then the live frames,
  dropping an `item` whose `seq` it already sent. No item is lost or doubled between the stored read
  and the live stream. Text deltas are never stored and are not replayed.
- **The close codes** are `4001 revoked`, `4002 lagged: reconnect after seq <n>`, `1001` for a
  controller stop and `1008` for a data frame from the client. The stream is read-only, and the server
  pings every 30 seconds.
- **A revoke closes the device's streams.** An admitted `device_revoke` ends every open stream of that
  device with 4001 within one second. Each verb call authenticates its own request, so a revoked token
  is refused at once on the verbs too (C13).
- **The server on every answer.** Every answer, a `101` and each error included, carries
  `pf-server-id` (the SPKI SHA-256 hex of the pinned key) and `pf-server-name`. `ApiStatus` gains an
  optional `server`, so an existing client still parses it. The id is no secret: any TLS peer sees the
  key. This is the groundwork of an app that talks to more than one server.
- **The request ledger** gains the route `stream` and the outcome `upgraded` (the `101`): one line for
  each connection, written when the upgrade is answered, and none at the close. The router maps every
  429 to `blocked`, so a ninth stream's refusal reads `outcome=blocked`.
- **A new crate.** `tokio-tungstenite` (pinned with `=`) and axum's `ws` feature (ADR-0008, amended).
- **The device model** keeps `last_seen` as one of the store's writes outside admission. This
  amendment adds none: the store's writes outside admission stay the five of `r1.s3.w2` and
  `touch_device`.

Known limits: a stream that lags is closed and must reconnect; a reconnect does not replay the text
deltas of a turn in progress (its finished items arrive when it ends); `session_redact` cannot recall
an item that a device already read.

Revisit this ADR when a second operator, a device outside the operator's tailnet, or a browser
client served by the server is planned.

## The pinned client's stream (r1.s3.w4)

Amendment, 2026-11-09. `r1.s3.w4` gives `pf-client` the client half of the chat stream (`GET /v1/stream`)
and the seven chat verbs as typed methods. It adds no route, no verb and no wire type, and it changes
no decision above.

- **One pinned connection for the verbs and the stream.** `pf-client` factors the connect half of its
  call into one function, `connect_pinned`: TCP, then the TLS 1.3 handshake with `PinVerifier`, `key_changed`
  on a pin mismatch and `unreachable` on any other failure. The verbs and the stream both use it, so
  the pin check, "TLS 1.3 only" and "nothing written before the check" are one code path. The upgrade is
  a WebSocket handshake on that TLS stream (`tokio-tungstenite`'s async client).
- **The token travels only in the upgrade.** It is the `Authorization: Bearer` header of the upgrade,
  marked sensitive, and never in the URL. No stream event, error message, `Debug` print, bridge answer or
  webview value holds it.
- **The decode rule.** Each text message is one JSON object. A `type` the client does not know is skipped
  (ADR-0004's additive rule). A known `type` that does not decode into `ChatFrame` ends the stream as
  `server`, so an item is never skipped silently.
- **The reconnect sends the client's own `after`.** The client keeps the last `seq` it holds and sends
  `after=<that seq>` at each reconnect, as `docs/contracts/api.md` says, not the `<n>` of a 4002
  reason. It drops an `item` of the followed session whose `seq` it holds, and a redaction's re-send
  (`redacted_at` set) always passes. Each reconnect delivers the server's fresh `hello` and `caught`.
- **The outcomes.** `4001` is `unauthorized` ("this device was revoked; pair it again") and `1008` is `server`,
  neither retried. `4002` retries at once, `1001`, a drop and a silence of 75 seconds retry on the schedule. A key that is
  not the pinned one is `key_changed` and never retries. Every HTTP answer instead of `101` (400, 401, 404,
  429, 5xx) is one `closed` event with the status's kind and the server's text, and is never retried, a
  429 included. `forget()` ends the client's open streams.
- **The schedule, and why it is not polling.** The retry schedule is 4, 8, 16 and 32 seconds, then every 60
  seconds, and a connection open for 60 seconds or more resets it. While a connection is open no timer makes
  a request: the server pushes, and the silence limit is a local deadline that sends nothing. The schedule
  runs only while there is no connection. An attempt that gets no HTTP answer writes no server line, and an
  HTTP refusal is never retried, so a server that keeps accepting and dropping sees at most one connection
  a minute after the first five attempts, and a connected client costs one `api.ledger` line per connection
  (A8, A10).
- **The server memory.** `Transport` keeps the last `ServerRef` that an answer named (`pf-server-id` and
  `pf-server-name`, on every answer including a `101`, and each `hello`'s `server`), shared by its clones, and
  only when the id is the stored pin. A header that names another id is ignored and the answer is still used:
  the TLS pin is the check, and the header is a label. `forget()` clears it, and `PairedServer.server` carries
  it to the UI. It is the groundwork of an app that talks to more than one server.
- **Limits.** `session_start` and `session_send` may open a chat process, so they wait 90 seconds (the other
  verbs 30). A stream that lags is closed by the server and reconnects; a reconnect does not replay the
  deltas of a turn in progress.

### Verified claims (r1.s3.w4)
- The stream decodes the host's frames in order and skips an unknown `type`; a known type with a bad body ends
  it; `4001` ends it once with no retry; a `4002` after item 5 reconnects at once with `after=5` and drops the
  re-sent items 4 and 5 while a redaction's re-send passes; `1001` and a dropped feed retry on the schedule, and
  a long connection resets it; silence past the limit counts as a drop; a listener that starts later gets the
  next attempt; another key is `key_changed` with no request and no retry; each HTTP refusal is one `closed`
  event and one host record; a held stream makes one upgrade and no request on a timer; the token is in the
  upgrade only; the caller's close and `forget()` end a stream
  (`crates/pf-client/tests/stream.rs`, 2026-11-09).
- The seven chat verbs post their arguments and parse their answers, `session_start` and `session_end` read
  the controller's own answers, the chat-open timeout is longer than the request timeout, and the server memory
  behaves as above (`crates/pf-client/tests/chat_verbs.rs`, 2026-11-09).
- Against a real controller with `FakeChat`: the whole chat flow, a cut TCP relay (every item once, in order,
  no gap), a revoke (`closed` within a second, no later upgrade in `api.ledger`, the next call `unauthorized`),
  one `route=stream` line per connection and none while a stream is idle, and the bridge carrying the same
  frames (`crates/pulse-factory/tests/client_chat.rs`, 2026-11-09).

## The device verbs and the confirm code (r1.s4.w1)

This amends the allowlist rule: a later spine that adds a verb amends this ADR, and this item does.

- **Six verbs join the allowlist:** `requests`, `show`, `confirm_code`, `answer`, `clear`, `ack`.
  `claim`, `arm`, `halt`, `device_pair` and `device_revoke` stay off it. `confirm_code` is on the
  API only. `confirm_code`, `answer`, `clear` and `ack` go through the controller's device path
  (`handle_device`), which is not on the socket; `requests` and `show` run through the ordinary
  handler.
- **The per-device confirm is the confirm code.** It replaces "the human confirm of a merge-ack comes
  with `r1.s4`". A code is one-use, 120 seconds, in memory only, bound to the device, one request, its
  revision, one verb with its parameters, the claim generation at issue and, for an ack, the PR's head
  SHA. There are still no per-device scopes. `docs/contracts/api.md` and `requests.md` hold the rules.
- **A refusal has a word.** The closed list is `pf_api::host::REFUSALS`. The 422 body carries
  `refusal`, `RequestRecord.refusal` carries it, and the request ledger line ends with
  `refusal=<word>`. A word outside the list is dropped.
- **Request frames.** `RequestFrame` (`requests`, `request`, `claim`) is a second wire type on the
  same stream. `ChatFrame` does not change, and a client skips a `type` it does not know.
- **No secret in a log.** No confirm code is in any ledger, the store, stderr or an error. The store
  keeps its SHA-256 in the admission content, and the code table keeps only the hash.
- **Known limit (the revoke window).** A revoke refuses the device's next call (401) and voids its
  live codes, but an act already past the code check completes, because the check and the engine call
  run under one lock and the revoke does not wait for it. An ack in flight still merges. A revoked
  device's claim stays current until the operator's next `claim`.
- **Known limit.** The code proves the device, the freshness and the head SHA, not that a person
  pressed the button. A paired device's token that is stolen, with an unspent code, acts until the
  operator revokes the device.

## The settings verbs (r1.s4.w4)

- **Three verbs join the allowlist:** `settings_get`, `settings_set` and `settings_unset`. A device
  acts as the operator (ADR-0009), so it reads and writes the scoped settings of ADR-0016. The verbs
  are in `pf_api::host::ALLOWLIST` and in the request ledger's list of verb names, and the router test
  names the three, and the verbs that stay off the list, with no exact count.
- **A device write goes through the same admission as the CLI's:** `settings_set` and `settings_unset`
  commit through `run_command` under `@controller` with a command ID, and the bridge passes the device
  (`by=D-<n>`). A device is no local process, so the controller's caller check does not apply to it. The
  router has authenticated the token and refuses a revoked one with 401 before the bridge runs.
- **The ledgers.** The `controller.ledger` line of a device's write ends `by=D-<n> command=<id>`.
  The `api.ledger` has one line for the call, with the device and the outcome (`ok`, `replayed` or
  `refused`).
- **The wire types** are generated with `ts-rs` into `apps/api-client/src/gen/` (ADR-0013).
- Rules and limits: ADR-0016. The verbs' arguments and answers are `cli.md`'s.

## The device `arm` and the `arm` subject (r1.s4.w2)

Each spine that adds a verb amends this ADR (the allowlist rule). This one adds one.

- **`arm` joins the allowlist.** It takes `DeviceArmArgs { proposal, plan_sha256, code, command_id }`
  and answers `ArmActed`. It runs only with a confirm code from a start card. The operator's CLI
  `arm` is unchanged, and a socket `arm` that carries a code and no plan is refused.
- **The `arm` subject of the confirm code** is `{ kind: "arm", proposal, plan_sha256 }`. The code binds
  the proposal and `plan_sha256`, the SHA-256 of the plan, its summary and the package snapshot's
  pin. It binds no claim generation. A wrong hash, an unknown, withdrawn, failed, armed or
  being-armed proposal is refused `subject_moved`. The nine refusal words do not change.
- **The spend checks the plan again.** It checks the snapshot's hash before any of its code runs, then
  builds the plan again from the snapshot. A different hash is `subject_moved`.
- **Two chat item kinds** reach the stream: `start_card` and `start_result`. `ConfirmIssued` keeps its
  required `subject`, and gains an optional `start`.

## The run list, the request frames in the client and the `verb` command (r1.s4.w3)

Amendment, 2026-11-10. This item amends the allowlist rule again, as its own text asks: a later spine
that adds a verb amends this ADR.

- **One verb joins the allowlist: `run_list`.** It is a typed read of every run the controller holds:
  `RunList { schema_version, runs }` with one `RunSummary` per run (`id`, `spine`, `workflow`, `state`,
  `halted_reason`, `end_reason`, `open_requests` and `waiting`). `waiting` is the `end_when` waiting
  text that the last tick noted, which the driver now records beside its notes; it is what lets a device
  show a run that waits for the operator and see it end. A device had no read of a run: `runs` and
  `status` stay off the allowlist (`status` is untyped and large, `runs` has no waiting text, and the
  router test keeps both off).
- **Its controls.** Device auth only (the router's token check; the peer guard counts a failure),
  read-only (no admission, no `controller.ledger` line, no store write, no driver action; the one
  `api.ledger` line is its only trace), one allowlist entry, and no run units, sessions, costs or tokens
  in the answer. `run_list` is in the request ledger's list of verb names.
- **The client carries the request frames.** `pf-client` decodes a `type` of `requests`, `request` or
  `claim` into `RequestFrame` and emits it as its own event, `{"event":"request_frame","frame":…}`,
  beside `frame`, so no consumer of `ChatFrame` changes (the client half of the `r1.s4.w1` rule that a
  client skips a `type` it does not know). A request frame that does not decode is **dropped and
  logged, and the stream goes on**: the requests state is a cache that the next `requests` snapshot
  repairs, and ending the stream would cut the chat it carries. The log is one stderr line per
  connection, with no frame text, because a request carries seat-written text. A request frame never
  moves the client's last chat `seq`.
- **The client carries the refusal word.** `ClientError` gains `refusal`, kept only for a 422 and only
  when the word is in `pf_api::host::REFUSALS`; an error with no word keeps its JSON `{"kind",
  "message"}` byte for byte, and the test bridge's error envelope carries the word beside them. The
  client treats a call's `code` argument as a secret when it builds an error: a server text that holds
  it becomes `the server answered <status>`.
- **`ack` waits up to 90 seconds.** It merges through the forge. A timeout is `unreachable`, and a
  retry with the same command ID replays and acts on nothing.
- **The Mac app reaches every verb added since `r1.s4` through one `verb` command.** It calls
  `pf-client`'s `Transport::verb`, which checks the name against `pf_api::ALLOWLIST` before it
  connects, so the command passes only allowlisted verbs and a webview reaches no verb a device may not
  call. A later item's verb needs no edit in `apps/desktop`. `pf-client` re-exports `serde_json::Value`
  for it, so the desktop's manifest and lock do not change.
- **No other change.** No route, no admission, no store change and no new frame.

## The folder list (`r1.s5.w5`)

Amendment, 2026-11-13. This item amends the allowlist rule again, as its own text asks: a later spine
that adds a verb amends this ADR. The Mac app's Add a project picker needs to see the folders of the
server. A device had no read of the server's disk.

- **One verb joins the allowlist: `folder_list`.** `FolderListArgs { path? }` answers a `FolderList`: the
  folders of one path, with each folder's git mark and branch, and the live project whose root it is. The
  list is sorted by name and holds at most 500 entries.
- **It is confined to the home folder.** The controller's home folder is its `$HOME`, made canonical at
  start. The verb makes the requested path canonical first, so `..` and a symlink are resolved before the
  check, and the canonical path must be the home folder or lie under it. Otherwise it is refused with
  no listing. A symlink inside the home folder whose target leaves it is not listed. A path outside the
  home folder gets the same refusal whether it exists or not, so the verb is no oracle for the rest of the
  disk. One limit: a path lexically inside the home folder that does not resolve (a dangling symlink)
  answers that it does not exist, so a symlink already inside the home folder shows whether its target
  resolves; the verb only reads and cannot make one. A server with no home folder refuses.
- **It opens no file.** It reads directory entries and metadata, and runs git against a folder that has a
  `.git`. A file, a FIFO, a socket and a broken link are skipped, so none can stop or slow a listing. No
  file content, remote URL or token reaches the answer, a ledger or stderr.
- **Its controls.** Device auth only (the router's token check; a revoked device is 401 at once; the peer
  guard counts a failure), read-only (no admission, no `controller.ledger` line, no store write), and one
  `api.ledger` line for each call, with the device, the verb and the outcome. It takes no confirm code,
  because it binds no act. It has no CLI form: the CLI user has a shell.
- **Two optional arguments on verbs that exist.** `project_add` gains `git_init` and `project_remove`
  gains `end_sessions`. Both verbs were on the allowlist already. Each keeps the admitted path of a
  state-changing command, with its `controller.ledger` line. `end_sessions` makes each session it ends write
  its own `chat_session_end` line. ADR-0015 holds their rules.
- **`RunSummary` gains `project_id`** (the ninth field), so a device can tell that a run belongs to a
  project before it asks to remove the project. It is the project of the chat that proposed the run.
- **No other change.** No route, no confirm code and no store change.
