# ARCHITECTURE.md — the codemap

What is where, and what may depend on what. A map of the present: a change
rewrites the cell it touches.

```text
camera ──RS-422 FreeD──► UNO R4 WiFi (src/main.cpp)        ──UDP──► XR engines
                              │ diag broadcast UDP 50999, control UDP 50998
                              ▼
ts5-server, /opt/xr-freed-to-udp                 browsers on the office LAN
┌─────────────────────────────────────────────┐          │
│ dashboard/server/    xrfd-dashboard (Rust)   │◄─────────┘ http://ts5:10000
│   http.rs routes     bridge.rs the one       │   SSE live updates
│   state.rs           UDP thread              │
│   protocol.rs        the device's messages   │
│   history.rs         events and status lines │
│                      on disk, data/history/  │
│   service.rs, clock.rs, main.rs              │
│ dashboard/frontend/  Svelte + Vite, built to │
│                      dist/ by install.sh     │
└─────────────────────────────────────────────┘
```

| Directory | Holds | Runs on | Depends on |
| --- | --- | --- | --- |
| `src/`, `include/`, `lib/` | the firmware; `include/` the pure logic | the UNO R4 WiFi | PlatformIO, `renesas-ra`, Ethernet 2.0.2 |
| `test/` | the native unit tests (`unity`) | the host, the runner | `include/` |
| `platformio.ini` | the board, debug and native environments | — | — |
| `dashboard/backend/` | the Python backend the binary replaces: a reference beside it, and what the Windows scripts start | by hand (the Windows PC as fallback) | flask, waitress |
| `dashboard/server/` | the backend: the binary `xrfd-dashboard`, the bridge and the page's server, its tests in each module | ts5-server | chrono, serde, signal-hook, socket2, tiny_http |
| `dashboard/frontend/` | the Svelte page, built to `dist/`, which the binary serves | the build step | svelte, vite |
| `dashboard/setup/` | `run.sh`, `install.sh`, `firewall.sh`, the unit; the `.ps1` fallbacks | ts5-server (the Windows PC as fallback) | uv, npm |
| `tools/` | operator aids: control, monitor, send, firewall, in `.sh`, `.bat`, `.ps1` | the operator's PC | — |
| `Example/` | the FreeD sample scripts | — | — |
| `server.toml`, `MANUAL.md`, `README.md` | the server-manager manifest, the manual, the design notes | — | — |
| `doc/` | the design in force, the plans | — | — |
| `scripts/`, `.claude/`, `roles/`, `git-hooks/` | the harness and the check | the sessions' host | `chun-harness`, their source |
