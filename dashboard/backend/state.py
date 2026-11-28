"""Thread-safe shared dashboard state (ported from tools/xrfd_dashboard.ps1)."""

import threading
import time
from collections import deque
from datetime import datetime

from . import config
from .history import History

_CLOCK_FORMAT = "%Y-%m-%d %H:%M:%S"


def _clock(epoch: float) -> str:
    return datetime.fromtimestamp(epoch).strftime(_CLOCK_FORMAT)


class State:
    def __init__(self, history: History | None = None):
        self._lock = threading.RLock()
        self._history = history
        self._started = time.time()
        self.device_ip = ""  # currently *selected* device (detail below is its)
        self.last_seen = 0.0  # wall time (time.time()) of last diag
        self._devices = {}  # ip -> wall time of that device's last diag
        self._silent = set()  # ips whose silence has been logged, until they return
        self.up = 0
        self.rx = 0
        self.fps = 0.0
        self.dhcp_ok = 0
        self.dhcp_fail = 0
        self.rtr = "?"
        self.conflict = False
        self.diag_state = {}  # "0" -> {state,ok,fail,skip}
        self.targets = []  # [{n,on,ip,port}]
        self._log = deque(maxlen=config.LOG_MAXLEN)
        if history:
            self._log.extend(history.recent_events(config.LOG_MAXLEN))
        self._prev_rx = -1
        self._prev_ms = -1
        self._prev_time = None
        self._subscribers = set()

    def add_log(self, kind, msg):
        when = datetime.now().astimezone()
        with self._lock:
            self._log.appendleft({"t": when.strftime(_CLOCK_FORMAT), "k": kind, "m": msg})
            if self._history:
                self._history.event(kind, msg, when)

    def update_from_diag(self, parsed, src_ip, now: float | None = None):
        if parsed is None:
            return
        now = time.time() if now is None else now
        with self._lock:
            # Every device that broadcasts stays in the registry so the UI can
            # offer it; only the selected one drives the detail fields below.
            if src_ip not in self._devices:
                self.add_log("info", f"device found: {src_ip}")
            elif src_ip in self._silent:
                self._silent.discard(src_ip)
                since = _clock(self._devices[src_ip])
                self.add_log("info", f"signal restored: {src_ip} (silent since {since})")
            self._devices[src_ip] = now

            if not self.device_ip:
                self.device_ip = src_ip
            elif src_ip != self.device_ip:
                return

            up, rx, ms = parsed["up"], parsed["rx"], parsed["ms"]
            # real reboot: up AND rx both reset (millis wrap resets up only)
            if up < self.up and rx < self.rx:
                self.add_log("warn", f"DEVICE REBOOTED (uptime reset: {self.up}s -> {up}s)")
                self._prev_rx = -1
                self._prev_ms = -1

            self.last_seen = now
            self.up, self.rx = up, rx
            self.dhcp_ok, self.dhcp_fail = parsed["dhcp_ok"], parsed["dhcp_fail"]
            self.rtr = parsed["rtr"]
            if parsed["conflict"] and not self.conflict:
                self.add_log("warn", "IP CONFLICT detected on device LAN!")
            self.conflict = parsed["conflict"]

            # fps from device ms when available, else PC-clock fallback
            if ms is not None and self._prev_ms >= 0 and ms > self._prev_ms and rx >= self._prev_rx:
                dms = ms - self._prev_ms
                if dms >= 1000:
                    self.fps = round((rx - self._prev_rx) * 1000.0 / dms, 2)
            elif ms is None and self._prev_rx >= 0 and rx > self._prev_rx and self._prev_time:
                dt = now - self._prev_time
                if dt > 0.5:
                    self.fps = round((rx - self._prev_rx) / dt, 2)
            self._prev_rx = rx
            self._prev_ms = ms if ms is not None else -1
            self._prev_time = now

            self.diag_state = parsed["targets"]

    def mark_silent(self, now: float | None = None) -> None:
        """Log each device whose status lines have stopped, once per silence."""
        now = time.time() if now is None else now
        with self._lock:
            newly_silent = [
                ip
                for ip, seen in self._devices.items()
                if now - seen > config.DEVICE_LIVE_S and ip not in self._silent
            ]
            for ip in newly_silent:
                self._silent.add(ip)
                self.add_log("warn", f"NO SIGNAL: {ip} (last status {_clock(self._devices[ip])})")

    def signal(self, now: float | None = None) -> str:
        """Whether the selected device is being heard: "ok", "lost" or "none".

        Read from the registry, not from `last_seen`, which a switch of device
        clears until that device's next line: a live device just switched to
        is "ok", never "lost".
        """
        now = time.time() if now is None else now
        with self._lock:
            if self.device_ip not in self._devices:
                return "none"
            heard = now - self._devices[self.device_ip] <= config.DEVICE_LIVE_S
            return "ok" if heard else "lost"

    def select_device(self, ip):
        """Point the dashboard at a different discovered device.

        Returns False if that IP has never been heard from. Detail counters are
        cleared so the previous device's uptime/rx/fps can't be misread as the
        new one's, and so the reboot heuristic doesn't fire on the switch.
        """
        with self._lock:
            if ip not in self._devices:
                return False
            if ip == self.device_ip:
                return True
            self.device_ip = ip
            self.last_seen = 0.0
            self.up = 0
            self.rx = 0
            self.fps = 0.0
            self.dhcp_ok = 0
            self.dhcp_fail = 0
            self.rtr = "?"
            self.conflict = False
            self.diag_state = {}
            self.targets = []
            self._prev_rx = -1
            self._prev_ms = -1
            self._prev_time = None
            self.add_log("info", f"switched to {ip}")
            return True

    def snapshot(self, now: float | None = None):
        now = time.time() if now is None else now
        with self._lock:
            # 999999 stands for "never heard"; a real age is not capped, or a
            # device silent for twelve days reads the same as one never seen.
            age = int(now - self.last_seen) if self.last_seen else 999999
            selected_seen = self._devices.get(self.device_ip, 0.0)
            devices = [
                {
                    "ip": ip,
                    "ageSec": int(now - seen),
                    "live": (now - seen) <= config.DEVICE_LIVE_S,
                }
                for ip, seen in sorted(self._devices.items())
            ]
            tl = []
            for t in self.targets:
                d = self.diag_state.get(str(t["n"]))
                entry = {
                    "n": t["n"],
                    "on": t["on"],
                    "ip": t["ip"],
                    "port": t["port"],
                    "state": "off",
                    "ok": 0,
                    "fail": 0,
                    "skip": 0,
                }
                if d:
                    entry.update(state=d["state"], ok=d["ok"], fail=d["fail"], skip=d["skip"])
                tl.append(entry)
            return {
                "deviceIp": self.device_ip,
                "devices": devices,
                "ageSec": age,
                "live": bool(self.device_ip) and age <= config.DEVICE_LIVE_S,
                "signal": self.signal(now),
                "lastSeen": _clock(selected_seen) if selected_seen else "",
                "silentSec": int(now - selected_seen) if selected_seen else 0,
                "listeningSince": _clock(self._started),
                "up": self.up,
                "rx": self.rx,
                "fps": self.fps,
                "dhcpOk": self.dhcp_ok,
                "dhcpFail": self.dhcp_fail,
                "rtr": self.rtr,
                "conflict": self.conflict,
                "targets": tl,
                "log": list(self._log)[:40],
            }

    def subscribe(self):
        import queue

        q = queue.Queue(maxsize=10)
        with self._lock:
            self._subscribers.add(q)
        return q

    def unsubscribe(self, q):
        with self._lock:
            self._subscribers.discard(q)

    def publish(self, now: float | None = None):
        snap = self.snapshot(now)
        with self._lock:
            subs = list(self._subscribers)
        for q in subs:
            try:
                q.put_nowait(snap)
            except Exception:
                pass  # slow/full consumer: drop this update, never block the bridge
