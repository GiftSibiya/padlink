import math
import tempfile
import unittest
from pathlib import Path

from evdev import AbsInfo, ecodes as E

from padlink.core import capture, discovery, hiddesc, profiles
from padlink.core.bridge import Translator, heuristic_map
from padlink.core.ff import RumbleMixer
from padlink.core.hidraw import parse_hex
from padlink.core.logger import SessionLog, read_log

# Android-style HID gamepad: 15 buttons, hat, X/Y/Z/Rz, brake/gas, vendor output report 5 (8 bytes)
GAMEPAD_DESC = bytes.fromhex(
    "05 01 09 05 a1 01 85 01"
    "05 09 19 01 29 0f 15 00 25 01 75 01 95 0f 81 02"
    "75 01 95 01 81 03"
    "05 01 09 39 15 00 25 07 75 04 95 01 81 42"
    "75 04 95 01 81 03"
    "09 30 09 31 09 32 09 35 15 00 26 ff 00 75 08 95 04 81 02"
    "05 02 09 c5 09 c4 95 02 81 02"
    "85 05 06 00 ff 09 01 15 00 26 ff 00 75 08 95 08 91 02"
    "c0".replace(" ", "")
)

PID_DESC = bytes.fromhex("05 0f 09 21 a1 02 85 01 09 22 15 01 25 28 75 08 95 01 91 02 c0".replace(" ", ""))


class DescriptorTests(unittest.TestCase):
    def test_gamepad(self):
        d = hiddesc.parse(GAMEPAD_DESC)
        self.assertTrue(d.uses_report_ids)
        self.assertEqual(d.applications, ["Application(Generic Desktop:Gamepad)"])
        inp = d.by_kind("input")[0]
        self.assertEqual((inp.report_id, inp.payload_len, inp.wire_len), (1, 9, 10))
        out = d.by_kind("output")
        self.assertEqual(len(out), 1)
        self.assertEqual((out[0].report_id, out[0].payload_len), (5, 8))
        self.assertEqual(d.vendor_pages, {0xFF00})
        self.assertFalse(d.has_pid)
        self.assertTrue(any("Hat switch" in line for line in d.summary()))

    def test_pid_detected(self):
        self.assertTrue(hiddesc.parse(PID_DESC).has_pid)

    def test_truncated_does_not_crash(self):
        hiddesc.parse(GAMEPAD_DESC[:-7])
        hiddesc.parse(b"\xfe")


class ProfileTests(unittest.TestCase):
    def test_render_tokens(self):
        spec = profiles.RumbleSpec(report=[5, "strong", "weak", "max", "any", 0xAA], scale=255)
        self.assertEqual(spec.render(0xFFFF, 0), bytes([5, 255, 0, 255, 255, 0xAA]))
        self.assertEqual(spec.render(0, 0x7FFF), bytes([5, 0, 127, 127, 255, 0xAA]))
        self.assertEqual(spec.render(0, 0), bytes([5, 0, 0, 0, 0, 0xAA]))

    def test_swap_scale_stop(self):
        spec = profiles.RumbleSpec(report=[3, "strong", "weak"], scale=100, swap=True, stop=[3, 0, 0xFF])
        self.assertEqual(spec.render(0xFFFF, 0), bytes([3, 0, 100]))
        self.assertEqual(spec.render(0, 0), bytes([3, 0, 0xFF]))

    def test_16bit_split(self):
        spec = profiles.RumbleSpec(report=["strong_hi", "strong_lo"], scale=0xFFFF)
        self.assertEqual(spec.render(0x1234, 0), bytes([0x12, 0x34]))

    def test_bad_token(self):
        with self.assertRaises(ValueError):
            profiles.RumbleSpec(report=["nope"]).render(1, 1)

    def test_roundtrip(self):
        with tempfile.TemporaryDirectory() as tmp:
            p = profiles.Profile(0x3537, 0x1041, "pad", profiles.RumbleSpec(report=[5, "strong"]), {"keys": {}})
            path = p.save(Path(tmp))
            self.assertEqual(path.name, "3537_1041.json")
            q = profiles.load(0x3537, 0x1041, Path(tmp))
            self.assertEqual(q.rumble.report, [5, "strong"])
            self.assertEqual(q.input_map, {"keys": {}})


class MixerTests(unittest.TestCase):
    def test_timing_and_mix(self):
        m = RumbleMixer()
        m.upload({"id": 0, "strong": 40000, "weak": 0, "length": 100, "delay": 0})
        m.upload({"id": 1, "strong": 40000, "weak": 1000, "length": 0, "delay": 50})
        m.play(0, 1, now=0.0)
        m.play(1, 1, now=0.0)
        self.assertEqual(m.levels(0.01), (40000, 0))
        self.assertEqual(m.levels(0.06), (0xFFFF, 1000))  # summed and clamped
        self.assertAlmostEqual(m.next_deadline(0.06), 0.1)
        self.assertEqual(m.levels(0.2), (40000, 1000))  # effect 0 expired, 1 infinite
        m.set_gain(0x7FFF)
        self.assertEqual(m.levels(0.2), (40000 * 0x7FFF // 0xFFFF, 1000 * 0x7FFF // 0xFFFF))
        m.play(1, 0, now=0.3)
        self.assertEqual(m.levels(0.3), (0, 0))
        self.assertIsNone(m.next_deadline(0.3))

    def test_repeat_count(self):
        m = RumbleMixer()
        m.upload({"id": 2, "strong": 1, "weak": 1, "length": 100, "delay": 0})
        m.play(2, 3, now=0.0)
        self.assertEqual(m.levels(0.25), (1, 1))
        self.assertEqual(m.levels(0.31), (0, 0))

    def test_erase_and_unknown(self):
        m = RumbleMixer()
        m.play(9, 1, now=0)  # unknown id is ignored
        m.upload({"id": 3, "strong": 5, "weak": 5, "length": 0, "delay": 0})
        m.play(3, 1, now=0)
        m.erase(3)
        self.assertEqual(m.levels(1), (0, 0))
        self.assertEqual(m.next_deadline(0), None)
        self.assertTrue(math.isinf(float("inf")))


class TranslatorTests(unittest.TestCase):
    def setUp(self):
        keys = {E.BTN_SOUTH, E.BTN_EAST, E.BTN_NORTH, E.BTN_WEST, E.BTN_TL, E.BTN_TR, E.BTN_TL2, E.BTN_TR2,
                E.BTN_SELECT, E.BTN_START, E.BTN_MODE, E.BTN_THUMBL, E.BTN_THUMBR}
        self.absinfo = {c: AbsInfo(128, 0, 255, 0, 15, 0) for c in (E.ABS_X, E.ABS_Y, E.ABS_Z, E.ABS_RZ)}
        self.absinfo[E.ABS_GAS] = AbsInfo(0, 0, 255, 0, 0, 0)
        self.absinfo[E.ABS_BRAKE] = AbsInfo(0, 0, 255, 0, 0, 0)
        self.absinfo[E.ABS_HAT0X] = AbsInfo(0, -1, 1, 0, 0, 0)
        self.map = heuristic_map(keys, set(self.absinfo))
        self.t = Translator(self.map, self.absinfo)

    def test_heuristic_android_layout(self):
        self.assertEqual(self.map["axes"]["ABS_Z"]["to"], "ABS_RX")
        self.assertEqual(self.map["axes"]["ABS_RZ"]["to"], "ABS_RY")
        self.assertEqual(self.map["axes"]["ABS_BRAKE"]["to"], "ABS_Z")
        self.assertEqual(self.map["axes"]["ABS_GAS"]["to"], "ABS_RZ")
        self.assertNotIn("BTN_TL2", self.map["key_to_axis"])  # analog triggers exist

    def test_scaling(self):
        self.assertEqual(self.t.translate(E.EV_ABS, E.ABS_X, 0), [(E.EV_ABS, E.ABS_X, -32768)])
        self.assertEqual(self.t.translate(E.EV_ABS, E.ABS_X, 255), [(E.EV_ABS, E.ABS_X, 32767)])
        self.assertEqual(self.t.translate(E.EV_ABS, E.ABS_GAS, 255), [(E.EV_ABS, E.ABS_RZ, 255)])
        self.assertEqual(self.t.translate(E.EV_ABS, E.ABS_HAT0X, -1), [(E.EV_ABS, E.ABS_HAT0X, -1)])

    def test_buttons(self):
        self.assertEqual(self.t.translate(E.EV_KEY, E.BTN_SOUTH, 1), [(E.EV_KEY, E.BTN_A, 1)])
        self.assertEqual(self.t.translate(E.EV_KEY, 0x2FF, 1), [])

    def test_digital_triggers_and_dpad(self):
        m = heuristic_map({E.BTN_SOUTH, E.BTN_TL2, E.BTN_DPAD_LEFT, E.BTN_DPAD_RIGHT}, {E.ABS_X, E.ABS_Y})
        t = Translator(m, {})
        self.assertEqual(t.translate(E.EV_KEY, E.BTN_TL2, 1), [(E.EV_ABS, E.ABS_Z, 255)])
        self.assertEqual(t.translate(E.EV_KEY, E.BTN_DPAD_LEFT, 1), [(E.EV_ABS, E.ABS_HAT0X, -1)])
        self.assertEqual(t.translate(E.EV_KEY, E.BTN_DPAD_RIGHT, 1), [(E.EV_ABS, E.ABS_HAT0X, 0)])
        self.assertEqual(t.translate(E.EV_KEY, E.BTN_DPAD_LEFT, 0), [(E.EV_ABS, E.ABS_HAT0X, 1)])

    def test_invert(self):
        t = Translator({"axes": {"ABS_Y": {"to": "ABS_Y", "invert": True}}}, self.absinfo)
        self.assertEqual(t.translate(E.EV_ABS, E.ABS_Y, 0), [(E.EV_ABS, E.ABS_Y, 32767)])


class MiscTests(unittest.TestCase):
    def test_bitmask(self):
        self.assertEqual(discovery.parse_bitmask("3"), {0, 1})
        bits = discovery.parse_bitmask("1 0")
        self.assertEqual(bits, {discovery._long_bits()})
        self.assertEqual(discovery.parse_bitmask(""), set())

    def test_parse_hex(self):
        self.assertEqual(parse_hex("05 ff 0"), b"\x05\xff\x00")
        self.assertEqual(parse_hex("0x05,0xff"), b"\x05\xff")
        self.assertEqual(parse_hex("05ff00"), b"\x05\xff\x00")

    def test_usbmon_line(self):
        line = "ffff8f0a 3575914555 S Io:1:004:2 -115:8 8 = 05ff0000 00000000"
        rec = capture.parse_line(line)
        self.assertEqual((rec["bus"], rec["dev"], rec["ep"], rec["dir"], rec["type"]), (1, 4, 2, "out", "interrupt"))
        self.assertEqual(rec["hex"], "05 ff 00 00 00 00 00 00")
        self.assertIsNone(capture.parse_line("garbage"))

    def test_log_roundtrip(self):
        with tempfile.TemporaryDirectory() as tmp:
            log = SessionLog("3537:1041", "test", {"x": 1}, directory=Path(tmp))
            seen = []
            log.listeners.append(seen.append)
            log.write("ff_play", id=0, value=1)
            log.close()
            recs = read_log(log.path)
            self.assertEqual([r["kind"] for r in recs], ["session", "ff_play"])
            self.assertEqual(seen[0]["id"], 0)



class LutrisApplyTests(unittest.TestCase):
    def test_apply_and_remove(self):
        from padlink.core import integration

        with tempfile.TemporaryDirectory() as tmp:
            old = integration.LUTRIS_GAMES
            integration.LUTRIS_GAMES = Path(tmp)
            try:
                cfg = Path(tmp) / "nfs-123.yml"
                cfg.write_text("game:\n  exe: /x.exe\nsystem:\n  env:\n    FOO: bar\n")
                path, backup = integration.apply_lutris_env("nfs", {"SDL_GAMECONTROLLER_IGNORE_DEVICES": "0x3537/0x1041"})
                self.assertEqual(path, cfg)
                self.assertTrue(backup.exists())
                import yaml
                env = yaml.safe_load(cfg.read_text())["system"]["env"]
                self.assertEqual(env, {"FOO": "bar", "SDL_GAMECONTROLLER_IGNORE_DEVICES": "0x3537/0x1041"})
                integration.apply_lutris_env("nfs-123", {"SDL_GAMECONTROLLER_IGNORE_DEVICES": ""}, remove=True)
                self.assertEqual(yaml.safe_load(cfg.read_text())["system"]["env"], {"FOO": "bar"})
                self.assertEqual(yaml.safe_load(cfg.read_text())["game"]["exe"], "/x.exe")
            finally:
                integration.LUTRIS_GAMES = old


if __name__ == "__main__":
    unittest.main()
