"""Render controller artwork SVGs (see padlink/core/art.py for the contract).

Each frame is drawn in passes, each with a fresh librsvg handle carrying a
generated user stylesheet (stylesheets can't be reliably reset on a handle):
  1. each pulled trigger, filled with the accent colour and clipped to the
     pulled fraction (bottom-up)
  2. the whole document on top: theme recolouring, pressed buttons filled,
     stick knobs hidden
  3. each stick knob, translated by the stick position within its ring
"""

from __future__ import annotations

import os
from pathlib import Path

import gi

gi.require_version("Rsvg", "2.0")
from evdev import ecodes as E  # noqa: E402
from gi.repository import GLib, Rsvg  # noqa: E402

from padlink.core import art  # noqa: E402

KEY_IDS = {E.BTN_A: "btn-south", E.BTN_B: "btn-east", 0x133: "btn-west", 0x134: "btn-north",
           E.BTN_TL: "btn-lb", E.BTN_TR: "btn-rb", E.BTN_SELECT: "btn-select", E.BTN_START: "btn-start",
           E.BTN_MODE: "btn-home", E.BTN_THUMBL: "ls-knob", E.BTN_THUMBR: "rs-knob"}
BLACKS = ("#000", "#000000", "black", "#231f20", "#231F20")


def _rgb(c) -> str:
    return f"rgb({round(c[0] * 255)},{round(c[1] * 255)},{round(c[2] * 255)})"


class SvgArt:
    def __init__(self, path: Path):
        self.path = path
        self.mtime = 0.0
        self.data = b""
        self.vb = None
        self.error: str | None = None
        self.reload()

    def reload(self) -> None:
        try:
            self.mtime = os.stat(self.path).st_mtime
            self.data = self.path.read_bytes()
            self.vb = art.viewbox(self.data)
            Rsvg.Handle.new_from_data(self.data)  # parse check
            self.error = None if self.vb else "no viewBox"
        except (OSError, GLib.Error, Exception) as exc:  # noqa: BLE001 - shown in the UI
            self.error = str(exc)

    def maybe_reload(self) -> bool:
        """Hot reload: pick up edits saved from Illustrator/Inkscape."""
        try:
            if os.stat(self.path).st_mtime != self.mtime:
                self.reload()
                return True
        except OSError:
            pass
        return False

    def _handle(self, css: str) -> Rsvg.Handle:
        h = Rsvg.Handle.new_from_data(self.data)
        h.set_stylesheet(css.encode())
        return h

    @staticmethod
    def _rect(r: Rsvg.Rectangle, vp: Rsvg.Rectangle, vb, scale: float) -> tuple[float, float, float, float]:
        """Map get_geometry_for_layer() output to widget pixels.

        librsvg (2.63) offsets the result by the viewport origin but does not
        scale it to the viewport size, so undo the offset, scale, re-offset.
        """
        return (vp.x + (r.x - vp.x - vb[0]) * scale, vp.y + (r.y - vp.y - vb[1]) * scale,
                r.width * scale, r.height * scale)

    def render(self, cr, width: int, height: int, pressed: set[int], axes: dict[int, int],
               fg, accent, active: bool) -> None:
        if self.error or not self.vb:
            raise RuntimeError(self.error or "no viewBox")
        vb = self.vb
        _vx, _vy, vw, vh = vb
        scale = min(width / vw, height / vh)
        vp = Rsvg.Rectangle()
        vp.width, vp.height = vw * scale, vh * scale
        vp.x, vp.y = (width - vp.width) / 2, (height - vp.height) / 2
        fgc, acc = _rgb(fg), _rgb(accent)

        common = ["#guides { display: none; }"]
        for b in BLACKS:
            common.append(f'[stroke="{b}"] {{ stroke: {fgc}; }}')
            common.append(f'[fill="{b}"] {{ fill: {fgc}; }}')
        on_ids = {KEY_IDS[k] for k in pressed if k in KEY_IDS}
        hx, hy = axes.get(E.ABS_HAT0X, 0), axes.get(E.ABS_HAT0Y, 0)
        on_ids |= {i for i, cond in (("dpad-left", hx < 0), ("dpad-right", hx > 0),
                                     ("dpad-up", hy < 0), ("dpad-down", hy > 0)) if cond}
        fill_on = lambda i: f"#{i} {{ fill: {acc}; fill-opacity: 0.9; }}"  # noqa: E731
        digital = {"lt": E.BTN_TL2 in pressed, "rt": E.BTN_TR2 in pressed}
        trig_css = [f"#{i} {{ stroke: {acc}; }}" for i, on in digital.items() if on]

        cr.push_group()
        # triggers first: accent fill clipped to the pulled fraction, so the artwork
        # (outline, labels) drawn afterwards stays on top of it
        pulled = []
        for tid, axis in (("lt", E.ABS_Z), ("rt", E.ABS_RZ)):
            val = max(0, min(255, axes.get(axis, 0))) / 255.0
            if val <= 0:
                continue
            pulled.append(tid)
            h = self._handle("\n".join(common + [f"#{tid} {{ fill: {acc}; fill-opacity: {0.35 + 0.65 * val:.2f}; "
                                                 f"stroke: none; }}"]))
            ok, ink, _logical = h.get_geometry_for_layer(f"#{tid}", vp)
            if not ok:
                continue
            x, y, w, hgt = self._rect(ink, vp, vb, scale)
            cr.save()
            cr.rectangle(x - 2, y + hgt * (1 - val), w + 4, hgt * val + 2)
            cr.clip()
            h.render_layer(cr, f"#{tid}", vp)
            cr.restore()

        base = self._handle("\n".join(common + [fill_on(i) for i in on_ids if not i.endswith("knob")] + trig_css
                                      + [f"#{t} {{ fill-opacity: 0; }}" for t in pulled]
                                      + ["#ls-knob, #rs-knob { display: none; }"]))
        base.render_document(cr, vp)

        # sticks: move the knob inside its ring
        knobs = self._handle("\n".join(common + [fill_on(i) for i in on_ids if i.endswith("knob")]))
        for stick, ax, ay in (("ls", E.ABS_X, E.ABS_Y), ("rs", E.ABS_RX, E.ABS_RY)):
            ok_r, ring, _ = knobs.get_geometry_for_layer(f"#{stick}-ring", vp)
            ok_k, knob, _ = knobs.get_geometry_for_layer(f"#{stick}-knob", vp)
            if not (ok_r and ok_k):
                continue
            ring_px = self._rect(ring, vp, vb, scale)
            travel = art.stick_travel(ring_px, self._rect(knob, vp, vb, scale))
            x = max(-1.0, min(1.0, axes.get(ax, 0) / 32767.0))
            y = max(-1.0, min(1.0, axes.get(ay, 0) / 32767.0))
            dx, dy = x * travel, y * travel
            if abs(x) > 0.08 or abs(y) > 0.08:
                cx, cy = ring_px[0] + ring_px[2] / 2, ring_px[1] + ring_px[3] / 2
                cr.move_to(cx, cy)
                cr.line_to(cx + dx, cy + dy)
                cr.set_source_rgba(*accent, 0.8)
                cr.set_line_width(max(1.0, 1.5 * scale))
                cr.stroke()
            cr.save()
            cr.translate(dx, dy)
            knobs.render_layer(cr, f"#{stick}-knob", vp)
            cr.restore()

        cr.pop_group_to_source()
        cr.paint_with_alpha(1.0 if active else 0.6)
