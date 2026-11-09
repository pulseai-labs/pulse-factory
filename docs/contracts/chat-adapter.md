# Chat adapter contract

A chat adapter lets the operator hold a chat with a coding agent. It implements `ChatPort`
(`crates/pf-engine/src/port/chat.rs`). The port is separate from the run seats' agent port
(`docs/contracts/agent-adapter.md`). The decisions are in ADR-0014.

Two adapters implement it: `ClaudeChat` (`crates/pf-adapter-claude`), which drives a managed copy of
the Claude Code CLI, and `FakeChat` (`crates/pf-adapter-fake`), a scripted adapter with no process.

The port is neutral: no runtime name, flag or planning-method word in `pf-engine`. What follows
describes the port first, then what `ClaudeChat` adds.

## The calls

A call blocks until it is done, at most a timeout (`ChatError::Failed` on a timeout). An async caller
runs it with `spawn_blocking`.

| Call | What it does |
|---|---|
| `options()` | The options the adapter declares: `id`, `label`, `values`, `default`, `free`, `live`. |
| `mid_turn()` | What a `send` during a running turn does: `Steer` or `NextTurn`. |
| `open(&ChatOpen)` | Starts a chat process, new or resumed. Returns `ChatOpened`. |
| `send(chat, kind, text)` | Delivers a message. `kind` is `Operator` or `Notice`. |
| `answer(chat, request_id, &ChatAnswer)` | Answers an approval or a question. |
| `interrupt(chat)` | Ends the running turn. The process stays. With no turn open it is `Ok(())` and does nothing. |
| `set_option(chat, id, value)` | Changes a `live` option of this chat. |
| `context_usage(chat)` | `used_tokens` and `max_tokens`. |
| `background_tasks(chat)` | The tasks that run now. It still answers after a stop. |
| `is_alive(chat)` | Is the process alive? An unknown chat is `Ok(false)`. |
| `stop(chat)` | Ends the process and every process in its group. An unknown chat is `Ok(())`. It is safe to repeat. |
| `process_id(chat)` | The leader's operating-system pid, while it runs. ADR-0005's caller check can use it. |

`ChatOpen` holds `cwd` (the session's worktree; a resume uses the same one), `resume`
(`Option<SessionRef>`: `None` starts a new runtime session), `instructions`, `model`, `effort`,
`permission_mode`, `env` (the agent's launch-env map) and `options` (runtime-specific options by id).
`ChatOpened` holds `chat` (a `ChatId`), `session` (the runtime's own session id, which survives the
process), `runtime_version`, `qualified` and `events`.

## Events

The adapter pushes each event on `events`, an unbounded channel, as it happens. Nothing drains on a
tick, and a reader thread never blocks the runtime. **A consumer may drop the receiver mid-stream**,
for example when the controller closes a revoked device's streams. That does not stop the process and
fails no call. Later events are dropped. Only `stop` ends the process, and the adapter must go on
working.

Each event is JSON with a `type` tag in snake case. These are the shapes:

```json
{"type":"turn_started","cause":"operator"}
{"type":"text_delta","text":"Hel"}
{"type":"text","text":"Hello"}
{"type":"tool_started","call_id":"toolu_01","tool":"Bash","input":{"text":"{\"command\":\"ls\"}","bytes":16,"truncated":false}}
{"type":"tool_finished","call_id":"toolu_01","output":{"text":"a.txt","bytes":5,"truncated":false},"is_error":false}
{"type":"approval_request","request_id":"r1","tool":"Bash","input":{"text":"{\"command\":\"touch x\"}","bytes":21,"truncated":false},"reason":"Create a file"}
{"type":"question","request_id":"q1","items":[{"text":"Which option do you prefer?","header":"Preference","options":[{"label":"A","description":"Option A"},{"label":"B","description":"Option B"}],"multi_select":false}]}
{"type":"request_closed","request_id":"r1"}
{"type":"task_started","task":{"id":"bdv4952p9","kind":"local_bash","description":"sleep 5","ambient":false}}
{"type":"task_finished","task_id":"bdv4952p9","end":"completed","summary":"Background command finished"}
{"type":"turn_result","cause":"operator","usage":{"input_tokens":9,"output_tokens":12,"cache_read_tokens":0,"cache_write_tokens":13006,"cost_micros":3465},"is_error":false,"stop":"end_turn","duration_ms":1042,"cache_ttl_seconds":3600}
{"type":"session_moved","session":"6f0c2e9a-1f1c-4b6e-8a3a-0c9d1e2f3a4b"}
{"type":"exited","code":0,"reason":"stopped"}
```

- `cause` of a turn is `operator` or `notice` (from the send it consumed) or `task` (a background task
  the chat started finished, and the runtime started the turn itself).
- `reason` of an approval is the runtime's decision text, else the request's own description.
- `stop` of a result is the result's `stop_reason`; for an error result it is the error subtype
  (`error_during_execution` for an interrupt), because an interrupt's `stop_reason` is `tool_use`.
- `session_moved`: the runtime reports a session id other than the current one. The caller (the
  controller) then updates the session's `runtime_session_ref`.
- `exited` is always the last event. `code` is `null` when a signal ended the process.

### The 16 KB cap

`cap(text)` returns `Capped { text, bytes, truncated }` with at most `CAP_BYTES` (16 KB) bytes
**including its mark**. Text of at most `CAP_BYTES` bytes stays whole with no mark. Longer text is cut
back to a UTF-8 boundary so that the content plus the mark `…[truncated: <bytes> bytes in all]` fits
in `CAP_BYTES`. `bytes` is the original size. The adapter caps a tool's input (its JSON text) and
output once, in the event it pushes, so the stream and the store carry the same text. It keeps an
approval's original input for the answer. The runtime's own transcript keeps the whole text.

### Per-turn usage

`TurnResult.usage` is the turn's own usage, never a running total, and its cost is in `cost_micros`.
`cache_ttl_seconds` is 300 or 3600 when the turn's usage says which cache it wrote, else `null`.

The Claude Code adapter derives it so: `total_cost_usd` and `modelUsage` of a `result` are cumulative for
the process. The adapter keeps the last totals per process as integers (cost in micros,
`round(total_cost_usd × 10⁶)`, and the token sums over `modelUsage`: input, output, cache read and
cache creation). A turn's usage is each total minus the last. A new process, a resumed one included,
starts at zero. A negative difference is a protocol error: it is logged, counts as zero, and becomes the
new baseline. So a session's total is exactly the sum of its turns. Subagent spend lands in the turn
whose `result` first reports it (a known limit).

## The order grammar: `check_order`

`check_order(&[ChatEvent]) -> Result<(), String>` is the event grammar in code. Both adapters' tests
call it on every sequence they produce, and the engine's test calls it on every recording, so
`FakeChat`, `ClaudeChat` and the recordings cannot drift apart. The rules, as the recordings of CLI
2.1.319 settled them:

- each turn starts with `TurnStarted` and ends with one `TurnResult` whose `cause` matches it. No
  turn starts inside an open turn;
- `TextDelta`, `Text`, `ToolStarted`, `ToolFinished`, `ApprovalRequest`, `Question` and
  `RequestClosed` come only inside a turn;
- each `ToolFinished` follows a `ToolStarted` with its `call_id` in the same turn;
- a request id appears once, and `RequestClosed` names an open request, once;
- `TaskStarted` and `TaskFinished` may come inside a turn or between turns. A `TaskFinished` names a
  started task, once;
- a turn with cause `Task` follows a `TaskFinished` that no earlier `Task` turn answered;
- `SessionMoved` comes between turns, or right after a `TurnStarted` and before the turn's other
  events. Never in the middle of a turn;
- `Exited` comes once, last. A turn open at `Exited` has no `TurnResult`, which is allowed.

Two things the grammar does **not** hold, and why:

- **The deltas before a `Text` join to its text.** This is a fact of all 21 recordings, held by a
  test (`decode_fixtures.rs`), not a rule. A steered or multi-block turn makes a rule fragile.
- **The selected `MidTurn` mode** shapes the sequences the grammar sees (under `Steer` a steered send
  adds no `TurnStarted`; under `NextTurn` each send has its own turn), but the grammar accepts both.

## Answers

`ChatAnswer` is `Allow`, `Deny { message }` or `Answers(map)` (question text to the chosen label). It
serialises **adjacently tagged**: `{"type":"allow"}`, `{"type":"deny","value":{"message":"no"}}` and
`{"type":"answers","value":{"Which option do you prefer?":"B"}}`. So a question whose text is the word
`type` cannot collide with the tag.

- `Allow` answers an approval and runs the tool with its original input. `Deny` answers an approval
  and gives the runtime the message, which the model sees as a failed tool call.
- `Answers` answers a question: the runtime allows the call with the original input plus `answers`.
- `Answers` to an approval, `Allow` or `Deny` to a question, or an unknown, answered or closed request
  id is `Refused`, and nothing is written.
- An unanswered request waits with no time limit. A request the runtime withdraws gives
  `RequestClosed`. `AskUserQuestion` reaches the host even under `bypassPermissions`.

## Options and live changes

`options()` is fixed for a qualified version. `ClaudeChat` declares:

| `id` | Values | Default | `live` |
|---|---|---|---|
| `model` | free; a model id of `[A-Za-z0-9._:\[\]-]{1,64}` (the list of the qualification is the `values`) | `claude-opus-5-5` | yes |
| `effort` | `low`, `medium`, `high`, `xhigh`, `max` | `high` | yes |
| `permission_mode` | `acceptEdits`, `auto`, `bypassPermissions`, `manual`, `dontAsk`, `plan` | `bypassPermissions` | yes |
| `artifacts` | `on`, `off` | `on` | no |

`open` reads `model`, `effort` and `permission_mode` from their own fields of `ChatOpen`. It reads
`artifacts` from `ChatOpen.options`. A key of `options` that is one of those three fields, or that no
option declares, is `Refused`. A value that fails its option is `Refused`.

`set_option` changes a `live` option of the running chat. It sends `set_model`, `apply_flag_settings`
with `{"effortLevel": …}`, or `set_permission_mode`, each a control request that waits for its
response. An unknown id, a non-live option, or a value outside `values` when `free` is false is
`Refused`. A change applies to this chat only. A non-live option (`artifacts`) changes at the chat's
next process: the controller stops the process and resumes it (one cache write).

If a future CLI cannot change a setting live, the degrade is: `set_option` succeeds, and at the next
`send` the adapter stops the process and opens it again with `--resume` and the new flags. The
qualification of 2.1.319 selected the live path, and the adapter builds that path only.

### The `artifacts` option

`artifacts` is the Artifact tools of claude.ai: `Artifact`, `ArtifactComments` and `ArtifactData`.
The adapter sets `CLAUDE_CODE_ARTIFACT` to `1` or `0` in the chat process's environment, explicitly
both ways. With `1` the CLI lists the three tools, and with `0` it lists none. They cost about
**12995 tokens in each request** (41001 first-turn request tokens with them, 28006 without; case 17 of
the qualification). OpenBot's comment puts it near 13k, and that holds.

**Q1a, corrected.** An earlier premise said headless Claude Code has no `Artifact` tool. That was wrong:
with `CLAUDE_CODE_ARTIFACT=1`, CLI 2.1.319 lists the tools headless, and a headless publish works
(operator ruling, 2026-11-08). A headless delete is refused ("needs the user's confirmation"), so a
published page needs a person to delete it.

The option is not live. The CLI reads the variable at start, and a live
`apply_flag_settings {"settings":{"enableArtifact":false}}` removes the tools but `true` does not add
them back. If the tools fail to appear or a publish fails for a version, the per-version constant
`ARTIFACTS_SUPPORTED` is `false`, `options()` offers only `off`, and the adapter launches with
`CLAUDE_CODE_ARTIFACT=0` even when an agent stores `on`. It logs one line naming the degrade. The
degrade wins over the stored value.

## Sending during a turn: `MidTurn`

`mid_turn()` names what a `send` during a running turn does. It is a constant of the adapter, and
`FakeChat::new()` takes the same value.

- **`Steer`** (selected for CLI 2.1.319, case 10): the message goes into the running turn. No new
  `TurnStarted` comes, the turn's later events answer it, and its one `TurnResult` (cause unchanged)
  covers both messages.
- **`NextTurn`** (the degrade, for a CLI that queues or refuses a mid-turn message): the message
  becomes the next turn, with its own `TurnStarted` and `TurnResult`.

## Errors

| Error | Meaning |
|---|---|
| `Refused(why)` | The adapter will not do the call: an unqualified version, a bad option value, a reserved env name, an answer to an unknown request. |
| `Failed(why)` | The adapter tried and failed, or a call timed out. |
| `ResumeFailed(why)` | `open` with `resume` cannot resume that session. An adapter never answers this by starting a fresh context. |
| `RuntimeNotReady(why)` | The runtime's own binary is missing or fails its check. The text names the fix. |

`Display` text names the kind: "refused by the adapter", "failed", "resume failed", "runtime not
ready".

### `ResumeFailed`

For a resume, `ClaudeChat` looks for `<config_dir>/projects/*/<id>.jsonl` before it spawns anything.
None found is `ResumeFailed` and no process starts. A process that exits before `initialize` is
answered, or that writes an error `result` first (case 15: "No conversation found with session ID"), is
`ResumeFailed` too. The controller shows "context lost" for a session that has had a message, and the
operator chooses to end the session. The controller passes `resume` only for such a session: a session
with no message has no transcript, so it opens with no `resume` and the adapter returns a new ID
(operator ruling on PR #41, class C1).

### `RuntimeNotReady`

A missing managed copy, a copy whose size or SHA-256 differs from the pin, or a platform with no pin
is `RuntimeNotReady`, before any process starts. The text names the path and the fix: run Pulse
Factory's install step for the chat runtime.

## The Claude Code adapter

### The managed copy and the override

The chat does not run the user's `claude`: it updates itself. Each Pulse Factory release names one
qualified CLI version, and `pinned.rs` holds its SHA-256 and size per platform (`linux-x64`,
`darwin-arm64`), read from the npm manifest of the Agent SDK version that carries it.

- With no override the binary is `<share_dir>/claude/<version>/claude`, never `~/.local/share/claude`
  and never a `claude` found on `PATH`.
- `open` hashes the copy in chunks (it never reads about 340 MB into memory) and verifies once per
  (size, modification time) for the adapter's life. A change verifies again. The copy's `--version`
  must print the pinned version, else `Refused`.
- The first `init` must report the same `claude_code_version` as `--version`, or the adapter stops
  the process.
- `claude_bin_override` runs any other binary with no hash check. The adapter still reads its
  `--version` and does not refuse it: `ChatOpened.qualified` is `false` and `runtime_version` is what
  it printed, so the controller can show the chat as unqualified. An override whose version cannot
  be read is `Failed`. With the managed copy, `qualified` is `true`.
- The adapter never fetches. The install step (w3) fetches the copy from the npm registry, checks its
  SHA-256 against the pin and places it. Pulse Factory never redistributes the binary.

### The command line and the environment

The command line is the same for every process of one session, and a pure function of `ChatOpen` and
the session id:

```text
claude -p --input-format stream-json --output-format stream-json --verbose
       --include-partial-messages --permission-prompt-tool stdio
       --allow-dangerously-skip-permissions
       --permission-mode <permission_mode> --model <model> --effort <effort>
       --disallowedTools ScheduleWakeup,CronCreate,CronDelete,CronList,RemoteTrigger
       [--append-system-prompt <instructions>]       (left out when empty)
       (--session-id <uuid> | --resume <uuid>)
```

A new session gets a fresh version-4 UUID. The flags never include `--bare`, `--no-session-persistence`,
`--setting-sources` or `--max-turns`, and the environment never sets `CLAUDE_CONFIG_DIR`. The
program runs with no shell, and the instructions are one argument.

The child's environment is an allowlist:

- from the inherited environment, when set: `HOME`, `USER`, `LOGNAME`, `PATH`, `SHELL`, `LANG`,
  `LANGUAGE`, every `LC_*`, `TZ`, `TMPDIR`, `TERM`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`,
  `XDG_CACHE_HOME`, `XDG_STATE_HOME`, `XDG_RUNTIME_DIR` and `SSH_AUTH_SOCK`. Case 3 of the
  qualification showed no other name is needed;
- the agent's `env` map, each name matching `[A-Z0-9_]+`. These are refused, and the refusal names
  the variable: `ANTHROPIC_*`, `CLAUDE_CONFIG_DIR`, `CLAUDECODE`, `CLAUDE_CODE_*`, `PF_*`, `ORCA_*`,
  `HERDR_*`, and any name the adapter sets;
- set by the adapter: `DISABLE_AUTOUPDATER=1` and `CLAUDE_CODE_ARTIFACT` (`1` or `0`). These are the
  only `CLAUDE_CODE_*` names the child gets. An inherited `CLAUDE_CODE_ARTIFACT` or
  `CLAUDE_CODE_ENTRYPOINT` is dropped, and the adapter never sets `CLAUDE_CODE_ENTRYPOINT`.

So `ANTHROPIC_API_KEY`, `PF_SEAT_TOKEN`, the Orca and herdr hook tokens and a parent Claude Code
session's variables never reach the chat. An `init` whose `apiKeySource` is not `none` ends the
process, and so does one whose tool list holds a timer tool.

### The control protocol subset

`initialize` is sent first, with no hooks, no MCP servers and no `perTaskStopAffordance`. It must be
answered within `init_seconds` (60), else the adapter kills the group and returns `Failed` (or
`ResumeFailed` for a resume). Control requests wait `control_seconds` (30) for their response. The
adapter writes `set_model`, `apply_flag_settings`, `set_permission_mode`, `get_context_usage`
(`detail: "summary"`), `interrupt` and its answers to `can_use_tool`. It **never** writes the
`background_tasks` control request: it moves foreground tasks to the background and reports nothing.

Inbound control requests the adapter does not serve (`hook_callback`, `mcp_message`, `elicitation`,
`request_user_dialog`, any unknown subtype) get an error response at once, and a log line. Messages
of a subagent (a `parent_tool_use_id`) give no text or tool events, and thinking blocks give none.

### Stop, exit and the interrupt's known limit

`stop` closes stdin and waits up to 5 s for the process to end. Then it sends `TERM` to the group,
waits 2 s, sends `KILL` to the group, and waits for the leader. The reader sends `Exited` once, when
stdout ends and the leader is reaped, with the exit code and a redacted one-line reason of at most
500 characters. A child that leaves the group on its own escapes `stop`: a known limit. A background
`Bash` of the CLI runs in its own session, and `TERM` to the group still ended it in the qualification.

An interrupt stops the running turn **and its background agents** (the CLI's default without
`perTaskStopAffordance`, accepted for `r1.s3`). A background `Bash` survived an interrupt in case 9. A
per-task stop is a later feature.

### The lifetime split

The adapter has no timer and decides no release. It offers `background_tasks`, `is_alive`, `stop` and
the events. The 30-minute idle release, the rule that a process with a running background task is
not released, the cap on live chat processes and the restart rule belong to the controller. A turn
starts only on `send`, or when a background task the chat started finishes (the CLI starts that turn
itself; the adapter sends no notice).

## Test paths

- **`FakeChat`** has no process and no network. It follows the port exactly: the same event order, the
  same usage shape, the same cap and the same refusals. Every behaviour is driven by the message text
  (`fake:tool`, `fake:big`, `fake:approval`, `fake:question`, `fake:task <ms>`, `fake:slow <ms>`,
  `fake:fail`, `fake:exit`). It is for the controller's and the API's tests.
- **`pf-claude-replay`** (a binary of `pf-adapter-claude`) stands in for `claude` and plays a recorded
  fixture back. The caller's `claude_bin_override` points at a two-line wrapper script that runs
  `exec <path>/pf-claude-replay --fixture <file> "$@"`, because the environment allowlist passes no
  Pulse Factory variable to the child. Such a chat opens with `qualified` `false`. It prints
  `<cli_version> (Claude Code)` for `--version`, checks the fixture's header and `body_sha256` (exit 2
  on a mismatch), plays the body in order, keeps the recorded gaps with `--pace recorded`, and starts
  no other program.
- **Fixtures** live in `crates/pf-adapter-claude/fixtures/`. The recorder writes them and nobody else:
  line 1 is a header (`cli_version`, `recorded`, `recorder`, `case`, `body_sha256`), and each later line
  is `{"to":"cli"|"host","t_ms":…,"msg":…}`. They are scrubbed before they are hashed: home and
  temporary paths, account blocks, e-mail addresses, tokens and artifact URLs are replaced or removed,
  and the lists that name the operator's own setup are empty.
- **The qualification recorder** (`tests/real_probe_chat.rs`) runs only with `PF_REAL_CLAUDE=1`, from
  `#[ignore]`d tests, against a counted budget of 30 real processes in
  `~/.local/state/pulse-factory/claude-probe-processes`. A plain `cargo test` and CI never start the
  real CLI.

## Before you ship a chat adapter

1. List each call of the port and, for each, how the runtime does it. Where it cannot, say what the
   adapter refuses or how it degrades.
2. Qualify a runtime version by a real run, and record the results in an ADR (see ADR-0014's
   "Verified claims").
3. Record fixtures from that run. Never write them by hand or from a type file.
4. Run real processes only from marked, ignored, counted tests. A plain `cargo test` and CI never start
   one.
5. Make the version pin and the binary check refuse before a process starts.
6. Test the adapter against its own recordings (a replay), with `check_order` on every sequence.

## Bump the CLI pin

A release step. Do it for every new CLI version of the Claude Code chat adapter.

1. Find the SDK package version on the npm registry whose `manifest.json` names the new CLI version.
2. Place a managed copy of the new version by hand, and check its SHA-256 against the manifest.
3. Qualify the version: run the recorder (Approach 1 of the item that wrote this adapter) against the
   managed copy, with a fresh budget agreed with the planner.
4. Record the fixtures again.
5. Update `pinned.rs`: the CLI version, the SDK package version, and the per-platform SHA-256 and size
   read from the manifest.
6. Update `MID_TURN` and the other selected paths in `version.rs` to the new results.
7. Amend ADR-0014's verified claims.
8. Ship.
