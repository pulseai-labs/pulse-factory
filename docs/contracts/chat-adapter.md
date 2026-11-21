# Chat adapter contract

A chat adapter lets the operator hold a chat with a coding agent. It implements `ChatPort`
(`crates/pf-engine/src/port/chat.rs`). The port is separate from the run seats' agent port
(`docs/contracts/agent-adapter.md`). The decisions are in ADR-0014.

Two adapters implement it: `ClaudeChat` (`crates/pf-adapter-claude`), which drives a managed copy of
the Claude Code CLI, and `FakeChat` (`crates/pf-adapter-fake`), a scripted adapter with no process.
`ClaudeChat` runs the CLI in a **session host** (`docs/contracts/session-host.md`), outside the
controller, so a controller restart keeps the process: see "The chat in a session host".

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
| `set_hosts(hosts)` | The controller hands over its host service (`Arc<dyn SessionHosts>`), once, before any chat opens or rebinds. An adapter that runs no host ignores it (default: nothing). |
| `rebind(unit, &ChatResumePoint)` | Attach to the host of `unit` again after a controller restart and read on from the controller's position. Returns `ChatRebind`: `Survived(ChatOpened)`, `Lost { exit }` or `Unreachable { why }`. The default is `Lost` with the cause "no session host". |
| `ack(chat, to)` | Say the output below cursor `to` is stored. The adapter passes it to the host, so the host may drop those bytes. After the exit's cursor it also sends `exit_ack`, and the host ends at once. The default does nothing. |
| `detach(chat)` | Drop the connection to the chat and stop nothing: the controller stops and the process lives on. The default does nothing. |

`ChatOpen` holds `cwd` (the session's worktree; a resume uses the same one), `resume`
(`Option<SessionRef>`: `None` starts a new runtime session), `instructions`, `model`, `effort`,
`permission_mode`, `env` (the agent's launch-env map), `options` (runtime-specific options by id) and
`deny_commands` (`Vec<String>`: shell command patterns the runtime must refuse to run, in a
runtime-neutral form; see "The deny list"). It also holds the ids a launch record names:
`chat_session` (the store's `S-<n>`, the host's session key), `project`, `agent_id`, `worktree_id`,
`deny_from` (the scope of the deny list); `secret_env` (the `PF_AGENT_ENV_<AGENT>` pairs, apart from
`env`, so a launch record keeps `sha256:<hex>` of each value and never the value); and
`recorded_binary` (`Option<RecordedBinary { path, sha256, version }>`: the runtime copy a resume must
start, from the launch record). `ChatOpened` holds `chat` (a `ChatId`), `session` (the runtime's own
session id, which survives the process), `runtime_version`, `qualified`, `events` and `host`
(`Option<ChatHost { unit, incarnation }>`, `None` for an adapter that runs no host).

## Events

The adapter pushes each event on `events`, an unbounded channel, as it happens. The channel carries
`ChatDelivery { event, at, checkpoint }`: the event, the host's output cursor after the line that
gave it (`at`, `None` for an adapter with no host) and, on the last event of a line after which the
adapter is quiet (no turn and no request open), the adapter's own state (`checkpoint`, opaque to the
caller). The caller stores the event with its `at`, and acknowledges a checkpoint's `at` with `ack`
after the store committed it. A line that gives n events gives its last event `at` = the line's end
and event i (counting from 0, i below n-1) `at` = the line's end minus (n-1-i), so every event of a
line has its own position and only the last may carry the checkpoint. Nothing drains on a
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
{"type":"output_lost","from":65536,"to":131072}
{"type":"exited","code":0,"reason":"stopped"}
```

- `cause` of a turn is `operator` or `notice` (from the send it consumed) or `task` (a background task
  the chat started finished, and the runtime started the turn itself). A turn that a message started
  is never `task`: when the adapter no longer knows the send after a controller restart (the
  checkpoint holds only the sends open at a quiet point), the cause is `operator`.
- `reason` of an approval is the runtime's decision text, else the request's own description.
- `stop` of a result is the result's `stop_reason`; for an error result it is the error subtype
  (`error_during_execution` for an interrupt), because an interrupt's `stop_reason` is `tool_use`.
- `session_moved`: the runtime reports a session id other than the current one. The caller (the
  controller) then updates the session's `runtime_session_ref`.
- `output_lost`: the host dropped the output from cursor `from` to `to` before the controller read
  it (the host's replay buffer overflowed while the controller was down). The event covers the part
  above the cursor the store consumed. It may come anywhere before `exited`. The first message after
  a gap that belongs in a turn opens one with cause `operator`, so its `turn_result` is still stored.
  The controller adds a `notice` item with the code `output_lost`.
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
cache creation). A turn's usage is each total minus the last. The first `result` of a process, in a turn
or not, sets the baseline: its totals minus its `model_usage_since_last_result` (the turn's own usage, per
model, with `costUSD`), each class on its own and never below zero; the cost is the dollar difference,
rounded once. A new process carries nothing over, so it starts
at zero. A resumed process carries the session's totals over into its first `result` (CLI 2.1.319), so
it starts from them. A first `result` without that field starts at zero. A negative difference is a
protocol error: it is logged, counts as zero, and becomes the new baseline. So a session's total is
exactly the sum of its turns, across a resume too. Subagent spend lands in the turn whose `result` first
reports it (a known limit).

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
- `OutputLost` may come anywhere before `Exited`, inside a turn or between turns;
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
       --disallowedTools <timer tools>,Bash(<c>),Bash(<c> *),…     (one argument; see "The deny list")
       [--append-system-prompt <instructions>]       (left out when empty)
       (--session-id <uuid> | --resume <uuid>)
```

`<timer tools>` is `ScheduleWakeup,CronCreate,CronDelete,CronList,RemoteTrigger`. With an empty
`deny_commands` the argument is those five and nothing else, as before `r1.s4.w4`.

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

### The deny list

`ChatOpen.deny_commands` is the list of shell commands that the runtime must refuse to run. The
controller resolves it at each open, new or resumed, from the setting `chat.deny_commands` (`cli.md`,
ADR-0016), so a change reaches a process only at its next launch. The adapter turns each entry into
Claude Code deny rules, as `Bash(...)` entries of the one `--disallowedTools` argument.

**The pattern grammar** is one rule for the controller and both adapters: `pf_engine::port::chat::
check_deny_command` for an entry and `check_deny_commands` for a list.

- A pattern is words, and `*` stands for any text.
- It is 1 to 100 characters of `A-Z a-z 0-9`, space, `.`, `_`, `/`, `:`, `=`, `@`, `+`, `-` and `*`.
- Its first character is a letter or a digit. It has no trailing space and no two spaces in a row.
- `*` is a whole word (`git push * --force`) or the last character of a word (`--force*`, `--repo=*`,
  `+*`). It is never inside a word and never `**`.
- It holds **no `:*` anywhere**. Claude Code reads a rule that ends `:*` as its legacy prefix form,
  so `Bash(git push * :*)` would deny every push.
- So no `(`, `)`, `,`, quote, `;`, `&`, `|` or control character can reach a rule.
- A list holds at most 32 entries and no duplicate.

`validate` refuses a spec whose list fails the check, and names the entry. `FakeChat::open` refuses
it the same way and otherwise ignores the list.

**The rule form.** `launch::disallowed_tools(deny)` is the five timer tools, then, for each entry `c`
in order, `Bash(c)` and `Bash(c *)`, or `Bash(c)` alone when `c` ends in `*`, all joined by `,`.
`Bash(c)` matches the bare command, and `Bash(c *)` matches it with arguments (the pinned help's
`Bash(git *)` form). The fifteen built-in patterns make 25 rules, so the argument holds 30.

**The built-in patterns,** in order: `git push -f*`, `git push * -f*`, `git push --force*`,
`git push * --force*`, `git push * +*`, `git push --delete`, `git push * --delete`, `git push -d`,
`git push * -d`, `git branch -D`, `git branch * -D`, `gh pr merge`, `gh -R * pr merge`,
`gh --repo * pr merge` and `gh --repo=* pr merge`. They cover the common spellings of a forced push,
a remote branch delete, a local branch delete and `gh pr merge`, with a flag before or after the
remote or branch.

**A speed bump, not a boundary.** A deny rule matches the command text the model writes. It stops an
accidental merge, not a determined one. The residue gets past it: an absolute path
(`/usr/bin/git push -f`), `gh api …/merge`, a script, an alias, a combined short flag
(`git push -uf`), the colon refspec (`git push origin :branch`, whose only rule would end `:*`), an
environment prefix (`GIT_DIR=… git push -f`) and another tool. The chat keeps `HOME`, so it reaches
the operator's `gh` login and git credentials, and a merge it makes skips the ack, its confirm code
and its ledger line. The owner of that limit is the Release 2 authority-rules spine. A PATH shim
was considered and not taken: an absolute path bypasses it, so it is no stronger than the rules.

**Qualified by case 19** (`real_probe_chat.rs`, CLI 2.1.319, `bypassPermissions`, one probe process).
The 30-rule argument parsed, since the CLI ran. One rule held under `bypassPermissions`, in the
rule's own text (`Permission to use Bash with command … has been denied.`): `gh -R <repo> pr merge 1`
met `Bash(gh -R * pr merge *)`, so a `*` in the middle of a command matches in 2.1.319. The recording
does not show the rest. The host's own `PreToolUse` hook refused `git branch -D` and `git push -f`
before a rule applied, and the model made no `Bash` call for `git push origin main --force` and for
`true && git branch -D …`. Those four are unverified; ADR-0014's amendment records them.

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

`stop` sends `Terminate` to the child's group through the host and waits up to 5 s for the exit.
Then it stops the host (the host's own `SIGTERM`, 5 s, `SIGKILL`), and the host service ends the
registry row. The reader sends `Exited` once, when the host reports the child's exit, with the exit
code and a one-line reason: the violation that stopped the chat, `stopped`, or "process exited (code
<n>)" / "process exited (signal <n>)". The host keeps the child's error output (the unit's journal
under systemd), so the reason holds no stderr tail. A child that leaves the group on its own escapes
`stop`: a known limit. A background `Bash` of the CLI runs in its own session, and `TERM` to the
group still ended it in the qualification.

An interrupt stops the running turn **and its background agents** (the CLI's default without
`perTaskStopAffordance`, accepted for `r1.s3`). A background `Bash` survived an interrupt in case 9. A
per-task stop is a later feature.

### The lifetime split

The adapter has no timer and decides no release. It offers `background_tasks`, `is_alive`, `stop` and
the events. The 30-minute idle release, the rule that a process with a running background task is
not released, the cap on live chat processes and the restart rule belong to the controller. A turn
starts only on `send`, or when a background task the chat started finishes (the CLI starts that turn
itself; the adapter sends no notice).

### The chat in a session host

`ClaudeChat::open` validates, builds the environment and resolves the binary, then calls
`SessionHosts::launch` with kind `chat`, key `chat_session`, `argv` (the binary, and the flags of the
launch with `--session-id <uuid>` or `--resume <id>`), `cwd`, the allowlisted `env`, `secrets` and
transport `Pipes`. The launch record body holds:

- `runtime`: `{id, version, qualified, binary_path, binary_sha256}`. `store check-upgrade` reports
  `binary_path` as a CLI copy, so the install step keeps it while the host lives. The override binary
  is hashed too;
- `settings`: `model`, `effort`, `permission_mode`, `options`, `instructions`, `launch_env` (the
  agent's launch environment, never a secret) and `chat.deny_commands` with its scope. The service adds
  the four `session.*` keys. A `session set` writes the next revision of the record with the new
  values; `argv` stays as the host ran it;
- `run_binding`: `{chat_session, project_id, agent_id, worktree_id}`;
- `resume`: `{"flag": "--resume", "id_from": "session_row"}`. A resume takes the session ID from the
  session row, never from an old `argv`. A `SessionMoved` writes the row and no revision.

The reader thread reads host events, splits the bytes into lines and feeds the decoder. Control
writes go through the host's input. `process_id` is the child's PID from the host's `hello`, so the
caller check of ADR-0005 still finds the chat. A violation sends `Kill`.

**The checkpoint** is the decoder's state at a quiet point: the process totals and whether a
`result` came (the per-turn cost baseline), the background tasks and the runtime's session ID. It is
small and opaque to the controller.

**Commit, then acknowledge.** The controller writes each item with its `at` in one transaction and
calls `ack(chat, at)` only for a delivery that carried a checkpoint, so the bytes of a running turn
stay in the host until its `turn_result` is stored. A `TurnStarted` gives no item: the controller
writes its `at` alone, with the time the turn started (`turn_since` in the session's cursor), and
acknowledges nothing. Each later position of the turn names that time again, and a checkpoint or
the exit clears it. After `Exited` it acknowledges the exit's cursor
and the adapter sends `exit_ack`. The exit comes at the cursor of the last output line, so an
exit at `committed` is a first exit unless the session's cursor holds `exited`, which the write of the
exit's own notice sets in the same transaction; only an exit read again after that stores nothing.

**`rebind`.** After a controller restart `rebind(unit, point)` reads the launched record through
`SessionHosts`, builds the decoder from `point.checkpoint`, seeds the open requests from
`point.pending` (ID, kind and the stored input text, `None` when it was cut), and reads from
`point.acked`. A line that ends at or below `point.committed` is read again: the decoder updates its
state and the adapter sends the same events, and sends no control response for an inbound request
(the controller already answered or closed it). A host whose child ended while the controller was
down is survived too: the adapter reads what the child left, then sends `Exited`. The controller
keeps the process's start (the start of the host's registry row) and the start of the turn that ran
at the restart (the cursor's `turn_since`): a rebind is no new process and no new turn. A cursor
written before `turn_since` existed holds none, so a turn that ran across that upgrade counts from
the rebind. When the host dropped the result of the turn that ran at the restart (`OutputLost`), the
controller cannot see that the turn ended: the start stays until the next result or the exit, and a
later turn in the lines that are left shows it until its own result. A chat with no host is never
rebound: its times come from the controller's clock at its open.

**Output lost.** A host `Gap` becomes `OutputLost` for the part above `committed`, and the adapter
skips to the next newline. A request whose line was lost can still be answered from `point.pending`; a
request whose stored input was cut can be denied, and an allow is refused with a reason that says so.

**G8.** A resume carries `recorded_binary`. The adapter checks that the file exists and has the
recorded SHA-256. If not, `open` fails with `RuntimeNotReady` ("the CLI copy this chat was launched
with is gone or changed: <path> (sha256 <hex>); end the session and start a new one, or put that copy
back") and starts no other binary. It is not "context lost".

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
- **Fixtures** live in `crates/pf-adapter-claude/fixtures/`. The recorder writes them and nobody else
  (one exception, below):
  line 1 is a header (`cli_version`, `recorded`, `recorder`, `case`, `body_sha256`), and each later line
  is `{"to":"cli"|"host","t_ms":…,"msg":…}`. They are scrubbed before they are hashed: home and
  temporary paths, account blocks, e-mail addresses, tokens and artifact URLs are replaced or removed,
  and the lists that name the operator's own setup are empty.
- **The qualification recorder** (`tests/real_probe_chat.rs`) runs only with `PF_REAL_CLAUDE=1`, from
  `#[ignore]`d tests, against a counted budget of 31 real processes (30, and one more for case 20 alone, by the
  operator's ruling of 2026-11-10) in `~/.local/state/pulse-factory/claude-probe-processes`. A plain `cargo test` and CI never start the
  real CLI. Case 19 (`case_19_deny_rules`) qualifies the deny rules of "The deny list" and records
  `fixtures/deny_rules.jsonl`: an operator turn and two `Task` turns, then the deny-rule prompts.
- **Case 20** (`case_20_start_proposal`, `r1.s4.w2`) is the 31st and last process of the budget. It
  asks whether the Orchestrator, with the persona of ADR-0015 and the skill under a temporary share
  directory, starts a spine by running `pulse-factory propose` and never `pulse-factory arm`. A stub
  `pulse-factory` first on `PATH` records its argv and starts nothing. One function in
  `tests/common/propose_rule.rs` judges a call, for the stub's records and for the recorded `Bash`
  command alike: a call is a proposal of `s1` when its program is `pulse-factory`, its first argument
  is `propose` and `s1` is a positional argument (a flag and its value are not). Case 20 ran twice.
  The first process (the 30th) made no recording: the scrub guard refused the skill's own text,
  which holds the elided path `/home/…`. The operator ruled (2026-11-10) that the scrubber gets a rule
  for that text, that the guard stays as it is, and that the limit rises to 31 for this case alone.
  The second process (the 31st, 2026-11-10, `PF_REAL_CLAUDE=1`, AC-15; the counter reads 31) wrote
  `fixtures/start_proposal.jsonl`, a real recording of 152 body lines. Its `FINDING` lines:
  - `start_proposal.tools_used: Read,Bash`: the model read the skill, then ran one shell call.
  - `start_proposal.read_the_skill: true`: it read `<TMP>/share/skill/SKILL.md` first, from the path
    the persona names.
  - `start_proposal.tool_use: "Bash"` was `pulse-factory propose s1 --workflow paper-spine-d20 --ledger
    <TMP>/ledger.md --param paper_env=<TMP>/paper --param base_branch=main 2>&1; echo "exit=$?"`. It
    exited 0 and the stub answered with the card line. The model never ran `arm`.
  - `start_proposal.stub.calls: 1`, `start_proposal.stub.proposals_of_s1: 1`,
    `start_proposal.stub.arms: 0`: the stub's own record agrees with the recorded stream.
  - The model spelled the paper env as `--param paper_env=…`, not as `--paper …`: `skill/SKILL.md`
    does not name `--paper`. The matching rule accepts both, because it needs only `propose` and the
    positional `s1`. The demos do not read the recorded command: `start-env.sh` runs `propose` with
    its own arguments.
  - The turn ended with 2 tool results and one `result` message. No operator hook blocked the stub.

  The first process (the 30th) read the skill and then ran three `Bash` calls: two looked for the
  workflow and the CLI (`ls`, `find`, `command -v pulse-factory`) and read `propose --help`, and the
  third was the same `propose s1` call. The second process went to the proposal at once. Neither run
  ran `arm`.

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
