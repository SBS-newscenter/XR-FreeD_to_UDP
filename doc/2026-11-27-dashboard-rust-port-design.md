# The dashboard's backend as a Rust binary — design

The design of Issue #3. It replaces the Python backend described in
sections 4 to 7 of `doc/2026-06-16-xrfd-dashboard-svelte-migration-design.md`;
that document stays in force for the page and for the firewall.

## What is measured

Taken on ts5-server, 2026-11-27 12:36 KST, the services as they ran that day
(`ps -eo rss`):

| Service | Built with | Resident |
| --- | --- | --- |
| this dashboard's backend | Python, Flask, waitress | 35.8 MB |
| `ts5-portal` | Rust, `tiny_http`, threads | 12.5 MB |
| `server-manager` | Rust, a terminal program | 8.5 MB |
| `tally` | Python | 130.7 MB |
| `schedule-dashboard` | Python, Flask | 121.4 MB |

The box holds 23.9 GB and 22.1 GB of it was available. Memory is not what the
port buys on this box today: it returns about 25 MB. What it buys is stated in
D2.

## Decisions

**D1. The backend moves, and nothing else.** The routes, the snapshot's
fields, the event stream and the history files keep their shape, so the page
under `dashboard/frontend/` and the firmware are not touched. A viewer cannot
tell which backend is serving.

**D2. The reason is the build, and one toolchain.** In Python a mistake in a
branch that rarely runs is found when that branch runs on the server. The
branches that rarely run here are the ones that matter: a device falling
silent, a device returning, a disk refusing a write. In Rust the same
mistakes stop the build: a value that may be absent has to be handled where
it is read, a state shared between the bridge thread and the routes cannot be
reached without its lock, and a set of cases (`ok`, `lost`, `none`) cannot be
matched with one left out. The second reason is that the server's other
services built here are Rust, so one toolchain, one build step and one way to
start a service cover them all.

**D3. Threads, and no async runtime.** An async runtime such as `tokio` lets
one thread serve thousands of connections by switching between them. This
service has one UDP socket and a handful of viewers. It runs one thread for
the UDP socket, a few for requests, and one per open event stream, as
`ts5-portal` does. `tokio` with a web framework on it would add on the order
of a hundred crates to build and audit, and a second way of writing every
function that waits, for a load this service does not have. Open event
streams are capped at 32; the next one is refused with 503.

**D4. The dependencies are six.** `tiny_http` for HTTP, `serde` and
`serde_json` for the snapshot, `signal-hook` for a clean stop, `chrono` for
local time in the history files and the event log, and `socket2` to set
`SO_REUSEPORT` on the status port before it is bound, which the standard
library's `UdpSocket` cannot do and D7 needs. The standard library covers the
rest of UDP, threads, locks and files. `Cargo.lock` is committed and the
build is `cargo build --release --locked`.

**D5. One crate, under `dashboard/server/`.** A library with the modules the
Python package has (`protocol`, `state`, `history`, `bridge`, `http`) and a
`main.rs` that wires them. The binary is `xrfd-dashboard`. It reads the port
from `server.toml`, as `ts5-portal` does, so the port is written once.

**D6. The tests are ported one for one.** Each of the Python backend's tests
has a Rust test of the same name and the same assertion, with time and paths
passed in as they are now. The check gains `cargo fmt --check`, `cargo clippy
-- -D warnings` and `cargo test`, and loses the Python gates when the Python
backend leaves.

**D7. The two run side by side for one broadcast day, the Rust binary as the
service.** The Rust binary is the service on port 10000, the one viewers
open. The Python backend runs beside it on port 9999 for that day, started by
hand from the same checkout, as the reference. Port 10001 is taken on
ts5-server by the file share. Both backends bind UDP 50999 with
`SO_REUSEPORT`, and a broadcast is delivered to every socket bound to it, so
both hear the device. The Python backend takes its port and its history
directory from its start command for that day, so the two write separate
files. At the end of the day their `/api/status` and their history files are
compared.

**D8. The switch and the removal are two pull requests.** The first makes
`dashboard/setup/run.sh` start the binary and `install.sh` build it and the
page, adds the Rust gates to the check, and leaves the Python backend in the
tree, startable on port 9999. server-manager's update on ts5-server then
builds and restarts the service as it does now. The second, after the day of
D7, removes `dashboard/backend/`, its tests, `pyproject.toml`, `uv.lock`, the
Python gates of the check, and the scripts of D9.

**D9. The Windows setup scripts go.** `dashboard/setup/install.ps1`,
`run.ps1` and `firewall.ps1` install and start the Python backend on a
Windows PC, and are removed with it. A Windows PC that has to host the
dashboard runs the Linux scripts under WSL2. That route is not tried: the
device's status arrives as a LAN broadcast, which WSL2 passes to its guest
only in its mirrored networking mode. `tools/xrfd_dashboard.ps1` stays as the
single-file fallback.
