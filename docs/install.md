# Install

This is the clean-checkout path of Release 0's first exit criterion: build and install
`pulse-factory` and its systemd user unit, arm a planned one-round spine, close your session, and
later see each item PR merged into the spine branch.

## What you need

- Linux with `systemd --user`. The unit runs as your user. It needs no root.
- A Rust toolchain (`rust-toolchain.toml` in the repository names the channel), `git` and `jq`.
- `gh`, logged in as the account that owns the repository. Seats use your `gh` and git credentials
  (ADR-0005), and `arm` reads the forge through `gh repo view`.
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
2. Copies `workflows/` and `skill/` to `~/.local/share/pulse-factory/` (`$PF_SHARE_DIR` overrides it).
3. Copies `deploy/systemd/pulse-factory.service` to `~/.config/systemd/user/`.
4. Sets `XDG_RUNTIME_DIR` to `/run/user/<uid>` when it is unset, so `systemctl --user` finds the user
   bus. Then it runs `systemctl --user daemon-reload`, `enable --now pulse-factory` and
   `try-restart pulse-factory`.

Check the controller:

```bash
systemctl --user status pulse-factory
pulse-factory status        # exits 1 with a clear message when no run exists yet
```

The unit sets `PATH` to `~/.local/bin`, `~/.cargo/bin` and the system directories. Seats inherit it,
so `gh`, `git` and `omp` must be on one of those paths.

## Try it with no model and no spend

The paper env runs the whole journey with scripts in place of seats, against a bare git repository:

```bash
PF_BIN=pulse-factory scripts/demo/golden-journey-a.sh
PF_BIN=pulse-factory scripts/demo/arm-dry-run.sh
```

`golden-journey-a.sh` starts its own controller in a temporary state directory, so it works with or
without the installed unit. It builds nothing. `arm-dry-run.sh` dry-runs the shipped
`workflows/ossify-spine` against stubs and changes nothing.

## Arm a spine

From your own session (the skill in `~/.local/share/pulse-factory/skill/SKILL.md` says how a session
works out the values):

```bash
pulse-factory arm <spine> --workflow ossify-spine --ledger <ledger file> \
  --plugin-root <plugin cache> --param repo=<path> --param ... --dry-run
```

Read the summary: items, roles and models, caps, the most launches, and that item PRs merge into the
spine branch on green checks with no ack. Then run the same command with `--yes` in place of
`--dry-run`. You can close your session. Later:

```bash
pulse-factory status          # every unit and every session, with tokens and cost
pulse-factory halt --reason "stop for now"    # running seats finish; nothing new launches or merges
```

A parked unit needs a new run until `r0.s2` (`arm --replace <run>`).

## Remove

```bash
systemctl --user disable --now pulse-factory
rm ~/.config/systemd/user/pulse-factory.service ~/.local/bin/pulse-factory
rm -r ~/.local/share/pulse-factory
```

The state directory (`~/.local/state/pulse-factory`) holds the run store, queue and ledgers. Keep it
until you have read what you need from it.
