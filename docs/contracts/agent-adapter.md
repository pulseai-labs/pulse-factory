# Agent adapter contract

An **agent adapter** lets the engine run one coding agent. This file is for the author of the next
adapter. It says what the port needs, what an adapter may refuse, and how rules and events reach the
controller. It names no detail of one agent as a requirement. The first adapter is in
`crates/pf-adapter-omp`. A scripted adapter for tests is in `crates/pf-adapter-fake`.

The port is `pf_engine::port::agent::AgentPort`. ADR-0011 sets the nine duties and ADR-0002 sets the
rule that the engine knows no agent.

## What an adapter is

- One crate per agent. It depends on `pf-engine` and may depend on `pf-rules`.
- Nothing in `pf-engine` or `pf-rules` may name an agent, a terminal crate or a flag of one agent.
  `crates/pf-engine/tests/port_is_neutral.rs` enforces this.
- The workflow file names a **runtime** for each role (`LaunchPlan.runtime`). It is a free name. The
  controller registers each adapter under its runtime name in an `AgentRouter`
  (`pf_engine::port::agent`). The router is itself an `AgentPort`.
- The router sends a launch to the adapter of the plan's runtime. It returns a `SessionRef` of the
  form `<runtime>:<inner>`. Every later call for that session goes to the same adapter, which gets its
  own `<inner>` reference back. So an adapter must give a reference that it can read back by itself,
  and it must not rely on the tag.
- A runtime name has text and no `:`. A plan with a runtime that has no adapter is refused with
  `AgentError::Refused("no adapter for runtime \"<name>\"")`. The unit then parks with that reason.
- An adapter keeps no state that must survive a restart (ADR-0007). After a restart, the controller
  asks `is_alive` for each stored reference. A reference that the adapter does not know is not alive:
  `is_alive` answers `Ok(false)` for it, never an error. (A router answers `Refused` for a reference
  with no runtime tag.)

## The nine duties

Each duty is one method of `AgentPort`. `is_alive` and `stop` are two more calls, for liveness and
for ending a session. An adapter returns
`AgentError::Refused` when it will not do a call, and `AgentError::Failed` when it tried and could not.

| # | Method | What the adapter must do |
|---|---|---|
| 1 | `launch(plan)` | Start one interactive session with the plan's model and settings. Return a reference. Fail fast: see "First turn". |
| 2 | `status(session)` | Report `Working`, `Idle` or `Ended`, `turn_ended` and `first_turn_started`. `turn_ended` is true only when a turn has ended and no other turn follows (a turn that will continue is not an end). |
| 3 | `deliver(session, message)` | Put a message into the session. It reaches an idle session only. A call on a busy session returns `AgentError::Failed` and delivers nothing. |
| 4 | `interrupt(session)` | Stop the running turn and leave the session alive and idle. |
| 5 | `usage(session)` | Sum token usage by kind (`input`, `output`, `cache_read`, `cache_write`) and cost in millionths of a dollar, over every turn so far. |
| 6 | `answer_dialog(session, dialog)` | Answer a tool dialog that the adapter knows. Return `DialogAnswer::Unknown` for any other. `Unknown` parks the unit and never blocks it silently. |
| 7 | `apply_rules(plan, rules)` | Edit the plan so the runtime asks for no permission (`rules.prompts_off`) and enforces the role's rules (see "Rules"). The first adapter has no known dialog to answer, so duty 6 answers none. |
| 8 | `remove_ask_tool(plan)` | Edit the plan so the seat has no tool that asks a person a question. Also leave out any tool that wakes the seat later, by its own timer. |
| 9 | `hand_off(session, successor)` | Start the successor session so it continues from a handoff file. The old session ends. The successor has the same runtime as the session. A router refuses any other. |

The engine calls duty 7, then duty 8, then duty 1, in that order, on the same plan.

### `stop`

`stop(session)` ends the session's process **and every process it started**, and waits for the
leader. It is safe to call twice. A reference the adapter does not know, and an untagged reference to
a router, is `Ok(())`: there is nothing to stop. Reads such as `usage` and `drain_events` still work
after a stop, so the controller can take the last reading. The controller calls `stop` for every
session it ends, so no process outlives its stage.

### What a runtime may refuse

A launch is refused (`AgentError::Refused`, never `Failed`) when:

- the agent version is not qualified. An adapter holds a list of qualified versions and refuses any
  other;
- the runtime cannot run without prompts, so duty 7 cannot be met;
- a plugin, skill, tool or server that the plan does not declare would reach the seat and the
  adapter cannot limit it. The adapter checks this before the session starts, and again from what the
  session reports. Fail closed;
- a setting is missing or not readable (for example a non-numeric `first_turn_seconds`).

The engine parks the unit with the reason `launch refused: <why>`.

## First turn

A launch that reaches no first turn must fail fast, with the screen in its reason. In the first run,
three seats sat 48 minutes each at a prompt that no one could answer.

- The adapter waits `first_turn_seconds` (default 120) for the session to start its first turn. It
  also stops waiting when the process ends.
- On a timeout or an early exit, the adapter ends the process and returns `AgentError::Failed`. The
  text is one flat line: a short head that says what happened, then the **last 450 characters of the
  screen, redacted** (`pf_engine::status::redact`, and the seat's own token).
- The engine parks the unit with `launch failed: <text>` through `one_line_reason`. A reason holds 500
  characters. The adapter must shrink the tail so that `launch failed: ` plus the head plus the tail is
  at most 500 characters, because a cut takes the end of the screen, where a prompt sits.
- The screen is data for this reason only. An adapter never reads the screen to detect a state.
- `r0.s1.w2` counts a dispatch after the first turn. So a launch that fails here does not use up a
  unit's attempts.
- `SessionStatus.first_turn_started` is true once the first turn has started.

## Rules

Rules reach the runtime through its own hook. The adapter does not enforce them from outside.

- `pf_rules::decide(rules, context, tool_call) -> Decision` judges one tool call. `rules` is the role's
  `pf_workflow::rules::RoleRules`. `context` holds the seat, its current and own branch, the default
  branch, the forge's API hosts and the seat's own paths. `tool_call` is `{ tool, command or path }`,
  so any runtime can state its calls this way.
- A `deny` is a block with the decision's `reason`. The hook must stop the call and show the reason to
  the seat. The deny list and the role classes are in `docs/contracts/role-rules.md` and ADR-0005.
- **Fail closed.** If the hook cannot run, cannot read the rules or cannot read the call, it denies.
- `pf_rules::audit_line(context, tool_call, decision)` gives one redacted line for each decision, allow
  or deny: seat, command, rule and outcome. The hook writes every line. No secret stays in it.
- A shell command that the rules cannot read is denied with the rule `unparsed-command`. A push whose
  branch is one quoted variable is allowed only for a role that may push its own branch, with the rule
  `own-branch-unverified`. The adapter's push check (see "Push events") guards the default branch after
  the fact.
- The rules read a brace expansion of plain text as the words it makes. They read a `function`,
  `coproc` or `time { … }` body, and a compound command after `time`, as commands. A `case` pattern
  and the values of an array literal are not commands. These are denied with `unparsed-command`:
  - a glob or an expansion in a command name or a `gh` or `git` subcommand;
  - a brace expansion that mixes quoted text with other parts, in any command (`"--no-ver"{ify,}`).
    A brace beside a variable only makes the word unread, which is denied in a command name, a
    `gh` or `git` subcommand and the arguments of a push;
  - a wrapper whose options are not read, from a fixed list of 33 names (`taskset`, `chrt`,
    `setpriv`, `unshare`, `nsenter`, `strace`, `setarch`, `prlimit`, `numactl`, `systemd-run` and
    others in `UNREAD_WRAPPERS`). A wrapper that is not on the list is not read;
  - a git config that runs a command the rules cannot read: an alias, or a key such as
    `core.fsmonitor`, `core.sshCommand` or `credential.helper` with more than a bare program name,
    given by `git -c`, `git --config-env`, `git config` (a write; a read or an `--unset` is
    allowed), or the `GIT_CONFIG_COUNT`, `GIT_CONFIG_KEY_n`, `GIT_CONFIG_VALUE_n` and
    `GIT_CONFIG_PARAMETERS` variables; and a `git -c` value that is a variable;
  - an alias set by `gh alias set` or `gh alias import`.
  A brace sequence of more than 256 words, or with a number beyond 64 bits, is not expanded. Its word
  counts as unread, like a variable.
- The seat token: the adapter sets a fresh `PF_SEAT_TOKEN` in each seat's environment, different for
  each launch. A hook that calls the controller presents it. The token never appears in a log, a reason
  or an event.

## Events

An adapter reports what happened as neutral events. The controller (`r0.s1.w5`) drains them with
`drain_events(session)`. Each call returns the events since the last call, oldest first.

| Event | Meaning |
|---|---|
| `Activity` | The session did something: a turn, a tool call or a message. |
| `Stalled { screen_tail }` | The session showed no activity within `stall_minutes`. The tail is redacted and flat. |
| `Push { id, repo, branch, sha, outcome }` | A seat pushed. See below. |
| `DefaultBranchMoved { remote, branch, before, after }` | A push left a target the adapter could not read, and the default branch of the remote moved since the launch. |

### Stall

The rule is in ADR-0006. The adapter reports `Stalled` as a **candidate** only. The controller decides
with `pf_engine::unit::stall_decision(last_turn, last_file_change, last_commit, last_pr, prior_stalls,
limit, now)`, which gives `Active`, `Relaunch` or `Park`. The controller reads the file, commit and PR
times. A seat that writes files in a long tool call is `Active`. After one relaunch, the next stall
parks the unit through `Engine::park`.

### Push events

- The adapter learns of each tool call that ran a shell command, and its exit code. It does **not**
  learn the call's output: a first push prints no SHA, an update prints a short one, and a seat can
  print any text. A branch and a SHA never come from output.
- It finds each push with `pf_rules::push_of(command)`. An intent is **read** when its branch is
  literal and its directory is literal or absent. Any other intent has an **unread** target.
- `id` is fresh for each event. `repo` is the **declared** repo of the plan (the `repo` setting),
  never text that the seat's remote printed.
- `outcome` is `PushOutcome::Pushed`, or `PushOutcome::Failed(reason)` with a one-line reason. A
  failed push gives an event too. The call's exit code is the outcome of each line it gives.
- **A read push** gives one `Push` for the branch the command names. The SHA is
  `git -C <dir> rev-parse <branch>`; `HEAD` or no branch means the directory's current branch. When
  nothing can be read, `sha` is `unknown`.
- **An unread push** is measured in git. For a seat with a `push_dir`, the adapter reads
  `git -C <push_dir> for-each-ref refs/remotes/<remote>` at launch and again after each bash call
  with an unread target. It leaves out `<remote>/HEAD`. Each other ref that changed since the last
  read gives one `Push`, named by that ref, with that ref's SHA, in name order. A failed read counts
  as no ref changed.
- **The last `Push` of such a call is always `push_branch`**, with its SHA from
  `git -C <push_dir> rev-parse <push_branch>`, whether or not its ref changed, because the
  controller's `last_successful_push` reads the unit's last ledger line. A failed `rev-parse` gives the
  SHA `unknown`. A call that moved no ref gives that one line. Without a `push_branch`, a call that
  moved no ref gives one line with the branch and SHA `unknown`.
- The controller writes the ledger line with `Engine::record_push`.
- A `Push` from a seat with no `push_dir` has no measurement: the rules deny that seat a push.

### Default branch check

The `default_branch` setting names the branch. For a seat with a `push_dir`:

- **At launch** the adapter reads `git -C <push_dir> ls-remote --exit-code <remote> refs/heads/<default
  branch>`, with a 30-second limit. Only a line for exactly that ref counts, so `x/main` is not
  `main`. **A failed read refuses the launch** (`AgentError::Refused`): a seat that pushes must have a
  baseline.
- **After a push with an unread target** it reads the same ref again:
  - unchanged: nothing;
  - moved, and `git -C <push_dir> cat-file -e <sha>^{commit}` finds the new tip: `DefaultBranchMoved`,
    and the new tip becomes the baseline;
  - moved, and the repository lacks it: someone else moved it. The new tip becomes the baseline and
    nothing is reported, so a later unread push compares against it;
  - a failed read: one retry, then `DefaultBranchMoved` with an empty `after`.
- `remote` is the setting of that name (default `origin`). The remote's name in a seat's command is
  never used for a read.
- Without a `default_branch` setting, an adapter may take the branch from the remote's `HEAD`, and a
  failed read then leaves no baseline.

## `resolve_plugin`

`resolve_plugin(name, min_version) -> ResolvedPlugin { name, version, path }` finds a plugin by name at
a version of at least `min_version`. It makes no prompt of its own and starts no session. A plugin it
cannot find, or finds at a lower version, is `AgentError::Refused`. A router has no runtime to go by,
so it asks its adapters in name order and returns the first answer that succeeds.

## Launch dialogs

No launch dialog is known for the first adapter. A dialog that an adapter does find (a trust question,
an update question) must be either answered by duty 6 or named in the adapter's own documentation as
a requirement to meet before a launch. A dialog that is neither is `DialogAnswer::Unknown`, which parks
the unit with the dialog's text. The adapter never leaves a seat at a prompt without a reason that the
controller can show.

## Settings an adapter reads

The plan's `settings` map is rendered by the engine (`docs/contracts/launch-settings.md`). An adapter
reads these keys and ignores the rest:

| Key | Use |
|---|---|
| `cwd` | The seat's working directory. |
| `message` | The first message. |
| `brief` | The brief paths, comma-joined. |
| `env.<NAME>` | One environment variable for the seat. |
| `plugins` | The pinned plugin directories, comma-joined. |
| `skills` | The skill names the seat may use, comma-joined. |
| `first_turn_seconds` | The time for a first turn. Default 120. |
| `stall_minutes` | The time without activity before `Stalled`. Default 20. |
| `push_dir`, `push_branch` | Where to measure a push whose target the rules cannot read. |
| `remote`, `default_branch` | The remote to read, and the branch of it that a push must not move. The controller adds them. |
| `repo` | The declared repo for `Push` events. The workflow file does not admit this key. The controller adds it. |
| `thinking` | An optional reasoning level. The workflow file does not admit this key. The controller adds it. |

An adapter documents any other key it reads. It does not change the meaning of a key above.

## Before you ship an adapter

1. List the nine duties and, for each, how the runtime does it. Where it cannot, say what the adapter
   refuses.
2. Qualify an agent version by a real session, and record the result in an ADR.
3. Run the real sessions only from marked, ignored tests. Count them. A plain `cargo test` and CI never
   start a paid session.
4. Make the hook fail closed, and test it with a call it must deny.
5. Test the first-turn failure with a silent stub and with a stub that prints a prompt and a token.
