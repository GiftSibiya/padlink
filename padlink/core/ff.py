"""Force-feedback plumbing.

- effect_to_dict(): normalise a kernel ff_effect into a plain dict
- RumbleMixer:      tracks uploaded effects and their playback, mixes them
                    into a (strong, weak) pair like the kernel's ff-memless
- sinks:            deliver (strong, weak) to hardware: a hidraw output report
                    built from a profile, or a native evdev FF device
- RumbleDriver:     thread that applies mixer output to a sink on time
- evdev_rumble():   one-shot rumble test through the kernel FF API
"""

from __future__ import annotations

import math
import threading
import time

from evdev import InputDevice, ecodes, ff

from padlink.core.hidraw import HidrawDevice, hexstr
from padlink.core.profiles import RumbleSpec

MAX = 0xFFFF

FF_TYPE_NAMES = {
    ecodes.FF_RUMBLE: "rumble", ecodes.FF_PERIODIC: "periodic", ecodes.FF_CONSTANT: "constant",
    ecodes.FF_RAMP: "ramp", ecodes.FF_SPRING: "spring", ecodes.FF_FRICTION: "friction",
    ecodes.FF_DAMPER: "damper", ecodes.FF_INERTIA: "inertia",
}
WAVEFORMS = {ecodes.FF_SQUARE: "square", ecodes.FF_TRIANGLE: "triangle", ecodes.FF_SINE: "sine",
             ecodes.FF_SAW_UP: "saw_up", ecodes.FF_SAW_DOWN: "saw_down", ecodes.FF_CUSTOM: "custom"}


def _s16_to_mag(v: int) -> int:
    return min(MAX, abs(int(v)) * 2)


def effect_to_dict(eff) -> dict:
    """Convert an evdev ff.Effect (ctypes struct) to a JSON-friendly dict.

    Every dict carries "strong"/"weak" (0..65535): the rumble motor levels this
    effect should produce on a two-motor gamepad.
    """
    etype = int(eff.type)
    d = {
        "type": FF_TYPE_NAMES.get(etype, f"0x{etype:02x}"),
        "id": int(eff.id),
        "direction": int(eff.direction),
        "length": int(eff.ff_replay.length),
        "delay": int(eff.ff_replay.delay),
        "strong": 0,
        "weak": 0,
    }
    u = eff.u
    if etype == ecodes.FF_RUMBLE:
        d["strong"] = int(u.ff_rumble_effect.strong_magnitude)
        d["weak"] = int(u.ff_rumble_effect.weak_magnitude)
    elif etype == ecodes.FF_PERIODIC:
        p = u.ff_periodic_effect
        d.update(waveform=WAVEFORMS.get(int(p.waveform), int(p.waveform)), period=int(p.period),
                 magnitude=int(p.magnitude), offset=int(p.offset))
        mag = min(MAX, _s16_to_mag(p.magnitude) + _s16_to_mag(p.offset) // 2)
        d["strong"] = d["weak"] = mag
    elif etype == ecodes.FF_CONSTANT:
        d["level"] = int(u.ff_constant_effect.level)
        d["strong"] = d["weak"] = _s16_to_mag(u.ff_constant_effect.level)
    elif etype == ecodes.FF_RAMP:
        r = u.ff_ramp_effect
        d.update(start_level=int(r.start_level), end_level=int(r.end_level))
        d["strong"] = d["weak"] = max(_s16_to_mag(r.start_level), _s16_to_mag(r.end_level))
    return d


class RumbleMixer:
    """Pure-logic effect table + playback state. Time is passed in explicitly."""

    def __init__(self):
        self.effects: dict[int, dict] = {}
        self.playing: dict[int, tuple[float, float]] = {}  # id -> (start, end)
        self.gain = MAX

    def upload(self, effect: dict) -> None:
        self.effects[effect["id"]] = effect

    def erase(self, effect_id: int) -> None:
        self.effects.pop(effect_id, None)
        self.playing.pop(effect_id, None)

    def play(self, effect_id: int, count: int, now: float) -> None:
        eff = self.effects.get(effect_id)
        if eff is None:
            return
        if count <= 0:
            self.playing.pop(effect_id, None)
            return
        start = now + eff.get("delay", 0) / 1000.0
        length = eff.get("length", 0)
        end = math.inf if length == 0 else start + count * length / 1000.0
        self.playing[effect_id] = (start, end)

    def set_gain(self, gain: int) -> None:
        self.gain = max(0, min(MAX, int(gain)))

    def stop_all(self) -> None:
        self.playing.clear()

    def levels(self, now: float) -> tuple[int, int]:
        strong = weak = 0
        for eid, (start, end) in list(self.playing.items()):
            if now >= end:
                del self.playing[eid]
                continue
            if now < start:
                continue
            eff = self.effects.get(eid)
            if eff:
                strong += eff["strong"]
                weak += eff["weak"]
        strong = min(MAX, strong) * self.gain // MAX
        weak = min(MAX, weak) * self.gain // MAX
        return strong, weak

    def next_deadline(self, now: float) -> float | None:
        times = []
        for start, end in self.playing.values():
            if start > now:
                times.append(start)
            if end != math.inf:
                times.append(end)
        return min(times) if times else None


class RumbleSink:
    name = "sink"

    def set(self, strong: int, weak: int) -> None:
        raise NotImplementedError

    def close(self) -> None:
        pass


class HidrawSink(RumbleSink):
    """Write a profile-defined output report to a hidraw node."""

    def __init__(self, path: str, spec: RumbleSpec, log=None):
        self.name = f"hidraw:{path}"
        self.path = path
        self.spec = spec
        self.log = log
        self.dev = HidrawDevice(path, write=True)

    def set(self, strong: int, weak: int) -> None:
        data = self.spec.render(strong, weak)
        try:
            self.dev.write(data)
            if self.log:
                self.log.write("hid_tx", node=self.path, hex=hexstr(data), ok=True)
        except OSError as exc:
            if self.log:
                self.log.write("hid_tx", node=self.path, hex=hexstr(data), ok=False, error=str(exc))
            raise

    def close(self) -> None:
        try:
            self.set(0, 0)
        except OSError:
            pass
        self.dev.close()


class EvdevSink(RumbleSink):
    """Drive a device that already has kernel FF support (xpad, hid-playstation...)."""

    def __init__(self, path: str, log=None):
        self.name = f"evdev:{path}"
        self.path = path
        self.log = log
        self.dev = InputDevice(path)
        self.effect_id = -1

    def set(self, strong: int, weak: int) -> None:
        if strong == 0 and weak == 0:
            if self.effect_id >= 0:
                self.dev.write(ecodes.EV_FF, self.effect_id, 0)
            return
        eff = ff.Effect(
            ecodes.FF_RUMBLE, self.effect_id, 0, ff.Trigger(0, 0), ff.Replay(0, 0),
            ff.EffectType(ff_rumble_effect=ff.Rumble(strong_magnitude=strong, weak_magnitude=weak)),
        )
        self.effect_id = self.dev.upload_effect(eff)
        self.dev.write(ecodes.EV_FF, self.effect_id, 1)

    def close(self) -> None:
        try:
            if self.effect_id >= 0:
                self.dev.write(ecodes.EV_FF, self.effect_id, 0)
                self.dev.erase_effect(self.effect_id)
        except OSError:
            pass
        self.dev.close()


class NullSink(RumbleSink):
    name = "log-only"

    def set(self, strong: int, weak: int) -> None:
        pass


class RumbleDriver(threading.Thread):
    """Applies RumbleMixer output to a sink, honouring effect timing,
    keepalive and a minimum write interval."""

    def __init__(self, sink: RumbleSink, log=None, keepalive_ms: int = 0, min_interval_ms: int = 8):
        super().__init__(daemon=True, name="padlink-rumble")
        self.sink = sink
        self.log = log
        self.mixer = RumbleMixer()
        self.keepalive = keepalive_ms / 1000.0
        self.min_interval = min_interval_ms / 1000.0
        self.cond = threading.Condition()
        self.running = True
        self.current = (0, 0)
        self.last_write = 0.0
        self.errors = 0

    # Mixer access from other threads goes through these.
    def upload(self, effect: dict) -> None:
        with self.cond:
            self.mixer.upload(effect)
            self.cond.notify()

    def erase(self, effect_id: int) -> None:
        with self.cond:
            self.mixer.erase(effect_id)
            self.cond.notify()

    def play(self, effect_id: int, count: int) -> None:
        with self.cond:
            self.mixer.play(effect_id, count, time.monotonic())
            self.cond.notify()

    def set_gain(self, gain: int) -> None:
        with self.cond:
            self.mixer.set_gain(gain)
            self.cond.notify()

    def direct(self, strong: int, weak: int, ms: int = 0) -> None:
        """Play a one-off rumble without the caller managing effect IDs."""
        self.upload({"id": -100, "type": "rumble", "strong": strong, "weak": weak, "length": ms, "delay": 0})
        self.play(-100, 1 if (strong or weak) else 0)

    def stop(self) -> None:
        with self.cond:
            self.running = False
            self.mixer.stop_all()
            self.cond.notify()
        self.join(timeout=1)
        try:
            self.sink.set(0, 0)
        except OSError:
            pass
        self.sink.close()

    def run(self) -> None:
        while True:
            with self.cond:
                if not self.running:
                    return
                now = time.monotonic()
                target = self.mixer.levels(now)
                due = target != self.current
                active = target != (0, 0)
                if not due and active and self.keepalive and now - self.last_write >= self.keepalive:
                    due = True
                if due and now - self.last_write < self.min_interval:
                    wait = self.min_interval - (now - self.last_write)
                    self.cond.wait(wait)
                    continue
                if not due:
                    deadline = self.mixer.next_deadline(now)
                    timeouts = [deadline - now] if deadline else []
                    if active and self.keepalive:
                        timeouts.append(self.keepalive - (now - self.last_write))
                    self.cond.wait(max(0.001, min(timeouts)) if timeouts else None)
                    continue
            try:
                self.sink.set(*target)
                self.errors = 0
            except OSError as exc:
                self.errors += 1
                if self.log:
                    self.log.write("note", text=f"rumble sink error: {exc}")
                if self.errors > 20:
                    return
            self.current = target
            self.last_write = time.monotonic()
            if self.log:
                self.log.write("rumble_out", strong=target[0], weak=target[1], sink=self.sink.name)


def evdev_rumble(path: str, strong: int, weak: int, ms: int) -> None:
    """Upload and play a single rumble effect through the kernel FF API."""
    dev = InputDevice(path)
    try:
        eff = ff.Effect(
            ecodes.FF_RUMBLE, -1, 0, ff.Trigger(0, 0), ff.Replay(ms, 0),
            ff.EffectType(ff_rumble_effect=ff.Rumble(strong_magnitude=strong, weak_magnitude=weak)),
        )
        eid = dev.upload_effect(eff)
        try:
            dev.write(ecodes.EV_FF, eid, 1)
            time.sleep(ms / 1000.0)
            dev.write(ecodes.EV_FF, eid, 0)
        finally:
            dev.erase_effect(eid)
    finally:
        dev.close()
