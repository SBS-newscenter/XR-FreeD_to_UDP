from datetime import datetime, timedelta, timezone

from backend.history import History

KST = timezone(timedelta(hours=9))
NOON = datetime(2026, 11, 27, 12, 0, 5, tzinfo=KST)


def test_event_is_one_record_in_that_days_file(tmp_path):
    # Red when _append stops writing, or names the file by another day.
    History(tmp_path, keep_days=30).event("warn", "NO SIGNAL: 10.10.204.87", NOON)
    text = (tmp_path / "events-2026-11-27.txt").read_text()
    assert text == "2026-11-27 12:00:05 +0900 | warn | NO SIGNAL: 10.10.204.87\n"


def test_multi_line_message_stays_one_record(tmp_path):
    # Red when the newline replacement goes: a `targets` reply would then
    # spill into lines recent_events cannot read back.
    History(tmp_path, keep_days=30).event("cmd", "targets  ->  t0 on\nt1 off", NOON)
    lines = (tmp_path / "events-2026-11-27.txt").read_text().splitlines()
    assert lines == ["2026-11-27 12:00:05 +0900 | cmd | targets  ->  t0 on / t1 off"]


def test_recent_events_are_newest_first_across_days_and_limited(tmp_path):
    # Red when the files or their lines are read oldest first, or the limit is ignored.
    history = History(tmp_path, keep_days=30)
    history.event("info", "first", NOON - timedelta(days=1))
    history.event("info", "second", NOON)
    history.event("warn", "third", NOON + timedelta(seconds=1))
    assert history.recent_events(2) == [
        {"t": "2026-11-27 12:00:06", "k": "warn", "m": "third"},
        {"t": "2026-11-27 12:00:05", "k": "info", "m": "second"},
    ]


def test_recent_events_of_an_empty_directory_is_empty(tmp_path):
    # Red when a first start, with no history directory yet, raises.
    assert History(tmp_path / "absent", keep_days=30).recent_events(80) == []


def test_status_line_is_recorded_with_its_device(tmp_path):
    # Red when status() stops appending.
    line = "XRFD up=10 ms=10000 ip=10.10.204.87 rx=5 dhcp=0/0 rtr=Y t0=A,5,0,0"
    History(tmp_path, keep_days=30).status("10.10.204.87", line, NOON)
    text = (tmp_path / "status-2026-11-27.txt").read_text()
    assert text == f"2026-11-27 12:00:05 +0900 | 10.10.204.87 | {line}\n"


def test_old_status_files_are_deleted_and_nothing_else(tmp_path):
    # Red when _prune is not called, deletes by the wrong cutoff, or reaches
    # past its own status files.
    history = History(tmp_path, keep_days=30)
    old, kept = NOON - timedelta(days=31), NOON - timedelta(days=30)
    history.status("10.10.204.87", "XRFD old", old)
    history.status("10.10.204.87", "XRFD kept", kept)
    history.event("info", "an old event", old)
    (tmp_path / "status-notes.txt").write_text("not ours")
    history.status("10.10.204.87", "XRFD today", NOON)
    assert sorted(path.name for path in tmp_path.iterdir()) == [
        "events-2026-10-27.txt",
        "status-2026-10-28.txt",
        "status-2026-11-27.txt",
        "status-notes.txt",
    ]


def test_refused_write_is_reported_once_and_does_not_raise(tmp_path, capsys):
    # Red when the OSError escapes into the bridge, or every line repeats the complaint.
    blocker = tmp_path / "history"
    blocker.write_text("a file where the directory should be")
    history = History(blocker, keep_days=30)
    history.event("info", "one", NOON)
    history.event("info", "two", NOON)
    complaints = capsys.readouterr().err.splitlines()
    assert len(complaints) == 1 and "appending to" in complaints[0]
