# XRFD Dashboard (Rust + Svelte)

Central web dashboard for the XR-FreeD device. A Rust binary, `xrfd-dashboard`
(`dashboard/server/`), runs a UDP bridge thread that receives the device's diag
broadcast (UDP 50999) and relays control commands (UDP 50998); it serves a
Svelte/Vite frontend with live updates over SSE.

The Python backend under `dashboard/backend/` is what it replaces. It stays in the
tree while the two are compared (see *The Python backend as a reference*), and is what
the Windows scripts start.

Runs as one always-on service on a PC that is **on the device's LAN segment**
(broadcasts do not cross routers/subnets). Other PCs/tablets browse to it over the LAN.

Primary target is **Linux** (`server-manager` supervises it via `server.toml`); the
**Windows** scripts are kept as a fallback.

## Install (once)

### Linux (primary)
```
cd dashboard
bash setup/install.sh
```
Builds the binary (`cargo build --release --locked`) and the frontend, installs the Python
backend's packages via `uv`, and — only if `ufw` is active — adds inbound rules for
UDP 50999 / TCP 10000.

Prerequisites, all **build time only**; the running service is the one binary and the built page:
- Rust — `rustup` (https://rustup.rs); the floor is `rust-version` in `dashboard/server/Cargo.toml`
- Node.js — e.g. via fnm/nvm
- `uv` — `curl -LsSf https://astral.sh/uv/install.sh | sh`, while the Python backend is in the tree

### Windows (fallback, as Administrator)
Installs and starts the Python backend, not the binary.
```
cd dashboard
powershell -ExecutionPolicy Bypass -File setup\install.ps1
```
Administrator is required for the firewall rules — without them the diag broadcast is
blocked on the Public profile and the dashboard shows no device. (On Linux a default
server has no firewall, so the rule is usually unnecessary — `firewall.sh` is a no-op then.)

Prerequisites: `uv` and Node.js on PATH (uv manages Python 3.14 itself):
- `uv` — `powershell -ExecutionPolicy ByPass -c "irm https://astral.sh/uv/install.ps1 | iex"` (or `winget install astral-sh.uv`)

## Run

### Linux
```
bash dashboard/setup/run.sh
```
Or as a managed service (autostart + restart-on-failure):
```
# edit WorkingDirectory in setup/xr-freed-to-udp.service to the deploy path, then:
sudo cp setup/xr-freed-to-udp.service /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl enable --now xr-freed-to-udp
```

### Windows (fallback)
```
cd dashboard
powershell -ExecutionPolicy Bypass -File setup\run.ps1
```

Then open `http://localhost:10000` (this PC) or `http://<this-pc-ip>:10000` (other PCs).
The service conforms to the workspace `server.toml` contract, so `server-manager` can
supervise start/stop/restart and boot autostart.

## Develop

```
# the binary's tests, lint and format
cd dashboard/server && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
# the Python backend's tests
cd dashboard && uv run pytest backend/tests -v
# frontend dev server (proxies /api and /events to the running backend on :10000)
cd dashboard/frontend && npm run dev
```

## Endpoints

- `GET /` — Svelte dashboard
- `GET /api/status` — current state snapshot (JSON)
- `GET /events` — SSE stream of state snapshots
- `POST /api/cmd` `{ "cmd": "target 1 on" }` — relay a control command, returns the reply
- `GET /health` — `{status, pid, uptime_seconds, device}` (server.toml contract)

## The Python backend as a reference

Started by hand beside the service, on a port and with a history directory of its own,
it hears the same broadcast (both bind UDP 50999 with `SO_REUSEPORT`) and writes its own
files, so the two can be compared:
```
cd dashboard && .venv/bin/python -m backend.app --port 9999 --history-dir ../data/history-python
```
Given these two arguments it writes no pidfile and no lifecycle line: those are the
service's (`dashboard/backend/app.py::parse_args`).

## No-signal alarm

The page shows a red band when the selected device's status lines stop for more than
`DEVICE_LIVE_S` (12 s), with the time of the last line and how long ago it was, and dims
the panels below it: they hold the last values received, not the present. A second band
appears when the page loses its own stream from this server.
`dashboard/server/src/bridge.rs::Bridge::watch_silence` logs the silence once and
republishes every `SILENT_REPUBLISH_S` while it lasts, so a page that was already open
learns of it.

## History on disk

`dashboard/server/src/history.rs::History` appends to two text files per local day under
`data/history/`, one record per line, fields joined by ` | `:

- `events-YYYY-MM-DD.txt` — time, kind, message: the page's event log. Kept, and the
  newest `LOG_MAXLEN` are loaded back into the page when the service starts.
- `status-YYYY-MM-DD.txt` — time, device address, the status line as received, for
  every device heard (about 3 MB a day per device). Files older than `STATUS_KEEP_DAYS`
  (30) are deleted on the first line of each day.

A write the disk refuses is reported once on stderr and dropped; the bridge keeps running.

## Device control (CLI)

From any PC on the device LAN:
```
tools/xrfd_ctl.sh "target 1 on"      # Linux/macOS — auto-discovers the device via diag broadcast
tools/xrfd_ctl.sh "status"
```
The Windows `target1_on.bat` / `status.bat` / `send.bat` are thin double-click wrappers
over the same UDP control; `xrfd_ctl.sh` covers them all.

## Legacy

`tools/xrfd_dashboard.ps1` (single-file PowerShell dashboard) remains as a lightweight
per-PC fallback. This service is the primary dashboard.
