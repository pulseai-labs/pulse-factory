# Install

This is the clean-checkout path of Release 0's first exit criterion: build and install
`pulse-factory` and its systemd user unit, arm a planned one-round spine, close your session, and
later see each item PR merged into the spine branch.

## What you need

- Linux with `systemd --user`. The unit runs as your user. It needs no root.
- A Rust toolchain (`rust-toolchain.toml` in the repository names the channel), `git` and `jq`.
- Node 22 and `npm`: the install builds the UI (`scripts/install-ui.sh`) with them.
- `gh`, logged in as the account that owns the repository. Seats use your `gh` and git credentials
  (ADR-0005), and `arm` reads the forge through `gh repo view`. The controller itself pushes a base
  branch with the same credentials, where a stage names one with `base_push`.
- OMP at a qualified version, and the method's plugin (for the shipped workflow, `ossify`).
- User lingering, so the controller keeps running after you log out. Check it with
  `loginctl show-user "$USER" -p Linger`. If it prints `Linger=no`, run
  `sudo loginctl enable-linger "$USER"` once.

## Build and install

From a clean checkout:

```bash
scripts/install.sh
```

The script does these steps and no others:

1. `nice -n 10 ionice -c3 cargo install --path crates/pulse-factory --locked --root ~/.local`. The
   binary lands in `~/.local/bin`. **This is a cold build.** It writes a lot to disk, so it runs at low
   priority. Run it once, not in a loop, and never at the same time as another build. It uses your
   `~/.cargo/config.toml` as it is, and it never edits that file.
2. `nice -n 10 ionice -c3 cargo install --path crates/pf-client --bin pf-client-bridge --locked --root
   ~/.local`. This is the test bridge, `pf-client-bridge`, beside the main binary. It is for the UI demo
   (`scripts/demo/ui-pair.sh`, d12) and for tests. The Mac app does not use it, and the controller does
   not need it. It uses the workspace's target, so this second install is incremental.
3. `nice -n 10 ionice -c3 cargo install --path crates/pf-adapter-claude --bin pf-claude-replay --locked
   --root ~/.local`. This is the replay of recorded Claude Code sessions, beside the main binary. It is
   for the chat demo (`scripts/demo/chat-api.sh`, d18) and for tests. The controller does not need it.
   The `pulse-factory` install also puts `pf-stream-reader`, a small client of the chat stream for the
   same demo, in `~/.local/bin`.
4. Copies `workflows/` and `skill/` to `~/.local/share/pulse-factory/` (`$PF_SHARE_DIR` overrides it).
5. Builds the UI bundle (`scripts/install-ui.sh`, Node 22 and `npm`) into `~/.local/share/pulse-factory/ui/`.
6. Runs `pulse-factory chat-runtime install`. It fetches the managed Claude Code copy that the chat
   runs, at the pinned version, from the npm registry (`$PF_CHAT_REGISTRY`, default
   `https://registry.npmjs.org`), checks its SHA-256 against the pin and places it at
   `~/.local/share/pulse-factory/claude/<version>/claude`. The binary is Anthropic's official
   per-platform package: Pulse Factory never ships it. When the fetch fails, the script prints the
   cause and goes on: the controller checks the copy at each start and fetches it again. Until a good
   copy is in place, `session start` is refused with a message that names this verb. The chat never
   runs a `claude` that it finds on `PATH`.
7. Copies `deploy/systemd/pulse-factory.service` to `~/.config/systemd/user/`. The unit states
   `KillMode=control-group`, so a stop of the controller also ends any process left in its cgroup,
   such as a background command that a chat started.
8. Sets `XDG_RUNTIME_DIR` to `/run/user/<uid>` when it is unset, so `systemctl --user` finds the user
   bus. Then it runs `systemctl --user daemon-reload`, `enable --now pulse-factory` and
   `try-restart pulse-factory`.

Check the controller:

```bash
systemctl --user status pulse-factory
pulse-factory status        # exits 1 with a clear message when no run exists yet
```

The unit sets `PATH` to `~/.local/bin`, `~/.cargo/bin` and the system directories. Seats inherit it,
so `gh`, `git` and `omp` must be on one of those paths.

**Install with no active run.** Each release of the binary can migrate the store (migration 0004
adds the `devices` table, and migration 0006 adds the session's runtime options). The controller refuses to migrate while a run is active, so it does not
start until no run is active (ADR-0003, ADR-0007). End or halt every run, then install.

## Turn on the network API

The controller serves a network API over TLS, so a paired device on your tailnet can call the project
verbs (ADR-0012, `docs/contracts/api.md`). The API is off until you set an address. Nothing listens
and no key exists until then.

1. Write the address to the controller's environment file. Use the address of this host on your
   tailnet (draco-desk uses `100.90.203.21`) and a free port:

   ```bash
   mkdir -p ~/.config/pulse-factory
   echo 'PF_API_LISTEN=100.90.203.21:7443' > ~/.config/pulse-factory/controller.env
   ```

   The unit reads the file through `EnvironmentFile=-%h/.config/pulse-factory/controller.env`. The
   `controller --api-listen <addr>` flag does the same and wins over the variable.
2. Restart the unit so it reads the file: `systemctl --user restart pulse-factory`. On the first
   start with the API on, the controller makes an ECDSA P-256 key and a self-signed certificate in
   `~/.local/state/pulse-factory/api/` (`key.pem`, mode 0600, and `cert.pem`).
3. Check it. `pulse-factory api status` prints `listening on <addr>` and the SPKI hash of the key. It
   prints `waiting for <addr>` when the address is not on this host yet, for example before Tailscale
   starts. The controller tries again every 5 seconds and keeps serving the socket meanwhile.
4. Pair a device: run `pulse-factory device pair` on this host (over ssh, if you are elsewhere). It
   prints one code that works once, for 10 minutes. Type the code and a device name into the device.
   The device checks the key's SPKI hash against the code before it sends the code.
5. See and cut off devices with `pulse-factory device list` and `pulse-factory device revoke <id>`.
   A revoked device's next call is refused.

To turn the API off, remove the line and restart the unit. The key and the paired devices stay. If
the key files are deleted, the next start makes a new key: every paired device then fails closed.
Revoke them all and pair again.

## Set up the chat

A paired device and the CLI can chat with a project's Orchestrator: `pulse-factory session start
--project P-1` (`docs/contracts/cli.md`, `session`). The chat runs the managed Claude Code copy that
`pulse-factory chat-runtime install` places. The controller reads these settings from the same
`~/.config/pulse-factory/controller.env` as `PF_API_LISTEN`, one `NAME=value` per line:

- `PF_CHAT_CLAUDE_BIN=<path>` runs another Claude Code binary instead of the managed copy. The
  controller prints `the chat runs an unqualified Claude Code binary: <path>` at start, and every
  session list and every `chat open` line marks the process `unqualified`. Leave it unset for the
  managed copy.
- `PF_CHAT_MAX_LIVE=<n>` caps the live chat processes, 1 to 64, default 4. A chat process takes 205 to
  537 MB. A bad value prints one line and keeps 4.
- `PF_AGENT_ENV_<AGENT>=NAME=value NAME2=value2` sets environment variables for the chat processes of
  one agent, as server-side settings. The agent ID is upper-cased and each other character becomes
  `_`, so the built-in Orchestrator is `PF_AGENT_ENV_ORCHESTRATOR`. A name that the adapter reserves
  (`ANTHROPIC_*`, `PF_*` and the like) is refused at the open, and the refusal names it.
- `PF_SERVER_NAME=<name>` names this server in every API answer (`pf-server-name`). The default is the
  host name. It is 1 to 64 characters of `A-Z a-z 0-9 . _ -`.
- `PF_CHAT_REGISTRY=<url>` is the registry that `chat-runtime install` and the start-time check
  fetch from. The default is `https://registry.npmjs.org`.

A chat process opens only on `session start` or on a `session send` to a session with no process. The
controller releases it after 30 idle minutes and stops every live chat on `pulse-factory halt` and
when the controller stops.

## Install the Mac app

The Mac app is a Tauri 2 shell over the same UI bundle. Its Rust side makes the pinned TLS call and
keeps the device token in the macOS Keychain (ADR-0012, ADR-0013). GitHub's `macos-latest` runner
builds it for Apple silicon (`aarch64-apple-darwin`) and signs it ad hoc. On the MacBook you sign it
again with your own Apple Development identity (step 4), so the Keychain stops asking at each build. It
is still not notarized (`macos-signing` waits for Release 2). Do these steps on the
MacBook unless a step says draco-desk.

Before you start, on draco-desk:

- **No active run.** Install the controller binary that serves the API only when no run is active.
  The controller refuses to migrate while a run is active (see "Build and install"). End or halt every
  run first.
- **The API is on, on the tailnet address.** `PF_API_LISTEN` is draco-desk's tailnet address (see "Turn
  on the network API"), and `pulse-factory api status` prints `listening on <addr>`. The MacBook must be
  on the same tailnet.

On the MacBook, with `gh` logged in:

1. Find the newest green `macos-app` run on `main`:

   ```bash
   gh run list -R pulseai-labs/pulse-factory --workflow macos-app --branch main --status success -L 1
   ```

   The first column of the line is the run's ID.
2. Download the artifact, unpack it and move the app. Replace `<id>` with the run's ID:

   ```bash
   gh run download <id> -R pulseai-labs/pulse-factory -n pulse-factory-macos-app
   ditto -x -k pulse-factory-macos-app.zip .
   mv "Pulse Factory.app" /Applications/
   ```
3. Check the quarantine flag:

   ```bash
   xattr -p com.apple.quarantine "/Applications/Pulse Factory.app"
   ```

   A file that `gh` downloads carries no quarantine flag, and `xattr` then prints an error such as
   `No such xattr: com.apple.quarantine`. That is the good answer. A copy that came through a browser
   carries the flag. Gatekeeper refuses an ad-hoc-signed app that carries it, so clear it:

   ```bash
   xattr -dr com.apple.quarantine "/Applications/Pulse Factory.app"
   ```
4. Sign the app with your Apple Development identity, from a checkout of this repo on the MacBook:

   ```bash
   PF_SIGNING_IDENTITY="Apple Development" scripts/sign-mac-app.sh
   ```

   The ad-hoc signature changes with every build, and macOS keys the Keychain item that holds the
   device token to it, so an ad-hoc build asks for your login password at each new build. A signature
   from an Apple-issued identity keys the item to your team ID instead. The script refuses an identity
   that Apple did not issue, such as a self-signed one, before it changes the app. If the Keychain
   already holds a token from an ad-hoc build, the first launch of the first build signed this way asks
   once: type the password and press "Always Allow". A new pairing asks nothing. Later builds open with
   no prompt. When more than one identity matches the name, pass the full name or the SHA-1 hash that
   `security find-identity` prints. A free Apple
   Development certificate is enough (Xcode: Settings > Accounts > Manage Certificates), and
   `security find-identity -v -p codesigning` lists the ones you have. Do this again after every new
   download. The app stays unnotarized: Developer ID signing and notarization are still the
   `macos-signing` deferral.
5. Open `Pulse Factory` from `/Applications`. It shows the pair screen (3a).
6. On draco-desk, over ssh, make a pairing code: `pulse-factory device pair`. Paste the code into the
   pair screen and give the device a name. The app checks the key's SPKI hash against the code before
   it sends the code, then keeps the token in the login Keychain (service `pulse-factory.device-token`).
7. On draco-desk, check the pairing: `pulse-factory device list` shows exactly one row for the
   MacBook. Add a project in the app and see it listed.

If the app shows `server key changed: re-pair`, the controller's key is not the one the app pinned.
Run `pulse-factory device revoke <id>` for the old device on draco-desk and pair again. If it shows
that this device is no longer paired, the device was revoked or idle for 30 days: pair again.

`scripts/demo/community-edition.sh` (d14) says whether the public repo's CI passed on the checked-out
commit, with its `Smoke` step green. It asks GitHub once, never waits and needs the network: with no
run for the commit it says to push, and with a run that has not finished it says so.

## Try it with no model and no spend

The paper env runs the whole journey with scripts in place of seats, against a bare git repository:

```bash
PF_BIN=pulse-factory scripts/demo/golden-journey-a.sh
PF_BIN=pulse-factory scripts/demo/golden-journey-b.sh
PF_BIN=pulse-factory scripts/demo/arm-dry-run.sh
```

`golden-journey-a.sh` starts its own controller in a temporary state directory, so it works with or
without the installed unit. It builds nothing. `golden-journey-b.sh` is the same journey with the
claim: a gaps request is answered, the spine PR waits for the ack, and the ack merges it into the
base branch. `arm-dry-run.sh` dry-runs the shipped
`workflows/ossify-spine` against stubs and changes nothing.

## Arm a spine

From your own session (the skill in `~/.local/share/pulse-factory/skill/SKILL.md` says how a session
works out the values):

```bash
pulse-factory arm <spine> --workflow ossify-spine --ledger <ledger file> \
  --plugin-root <plugin cache> --param repo=<path> --param ... --dry-run
```

The shipped workflow also declares `base_branch`, the branch the spine PR merges into. The skill
reads it with `oss spine_base_get <spine> <repo>`. Read the summary: items, roles and models, caps,
the most launches, that item PRs merge into the spine branch on green checks with no ack, and that
the spine PR merges into the base branch only on the claim's ack. Then run the same command with
`--yes` in place of `--dry-run`. You can close your session. Later:

```bash
pulse-factory status          # every unit and every session, with tokens and cost
pulse-factory halt --reason "stop for now"    # running seats finish; nothing new launches or merges
```

A parked unit has an open request. Read it with `pulse-factory requests` and `pulse-factory show <R-n>`,
make a claim with `pulse-factory claim`, then resume the unit in the same run with
`pulse-factory answer <R-n> --rev <k> --text "<ruling>"` or `pulse-factory clear <run> <item>`.

When every item has merged, a spine-close seat opens the spine PR and the controller opens a
`merge_ack` request. `pulse-factory show <R-n>` names the PR, the base branch and the head SHA.
`pulse-factory ack <R-n> --rev <k>` merges the PR with that SHA. Then run `/ossify:close <spine>`
for the second pass, in your own session or in the project's Orchestrator chat. The run ends when the method records the close.

## Remove

```bash
systemctl --user disable --now pulse-factory
rm ~/.config/systemd/user/pulse-factory.service ~/.local/bin/pulse-factory
rm -r ~/.local/share/pulse-factory
```

The state directory (`~/.local/state/pulse-factory`) holds the run store, queue and ledgers. Keep it
until you have read what you need from it.
