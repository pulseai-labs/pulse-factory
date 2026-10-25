# ADR-0011 — An agent adapter port, with OMP as the first adapter

- **Status:** Proposed
- **Date:** 2026-10-25

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
- Execution units are interactive terminal sessions. There is no headless or API substitute.
- Release 0 runs cheap seats only, in OMP, as the r3.s2 pilot did. The pilot drove OMP through T3
  Code's injected extension, and Pulse Factory depends on no T3 Code. So it was not known whether
  Pulse Factory could get these signals from an interactive OMP session by itself. The spike below
  settled it.

## Decision
- `pf-engine` defines an agent adapter port with the nine duties.
- `crates/pf-adapter-omp` is the first adapter. It launches OMP as an interactive TUI in a PTY, with:
  - `--no-extensions`, `--no-skills` and `--no-rules`, so discovered operator extensions, skills and
    rules do not load. Only the `--hook` path Pulse Factory passes is loaded. Whether OMP still loads
    MCP servers from its own config is unverified. The adapter must check it, or launch with an
    isolated `--profile`, before relying on "no wake tool reaches the seat";
  - `--approval-mode yolo`, which turns prompts off;
  - a `--tools` list that leaves out `ask`, and leaves out `task` unless the adapter tracks subagents;
  - `--hook` loading Pulse Factory's own OMP extension.
- The extension uses only OMP's public extension API:
  - **turn end** is `agent_end` without `willContinue === true`;
  - **delivery** is `pi.sendUserMessage` once `ctx.isIdle()` is true;
  - **deny** is a `tool_call` handler that returns `{ block: true, reason }`, with the reason taken
    from `pulse-factory hook`'s rule decision;
  - **usage** is the `usage` on each assistant `message_end`.
- The adapter reads `omp --version` at launch, and refuses a version not yet qualified. 18.11.3 is
  qualified by the spike.
- Duties 4, 6 and 9 are designed in the skeleton spine. The spike did not test them.

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

### Unverified claims
- Whether OMP loads MCP servers from its config when launched with the flags above.
- Interrupting a session (duty 4).
- Answering launch dialogs (duty 6).
- Handing off to a successor session (duty 9).
- Turn end while OMP subagents run.

The skeleton spine's adapter item verifies these.

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
