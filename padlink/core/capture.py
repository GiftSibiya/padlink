"""USB traffic capture through usbmon (text interface), for reverse-engineering.

Typical use: pass the dongle through to a Windows VM running the official app
(or any game with working rumble), run `sudo -E padlink capture 3537:1041`
on the host, make it rumble, then look for the OUT reports in the log.

usbmon text line format (Documentation/usb/usbmon.rst):
  <urb tag> <timestamp us> <S|C|E> <type+dir>:<bus>:<dev>:<ep> <status> <len> [= data words]
"""

from __future__ import annotations

import os
import pwd
import time
from pathlib import Path

from padlink.core.discovery import Controller
from padlink.core.logger import SessionLog

DEBUGFS = Path("/sys/kernel/debug/usb/usbmon")


class CaptureError(RuntimeError):
    pass


def usb_address(ctrl: Controller) -> tuple[int, int]:
    root = Path(ctrl.key)
    try:
        return int((root / "busnum").read_text()), int((root / "devnum").read_text())
    except OSError:
        raise CaptureError("capture only works for USB devices (the dongle), not Bluetooth/virtual") from None


def parse_line(line: str) -> dict | None:
    parts = line.split()
    if len(parts) < 4:
        return None
    try:
        addr = parts[3].split(":")
        xfer, bus, dev, ep = addr[0], int(addr[1]), int(addr[2]), int(addr[3])
    except (IndexError, ValueError):
        return None
    rec = {"event": parts[2], "xfer": xfer, "bus": bus, "dev": dev, "ep": ep,
           "dir": "in" if xfer.endswith("i") else "out", "type": {"C": "control", "I": "interrupt",
                                                               "B": "bulk", "Z": "iso"}.get(xfer[0], xfer[0])}
    if "=" in parts:
        idx = parts.index("=")
        rec["hex"] = " ".join(w[i:i + 2] for w in parts[idx + 1:] for i in range(0, len(w), 2))
    if xfer.startswith("C") and parts[2] == "S" and len(parts) > 9 and parts[4] == "s":
        rec["setup"] = " ".join(parts[5:10])
    return rec


def _default_log_dir() -> Path | None:
    user = os.environ.get("SUDO_USER")
    if user:
        home = Path(pwd.getpwnam(user).pw_dir)
        return home / ".local/state/padlink/logs"
    return None


def run(ctrl: Controller, out: str | None = None, show: bool = True) -> int:
    bus, dev = usb_address(ctrl)
    src = DEBUGFS / f"{bus}u"
    if not DEBUGFS.exists():
        raise CaptureError("usbmon not available: sudo modprobe usbmon (and debugfs mounted at /sys/kernel/debug)")
    fh = open(src)  # PermissionError propagates
    log = SessionLog(ctrl.id_str, "capture", {"device": ctrl.to_dict(), "usb": {"bus": bus, "dev": dev}},
                     directory=_default_log_dir(), path=Path(out) if out else None)
    sudo_uid = os.environ.get("SUDO_UID")
    print(f"capturing bus {bus} device {dev} from {src} -> {log.path}  (Ctrl+C to stop)")
    try:
        for line in fh:
            rec = parse_line(line)
            if not rec or rec["bus"] != bus or rec["dev"] != dev:
                continue
            log.write("usb", **rec)
            if show and rec["event"] in ("S", "C") and rec.get("hex"):
                arrow = "->" if rec["dir"] == "out" else "<-"
                if rec["dir"] == "out" or rec["event"] == "C":
                    print(f"{time.strftime('%H:%M:%S')} {rec['type']:9s} ep{rec['ep']} {arrow} {rec['hex']}")
    except KeyboardInterrupt:
        pass
    finally:
        log.close()
        fh.close()
        if sudo_uid and log.path:
            try:
                os.chown(log.path, int(sudo_uid), int(os.environ.get("SUDO_GID", sudo_uid)))
            except OSError:
                pass
    return 0
