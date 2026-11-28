#!/usr/bin/env bash
# dashboard/setup/run.sh
# Starts the dashboard service: the binary install.sh built. The one entry
# point for the systemd unit, server-manager (server.toml) and a hand start.
# Arguments are passed on to the binary (--port, --root).
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)" # -> the repository
bin="$root/dashboard/server/target/release/xrfd-dashboard"
if [[ ! -x "$bin" ]]; then
  echo "run.sh: no built binary at $bin; run dashboard/setup/install.sh first" >&2
  exit 1
fi
# server.toml, data/, log/ and the built page are found from the repository.
cd "$root"
# exec: the binary replaces this shell, so systemd's SIGTERM reaches it itself.
exec "$bin" "$@"
