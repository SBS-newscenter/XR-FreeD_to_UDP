"""Flask app factory + waitress entrypoint for the XRFD dashboard."""

import argparse
import json
import os
import queue
import signal
import socket
import sys
import time
from pathlib import Path

from flask import Flask, Response, jsonify, request, send_from_directory

from . import __version__, config, lifecycle
from .access_log import AccessLog


def create_app(state, bridge):
    app = Flask(__name__, static_folder=None)
    start_time = time.time()

    @app.get("/api/status")
    def api_status():
        return jsonify(state.snapshot())

    @app.get("/health")
    def health():
        is_local = request.remote_addr in ("127.0.0.1", "::1")
        obj = {"status": "ok"}
        if is_local:
            obj["pid"] = os.getpid()
            obj["uptime_seconds"] = round(time.time() - start_time, 1)
            obj["device"] = state.device_ip
        return jsonify(obj)

    @app.post("/api/cmd")
    def api_cmd():
        data = request.get_json(silent=True) or {}
        cmd = data.get("cmd")
        if not cmd:
            return jsonify({"error": "missing 'cmd'"}), 400
        reply = bridge.send_command(cmd)
        state.add_log("cmd", "{}  ->  {}".format(cmd, reply if reply else "(no reply)"))
        if cmd.startswith("target "):
            bridge._maybe_refresh_targets()
        return jsonify({"ok": reply is not None, "reply": reply})

    @app.post("/api/device")
    def api_device():
        data = request.get_json(silent=True) or {}
        ip = data.get("ip")
        if not ip:
            return jsonify({"error": "missing 'ip'"}), 400
        if not state.select_device(ip):
            return jsonify({"error": f"unknown device: {ip}"}), 404
        state.publish()  # push the switch to every open SSE client
        return jsonify({"ok": True, "selected": ip})

    @app.get("/events")
    def events():
        def stream():
            q = state.subscribe()
            try:
                # immediate snapshot so a fresh client paints at once
                yield f"data: {json.dumps(state.snapshot())}\n\n"
                while True:
                    try:
                        snap = q.get(timeout=15)
                        yield f"data: {json.dumps(snap)}\n\n"
                    except queue.Empty:
                        yield ": keepalive\n\n"  # comment frame keeps proxies open
            finally:
                state.unsubscribe(q)

        return Response(
            stream(),
            mimetype="text/event-stream",
            headers={"Cache-Control": "no-cache", "X-Accel-Buffering": "no"},
        )

    @app.get("/")
    def index():
        if not (config.FRONTEND_DIST / "index.html").exists():
            return ("frontend not built — run: cd dashboard/frontend && npm run build", 503)
        return send_from_directory(config.FRONTEND_DIST, "index.html")

    @app.get("/<path:path>")
    def assets(path):
        if (config.FRONTEND_DIST / path).exists():
            return send_from_directory(config.FRONTEND_DIST, path)
        return ("not found", 404)

    return app


def _write_pidfile():
    config.DATA_DIR.mkdir(parents=True, exist_ok=True)
    config.PIDFILE.write_text(
        f'{{"pid": {os.getpid()}, "command": "python -m backend.app", '
        f'"started_at": "{time.strftime("%Y-%m-%dT%H:%M:%S")}"}}'
    )


def _lan_ips():
    """Best-effort list of this host's non-loopback IPv4 addresses."""
    ips = set()
    try:
        for info in socket.getaddrinfo(socket.gethostname(), None, socket.AF_INET):
            ip = info[4][0]
            if not ip.startswith("127.") and not ip.startswith("169.254."):
                ips.add(ip)
    except OSError:
        pass
    return sorted(ips)


def parse_args(argv: list[str] | None) -> argparse.Namespace:
    """The arguments of a reference instance: one run by hand beside the
    service to compare the two. It takes a port and a history directory of
    its own, and leaves the service's pidfile and lifecycle log alone. With
    no argument this is the service itself, as the Windows scripts start it.
    """
    parser = argparse.ArgumentParser(prog="python -m backend.app")
    parser.add_argument("--port", type=int, help="serve the page here, as a reference instance")
    parser.add_argument("--history-dir", type=Path, help="the reference instance's own history")
    args = parser.parse_args(argv)
    if (args.port is None) != (args.history_dir is None):
        parser.error("--port and --history-dir are given together")
    return args


def main(argv: list[str] | None = None):
    from waitress import serve

    from .history import History
    from .state import State
    from .udp_bridge import UdpBridge

    args = parse_args(argv)
    is_service = args.port is None
    history = History(args.history_dir or config.HISTORY_DIR, config.STATUS_KEEP_DAYS)
    state = State(history)
    bridge = UdpBridge(state, history)
    bridge.start()
    state.add_log("info", "dashboard started")
    _log_dir = config.REPO_ROOT / "log"
    if is_service:
        _write_pidfile()
        lifecycle.append(
            _log_dir, "xr-freed-to-udp", "STARTED", f"version={__version__} pid={os.getpid()}"
        )
    app = create_app(state, bridge)
    port = args.port or config.WEB_PORT
    # 0.0.0.0 = bind on all interfaces; browse via one of these reachable URLs:
    print(f"XRFD dashboard running (waitress, listening on 0.0.0.0:{port}):", flush=True)
    print(f"  local:   http://localhost:{port}", flush=True)
    for ip in _lan_ips():
        print(f"  LAN:     http://{ip}:{port}", flush=True)
    # systemd `stop` sends SIGTERM; Python's default would kill the process outright
    # and skip the finally-block cleanup. Raising SystemExit lets waitress unwind
    # cleanly (it reraises SystemExit), so the STOPPED log + pidfile removal run.
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    try:
        serve(AccessLog(app), host="0.0.0.0", port=port, threads=8)
    finally:
        bridge.stop()
        if is_service:
            config.PIDFILE.unlink(missing_ok=True)
            lifecycle.append(_log_dir, "xr-freed-to-udp", "STOPPED", f"pid={os.getpid()}")


if __name__ == "__main__":
    main()
