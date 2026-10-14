"""Run the bridge in the Rust `padlink` binary (`padlink bridge --json`) instead of in-process.

RustBridge has the same surface as bridge.Bridge (start/stop/alive/stats/
virtual_path/error), so the GTK app can use either. Every log record the Rust
bridge writes is handed to the log's listeners; the Rust process writes the
JSONL file itself when log_file is set.

Which bridge the app uses: PADLINK_BRIDGE=python|rust, default rust when the
binary is found (this checkout's target/release and target/debug, then PATH).
"""

from __future__ import annotations

import json
import os
import shutil
import signal
import subprocess
import threading
from pathlib import Path

from padlink.core.discovery import Controller

REPO = Path(__file__).resolve().parents[2]


def _is_native(path: Path) -> bool:
    """The Rust binary, not an old shell wrapper around the Python CLI."""
    try:
        with open(path, "rb") as fh:
            return fh.read(4) == b"\x7fELF"
    except OSError:
        return False


def find_binary() -> str | None:
    for build in ("release", "debug"):
        p = REPO / "target" / build / "padlink"
        if p.is_file() and os.access(p, os.X_OK):
            return str(p)
    found = shutil.which("padlink")
    if found and _is_native(Path(found)):
        return found
    return None


def use_rust() -> bool:
    choice = os.environ.get("PADLINK_BRIDGE", "").lower()
    if choice == "python":
        return False
    return find_binary() is not None


class RustBridge:
    def __init__(self, ctrl: Controller, log, mode: str = "xbox360", grab: bool = True, log_file: bool = False,
                 binary: str | None = None):
        self.ctrl = ctrl
        self.log = log
        self.mode = mode
        self.grab = grab
        self.log_file = log_file
        self.binary = binary or find_binary()
        self.proc: subprocess.Popen | None = None
        self.thread: threading.Thread | None = None
        self.error: str | None = None
        self.virtual_path: str | None = None
        self.log_path: str | None = None
        self.stats = {"uploads": 0, "plays": 0, "inputs": 0}
        self._stderr: list[str] = []

    def command(self) -> list[str]:
        node = self.ctrl.primary_event.path if self.ctrl.primary_event else self.ctrl.id_str
        cmd = [self.binary, "bridge", node, "--as", self.mode, "--json"]
        if not self.grab:
            cmd.append("--no-grab")
        if self.log_file:
            cmd.append("--log")
        return cmd

    def start(self) -> None:
        if not self.binary:
            raise FileNotFoundError("padlink binary not found (./install.sh or cargo build, or set PADLINK_BRIDGE=python)")
        self.proc = subprocess.Popen(self.command(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                                     bufsize=1)
        self.thread = threading.Thread(target=self._read, daemon=True, name="padlink-rsbridge")
        self.thread.start()
        threading.Thread(target=self._read_stderr, daemon=True).start()

    def _read(self) -> None:
        assert self.proc and self.proc.stdout
        for line in self.proc.stdout:
            try:
                rec = json.loads(line)
            except ValueError:
                continue
            if rec.get("kind") == "status":
                self.virtual_path = rec.get("virtual")
                self.stats = rec.get("stats") or self.stats
                self.error = rec.get("error")
                self.log_path = rec.get("log")
                continue
            for cb in list(self.log.listeners):
                try:
                    cb(rec)
                except Exception:
                    pass
        code = self.proc.wait()
        if code and not self.error:
            tail = " ".join(l.strip() for l in self._stderr[-3:] if l.strip())
            self.error = tail or f"padlink exited with status {code}"

    def _read_stderr(self) -> None:
        assert self.proc and self.proc.stderr
        for line in self.proc.stderr:
            self._stderr.append(line)

    def stop(self) -> None:
        if self.proc and self.proc.poll() is None:
            self.proc.send_signal(signal.SIGTERM)
            try:
                self.proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        if self.thread:
            self.thread.join(timeout=3)
        if self.proc:
            for pipe in (self.proc.stdout, self.proc.stderr):
                if pipe:
                    pipe.close()

    @property
    def alive(self) -> bool:
        return bool(self.proc and self.proc.poll() is None)
