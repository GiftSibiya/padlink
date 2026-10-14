"""Offscreen render checks for SVG controller artwork (needs librsvg + cairo)."""

import tempfile
import unittest
from pathlib import Path

from evdev import ecodes as E

try:
    import cairo

    from padlink.core import art
    from padlink.gtk.svgart import SvgArt
except (ImportError, ValueError) as exc:  # no GTK/rsvg available
    SKIP = str(exc)
else:
    SKIP = None

RED = (1.0, 0.0, 0.0)
WHITE = (1.0, 1.0, 1.0)


@unittest.skipIf(SKIP, f"rendering stack unavailable: {SKIP}")
class SvgArtRenderTest(unittest.TestCase):
    W, H = 600, 450  # different aspect than the 800x520 artboard: exercises scale + offset

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        path = Path(self.tmp.name) / "xbox.svg"
        path.write_text(art.template_svg("xbox"))
        self.art = SvgArt(path)
        self.assertIsNone(self.art.error)
        self.scale = min(self.W / 800, self.H / 520)
        self.ox = (self.W - 800 * self.scale) / 2
        self.oy = (self.H - 520 * self.scale) / 2

    def tearDown(self):
        self.tmp.cleanup()

    def render(self, pressed=(), axes=None):
        surf = cairo.ImageSurface(cairo.FORMAT_ARGB32, self.W, self.H)
        cr = cairo.Context(surf)
        self.art.render(cr, self.W, self.H, set(pressed), axes or {}, WHITE, RED, True)
        surf.flush()
        return surf

    def pixel(self, surf, ux, uy):
        """RGB at template user coordinates (ux, uy)."""
        x = int(self.ox + ux * self.scale)
        y = int(self.oy + uy * self.scale)
        data = surf.get_data()
        i = y * surf.get_stride() + x * 4
        b, g, r, a = data[i], data[i + 1], data[i + 2], data[i + 3]
        return r, g, b, a

    def is_red(self, px):
        r, g, b, a = px
        return a > 150 and r > 150 and g < 90 and b < 90

    def test_pressed_button_filled(self):
        # btn-south centre in the xbox template: face (576, 200) + (0, 46); sample beside the label
        self.assertFalse(self.is_red(self.pixel(self.render(), 576 - 14, 246)))
        self.assertTrue(self.is_red(self.pixel(self.render(pressed=[E.BTN_A]), 576 - 14, 246)))

    def test_trigger_fill_is_partial_and_bottom_up(self):
        # rt rect: x 534..634, y 4..48; half pulled -> bottom half red, top half not
        surf = self.render(axes={E.ABS_RZ: 128})
        self.assertTrue(self.is_red(self.pixel(surf, 545, 44)))
        self.assertFalse(self.is_red(self.pixel(surf, 545, 8)))

    def test_knob_moves_within_ring(self):
        # ls ring centre (250, 200) r48, knob r24 -> travel 24 user units; full right
        surf = self.render(pressed=[E.BTN_THUMBL], axes={E.ABS_X: 32767})
        self.assertTrue(self.is_red(self.pixel(surf, 250 + 24, 200)))   # knob centre moved right
        self.assertFalse(self.is_red(self.pixel(surf, 250 - 20, 200)))  # old position empty
        self.assertFalse(self.is_red(self.pixel(surf, 250 + 24 + 28, 200)))  # did not overshoot the ring


if __name__ == "__main__":
    unittest.main()
