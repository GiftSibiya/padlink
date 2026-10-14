"""Dump golden fixtures from the Python implementation for the Rust port.

    python3 tools/gen_fixtures.py            # writes fixtures/golden/*.json
    python3 tools/gen_fixtures.py --check    # exit 1 if the files would change

Every file is deterministic (no timestamps; dict keys keep Python's insertion
order, which some outputs depend on, e.g. format_record) so the Python side
can be re-run in CI and a diff means behaviour changed. The Rust tests load
the same files and must reproduce every expected value.

Events are [type, code, value] triplets with numeric evdev codes; names are
added next to them only for readability.
"""

from __future__ import annotations

import argparse
import json
import sys
import tempfile
from dataclasses import asdict
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from evdev import AbsInfo, ecodes as E, ff  # noqa: E402

from padlink.core import art, discovery, hiddesc, personas, profiles  # noqa: E402
from padlink.core.bridge import Translator, code_name, heuristic_map  # noqa: E402
from padlink.core.ff import RumbleMixer, effect_to_dict  # noqa: E402
from padlink.core.hidraw import hexstr, parse_hex  # noqa: E402
from padlink.core.logger import format_record  # noqa: E402

OUT = ROOT / "fixtures" / "golden"
HID_DIR = ROOT / "fixtures" / "hid"


def absinfo(a: AbsInfo | None) -> dict | None:
    return None if a is None else {"value": a.value, "min": a.min, "max": a.max, "fuzz": a.fuzz,
                                   "flat": a.flat, "resolution": a.resolution}


def named(events) -> list[dict]:
    return [{"ev": [t, c, v], "name": code_name(t, c)} for t, c, v in events]


# ---------------------------------------------------------------- hiddesc
def hid_cases() -> list[tuple[str, bytes]]:
    cases = [(p.stem, parse_hex(p.read_text())) for p in sorted(HID_DIR.glob("*.hex"))]
    cases += [
        ("empty", b""),
        ("truncated_short_item", bytes.fromhex("05 01 09 05 a1 01 26 ff".replace(" ", ""))),
        ("long_item_skipped", bytes.fromhex("fe 02 10 aa bb 05 01 09 05 a1 01 75 08 95 01 81 02 c0".replace(" ", ""))),
        ("long_item_truncated", bytes.fromhex("fe")),
        ("push_pop", bytes.fromhex(
            "05 01 09 05 a1 01 85 02 75 08 95 02 a4 75 10 95 01 09 30 81 02 b4 09 31 09 32 81 02 c0".replace(" ", ""))),
        ("extended_usage", bytes.fromhex(
            "05 01 09 05 a1 01 0b 70 00 0f 00 15 00 25 7f 75 08 95 01 91 02 c0".replace(" ", ""))),
        ("signed_logical", bytes.fromhex(
            "05 01 09 05 a1 01 09 30 09 31 15 81 25 7f 75 08 95 02 81 02 "
            "16 00 80 26 ff 7f 75 10 95 01 09 32 81 02 c0".replace(" ", ""))),
        ("unsigned_lmax_high_bit", bytes.fromhex(
            "05 01 09 05 a1 01 09 30 15 00 25 ff 75 08 95 01 81 02 c0".replace(" ", ""))),
        ("vendor_feature_4byte_page", bytes.fromhex(
            "07 00 ff 00 00 09 01 a1 01 85 03 15 00 26 ff 00 75 08 95 3f b1 02 c0".replace(" ", ""))),
        ("unbalanced_end_collection", bytes.fromhex("c0 c0 05 01 09 05 a1 01 c0".replace(" ", ""))),
    ]
    return cases


def gen_hiddesc() -> dict:
    out = []
    for name, raw in hid_cases():
        d = hiddesc.parse(raw)
        out.append({
            "name": name,
            "hex": raw.hex(" "),
            "applications": d.applications,
            "uses_report_ids": d.uses_report_ids,
            "has_pid": d.has_pid,
            "vendor_pages": sorted(d.vendor_pages),
            "reports": [{
                "kind": r.kind, "report_id": r.report_id, "bits": r.bits,
                "payload_len": r.payload_len, "wire_len": r.wire_len,
                "usage_pages": sorted(r.usage_pages),
                "fields": [{**asdict(f), "usage_range": list(f.usage_range) if f.usage_range else None,
                            "describe": f.describe()} for f in r.fields],
            } for r in d.reports],
            "summary": d.summary(),
        })
    return {"cases": out}


# ---------------------------------------------------------------- profiles
SPECS = [
    {"report": [5, 0, "strong", "weak", 0, 0, 0, 0]},
    {"report": [5, 0, "strong", "weak", 0, 0, 0, 0], "swap": True},
    {"report": [3, 15, "strong", "weak", 0, 0, 255, 0], "stop": [3, 0, 0, 0, 0, 0, 0, 0]},
    {"report": [0, 8, "strong_hi", "strong_lo", "weak_hi", "weak_lo"], "scale": 65535},
    {"report": [1, "max", "any", 0x1FF, -1], "scale": 100},
    {"report": ["strong", "weak"], "scale": 1},
    {"report": ["strong", "weak"], "scale": 1000},
]
MAGS = [0, 1, 128, 129, 255, 256, 257, 32767, 32768, 32896, 49151, 65534, 65535]


def gen_profiles() -> dict:
    render = []
    for spec_kw in SPECS:
        spec = profiles.RumbleSpec(**spec_kw)
        rows = [{"strong": s, "weak": w, "hex": hexstr(spec.render(s, w))}
                for s in MAGS for w in (0, 300, 65535)]
        render.append({"spec": spec.to_dict(), "rows": rows})

    bad = profiles.RumbleSpec(report=["strong", "loud"])
    try:
        bad.render(1, 1)
        bad_err = None
    except ValueError as exc:
        bad_err = str(exc)

    examples = [
        {"name": "GameSir T4N Lite (HID mode)", "vendor": "3537", "product": "1041",
         "rumble": {"transport": "hidraw", "interface": None, "report": [5, 0, "strong", "weak", 0, 0, 0, 0],
                    "stop": None, "scale": 255, "keepalive_ms": 0, "min_interval_ms": 8, "swap": False},
         "input_map": None, "notes": ""},
        {"vendor": "045E", "product": "28e"},  # minimal, uppercase/short hex ids
        {"name": "partial rumble", "vendor": "054c", "product": "09cc",
         "rumble": {"transport": "evdev", "report": []},
         "input_map": {"keys": {"BTN_SOUTH": "BTN_A"}, "axes": {"ABS_Y": {"to": "ABS_Y", "invert": True}},
                       "key_to_axis": {}, "dpad_buttons": False},
         "notes": "free text"},
    ]
    roundtrip = []
    for data in examples:
        p = profiles.Profile.from_dict(data)
        roundtrip.append({"input": data, "vendor": p.vendor, "product": p.product,
                          "filename": profiles.profile_path(p.vendor, p.product, Path(".")).name,
                          "to_dict": p.to_dict(),
                          "saved_text": json.dumps(p.to_dict(), indent=2) + "\n"})
    return {"render": render, "unknown_token_error": bad_err, "roundtrip": roundtrip}


# ---------------------------------------------------------------- personas
CANON_EVENTS = (
    [(E.EV_KEY, c, v) for c in (E.BTN_A, E.BTN_B, 0x133, 0x134, E.BTN_TL, E.BTN_TR, E.BTN_SELECT, E.BTN_START,
                                E.BTN_MODE, E.BTN_THUMBL, E.BTN_THUMBR, E.BTN_TL2, E.BTN_TR2, 0x2FF) for v in (1, 0)]
    + [(E.EV_ABS, c, v) for c in (E.ABS_X, E.ABS_Y, E.ABS_RX, E.ABS_RY)
       for v in (-32768, -32767, -16384, -129, -128, -1, 0, 1, 127, 128, 16384, 32766, 32767)]
    + [(E.EV_ABS, c, v) for c in (E.ABS_Z, E.ABS_RZ) for v in (0, 1, 63, 64, 65, 127, 128, 200, 254, 255)]
    + [(E.EV_ABS, c, v) for c in (E.ABS_HAT0X, E.ABS_HAT0Y) for v in (-1, 0, 1)]
    + [(E.EV_ABS, E.ABS_BRAKE, 10), (E.EV_REL, E.REL_X, 1), (E.EV_MSC, E.MSC_SCAN, 5)]
)


def gen_personas() -> dict:
    out = []
    for p in personas.PERSONAS.values():
        caps = p.capabilities()
        out.append({
            "id": p.id, "label": p.label, "description": p.description, "name": p.name,
            "vendor": p.vendor, "product": p.product, "version": p.version, "bustype": p.bustype,
            "id_str": p.id_str, "spec_layout": p.spec_layout, "trigger_buttons": p.trigger_buttons,
            "face": {str(k): v for k, v in p.face.items()},
            "stick": absinfo(p.stick), "trigger": absinfo(p.trigger),
            "capabilities": {"keys": caps[E.EV_KEY], "abs": [[c, absinfo(a)] for c, a in caps[E.EV_ABS]]},
            "convert": [{"in": list(ev), "out": [list(o) for o in p.convert(*ev)]} for ev in CANON_EVENTS],
        })
    lookups = {s: (personas.get(s).id if s in personas.PERSONAS or s in personas.ALIASES else None)
               for s in ("xpad", "xbox360", "ds4", "nope")}
    phys = {s: (lambda r: r.id if r else None)(personas.from_phys(s))
            for s in ("padlink/virtual/3537:1041/ds4", "padlink/virtual/3537:1041/mirror",
                      "usb-0000:00:14.0-1/input0", "ds4", "")}
    return {"default": personas.DEFAULT, "aliases": personas.ALIASES, "personas": out,
            "get": lookups, "from_phys": phys}


# ---------------------------------------------------------------- translator
PAD_KEYS = [E.BTN_SOUTH, E.BTN_EAST, E.BTN_NORTH, E.BTN_WEST, E.BTN_TL, E.BTN_TR, E.BTN_TL2, E.BTN_TR2,
            E.BTN_SELECT, E.BTN_START, E.BTN_MODE, E.BTN_THUMBL, E.BTN_THUMBR]
A255 = AbsInfo(128, 0, 255, 0, 15, 0)
TRANSLATOR_CASES = [
    # name, keys, absinfo, driver
    ("android_dongle", PAD_KEYS,
     {E.ABS_X: A255, E.ABS_Y: A255, E.ABS_Z: A255, E.ABS_RZ: A255, E.ABS_GAS: AbsInfo(0, 0, 255, 0, 0, 0),
      E.ABS_BRAKE: AbsInfo(0, 0, 255, 0, 0, 0), E.ABS_HAT0X: AbsInfo(0, -1, 1, 0, 0, 0),
      E.ABS_HAT0Y: AbsInfo(0, -1, 1, 0, 0, 0)}, "hid-generic"),
    ("hid_playstation", PAD_KEYS + [E.BTN_DPAD_UP],
     {c: AbsInfo(128, 0, 255, 0, 0, 0) for c in (E.ABS_X, E.ABS_Y, E.ABS_RX, E.ABS_RY, E.ABS_Z, E.ABS_RZ)}
     | {E.ABS_HAT0X: AbsInfo(0, -1, 1, 0, 0, 0), E.ABS_HAT0Y: AbsInfo(0, -1, 1, 0, 0, 0)}, "playstation"),
    ("xpad_native", [E.BTN_A, E.BTN_B, E.BTN_X, E.BTN_Y, E.BTN_TL, E.BTN_TR, E.BTN_SELECT, E.BTN_START, E.BTN_MODE,
                     E.BTN_THUMBL, E.BTN_THUMBR],
     {c: AbsInfo(0, -32768, 32767, 16, 128, 0) for c in (E.ABS_X, E.ABS_Y, E.ABS_RX, E.ABS_RY)}
     | {E.ABS_Z: AbsInfo(0, 0, 255, 0, 0, 0), E.ABS_RZ: AbsInfo(0, 0, 255, 0, 0, 0),
        E.ABS_HAT0X: AbsInfo(0, -1, 1, 0, 0, 0), E.ABS_HAT0Y: AbsInfo(0, -1, 1, 0, 0, 0)}, "xpad"),
    ("generic_joystick", list(range(0x120, 0x12C)) + [0x2C0, 0x2C1],
     {E.ABS_X: AbsInfo(0, 0, 1023, 0, 0, 0), E.ABS_Y: AbsInfo(0, 0, 1023, 0, 0, 0)}, None),
    ("digital_triggers_dpad_buttons", [E.BTN_SOUTH, E.BTN_TL2, E.BTN_TR2, E.BTN_DPAD_UP, E.BTN_DPAD_DOWN,
                                       E.BTN_DPAD_LEFT, E.BTN_DPAD_RIGHT],
     {E.ABS_X: AbsInfo(0, 0, 2, 0, 0, 0), E.ABS_Y: AbsInfo(0, -2, 2, 0, 0, 0)}, "hid-generic"),
    ("no_absinfo", [E.BTN_SOUTH], {}, None),
]

# extra explicit maps, to cover invert, missing/degenerate absinfo and hat clamping
EXPLICIT_MAPS = [
    ("invert_and_halfway", {"axes": {"ABS_X": {"to": "ABS_X", "invert": True}, "ABS_Y": {"to": "ABS_Y"},
                                     "ABS_Z": {"to": "ABS_Z", "invert": True}, "ABS_RZ": {"to": "ABS_HAT0X"}}},
     {E.ABS_X: AbsInfo(1, 0, 2, 0, 0, 0), E.ABS_Y: AbsInfo(0, 0, 2, 0, 0, 0), E.ABS_Z: AbsInfo(0, 0, 2, 0, 0, 0)}),
    ("degenerate_absinfo", {"axes": {"ABS_X": {"to": "ABS_X"}, "ABS_Y": {"to": "ABS_Z"}}},
     {E.ABS_X: AbsInfo(5, 5, 5, 0, 0, 0)}),
]


def drive(t: Translator, keys: list[int], info: dict) -> list[dict]:
    seq = []
    for k in keys + [E.BTN_DPAD_LEFT, E.BTN_DPAD_RIGHT, E.BTN_DPAD_UP, E.BTN_DPAD_DOWN, 0x2FF]:
        seq += [(E.EV_KEY, k, 1)]
    # overlapping dpad presses exercise the hat state machine
    seq += [(E.EV_KEY, E.BTN_DPAD_LEFT, 0), (E.EV_KEY, E.BTN_DPAD_RIGHT, 0), (E.EV_KEY, E.BTN_DPAD_UP, 0),
            (E.EV_KEY, E.BTN_DPAD_DOWN, 0)]
    for k in keys:
        seq += [(E.EV_KEY, k, 0), (E.EV_KEY, k, 2)]
    for code in sorted(set(info) | {E.ABS_X, E.ABS_Y, E.ABS_RZ, E.ABS_HAT0X}):
        a = info.get(code)
        lo, hi = (a.min, a.max) if a else (-5, 5)
        mid = (lo + hi) // 2
        for v in sorted({lo - 1, lo, lo + 1, mid, mid + 1, hi - 1, hi, hi + 1, -2, -1, 0, 1, 2}):
            seq.append((E.EV_ABS, code, v))
    seq += [(E.EV_SYN, 0, 0), (E.EV_REL, E.REL_X, 3)]
    return [{"in": list(ev), "out": [list(o) for o in t.translate(*ev)]} for ev in seq]


def gen_translator() -> dict:
    cases = []
    for name, keys, info, driver in TRANSLATOR_CASES:
        m = heuristic_map(set(keys), set(info), driver)
        cases.append({"name": name, "keys": keys, "driver": driver,
                      "absinfo": {str(c): absinfo(a) for c, a in info.items()},
                      "heuristic_map": m, "events": drive(Translator(m, info), keys, info)})
    for name, m, info in EXPLICIT_MAPS:
        cases.append({"name": name, "keys": [], "driver": None,
                      "absinfo": {str(c): absinfo(a) for c, a in info.items()},
                      "map": m, "events": drive(Translator(m, info), [], info)})
    names = {f"{t}:{c}": code_name(t, c) for t, c in [
        (E.EV_KEY, 0x130), (E.EV_KEY, 0x133), (E.EV_KEY, 0x134), (E.EV_KEY, 0x120), (E.EV_KEY, 0x2C0),
        (E.EV_KEY, 0x2FF), (E.EV_ABS, 0), (E.EV_ABS, 0x10), (E.EV_ABS, 0x09), (E.EV_ABS, 0x0A),
        (E.EV_FF, 0x50), (E.EV_SYN, 0), (99, 1)]}
    return {"cases": cases, "code_names": names}


# ---------------------------------------------------------------- ff
def make_effect(kind: str, **kw) -> ff.Effect:
    replay = ff.Replay(kw.get("length", 0), kw.get("delay", 0))
    trig = ff.Trigger(0, 0)
    eid, direction = kw.get("id", 0), kw.get("direction", 0)
    if kind == "rumble":
        u = ff.EffectType(ff_rumble_effect=ff.Rumble(strong_magnitude=kw["strong"], weak_magnitude=kw["weak"]))
        return ff.Effect(E.FF_RUMBLE, eid, direction, trig, replay, u)
    if kind == "periodic":
        env = ff.Envelope(0, 0, 0, 0)
        p = ff.Periodic(kw.get("waveform", E.FF_SINE), kw.get("period", 100), kw["magnitude"], kw.get("offset", 0),
                        0, env, 0, None)
        return ff.Effect(E.FF_PERIODIC, eid, direction, trig, replay, ff.EffectType(ff_periodic_effect=p))
    if kind == "constant":
        c = ff.Constant(kw["level"], ff.Envelope(0, 0, 0, 0))
        return ff.Effect(E.FF_CONSTANT, eid, direction, trig, replay, ff.EffectType(ff_constant_effect=c))
    if kind == "ramp":
        r = ff.Ramp(kw["start"], kw["end"], ff.Envelope(0, 0, 0, 0))
        return ff.Effect(E.FF_RAMP, eid, direction, trig, replay, ff.EffectType(ff_ramp_effect=r))
    if kind == "spring":
        return ff.Effect(E.FF_SPRING, eid, direction, trig, replay, ff.EffectType())
    raise ValueError(kind)


EFFECTS = [
    ("rumble", {"id": 0, "strong": 65535, "weak": 0, "length": 500}),
    ("rumble", {"id": 3, "strong": 1234, "weak": 40000, "length": 0, "delay": 20, "direction": 0x4000}),
    ("periodic", {"id": 1, "magnitude": 16384, "offset": 0, "waveform": E.FF_SINE, "period": 50, "length": 100}),
    ("periodic", {"id": 2, "magnitude": -32768, "offset": 32767, "waveform": E.FF_SQUARE}),
    ("periodic", {"id": 4, "magnitude": 100, "offset": -101, "waveform": E.FF_SAW_DOWN}),
    ("periodic", {"id": 5, "magnitude": 1, "offset": 0, "waveform": 0x99}),
    ("constant", {"id": 6, "level": -32768, "length": 250}),
    ("constant", {"id": 7, "level": 12345}),
    ("ramp", {"id": 8, "start": -100, "end": 20000}),
    ("spring", {"id": 9, "length": 10}),
]

# Mixer scripts. Times are seconds as passed to the mixer; results are recorded
# exactly. Rust must compute start/end with the same float operations:
#   start = now + delay / 1000.0 ; end = start + (count * length) / 1000.0
MIXER_SCRIPTS = {
    "single_timed": [
        ("upload", {"id": 0, "strong": 40000, "weak": 1000, "length": 100, "delay": 0}),
        ("play", 0, 1, 0.0), ("levels", 0.0), ("levels", 0.05), ("deadline", 0.05),
        ("levels", 0.1), ("levels", 0.2), ("deadline", 0.2),
    ],
    "delay_and_repeat": [
        ("upload", {"id": 1, "strong": 1000, "weak": 2000, "length": 30, "delay": 50}),
        ("play", 1, 3, 1.0), ("levels", 1.0), ("deadline", 1.0), ("levels", 1.049), ("levels", 1.05),
        ("deadline", 1.05), ("levels", 1.139), ("levels", 1.14), ("levels", 1.2),
    ],
    "mix_saturate_and_gain": [
        ("upload", {"id": 0, "strong": 50000, "weak": 10, "length": 0, "delay": 0}),
        ("upload", {"id": 1, "strong": 50000, "weak": 3, "length": 0, "delay": 0}),
        ("play", 0, 1, 0.0), ("play", 1, 1, 0.0), ("levels", 0.0), ("deadline", 0.0),
        ("gain", 32768), ("levels", 1.0), ("gain", 70000), ("levels", 1.0), ("gain", -5), ("levels", 1.0),
        ("gain", 1), ("levels", 1.0), ("gain", 65535),
        ("play", 1, 0, 2.0), ("levels", 2.0), ("erase", 0), ("levels", 2.0), ("deadline", 2.0),
    ],
    "replace_and_unknown": [
        ("play", 7, 1, 0.0), ("levels", 0.0),
        ("upload", {"id": 0, "strong": 100, "weak": 100, "length": 1000, "delay": 0}),
        ("play", 0, 1, 0.0), ("upload", {"id": 0, "strong": 9, "weak": 8, "length": 1000, "delay": 0}),
        ("levels", 0.5), ("play", 0, 1, 0.9), ("levels", 1.5), ("deadline", 1.5), ("erase", 42),
        ("stop_all",), ("levels", 1.5), ("deadline", 1.5),
    ],
    "float_edges": [
        ("upload", {"id": 0, "strong": 1, "weak": 65535, "length": 1, "delay": 1}),
        ("play", 0, 7, 0.1), ("levels", 0.101), ("levels", 0.1009999), ("levels", 0.108),
        ("levels", 0.10799999999), ("deadline", 0.0),
    ],
}


def gen_ff() -> dict:
    effects = []
    for kind, kw in EFFECTS:
        effects.append({"kind": kind, "params": kw, "dict": effect_to_dict(make_effect(kind, **kw))})
    scripts = {}
    for name, ops in MIXER_SCRIPTS.items():
        m = RumbleMixer()
        trace = []
        for op in ops:
            if op[0] == "upload":
                eff = {"type": "rumble", **op[1]}
                m.upload(eff)
                trace.append({"op": "upload", "effect": eff})
            elif op[0] == "play":
                m.play(op[1], op[2], op[3])
                trace.append({"op": "play", "id": op[1], "count": op[2], "now": op[3]})
            elif op[0] == "erase":
                m.erase(op[1])
                trace.append({"op": "erase", "id": op[1]})
            elif op[0] == "gain":
                m.set_gain(op[1])
                trace.append({"op": "gain", "gain": op[1], "result": m.gain})
            elif op[0] == "stop_all":
                m.stop_all()
                trace.append({"op": "stop_all"})
            elif op[0] == "levels":
                trace.append({"op": "levels", "now": op[1], "result": list(m.levels(op[1]))})
            elif op[0] == "deadline":
                trace.append({"op": "deadline", "now": op[1], "result": m.next_deadline(op[1])})
        scripts[name] = trace
    return {"effect_to_dict": effects, "mixer": scripts}


# ---------------------------------------------------------------- misc
def gen_misc() -> dict:
    hex_inputs = ["05 ff 00", "05ff00", "0x05,0xff,0x00", "5:f:0", "  0X1  ff ", "abc", "", "0102 3"]
    parse = {}
    for s in hex_inputs:
        try:
            parse[s] = hexstr(parse_hex(s))
        except ValueError as exc:
            parse[s] = {"error": str(exc)}
    long_bits = discovery._long_bits()
    bitmasks = {s: sorted(discovery.parse_bitmask(s)) for s in
                ["", "0", "3", "1 0", "1f 0 ffff", "8000000000000000", "fffe 0 0 0"]}
    t0 = 1000.0
    records = [
        {"t": 1000.0, "kind": "session", "version": "0.1.0", "command": "bridge", "label": "x"},
        {"t": 1000.5, "kind": "input", "node": "/dev/input/event5", "type": 1, "code": 304, "value": 1,
         "name": "BTN_SOUTH"},
        {"t": 1001.25, "kind": "hid_tx", "node": "/dev/hidraw3", "hex": "05 00 ff 00", "ok": True},
        {"t": 1001.5, "kind": "hid_tx", "node": "/dev/hidraw3", "hex": "05", "ok": False, "error": "EPIPE"},
        {"t": 1002.0, "kind": "ff_play", "id": 0, "value": 1},
        {"t": 1002.0, "kind": "ff_play", "id": 0, "value": 0},
        {"t": 1003.0, "kind": "rumble_out", "strong": 65535, "weak": 0, "sink": "log-only"},
        {"t": 1004.0, "kind": "note", "text": "unicode ✓", "data": {"b": [1, 2]}},
        {"kind": "corrupt", "line": "{oops"},
    ]
    formatted = [{"record": r, "relative": format_record(r, t0)} for r in records]
    return {"parse_hex": parse, "long_bits": long_bits, "parse_bitmask": bitmasks,
            "format_record": formatted}


def gen_art() -> dict:
    ids = [(0x045E, 0x028E), (0x045E, 0x02EA), (0x045E, 0x0B12), (0x045E, 0x9999), (0x054C, 0x05C4),
           (0x054C, 0x09CC), (0x054C, 0x0CE6), (0x054C, 0x1234), (0x057E, 0x2009), (0x057E, 0x2006),
           (0x3537, 0x1041)]
    templates = {}
    for fam in art.FAMILIES:
        p = ROOT / "data" / "art-templates" / f"{fam}.svg"
        templates[fam] = {"file": str(p.relative_to(ROOT)), "matches_template_svg": art.template_svg(fam) == p.read_text(),
                          "viewbox": list(art.viewbox(p)), "check": [list(c) for c in art.check(p)]}
    checks = {}
    shapes = "".join(f'<rect id="{i}"/>' for i in art.REQUIRED_IDS if i not in ("btn-south", "lt", "body"))
    for name, svg in {
        "minimal_ok": f'<svg xmlns="{art.SVG_NS}" viewBox="0 0 10 10"><g id="body"/>'
                      + "".join(f'<rect id="{i}"/>' for i in art.REQUIRED_IDS if i != "body") + "</svg>",
        "missing_and_mangled": f'<svg xmlns="{art.SVG_NS}" width="10" height="10"><path id="body"/>{shapes}'
                               '<circle id="btn-south_1_"/><g id="lt"><rect/></g><rect id="touchpad"/></svg>',
        "groups_style_image_text": f'<svg xmlns="{art.SVG_NS}"><style>.a{{}}</style><image/><text>x</text>'
                                   '<g id="body"/>' + "".join(f'<g id="{i}"/>' for i in art.REQUIRED_IDS
                                                             if i != "body") + '<rect id="btn-capture"/></svg>',
        "illustrator_x5F": f'<svg xmlns="{art.SVG_NS}" viewBox="0 0 1 1"><rect id="body"/>'
                           + "".join(f'<rect id="{i.replace("_", "_x5F_")}_2_"/>' if i == "btn-lb" else f'<rect id="{i}"/>'
                                     for i in art.REQUIRED_IDS if i != "body") + "</svg>",
        "no_namespace": '<svg viewBox="0 0 1 1"><style/><rect id="body"/></svg>',
    }.items():
        path = Path(tempfile.mkdtemp()) / f"{name}.svg"
        path.write_text(svg)
        checks[name] = {"svg": svg, "check": [list(c) for c in art.check(path)]}
    viewboxes = {}
    for svg in ['<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 400 260"/>',
                '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0,0, 10.5 7"/>',
                '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 0 10" width="20px" height="10px"/>',
                '<svg xmlns="http://www.w3.org/2000/svg" width="100%" height="50"/>',
                '<svg xmlns="http://www.w3.org/2000/svg"/>']:
        vb = art.viewbox(svg.encode())
        viewboxes[svg] = list(vb) if vb else None
    return {
        "families": list(art.FAMILIES), "required_ids": art.REQUIRED_IDS, "optional_ids": art.OPTIONAL_IDS,
        "candidates": {f"{v:04x}:{p:04x}": art.candidates(v, p) for v, p in ids},
        "templates": templates, "viewbox": viewboxes, "check": checks,
        "stick_travel": [{"ring": r, "knob": k, "result": art.stick_travel(r, k)} for r, k in [
            ((0, 0, 60, 60), (0, 0, 30, 30)), ((0, 0, 60, 40), (0, 0, 20, 30)), ((0, 0, 10, 10), (0, 0, 30, 30))]],
    }


# ---------------------------------------------------------------- prober / capture / doctor parsing
def gen_prober() -> dict:
    from padlink.core import prober
    from padlink.core.discovery import Controller, HidrawNode

    def cand_json(c) -> dict:
        return {"label": c.label, "report_id": c.report_id, "template": c.template, "scale": c.scale,
                "stop": c.stop, "meta": c.meta, "on": hexstr(c.on_bytes()), "off": hexstr(c.off_bytes()),
                "spec": c.spec().to_dict()}

    cases = []
    for name, raw in hid_cases():
        tmp = Path(tempfile.mkdtemp())
        (tmp / "report_descriptor").write_bytes(raw)
        node = HidrawNode(path="/dev/hidraw99", sys_path=str(tmp), hid_sys_path=str(tmp), interface="01")
        ctrl = Controller(key="/sys/x", name="x", vendor=1, product=2, version=0, bustype=3, driver=None, uniq="",
                          phys="", virtual=False, hidraw=[node])
        for blind in (False, True):
            cands = prober.stage1_candidates(ctrl, blind=blind)
            case = {"descriptor": name, "blind": blind, "stage1": [cand_json(c) for c in cands]}
            hit = next((c for c in cands if c.meta.get("kind") == "all_ff"), None)
            if hit:
                case["stage2"] = [cand_json(c) for c in prober.stage2_byte_candidates(hit)]
                length = hit.meta["length"]
                maps = [m for m in ({}, {0: "strong"}, {0: "strong", 1: "weak"}, {length - 1: "both", 0: "weak"})
                        if all(0 <= k < length for k in m)]
                case["build"] = [{"motors": {str(k): v for k, v in m.items()},
                                  "result": cand_json(prober.build_from_bytes(hit, m))} for m in maps]
            cases.append(case)
    fits = [{"payload": p, "length": n, "result": prober._fit(list(p), n)}
            for p in ([1, 2, 3], [1, 0, 0], [1, 2, 0], ["strong", "weak"]) for n in (0, 2, 3, 5)]
    return {"known_layouts": [list(x) for x in prober.KNOWN_LAYOUTS], "cases": cases, "fit": fits}


def gen_capture() -> dict:
    from padlink.core import capture
    lines = [
        "ffff8a1f c1a3e2c0 3575215 S Io:3:005:2 -115:8 8 = 0500ff00 00000000",
        "ffff8a1f c1a3e2c0 3575290 C Io:3:005:2 0 8 >",
        "ffff8a1f 3575300 C Ii:3:005:1 0:8 20 = 01020304 05060708 090a0b0c 0d0e0f10 11121314",
        "ffff8a1f 3575400 S Co:3:005:0 s 21 09 0205 0000 0003 3 = 05ff00",
        "ffff8a1f 3575400 S Ci:3:005:0 s a1 01 0105 0000 0040 64 <",
        "ffff8a1f 3575500 E Bo:1:002:3 -32 0",
        "ffff8a1f 3575600 S Zi:2:009:4 -115 256 <",
        "short line",
        "ffff 1 S Xo:bad:005:2 0 0",
        "",
    ]
    return {"lines": [{"line": l, "record": capture.parse_line(l)} for l in lines]}


def gen_winebus() -> dict:
    from padlink.core import doctor
    regs = {
        "none": "WINE REGISTRY Version 2\n\n[Software\\\\Wine] 1700000000\n\"Version\"=\"win10\"\n",
        "keys": "WINE REGISTRY Version 2\n\n[System\\\\CurrentControlSet\\\\Services\\\\winebus] 1700000000\n"
                "#time=1da\n\"DisableHidraw\"=dword:00000001\n\"DisableInput\"=dword:00000001\n"
                "\"Enable SDL\"=dword:00000000\n\n[System\\\\Other] 1\n\"X\"=\"y\"\n",
        "at_end": "[System\\\\CurrentControlSet\\\\Services\\\\winebus] 1\n\"Map Controllers\"=dword:00000000",
        "empty_section": "[System\\\\CurrentControlSet\\\\Services\\\\winebus] 1\n\n\"After\"=1\n",
    }
    out = {}
    for name, text in regs.items():
        tmp = Path(tempfile.mkdtemp())
        (tmp / "system.reg").write_text(text)
        res = doctor._winebus_keys(tmp)
        out[name] = {"text": text, "keys": res["keys"] if res else None}
    return {"cases": out}


GENERATORS = {"hiddesc": gen_hiddesc, "profiles": gen_profiles, "personas": gen_personas,
              "translator": gen_translator, "ff": gen_ff, "misc": gen_misc, "art": gen_art,
              "prober": gen_prober, "capture": gen_capture, "winebus": gen_winebus}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--check", action="store_true", help="fail if any fixture would change")
    args = ap.parse_args()
    OUT.mkdir(parents=True, exist_ok=True)
    changed = []
    for name, fn in GENERATORS.items():
        text = json.dumps(fn(), indent=1, ensure_ascii=False) + "\n"
        path = OUT / f"{name}.json"
        if not path.exists() or path.read_text() != text:
            changed.append(path.relative_to(ROOT))
            if not args.check:
                path.write_text(text)
    for p in changed:
        print(f"{'would change' if args.check else 'wrote'}: {p}")
    return 1 if (args.check and changed) else 0


if __name__ == "__main__":
    sys.exit(main())
