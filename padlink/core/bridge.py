"""Rumble bridge: physical controller -> virtual uinput pad with force feedback.

Games (through Wine/Proton/SDL) see the virtual pad. It has FF_RUMBLE, so
rumble requests reach padlink through the uinput FF upload/play protocol, and
padlink forwards them to the real controller:
  - with a profile:   hidraw output report (devices without kernel FF)
  - native FF device: evdev FF on the real node (useful for testing with a DS4)
  - otherwise:        log only, which still shows what the game asked for

Two personalities:
  xpad   (default) an Xbox 360 pad (045e:028e). Every game and SDL knows it.
         Input is translated with an input map (heuristic, or from the profile).
  mirror clone the source's capabilities 1:1 and add FF.

Hide the physical device from games (SDL_GAMECONTROLLER_IGNORE_DEVICES, see
`padlink lutris-env`) so they only see the virtual pad. The bridge also grabs
the physical event node so no other reader receives its input.
"""

from __future__ import annotations

import errno
import os
import select
import threading
import time

from evdev import AbsInfo, InputDevice, UInput, UInputError, ecodes

from padlink.core import ff as fflib
from padlink.core import profiles
from padlink.core.discovery import PADLINK_VIRTUAL_PHYS, Controller

E = ecodes

XPAD_KEYS = [E.BTN_A, E.BTN_B, E.BTN_X, E.BTN_Y, E.BTN_TL, E.BTN_TR, E.BTN_SELECT, E.BTN_START,
             E.BTN_MODE, E.BTN_THUMBL, E.BTN_THUMBR]
STICK = AbsInfo(value=0, min=-32768, max=32767, fuzz=16, flat=128, resolution=0)
TRIGGER = AbsInfo(value=0, min=0, max=255, fuzz=0, flat=0, resolution=0)
HAT = AbsInfo(value=0, min=-1, max=1, fuzz=0, flat=0, resolution=0)
XPAD_ABS = {E.ABS_X: STICK, E.ABS_Y: STICK, E.ABS_RX: STICK, E.ABS_RY: STICK,
            E.ABS_Z: TRIGGER, E.ABS_RZ: TRIGGER, E.ABS_HAT0X: HAT, E.ABS_HAT0Y: HAT}
FF_CAPS = [E.FF_RUMBLE, E.FF_PERIODIC, E.FF_SQUARE, E.FF_TRIANGLE, E.FF_SINE, E.FF_CONSTANT,
           E.FF_RAMP, E.FF_GAIN]

DPAD_BUTTONS = {E.BTN_DPAD_LEFT: (E.ABS_HAT0X, -1), E.BTN_DPAD_RIGHT: (E.ABS_HAT0X, 1),
                E.BTN_DPAD_UP: (E.ABS_HAT0Y, -1), E.BTN_DPAD_DOWN: (E.ABS_HAT0Y, 1)}


def code_name(etype: int, code: int) -> str:
    table = {E.EV_KEY: E.keys, E.EV_ABS: E.ABS, E.EV_FF: E.FF, E.EV_REL: E.REL, E.EV_MSC: E.MSC,
             E.EV_SYN: E.SYN}.get(etype, {})
    name = table.get(code, str(code))
    if isinstance(name, (list, tuple)):
        name = name[0]
    return name


def code_of(name: str) -> int:
    value = getattr(E, name, None)
    if value is None:
        raise ValueError(f"unknown evdev code {name!r}")
    return value


def heuristic_map(keys: set[int], abses: set[int]) -> dict:
    """Guess an xpad mapping from the source's evdev capabilities.

    Matches the Linux gamepad convention and the Android HID layout most
    2.4G dongles use (buttons 1..15 -> BTN_SOUTH.., right stick on Z/RZ,
    analog triggers on BRAKE/GAS).
    """
    m: dict = {"keys": {}, "axes": {}, "key_to_axis": {}, "dpad_buttons": False}
    gamepad_keys = {E.BTN_A: "BTN_A", E.BTN_B: "BTN_B", E.BTN_X: "BTN_X", E.BTN_Y: "BTN_Y",
                    E.BTN_TL: "BTN_TL", E.BTN_TR: "BTN_TR", E.BTN_SELECT: "BTN_SELECT",
                    E.BTN_START: "BTN_START", E.BTN_MODE: "BTN_MODE", E.BTN_THUMBL: "BTN_THUMBL",
                    E.BTN_THUMBR: "BTN_THUMBR"}
    if keys & set(range(0x130, 0x13F)):
        for code, target in gamepad_keys.items():
            if code in keys:
                m["keys"][code_name(E.EV_KEY, code)] = target
    else:
        # Generic joystick buttons (BTN_TRIGGER..). Common DirectInput order.
        order = ["BTN_X", "BTN_A", "BTN_B", "BTN_Y", "BTN_TL", "BTN_TR", "@ABS_Z", "@ABS_RZ",
                 "BTN_SELECT", "BTN_START", "BTN_THUMBL", "BTN_THUMBR", "BTN_MODE"]
        src = sorted(k for k in keys if 0x120 <= k < 0x130 or 0x2C0 <= k < 0x2E8)
        for code, target in zip(src, order):
            if target.startswith("@"):
                m["key_to_axis"][code_name(E.EV_KEY, code)] = target[1:]
            else:
                m["keys"][code_name(E.EV_KEY, code)] = target

    def axis(src: int, dst: str) -> None:
        if src in abses:
            m["axes"][code_name(E.EV_ABS, src)] = {"to": dst, "invert": False}

    axis(E.ABS_X, "ABS_X")
    axis(E.ABS_Y, "ABS_Y")
    if E.ABS_RX in abses and E.ABS_RY in abses:
        axis(E.ABS_RX, "ABS_RX")
        axis(E.ABS_RY, "ABS_RY")
        axis(E.ABS_Z, "ABS_Z")
        axis(E.ABS_RZ, "ABS_RZ")
    else:
        axis(E.ABS_Z, "ABS_RX")
        axis(E.ABS_RZ, "ABS_RY")
    axis(E.ABS_BRAKE, "ABS_Z")
    axis(E.ABS_GAS, "ABS_RZ")
    axis(E.ABS_HAT0X, "ABS_HAT0X")
    axis(E.ABS_HAT0Y, "ABS_HAT0Y")

    has_trigger_axes = any(v["to"] in ("ABS_Z", "ABS_RZ") for v in m["axes"].values())
    if not has_trigger_axes:
        if E.BTN_TL2 in keys:
            m["key_to_axis"]["BTN_TL2"] = "ABS_Z"
        if E.BTN_TR2 in keys:
            m["key_to_axis"]["BTN_TR2"] = "ABS_RZ"
    if keys & set(DPAD_BUTTONS):
        m["dpad_buttons"] = True
    return m


class Translator:
    """Applies an input map to evdev events. Pure logic, testable."""

    def __init__(self, mapping: dict, absinfo: dict[int, AbsInfo]):
        self.keys = {code_of(k): code_of(v) for k, v in mapping.get("keys", {}).items()}
        self.key_to_axis = {code_of(k): code_of(v) for k, v in mapping.get("key_to_axis", {}).items()}
        self.axes = {}
        for src, spec in mapping.get("axes", {}).items():
            scode = code_of(src)
            self.axes[scode] = (code_of(spec["to"]), bool(spec.get("invert")), absinfo.get(scode))
        self.dpad = bool(mapping.get("dpad_buttons"))
        self.hat_state = {E.ABS_HAT0X: [0, 0], E.ABS_HAT0Y: [0, 0]}  # [neg, pos] pressed

    @staticmethod
    def scale(value: int, src: AbsInfo | None, dst: AbsInfo, invert: bool) -> int:
        if src is None or src.max == src.min:
            out = value
        else:
            frac = (value - src.min) / (src.max - src.min)
            if invert:
                frac = 1.0 - frac
            out = round(dst.min + frac * (dst.max - dst.min))
        return max(dst.min, min(dst.max, out))

    def translate(self, etype: int, code: int, value: int) -> list[tuple[int, int, int]]:
        if etype == E.EV_KEY:
            if code in self.keys:
                return [(E.EV_KEY, self.keys[code], value)]
            if code in self.key_to_axis:
                dst = self.key_to_axis[code]
                return [(E.EV_ABS, dst, XPAD_ABS[dst].max if value else 0)]
            if self.dpad and code in DPAD_BUTTONS:
                axis, direction = DPAD_BUTTONS[code]
                state = self.hat_state[axis]
                state[0 if direction < 0 else 1] = 1 if value else 0
                return [(E.EV_ABS, axis, state[1] - state[0])]
            return []
        if etype == E.EV_ABS and code in self.axes:
            dst, invert, src_info = self.axes[code]
            if dst in (E.ABS_HAT0X, E.ABS_HAT0Y):
                return [(E.EV_ABS, dst, max(-1, min(1, value)))]
            return [(E.EV_ABS, dst, self.scale(value, src_info, XPAD_ABS[dst], invert))]
        return []


def pick_hidraw(ctrl: Controller, spec: profiles.RumbleSpec) -> str | None:
    if not ctrl.hidraw:
        return None
    if spec.interface is not None:
        for node in ctrl.hidraw:
            if node.interface == spec.interface:
                return node.path
    return ctrl.hidraw[0].path


def make_sink(ctrl: Controller, profile: profiles.Profile | None, log) -> tuple[fflib.RumbleSink, profiles.RumbleSpec | None]:
    if profile and profile.rumble and profile.rumble.transport == "hidraw":
        path = pick_hidraw(ctrl, profile.rumble)
        if path:
            try:
                return fflib.HidrawSink(path, profile.rumble, log), profile.rumble
            except OSError as exc:
                log.write("note", text=f"cannot open {path} for rumble ({exc}); FF requests will only be logged. "
                                       f"Install the udev rule: padlink udev-rule {ctrl.id_str}")
                return fflib.NullSink(), None
    node = ctrl.ff_node
    if node is not None:
        return fflib.EvdevSink(node.path, log), None
    return fflib.NullSink(), None


class Bridge:
    """Run with start()/stop() (thread) or run() (blocking)."""

    def __init__(self, ctrl: Controller, log, mode: str = "xpad", grab: bool = True,
                 log_input: bool = False, mapping: dict | None = None,
                 vendor: int | None = None, product: int | None = None, name: str | None = None):
        self.ctrl = ctrl
        self.log = log
        self.mode = mode
        self.grab = grab
        self.log_input = log_input
        self.mapping_override = mapping
        self.vendor = vendor
        self.product = product
        self.name = name
        self.ui: UInput | None = None
        self.src: InputDevice | None = None
        self.driver: fflib.RumbleDriver | None = None
        self.translator: Translator | None = None
        self.thread: threading.Thread | None = None
        self._stop_r, self._stop_w = os.pipe()
        self.error: str | None = None
        self.virtual_path: str | None = None
        self.stats = {"uploads": 0, "plays": 0, "inputs": 0}

    # -- setup ---------------------------------------------------------
    def _open(self) -> None:
        node = self.ctrl.primary_event
        if node is None:
            raise RuntimeError("controller has no event node")
        self.src = InputDevice(node.path)
        caps = self.src.capabilities(absinfo=True)
        keys = set(caps.get(E.EV_KEY, []))
        absinfo = {code: info for code, info in caps.get(E.EV_ABS, [])}
        profile = profiles.load(self.ctrl.vendor, self.ctrl.product)

        tag = f"{PADLINK_VIRTUAL_PHYS}/{self.ctrl.id_str}"
        if self.mode == "mirror":
            events = {t: c for t, c in caps.items() if t not in (E.EV_SYN, E.EV_FF)}
            events[E.EV_FF] = FF_CAPS
            self.ui = UInput(events, name=self.name or f"{self.src.name} (padlink)",
                             vendor=self.vendor if self.vendor is not None else self.ctrl.vendor,
                             product=self.product if self.product is not None else self.ctrl.product,
                             version=0x0110, bustype=E.BUS_USB, phys=tag, max_effects=16)
            self.translator = None
        else:
            mapping = self.mapping_override or (profile.input_map if profile and profile.input_map else None) \
                or heuristic_map(keys, set(absinfo))
            self.translator = Translator(mapping, absinfo)
            events = {E.EV_KEY: XPAD_KEYS, E.EV_ABS: list(XPAD_ABS.items()), E.EV_FF: FF_CAPS}
            self.ui = UInput(events, name=self.name or "Microsoft X-Box 360 pad",
                             vendor=self.vendor if self.vendor is not None else 0x045E,
                             product=self.product if self.product is not None else 0x028E,
                             version=0x0110, bustype=E.BUS_USB, phys=tag, max_effects=16)
            self.log.write("note", text="input map", mapping=mapping,
                           source="override" if self.mapping_override else
                           ("profile" if profile and profile.input_map else "heuristic"))
        self.virtual_path = self.ui.device.path if self.ui.device else None

        sink, spec = make_sink(self.ctrl, profile, self.log)
        self.driver = fflib.RumbleDriver(sink, self.log, keepalive_ms=spec.keepalive_ms if spec else 0,
                                         min_interval_ms=spec.min_interval_ms if spec else 8)
        self.driver.start()
        if self.grab:
            try:
                self.src.grab()
            except OSError as exc:
                self.log.write("note", text=f"could not grab {node.path}: {exc}")
        self.log.write("note", text="bridge up", source=node.path, virtual=self.virtual_path,
                       mode=self.mode, sink=sink.name)

    # -- event handling ---------------------------------------------------
    def _handle_source(self) -> None:
        assert self.src and self.ui
        for ev in self.src.read():
            if ev.type == E.EV_SYN:
                self.ui.syn()
                continue
            if self.log_input and ev.type in (E.EV_KEY, E.EV_ABS):
                self.log.write("input", node=self.src.path, type=ev.type, code=ev.code, value=ev.value,
                               name=code_name(ev.type, ev.code))
            self.stats["inputs"] += 1
            if self.translator is None:
                if ev.type != E.EV_FF:
                    self.ui.write(ev.type, ev.code, ev.value)
            else:
                for t, c, v in self.translator.translate(ev.type, ev.code, ev.value):
                    self.ui.write(t, c, v)

    def _handle_uinput(self) -> None:
        assert self.ui and self.driver
        for ev in self.ui.read():
            if ev.type == E.EV_UINPUT:
                if ev.code == E.UI_FF_UPLOAD:
                    try:
                        upload = self.ui.begin_upload(ev.value)
                    except UInputError as exc:  # requester gave up (timeout) before we got to it
                        self.log.write("note", text=f"stale FF upload request: {exc}")
                        continue
                    try:
                        eff = fflib.effect_to_dict(upload.effect)
                        self.driver.upload(eff)
                        self.stats["uploads"] += 1
                        self.log.write("ff_upload", id=eff["id"], effect=eff)
                        upload.retval = 0
                    except Exception as exc:  # never leave the game's ioctl hanging
                        upload.retval = -errno.EINVAL
                        self.log.write("note", text=f"upload failed: {exc}")
                    self.ui.end_upload(upload)
                elif ev.code == E.UI_FF_ERASE:
                    try:
                        erase = self.ui.begin_erase(ev.value)
                    except UInputError as exc:
                        self.log.write("note", text=f"stale FF erase request: {exc}")
                        continue
                    self.driver.erase(int(erase.effect_id))
                    self.log.write("ff_erase", id=int(erase.effect_id))
                    erase.retval = 0
                    self.ui.end_erase(erase)
            elif ev.type == E.EV_FF:
                if ev.code == E.FF_GAIN:
                    self.driver.set_gain(ev.value)
                    self.log.write("ff_gain", gain=ev.value)
                else:
                    self.driver.play(ev.code, ev.value)
                    self.stats["plays"] += 1
                    self.log.write("ff_play", id=ev.code, value=ev.value)

    def run(self) -> None:
        try:
            self._open()
            assert self.src and self.ui
            fds = {self.src.fd: self._handle_source, self.ui.fd: self._handle_uinput, self._stop_r: None}
            while True:
                ready, _, _ = select.select(list(fds), [], [])
                if self._stop_r in ready:
                    break
                for fd in ready:
                    fds[fd]()
        except OSError as exc:
            if exc.errno == errno.ENODEV:
                self.error = "controller disconnected"
            else:
                self.error = str(exc)
            self.log.write("note", text=f"bridge stopped: {self.error}")
        except Exception as exc:
            self.error = f"{type(exc).__name__}: {exc}"
            self.log.write("note", text=f"bridge crashed: {self.error}")
        finally:
            self._teardown()

    def _teardown(self) -> None:
        if self.driver:
            self.driver.stop()
            self.driver = None
        if self.src:
            try:
                if self.grab:
                    self.src.ungrab()
            except OSError:
                pass
            self.src.close()
            self.src = None
        if self.ui:
            self.ui.close()
            self.ui = None
        self.log.write("note", text="bridge down", stats=self.stats)

    def start(self) -> None:
        self.thread = threading.Thread(target=self.run, daemon=True, name="padlink-bridge")
        self.thread.start()

    def stop(self) -> None:
        os.write(self._stop_w, b"x")
        if self.thread:
            self.thread.join(timeout=3)

    @property
    def alive(self) -> bool:
        return bool(self.thread and self.thread.is_alive())


def wait_for(spec: str, include_all: bool = False, poll: float = 2.0) -> Controller:
    """Block until a controller matching spec appears (ignores padlink's own pads)."""
    from padlink.core import discovery

    while True:
        try:
            ctrl = discovery.resolve(spec, [c for c in discovery.scan(include_all) if not c.is_padlink])
            if ctrl.primary_event and ctrl.primary_event.readable:
                return ctrl
        except discovery.NoSuchController:
            pass
        time.sleep(poll)
