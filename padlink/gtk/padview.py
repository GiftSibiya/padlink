"""A wireframe gamepad drawn with cairo that lights up as input arrives.

The view works in xpad terms (BTN_A, ABS_RX, ...): the Input page runs raw
events through the same Translator the bridge uses, so what you see here is
also what a game would see through the virtual pad.

Everything is drawn in a 400x260 design space and scaled to fit.
"""

from __future__ import annotations

import math

from evdev import ecodes as E

from padlink.gtk.util import Adw, Gtk

W, H = 400.0, 260.0

# Physical position of each xpad button (xpad: 0x133 is the left "X" button).
SOUTH, EAST, WEST, NORTH = E.BTN_A, E.BTN_B, 0x133, 0x134

FAMILIES = {
    "xbox": {
        "face": {SOUTH: "A", EAST: "B", WEST: "X", NORTH: "Y"},
        "shoulder": ("LB", "RB", "LT", "RT"),
        "small": ("View", "Menu", "Guide"),
        "pos": {"ls": (125, 100), "dpad": (160, 162), "rs": (240, 162), "face": (288, 100),
                "select": (172, 100), "start": (228, 100), "mode": (200, 70)},
    },
    "sony": {
        "face": {SOUTH: "cross", EAST: "circle", WEST: "square", NORTH: "triangle"},
        "shoulder": ("L1", "R1", "L2", "R2"),
        "small": ("Share", "Options", "PS"),
        "pos": {"dpad": (112, 100), "face": (288, 100), "ls": (158, 165), "rs": (242, 165),
                "select": (140, 58), "start": (260, 58), "mode": (200, 165), "touchpad": (200, 72)},
    },
    "nintendo": {
        "face": {SOUTH: "B", EAST: "A", WEST: "Y", NORTH: "X"},
        "shoulder": ("L", "R", "ZL", "ZR"),
        "small": ("−", "+", "Home"),
        "pos": {"ls": (125, 100), "dpad": (160, 162), "rs": (240, 162), "face": (288, 100),
                "select": (172, 100), "start": (228, 100), "mode": (200, 70)},
    },
}


def family_for(vendor: int) -> str:
    return {0x054C: "sony", 0x057E: "nintendo"}.get(vendor, "xbox")


class PadView(Gtk.DrawingArea):
    def __init__(self):
        super().__init__(hexpand=True, content_height=280)
        self.family = "xbox"
        self.pressed: set[int] = set()
        self.axes: dict[int, int] = {}
        self.active = False
        self.set_draw_func(self._draw)

    # -- state -------------------------------------------------------------
    def configure(self, family: str) -> None:
        self.family = family if family in FAMILIES else "xbox"
        self.reset()

    def reset(self) -> None:
        self.pressed.clear()
        self.axes.clear()
        self.queue_draw()

    def set_active(self, active: bool) -> None:
        self.active = active
        self.queue_draw()

    def apply(self, etype: int, code: int, value: int) -> None:
        if etype == E.EV_KEY:
            if value:
                self.pressed.add(code)
            else:
                self.pressed.discard(code)
        elif etype == E.EV_ABS:
            self.axes[code] = value

    # -- drawing helpers ---------------------------------------------------
    def _colors(self):
        fg = self.get_color()
        try:
            acc = Adw.StyleManager.get_default().get_accent_color_rgba()
        except AttributeError:
            acc = None
        accent = (acc.red, acc.green, acc.blue) if acc else (0.21, 0.52, 0.89)
        return (fg.red, fg.green, fg.blue), accent

    def _stroke(self, cr, alpha=0.65):
        cr.set_source_rgba(*self.fg, alpha if self.active else alpha * 0.6)
        cr.stroke()

    def _fill_or_stroke(self, cr, on: bool, amount: float = 1.0):
        if on:
            cr.set_source_rgba(*self.accent, 0.35 + 0.65 * amount)
            cr.fill_preserve()
        self._stroke(cr)

    def _round_rect(self, cr, x, y, w, h, r):
        r = min(r, w / 2, h / 2)
        cr.new_sub_path()
        cr.arc(x + w - r, y + r, r, -math.pi / 2, 0)
        cr.arc(x + w - r, y + h - r, r, 0, math.pi / 2)
        cr.arc(x + r, y + h - r, r, math.pi / 2, math.pi)
        cr.arc(x + r, y + r, r, math.pi, 3 * math.pi / 2)
        cr.close_path()

    def _text(self, cr, text, x, y, size=9.0, on=False, bold=True):
        import cairo

        cr.select_font_face("Sans", cairo.FONT_SLANT_NORMAL,
                            cairo.FONT_WEIGHT_BOLD if bold else cairo.FONT_WEIGHT_NORMAL)
        cr.set_font_size(size)
        ext = cr.text_extents(text)
        cr.move_to(x - ext.width / 2 - ext.x_bearing, y - ext.height / 2 - ext.y_bearing)
        if on:
            cr.set_source_rgba(1, 1, 1, 0.95)
        else:
            cr.set_source_rgba(*self.fg, 0.8 if self.active else 0.5)
        cr.show_text(text)
        cr.new_path()  # show_text leaves a current point; arcs would connect to it

    # -- parts -------------------------------------------------------------
    def _body(self, cr):
        cr.new_path()
        cr.move_to(120, 40)
        cr.line_to(280, 40)
        cr.curve_to(340, 40, 370, 70, 385, 150)
        cr.curve_to(395, 210, 380, 250, 350, 248)
        cr.curve_to(325, 246, 310, 215, 290, 197)
        cr.line_to(110, 197)
        cr.curve_to(90, 215, 75, 246, 50, 248)
        cr.curve_to(20, 250, 5, 210, 15, 150)
        cr.curve_to(30, 70, 60, 40, 120, 40)
        cr.close_path()
        cr.set_source_rgba(*self.fg, 0.04)
        cr.fill_preserve()
        cr.set_line_width(2.0)
        self._stroke(cr, 0.55)
        cr.set_line_width(1.5)

    def _shoulders(self, cr, labels):
        lb, rb, lt, rt = labels
        for x, key, axis, digital, blabel, tlabel in ((78, E.BTN_TL, E.ABS_Z, E.BTN_TL2, lb, lt),
                                                      (252, E.BTN_TR, E.ABS_RZ, E.BTN_TR2, rb, rt)):
            # trigger: fills from the bottom with the analog value; outline lights on the digital click
            val = max(0, min(255, self.axes.get(axis, 0))) / 255.0
            self._round_rect(cr, x + 10, 2, 50, 22, 6)
            if digital in self.pressed:
                cr.set_source_rgba(*self.accent, 1.0)
                cr.set_line_width(2.5)
                cr.stroke()
                cr.set_line_width(1.5)
            else:
                self._stroke(cr)
            if val > 0:
                cr.save()
                self._round_rect(cr, x + 10, 2, 50, 22, 6)
                cr.clip()
                cr.rectangle(x + 10, 2 + 22 * (1 - val), 50, 22 * val)
                cr.set_source_rgba(*self.accent, 0.35 + 0.65 * val)
                cr.fill()
                cr.restore()
            self._text(cr, tlabel, x + 35, 13, 8.5, on=val > 0.5)
            # bumper
            on = key in self.pressed
            self._round_rect(cr, x, 28, 70, 13, 6)
            self._fill_or_stroke(cr, on)
            self._text(cr, blabel, x + 35, 34.5, 8, on=on)

    def _stick(self, cr, cx, cy, ax, ay, thumb, label):
        cr.new_sub_path()
        cr.arc(cx, cy, 24, 0, 2 * math.pi)
        self._stroke(cr, 0.5)
        x = max(-1.0, min(1.0, self.axes.get(ax, 0) / 32767.0))
        y = max(-1.0, min(1.0, self.axes.get(ay, 0) / 32767.0))
        kx, ky = cx + x * 13, cy + y * 13
        if abs(x) > 0.08 or abs(y) > 0.08:
            cr.move_to(cx, cy)
            cr.line_to(kx, ky)
            cr.set_source_rgba(*self.accent, 0.8)
            cr.stroke()
        on = thumb in self.pressed
        cr.new_sub_path()
        cr.arc(kx, ky, 12, 0, 2 * math.pi)
        self._fill_or_stroke(cr, on)
        self._text(cr, label, kx, ky, 7.5, on=on)

    def _dpad(self, cr, cx, cy):
        hx = self.axes.get(E.ABS_HAT0X, 0)
        hy = self.axes.get(E.ABS_HAT0Y, 0)
        arm, half, length = 7.0, 7.0, 17.0
        arms = {
            "up": ((cx - half, cy - half - length, arm * 2, length), hy < 0),
            "down": ((cx - half, cy + half, arm * 2, length), hy > 0),
            "left": ((cx - half - length, cy - half, length, arm * 2), hx < 0),
            "right": ((cx + half, cy - half, length, arm * 2), hx > 0),
        }
        for (x, y, w, h), on in arms.values():
            self._round_rect(cr, x, y, w, h, 3)
            self._fill_or_stroke(cr, on)
        cr.rectangle(cx - half, cy - half, half * 2, half * 2)
        self._stroke(cr, 0.3)

    def _face(self, cr, cx, cy, labels):
        offsets = {SOUTH: (0, 23), EAST: (23, 0), WEST: (-23, 0), NORTH: (0, -23)}
        for code, (dx, dy) in offsets.items():
            x, y = cx + dx, cy + dy
            on = code in self.pressed
            cr.new_sub_path()
            cr.arc(x, y, 11, 0, 2 * math.pi)
            self._fill_or_stroke(cr, on)
            label = labels[code]
            if label in ("cross", "circle", "square", "triangle"):
                self._sony_symbol(cr, label, x, y, on)
            else:
                self._text(cr, label, x, y, 9.5, on=on)

    def _sony_symbol(self, cr, kind, x, y, on):
        cr.save()
        cr.set_line_width(1.6)
        if kind == "cross":
            cr.move_to(x - 4.5, y - 4.5)
            cr.line_to(x + 4.5, y + 4.5)
            cr.move_to(x + 4.5, y - 4.5)
            cr.line_to(x - 4.5, y + 4.5)
        elif kind == "circle":
            cr.new_sub_path()
            cr.arc(x, y, 5, 0, 2 * math.pi)
        elif kind == "square":
            cr.rectangle(x - 4.5, y - 4.5, 9, 9)
        else:
            cr.move_to(x, y - 5.5)
            cr.line_to(x + 5.5, y + 4)
            cr.line_to(x - 5.5, y + 4)
            cr.close_path()
        if on:
            cr.set_source_rgba(1, 1, 1, 0.95)
            cr.stroke()
        else:
            self._stroke(cr, 0.85)
        cr.restore()

    def _small(self, cr, pos, labels):
        sel, start, mode = labels
        for key, name, label in ((E.BTN_SELECT, "select", sel), (E.BTN_START, "start", start)):
            x, y = pos[name]
            on = key in self.pressed
            self._round_rect(cr, x - 10, y - 5, 20, 10, 5)
            self._fill_or_stroke(cr, on)
            self._text(cr, label, x, y + 13, 6.5, bold=False)
        x, y = pos["mode"]
        on = E.BTN_MODE in self.pressed
        cr.new_sub_path()
        cr.arc(x, y, 10, 0, 2 * math.pi)
        self._fill_or_stroke(cr, on)
        self._text(cr, mode, x, y, 6.5, on=on)

    def _draw(self, _area, cr, width, height):
        self.fg, self.accent = self._colors()
        fam = FAMILIES[self.family]
        pos = fam["pos"]
        scale = min(width / W, height / H)
        cr.translate((width - W * scale) / 2, (height - H * scale) / 2)
        cr.scale(scale, scale)
        cr.set_line_width(1.5)

        self._body(cr)
        if "touchpad" in pos:
            tx, ty = pos["touchpad"]
            self._round_rect(cr, tx - 42, ty - 26, 84, 50, 8)
            self._stroke(cr, 0.3)
        self._shoulders(cr, fam["shoulder"])
        self._stick(cr, *pos["ls"], E.ABS_X, E.ABS_Y, E.BTN_THUMBL, "L3" if self.family == "sony" else "LS")
        self._stick(cr, *pos["rs"], E.ABS_RX, E.ABS_RY, E.BTN_THUMBR, "R3" if self.family == "sony" else "RS")
        self._dpad(cr, *pos["dpad"])
        self._face(cr, *pos["face"], fam["face"])
        self._small(cr, pos, fam["small"])
        if not self.active:
            self._text(cr, "press Start to watch input", W / 2, H - 6, 9, bold=False)
