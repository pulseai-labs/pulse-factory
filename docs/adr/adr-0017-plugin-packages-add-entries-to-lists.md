# ADR-0017 — Plugin packages add entries to lists

- **Status:** Proposed, 2026-11-16; ruled by the operator the same day. It takes effect when the code
  that builds it lands, and that work's close marks it Accepted.
- **Date:** 2026-11-16

## Context
- The engine is generic (ADR-0002), and a workflow is data (ADR-0004). Today the only workflow ships
  in this repo, so a method's workflow, roles and rules change only by a change to Pulse Factory.
- A method should arrive as a package that Pulse Factory installs, and a package must not be able to
  weaken the controls that keep a run safe: the claim, admission, the ledger and the seat floors
  (ADR-0005, ADR-0006, ADR-0009).
- Sessions run in three harnesses with three plugin formats. One skill must reach each of them.

## Decision

**A plugin adds entries to lists. It never changes the core.** If a plugin needs a core change, the
core lacks a list.

**Depth 1, data only.** A plugin may add:
- workflows and workflow templates;
- routines (a trigger, a filter and a workflow), reserved: none runs until routines exist;
- agents, with instructions and a brain. Their authority is a rule set from the plugin's own list and
  no grant;
- skills for sessions;
- request kinds: fields from a closed set of field types, a card from a closed set of card layouts,
  and the answers allowed. A plugin's request kind is always above the line (ADR-0006, amendment of
  2026-11-16);
- defaults for its own settings keys, named `plugin.<id>.<key>`, typed from a closed set (ADR-0016,
  amendment of 2026-11-16). A plugin never sets a core key;
- rule sets for its workflow roles, and merge rules chosen from a fixed list. The adapter still adds
  the deny list (ADR-0004), and no merge rule merges into a default branch without the claim's ack
  (ADR-0005).

**Depth 2, code outside the controller** (runtimes, forges, plan readers, event sources, notice
channels) is not open yet. The first-party adapters stay crates in this repo. The protocol for an
outside adapter is decided when the first one is planned. A plan reader is a source behind the
unit-source port, not a port of its own. **Depth 3**, screens, is not open.

**Closed to every plugin:** the store, admission, the claim, the clear grant and its list, the ledger,
the seat floors, the host protocol (ADR-0001, amendment of 2026-11-16) and the core settings keys.

**The package.**
- A directory, or an archive of one, with a strict manifest, `pulse-plugin.yaml`:
  - `version: 1` is the manifest format. Unknown fields and unknown versions are refused, as for the
    workflow file;
  - `id` (`<owner>/<name>`) and `release` (semver);
  - `requires.pulse_factory`, the lowest Pulse Factory version it runs on;
  - `provides`: one list per kind above, each a set of relative paths inside the package.
- At install, Pulse Factory computes a content-hash manifest of every file. A run pins it (ADR-0007),
  and a launch record names it (ADR-0003, amendment of 2026-11-16).
- An installed release is immutable, in Pulse Factory's own share directory, one directory per ID and
  release. Pulse Factory never writes into a harness's own config directory.
- A workflow file still declares the harness plugins its seats need, each with a minimum version
  (ADR-0004). A Pulse Factory plugin does not install harness plugins.

**One skill, three harnesses.** A skill is stored once, in the open `SKILL.md` format. Each runtime
adapter delivers a session's skill set in its harness's own way and returns what it delivered for the
launch record (ADR-0011, amendment of 2026-11-16). No skill is installed in a harness's home
directory. A skill is data: no delivered skill may run shell when it loads, so the adapter turns off a
harness's inline skill shell. Without that, a skill's shell would run outside the seat's hook.

**Install, upgrade, remove.**
- `pulse-factory plugin add <path>` parses the package and shows what it adds: each list and entry,
  each rule set's allow list, each request kind and each settings default. Installing is a claim act:
  the operator confirms on the CLI or with a device confirm code. A seat or a chat cannot install.
- A new release installs beside the old one. Running runs and sessions keep the release they pinned.
  New arms and launches use the active release.
- A remove is refused while a live run or launch record pins the release.
- Enabling a plugin for a project is a settings key, `plugins.enabled`: a policy, not a floor.

Rejected alternatives:
- **Plugins as code inside the controller.** A plugin could then change the core and pass a floor.
- **One plugin format per harness.** A method would ship three copies of each skill.
- **A depth-2 protocol now.** No outside runtime or forge is planned; a protocol designed with no user
  would be guessed.

## Consequences
- The first-party method workflow can move out of this repo into a package, with no engine change.
- A plugin's change never touches a running session or run.
- Pulse Factory owns how a skill reaches each harness, so a new harness needs one adapter duty, not a
  change to every plugin.
