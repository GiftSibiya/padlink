"""XDG locations used by padlink."""

import os
from pathlib import Path


def _xdg(var: str, default: str) -> Path:
    return Path(os.environ.get(var) or Path.home() / default)


CONFIG_DIR = _xdg("XDG_CONFIG_HOME", ".config") / "padlink"
STATE_DIR = _xdg("XDG_STATE_HOME", ".local/state") / "padlink"
PROFILE_DIR = CONFIG_DIR / "profiles"
LOG_DIR = STATE_DIR / "logs"
