from datetime import datetime

from backend import protocol
from backend.state import State

DEV = "10.10.204.123"


def feed(st, line, src=DEV):
    st.update_from_diag(protocol.parse_diag(line), src)


def test_first_packet_sets_device_and_logs():
    st = State()
    feed(st, f"XRFD up=10 ms=10000 ip={DEV} rx=5 dhcp=0/0 rtr=Y")
    snap = st.snapshot()
    assert snap["deviceIp"] == DEV
    assert snap["live"] is True
    assert snap["up"] == 10 and snap["rx"] == 5 and snap["rtr"] == "Y"
    assert any(e["m"].startswith("device found") for e in snap["log"])


def test_no_device_snapshot_is_safe():
    # Regression guard for the PS5.1 Int32 overflow: no device -> huge age, must not crash
    snap = State().snapshot()
    assert snap["deviceIp"] == "" and snap["live"] is False
    assert isinstance(snap["ageSec"], int) and snap["ageSec"] >= 0


def test_fps_from_device_ms():
    st = State()
    feed(st, f"XRFD up=1 ms=1000 ip={DEV} rx=0 dhcp=0/0 rtr=Y")
    feed(st, f"XRFD up=2 ms=2000 ip={DEV} rx=60 dhcp=0/0 rtr=Y")
    assert st.snapshot()["fps"] == 60.0  # 60 frames over 1000 ms


def test_reboot_detection_resets_fps_baseline():
    st = State()
    feed(st, f"XRFD up=100 ms=100000 ip={DEV} rx=6000 dhcp=0/0 rtr=Y")
    feed(st, f"XRFD up=2 ms=2000 ip={DEV} rx=10 dhcp=0/0 rtr=Y")  # up & rx both reset
    assert any("REBOOT" in e["m"] for e in st.snapshot()["log"])


def test_log_capped_at_maxlen():
    st = State()
    for i in range(200):
        st.add_log("info", f"msg {i}")
    assert len(st._log) <= 80


def test_subscribe_receives_published_snapshot():
    st = State()
    q = st.subscribe()
    st.publish()
    item = q.get(timeout=1)
    assert item["deviceIp"] == ""  # a snapshot dict
    st.unsubscribe(q)
    assert q not in st._subscribers


def test_update_from_diag_publishes_to_subscribers():
    st = State()
    q = st.subscribe()
    st.update_from_diag(protocol.parse_diag(f"XRFD up=1 ms=1000 ip={DEV} rx=0 dhcp=0/0 rtr=Y"), DEV)
    st.publish()
    assert q.get(timeout=1)["deviceIp"] == DEV


def test_log_timestamp_includes_date():
    # Event log spans days on a long-running device; time-only stamps made it
    # impossible to tell which day an entry belongs to.
    st = State()
    st.add_log("info", "hello")
    stamp = st.snapshot()["log"][0]["t"]
    datetime.strptime(stamp, "%Y-%m-%d %H:%M:%S")  # raises ValueError on mismatch


DEV2 = "10.10.204.124"


def test_second_device_is_registered_not_discarded():
    # Replacing a converter means both units are briefly live. The old build
    # silently dropped the second one; it must show up as selectable instead.
    st = State()
    feed(st, f"XRFD up=10 ms=10000 ip={DEV} rx=5 dhcp=0/0 rtr=Y")
    feed(st, f"XRFD up=3 ms=3000 ip={DEV2} rx=1 dhcp=0/0 rtr=Y", src=DEV2)
    snap = st.snapshot()
    assert snap["deviceIp"] == DEV  # selection unchanged
    assert {d["ip"] for d in snap["devices"]} == {DEV, DEV2}
    assert snap["up"] == 10 and snap["rx"] == 5  # detail still the selected one


def test_unselected_device_does_not_overwrite_detail():
    st = State()
    feed(st, f"XRFD up=100 ms=100000 ip={DEV} rx=6000 dhcp=0/0 rtr=Y")
    feed(st, f"XRFD up=1 ms=1000 ip={DEV2} rx=0 dhcp=0/0 rtr=N", src=DEV2)
    snap = st.snapshot()
    assert snap["up"] == 100 and snap["rx"] == 6000 and snap["rtr"] == "Y"
    # a second device must not trip the reboot heuristic on the selected one
    assert not any("REBOOT" in e["m"] for e in snap["log"])


def test_select_device_switches_and_resets_detail():
    st = State()
    feed(st, f"XRFD up=100 ms=100000 ip={DEV} rx=6000 dhcp=3/1 rtr=Y")
    feed(st, f"XRFD up=7 ms=7000 ip={DEV2} rx=42 dhcp=0/0 rtr=N", src=DEV2)
    assert st.select_device(DEV2) is True
    snap = st.snapshot()
    assert snap["deviceIp"] == DEV2
    # stale counters from the previous device must not linger
    assert snap["up"] == 0 and snap["rx"] == 0 and snap["fps"] == 0
    assert snap["dhcpOk"] == 0 and snap["dhcpFail"] == 0 and snap["rtr"] == "?"
    assert snap["targets"] == []
    assert snap["live"] is False  # nothing heard from it *since* the switch


def test_select_device_then_next_diag_populates():
    st = State()
    feed(st, f"XRFD up=100 ms=100000 ip={DEV} rx=6000 dhcp=0/0 rtr=Y")
    feed(st, f"XRFD up=7 ms=7000 ip={DEV2} rx=42 dhcp=0/0 rtr=N", src=DEV2)
    st.select_device(DEV2)
    feed(st, f"XRFD up=8 ms=8000 ip={DEV2} rx=100 dhcp=0/0 rtr=N", src=DEV2)
    snap = st.snapshot()
    assert snap["up"] == 8 and snap["rx"] == 100 and snap["live"] is True


def test_select_unknown_device_rejected():
    st = State()
    feed(st, f"XRFD up=10 ms=10000 ip={DEV} rx=5 dhcp=0/0 rtr=Y")
    assert st.select_device("10.10.204.199") is False
    assert st.snapshot()["deviceIp"] == DEV


def test_device_found_logged_once_per_ip():
    st = State()
    for _ in range(3):
        feed(st, f"XRFD up=10 ms=10000 ip={DEV2} rx=5 dhcp=0/0 rtr=Y", src=DEV2)
    found = [e for e in st.snapshot()["log"] if e["m"].startswith("device found")]
    assert len(found) == 1


LINE = f"XRFD up=10 ms=10000 ip={DEV} rx=5 dhcp=0/0 rtr=Y"
T0 = 1_800_000_000.0  # a fixed wall time: these tests read no clock


def feed_at(st, now, line=LINE, src=DEV):
    st.update_from_diag(protocol.parse_diag(line), src, now=now)


def silence_logs(st):
    return [e["m"] for e in st.snapshot()["log"] if e["m"].startswith("NO SIGNAL")]


def test_signal_is_none_then_ok_then_lost():
    # Red when signal() stops reading the registry, or the threshold moves.
    st = State()
    assert st.snapshot(T0)["signal"] == "none"
    feed_at(st, T0)
    assert st.snapshot(T0 + 12)["signal"] == "ok"
    assert st.snapshot(T0 + 13)["signal"] == "lost"


def test_silence_is_logged_once():
    # Red when mark_silent forgets which devices it has already logged: the
    # bridge calls it twice a second and the log would fill with one line.
    st = State()
    feed_at(st, T0)
    st.mark_silent(T0 + 12)
    assert silence_logs(st) == []
    st.mark_silent(T0 + 13)
    st.mark_silent(T0 + 60)
    assert len(silence_logs(st)) == 1 and DEV in silence_logs(st)[0]


def test_return_after_silence_is_logged_and_rearms_the_alarm():
    # Red when a returning device is not taken out of the silent set: its
    # second silence would then pass unlogged.
    st = State()
    feed_at(st, T0)
    st.mark_silent(T0 + 13)
    feed_at(st, T0 + 100)
    assert any(e["m"].startswith("signal restored") for e in st.snapshot()["log"])
    assert st.snapshot(T0 + 100)["signal"] == "ok"
    st.mark_silent(T0 + 113)
    assert len(silence_logs(st)) == 2


def test_snapshot_says_when_the_selected_device_was_last_heard():
    # Red when lastSeen or silentSec leave the snapshot: the page's alarm
    # would have nothing to print.
    st = State()
    feed_at(st, T0)
    snap = st.snapshot(T0 + 300)
    assert snap["silentSec"] == 300
    datetime.strptime(snap["lastSeen"], "%Y-%m-%d %H:%M:%S")
    datetime.strptime(snap["listeningSince"], "%Y-%m-%d %H:%M:%S")


def test_a_long_silence_keeps_its_real_age():
    # Red when the age is capped again: at 999999 s a device silent for
    # twenty days read the same as one never heard.
    st = State()
    feed_at(st, T0)
    twenty_days = 20 * 86400
    snap = st.snapshot(T0 + twenty_days)
    assert snap["ageSec"] == twenty_days
    assert snap["devices"][0]["ageSec"] == twenty_days


def test_switching_to_a_live_device_raises_no_alarm():
    # Red when signal() reads last_seen, which a switch clears until the
    # next line: the page would flash NO SIGNAL on every switch.
    st = State()
    feed_at(st, T0)
    feed_at(st, T0, src=DEV2)
    st.select_device(DEV2)
    assert st.snapshot(T0 + 1)["signal"] == "ok"


def test_log_is_written_to_history_and_read_back_on_start(tmp_path):
    # Red when add_log stops handing events to the history, or a new State
    # stops loading them: a restart would open with an empty event log.
    from backend.history import History

    State(History(tmp_path, keep_days=30)).add_log("warn", "before the restart")
    restarted = State(History(tmp_path, keep_days=30))
    assert [e["m"] for e in restarted.snapshot()["log"]] == ["before the restart"]
