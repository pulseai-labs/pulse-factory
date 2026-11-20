#!/usr/bin/env bash
# Install Pulse Factory for this user from a clean checkout: the binary, the test bridge, the workflow
# packages, the skill and the systemd user unit. Run it from anywhere. It builds with `cargo install`, so it
# uses the cargo config of the host as it is.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
share="${PF_SHARE_DIR:-$HOME/.local/share/pulse-factory}"
unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"

# The session host installer reads the guard's answer with `jq`: check for it before anything builds.
command -v jq >/dev/null || { echo "jq is not on PATH; install jq, then run this again (nothing is installed)." >&2; exit 1; }

# The upgrade guard. Before this script installs or replaces any file, the new binary checks the store
# (ADR-0003, ADR-0007). With a run active and a migration pending, or with a live session host whose
# protocol the new binary drops, the controller would refuse to start, and every seat's hook would be
# denied. So the script stops here and the old controller keeps running. The build goes into the shared
# target, so the `cargo install` below finds most of it built. A guard that cannot answer stops the install too.
state="${PF_STATE_DIR:-$HOME/.local/state/pulse-factory}"
built="$(nice -n 10 ionice -c3 cargo build --release --locked --manifest-path "$root/Cargo.toml" -p pulse-factory --bin pulse-factory --message-format=json-render-diagnostics)"
guard_bin="$(printf '%s\n' "$built" | grep '"kind":\["bin"\]' | grep '"name":"pulse-factory"' |
  sed -n 's/.*"executable":"\([^"]*\)".*/\1/p' | tail -n 1)"
if [ -z "$guard_bin" ] || [ ! -x "$guard_bin" ]; then
  echo "the build did not name the pulse-factory binary, so the upgrade check cannot run." >&2
  echo "nothing is installed and the old controller keeps running." >&2
  exit 1
fi
if report="$(PF_STATE_DIR="$state" "$guard_bin" store check-upgrade --json)"; then
  guard_rc=0
else
  guard_rc=$?
fi
if [ "$guard_rc" -eq 3 ]; then
  echo "the upgrade must not start. The store check names:" >&2
  printf '%s\n' "$report" >&2
  echo "end or halt the runs, or let the hosts end, then install again." >&2
  echo "nothing is installed and the old controller keeps running." >&2
  exit 3
elif [ "$guard_rc" -ne 0 ]; then
  echo "pulse-factory store check-upgrade failed (exit $guard_rc), so the install stops." >&2
  printf '%s\n' "$report" >&2
  echo "nothing is installed and the old controller keeps running." >&2
  exit 1
fi

# The session host binary goes to `<share>/hosts/<sha256>/pf-host`, and `~/.local/bin/pf-host` points at
# it. The build is niced and goes into the shared target, as the guard's build does. The guard's answer
# names the host binaries that live records run from, and `install-hosts.sh` keeps each of them. It
# removes no cli copy.
built_host="$(nice -n 10 ionice -c3 cargo build --release --locked --manifest-path "$root/Cargo.toml" -p pf-host --message-format=json-render-diagnostics)"
host_bin="$(printf '%s\n' "$built_host" | grep '"kind":\["bin"\]' | grep '"name":"pf-host"' |
  sed -n 's/.*"executable":"\([^"]*\)".*/\1/p' | tail -n 1)"
if [ -z "$host_bin" ] || [ ! -x "$host_bin" ]; then
  echo "the build did not name the pf-host binary, so the session host cannot be installed." >&2
  echo "nothing more is installed; the old controller keeps running." >&2
  exit 1
fi
report_file="$(mktemp)"
trap 'rm -f "$report_file"' EXIT
printf '%s\n' "$report" > "$report_file"
bash "$root/scripts/install-hosts.sh" "$share" "$host_bin" "$report_file"

# The binary goes to ~/.local/bin. The build is niced, because it writes a lot.
nice -n 10 ionice -c3 cargo install --path "$root/crates/pulse-factory" --locked --root "$HOME/.local"

# The test bridge goes beside it, for scripts/demo/ui-pair.sh (d12) and tests. With `--path` it uses the
# workspace's target, so this second install is incremental.
nice -n 10 ionice -c3 cargo install --path "$root/crates/pf-client" --bin pf-client-bridge --locked --root "$HOME/.local"

# The replay of recorded Claude Code sessions goes beside it, so scripts/demo/chat-api.sh (d18) has it
# prebuilt. It is the same incremental install.
nice -n 10 ionice -c3 cargo install --path "$root/crates/pf-adapter-claude" --bin pf-claude-replay --locked --root "$HOME/.local"

# The workflow packages and the skill go to the share directory.
mkdir -p "$share" "$unit_dir"
rm -rf "$share/workflows" "$share/skill"
cp -R "$root/workflows" "$share/workflows"
cp -R "$root/skill" "$share/skill"

# The UI bundle: install-ui.sh builds apps/ui with Node 22 and npm, and replaces "$share/ui".
PF_SHARE_DIR="$share" bash "$root/scripts/install-ui.sh"

# The managed Claude Code copy for the chat, at the pinned version. A failure prints its cause and the
# install goes on: the controller tries again at its next start (ADR-0014).
if ! PF_SHARE_DIR="$share" "$HOME/.local/bin/pulse-factory" chat-runtime install; then
  echo "the chat runtime is not installed (see the cause above)." >&2
  echo "the controller tries again at its next start, or run: pulse-factory chat-runtime install" >&2
fi

cp "$root/deploy/systemd/pulse-factory.service" "$unit_dir/pulse-factory.service"

# A shell with no login session has no XDG_RUNTIME_DIR, and `systemctl --user` then cannot find
# the user bus (ADR-0001).
if [ -z "${XDG_RUNTIME_DIR:-}" ]; then
  XDG_RUNTIME_DIR="/run/user/$(id -u)"
  export XDG_RUNTIME_DIR
fi

systemctl --user daemon-reload
systemctl --user enable --now pulse-factory
systemctl --user try-restart pulse-factory

# The unit must outlive a logout.
linger="$(loginctl show-user "$(id -un)" --property=Linger --value 2>/dev/null || true)"
if [ "$linger" != "yes" ]; then
  echo "lingering is off, so the controller stops when you log out." >&2
  echo "run: loginctl enable-linger $(id -un)" >&2
fi

echo "pulse-factory is installed. Check it with: systemctl --user status pulse-factory"
