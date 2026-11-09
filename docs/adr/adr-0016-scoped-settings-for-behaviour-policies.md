# ADR-0016 — Scoped settings for behaviour policies

- **Status:** Proposed, 2026-11-09, at `r1.s4.w4`. It becomes Accepted at the `r1.s4` close.
- **Date:** 2026-11-09

## Context
- Release 1's third exit criterion: the operator starts the spine from the Orchestrator chat. A chat
  that runs unattended needs two policies the operator can change with no restart: how many turns it
  may start on its own, and which shell commands its runtime must refuse.
- The grill of spine `r1.s4` (Q4, Q5 and Q5a) and its planning audit (critic C3, C6 to C10) fixed the
  rules below. Q5a asked for one layer, not a constant per policy and not a column per policy: the
  operator sets a value per server, per project, per agent, or per agent in one project, and the most
  specific scope wins.
- Q5a's operator context names what comes later: project agents and global agents, and per-project
  overrides of an agent's model, effort and runtime. The layer must carry those without a new table,
  and it must not name a repo layout.
- ADR-0001 puts the verbs in the controller. ADR-0002 keeps the engine free of any setting. ADR-0003
  and ADR-0007 give the store a forward-only migration. ADR-0005 and ADR-0009 say a device acts as the
  operator and a seat or a chat process does not. ADR-0012 puts the verbs on the device allowlist.

## Decision

**The layer.** One table, `settings` (migration `0007_settings.sql`), holds one value of one key at
one scope. A row is `(project_id, agent_id, key, value, set_by, set_at)`. `project_id` is a `P-<n>`
and `agent_id` is an `agents.id`. `''` in a scope column means "every", so both empty is the server
scope. The four scopes are the server default, one project, one agent, and one agent in one project.
A project's value is the operator's choice for that project and every agent in it. An agent's value
is the agent's default everywhere. The pair overrides both.

**The order.** A context reads the scopes most specific first, in the words
"project and agent, project, agent, server, then the built-in". That is `(project, agent)`, then `(project, every)`, then `(every, agent)`,
then the server row, then the binary's own value. A context with no project skips the two project
scopes, and one with no agent skips the two agent scopes. Project sits above agent because the
operator's choice for a project must hold for each agent in it, and an agent's own default must not
undo it. A later override of an agent's model, effort or runtime for one project is a key at
`(project, agent)`, so it needs no new rule.

**`''`, not NULL.** SQLite treats NULLs as distinct in a primary key, so two server rows of one key
could coexist. The port maps `''` to `None` both ways, and no `''` reaches the wire. Triggers hold the
rest: a row names a live project and a known agent at insert, and its scope and key never change. The
table has no `CHECK` on the key list, so a later key needs no rebuild.

**The resolver.** The controller resolves a key for a context on every read. There is no cache: a
change applies at the next read. A stored value that this binary cannot read (a hand edit) is skipped
with one stderr line, and the next scope decides. A stored key that this binary does not know is
ignored, so a rollback to the same schema version (ADR-0007) stays safe.

**Merge modes.** Each key in the registry declares a `merge` mode. Both keys are `override` now: the
first row on the chain wins whole, and every other stored row on the chain is masked. `settings get`
reports the resolved value, the scope it came from, and each masked value with its scope, so the
operator sees what a more specific row hides. `union` (for lists) and `min` (for numbers) are reserved
names. A later key that needs one adds it to the registry and the resolver, with no migration,
because the store keeps every scope's row as it is.

**The registry.** The controller decides which keys exist. The engine never reads a setting, and the
store port carries opaque keys and JSON values (ADR-0002). This release has two keys:
- `chat.self_turn_cap`: a JSON integer, 0 to 100, `0` is off, built-in `3`. It caps the turns a chat
  starts on its own (a background task finished) since the operator's last message. A cap of N lets N
  such turns run whole, and the controller cuts the next one as it starts (ADR-0015's amendment). A
  change applies at the next self-started turn of a live process.
- `chat.deny_commands`: a JSON array of 0 to 32 shell command patterns, no duplicates, each passing the
  chat port's pattern check (ADR-0014's amendment). The built-in is the fifteen patterns of that
  amendment. A change applies at the chat's next launch.

An unknown key, or a value that fails its key's rule, is a refused command that names the rule.

**Writes.** A row changes only through an admitted command, `setting_set` or `setting_unset`, with a
command ID (ADR-0003). Each is one admission under `@controller` and one `controller.ledger` line that
names the key, the scope, the new value and who set it (`by=cli` or `by=D-<n>`). Who called is not
content, so the same command ID and content from the CLI and from a device replays. The store port has
no settings write outside these two effects.

**Callers.** The CLI operator and a paired device may write (ADR-0009). A chat process or a seat may
not: `settings_set` and `settings_unset` run the controller's caller check (ADR-0005). The CLI does not
pre-refuse them under `PF_SEAT_TOKEN`, so the refusal reaches the controller, which audits it. `settings
get` is a read and is open to every caller.

**The audit of a refused caller.** A refused write is answered at once. Its durable record is bounded,
so a refused caller gets no unbounded durable writes (ruling 8). Refusals coalesce per caller key
(`chat:<S-n>`, `seat:<pid>`, `seat-token` or `unknown`), bucket (one of the two keys, else `other`) and
one-minute window. The window's first refusal commits one refused admission under a command ID that the
controller makes, `caller-refused-<32 hex>`, and writes its line at once. The close, at the tick after
the window or at the caller's next refusal after it, commits one more with the count. So a caller gets
at most two lines per bucket per minute, six across the three buckets. The caller's own command ID is
never used up, and a guessed one cannot poison a later operator command.

**The guarantee's limit.** The caller check stops a chat that does not evade it. A same-user process
can evade it: `env -u PF_SEAT_TOKEN`, a double fork that leaves the ancestor chain, a rewrite of its own
`/proc` environment, or a direct write to the store file, which the operator's user owns. So "a chat
cannot raise its own cap" holds only against a chat that does not evade the check. This is a known
limit. Its owner is the Release 2 authority-rules spine.

**The authority floors are not settings.** The confirm code, the caller checks and the claim are not
keys and never become keys (Q5a). A policy that the operator can set per project must not be able to
turn off the control that stops a chat from setting it.

**How later policies extend it.** A per-project override of an agent's model, effort or runtime, and
the settings of project-scoped agents, are new keys on the same scopes, not a new layer. A
project-scoped agent is an `agents` row, and its settings use the scopes above.

## Rejected alternatives
- **Per-agent columns.** They give no per-project difference, and each new policy needs a migration.
- **A config file per project.** It needs an edit outside the app and a restart for each change.
- **Agent above project.** An agent's default would undo the operator's choice for a project.
- **A cache in the controller.** A change would then need an invalidation, and a stale value is a worse
  fault than one read of a small table.
- **NULL for "every".** NULLs are distinct in a primary key.
- **A union or a minimum as the first merge mode.** Neither key needs one. The name is reserved.
- **One refusal line for each refused write.** A chat that loops on `settings set` would write without
  bound. The coalesced audit costs the record of the exact count in the window's middle.

## Consequences
- A policy the operator can change with no restart, per scope, with an audit line for each change.
- The controller's ledger gains two commands, `setting_set` and `setting_unset`. `settings get` costs no
  admission.
- A removed project's rows stay in the table and never apply again, because a re-added root gets a new
  `P-<n>` (ADR-0015).
- The environment's chat settings (`PF_CHAT_MAX_LIVE` and the rest) stay outside the layer. They change
  with a restart. "The chat settings" are the environment, and "settings" are these scoped values.
- A crash or a restart loses the count of an open refusal window, which is at most one minute.
- The deny list is a speed bump, not a boundary (ADR-0014's amendment). The cap is a control on a
  chat that does not evade the caller check.
- **Revisit when** project-scoped agents or per-project overrides of an agent's model, effort or runtime
  are planned, or the Release 2 authority-rules spine plans its authority model.

### Unverified claims
- That a deny rule's mid-command `*` matches as the amendment of ADR-0014 assumes, for every pattern.
  Case 19 showed it for one command (`gh -R <repo> pr merge 1`). The three other required checks were
  not shown, because the host's own hook answered first or the model made no `Bash` call.
- That no caller can evade the caller check in a way this record does not list. The check reads
  `/proc`, and any way to change what it reads is outside its guarantee.
- That one minute is the right size for the refusal window. It is a `Config` value.
