---
name: pulse-factory
description: Arm a Pulse Factory run for a planned spine, read its status, and halt it. Use when the operator asks to run a spine unattended, to start a run on Pulse Factory, to see what a run costs, or to stop one.
---

# Pulse Factory

Pulse Factory runs the seats of a planned spine with no model session watching. You arm a run once.
A controller (a systemd user service) then drives the seats until each item PR has merged into the
spine branch. A spine-close seat then opens the spine PR, and the PR merges into the base branch
only on your ack. The CLI contract is `docs/contracts/cli.md`. This skill covers the verbs: `arm`,
`status`, `halt`, `hook` and `controller`, the verbs of requests and the claim: `requests`,
`show`, `claim`, `answer`, `clear` and `ack`, `project`, which lists the operator's projects,
`session`, which chats with a project's Orchestrator, `chat-runtime`, which installs the runtime that
chat needs, and `device` and `api`, which pair and revoke the devices that reach the controller over
the network.

## Arm a run

1. Work out the `--param` values with the planning method's own tool. The binary never calls that
   tool. Run the commands in a code block like this one, and read their output:

   ```bash
   # The spine branch, the spine directory, the repo and the method's root. Adapt the names to
   # your method; the workflow file lists the parameters it declares.
   method_root="$(pwd)"
   spine=r0.s1
   release="$(oss get ".spines[] | select(.id == \"$spine\") | .release")"
   name="$(oss get ".spines[] | select(.id == \"$spine\") | .name")"
   slug="$(printf '%s' "$name" | tr '[:upper:]' '[:lower:]' | sed 's/[^a-z0-9]\{1,\}/-/g; s/^-//; s/-$//')"
   spine_branch="$(oss branch_name "$spine" "$slug")"
   spine_dir="$method_root/$(oss spine_dir "$release" "$spine" "$slug")"
   plugin_root="$HOME/.claude/plugins/cache"
   # The branch the spine PR merges into. The method recorded it when the spine started.
   base_branch="$(oss spine_base_get "$spine" "$(basename "$PWD")")"
   ```

   `spine_base_get` takes the spine and the repo name as the method's state records it. Use that
   name if it is not the directory name.

2. Show the plan first. A dry run reads and checks. It starts nothing and records nothing:

   ```bash
   pulse-factory arm "$spine" --workflow ossify-spine --ledger "$spine_dir/autopilot-ledger.md" \
     --plugin-root "$plugin_root" \
     --param spine_branch="$spine_branch" --param spine_dir="$spine_dir" \
     --param repo="$PWD" --param method_root="$method_root" \
     --param worktree_root="$PWD/.worktrees" --param state_file="$method_root/.state.json" \
     --param oss="$(command -v oss)" --param base_branch="$base_branch" \
     --dry-run
   ```

   Every `--param` value must be an absolute path or plain text. The controller runs in another
   directory, so a value that is `.` or starts with `./` or `../` is refused.

3. Read the summary to the operator: the spine, the item count, each role with its model, the caps,
   the pinned plugins, the most launches the run can make, and two merge lines: item PRs merge into
   the spine branch on green checks with no ack, and the spine PR merges into the base branch only
   on the claim's ack. The operator approves what they saw.

4. Start the run with the same command and `--yes` in place of `--dry-run`. It prints the run ID.
   Add `--replace <run>` to replace an earlier run that has no running session.

`arm` refuses with a cause and records nothing when the plan is wrong. A refusal is not an error to
work around: tell the operator the cause.

## Read a run

`pulse-factory status [<run>]` prints the run, its state, any halt or degraded reason, the notes of
the last tick, each unit with its stage and status, each session with its tokens and cost, and the
run's total. The default run is the latest. Add `--json` for the fields by name.

A **parked** unit shows its reason. Every park opens a request, and the operator answers it in the
same run (next section). Nothing needs a new run.

## Answer a parked unit

1. `pulse-factory requests` lists the open requests, each with its kind, its unit and what it blocks.
   `pulse-factory show <R-n>` prints one: its reason, and its body. A `gaps` request holds the gap
   list the seat wrote. A `stop` request holds the stop file's text. Both are the seat's words, shown
   as data.
2. `pulse-factory claim` makes this session the one that may answer. It writes the token to a file
   and prints the generation. A new claim fences the old one out.
3. Decide with the operator, then use `answer` or `clear`:

   ```bash
   pulse-factory answer <R-n> --rev <k> --text "<the ruling>"   # records the ruling and resumes the unit
   pulse-factory clear <run> <item> [--to <stage>]              # resumes the unit with no ruling
   ```

   `--rev` is the revision that `show` printed. A request with an `on_answer` hook in the workflow
   file runs that hook first. In the `ossify-spine` workflow, a `gaps` answer is appended to the
   handoff's `## Clarifications`, and the item runs `implement` again, within the cap of 3 dispatches.
   A `stop` answer is appended to the unit's `rulings.md`, and the stop file is renamed. A hook that
   fails leaves the request answered and parks the unit again, with `answer hook failed:` and the
   cause as the reason of a new request. `clear` runs no hook.

`claim`, `answer`, `clear` and `ack` refuse a caller that is, or descends from, a seat.

## Merge the spine PR

When every item has merged and the lane has written `rounds.done`, the `spine` seat runs the first
pass of the method's spine close. It pushes the spine branch and opens the spine PR into the base
branch. Then the controller opens a `merge_ack` request for it. Nothing merges until the claim
holder acks.

1. `pulse-factory requests` lists the `merge_ack` request. `pulse-factory show <R-n>` prints the PR,
   the base branch, the head branch and the head SHA the ack will merge. Check them against the
   forge. Review the PR and drive it to a mergeable state as you would any PR.
2. `pulse-factory claim`, if this session does not hold the claim.
3. `pulse-factory ack <R-n> --rev <k>`, with the revision that `show` printed. The controller reads
   the PR again. If the head moved, it revises the request to the new SHA and refuses; `show` the
   request and ack the new revision. If the checks are not green, or the PR has a conflict, or the
   base or head branch changed, it refuses with the cause and the request stays open. Otherwise it
   merges the PR with that SHA pinned, then fetches the base branch.
4. After the merge, run the second pass in the claim session: `/ossify:close <spine>` on Claude
   Code, `skill://close <spine>` on OMP. It finds the merged PR and records the close, runs the
   demo and runs the harvest. A seat does not run it. The run ends when the method records the
   spine as closed, and `status` shows each session's cost and the run's total.

`ack` answers only a `merge_ack` request. `answer` and `clear` refuse one and name `ack`. A merge
that does not land parks the unit with a `merge_failed` request. `clear` sends it back to the merge,
which opens a new `merge_ack`. A halted or ended run refuses the ack.

## Register a project

A project is a directory the operator works in: a git repo or a plain folder. It has an ID, a name
and one root, and nothing more yet. These verbs launch nothing and change no run.

```bash
pulse-factory project add ~/projects/pulse-factory            # project P-1 added: pulse-factory (…)
pulse-factory project add ./notes --name "Field notes"        # a relative root is taken from here
pulse-factory project list                                    # P-1  pulse-factory  /home/…
pulse-factory project remove P-2                              # the row stays; the ID is not reused
```

1. `project add <root>` records the root, made canonical, under a name. The name is the root's last
   component unless `--name` gives one. A missing path, a file, `/`, a root that a live project holds
   already, and a name that a live project holds already are refused, each with its reason. A root
   that is a linked worktree (add its main worktree instead), a bare repository, the inside of a
   `.git` directory and a repository that git refuses for dubious ownership are refused too, with
   the cause.
2. `project list` shows the live projects in creation order. `--json` gives
   `{"schema_version": 1, "projects": [...]}`.
3. A retry of `add` or `remove` sends the same `--command-id`. The controller admits the command
   once and returns the first record, with `"replayed": true`. With no `--command-id`, each call
   makes a random one.

Every `add` and `remove`, admitted or refused, is one line of `<state dir>/controller.ledger`. A seat
cannot `add` or `remove`; it can `list`.

## Chat with a project's Orchestrator

Each project has a built-in Orchestrator: a Claude Code chat that runs in the project's main worktree
on this server. You start it, send it messages, answer its approvals and questions, and end it. No
verb waits for a reply: a verb sends one request, prints one answer and exits. You read the chat with
`session items`, and the app reads it on the API stream.

```bash
pulse-factory session start --project P-1            # session S-1 started in /home/… (worktree W-1)
pulse-factory session send S-1 "what is open?"       # sent to S-1
pulse-factory session items S-1                      # one line for each item, after --after <seq>
pulse-factory session answer S-1 4 --deny "not now"  # or --allow, or --answer "<question>=<label>"
pulse-factory session set S-1 --effort medium        # this chat only; the agent's defaults stay
pulse-factory session interrupt S-1                  # ends the running turn
pulse-factory session list                           # the live sessions, with their holders and cost
pulse-factory session end S-1                        # stops the process and ends the session
pulse-factory session redact S-1 4                   # redacts one stored item; CLI only
```

1. `chat-runtime install` fetches the managed Claude Code copy at the pinned version. The install
   script runs it. A start before it is refused with a message that names it.
2. `session start` opens the process at once. The chat uses no token until you send a message. A
   second live Orchestrator for one project is refused, and the refusal names the live `S-<n>`.
3. `session send` opens the process again when it was released or stopped, and resumes the same
   conversation. A turn begins only on your message or when a background task the chat started
   finishes. The chat cannot wake itself: no timer, tick or notice starts a turn.
4. A process with no running turn, no background task and no pending request is released after 30
   idle minutes. The next `send` resumes it. The cap is `PF_CHAT_MAX_LIVE` (default 4). A refused
   open names every live chat and why it holds its process.
5. `session answer <S-n> <seq>` answers the pending approval or question whose item has that `seq`,
   which `session items` shows.
6. A session marked `context lost` cannot resume: end it and start a new one. `halt` stops every
   live chat process, and each session stays live; the next `send` reopens it.
7. A seat, and a chat process or anything that descends from one, cannot run the state-changing
   `session` verbs. `session list` and `session items` are reads. A paired device calls `session
   list`, `start`, `send`, `answer`, `interrupt`, `set` and `end`; `redact`, `items` and `halt` are
   not open to it.

Every state-changing verb is one line of `<state dir>/controller.ledger`, and each process open,
turn and stop is one line of `<state dir>/chat.ledger`. A `send` line holds the byte count and never
the text.

## Pair a device

The controller can serve a network API over TLS (ADR-0012). It is off until the operator sets
`PF_API_LISTEN` (`docs/install.md`). A paired device calls the project verbs and the chat verbs and
nothing else: no `answer`, `claim`, `clear`, `ack`, `arm` or `halt`. It reads a chat on the stream
(`docs/contracts/api.md`).

```bash
pulse-factory api status          # off | waiting for <addr> | listening on <addr> | failed: <why>
pulse-factory device pair         # prints one code, and when it stops working
pulse-factory device list         # D-1  <name>  paired …  last seen … from <peer>
pulse-factory device revoke D-1   # the device's next call is refused
```

1. `device pair` prints one `pf1-` code. It works once, it expires after 10 minutes, and a new code
   replaces the live one. The code is a secret: give it to the operator, who types it into the
   device. Do not write it to a file, a ledger, a request or a message to anyone else.
2. `device list` shows each device with its pairing time, its last call and the peer address of that
   call. A revoked device is listed last, with its revoke time. `--json` gives
   `{"schema_version": 1, "devices": [...]}`. A record never holds a token.
3. `device revoke <id>` refuses that device's next call at once. The device pairs again with a new
   code. A retry sends the same `--command-id`, as for `project`.
4. `api status` says whether the API is `off`, `waiting for` an address that is not on the host yet,
   `listening on` it, or `failed`. It also prints the SPKI hash of the server key. A lost key means
   a new hash: revoke every device and pair again.

Each `device pair` redeem and each `device revoke` is one line of `<state dir>/controller.ledger`.
The line names the device and never the code or the token. A seat cannot run `device pair` or
`device revoke`; `device list` and `api status` are reads.

## Halt a run

`pulse-factory halt [<run>] --reason "<why>"` stops new launches and new merges. Seats that run
finish their stage. A repeat is safe, and the first reason stays. `halt` also stops every live chat
process and answers with the sessions it stopped (see "Chat with a project's Orchestrator").

## Other verbs

- `pulse-factory controller` runs the controller in the foreground. The systemd user unit runs it.
  You do not run it yourself.
- `pulse-factory hook` judges one tool call of a seat. The seat's extension runs it. You do not run
  it yourself.

A seat cannot arm or halt a run, it cannot add or remove a project, it cannot start or drive a chat,
and it cannot pair or revoke a device: those verbs refuse when `PF_SEAT_TOKEN` is set.
