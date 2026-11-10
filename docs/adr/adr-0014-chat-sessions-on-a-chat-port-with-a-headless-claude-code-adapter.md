# ADR-0014 — Chat sessions on a chat port, with a headless Claude Code adapter

- **Status:** Accepted, 2026-11-09. It was owed at the `r1.s3.w1` close (proposed 2026-11-08, at
  `r1.s3.w1`). The line was written at `r1.s4.w1`.
- **Date:** 2026-11-08
- **Touch surface:** `crates/pf-engine/src/port/chat*`, `crates/pf-adapter-claude/**`.
- **Revisit when:** a second chat runtime is planned, or the pinned CLI's stream-json control
  protocol fails a qualification run.

## Context
- Release 1, exit criterion 2: a reply in the app comes from a real Claude Code session in the
  project's worktree on draco-desk, with its cost. The operator chats with an agent (ADR-0002's
  vocabulary), so the engine needs a way to hold a chat that is not a run seat.
- ADR-0011's agent port serves run seats. Its rule "Execution units are interactive terminal
  sessions" fits a seat that works alone for hours. A chat needs different things: every text delta
  as it streams, approvals and questions the operator answers, a message sent during a turn, live
  changes of model, effort and permission mode, the cost of each turn, and a resume of the same
  runtime session after the process was released.
- Three chat apps were read as behaviour on 2026-11-08 (T3 Code, Orca and OpenBot; the evidence is in
  the AI workspace at `docs/superpowers/evidence/2026-11-08-claude-code-chat-drivers/`). All three
  drive Claude Code through the Agent SDK's streaming `query()`, keep one live process per session
  and reopen it with `resume`. The SDK is a thin layer: it starts a `claude` binary and speaks
  `stream-json` with a control protocol over stdin and stdout. Only OpenBot reads
  `total_cost_usd`. OpenBot also pins a managed copy of the CLI and sets `CLAUDE_CODE_ARTIFACT=0`.
- A headless probe on draco-desk (CLI 2.1.319) showed the subscription login works headless
  (`apiKeySource` `none`) and the `result` carries `total_cost_usd` and `usage`. It also showed that
  `CLAUDE_CODE_ARTIFACT=1` gives the headless CLI the `Artifact` tools. The first reading of the
  probe said headless mode has no `Artifact` tool. That was wrong, and the operator ruled on it
  (2026-11-08).
- The user's own `claude` updates itself (2.1.316 to 2.1.319 arrived between 2026-10-31 and
  2026-11-06), so a chat that ran it would change behaviour without a release.

## Decision
- **A chat port.** `pf-engine` defines `ChatPort` (`crates/pf-engine/src/port/chat.rs`), a fifth
  port beside the store, agent, forge and unit-source ports. It is separate from `AgentPort` (grill
  Q3): the two have different shapes, and a chat adapter's calls are not a run seat's duties. The
  port names no runtime. It reuses `SessionRef` and `Usage` from `port::agent` and moves neither.
  Its contract is `docs/contracts/chat-adapter.md`.
- **One adapter crate for Claude Code.** `crates/pf-adapter-claude` implements `ChatPort` as
  `ClaudeChat`. It drives the pinned `claude` CLI headless, with `-p --input-format stream-json
  --output-format stream-json --verbose --include-partial-messages --permission-prompt-tool stdio`
  and the control subset of the contract (`initialize`, `interrupt`, `set_model`,
  `apply_flag_settings`, `set_permission_mode`, `get_context_usage`, and the answers to
  `can_use_tool`). It uses no SDK and no Node sidecar. It never sends the `background_tasks` control
  request (it moves tasks to the background).
- **The subscription login and an allowlisted environment.** The chat never gets `ANTHROPIC_API_KEY`;
  the child's environment is an allowlist (`HOME`, `PATH`, the locale and XDG names and a few more),
  the agent's own env map, `DISABLE_AUTOUPDATER=1` and `CLAUDE_CODE_ARTIFACT` set to `1` or `0`. No
  Orca, herdr, Claude Code session or Pulse Factory name reaches it. The flags never include
  `--bare`, `--no-session-persistence`, `--setting-sources` or `--max-turns`, and the environment
  never sets `CLAUDE_CONFIG_DIR`. User, project and local settings load, and the transcript lands in
  the default `~/.claude`, so a terminal can resume the same session. An `init` whose `apiKeySource`
  is not `none` ends the process.
- **One live process per chat, six lifetime rules.** The adapter keeps a process alive between
  turns and has no timer. The controller owns the 30-minute idle release, the rule that a process
  with a running background task is not released, the cap on live chat processes and the restart
  rule (w3). A released chat reopens with `--resume` on the same runtime session id.
- **Transcript and resume.** Before it starts a process for a resume, `open` looks for
  `<config_dir>/projects/*/<id>.jsonl`. None found is `ResumeFailed`. A process that exits before
  `initialize` is answered, or that writes the error result case 15 recorded, is `ResumeFailed` too.
  An adapter never answers a failed resume with a fresh context: the controller shows "context lost"
  and the operator chooses.
- **Bypass by default, declared options.** The default permission mode is `bypassPermissions`
  (Q5). `options()` declares `model`, `effort`, `permission_mode` and `artifacts`. The first three are
  live: `set_option` sends `set_model`, `apply_flag_settings` with `effortLevel`, or
  `set_permission_mode`. `AskUserQuestion` still reaches the host under bypass.
- **A managed copy of the CLI, pinned and checked.** Pulse Factory's developers manage the chat's
  Claude Code version (operator ruling, 2026-11-08, OpenBot's model). `pinned.rs` holds CLI 2.1.319,
  the SDK package version whose manifest carries it (`0.3.319`) and the SHA-256 and size of the
  official per-platform binary (`@anthropic-ai/claude-agent-sdk-<platform>`) for `linux-x64` and
  `darwin-arm64`. The copy lives at `<share_dir>/claude/2.1.319/claude`, in Pulse Factory's own
  directory. An install step (w3) fetches it; Pulse Factory never redistributes it, and w1 never
  fetches at runtime. `open` hashes it in chunks, once per (size, modification time), and refuses a
  missing or changed copy with `RuntimeNotReady` before a process starts. A `claude_bin_override`
  runs any binary with no hash check and marks the chat `qualified: false`.
- **Recordings, a replay binary and a fake as the test paths** (A5). The qualification run recorded
  21 fixtures from the real CLI, scrubbed and hashed. `pf-claude-replay` plays one back in place of
  `claude`: the real `ClaudeChat` runs against it with no token and no network. `FakeChat`
  (`crates/pf-adapter-fake`) is a scripted adapter with no process, for the controller's and the
  API's tests. No ordinary test, no `auto:` line and no CI job starts the real CLI. The qualification
  recorder runs only with `PF_REAL_CLAUDE=1`, against a counted
  budget of 31 processes (30, and one more for case 20 alone, by the operator's ruling of 2026-11-10).
- **The 16 KB cap.** `cap` cuts a tool's input and output once, in the event, to at most 16 KB with
  its mark, and `Capped` carries the original size. The adapter keeps an approval's original input
  for its answer.
- **Per-turn usage.** `TurnResult.usage` is the turn's own usage. `total_cost_usd` and `modelUsage`
  are cumulative per process, so the adapter keeps the last totals as integers (cost in micros) and
  subtracts. A new process, a resumed one included, starts at zero. A total that goes down counts
  zero for that turn and becomes the new baseline. The session's total is the sum of its turns.
- **Mid-turn steering** (planner ruling, 2026-11-08). A `send` during a turn goes into the running
  turn (`MidTurn::Steer`). Case 10 showed the CLI folds it in. `NextTurn` stays the declared degrade
  for a CLI that cannot.
- **`artifacts`.** The option's env var is `CLAUDE_CODE_ARTIFACT`. It defaults to `on` for the
  Orchestrator. It is not live: `CLAUDE_CODE_ARTIFACT=1` lists `Artifact`, `ArtifactComments` and
  `ArtifactData`, and `0` lists none. The CLI reads it at start, and a live `enableArtifact:false`
  removes the tools but `true` does not add them back. So a change applies at the chat's next
  process. The cost is 12995 tokens in each request (case 17).
- **No per-task stop.** `initialize` carries no `perTaskStopAffordance` (operator ruling,
  2026-11-08, accepted for `r1.s3`). An interrupt then ends the running turn and its background
  agents. What it does to a background `Bash` is case 9's result below. A per-task stop is a later
  feature.
- **This ADR amends ADR-0011 for chat sessions only.** The terminal rule holds for run seats.

Rejected alternatives:
- **A Node sidecar on the Agent SDK.** It adds a runtime and a package. It stays the fallback behind
  the same port, if a qualification run ever fails the stream-json protocol.
- **One process per turn.** A fresh process costs about 0.9 s before the model call and a cache
  write each time.
- **New methods on `AgentPort`.** Run seats and chats differ in shape. The agent port would grow
  duties no seat needs.
- **A PTY-driven interactive `claude` for chat.** It reads a screen, with no structured deltas, no
  cost field and no approvals as data.
- **The user's installed `claude`.** It updates itself, so the chat would change with no release.

## Consequences
- Each CLI upgrade needs a qualification run, new recordings and a release step ("Bump the CLI pin"
  in the contract).
- The operator's own hooks, such as the seat-mods and Orca hooks of the user settings, run in a chat
  session. In the qualification they refused one write outside the worktree (case 18). This is an
  operator-environment fact, not an adapter defect.
- Q4's known limits stand: the tool output a secret prints still reaches the events (the
  seat-output scrubber is later work), and the transcript is the runtime's, not Pulse Factory's.
- A chat that resumes a session whose last turn a newer terminal CLI wrote may fail, and
  `ResumeFailed` then shows "context lost".
- A background `Bash` runs in its own session, so a group kill alone does not end it. The stop
  sequence works because it sends `TERM` to the group (case 16).
- Subagent spend lands in the turn whose `result` first reports it.
- A process whose `init` reports an API key source other than `none`, a timer tool, or another
  version is stopped, and the reason is in `Exited`. These checks come after the fact; the
  environment allowlist is the guard.
- One throwaway private page titled `pf-probe` stays on the operator's account from case 18. Headless
  delete is refused, so the operator deletes it by hand. The item's report names it.

### Verified claims
Settled on 2026-11-08 on draco-desk, with CLI 2.1.319 from the managed copy (SHA-256 `dabafec8…8eed`,
340226576 bytes), model `haiku`, effort `low`, each case in a fresh temporary git directory. The
recorder spent **28 of the 30 real processes** of its budget (case 1 ran twice, and cases 4, 7, 9, 14,
16 and 18 twice). Each case's recording is `crates/pf-adapter-claude/fixtures/<case>.jsonl`. The
required cases (1, 7, 9, 15, and `--permission-prompt-tool stdio` accepted) all passed, so **2.1.319
is qualified**.

1. **Plain turn.** Text deltas, finished text blocks and a `result` with `total_cost_usd`, `usage`
   and `modelUsage`. Three turns in one process gave cumulative costs 0.00346456, 0.00621792 and
   0.006525. `modelUsage` is cumulative (turn 2: 41008 cache-creation tokens while that turn's `usage`
   said 13006) and counts side calls that `usage` omits. The cache time to live is 1 hour
   (`ephemeral_1h_input_tokens` above 0, `ephemeral_5m` 0), so `cache_ttl_seconds` is 3600. Closing
   stdin gave exit 0. Besides the SDK's kinds, the CLI wrote `command_lifecycle`, `rate_limit_event`,
   `system/status`, `system/ui_status`, `system/thinking_tokens`, `system/session_metadata` and hook
   messages; the decoder ignores them.
2. **`init`.** It comes after the first user message and again at each turn. With a dummy
   `ANTHROPIC_API_KEY` in the recorder's environment: `apiKeySource` `none`, `permissionMode`
   `bypassPermissions`, version 2.1.319, no timer tool. The child's environment held `CLAUDE_CODE_ARTIFACT`,
   `DISABLE_AUTOUPDATER`, `HOME`, `LANG`, `LOGNAME`, `PATH`, `SHELL`, `TERM`, `USER` and
   `XDG_RUNTIME_DIR`, and no `ANTHROPIC_*`, `ORCA_*`, `HERDR_*` or `PF_*` name.
3. **The allowlist is enough** for auth and the network: the turn succeeds. The recorder's own names
   that the allowlist drops and that bear on proxies, certificates or D-Bus were `SSL_CERT_DIR` and
   `SSL_CERT_FILE` only. No name was added to the allowlist.
4. **Operator hooks.** The session-start hooks of the user settings ran (3 started, 3 responses),
   none failed, and process stderr held 0 bytes.
5. **A fresh git directory** raised no dialog, no trust message and no stderr.
6. **Models.** `default` and `opus` resolve to `claude-opus-5-5`, `fable` to `claude-fable-5-1`,
   `sonnet` to `claude-sonnet-5-5`, `haiku` to `claude-haiku-5-5`, and the list holds
   `claude-haiku-4-5-20251001`, `claude-sonnet-5`, `claude-opus-5`, `claude-fable-5`,
   `claude-opus-4-8`, `claude-opus-4-7`, `claude-opus-4-6` and `claude-sonnet-4-6`. w2's seed
   `claude-opus-5-5` needs no gap.
7. **Permissions.** `set_permission_mode` `manual` succeeded (the response says `default`). A
   read-only `echo` runs with no request, and `touch` asks. The `can_use_tool` request holds
   `tool_name`, `input`, `tool_use_id`, `description`, `display_name`, `blocked_path` and
   `permission_suggestions`, and no `decision_reason`, so the adapter's `reason` is `decision_reason`
   else `description`. `deny` gave a `tool_result` with `is_error` true; `allow` with `updatedInput`
   ran the tool. `AskUserQuestion` arrives as `can_use_tool` with `requires_user_interaction`;
   `updatedInput` = the input plus `answers` (question text to label) worked.
8. **Bypass.** Under `bypassPermissions` a `Bash` call raises no request, but `AskUserQuestion` still
   reaches the host.
9. **Interrupt.** The response is `{"still_queued":[]}`, the `result` came at once with subtype
   `error_during_execution`, `is_error` true and `stop_reason` `tool_use`. The process lived and the
   next send started a turn. So `TurnResult.stop` is the error subtype for an error result. **A
   background `Bash` (`sleep 113`) survived the interrupt.** A background task runs in its own
   process group and session.
10. **Mid-turn send.** The CLI folded it into the running turn: one `init`, one `result` whose
    `user_message_uuids` lists both ids. Selected: `MID_TURN = Steer`.
11. **Background `Bash`.** `background_tasks_changed` lists it (`task_id`, `task_type` `local_bash`,
    `description`, `run_id`, no `ambient` key) and drops it after; `task_started`, `task_updated` and
    `task_notification` came. **The CLI starts a turn itself** on the finish: an `init` with no
    `user_message_uuids`, which is the `Task` cause. A repeated `initialize` gets a success response
    and then a `background_tasks_changed` snapshot. Selected: self-wake `Cli`, the adapter sends no
    notice; task signal `background_tasks_changed`.
12. **Live changes** all work: `set_model`, `apply_flag_settings` with `effortLevel`, and
    `set_permission_mode`. The next `init` showed the new values. Selected: live changes yes.
13. **`get_context_usage`** with `detail: "summary"` answers `totalTokens` and `maxTokens` (28005 and
    1000000 for `haiku`). Selected: context usage yes.
14. **Big output.** `seq 1 20000` (106 KB) stays in a file, and the CLI sends a 2 KB preview
    (`<persisted-output>`). A second call, `seq 1 4000`, gave an inline `tool_result` of 18892
    bytes, which the adapter caps.
15. **Resume.** `--resume` works: the transcript is `~/.claude/projects/*/<id>.jsonl`, the resumed
    `init` has the same `session_id` (so no `SessionMoved` in the recording), and the reply knew the
    first turn. A resume of an id with no transcript: no `initialize` answer, one error `result`,
    stderr `No conversation found with session ID: <id>`, exit 1.
16. **Stop.** With stdin closed and a background `sleep` running, the leader did not exit in 5 s.
    `TERM` to the group ended the leader (code 143) and the task. `KILL` alone left the task, which
    has its own session. The stop sequence (close stdin, 5 s, `TERM`, 2 s, `KILL`) works because of
    `TERM`.
17. **`CLAUDE_CODE_ARTIFACT`.** With `1`, `init.tools` lists `Artifact`, `ArtifactComments` and
    `ArtifactData`; with `0` it lists none. The first-turn request held 41001 tokens with them and
    28006 without, so **12995 tokens per request** (OpenBot's "about 13k" holds). `apply_flag_settings`
    `{"settings":{"enableArtifact":false}}` removes the tools on the next turn, and `true` does not add
    them to a process started with `0`. So `artifacts` is `live: false`. Selected: artifacts
    supported, default `on`.
18. **Headless publish** works with the tools on: "Published … Version 1". The first attempt was
    refused by the operator's seat-mods hook, which blocked a page file written outside the case's
    own directory. **Headless delete is refused** ("needs the user's confirmation"). One throwaway
    private page titled `pf-probe` stays on the operator's account.

Recorded facts about the stream: the deltas of a text block join to its `Text` in all 21
recordings. This is a fact of the recordings, held by a test, and not a rule of `check_order`.

### Unverified claims
- OpenBot's comment that requests labelled `sdk-*` count about twice as heavily on the 5-hour window.
  The chat sets no `CLAUDE_CODE_ENTRYPOINT`, and the claim was not measured.
- What the CLI does to a background `Bash` when an interrupt arrives from a process that sent
  `perTaskStopAffordance`. The adapter does not send it.
- The behaviour of a session whose last turn a newer terminal CLI wrote, when the managed copy
  resumes it.
- Any CLI version other than 2.1.319. Each needs its own qualification run.

## Amendment, 2026-11-09 (`r1.s4.w4`): the deny list at launch

The chat port gains one runtime-neutral field, and the Claude Code adapter turns it into deny rules.
The setting that supplies the list is ADR-0016's.

- **`ChatOpen.deny_commands`** is a list of shell command patterns that the runtime must refuse to
  run. The controller resolves `chat.deny_commands` for the session's project and agent at each open,
  new or resumed, and writes the count and the scope on the `chat.ledger` `open` line (`deny=`,
  `deny_from=`). A change reaches a process only at its next launch.
- **The pattern grammar** is one rule for the controller and both adapters (`check_deny_command`):
  1 to 100 characters of letters, digits, space, `.`, `_`, `/`, `:`, `=`, `@`, `+`, `-` and `*`; a
  letter or a digit first; no trailing space and no two spaces in a row; `*` only as a whole word or the
  last character of a word; **no `:*` anywhere**, because Claude Code reads a rule that ends `:*` as its
  legacy prefix form and `Bash(git push * :*)` would deny every push; so no `(`, `)`, `,`, quote, `;`,
  `&`, `|` or control character reaches a rule. A list holds at most 32 entries and no duplicate.
  `validate` and `FakeChat::open` refuse a list that fails it, and name the entry.
- **The rule form.** `launch::disallowed_tools` is the five timer tools, then `Bash(c)` and
  `Bash(c *)` for each entry `c`, or `Bash(c)` alone when `c` ends in `*`, joined by `,`, as the one
  argument after `--disallowedTools`. With an empty list it is the old argument.
- **The fifteen built-in patterns** cover the common spellings of a forced push (`-f`, `--force`,
  `--force-with-lease`, a `+` refspec, with a flag before or after the remote or branch), a remote
  branch delete (`--delete`, `-d`), `git branch -D`, and `gh pr merge`, with `-R` or `--repo` before the
  subcommand. The colon refspec (`git push origin :branch`) is not covered: its only rule would end
  `:*`. The list is in `chat-adapter.md`.
- **The known limit, in Q4's words.** The deny list is a **speed bump**, not a boundary. A deny rule
  matches the command text the model writes, so it stops an accidental merge by the model and not a
  determined one. The residue gets past it: an absolute path, `gh api …/merge`, a script, an alias, a
  combined short flag (`git push -uf`), the colon refspec, an environment prefix and another tool. The
  chat keeps `HOME`, so it reaches the operator's `gh` login and git credentials, and a merge it makes
  skips the ack, its confirm code and its ledger line. Cutting credentials at launch was not taken.
  The owner of the limit is the Release 2 authority-rules spine.
- **Rejected: a PATH shim.** A shim that stood in for `git` and `gh`, first on the chat's `PATH`, was
  considered and not taken. An absolute path bypasses it, so it is no stronger than the rules, and it
  adds a file the adapter must place and keep.

### Verified claims (case 19, `r1.s4.w4`)

Case 19 (`case_19_deny_rules`) ran once, on CLI 2.1.319 with `bypassPermissions`, in a temporary git
repo with a bare `origin` and a stub `gh`, with the fifteen built-in patterns (30 rules). It spent one
probe process (the counter reads 29 of 30). The planner granted that process. The recording is
`fixtures/deny_rules.jsonl`.
- **The 30-rule argument parses.** The CLI ran, so a comma-joined list with spaces inside the
  parentheses is accepted.
- **A deny rule holds under `bypassPermissions`,** and the CLI answers in the rule's own text:
  `Permission to use Bash with command … has been denied.` Prompt 4, `gh -R pulseai-labs/pf-probe pr
  merge 1`, met `Bash(gh -R * pr merge *)`, so a `*` in the middle of a command matches in 2.1.319. The
  stub `gh` did not run (`gh-ran` was not created).
- **Prompt 0 gives the self-started turns.** Its two background `sleep` commands finish and the CLI
  starts two turns by itself: the recorded causes are `Operator`, `Task`, `Task`. That is the replay's
  input for `scripts/demo/chat-policy.sh`.
- Nothing changed in the temporary repo: the branch stayed and `origin/main` stayed at its commit.

### Unverified claims (case 19)

Case 19's required checks are not all met, and the one process is spent. A second run needs a new grant.
- **Prompts 1 and 2** (`git branch -D pf-probe`, `git push -f origin main`) were refused by the host's
  own `PreToolUse` hook, with a `seat-mods (implementer)` text, before any deny rule applied. The probe
  inherits the operator's `HOME` and hooks, so the recording shows a hook and not the rules. Whether
  `Bash(git branch -D *)` and `Bash(git push -f*)` hold is unverified. A clean rerun needs an isolated
  `HOME`, which is the operator's call.
- **Prompts 3 and 5** (`git push origin main --force`, `true && git branch -D pf-probe`): the model made
  no `Bash` call. The test is inconclusive on a flag after the arguments and on a compound command.
- That every built-in pattern's `*` matches as prompt 4 showed for one. The decode test
  (`decode_fixtures.rs`) asserts only that each recorded `Bash` call of the four commands got an error,
  and that at least three exist.
- The recording holds the hook's text, which names the operator's setup. The scrub leaves it as it is.
