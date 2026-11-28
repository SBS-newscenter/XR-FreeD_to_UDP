"""Disk history: what the dashboard heard, kept after the process is gone.

The page's event log and the device's status lines used to live in memory
only, so a restart, or a question asked weeks later, found nothing. Two
append-only text files per local day hold them instead, under one directory:

    events-YYYY-MM-DD.txt   <time> | <kind> | <message>      the event log
    status-YYYY-MM-DD.txt   <time> | <device ip> | <line>    every status line

Events are small and kept. Status files older than `keep_days` are deleted.

Failure: a write the disk refuses is reported on stderr and dropped. The
bridge keeps forwarding to the page, which is worth more than the record.
"""

import sys
import threading
from datetime import date, datetime, timedelta
from pathlib import Path

_TIME_FORMAT = "%Y-%m-%d %H:%M:%S %z"
_SEPARATOR = " | "


class History:
    def __init__(self, directory: Path, keep_days: int) -> None:
        self._directory = directory
        self._keep_days = keep_days
        self._lock = threading.Lock()  # events come from the bridge and from routes
        self._pruned_on: date | None = None
        self._last_complaint = ""

    def event(self, kind: str, message: str, when: datetime) -> None:
        self._append("events", when, kind, message)

    def status(self, device_ip: str, line: str, when: datetime) -> None:
        self._append("status", when, device_ip, line)
        if self._pruned_on != when.date():  # the first line of each day
            self._pruned_on = when.date()
            self._prune(when.date())

    def recent_events(self, limit: int) -> list[dict[str, str]]:
        """The newest `limit` events, newest first, in the event log's shape."""
        events: list[dict[str, str]] = []
        for path in sorted(self._directory.glob("events-*.txt"), reverse=True):
            try:
                lines = path.read_text(encoding="utf-8").splitlines()
            except OSError as error:
                self._complain(f"reading {path}", error)
                continue
            for line in reversed(lines):
                fields = line.split(_SEPARATOR, 2)
                if len(fields) != 3:
                    continue  # a torn or foreign line
                # The page shows local time without the zone: the first 19 characters.
                events.append({"t": fields[0][:19], "k": fields[1], "m": fields[2]})
                if len(events) == limit:
                    return events
        return events

    def _append(self, stem: str, when: datetime, tag: str, text: str) -> None:
        # One record is one line: a `targets` reply spans four.
        one_line = text.replace("\r", "").replace("\n", " / ")
        path = self._directory / f"{stem}-{when:%Y-%m-%d}.txt"
        record = f"{when.strftime(_TIME_FORMAT)}{_SEPARATOR}{tag}{_SEPARATOR}{one_line}\n"
        with self._lock:
            try:
                self._directory.mkdir(parents=True, exist_ok=True)
                with path.open("a", encoding="utf-8") as file:
                    file.write(record)
            except OSError as error:
                self._complain(f"appending to {path}", error)

    def _prune(self, today: date) -> None:
        cutoff = today - timedelta(days=self._keep_days)
        for path in self._directory.glob("status-*.txt"):
            try:
                day = date.fromisoformat(path.stem.removeprefix("status-"))
            except ValueError:
                continue  # not a file this module named
            if day < cutoff:
                try:
                    path.unlink()
                except OSError as error:
                    self._complain(f"deleting {path}", error)

    def _complain(self, doing: str, error: OSError) -> None:
        # A full disk fails every five seconds: say it once, not on every line.
        complaint = f"history: {doing} failed: {error}"
        if complaint != self._last_complaint:
            self._last_complaint = complaint
            print(complaint, file=sys.stderr, flush=True)
