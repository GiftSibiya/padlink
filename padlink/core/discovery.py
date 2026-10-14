"""Controller discovery via udev/sysfs.

Everything here works without access to the device nodes themselves: names,
IDs, capability bitmasks and HID report descriptors all come from sysfs, which
is world-readable. Node permissions are reported so the doctor can explain
what is missing.
"""

from __future__ import annotations

import os
from dataclasses import dataclass, field
from pathlib import Path

import pyudev
from evdev import ecodes

BUS_NAMES = {0x03: "usb", 0x05: "bluetooth", 0x06: "virtual", 0x18: "i2c", 0x19: "host"}

# Vendor/product pairs worth recognising by name in the UI and doctor.
KNOWN_DEVICES = {
    (0x3537, 0x1041): "GameSir / Zikway 2.4G dongle (HID mode)",
    (0x045E, 0x028E): "Xbox 360 controller (or XInput-mode clone)",
    (0x054C, 0x05C4): "Sony DualShock 4 (v1)",
    (0x054C, 0x09CC): "Sony DualShock 4 (v2)",
    (0x054C, 0x0CE6): "Sony DualSense",
}

PADLINK_VIRTUAL_PHYS = "padlink/virtual"


def parse_bitmask(text: str | None) -> set[int]:
    """Parse a sysfs capability bitmask ("1f 0 ffff...", most significant word first)."""
    if not text:
        return set()
    bits: set[int] = set()
    word_bits = _long_bits()  # the kernel prints unpadded unsigned longs
    for idx, word in enumerate(reversed(text.split())):
        value = int(word, 16)
        base = idx * word_bits
        bit = 0
        while value:
            if value & 1:
                bits.add(base + bit)
            value >>= 1
            bit += 1
    return bits


def _long_bits() -> int:
    import struct

    return struct.calcsize("l") * 8


def _code_name(table: dict, code: int) -> str:
    name = table.get(code, str(code))
    if isinstance(name, (list, tuple)):
        name = name[0]
    return name


@dataclass
class EventNode:
    path: str
    sys_path: str
    name: str
    input_sys_path: str
    caps: dict[str, set[int]]
    props: dict[str, str]

    @property
    def readable(self) -> bool:
        return os.access(self.path, os.R_OK)

    @property
    def writable(self) -> bool:
        return os.access(self.path, os.W_OK)

    @property
    def is_joystick(self) -> bool:
        return self.props.get("ID_INPUT_JOYSTICK") == "1"

    @property
    def has_ff(self) -> bool:
        # EV_FF alone isn't enough: uinput sets it whenever ff_effects_max > 0
        ff_codes = self.caps.get("ff", set()) - {ecodes.FF_GAIN, ecodes.FF_AUTOCENTER}
        return ecodes.EV_FF in self.caps.get("ev", set()) and bool(ff_codes)

    @property
    def ff_names(self) -> list[str]:
        return [_code_name(ecodes.FF, c) for c in sorted(self.caps.get("ff", set()))]

    @property
    def role(self) -> str:
        if self.is_joystick:
            return "joystick"
        for prop, role in (
            ("ID_INPUT_ACCELEROMETER", "motion"),
            ("ID_INPUT_TOUCHPAD", "touchpad"),
            ("ID_INPUT_MOUSE", "mouse"),
            ("ID_INPUT_KEYBOARD", "keyboard"),
            ("ID_INPUT_KEY", "keys"),
        ):
            if self.props.get(prop) == "1":
                return role
        return "other"

    def to_dict(self) -> dict:
        return {
            "path": self.path,
            "name": self.name,
            "role": self.role,
            "readable": self.readable,
            "writable": self.writable,
            "ff": self.ff_names,
            "buttons": len(self.caps.get("key", ())),
            "axes": len(self.caps.get("abs", ())),
        }


@dataclass
class HidrawNode:
    path: str
    sys_path: str
    hid_sys_path: str
    interface: str | None

    @property
    def readable(self) -> bool:
        return os.access(self.path, os.R_OK)

    @property
    def writable(self) -> bool:
        return os.access(self.path, os.W_OK)

    def descriptor(self) -> bytes:
        try:
            return Path(self.hid_sys_path, "report_descriptor").read_bytes()
        except OSError:
            from padlink.core import hidraw

            with hidraw.HidrawDevice(self.path) as dev:
                return dev.report_descriptor()

    def to_dict(self) -> dict:
        return {
            "path": self.path,
            "interface": self.interface,
            "readable": self.readable,
            "writable": self.writable,
        }


@dataclass
class Controller:
    key: str
    name: str
    vendor: int
    product: int
    version: int
    bustype: int
    driver: str | None
    uniq: str
    phys: str
    virtual: bool
    events: list[EventNode] = field(default_factory=list)
    joysticks: list[str] = field(default_factory=list)
    hidraw: list[HidrawNode] = field(default_factory=list)
    battery: dict | None = None
    index: int = 0

    @property
    def bus(self) -> str:
        if self.virtual:
            return "virtual"
        return BUS_NAMES.get(self.bustype, f"0x{self.bustype:02x}")

    @property
    def id_str(self) -> str:
        return f"{self.vendor:04x}:{self.product:04x}"

    @property
    def known_as(self) -> str | None:
        return KNOWN_DEVICES.get((self.vendor, self.product))

    @property
    def is_padlink(self) -> bool:
        return self.phys.startswith(PADLINK_VIRTUAL_PHYS)

    @property
    def primary_event(self) -> EventNode | None:
        for node in self.events:
            if node.is_joystick:
                return node
        return self.events[0] if self.events else None

    @property
    def has_ff(self) -> bool:
        return any(n.has_ff for n in self.events)

    @property
    def ff_node(self) -> EventNode | None:
        for node in self.events:
            if node.has_ff:
                return node
        return None

    def to_dict(self) -> dict:
        return {
            "index": self.index,
            "name": self.name,
            "id": self.id_str,
            "known_as": self.known_as,
            "bus": self.bus,
            "driver": self.driver,
            "uniq": self.uniq,
            "phys": self.phys,
            "virtual": self.virtual,
            "padlink_virtual": self.is_padlink,
            "sys_path": self.key,
            "has_ff": self.has_ff,
            "events": [n.to_dict() for n in self.events],
            "joysticks": self.joysticks,
            "hidraw": [n.to_dict() for n in self.hidraw],
            "battery": self.battery,
        }


def _attr(dev: pyudev.Device, name: str) -> str | None:
    try:
        return dev.attributes.asstring(name).strip()
    except (KeyError, UnicodeDecodeError, OSError):
        return None


def _root_for(dev: pyudev.Device) -> pyudev.Device:
    usb = dev.find_parent("usb", "usb_device")
    if usb is not None:
        return usb
    hid = dev.find_parent("hid")
    if hid is not None:
        return hid
    return dev.parent  # the inputN device (uinput / platform)


def _driver_for(dev: pyudev.Device) -> str | None:
    hid = dev.find_parent("hid")
    if hid is not None and hid.driver:
        return hid.driver
    intf = dev.find_parent("usb", "usb_interface")
    if intf is not None and intf.driver:
        return intf.driver
    return None


def _event_node(dev: pyudev.Device) -> EventNode:
    inp = dev.parent
    caps = {k: parse_bitmask(_attr(inp, f"capabilities/{k}")) for k in ("ev", "key", "abs", "ff", "rel")}
    return EventNode(
        path=dev.device_node,
        sys_path=dev.sys_path,
        name=_attr(inp, "name") or "?",
        input_sys_path=inp.sys_path,
        caps=caps,
        props={k: v for k, v in dev.properties.items() if k.startswith("ID_INPUT")},
    )


def _battery_for(ctx: pyudev.Context, root: str) -> dict | None:
    for ps in ctx.list_devices(subsystem="power_supply"):
        if ps.sys_path.startswith(root + "/"):
            return {
                "name": ps.sys_name,
                "capacity": _attr(ps, "capacity"),
                "status": _attr(ps, "status"),
            }
    return None


def scan(include_all: bool = False, ctx: pyudev.Context | None = None) -> list[Controller]:
    """Find game controllers.

    A controller is everything under one physical root (USB device, HID device,
    or virtual input device) that has at least one joystick event node. With
    include_all, any root owning a HID-backed or virtual event node is listed,
    which helps when a dongle is misclassified by udev.
    """
    ctx = ctx or pyudev.Context()
    roots: dict[str, pyudev.Device] = {}
    events = [d for d in ctx.list_devices(subsystem="input") if d.sys_name.startswith("event") and d.device_node]
    for dev in events:
        is_joy = dev.properties.get("ID_INPUT_JOYSTICK") == "1"
        hid_backed = dev.find_parent("hid") is not None or dev.find_parent("usb") is not None
        if is_joy or (include_all and hid_backed):
            root = _root_for(dev)
            roots.setdefault(root.sys_path, root)

    hidraws = list(ctx.list_devices(subsystem="hidraw"))
    joysticks = [d for d in ctx.list_devices(subsystem="input") if d.sys_name.startswith("js") and d.device_node]

    controllers: list[Controller] = []
    for root_path, root in sorted(roots.items()):
        prefix = root_path + "/"
        nodes = [_event_node(d) for d in events if d.sys_path.startswith(prefix)]
        nodes.sort(key=lambda n: (not n.is_joystick, int(n.path.rsplit("event", 1)[1])))
        primary = next((n for n in nodes if n.is_joystick), nodes[0] if nodes else None)
        if primary is None:
            continue
        pdev = pyudev.Devices.from_sys_path(ctx, primary.sys_path)
        inp = pdev.parent

        def hexattr(name: str) -> int:
            try:
                return int(_attr(inp, f"id/{name}") or "0", 16)
            except ValueError:
                return 0

        hid_nodes = []
        for h in hidraws:
            if h.sys_path.startswith(prefix) and h.device_node:
                hid = h.find_parent("hid")
                intf = h.find_parent("usb", "usb_interface")
                hid_nodes.append(
                    HidrawNode(
                        path=h.device_node,
                        sys_path=h.sys_path,
                        hid_sys_path=hid.sys_path if hid is not None else h.sys_path,
                        interface=_attr(intf, "bInterfaceNumber") if intf is not None else None,
                    )
                )
        hid_nodes.sort(key=lambda n: int(n.path.rsplit("hidraw", 1)[1]))

        controllers.append(
            Controller(
                key=root_path,
                name=primary.name,
                vendor=hexattr("vendor"),
                product=hexattr("product"),
                version=hexattr("version"),
                bustype=hexattr("bustype"),
                driver=_driver_for(pdev),
                uniq=_attr(inp, "uniq") or "",
                phys=_attr(inp, "phys") or "",
                virtual=root_path.startswith("/sys/devices/virtual/input"),
                events=nodes,
                joysticks=sorted(j.device_node for j in joysticks if j.sys_path.startswith(prefix)),
                hidraw=hid_nodes,
                battery=_battery_for(ctx, root_path),
            )
        )
    for i, c in enumerate(controllers):
        c.index = i
    return controllers


class NoSuchController(LookupError):
    pass


def resolve(spec: str | None, controllers: list[Controller] | None = None, include_all: bool = False) -> Controller:
    """Pick a controller by index, vid:pid, device node path, or name substring.

    With no spec, the first non-padlink controller is chosen.
    """
    controllers = controllers if controllers is not None else scan(include_all=include_all)
    if not controllers:
        raise NoSuchController("no controllers found (try --all, or check `padlink doctor`)")
    if spec is None:
        real = [c for c in controllers if not c.is_padlink]
        return (real or controllers)[0]
    if spec.isdigit() and int(spec) < len(controllers):
        return controllers[int(spec)]
    low = spec.lower()
    for c in controllers:
        if low == c.id_str:
            return c
    for c in controllers:
        paths = [n.path for n in c.events] + [n.path for n in c.hidraw] + c.joysticks
        if spec in paths:
            return c
    for c in controllers:
        if low in c.name.lower():
            return c
    raise NoSuchController(f"no controller matches {spec!r}")


def holders(paths: list[str]) -> list[dict]:
    """Processes (that we are allowed to inspect) holding any of the given nodes open."""
    targets = set()
    for p in paths:
        try:
            targets.add(os.stat(p).st_rdev)
        except OSError:
            pass
    found = []
    me = os.getpid()
    for pid_dir in Path("/proc").iterdir():
        if not pid_dir.name.isdigit() or int(pid_dir.name) == me:
            continue
        try:
            fds = list((pid_dir / "fd").iterdir())
        except OSError:
            continue
        hits = set()
        for fd in fds:
            try:
                st = os.stat(fd)
            except OSError:
                continue
            if st.st_rdev and st.st_rdev in targets:
                hits.add(os.readlink(fd))
        if hits:
            try:
                comm = (pid_dir / "comm").read_text().strip()
            except OSError:
                comm = "?"
            found.append({"pid": int(pid_dir.name), "comm": comm, "nodes": sorted(hits)})
    return found


def udev_monitor(ctx: pyudev.Context | None = None) -> pyudev.Monitor:
    """A udev monitor for input/hidraw/power_supply hotplug events."""
    ctx = ctx or pyudev.Context()
    mon = pyudev.Monitor.from_netlink(ctx)
    mon.filter_by("input")
    mon.filter_by("hidraw")
    mon.filter_by("power_supply")
    return mon
