#!/usr/bin/env bash
# Install Pulse Factory for this user from a clean checkout: the binary, the workflow packages, the
# skill and the systemd user unit. Run it from anywhere. It builds with `cargo install`, so it
# uses the cargo config of the host as it is.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
share="${PF_SHARE_DIR:-$HOME/.local/share/pulse-factory}"
unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"

# The binary goes to ~/.local/bin. The build is niced, because it writes a lot.
nice -n 10 ionice -c3 cargo install --path "$root/crates/pulse-factory" --locked --root "$HOME/.local"

# The workflow packages and the skill go to the share directory.
mkdir -p "$share" "$unit_dir"
rm -rf "$share/workflows" "$share/skill"
cp -R "$root/workflows" "$share/workflows"
cp -R "$root/skill" "$share/skill"
cp "$root/deploy/systemd/pulse-factory.service" "$unit_dir/pulse-factory.service"

# A shell with no login session has no XDG_RUNTIME_DIR, and `systemctl --user` then cannot find
# the user bus (ADR-0001, the unverified claim).
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
