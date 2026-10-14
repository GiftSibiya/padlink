"""End-to-end bridge test with a fake uinput 'dongle' (no hardware needed).

Needs write access to /dev/uinput and udev granting the session access to new
joystick nodes. Skipped otherwise.
"""

import os
import select
import subprocess
import sys
import time
import unittest

from evdev import AbsInfo, InputDevice, UInput, ecodes as E

from padlink.core import discovery
from padlink.core.bridge import Bridge
from padlink.core.logger import NullLog

FAKE_VID, FAKE_PID = 0x1234, 0x5678


def wait(pred, timeout=5.0):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        v = pred()
        if v:
            return v
        time.sleep(0.05)
    return None


@unittest.skipUnless(os.access("/dev/uinput", os.W_OK), "no /dev/uinput access")
class LiveBridgeTest(unittest.TestCase):
    def setUp(self):
        stick = AbsInfo(128, 0, 255, 0, 15, 0)
        caps = {
            E.EV_KEY: [E.BTN_SOUTH, E.BTN_EAST, E.BTN_NORTH, E.BTN_WEST, E.BTN_TL, E.BTN_TR,
                       E.BTN_SELECT, E.BTN_START, E.BTN_MODE, E.BTN_THUMBL, E.BTN_THUMBR],
            E.EV_ABS: [(E.ABS_X, stick), (E.ABS_Y, stick), (E.ABS_Z, stick), (E.ABS_RZ, stick),
                       (E.ABS_GAS, AbsInfo(0, 0, 255, 0, 0, 0)), (E.ABS_BRAKE, AbsInfo(0, 0, 255, 0, 0, 0)),
                       (E.ABS_HAT0X, AbsInfo(0, -1, 1, 0, 0, 0)), (E.ABS_HAT0Y, AbsInfo(0, -1, 1, 0, 0, 0))],
        }
        try:
            self.src = UInput(caps, name="padlink test dongle", vendor=FAKE_VID, product=FAKE_PID,
                              phys="padlink-test/source", max_effects=0)
        except OSError as exc:
            self.skipTest(f"cannot create uinput device: {exc}")
        self.records = []
        self.log = NullLog()
        self.log.listeners.append(self.records.append)
        self.bridge = None

    def tearDown(self):
        if self.bridge:
            self.bridge.stop()
        self.src.close()

    def test_rumble_and_input_flow(self):
        spec = f"{FAKE_VID:04x}:{FAKE_PID:04x}"
        ctrl = wait(lambda: next((c for c in discovery.scan() if c.id_str == spec
                                  and c.primary_event and c.primary_event.readable), None))
        if ctrl is None:
            self.skipTest("fake source not visible/readable (udev uaccess?)")
        self.assertFalse(ctrl.has_ff)

        self.bridge = Bridge(ctrl, self.log)
        self.bridge.start()
        virt = wait(lambda: next((c for c in discovery.scan() if c.is_padlink
                                  and c.primary_event and c.primary_event.writable), None))
        self.assertIsNotNone(virt, f"virtual pad did not appear; bridge error={self.bridge.error}")
        self.assertEqual(virt.id_str, "045e:028e")
        self.assertTrue(virt.has_ff)

        # input: source BTN_SOUTH -> virtual BTN_A ; source GAS -> virtual RZ trigger
        pad = InputDevice(virt.primary_event.path)
        try:
            self.src.write(E.EV_KEY, E.BTN_SOUTH, 1)
            self.src.write(E.EV_ABS, E.ABS_GAS, 255)
            self.src.syn()
            got = []
            end = time.monotonic() + 2
            while time.monotonic() < end and len(got) < 2:
                if select.select([pad], [], [], 0.1)[0]:
                    got += [(e.type, e.code, e.value) for e in pad.read() if e.type != E.EV_SYN]
            self.assertIn((E.EV_KEY, E.BTN_A, 1), got)
            self.assertIn((E.EV_ABS, E.ABS_RZ, 255), got)
        finally:
            pad.close()

        # rumble: a "game" (separate process, like a real one) uploads + plays an effect on the virtual pad
        game = (f"from padlink.core.ff import evdev_rumble; "
                f"evdev_rumble({virt.primary_event.path!r}, 0xFFFF, 0x4000, 150)")
        subprocess.run([sys.executable, "-c", game], check=True, timeout=10,
                       cwd=os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
        wait(lambda: any(r["kind"] == "ff_erase" for r in self.records), 2)
        kinds = [r["kind"] for r in self.records]
        self.assertIn("ff_upload", kinds)
        self.assertIn("ff_play", kinds)
        up = next(r for r in self.records if r["kind"] == "ff_upload")
        self.assertEqual((up["effect"]["strong"], up["effect"]["weak"]), (0xFFFF, 0x4000))
        outs = [(r["strong"], r["weak"]) for r in self.records if r["kind"] == "rumble_out"]
        self.assertIn((0xFFFF, 0x4000), outs)
        self.assertEqual(outs[-1], (0, 0))
        self.assertIn("ff_erase", [r["kind"] for r in self.records])


    def test_personas_spoof(self):
        spec = f"{FAKE_VID:04x}:{FAKE_PID:04x}"
        ctrl = wait(lambda: next((c for c in discovery.scan() if c.id_str == spec
                                  and c.primary_event and c.primary_event.readable), None))
        if ctrl is None:
            self.skipTest("fake source not visible/readable (udev uaccess?)")
        # fake source is Android/xpad-style: BTN_NORTH(0x133) is the LEFT button
        expect = {"xbox360": (0x133, [(E.EV_ABS, E.ABS_RZ, 255)]),
                  "ds4": (E.BTN_WEST, [(E.EV_ABS, E.ABS_RZ, 255), (E.EV_KEY, E.BTN_TR2, 1)]),
                  "switchpro": (E.BTN_WEST, [(E.EV_KEY, E.BTN_TR2, 1)])}
        for pid, (left_code, trigger_events) in expect.items():
            with self.subTest(persona=pid):
                from padlink.core import personas
                persona = personas.get(pid)
                bridge = Bridge(ctrl, self.log, mode=pid)
                bridge.start()
                try:
                    virt = wait(lambda: next((c for c in discovery.scan() if c.is_padlink
                                              and c.phys.endswith("/" + pid) and c.primary_event
                                              and c.primary_event.readable), None))
                    self.assertIsNotNone(virt, f"{pid}: no virtual pad ({bridge.error})")
                    self.assertEqual(virt.id_str, persona.id_str)
                    self.assertEqual(virt.name, persona.name)
                    pad = InputDevice(virt.primary_event.path)
                    try:
                        self.src.write(E.EV_KEY, E.BTN_NORTH, 1)
                        self.src.write(E.EV_ABS, E.ABS_GAS, 255)
                        self.src.syn()
                        got = []
                        end = time.monotonic() + 2
                        while time.monotonic() < end and len(got) < 1 + len(trigger_events):
                            if select.select([pad], [], [], 0.1)[0]:
                                got += [(e.type, e.code, e.value) for e in pad.read() if e.type != E.EV_SYN]
                        self.assertIn((E.EV_KEY, left_code, 1), got)
                        for ev in trigger_events:
                            self.assertIn(ev, got)
                    finally:
                        pad.close()
                        self.src.write(E.EV_KEY, E.BTN_NORTH, 0)
                        self.src.write(E.EV_ABS, E.ABS_GAS, 0)
                        self.src.syn()
                finally:
                    bridge.stop()
                    wait(lambda: not any(c.is_padlink for c in discovery.scan()), 3)


if __name__ == "__main__":
    unittest.main()
