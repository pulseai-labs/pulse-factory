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
  code or a key.
- **The token.** A route that needs a device token reads `Authorization: Bearer <token>`. The token is
  64 lower-case hex digits (32 random bytes). The server keeps only the SHA-256 of the 32 bytes. The
  token appears once, in the answer of `POST /v1/pair`, and nowhere else.
- **Acting as the operator.** A device acts as the operator and has no scope of its own (ADR-0009). A
  call goes through the controller's verb handler, the same code that the Unix socket runs, so each
  admission, refusal and ledger line is the CLI's.

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
"key_spki_sha256": "<64 hex digits>", "device": {…the calling device's record…}}`.

### `POST /v1/verbs/<verb>`

Needs a token. The body is the verb's JSON arguments, and an empty body means `{}`. The 200 answer is
the verb's JSON answer, the same JSON that the CLI prints with `--json`. A state-changing verb takes
a `command_id` argument, as on the CLI: the same ID with the same content returns the first record
with `replayed` true.

- **403** `{"error": "the verb \"<verb>\" is not open to devices"}` for a verb that is not on the
  allowlist, and for an unknown verb. The call does not reach the handler.
- **422** with the handler's message when the controller refuses the command (for example a root that
  does not exist).
- **400** `{"error": "the body is not JSON"}` for a body that is not JSON. **413** for a body over
  1 MiB.

### Status codes of every route

| Code | Meaning |
|---|---|
| 200 | The call worked. |
| 400 | The body is not JSON, or not JSON of the shape the route needs. |
| 401 | No token, a malformed token, an unknown token, a revoked token (on a token route); or a bad code (on `/v1/pair`). |
| 403 | The verb is not on the allowlist. |
| 404 | Any other path, including `/v2/…`. |
| 405 | A known route with the wrong method. |
| 413 | The body is over 1 MiB. |
| 422 | The controller refused the command, or the device name is invalid. |
| 500 | The controller failed inside the call. |

**One 401 for every reason.** On a token route the 401 is `{"error": "not authorized"}` whether the
header is missing, is not `Bearer <64 hex>`, names an unknown token, or names a revoked device. A
caller cannot tell a revoked token from a token that never existed. A route is checked for its token
before the verb or the body is read, so an unpaired caller learns nothing about the verbs.

## The allowlist

A device may call exactly these verbs through `POST /v1/verbs/<verb>`:

| Verb | CLI form |
|---|---|
| `project_add` | `project add` |
| `project_list` | `project list` |
| `project_remove` | `project remove` |

Every other verb is 403: `runs`, `status`, `requests`, `show`, `answer`, `claim`, `clear`, `ack`,
`arm`, `halt`, `hook`, `device_pair`, `device_list`, `device_revoke` and `api_status`. There is no
way for a device to pair another device, revoke one, claim, answer, ack, arm or halt. `halt` and
every merge-ack verb stay off the list: the human confirm of an ack comes with `r1.s4`.

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
  | `failed` | Another bind error, or the key could not be made. The controller prints the cause, does not try again, and keeps serving the socket. |

## Key loss

The SPKI hash is the identity of the server. If the key files are lost or removed, the next start
makes a new key, and the hash changes. Every paired client then fails closed at the pin check. The
operator then:

1. runs `pulse-factory device revoke <id>` for every old device (`device list` shows them);
2. runs `pulse-factory device pair` and pairs each device again.

There is no key rotation without a re-pair in this version.

## Out of this version

These come in later items and spines: a rate limit per peer address, one controller-ledger line for
each API request, a refusal of a `PF_API_LISTEN` address outside loopback and the tailnet, an idle
expiry for devices, and a "server key changed" message in the client (`r1.s2.w5`); a stream over
WebSocket (`r1.s3`); per-device scopes and the merge-ack confirm (`r1.s4`).
