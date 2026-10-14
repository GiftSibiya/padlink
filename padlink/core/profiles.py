"""Per-device profiles: how to make a device rumble, and how to map its input.

Stored as JSON in ~/.config/padlink/profiles/<vid>_<pid>.json, e.g.:

{
  "name": "GameSir T4N Lite (HID mode)",
  "vendor": "3537", "product": "1041",
  "rumble": {
    "transport": "hidraw",          # hidraw | evdev
    "interface": null,               # USB interface number of the hidraw node, null = first
    "report": [5, 0, "strong", "weak", 0, 0, 0, 0],
    "stop":   null,                  # optional explicit stop report (else report with 0,0)
    "scale": 255,                    # max value written for full magnitude
    "keepalive_ms": 0,               # resend while active (some pads time out)
    "min_interval_ms": 8,            # rate-limit writes
    "swap": false                    # swap strong/weak motors
  },
  "input_map": { ... see bridge.py ... }
}

Template tokens inside "report": "strong", "weak" (scaled to 0..scale),
"strong_hi"/"strong_lo"/"weak_hi"/"weak_lo" (16-bit split of the scaled value),
"max" (strong/weak whichever is larger), "any" (scale if either motor active else 0).
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from pathlib import Path

from padlink.core.paths import PROFILE_DIR

TOKENS = {"strong", "weak", "strong_hi", "strong_lo", "weak_hi", "weak_lo", "max", "any"}


@dataclass
class RumbleSpec:
    transport: str = "hidraw"
    interface: str | None = None
    report: list = field(default_factory=list)
    stop: list | None = None
    scale: int = 255
    keepalive_ms: int = 0
    min_interval_ms: int = 8
    swap: bool = False

    def render(self, strong: int, weak: int) -> bytes:
        """Build the output report for 16-bit magnitudes (0..65535)."""
        if self.swap:
            strong, weak = weak, strong
        if strong == 0 and weak == 0 and self.stop:
            return bytes(int(b) & 0xFF for b in self.stop)
        s = round(strong * self.scale / 0xFFFF)
        w = round(weak * self.scale / 0xFFFF)
        values = {
            "strong": s, "weak": w,
            "strong_hi": s >> 8, "strong_lo": s & 0xFF,
            "weak_hi": w >> 8, "weak_lo": w & 0xFF,
            "max": max(s, w), "any": self.scale if (s or w) else 0,
        }
        out = bytearray()
        for item in self.report:
            if isinstance(item, str):
                if item not in TOKENS:
                    raise ValueError(f"unknown template token {item!r}")
                out.append(values[item] & 0xFF)
            else:
                out.append(int(item) & 0xFF)
        return bytes(out)

    def to_dict(self) -> dict:
        return dict(self.__dict__)


@dataclass
class Profile:
    vendor: int
    product: int
    name: str = ""
    rumble: RumbleSpec | None = None
    input_map: dict | None = None
    notes: str = ""

    @property
    def path(self) -> Path:
        return profile_path(self.vendor, self.product)

    def to_dict(self) -> dict:
        return {
            "name": self.name,
            "vendor": f"{self.vendor:04x}",
            "product": f"{self.product:04x}",
            "rumble": self.rumble.to_dict() if self.rumble else None,
            "input_map": self.input_map,
            "notes": self.notes,
        }

    def save(self, directory: Path | None = None) -> Path:
        path = (directory or PROFILE_DIR) / f"{self.vendor:04x}_{self.product:04x}.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(self.to_dict(), indent=2) + "\n")
        return path

    @classmethod
    def from_dict(cls, data: dict) -> "Profile":
        rumble = data.get("rumble")
        return cls(
            vendor=int(str(data["vendor"]), 16),
            product=int(str(data["product"]), 16),
            name=data.get("name", ""),
            rumble=RumbleSpec(**rumble) if rumble else None,
            input_map=data.get("input_map"),
            notes=data.get("notes", ""),
        )


def profile_path(vendor: int, product: int, directory: Path | None = None) -> Path:
    return (directory or PROFILE_DIR) / f"{vendor:04x}_{product:04x}.json"


def load(vendor: int, product: int, directory: Path | None = None) -> Profile | None:
    path = profile_path(vendor, product, directory)
    if not path.exists():
        return None
    return Profile.from_dict(json.loads(path.read_text()))


def load_or_new(vendor: int, product: int, name: str = "") -> Profile:
    return load(vendor, product) or Profile(vendor=vendor, product=product, name=name)
