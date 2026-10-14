"""JSONL session logs for later analysis.

Each line is one JSON object with at least {"t": <unix time>, "kind": <str>}.
Kinds used by padlink:
  session      first line: padlink version, command, device snapshot
  input        evdev event   {node, type, code, value, name}
  hid_rx       hidraw read   {node, hex}
  hid_tx       hidraw write  {node, hex, ok, error?}
  ff_upload    effect upload {id, effect:{...}}
  ff_erase     effect erase  {id}
  ff_play      play/stop     {id, value}
  ff_gain      gain change   {gain}
  rumble_out   mixed output  {strong, weak} (0..65535) sent to the real device
  diag         doctor result {checks:[...]}
  note         free text     {text}
"""

from __future__ import annotations

import json
import re
import threading
import time
from pathlib import Path

from padlink import __version__
from padlink.core.paths import LOG_DIR

KEEP_LOGS = 100


class SessionLog:
    def __init__(self, label: str, command: str, extra: dict | None = None, directory: Path | None = None,
                 path: Path | None = None):
        directory = Path(path).parent if path else (directory or LOG_DIR)
        directory.mkdir(parents=True, exist_ok=True)
        safe = re.sub(r"[^A-Za-z0-9_.-]+", "_", label).strip("_") or "session"
        stamp = time.strftime("%Y%m%d-%H%M%S")
        self.path = Path(path) if path else directory / f"{stamp}-{command}-{safe}.jsonl"
        self._fh = self.path.open("a", buffering=1)
        self._lock = threading.Lock()
        self.listeners: list = []
        self.write("session", version=__version__, command=command, label=label, **(extra or {}))
        if path is None:
            _rotate(directory)

    def write(self, kind: str, **data) -> dict:
        record = {"t": round(time.time(), 6), "kind": kind, **data}
        line = json.dumps(record, default=_default)
        with self._lock:
            if self._fh:
                self._fh.write(line + "\n")
        for cb in list(self.listeners):
            try:
                cb(record)
            except Exception:
                pass
        return record

    def close(self) -> None:
        with self._lock:
            if self._fh:
                self._fh.close()
                self._fh = None

    def __enter__(self) -> "SessionLog":
        return self

    def __exit__(self, *exc) -> None:
        self.close()


class NullLog:
    """Drop-in when logging is disabled; still notifies listeners."""

    path = None

    def __init__(self):
        self.listeners: list = []

    def write(self, kind: str, **data) -> dict:
        record = {"t": round(time.time(), 6), "kind": kind, **data}
        for cb in list(self.listeners):
            try:
                cb(record)
            except Exception:
                pass
        return record

    def close(self) -> None:
        pass

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        pass


def _default(obj):
    if isinstance(obj, (bytes, bytearray)):
        return obj.hex(" ")
    if isinstance(obj, set):
        return sorted(obj)
    return str(obj)


def _rotate(directory: Path) -> None:
    logs = sorted(directory.glob("*.jsonl"))
    for old in logs[:-KEEP_LOGS]:
        try:
            old.unlink()
        except OSError:
            pass


def list_logs(directory: Path | None = None) -> list[Path]:
    directory = directory or LOG_DIR
    if not directory.exists():
        return []
    return sorted(directory.glob("*.jsonl"), reverse=True)


def read_log(path: Path) -> list[dict]:
    out = []
    with path.open() as fh:
        for line in fh:
            line = line.strip()
            if line:
                try:
                    out.append(json.loads(line))
                except json.JSONDecodeError:
                    out.append({"kind": "corrupt", "line": line})
    return out


def format_record(rec: dict, t0: float | None = None) -> str:
    t = rec.get("t", 0.0)
    ts = f"+{t - t0:9.3f}s" if t0 is not None else time.strftime("%H:%M:%S", time.localtime(t))
    kind = rec.get("kind", "?")
    rest = {k: v for k, v in rec.items() if k not in ("t", "kind")}
    if kind == "input":
        body = f"{rest.get('node', '')} {rest.get('name', '')} = {rest.get('value')}"
    elif kind in ("hid_rx", "hid_tx"):
        body = f"{rest.get('node', '')} {rest.get('hex', '')}" + ("" if rest.get("ok", True) else f"  ERROR {rest.get('error')}")
    elif kind == "ff_play":
        body = f"effect {rest.get('id')} {'PLAY x' + str(rest.get('value')) if rest.get('value') else 'STOP'}"
    elif kind == "rumble_out":
        body = f"strong={rest.get('strong')} weak={rest.get('weak')}"
    else:
        body = json.dumps(rest, default=_default)
    return f"{ts} {kind:10s} {body}"
