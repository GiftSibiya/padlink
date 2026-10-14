"""Rumble discovery for devices the kernel can't drive.

Stage 1 sends a series of candidate output reports and asks a human whether the
controller vibrated. Candidates come from the device's own HID descriptor
(every output report, filled with 0xFF) plus layouts used by known gamepad
families. Stage 2 narrows an all-0xFF hit down to the bytes that control each
motor, so the resulting profile can do variable strength.

Only *output* reports are sent, and every "on" is followed by an "off". Each
write is logged.
"""

from __future__ import annotations

import time
from dataclasses import dataclass, field

from padlink.core import hiddesc
from padlink.core.discovery import Controller, HidrawNode
from padlink.core.hidraw import HidrawDevice, hexstr
from padlink.core.profiles import RumbleSpec

# (label, payload template excluding the report ID byte, scale)
KNOWN_LAYOUTS: list[tuple[str, list, int]] = [
    ("Xbox One/Series HID (BT style)", [0x0F, 0x00, 0x00, "strong", "weak", 0xFF, 0x00, 0x00], 100),
    ("Xbox 360 wired style", [0x00, 0x08, 0x00, "strong", "weak", 0x00, 0x00, 0x00], 255),
    ("DragonRise / GreenAsia", [0x51, 0x00, "strong", 0x00, "weak", 0x00, 0x00, 0x00], 255),
    ("DragonRise (motors swapped)", [0x51, 0x00, "weak", 0x00, "strong", 0x00, 0x00, 0x00], 255),
    ("SmartJoy style", [0x01, "weak", "strong"], 255),
    ("DualShock 4 USB style", [0x07, 0x00, 0x00, "weak", "strong", 0x00, 0x00, 0x00], 255),
    ("Two bytes strong,weak", ["strong", "weak"], 255),
    ("Header 0x00 then strong,weak", [0x00, "strong", "weak"], 255),
    ("Header 0x08 then strong,weak", [0x08, "strong", "weak"], 255),
]


@dataclass
class Candidate:
    label: str
    node: HidrawNode
    report_id: int
    template: list  # full wire template, including the report ID byte
    scale: int = 255
    stop: list | None = None
    meta: dict = field(default_factory=dict)

    def spec(self) -> RumbleSpec:
        return RumbleSpec(transport="hidraw", interface=self.node.interface, report=list(self.template),
                          stop=self.stop, scale=self.scale)

    def on_bytes(self, strong: int = 0xFFFF, weak: int = 0xFFFF) -> bytes:
        return self.spec().render(strong, weak)

    def off_bytes(self) -> bytes:
        return self.spec().render(0, 0)


def _fit(payload: list, length: int) -> list | None:
    if length <= 0:
        return None
    if len(payload) > length:
        if any(v != 0 for v in payload[length:]):
            return None
        return payload[:length]
    return payload + [0] * (length - len(payload))


def output_reports(node: HidrawNode) -> tuple[hiddesc.Descriptor | None, list[hiddesc.Report]]:
    try:
        desc = hiddesc.parse(node.descriptor())
    except OSError:
        return None, []
    reports = []
    for r in desc.by_kind("output"):
        if r.usage_pages and r.usage_pages <= {0x08}:  # keyboard LEDs only
            continue
        reports.append(r)
    return desc, reports


def stage1_candidates(ctrl: Controller, blind: bool = False) -> list[Candidate]:
    cands: list[Candidate] = []
    for node in ctrl.hidraw:
        desc, reports = output_reports(node)
        targets = [(r.report_id, r.payload_len, desc.uses_report_ids if desc else False) for r in reports]
        if blind and not targets:
            targets = [(rid, 8, rid != 0) for rid in (0, 1, 2, 3, 5)]
        for rid, length, _uses_ids in targets:
            prefix = [rid]
            # 1) everything on: the most likely single "does anything vibrate" test
            cands.append(Candidate(f"report {rid:#04x}: all bytes 0xFF", node, rid,
                                   prefix + ["any"] * length, meta={"kind": "all_ff", "length": length}))
            # 2) known gamepad layouts that fit this report
            for label, payload, scale in KNOWN_LAYOUTS:
                fitted = _fit(list(payload), length)
                if fitted is not None:
                    cands.append(Candidate(f"report {rid:#04x}: {label}", node, rid, prefix + fitted,
                                           scale=scale, meta={"kind": "layout", "length": length}))
    return cands


def stage2_byte_candidates(hit: Candidate) -> list[Candidate]:
    """Single-byte tests to find which positions drive which motor."""
    length = hit.meta.get("length", len(hit.template) - 1)
    out = []
    for pos in range(length):
        tpl = [hit.report_id] + [0] * length
        tpl[1 + pos] = "any"
        out.append(Candidate(f"byte {pos} = 0xFF", hit.node, hit.report_id, tpl,
                             meta={"kind": "byte", "pos": pos, "length": length}))
    return out


def build_from_bytes(hit: Candidate, motors: dict[int, str]) -> Candidate:
    """motors: byte position -> 'strong' | 'weak' | 'both'. Unmarked bytes are 0.

    If nothing was identified, fall back to the all-0xFF report as on/off.
    """
    length = hit.meta.get("length", len(hit.template) - 1)
    if not motors:
        return hit
    tpl: list = [hit.report_id] + [0] * length
    for pos, motor in motors.items():
        tpl[1 + pos] = {"strong": "strong", "weak": "weak"}.get(motor, "max")
    return Candidate(f"report {hit.report_id:#04x}: learned layout", hit.node, hit.report_id, tpl,
                     meta={"kind": "learned", "length": length})


class Prober:
    """Sends candidates with logging. UI-agnostic: the caller asks the human."""

    def __init__(self, log=None):
        self.log = log
        self._devs: dict[str, HidrawDevice] = {}

    def _dev(self, path: str) -> HidrawDevice:
        if path not in self._devs:
            self._devs[path] = HidrawDevice(path, write=True)
        return self._devs[path]

    def send(self, path: str, data: bytes) -> str | None:
        try:
            self._dev(path).write(data)
            err = None
        except OSError as exc:
            err = str(exc)
        if self.log:
            self.log.write("hid_tx", node=path, hex=hexstr(data), ok=err is None, **({"error": err} if err else {}))
        return err

    def pulse(self, cand: Candidate, ms: int = 700) -> str | None:
        """Send on, wait, send off. Returns an error string if the write failed."""
        if self.log:
            self.log.write("note", text=f"probe: {cand.label}", template=cand.template)
        err = self.send(cand.node.path, cand.on_bytes())
        if err:
            return err
        time.sleep(ms / 1000.0)
        self.send(cand.node.path, cand.off_bytes())
        return None

    def close(self) -> None:
        for d in self._devs.values():
            d.close()
        self._devs.clear()
