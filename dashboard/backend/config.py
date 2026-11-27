"""Central configuration for the XRFD dashboard backend."""

from pathlib import Path

WEB_PORT = 10000  # HTTP/dashboard port (kept for continuity)
CTRL_PORT = 50998  # UDP: control commands to the device
DIAG_PORT = 50999  # UDP: device diag broadcast (received)

CMD_TIMEOUT_S = 3.0  # wait for a control reply
TARGETS_REFRESH_S = 30.0  # re-query 'targets' while a device is present
DEVICE_LIVE_S = 12  # device considered live if seen within this window
LOG_MAXLEN = 80  # event-log ring size
# While the selected device is silent, the page is sent a snapshot this often,
# so its "silent for" keeps counting without a status line to trigger it.
SILENT_REPUBLISH_S = 5.0
STATUS_KEEP_DAYS = 30  # status files kept on disk; about 3 MB a day per device

REPO_ROOT = Path(__file__).resolve().parents[2]  # .../XR-FreeD_to_UDP
DATA_DIR = REPO_ROOT / "data"
PIDFILE = DATA_DIR / "xrfd.pid"
HISTORY_DIR = DATA_DIR / "history"  # events-*.txt and status-*.txt
FRONTEND_DIST = Path(__file__).resolve().parents[1] / "frontend" / "dist"
