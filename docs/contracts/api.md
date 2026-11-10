# API contract

The controller serves a network API over TLS (ADR-0001, ADR-0012). A paired device, such as the
MacBook app, calls the same verbs as the CLI through it. This file is version 1 of the API. The CLI
verbs that manage it are `device` and `api` in `cli.md`. The Rust types are in `crates/pf-api`
(`types.rs`), and the TypeScript types that clients use are generated from them into
`apps/api-client/src/gen/`.

## Common rules

- **Off by default.** The API listens only on the address in `--api-listen` or `PF_API_LISTEN`. With
  neither, nothing listens and no key exists (see "The listener").
- **TLS 1.3 only**, with HTTP/1.1 on top. The server's certificate is self-signed. A client does not
  use a certificate chain: it pins the key (see "The pin check").
- **JSON.** Every answer except a refusal at the TLS layer is JSON with `Content-Type:
  application/json` and `Cache-Control: no-store`. A request body is JSON and at most 1 MiB.
- **Versioned path.** Every route is under `/v1`. Every success answer that is an object holds
  `schema_version` (now `1`). A change that removes or renames a field raises it, as for the CLI
  (ADR-0004).
- **Errors.** Every error answer is `{"error": "<text>"}`. The text never holds a token, a pairing
  code, a confirm code or a key. A refused device verb (422) also holds `"refusal": "<word>"`, one
  word of the closed list in "Confirm codes": `{"error": "<text>", "refusal": "code_used"}`. A client
  that reads no `refusal` still reads the error text.
- **The token.** A route that needs a device token reads `Authorization: Bearer <token>`. The token is
  64 lower-case hex digits (32 random bytes). The server keeps only the SHA-256 of the 32 bytes. The
  token appears once, in the answer of `POST /v1/pair`, and nowhere else.
- **The server on every answer.** Every answer of every route, a `101` and every error included,
  carries two headers: `pf-server-id: <SPKI SHA-256 hex of the key pinned at pairing>` and
  `pf-server-name: <name>`. The name is `PF_SERVER_NAME`, else the kernel's host name, else `server`,
  1 to 64 characters of `[A-Za-z0-9._-]`. The id is no secret: any TLS peer sees the key. A client
  that talks to more than one server tells them apart by these two headers. `GET /v1/status` and the
  `hello` frame carry the same pair as a `server` object (`{"id": "<hex>"|null, "name": "…"}`; the id
  is null only with the API off).
- **Acting as the operator.** A device acts as the operator and has no scope of its own (ADR-0009). A
  call goes through the controller's verb handler, the same code that the Unix socket runs, so each
  admission, refusal and ledger line is the CLI's. The exceptions are the device verbs `confirm_code`,
  `answer`, `clear` and `ack`: they go through the controller's device path, where a one-use confirm
  code replaces the claim token (see "Confirm codes").

## Routes

### `POST /v1/pair`

No token. The body is `{"code": "<the pairing code>", "device_name": "<1 to 64 characters>"}`. The
name has no control character and no line break.

It answers 200 with:

```json
{"schema_version": 1,
 "device": {"id": "D-1", "name": "…", "paired_at": "…", "last_seen_at": null, "last_peer": null, "revoked_at": null},
 "token": "<64 hex digits>"}
```

The server decodes the code, checks that its SPKI hash is the server's own, checks the name, compares
the secret in constant time and uses the code up before it answers. The redeem is the admission
`device_pair` under `@controller`, with one controller-ledger line that names the device, its name
and the peer address.

- **401** `{"error": "the code is not accepted"}` for every bad code, with no detail that says which:
  a malformed one, one that holds another server's key, a wrong secret, a used one, an expired one,
  and one that a newer code replaced.
- **422** with the reason for a device name that breaks the rules above.
- **400** for a body that is not JSON of that shape. **413** for a body over 1 MiB.
- A wrong secret and a bad name do not use up the live code, so a guess or a typo costs the operator
  no code.

### `GET /v1/status`

Needs a token. It answers 200 with `{"schema_version": 1, "version": "<server version>",
"key_spki_sha256": "<64 hex digits>", "device": {…the calling device's record…}, "server": {"id":
"<64 hex digits>", "name": "<name>"}}`. `server` is optional on the wire, so a client written before
`r1.s3.w3` still parses the answer.

### `POST /v1/verbs/<verb>`

Needs a token. The body is the verb's JSON arguments, and an empty body means `{}`. The 200 answer is
the verb's JSON answer, the same JSON that the CLI prints with `--json`. A state-changing verb takes
a `command_id` argument, as on the CLI: the same ID with the same content returns the first record
with `replayed` true.

- **403** `{"error": "the verb \"<verb>\" is not open to devices"}` for a verb that is not on the
  allowlist, and for an unknown verb. The call does not reach the handler.
- **422** with the handler's message when the controller refuses the command (for example a root that
  does not exist). A refused device verb adds `refusal` (see "Confirm codes").
- **400** `{"error": "the body is not JSON"}` for a body that is not JSON. **413** for a body over
  1 MiB.

### `GET /v1/stream[?session=<S-n>&after=<seq>]`

A WebSocket upgrade on the same TLS 1.3 listener, for the chat of a paired device. It needs a token
in `Authorization: Bearer <token>`, as every route does. The stream is read-only: the client acts
through the verbs, and reads here.

**The order of the checks:** the peer guard's block check (429); the token (401, no upgrade; it
counts as a token failure for the peer guard, like a 401 on any token route); the method (a
method other than `GET` is 405); the upgrade headers (400 when absent); the query (400 for a session
ID that is not `S-<n>` or an `after` that is not a number); the controller's open (404 for an
unknown session); then `101 Switching Protocols`. A refusal before the `101` is an ordinary JSON
error answer and no upgrade. At most **8** streams are open per device: the ninth upgrade is
**429** `{"error": "too many open streams"}`. That 429 is not a guard failure and does not block the
peer.

**The query.** `session` names the session to follow, and `after` is the last `seq` the client
already holds (default 0). With no `session` the stream carries the overview only.

**The frames.** Each is one WebSocket text message holding one JSON object with a `type`:

```json
{"type":"hello","server":{"id":"4673cd6f…933c","name":"demo-server"},"sessions":[{…ChatSessionRecord…}],"following":"S-1","after_seq":0}
{"type":"session","session":{…ChatSessionRecord…}}
{"type":"item","item":{"session_id":"S-1","seq":1,"answers_seq":null,"created_at":"2026-11-08T22:54:03.764Z","redacted_at":null,"body":{"kind":"operator_message","text":"hello"}}}
{"type":"caught","session_id":"S-1","last_seq":6}
{"type":"delta","session_id":"S-1","text":"The command was denied."}
{"type":"tool_running","session_id":"S-1","call_id":"toolu_01V9…","tool":"Bash"}
```

The same stream carries three more frames, the **request frames** (`RequestFrame`, a separate wire
type; the chat frames above do not change). Each holds a `RequestSummary`, as `requests` rows do, and
a `ClaimRecord`. A client that does not know a `type` skips it.

```json
{"type":"requests","requests":[{"id":"R-4","revision":1,"kind":"merge_ack","status":"open","run":"r1-s4","unit":null,"stage":"close","blocks":[],"opened_at":"2026-11-09T21:03:11.402Z","answered_at":null,"reason":"the spine PR waits for the claim's ack","verbs":["ack"],"pr":"pulseai-labs/pilot-fourseat#9","base":"main","head_sha":"9f2c0d1e5b7a43c8d6e1f0a2b3c4d5e6f7a8b9c0"}],"claim":{"generation":2,"holder":"device D-1 (MacBook)","device":"D-1","revoked":false,"claimed_at":"2026-11-09T21:04:50.118Z"}}
{"type":"request","request":{"id":"R-4","revision":2,"kind":"merge_ack","status":"open","run":"r1-s4","unit":null,"stage":"close","blocks":[],"opened_at":"2026-11-09T21:03:11.402Z","answered_at":null,"reason":"the spine PR head moved","verbs":["ack"],"pr":"pulseai-labs/pilot-fourseat#9","base":"main","head_sha":"0a1b2c3d4e5f60718293a4b5c6d7e8f901234567"}}
{"type": "claim", "claim": {"generation":3,"holder":"ann@draco-desk:claude","device":null,"revoked":false,"claimed_at":"2026-11-09T21:09:02.771Z"}}
```

- `requests` is sent on every connection right after `hello`, before the stored items. It holds the
  open requests of every run and the current claim (null when none). Reading `requests` on reopen is
  enough on its own: the stream only keeps it live.
- `request` is sent when a request is new or changed: opened, revised, answered or withdrawn.
- `claim` is sent when the current claim changes, with `claim` null when none holds it. A
  `confirm_code` changes no claim and sends none.
- The controller sends them after each tick and after each request verb, from the socket or the API.
  A change that lands between the snapshot and the live frames arrives twice, so a reader keys by
  `(id, revision, status)`. A request frame goes to every stream, whatever session it follows. A
  stream that lags closes with 4002, as for the chat frames.

- `hello` comes first. It names the server, holds every live session as a `ChatSessionRecord`
  (id, project, worktree, branch, state, process with its `qualified` flag and holders, model,
  effort, permission mode, runtime options, pending options, summed usage, the cache-warm time,
  the context gauge), `following` (the session ID or null) and `after_seq`.
- `session` carries a session's record again after anything that changes it: its state, its cost,
  its holders, its process. The app never polls (A8). A stream with no `session` query carries
  `hello`, `session` and the request frames only.
- `item` carries one stored item of the followed session: `operator_message`, `assistant_text`,
  `tool_call` (with its `input` and `output` as `{text, bytes, truncated}`), `approval_request`,
  `approval_answer`, `question_request`, `question_answer`, `background_task`, `notice`,
  `turn_result` (with its `usage` and `cost_micros`), `start_card`, `start_result`, `redacted` and
  `unknown`. A `start_card` holds `proposal`, `project_id`, `project_name`, `plan_sha256`, the
  `summary` (an `ArmSummary`), `params`, `ledger` and `plugin_roots`. A `start_result` answers a card
  once: `outcome` (`answered` or `withdrawn`), `result` (`armed`, `withdrawn` or `failed`), and
  `reason`, `run` and `device` when they apply. Its `seq` counts
  from 1 within the session with no gap. A redaction by `session redact` sends the item again with
  `redacted_at` set and the same `seq`. An open stream receives this frame even though it already
  holds that `seq`, and the client replaces its copy. A device that was not connected keeps the
  copy it read before (ADR-0015's known limit).
- `caught` says the stored items are sent. What follows is live.
- `delta` carries the text of a turn as it streams. Deltas are never stored, and a reconnect does
  not replay them. `tool_running` says a tool call started. The finished call arrives as one `item`.

**The reconnect.** The controller subscribes the connection to the live frames first. Then it sends
`hello`, then the stored items of the followed session with a `seq` above `after` in `seq` order
(pages of 500), then `caught`, then the live frames, dropping any `item` whose `seq` it already
sent, except a redaction's re-send (`redacted_at` set), which always passes. No item is lost or
doubled between the stored read and the live frames. A client that reconnects sends the last `seq`
it holds as `after`.

**The close codes.** The server closes with:

| Code | Reason | When |
|---|---|---|
| `4001` | `revoked` | An admitted `device_revoke` closed the device's streams at once. |
| `4002` | `lagged: reconnect after seq <n>` | The connection fell behind the live frames. Reconnect with `after=<n>`. |
| `1001` | `going away` | The controller is stopping. |
| `1008` | `policy violation` | The client sent a data frame. The stream is read-only. |

The server pings every 30 seconds. A stream holds one of the listener's 256 connection permits while
it lives. A revoke closes every open stream of that device within one second, and each verb call
checks its own token, so a revoked token is refused at once on the verbs as well. A stream request
that the router authenticated before a revoke committed, and that the host opens after it, is
refused with 401 at the open, so no stream of a revoked device stays open.

### Status codes of every route

| Code | Meaning |
|---|---|
| 101 | The stream upgraded (`GET /v1/stream`). |
| 200 | The call worked. |
| 400 | The body is not JSON, or not JSON of the shape the route needs. |
| 401 | No token, a malformed token, an unknown token, a revoked token, an idle device (on a token route); or a bad code (on `/v1/pair`). |
| 403 | The verb is not on the allowlist. |
| 404 | Any other path, including `/v2/…`. |
| 405 | A known route with the wrong method. |
| 413 | The body is over 1 MiB. |
| 422 | The controller refused the command, or the device name is invalid. |
| 429 | The peer address is blocked after 5 failures (see "Guards"), or a device has 8 streams open already. |
| 500 | The controller failed inside the call. |

**One 401 for every reason.** On a token route the 401 is `{"error": "not authorized"}` whether the
header is missing, is not `Bearer <64 hex>`, names an unknown token, or names a revoked device. A
caller cannot tell a revoked token, or the token of an idle device, from a token that never existed. A route is checked for its token
before the verb or the body is read, so an unpaired caller learns nothing about the verbs.

## The allowlist

A device may call exactly these verbs through `POST /v1/verbs/<verb>`:

| Verb | CLI form |
|---|---|
| `project_add` | `project add` |
| `project_list` | `project list` |
| `project_remove` | `project remove` |
| `session_list` | `session list` |
| `session_start` | `session start` |
| `session_send` | `session send` |
| `session_answer` | `session answer` |
| `session_interrupt` | `session interrupt` |
| `session_set` | `session set` |
| `session_end` | `session end` |
| `requests` | `requests` |
| `show` | `show` |
| `confirm_code` | none (API only) |
| `answer` | `answer`, with a confirm code in place of the token |
| `clear` | `clear`, with a confirm code in place of the token |
| `ack` | `ack`, with a confirm code in place of the token |
| `arm` | none (API only): `arm`, with a confirm code from a start card in place of a plan |
| `settings_get` | `settings get` |
| `settings_set` | `settings set` |
| `settings_unset` | `settings unset` |
| `run_list` | none (API only); on the host, `status` shows the same runs |

Every other verb is 403: `runs`, `status`, `claim`, `propose`, `halt`, `hook`, `device_pair`,
`device_list`, `device_revoke`, `api_status`, `session_items` and `session_redact`. `session_redact` (the redaction of one chat item) is CLI-only: a device that
read the item before keeps its copy (ADR-0015's known limit), and the operator decides on the host.
`session_items` is off the list because the stream's `after=0` gives the history. There is no way
for a device to pair another device, revoke one, claim, propose or halt. A device takes the claim only
by acting with a confirm code: there is no `claim` verb on the API. `halt` stays off the list. A
device arms a run only through `arm` with a confirm code from a start card, and that arm takes no
claim (`r1.s4.w2`).

The chat verbs take the arguments and give the answers that `cli.md` lists under `session`. Each
state-changing one takes a `command_id`, and the same ID with the same content returns the first
record with `replayed` true and acts on nothing.

The settings verbs (`r1.s4.w4`, ADR-0016) take the arguments and give the answers that `cli.md`
lists under `settings`: `SettingsGetArgs` and `SettingsView`, `SettingsSetArgs`, `SettingsUnsetArgs`
and `SettingChanged`, each a `ts-rs` type in `apps/api-client/src/gen/`. `settings_set` and
`settings_unset` take a `command_id` and commit through the same admission as the CLI's, under
`@controller`, with kind `setting_set` or `setting_unset`. A device is no local process, so the
controller's caller check does not apply to it: the router has authenticated the token and refuses a
revoked one (401), and the write is admitted as the operator's (ADR-0009). The `controller.ledger`
line of a device's write names it, `setting_set <key> admitted scope=<scope> value=<json> by=D-<n>
command=<id>`, and the request ledger has one `api.ledger` line for the call, with `verb=settings_<…>`,
`device=D-<n>` and the outcome (`ok`, `replayed`, or `refused` for an unknown key or a bad value).

### The device verbs

Each takes a JSON body. A state-changing verb takes a `command_id`: the same ID with the same content
returns the first record with `replayed` true and acts on nothing, with no second engine call and no
second merge.

- **`requests`**: `{"run"?: "<run>", "all"?: true}`. The answer is a `RequestList`: `schema_version`,
  `open`, `requests` (each a `RequestSummary` with `verbs`, and `pr`, `base` and `head_sha` for a
  `merge_ack`) and `claim` (the current claim, or null). It is the JSON of the CLI's `requests --json`.
- **`show`**: `{"id": "R-4"}`. The answer is a `RequestDetail`: the summary and `body`, `answer` and
  `answer_generation`. It is the JSON of `show --json`.
- **`confirm_code`**: `{"subject": {"kind":"request","request":"R-4","revision":1,"verb":"ack"},
  "command_id": "<id>"}`. The `verb` is `answer`, `clear` or `ack`, and it must be one that the
  request takes (`ack` for a `merge_ack`, `answer` or `clear` for the rest). An `answer` subject
  adds `"text_sha256": "<64 lower-case hex digits>"`, the SHA-256 of the answer text; the text never
  reaches the controller at issue. A `clear` subject adds `"to": "<stage>"` or no `to`. The answer
  is a `ConfirmIssued`: `schema_version`, `replayed`, `code` (null on a replay), `expires_at`,
  `device`, `generation` (the claim generation the code binds, or null), `holder` (the claim at
  the issue, so a dialog can say whom the act takes the claim from) and `subject` (the request,
  revision, kind, run, unit, reason, and for a `merge_ack` the `pr`, `base`, `head_branch` and
  `head_sha`). **Issuing a code moves no claim.** A replay of the command ID returns this first
  record with `replayed` true and `code` null, whatever moved since; after a restart, or 10 minutes
  past the code's expiry, the first record is gone and a replay carries the facts the store still
  has, with `expires_at` and `generation` null.
  The subject of a start card is `{"kind": "arm", "proposal": "sp-…", "plan_sha256": "<64 hex>"}`.
  The code binds that proposal and that plan hash, takes no claim (`generation` and `holder` are
  null), and the answer's `subject` has `kind` `start`, `request` the proposal and `reason` `start
  spine <spine>`. `ConfirmIssued.start` is a `StartShown`: `proposal`, `session`, `project_id`,
  `spine` and `plan_sha256`, so a dialog can name what the code starts. A proposal that is unknown
  (a restart), withdrawn, failed, being armed, already armed or whose plan hash differs is refused
  `subject_moved`, and the refusal text names the cause.
- **`answer`**: `{"request","rev","text","code","command_id"}`. The answer is a `RequestActed`:
  `schema_version`, `replayed`, `verb`, `device`, `generation`, `request` (the record after the act),
  and `resumed`, `hook` for an answer. Fields that do not belong to the verb, and every field on a
  replay, are null.
- **`clear`**: `{"request","rev","to"?,"code","command_id"}`. The request names the unit and must be
  its open request. The answer is a `RequestActed` with `to_stage`.
- **`ack`**: `{"request","rev","code","command_id"}`. The answer is a `RequestActed` with `merged`,
  `head_sha`, `pr`, `base` and `outcome`. The ack merges the spine PR through the engine path of the
  CLI's `ack`.
- **`run_list`** (`r1.s4.w3`): `{}` (`RunListArgs`; unknown fields are ignored). The answer is a
  `RunList`: `schema_version` 1 and `runs`, oldest first, one `RunSummary` for every run the controller
  holds (an ended run too, while the controller holds it). A `RunSummary` has exactly eight fields: `id`,
  `spine`, `workflow`, `state` (`active` or `ended`), `halted_reason` (null unless the run is halted),
  `end_reason` (null unless it ended), `open_requests` and `waiting`. **`waiting`** is the `end_when`
  waiting text that the last tick noted, rendered from the run's workflow (`workflow-file.md`,
  `end_when`), and null when the last tick noted none. It is what a device shows for a run that waits
  for the operator. `status` is off the allowlist, so this is the one read of a run that a device has.
  The verb is **device auth only** (the token check, and the peer guard counts a failure), **read-only**
  (no admission, no `controller.ledger` line, no store write, no driver action: the one `api.ledger`
  line, `verb=run_list`, is its only trace), and **one allowlist entry**. It carries no run units,
  sessions, costs or tokens. `status`'s JSON does not change.
- **`arm`** (`r1.s4.w2`): a `DeviceArmArgs`, `{"proposal","plan_sha256","code","command_id"}`. The
  answer is an `ArmActed`: `schema_version`, `replayed`, `device`, `proposal`, `run_id` and
  `project_id` (`run_id` is null on a replay that cannot name the run). The controller spends the code, then builds
  the plan again from the proposal's package snapshot and compares `plan_sha256`. A changed plan or a
  changed snapshot is a 422 `subject_moved` and the card gets a `failed` result. A transient cause
  (an earlier run is still active) is a 422 with the cause. The code is spent, and the proposal
  stays open for a new code. On success the run belongs to the chat's project, and the card gets a `start_result` with
  `result` `armed`, the run and the device. The device takes no claim, and the CLI's claim is
  untouched. A replay of the command ID names the same run and arms nothing again. `arm` on the Unix
  socket with a `code` and no plan is refused (`confirm codes are for paired devices; …`).

`answer`, `clear` and `ack` move the claim to the device when they act (`requests.md`, "The claim"):
the first device to act wins. An engine refusal after the controller admitted the act (checks red, a
halted run, a changed pinned file) is a 422 with the engine's cause, and the code is spent. A
`confirm_code`, `answer`, `clear` or `ack` on the Unix socket is refused (`confirm codes are for
paired devices; on this host use pulse-factory claim`). The peer guard covers these verbs: the token
is checked first, so five bad tokens on `confirm_code` block the peer as on any route. A refused
code is a 422 and no guard failure.

### Confirm codes

A device must hold a one-use confirm code to act on a request, or to arm a run from a start card. The code binds one request, its
revision, one verb with its parameters (`text_sha256` or `to`), the device, the claim generation
current at issue and, for an ack, the PR's head SHA. It is `pfc1-` and 32 hex digits, it works once,
and it expires 120 seconds after issue. At most 4 codes of a device are live. Codes live in memory,
and a restart voids them. The full rules are in `requests.md`, "Confirm codes".

A refused code is a 422 whose body holds `refusal`, one word of this closed list
(`pf_api::host::REFUSALS`):

| Word | Meaning |
|---|---|
| `code_unknown` | The table never held the code. Every live code of the calling device is voided. |
| `code_foreign` | Another device's code. That code is spent. |
| `code_used` | The code was spent before. |
| `code_voided` | The 4-per-device cap or a revoke voided the code. |
| `code_expired` | The code is past its time. |
| `code_subject` | The request, verb or parameters differ from those the code binds. |
| `subject_moved` | The request is closed, its revision moved, or the PR head moved. |
| `claim_moved` | The claim moved to someone else since the issue. The text names the new holder. |
| `device_revoked` | The device was revoked after the router checked its token. |

A word outside the list is dropped from the body and from the ledger. A revoke refuses the device's
next call with 401 and voids its live codes, but an act already past the code check completes, so an
ack in flight still merges. That is a known limit. The code proves the device, the freshness and the
head SHA, not that a person pressed the button.

**Each later spine that adds a verb amends ADR-0012** (planning audit C2). The list lives in
`pf_api::host::ALLOWLIST`, and the router test names every verb that must stay off it.

## Pairing codes

`pulse-factory device pair` (CLI only) prints one code. A code is one copyable string:

```
pf1-<base32>
```

`<base32>` is RFC 4648 base32, lower case, no padding, of these bytes in order:

| Bytes | Content |
|---|---|
| 1 | The version, `1`. |
| 10 | The secret: 80 bits from the random device. |
| 32 | The SHA-256 of the server key's SubjectPublicKeyInfo (the SPKI hash). |
| the rest | The listen address as UTF-8, such as `100.90.203.21:7443`. |

- The code works once. It expires 10 minutes after `device pair` made it. A new code replaces the
  live one, so at most one code is live. A controller restart voids it.
- The server keeps the secret in memory. The admission `device_pair` uses the SHA-256 of the secret as
  its command ID, so the store sees a redeem of one secret at most once.
- A client reads a code with surrounding white space and either letter case.
- The code is a secret. It is shown only in the output of `device pair`, text and `--json`. No log,
  ledger line, request or error holds it.

## The pin check

A client must check the server key before it sends the code or any token.

1. Read the SPKI hash and the address from the code.
2. Connect to the address with TLS 1.3. Do not use a certificate chain or a host name check.
3. Take the server's certificate, compute the SHA-256 of its SubjectPublicKeyInfo, and compare it with
   the hash from the code. If they differ, close the connection **before sending anything**.
4. Send `POST /v1/pair`. Keep the token and the hash. Pin the same hash for every later call.

A changed key fails closed: a client that holds a pin refuses a server with another key, and only a
new pairing (a new code) accepts a new key.

**What the client says.** A client that holds a pin and sees another key closes the connection before
it sends anything, and it tells its user exactly `server key changed: re-pair`. It sends no code and
no token. The server cannot see a client's pin, so it can write no line for this: a TLS failure
before HTTP is no request (see "The request ledger"). `scripts/demo/pair-api.sh` is the first client
to follow this rule: its pinned `curl` helper turns `curl`'s exit 90 (`CURLE_SSL_PINNEDPUBKEYNOTMATCH`)
into that text.

To check by hand with `curl`, turn the 64 hex digits of the hash into base64 and pin that. `-k`
skips the certificate chain, which a self-signed certificate cannot pass. The pin still decides:
`curl` closes the connection when the key differs.

```bash
pin="$(echo <the 64 hex digits> | xxd -r -p | base64)"
curl -k --pinnedpubkey "sha256//$pin" \
  -H 'Content-Type: application/json' \
  -d '{"code": "<the code>", "device_name": "my laptop"}' \
  "https://<the address>/v1/pair"
```

`pulse-factory api status` prints the same hash as 64 hex digits. The code carries it as 32 raw
bytes.

## The listener

`pulse-factory controller` serves the API on the controller's own Tokio runtime, beside the socket.

- **Address.** `--api-listen <addr>`, else the environment variable `PF_API_LISTEN`. The systemd unit
  reads `~/.config/pulse-factory/controller.env` as an optional environment file.
- **The key.** On the first start with the API on, the controller makes an ECDSA P-256 key and a
  self-signed certificate with `rcgen` in `<state dir>/api/`: `key.pem` (mode 0600) and `cert.pem`.
  Later starts load them. The certificate names the listen address. The SPKI hash is computed once
  per start.
- **The four states** (`pulse-factory api status`):

  | State | Meaning |
  |---|---|
  | `off` | No address is set. Nothing listens and no key was made. |
  | `waiting` | The address is not on this host yet ("address not available"). The controller tries again every 5 seconds, so it can start before Tailscale does. |
  | `listening` | The listener is bound and serves. |
  | `failed` | An address outside loopback and the tailnet (see "Guards"), another bind error, or the key could not be made. The controller prints the cause, does not try again, and keeps serving the socket. |

## Guards

Five guards sit around the routes. They add no verb, no route and no wire type.

### The peer guard

- **Two counters per peer address**, each over a sliding window of 10 minutes: *failed codes* (a
  `/v1/pair` answer of 400 or 401) and *failed token calls* (a 401 on `/v1/status` or
  `/v1/verbs/<verb>`).
- **The 5th failure of either kind inside the window blocks that peer for 10 minutes, on every
  route.** A success does not clear a count. A 403, 404, 405, 413, 422 or 500 counts toward neither
  counter. The block starts with both counters at zero.
- **A blocked request answers 429** with `Retry-After: <seconds>` and `{"error": "too many failed
  attempts from this address: try again after <UTC time>"}`. The block check runs first, before the
  token, the body or the path, so the request reaches no `ApiHost` method but `audit`. It uses up no
  code and touches no device.
- **Each peer is counted alone.** A second address with a valid code pairs while the first is
  blocked. The peer is the address of the TCP connection, so two clients behind one address share a
  count.
- **The state lives in memory.** A restart clears every count and every block, and voids the live
  code. The map drops entries whose window and block have passed, and it holds at most 4096 peers: it
  drops the oldest entry when it is full.
- The numbers (5 failures, 10 minutes, 10 minutes) are one `GuardConfig` in `pf_api::guard`.
- **An idle token blocks its own address.** The router cannot tell an idle token from a guessed one
  (see "Idle expiry"), so a client that keeps calling with an idle token blocks its own address,
  `/v1/pair` included, for 10 minutes. A client stops at the first 401. A blocked operator waits 10
  minutes or restarts the controller.

### The request ledger

Every API request leaves one line in `<state dir>/api.ledger`, beside `controller.ledger`. The
controller's own ledger holds one line per store admission, keyed by its sequence number. A read must
not cost a store row (ADR-0003, `touch_device`), so the request ledger is a separate file. A
state-changing call has both lines: its admission line in `controller.ledger` and its request line in
`api.ledger`.

```
<UTC time> api route=<route> verb=<verb|-> device=<D-n|-> peer=<ip> status=<code> outcome=<outcome>
```

A refused device verb ends with ` refusal=<word>`, one word of the closed list in "Confirm codes":
`… status=422 outcome=refused refusal=code_used`. No other line has it.

- `route` is `pair`, `status`, `verb`, `stream` or `other`. `device` is the device whose token matched, and
  `-` when none did (a pairing has no token, so it shows `-`). The device ID is the only name of the
  caller.
- `verb` is the name of a verb of the controller's own dispatch table (`runs`, `status`, `halt`,
  `project_add`, `device_pair`, …), and `-` on a route with no verb. Any other path text is written
  as `verb=? verb_len=<n>`, so a token or a code that a client sends as a verb name never reaches the
  file. Every value passes `field()`, so no value can break the line.
- `outcome` is one of: `ok` (200), `replayed` (a 200 whose answer has `"replayed": true`),
  `unauthorized` (401), `forbidden` (403), `refused` (422), `bad_request` (400), `too_large` (413),
  `not_found` (404), `method_not_allowed` (405), `blocked` (429), `failed` (500) and `upgraded` (101). The line of the
  failure that starts a block ends with `blocked_until=<UTC time>`.
- **A blocked peer's 429s are coalesced.** The first 429 of a block writes a line with `outcome=blocked
  count=1`. A later 429 writes a line only when 60 seconds have passed since that peer's last
  `blocked` line, and its `count=<n>` counts every 429 since that line, itself included. A peer that
  floods while blocked cannot fill the disk. The 429s of the last minute of a flood that then stops
  stay uncounted.
- **A stream is one line**, written when the upgrade is answered: `route=stream`, and `outcome=upgraded`
  for the `101` (a refusal has its usual outcome). The close writes no second line. A ninth open
  stream of one device is answered 429, and the router maps every 429 to `outcome=blocked`, so that
  refusal reads `outcome=blocked` in `api.ledger` although it is no guard block.
- **No token, code, confirm code, secret, key or message text** is in any line.
- The file is append-only, with no `fsync` per line, and it is not in the store. A crash can lose the
  last lines. A failed append prints one line on stderr, and the request still gets its answer.
- A TLS failure before HTTP (a pin mismatch, a handshake timeout) is no request and writes no line.
- The file has no rotation and no size cap in this version.

### The bind ranges

The controller checks the API address before it makes or loads a key. Allowed: `127.0.0.0/8`, `::1`,
`100.64.0.0/10` (Tailscale IPv4) and `fd7a:115c:a1e0::/48` (Tailscale IPv6), on any port, `0`
included. An IPv4-mapped IPv6 address counts as its IPv4 form. Any other address (`0.0.0.0`, `::`, a
LAN or public address) leaves the API `failed` with `the API address <ip> is outside loopback and the
tailnet (127.0.0.0/8, ::1, 100.64.0.0/10, fd7a:115c:a1e0::/48)`. No key is made, nothing listens, and
the controller keeps serving the socket. `api status` prints the text, and `device pair` refuses
with it. The check is `pf_api::guard::listen_allowed`.

### Idle expiry

A device is idle when its `last_seen_at`, or its `paired_at` when it has never called, is 30 days or
more in the past. An idle device's call answers 401, with the one `not authorized` text, and does not
touch the device, so the idle state holds. The device stays in `device list`, unrevoked. The operator
pairs it again with a new code (a new device ID) and may revoke the old row. `last_seen_at` lags by
up to a minute (the touch throttle), which does not matter at 30 days. Each idle refusal counts as a
failed token call.

### A changed server key

The server cannot see a client's pin, so this is a client rule: see "The pin check".

## Key loss

The SPKI hash is the identity of the server. If the key files are lost or removed, the next start
makes a new key, and the hash changes. Every paired client then fails closed at the pin check. The
operator then:

1. runs `pulse-factory device revoke <id>` for every old device (`device list` shows them);
2. runs `pulse-factory device pair` and pairs each device again.

There is no key rotation without a re-pair in this version.

## Out of this version

These come in later items and spines: the chat screens (`r1.s3.w5`); the Needs you screens and the
arm through a confirm code (`r1.s4`, later rounds); per-device scopes, and a user-presence check on
the confirm (a person's press, not only the device's); rotation or a size cap of `api.ledger`, `chat.ledger` and `controller.ledger` (the feature map's "controller ledger
rotation"); a per-device rate limit, a ban list that survives a restart and a limit on successful
calls; a `session_items` verb for devices (the stream's `after=0` gives the history); several
servers in one app (the server headers are its groundwork).
