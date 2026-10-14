"""Controller artwork (SVG) for the Input tab: naming contract, lookup,
validation and template generation. No GTK here; the renderer is
`crates/padlink-gtk/src/svgart.rs`.

Artwork contract (see README "Controller artwork"):
- one SVG per controller family (xbox, playstation, nintendo), optionally a
  more specific one per model (xbox360, xboxone, xboxseries, ds4, dualsense,
  switchpro); lookup prefers the model file
- interactive parts are single *shapes* (path/rect/circle/ellipse/polygon)
  with these ids; padlink fills them with the accent colour when pressed
- pure black (#000, #000000, black, Illustrator's #231F20) strokes/fills are
  recoloured to the theme's text colour; other colours are kept as drawn
- an element with id "guides" is never shown
"""

from __future__ import annotations

import re
import xml.etree.ElementTree as ET
from pathlib import Path

from padlink.core.paths import CONFIG_DIR

USER_ART_DIR = CONFIG_DIR / "art"
BUILTIN_ART_DIR = Path(__file__).resolve().parent.parent / "gtk" / "art"
TEMPLATE_DIR = Path(__file__).resolve().parents[2] / "data" / "art-templates"

FAMILIES = ("xbox", "playstation", "nintendo")
MODELS = {  # vid:pid -> model art name
    (0x045E, 0x028E): "xbox360", (0x045E, 0x02EA): "xboxone", (0x045E, 0x0B12): "xboxseries",
    (0x054C, 0x05C4): "ds4", (0x054C, 0x09CC): "ds4", (0x054C, 0x0CE6): "dualsense",
    (0x057E, 0x2009): "switchpro",
}

BUTTON_IDS = ["btn-south", "btn-east", "btn-west", "btn-north", "btn-lb", "btn-rb",
              "btn-select", "btn-start", "btn-home", "dpad-up", "dpad-down", "dpad-left", "dpad-right"]
ANALOG_IDS = ["lt", "rt", "ls-ring", "ls-knob", "rs-ring", "rs-knob"]
REQUIRED_IDS = ["body"] + BUTTON_IDS + ANALOG_IDS
OPTIONAL_IDS = ["touchpad", "btn-capture"]
SHAPE_TAGS = {"path", "rect", "circle", "ellipse", "polygon", "polyline", "line"}
SVG_NS = "http://www.w3.org/2000/svg"


def family_of(vendor: int) -> str:
    return {0x054C: "playstation", 0x057E: "nintendo"}.get(vendor, "xbox")


def candidates(vendor: int, product: int) -> list[str]:
    names = []
    model = MODELS.get((vendor, product))
    if model:
        names.append(model)
    names.append(family_of(vendor))
    return names


def find(vendor: int, product: int, dirs: list[Path] | None = None) -> Path | None:
    """User art (~/.config/padlink/art) wins over art shipped with padlink."""
    for name in candidates(vendor, product):
        for d in dirs or [USER_ART_DIR, BUILTIN_ART_DIR]:
            p = d / f"{name}.svg"
            if p.is_file():
                return p
    return None


def viewbox(path_or_bytes) -> tuple[float, float, float, float] | None:
    data = path_or_bytes.read_bytes() if isinstance(path_or_bytes, Path) else path_or_bytes
    root = ET.fromstring(data)
    vb = root.get("viewBox")
    if vb:
        parts = [float(x) for x in re.split(r"[ ,]+", vb.strip())]
        if len(parts) == 4 and parts[2] > 0 and parts[3] > 0:
            return tuple(parts)
    w, h = root.get("width"), root.get("height")
    try:
        return (0.0, 0.0, float(re.sub(r"[a-z%]+$", "", w)), float(re.sub(r"[a-z%]+$", "", h)))
    except (TypeError, ValueError):
        return None


def check(path: Path) -> list[tuple[str, str]]:
    """Validate an artwork file. Returns [(status, message)], status in ok/warn/fail."""
    out: list[tuple[str, str]] = []
    try:
        root = ET.parse(path).getroot()
    except (OSError, ET.ParseError) as exc:
        return [("fail", f"cannot parse SVG: {exc}")]
    if viewbox(path) is None:
        out.append(("fail", "no viewBox (or width/height); export with 'Responsive' on"))
    elements: dict[str, str] = {}
    for el in root.iter():
        eid = el.get("id")
        if eid:
            elements[eid] = el.tag.split("}")[-1]
    for eid in REQUIRED_IDS:
        tag = elements.get(eid)
        if tag is None:
            mangled = [k for k in elements if k.replace("_x5F_", "_").rstrip("_0123456789").rstrip("_") == eid]
            hint = f" (found {mangled[0]!r}: rename it, Illustrator added a suffix)" if mangled else ""
            out.append(("fail", f"missing #{eid}{hint}"))
        elif eid != "body" and tag not in SHAPE_TAGS:
            out.append(("warn", f"#{eid} is a <{tag}>: name the shape itself, not a group, or it can't be filled"))
    for eid in OPTIONAL_IDS:
        if eid in elements:
            out.append(("ok", f"optional #{eid} present"))
    if root.find(f".//{{{SVG_NS}}}style") is not None:
        out.append(("warn", "uses a <style> block (Illustrator 'Internal CSS'); export with 'Presentation "
                            "Attributes' so theme recolouring works"))
    if root.find(f".//{{{SVG_NS}}}image") is not None:
        out.append(("warn", "contains embedded raster images; these won't recolour or scale crisply"))
    if root.find(f".//{{{SVG_NS}}}text") is not None:
        out.append(("warn", "contains live <text>; convert text to outlines so it looks the same everywhere"))
    if not any(s == "fail" for s, _ in out):
        out.insert(0, ("ok", f"all {len(REQUIRED_IDS)} required elements present"))
    return out


# ------------------------------------------------------------------ templates
# Positions in a 400x260 design space (the built-in drawing uses the same).
LAYOUTS = {
    "xbox": {"ls": (125, 100), "dpad": (160, 162), "rs": (240, 162), "face": (288, 100),
             "select": (172, 100), "start": (228, 100), "mode": (200, 70)},
    "playstation": {"dpad": (112, 100), "face": (288, 100), "ls": (158, 165), "rs": (242, 165),
                    "select": (140, 58), "start": (260, 58), "mode": (200, 165), "touchpad": (200, 72)},
    "nintendo": {"ls": (125, 100), "dpad": (160, 162), "rs": (240, 162), "face": (288, 100),
                 "select": (172, 100), "start": (228, 100), "mode": (200, 70)},
}
LABELS = {
    "xbox": {"face": ("A", "B", "X", "Y"), "shoulder": ("LB", "RB", "LT", "RT"), "small": ("View", "Menu", "Guide")},
    "playstation": {"face": ("cross", "circle", "square", "triangle"), "shoulder": ("L1", "R1", "L2", "R2"),
                    "small": ("Share", "Options", "PS")},
    "nintendo": {"face": ("B", "A", "Y", "X"), "shoulder": ("L", "R", "ZL", "ZR"), "small": ("−", "+", "Home")},
}
BODY_PATH = ("M120 40 L280 40 C340 40 370 70 385 150 C395 210 380 250 350 248 C325 246 310 215 290 197 "
             "L110 197 C90 215 75 246 50 248 C20 250 5 210 15 150 C30 70 60 40 120 40 Z")


def _scale_path(d: str, k: float) -> str:
    return re.sub(r"-?\d+(\.\d+)?", lambda m: f"{float(m.group()) * k:g}", d)


def template_svg(family: str) -> str:
    """An 800x520 template with every required element named and placed."""
    if family not in LAYOUTS:
        raise KeyError(f"unknown family {family!r}; choose from {', '.join(FAMILIES)}")
    k = 2.0
    pos = {n: (x * k, y * k) for n, (x, y) in LAYOUTS[family].items()}
    lab = LABELS[family]
    S = 'fill="none" stroke="#000000" stroke-width="3" stroke-linejoin="round"'
    T = 'fill="#000000" font-family="sans-serif" font-weight="bold" text-anchor="middle" dominant-baseline="central"'
    a: list[str] = []
    add = a.append

    def rrect(eid, cx, cy, w, h, r):
        add(f'    <rect id="{eid}" x="{cx - w / 2:g}" y="{cy - h / 2:g}" width="{w:g}" height="{h:g}" rx="{r:g}" {S}/>')

    def circle(eid, cx, cy, r):
        add(f'    <circle id="{eid}" cx="{cx:g}" cy="{cy:g}" r="{r:g}" {S}/>')

    def text(t, x, y, size):
        add(f'    <text x="{x:g}" y="{y:g}" font-size="{size:g}" {T}>{t}</text>')

    add(f'    <path id="body" d="{_scale_path(BODY_PATH, k)}" {S}/>')
    if "touchpad" in pos:
        tx, ty = pos["touchpad"]
        rrect("touchpad", tx, ty, 168, 100, 16)
    for side, x in (("l", 78 * k), ("r", 252 * k)):
        rrect(f"{side}t", x + 70, 26, 100, 44, 12)
        rrect(f"btn-{side}b", x + 70, 69, 140, 26, 12)
    for stick in ("ls", "rs"):
        x, y = pos[stick]
        circle(f"{stick}-ring", x, y, 48)
        circle(f"{stick}-knob", x, y, 24)
    cx, cy = pos["dpad"]
    for eid, (dx, dy, w, h) in {"dpad-up": (0, -31, 28, 34), "dpad-down": (0, 31, 28, 34),
                                "dpad-left": (-31, 0, 34, 28), "dpad-right": (31, 0, 34, 28)}.items():
        rrect(eid, cx + dx, cy + dy, w, h, 6)
    fx, fy = pos["face"]
    for eid, (dx, dy) in {"btn-south": (0, 46), "btn-east": (46, 0), "btn-west": (-46, 0), "btn-north": (0, -46)}.items():
        circle(eid, fx + dx, fy + dy, 22)
    for eid, name in (("btn-select", "select"), ("btn-start", "start")):
        rrect(eid, *pos[name], 40, 20, 10)
    circle("btn-home", *pos["mode"], 20)

    # labels: not interactive, free to restyle or delete
    a.append('  </g>\n  <g id="labels">')
    for (dx, dy), label in zip(((0, 46), (46, 0), (-46, 0), (0, -46)), lab["face"]):
        x, y = fx + dx, fy + dy
        if label == "cross":
            add(f'    <path d="M{x - 9:g} {y - 9:g} L{x + 9:g} {y + 9:g} M{x + 9:g} {y - 9:g} L{x - 9:g} {y + 9:g}" {S}/>')
        elif label == "circle":
            add(f'    <circle cx="{x:g}" cy="{y:g}" r="10" {S}/>')
        elif label == "square":
            add(f'    <rect x="{x - 9:g}" y="{y - 9:g}" width="18" height="18" {S}/>')
        elif label == "triangle":
            add(f'    <path d="M{x:g} {y - 11:g} L{x + 11:g} {y + 8:g} L{x - 11:g} {y + 8:g} Z" {S}/>')
        else:
            text(label, x, y, 19)
    lb, rb, lt, rt = lab["shoulder"]
    for x, b, t in ((78 * k + 70, lb, lt), (252 * k + 70, rb, rt)):
        text(t, x, 26, 17)
        text(b, x, 69, 16)
    sel, start, home = lab["small"]
    text(sel, pos["select"][0], pos["select"][1] + 26, 13)
    text(start, pos["start"][0], pos["start"][1] + 26, 13)
    text(home, pos["mode"][0], pos["mode"][1], 12)

    guide_lines = [
        f"padlink controller template ({family}). Draw your artwork over these shapes.",
        "Keep the object NAMES (they become ids). Interactive parts must be single shapes, not groups.",
        "Black (#000000) strokes/fills follow the light/dark theme. Other colours are kept as drawn.",
        "Export: SVG, Presentation Attributes, Convert text to outlines, Object IDs = Layer Names, Responsive on.",
        "This 'guides' layer is hidden by padlink. Delete it or leave it.",
    ]
    guides = "\n".join(
        f'    <text x="400" y="{430 + i * 18}" font-size="12" fill="#888888" font-family="sans-serif" '
        f'text-anchor="middle">{line}</text>' for i, line in enumerate(guide_lines))
    return f"""<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 800 520" width="800" height="520">
  <g id="guides">
    <rect x="0.5" y="0.5" width="799" height="519" fill="none" stroke="#cccccc" stroke-dasharray="6 6"/>
{guides}
  </g>
  <g id="artwork">
{chr(10).join(a)}
  </g>
</svg>
"""


def stick_travel(ring: tuple[float, float, float, float], knob: tuple[float, float, float, float]) -> float:
    """How far (in the same units) a knob can move from centre inside its ring."""
    return max(0.0, (min(ring[2], ring[3]) - min(knob[2], knob[3])) / 2)
