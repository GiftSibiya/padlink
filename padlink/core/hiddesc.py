"""Minimal HID report descriptor parser.

Good enough to answer the questions padlink cares about: which input, output
and feature reports exist, how long they are, which usage pages they carry,
and whether the device speaks the PID (force feedback) usage page.
"""

from __future__ import annotations

from dataclasses import dataclass, field

USAGE_PAGES = {
    0x01: "Generic Desktop",
    0x02: "Simulation",
    0x05: "Game",
    0x06: "Generic Device",
    0x07: "Keyboard",
    0x08: "LED",
    0x09: "Button",
    0x0C: "Consumer",
    0x0F: "PID (force feedback)",
}

GENERIC_DESKTOP = {
    0x01: "Pointer", 0x02: "Mouse", 0x04: "Joystick", 0x05: "Gamepad", 0x06: "Keyboard",
    0x08: "Multi-axis", 0x30: "X", 0x31: "Y", 0x32: "Z", 0x33: "Rx", 0x34: "Ry", 0x35: "Rz",
    0x36: "Slider", 0x37: "Dial", 0x38: "Wheel", 0x39: "Hat switch", 0x3D: "Start",
    0x3E: "Select", 0x90: "D-pad up", 0x91: "D-pad down", 0x92: "D-pad right", 0x93: "D-pad left",
}

SIMULATION = {0xBA: "Rudder", 0xBB: "Throttle", 0xC4: "Accelerator", 0xC5: "Brake"}

PID = {
    0x21: "Set Effect Report", 0x22: "Effect Block Index", 0x25: "Effect Type",
    0x26: "ET Constant Force", 0x27: "ET Ramp", 0x30: "ET Square", 0x31: "ET Sine",
    0x50: "Duration", 0x70: "Magnitude", 0x77: "Effect Operation", 0x7D: "Device Gain",
    0x96: "Device Control", 0x97: "DC Enable Actuators",
}

COLLECTION_TYPES = {0: "Physical", 1: "Application", 2: "Logical", 3: "Report", 4: "Named Array",
                    5: "Usage Switch", 6: "Usage Modifier"}


def usage_page_name(page: int) -> str:
    if page >= 0xFF00:
        return f"Vendor 0x{page:04x}"
    return USAGE_PAGES.get(page, f"0x{page:02x}")


def usage_name(page: int, usage: int) -> str:
    table = {0x01: GENERIC_DESKTOP, 0x02: SIMULATION, 0x0F: PID}.get(page, {})
    if page == 0x09:
        return f"Button {usage}"
    return table.get(usage, f"0x{usage:02x}")


@dataclass
class Field:
    kind: str  # input | output | feature
    report_id: int
    bit_offset: int
    size: int
    count: int
    usage_page: int
    usages: list[int]
    usage_range: tuple[int, int] | None
    logical_min: int
    logical_max: int
    flags: int
    collection: str

    @property
    def constant(self) -> bool:
        return bool(self.flags & 0x01)

    @property
    def variable(self) -> bool:
        return bool(self.flags & 0x02)

    @property
    def bits(self) -> int:
        return self.size * self.count

    def describe(self) -> str:
        if self.constant:
            what = "padding"
        elif self.usage_range:
            lo, hi = self.usage_range
            what = f"{usage_page_name(self.usage_page)}: {usage_name(self.usage_page, lo)}..{usage_name(self.usage_page, hi)}"
        elif self.usages:
            names = ", ".join(usage_name(self.usage_page, u) for u in self.usages[:8])
            if len(self.usages) > 8:
                names += ", ..."
            what = f"{usage_page_name(self.usage_page)}: {names}"
        else:
            what = usage_page_name(self.usage_page)
        byte, bit = divmod(self.bit_offset, 8)
        return (f"byte {byte}.{bit} {self.count}x{self.size}bit "
                f"[{self.logical_min}..{self.logical_max}] {'var' if self.variable else 'array'} {what}")


@dataclass
class Report:
    kind: str
    report_id: int
    fields: list[Field] = field(default_factory=list)
    bits: int = 0

    @property
    def payload_len(self) -> int:
        return (self.bits + 7) // 8

    @property
    def wire_len(self) -> int:
        """Length including the report ID byte, as written to / read from hidraw."""
        return self.payload_len + (1 if self.report_id else 0)

    @property
    def usage_pages(self) -> set[int]:
        return {f.usage_page for f in self.fields if not f.constant}


@dataclass
class Descriptor:
    raw: bytes
    reports: list[Report]
    applications: list[str]
    uses_report_ids: bool

    def by_kind(self, kind: str) -> list[Report]:
        return [r for r in self.reports if r.kind == kind]

    @property
    def has_pid(self) -> bool:
        return any(0x0F in r.usage_pages for r in self.reports)

    @property
    def vendor_pages(self) -> set[int]:
        return {p for r in self.reports for p in r.usage_pages if p >= 0xFF00}

    def summary(self) -> list[str]:
        lines = [f"applications: {', '.join(self.applications) or '-'}",
                 f"report ids: {'yes' if self.uses_report_ids else 'no'}  "
                 f"PID force feedback page: {'YES' if self.has_pid else 'no'}  "
                 f"vendor pages: {', '.join(f'0x{p:04x}' for p in sorted(self.vendor_pages)) or '-'}"]
        for r in self.reports:
            lines.append(f"{r.kind:7s} report id {r.report_id:#04x}  {r.payload_len} bytes payload "
                         f"({r.wire_len} on the wire)")
            for f in r.fields:
                lines.append(f"    {f.describe()}")
        return lines


def _signed(value: int, nbytes: int) -> int:
    bits = nbytes * 8
    if nbytes and value & (1 << (bits - 1)):
        return value - (1 << bits)
    return value


def parse(raw: bytes) -> Descriptor:
    g = {"page": 0, "lmin": 0, "lmax": 0, "size": 0, "count": 0, "rid": 0}
    stack: list[dict] = []
    usages: list[int] = []
    umin = umax = None
    local_page: int | None = None  # page carried by a 32-bit extended usage
    collections: list[str] = []
    applications: list[str] = []
    reports: dict[tuple[str, int], Report] = {}
    uses_ids = False

    i = 0
    while i < len(raw):
        prefix = raw[i]
        if prefix == 0xFE:  # long item
            if i + 1 >= len(raw):
                break
            i += 3 + raw[i + 1]
            continue
        size = (0, 1, 2, 4)[prefix & 3]
        typ = (prefix >> 2) & 3
        tag = prefix >> 4
        data = raw[i + 1 : i + 1 + size]
        i += 1 + size
        uval = int.from_bytes(data, "little")
        sval = _signed(uval, len(data))

        if typ == 0:  # main
            if tag in (0x8, 0x9, 0xB):
                kind = {0x8: "input", 0x9: "output", 0xB: "feature"}[tag]
                rep = reports.setdefault((kind, g["rid"]), Report(kind, g["rid"]))
                fl = Field(
                    kind=kind, report_id=g["rid"], bit_offset=rep.bits, size=g["size"], count=g["count"],
                    usage_page=local_page or g["page"], usages=[u & 0xFFFF for u in usages],
                    usage_range=(umin, umax) if umin is not None and umax is not None else None,
                    logical_min=g["lmin"], logical_max=g["lmax"], flags=uval, collection="/".join(collections),
                )
                rep.fields.append(fl)
                rep.bits += fl.bits
            elif tag == 0xA:  # collection
                ctype = COLLECTION_TYPES.get(uval, f"0x{uval:02x}")
                uname = usage_name(g["page"], usages[0]) if usages else "?"
                label = f"{ctype}({usage_page_name(g['page'])}:{uname})"
                if uval == 1:
                    applications.append(label)
                collections.append(label)
            elif tag == 0xC and collections:
                collections.pop()
            usages, umin, umax, local_page = [], None, None, None
        elif typ == 1:  # global
            if tag == 0x0:
                g["page"] = uval
            elif tag == 0x1:
                g["lmin"] = sval
            elif tag == 0x2:
                g["lmax"] = sval if g["lmin"] < 0 else uval
            elif tag == 0x7:
                g["size"] = uval
            elif tag == 0x8:
                g["rid"] = uval
                uses_ids = True
            elif tag == 0x9:
                g["count"] = uval
            elif tag == 0xA:
                stack.append(dict(g))
            elif tag == 0xB and stack:
                g = stack.pop()
        elif typ == 2:  # local
            if tag == 0x0:
                if size == 4 and uval >> 16:  # extended usage: page in the high word
                    local_page = uval >> 16
                usages.append(uval & 0xFFFF)
            elif tag == 0x1:
                umin = uval & 0xFFFF
            elif tag == 0x2:
                umax = uval & 0xFFFF

    ordered = sorted(reports.values(), key=lambda r: ({"input": 0, "output": 1, "feature": 2}[r.kind], r.report_id))
    return Descriptor(raw=raw, reports=ordered, applications=applications, uses_report_ids=uses_ids)
