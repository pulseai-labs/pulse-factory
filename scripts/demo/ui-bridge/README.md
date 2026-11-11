# The test bridge

The test bridge is `pf-client-bridge`, a binary of the `pf-client` crate (`crates/pf-client`). It was a
Node script here until `r1.s2.w4`.

It serves the built UI bundle and the bridge protocol of `docs/contracts/ui-transport.md` on
`127.0.0.1`, and it makes the pinned TLS call to the controller with the real client. It stands in
for the Mac app's Rust side where the Mac app cannot run, such as headless Chromium on draco-desk
(the `desktop-bridge` fake in `SPINE.md`). It is for `scripts/demo/ui-pair.sh` (d12) and tests only.
The Mac app does not use it.

```sh
pf-client-bridge --ui <dir> --state <dir>   # prints: listening http://127.0.0.1:<port>
```

`scripts/install.sh` installs it beside `pulse-factory`. `scripts/demo/ui-pair.sh` finds it through
`PF_BRIDGE_BIN`, then next to `PF_BIN`, then on `PATH`.

## The chat

Since `r1.s3.w4` the bridge carries the chat too (`docs/contracts/ui-transport.md`, "The chat stream"):

- `POST /bridge/verbs/<verb>` reaches the seven chat verbs (`session_list`, `session_start`,
  `session_send`, `session_answer`, `session_interrupt`, `session_set`, `session_end`) through the typed
  methods of `pf-client`, so a bad argument fails at the bridge. `session_start` and `session_end`
  answer the transport's `SessionStarted` and `SessionEnded`. `session_redact` and `session_items`
  are off the allowlist and answer `bad_request` with no connection.
- `GET /bridge/stream[?session=<S-n>&after=<n>]` is the chat stream as Server-Sent Events: `200`,
  `Content-Type: text/event-stream`, each event one `data: <JSON>` line, and the response ends after a
  `closed` event. The reconnect is `pf-client`'s: the bridge sends a `reconnecting` event while it waits.
  When the page drops the response, the bridge closes the upstream socket. A browser's `EventSource`
  reaches it with no change to the content security policy.

The bridge still writes nothing to stdout or stderr but its one `listening` line.

## Needs you and the Settings rows

Since `r1.s4.w3` the bridge carries the device verbs, the settings verbs and `run_list` too:

- `POST /bridge/verbs/<verb>` reaches `requests`, `show`, `confirm_code`, `answer`, `clear`, `ack`,
  `settings_get`, `settings_set`, `settings_unset` and `run_list` through `pf-client`'s `transport.verb`,
  which checks the name against the allowlist before it connects. A refused device verb answers the
  error envelope with its `refusal` word beside `kind` and `message`; an error with no word is exactly
  `{"kind", "message"}`.
- The stream's SSE carries the request frames as `{"event":"request_frame","frame":{…}}` lines, beside
  the `frame` events of the chat. A request frame that `pf-client` cannot read is dropped and the stream
  goes on.

**The one exception to the rule above** (the bridge writes nothing to stdout or stderr but its
`listening` line): `pf-client` writes one stderr line per connection for a dropped request frame,
`pf-client: dropped a request frame this client cannot read (<n> so far on this stream)`, with no frame
text. The bridge prints no confirm code, token or answer text.
