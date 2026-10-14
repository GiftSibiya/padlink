"""Small persistent UI/user preferences (~/.config/padlink/settings.json)."""

from __future__ import annotations

import json

from padlink.core.paths import CONFIG_DIR

PATH = CONFIG_DIR / "settings.json"


def load() -> dict:
    try:
        return json.loads(PATH.read_text())
    except (OSError, ValueError):
        return {}


def get(key: str, default=None):
    return load().get(key, default)


def set(key: str, value) -> None:  # noqa: A001 - mirrors get()
    data = load()
    data[key] = value
    try:
        PATH.parent.mkdir(parents=True, exist_ok=True)
        PATH.write_text(json.dumps(data, indent=2) + "\n")
    except OSError:
        pass
