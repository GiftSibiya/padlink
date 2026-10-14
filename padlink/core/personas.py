"""Virtual pad personas: what the bridge's uinput device pretends to be.

The bridge translates the physical controller into canonical xpad events
(Xbox 360 codes and ranges). A persona then re-encodes those events the way
the *real* Linux driver for that controller would emit them: same name,
vendor/product/version, button codes, axis ranges, trigger and d-pad style.
That way SDL, Wine and games pick their built-in mapping for that controller.

Version numbers follow what the kernel drivers report over USB (these end up
in SDL's joystick GUID and select its controller mapping).
"""

from __future__ import annotations

from dataclasses import dataclass, field

from evdev import AbsInfo, ecodes as E

# canonical (xpad) codes: 0x133 = BTN_X = left face button, 0x134 = BTN_Y = top
X_LEFT, Y_TOP = 0x133, 0x134

XPAD_STICK = AbsInfo(value=0, min=-32768, max=32767, fuzz=16, flat=128, resolution=0)
XPAD_TRIGGER = AbsInfo(value=0, min=0, max=255, fuzz=0, flat=0, resolution=0)
HAT = AbsInfo(value=0, min=-1, max=1, fuzz=0, flat=0, resolution=0)

STICK_AXES = (E.ABS_X, E.ABS_Y, E.ABS_RX, E.ABS_RY)
TRIGGER_AXES = {E.ABS_Z: E.BTN_TL2, E.ABS_RZ: E.BTN_TR2}
DIGITAL_TRIGGER_THRESHOLD = 64  # of 255

COMMON_KEYS = [E.BTN_TL, E.BTN_TR, E.BTN_SELECT, E.BTN_START, E.BTN_MODE, E.BTN_THUMBL, E.BTN_THUMBR]


@dataclass
class Persona:
    id: str
    label: str
    description: str
    name: str
    vendor: int
    product: int
    version: int
    bustype: int = E.BUS_USB
    face: dict = field(default_factory=dict)  # canonical face key -> persona key
    stick: AbsInfo = XPAD_STICK
    trigger: AbsInfo | None = XPAD_TRIGGER  # None: digital triggers only
    trigger_buttons: bool = False  # also emit BTN_TL2/BTN_TR2 (hid-playstation does)
    spec_layout: bool = False  # follows Documentation/input/gamepad.rst (BTN_NORTH = top)

    @property
    def id_str(self) -> str:
        return f"{self.vendor:04x}:{self.product:04x}"

    def capabilities(self) -> dict:
        keys = sorted(set(self.face.values()) | set(COMMON_KEYS)
                      | ({E.BTN_TL2, E.BTN_TR2} if self.trigger is None or self.trigger_buttons else set()))
        absinfo = [(a, self.stick) for a in STICK_AXES]
        if self.trigger is not None:
            absinfo += [(E.ABS_Z, self.trigger), (E.ABS_RZ, self.trigger)]
        absinfo += [(E.ABS_HAT0X, HAT), (E.ABS_HAT0Y, HAT)]
        return {E.EV_KEY: keys, E.EV_ABS: absinfo}

    @staticmethod
    def _rescale(value: int, src: AbsInfo, dst: AbsInfo) -> int:
        frac = (value - src.min) / (src.max - src.min)
        return max(dst.min, min(dst.max, round(dst.min + frac * (dst.max - dst.min))))

    def convert(self, etype: int, code: int, value: int) -> list[tuple[int, int, int]]:
        """Canonical xpad event -> this persona's events."""
        if etype == E.EV_KEY:
            if code in self.face:
                return [(E.EV_KEY, self.face[code], value)]
            if code in COMMON_KEYS:
                return [(E.EV_KEY, code, value)]
            return []
        if etype != E.EV_ABS:
            return []
        if code in STICK_AXES:
            return [(E.EV_ABS, code, self._rescale(value, XPAD_STICK, self.stick))]
        if code in TRIGGER_AXES:
            out = []
            if self.trigger is not None:
                out.append((E.EV_ABS, code, self._rescale(value, XPAD_TRIGGER, self.trigger)))
            if self.trigger is None or self.trigger_buttons:
                out.append((E.EV_KEY, TRIGGER_AXES[code], 1 if value >= DIGITAL_TRIGGER_THRESHOLD else 0))
            return out
        if code in (E.ABS_HAT0X, E.ABS_HAT0Y):
            return [(E.EV_ABS, code, value)]
        return []


XBOX_FACE = {E.BTN_A: E.BTN_A, E.BTN_B: E.BTN_B, X_LEFT: X_LEFT, Y_TOP: Y_TOP}
# gamepad.rst positional: south, east, west (0x134), north (0x133)
SPEC_FACE = {E.BTN_A: E.BTN_SOUTH, E.BTN_B: E.BTN_EAST, X_LEFT: E.BTN_WEST, Y_TOP: E.BTN_NORTH}

PERSONAS: dict[str, Persona] = {p.id: p for p in [
    Persona("xbox360", "Xbox 360", "Most compatible: every game, Wine and SDL know it (xpad)",
            "Microsoft X-Box 360 pad", 0x045E, 0x028E, 0x0114, face=XBOX_FACE),
    Persona("xboxone", "Xbox One S", "Xbox One S controller over USB (xpad)",
            "Microsoft X-Box One S pad", 0x045E, 0x02EA, 0x0301, face=XBOX_FACE,
            trigger=AbsInfo(0, 0, 1023, 0, 0, 0)),
    Persona("xboxseries", "Xbox Series X|S", "Xbox Series controller over USB (xpad)",
            "Microsoft Xbox Series S|X Controller", 0x045E, 0x0B12, 0x0509, face=XBOX_FACE,
            trigger=AbsInfo(0, 0, 1023, 0, 0, 0)),
    Persona("ds4", "DualShock 4", "PlayStation 4 controller over USB (hid-playstation)",
            "Sony Interactive Entertainment Wireless Controller", 0x054C, 0x09CC, 0x8111, face=SPEC_FACE,
            stick=AbsInfo(128, 0, 255, 0, 0, 0), trigger=AbsInfo(0, 0, 255, 0, 0, 0), trigger_buttons=True,
            spec_layout=True),
    Persona("dualsense", "DualSense", "PlayStation 5 controller over USB (hid-playstation)",
            "Sony Interactive Entertainment DualSense Wireless Controller", 0x054C, 0x0CE6, 0x8111,
            face=SPEC_FACE, stick=AbsInfo(128, 0, 255, 0, 0, 0), trigger=AbsInfo(0, 0, 255, 0, 0, 0),
            trigger_buttons=True, spec_layout=True),
    Persona("switchpro", "Switch Pro", "Nintendo Switch Pro Controller (hid-nintendo); digital triggers",
            "Nintendo Switch Pro Controller", 0x057E, 0x2009, 0x8111, face=SPEC_FACE,
            stick=AbsInfo(0, -32767, 32767, 250, 500, 0), trigger=None, spec_layout=True),
]}

ALIASES = {"xpad": "xbox360"}
DEFAULT = "xbox360"


def get(persona_id: str) -> Persona:
    pid = ALIASES.get(persona_id, persona_id)
    if pid not in PERSONAS:
        raise KeyError(f"unknown persona {persona_id!r}; choose from {', '.join(PERSONAS)}")
    return PERSONAS[pid]


def from_phys(phys: str) -> Persona | None:
    """padlink tags its virtual pads' phys as padlink/virtual/<source>/<persona>."""
    tail = phys.rsplit("/", 1)[-1]
    return PERSONAS.get(tail)
