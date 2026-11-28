#!/usr/bin/env bash
# dashboard/setup/install.sh
# Builds what run.sh starts and what it serves: the Rust binary and the Svelte
# page. server-manager's update runs it after each pull; a hand run is the
# first install. Also installs the Python backend's packages while that
# backend is in the tree, and (optionally) adds firewall rules.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/.." && pwd)" # -> dashboard/

command -v cargo >/dev/null || {
  echo "ERROR: 'cargo' not found in PATH. Install Rust first: https://rustup.rs" >&2
  exit 1
}
command -v uv >/dev/null || {
  echo "ERROR: 'uv' not found in PATH. Install it first:" >&2
  echo "  curl -LsSf https://astral.sh/uv/install.sh | sh" >&2
  exit 1
}
for exe in node npm; do
  command -v "$exe" >/dev/null || {
    echo "ERROR: '$exe' not found in PATH (needed for the frontend build). Install it and re-run." >&2
    exit 1
  }
done

# Backend: the binary run.sh starts, built from the versions Cargo.lock pins.
(cd "$root/server" && cargo build --release --locked)

# The Python backend's packages: it is started by hand beside the service
# while the two are compared, and by the Windows scripts.
(cd "$root" && uv sync)

# Frontend: the page the binary serves from frontend/dist.
(cd "$root/frontend" && npm ci && npm run build)

# Firewall: optional, no-op unless ufw is active (see firewall.sh).
"$here/firewall.sh" || true

echo "Setup complete. Start with: dashboard/setup/run.sh  (or: systemctl enable --now xr-freed-to-udp)"
