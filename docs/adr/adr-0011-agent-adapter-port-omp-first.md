# ADR-0011 — An agent adapter port, with OMP as the first adapter

- **Status:** Accepted, 2026-11-05, at the Release 0 close
- **Date:** 2026-10-25
- **Amended:** 2026-10-26, at the planning of spine r0.s1, with text from its review round 1 on
  2026-10-31: `--no-skills` disables all skill loading,
  so a seat could not run the workflow's method skills, such as ossify's `/run-spine`.
- **Amended:** 2026-11-01, in `r0.s1.w3`: the six unverified claims were settled on real OMP sessions.
  Each result is under "Verified claims". No decision changed: each result took a fallback that
  this ADR already named.
- **Amended:** 2026-11-01, in `r0.s1.w5`: the port gets a `stop` call (see the duty list). The
  adapter measures a push in git and no longer reads it from a call's output.
- **Amended:** 2026-11-08, at `r1.s3.w1`: the terminal rule holds for run seats. Chat sessions run
  on the chat port (ADR-0014).
- **Amended:** 2026-11-11, in a patch: OMP 18.16.0 is qualified by a real probe run on DeepSeek V4.1
  Flash, and the probes' model moved to it (see "Qualified versions"). No decision changed.

## Context
- Each coding agent gets an adapter with nine duties:
  1. launch a session;
  2. read its status and turn end;
  3. deliver a message into an idle session;
  4. interrupt it;
  5. read its token usage;
  6. answer its known tool dialogs;
  7. apply prompts-off and the role rule set;
  8. remove the ask tool;
  9. hand off to a successor session.
- Two calls sit beside the nine and are not duties of the agent: `is_alive` asks whether a stored
  reference is still a live session, and `stop` ends a session's process and every child of it. `stop`
  is safe to repeat, and a reference the adapter does not know is `Ok`. The controller calls `stop` for
  every session it ends, so no seat outlives its stage. `is_alive` of a reference the adapter does not
  know is `Ok(false)`: after a restart the controller parks the unit that holds it.
- Execution units are interactive terminal sessions. There is no headless or API substitute.
- Release 0 runs cheap seats only, in OMP, as the r3.s2 pilot did. The pilot drove OMP through T3
  Code's injected extension, and Pulse Factory depends on no T3 Code. So it was not known whether
  Pulse Factory could get these signals from an interactive OMP session by itself. The spike below
  settled it.

## Decision
- `pf-engine` defines an agent adapter port with the nine duties.
- `crates/pf-adapter-omp` is the first adapter. It launches OMP as an interactive TUI in a PTY, with:
  - `--no-extensions` and `--no-rules`, so discovered operator extensions and rules do not load;
  - `--plugin-dir <path>` for each plugin the workflow declares (ADR-0004), with `--skills=<globs>`
    to limit the seat to those plugins' skills. The limit holds for skills (see "Verified claims").
    It does not hold for MCP servers. If an undeclared skill, hook, tool or MCP server reaches a
    seat, the adapter launches with a verified isolated `--profile` or refuses the launch. The
    adapter refuses. The only extension Pulse Factory passes is its `--hook` path;
  - `--approval-mode yolo`, which turns prompts off;
  - a `--tools` list that leaves out `ask` and every self-wake tool, and leaves out `task`: the
    adapter cannot track subagents (see "Verified claims");
  - `--hook` loading Pulse Factory's own OMP extension.
- The extension uses only OMP's public extension API:
  - **turn end** is `agent_end` without `willContinue === true`;
  - **delivery** is `pi.sendUserMessage` once `ctx.isIdle()` is true;
  - **deny** is a `tool_call` handler that returns `{ block: true, reason }`, with the reason taken
    from `pulse-factory hook`'s rule decision;
  - **usage** is the `usage` on each assistant `message_end`.
- The adapter reads `omp --version` at launch, and refuses a version not yet qualified. 18.11.3 is
  qualified by the spike and the probes of 2026-11-01. 18.16.0 is qualified by the probes of
  2026-11-11 (see "Qualified versions").
- Duties 4, 6 and 9 follow the probes of 2026-11-01 (see "Verified claims"): interrupt is Escape,
  there is no launch dialog to answer, and a handoff is a new launch that names a handoff file.

Rejected alternatives:
- **`--mode rpc` or `rpc-ui`.** It would need a recorded change to the interactive-terminal rule, and
  the spike shows it is not needed.
- **Reading the PTY screen.** It is fragile. Earlier multiplexer trials had detection failures.
- **T3 Code or herdr as a substrate.** Pulse Factory depends on no pilot or marketplace plugin.

## Consequences
- Signals arrive as structured events, so turn end, delivery, deny and usage need no screen
  heuristics.
- The adapter depends on OMP's extension API. An OMP upgrade needs a new qualification run.
- The extension is product code. It lives in `crates/pf-adapter-omp/` and is built and tested in the
  skeleton spine, not copied from the spike.

### Verified claims
These come from the spike of 2026-10-25, on OMP 18.11.3 with `zai/glm-5.3-flash`. The evidence is
in the AI workspace, at `docs/superpowers/evidence/2026-10-25-omp-adapter-spike/`.
- `agent_end` fired 10 ms after the final assistant message, in both turns.
- A delivered message started a new turn in the same millisecond.
- A force-push was blocked, and the model quoted the reason. A bare remote's `main` did not change.
- `usage` carried input, output, cache-read, cache-write and cost fields.
- With `ask` left out of `--tools`, the model had no ask tool.

These were settled on 2026-11-01, on OMP 18.11.3 and `zai/glm-5.3-flash`, by the real probes in
`crates/pf-adapter-omp/tests/real_probe_*.rs`. The count of real sessions is in the item's report.
- **MCP servers (2026-11-01).** OMP loads MCP servers from config under the launch flags. A project
  `.mcp.json` server and a declared plugin's `.mcp.json` server both started, and their tools were
  active in the seat although `--tools` named six tools. `pi.setActiveTools` from the extension did
  not remove them. Result: the adapter refuses a launch when a declared plugin brings an MCP server
  or when an MCP config file lies in the seat's directory tree or the operator's home, and it ends
  a launch whose seat reports a tool outside its list.
- **`--plugin-dir` with `--skills` (2026-11-01).** `--plugin-dir A --skills=alpha-*` gave the seat
  exactly plugin A's two skills. A skill of a plugin that was not passed, a project `.claude/skills`
  skill and the operator's user skills did not reach the seat. A plugin's `hooks/hooks.json`
  (SessionStart and PreToolUse) did not run. A plugin brought no tool except through an MCP
  server. With no declared skill the adapter passes `--no-skills`, and the extension reads the
  `<skills>` list of the system prompt so that the adapter ends a launch that holds a skill no
  declared glob matches.
- **Interrupting a session, duty 4 (2026-11-01).** Escape written to the PTY ends a running turn:
  `agent_end` without `willContinue` came 100 ms later, the session was idle and alive, the running
  bash call ended with exit code 1 and "Command aborted", and a message delivered next started a
  new turn.
- **Launch dialogs, duty 6 (2026-11-01).** No launch dialog appeared in any real launch, each in a
  directory OMP had not seen, with `--approval-mode yolo`. The first screen held only terminal
  capability queries. This holds on a host whose OMP setup wizard and update check are off. Duty 6
  answers no dialog: an unknown dialog returns `DialogAnswer::Unknown`, and the unit parks.
- **Handing off, duty 9 (2026-11-01).** A successor session continued from a handoff file. The first
  session wrote `handoff.md`; `hand_off` started the successor, whose first turn started; the
  successor read the file and wrote its content to a second file; the first process was gone. OMP
  carries no conversation across, so a handoff is a new launch whose message names the file.
- **Turn end while subagents run (2026-11-01).** With `task` in `--tools`, each subagent loaded the
  extension and wrote its own `agent_start` and `agent_end` into the seat's event file. One seat and
  two subagents gave three `agent_end` lines without `willContinue`, and the first came before the
  seat's last. A subagent's tool list was wider than `--tools` (it held `web_search`, `eval` and
  `find`). The adapter cannot attribute these events, so it leaves `task` out of `--tools`.
- **Context files (2026-11-01, finding).** `--no-rules` does not stop OMP from loading context
  files: the seat's system prompt held the operator's `~/.omp/agent/AGENTS.md`. This is a known
  limit, not a settled claim.

Note, 2026-11-11: the claims above were verified on `zai/glm-5.3-flash`, the model of their day.
The Z.AI quota has since ended, and the shipped `ossify-spine` roles now run
`ollama-daemon/deepseek-v4.1-flash:1017-cloud` (operator ruling G10). No claim was re-run on that
model, and no decision here changes. (Superseded in part the same day: see "Qualified versions".)

### Qualified versions
- **18.11.3:** the spike of 2026-10-25 and the probes of 2026-11-01, on `zai/glm-5.3-flash`.
- **18.16.0 (2026-11-11):** the real probes ran on OMP 18.16.0 with
  `ollama-daemon/deepseek-v4.1-flash:1017-cloud`, now `probe::PROBE_MODEL`. They spent 6 sessions
  of the budget (13 used before, 19 after, `LIMIT` 20). Each probe passed, and each result matched
  the claim above it:
  - `real_probe_launch`: the turn ended, the session was idle, and `usage` carried input, output,
    cache-read and cost fields. No launch dialog appeared (duty 6).
  - `real_probe_interrupt`: Escape ended the running turn after 100 ms. The session stayed alive and
    idle, the `sleep 120` bash call ended with exit code 1, and the next message started a new turn
    (duty 4).
  - `real_probe_mcp`: the project `.mcp.json` server started, its tool reached the seat, and the
    adapter refused the launch (`mcp__canary_ping`).
  - `real_probe_plugins`: `--plugin-dir` with `--skills=alpha-*` gave the seat exactly `alpha-one`
    and `alpha-two`. The undeclared plugin's skill, the project skill and the operator's user skills
    did not reach the seat, and the plugin's hook did not run.
  - `real_probe_handoff`: the successor read the first session's handoff file and answered from it,
    and the first process was gone (duty 9).
  - Not re-run: `real_probe_subagents` (3 sessions, which would pass `LIMIT`). The adapter still
    leaves `task` out of `--tools`, so that result does not change any launch.
- The controller ran OMP 18.11.3 through a PATH pin in its environment file from 2026-11-11 until this
  qualification shipped. A launch on an unqualified version parks the unit with a request, as before.

### Unverified claims
None. `r0.s1.w3` settled the six that this ADR listed.

### Spike contract

- **Status:** resolved (2026-10-25, 13:32:39Z–13:35:23Z, inside the 90-minute timebox)
- **Hypothesis:** an OMP extension loaded with `--hook` into an interactive OMP TUI session in a PTY
  can do four things:
  - (a) write a turn-end event within 2 s of the turn ending;
  - (b) take a message delivered into an idle session and start a new turn;
  - (c) deny a tool call by rule, with an error the model sees;
  - (d) expose the turn's token usage.
- **Falsifier:** any of (a)–(d) fails within the timebox. ADR-0011 then chooses between
  `--mode rpc-ui` and reading the PTY screen. `--mode rpc-ui` would need a recorded change to D-02.
  The operator rules on the choice.
- **Timebox:** 90 minutes. There is no Rust build. The extension uses OMP's own extension format, and
  the PTY driver is a short script.
- **code_fate:** discard. The code lived in a scratch directory outside both repos, and nothing
  merged.
- **Evidence retained:**
  - this decision note;
  - the README and two event logs in the AI workspace at
    `docs/superpowers/evidence/2026-10-25-omp-adapter-spike/`.
- **Decision it enables:** this ADR.

**Decision note.**
- All four held, and the falsifier did not trip:
  - (a) `agent_end` came 10 ms after the final message;
  - (b) the delivered message started a turn at once;
  - (c) the deny reason reached the model, and the remote did not change;
  - (d) `usage` came with every assistant message.
- An extra check passed too: with `ask` left out of `--tools`, the seat had no ask tool.
- The scratch directory was deleted after the note was written. The bone is written with the spike
  cited as its evidence.

## Amendment, 2026-11-08 (`r1.s3.w1`): chat sessions

The rule "Execution units are interactive terminal sessions" holds for **run seats**, which this port
serves. A **chat session**, where the operator talks to an agent, runs on a separate chat port
(`ChatPort`, ADR-0014), with its own adapter crates. A chat adapter drives its runtime headless over a
structured protocol, not a terminal. It does not replace the nine duties here and adds none to them.
The version rule above stands for this port. The chat port has its own: each runtime version is
qualified by a real run recorded in an ADR, and the Claude Code chat adapter pins one CLI version and
its SHA-256 (ADR-0014).

## Amendment, 2026-11-10 (`r1.s4.w2`): the `--skills` known limit is closed for the Orchestrator

The known limit of 2026-10-31 was that a launched session gets no skill of the method's plugin, so it
cannot arm a run. A seat still acts through its done files and gets no CLI skill. The Orchestrator
chat gets Pulse Factory's CLI skill through its persona, which names the skill's path (since `r1.s3`)
and now the verb `pulse-factory propose`: the model works out the workflow inputs with the skill and
the controller builds, checks and posts the start card. The adapter's launch does not change. No
`--plugin-dir` and no skill install in the operator's `~/.claude` is made. The chat records one real
case for this (case 20 of the Claude Code chat adapter, ADR-0014).
