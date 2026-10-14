//! A wireframe gamepad drawn with cairo that lights up as input arrives.
//!
//! The view works in xpad terms (BTN_A, ABS_RX, ...): the Input page runs raw
//! events through the same Translator the bridge uses, so what you see here is
//! also what a game would see through the virtual pad.
//!
//! If artwork exists for the controller (~/.config/padlink/art/*.svg, see
//! padlink_core::art) it is rendered by `SvgArt`; otherwise the built-in cairo
//! drawing below is used, in a 400x260 design space scaled to fit.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::f64::consts::PI;
use std::path::PathBuf;
use std::rc::Rc;

use adw::gtk::{self, cairo};
use adw::prelude::*;
use padlink_core::art;
use padlink_core::codes::*;

use crate::svgart::SvgArt;

const W: f64 = 400.0;
const H: f64 = 260.0;

// Physical position of each xpad button (xpad: 0x133 is the left "X" button).
const SOUTH: u16 = BTN_A;
const EAST: u16 = BTN_B;
const WEST: u16 = 0x133;
const NORTH: u16 = 0x134;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Xbox,
    Sony,
    Nintendo,
}

pub fn family_for(vendor: u16) -> Family {
    match vendor {
        0x054C => Family::Sony,
        0x057E => Family::Nintendo,
        _ => Family::Xbox,
    }
}

struct Layout {
    face: [(u16, &'static str); 4],
    shoulder: [&'static str; 4],
    small: [&'static str; 3],
    ls: (f64, f64),
    dpad: (f64, f64),
    rs: (f64, f64),
    face_pos: (f64, f64),
    select: (f64, f64),
    start: (f64, f64),
    mode: (f64, f64),
    touchpad: Option<(f64, f64)>,
}

fn layout(f: Family) -> Layout {
    let xbox_pos = |face, shoulder, small| Layout {
        face,
        shoulder,
        small,
        ls: (125.0, 100.0),
        dpad: (160.0, 162.0),
        rs: (240.0, 162.0),
        face_pos: (288.0, 100.0),
        select: (172.0, 100.0),
        start: (228.0, 100.0),
        mode: (200.0, 70.0),
        touchpad: None,
    };
    match f {
        Family::Xbox => xbox_pos(
            [(SOUTH, "A"), (EAST, "B"), (WEST, "X"), (NORTH, "Y")],
            ["LB", "RB", "LT", "RT"],
            ["View", "Menu", "Guide"],
        ),
        Family::Nintendo => {
            xbox_pos([(SOUTH, "B"), (EAST, "A"), (WEST, "Y"), (NORTH, "X")], ["L", "R", "ZL", "ZR"], ["−", "+", "Home"])
        }
        Family::Sony => Layout {
            face: [(SOUTH, "cross"), (EAST, "circle"), (WEST, "square"), (NORTH, "triangle")],
            shoulder: ["L1", "R1", "L2", "R2"],
            small: ["Share", "Options", "PS"],
            dpad: (112.0, 100.0),
            face_pos: (288.0, 100.0),
            ls: (158.0, 165.0),
            rs: (242.0, 165.0),
            select: (140.0, 58.0),
            start: (260.0, 58.0),
            mode: (200.0, 165.0),
            touchpad: Some((200.0, 72.0)),
        },
    }
}

type Rgb = (f64, f64, f64);

pub struct PadState {
    pub family: Family,
    pub pressed: HashSet<u16>,
    pub axes: HashMap<u16, i32>,
    pub active: bool,
    pub art: Option<SvgArt>,
}

/// The drawing widget plus its state.
#[derive(Clone)]
pub struct PadView {
    pub area: gtk::DrawingArea,
    state: Rc<RefCell<PadState>>,
}

impl PadView {
    pub fn new() -> Self {
        let area = gtk::DrawingArea::builder().hexpand(true).content_height(280).build();
        let state = Rc::new(RefCell::new(PadState {
            family: Family::Xbox,
            pressed: HashSet::new(),
            axes: HashMap::new(),
            active: false,
            art: None,
        }));
        let st = Rc::clone(&state);
        area.set_draw_func(move |area, cr, w, h| {
            let mut s = st.borrow_mut();
            draw(area, cr, w as f64, h as f64, &mut s);
        });
        PadView { area, state }
    }

    pub fn configure(&self, family: Family, ids: Option<(u16, u16)>) {
        {
            let mut s = self.state.borrow_mut();
            s.family = family;
            s.art = ids.and_then(|(v, p)| art::find(v, p, &art_dirs())).map(SvgArt::new);
        }
        self.reset();
    }

    pub fn art_status(&self) -> String {
        let s = self.state.borrow();
        match &s.art {
            None => "built-in drawing".into(),
            Some(a) => match &a.error {
                Some(e) => format!("artwork {} failed ({e}); using built-in drawing", a.path.display()),
                None => format!("artwork: {}", a.path.display()),
            },
        }
    }

    pub fn reset(&self) {
        {
            let mut s = self.state.borrow_mut();
            s.pressed.clear();
            s.axes.clear();
        }
        self.area.queue_draw();
    }

    pub fn set_active(&self, active: bool) {
        self.state.borrow_mut().active = active;
        self.area.queue_draw();
    }

    pub fn apply(&self, etype: u16, code: u16, value: i32) {
        let mut s = self.state.borrow_mut();
        if etype == EV_KEY {
            if value != 0 {
                s.pressed.insert(code);
            } else {
                s.pressed.remove(&code);
            }
        } else if etype == EV_ABS {
            s.axes.insert(code, value);
        }
    }

    pub fn queue_draw(&self) {
        self.area.queue_draw();
    }
}

/// User art wins over art shipped next to the binary.
pub fn art_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![art::user_art_dir()];
    if let Some(shipped) =
        std::env::current_exe().ok().and_then(|e| Some(e.parent()?.parent()?.join("share/padlink/art")))
    {
        dirs.push(shipped);
    }
    dirs
}

fn colors(area: &gtk::DrawingArea) -> (Rgb, Rgb) {
    let fg = area.color();
    // StyleManager::accent_color_rgba needs libadwaita 1.6; the named color works on every
    // version and still follows the system accent where there is one
    #[allow(deprecated)]
    let acc =
        area.style_context().lookup_color("accent_color").unwrap_or_else(|| gtk::gdk::RGBA::new(0.21, 0.52, 0.89, 1.0));
    ((fg.red() as f64, fg.green() as f64, fg.blue() as f64), (acc.red() as f64, acc.green() as f64, acc.blue() as f64))
}

struct Painter<'a> {
    cr: &'a cairo::Context,
    fg: Rgb,
    accent: Rgb,
    active: bool,
}

impl Painter<'_> {
    fn stroke(&self, alpha: f64) {
        let a = if self.active { alpha } else { alpha * 0.6 };
        self.cr.set_source_rgba(self.fg.0, self.fg.1, self.fg.2, a);
        let _ = self.cr.stroke();
    }

    fn fill_or_stroke(&self, on: bool) {
        if on {
            self.cr.set_source_rgba(self.accent.0, self.accent.1, self.accent.2, 1.0);
            let _ = self.cr.fill_preserve();
        }
        self.stroke(0.65);
    }

    fn round_rect(&self, x: f64, y: f64, w: f64, h: f64, r: f64) {
        let r = r.min(w / 2.0).min(h / 2.0);
        let cr = self.cr;
        cr.new_sub_path();
        cr.arc(x + w - r, y + r, r, -PI / 2.0, 0.0);
        cr.arc(x + w - r, y + h - r, r, 0.0, PI / 2.0);
        cr.arc(x + r, y + h - r, r, PI / 2.0, PI);
        cr.arc(x + r, y + r, r, PI, 3.0 * PI / 2.0);
        cr.close_path();
    }

    fn text(&self, text: &str, x: f64, y: f64, size: f64, on: bool, bold: bool) {
        let cr = self.cr;
        let weight = if bold { cairo::FontWeight::Bold } else { cairo::FontWeight::Normal };
        cr.select_font_face("Sans", cairo::FontSlant::Normal, weight);
        cr.set_font_size(size);
        if let Ok(ext) = cr.text_extents(text) {
            cr.move_to(x - ext.width() / 2.0 - ext.x_bearing(), y - ext.height() / 2.0 - ext.y_bearing());
        }
        if on {
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.95);
        } else {
            cr.set_source_rgba(self.fg.0, self.fg.1, self.fg.2, if self.active { 0.8 } else { 0.5 });
        }
        let _ = cr.show_text(text);
        cr.new_path(); // show_text leaves a current point; arcs would connect to it
    }

    fn body(&self) {
        let cr = self.cr;
        cr.new_path();
        cr.move_to(120.0, 40.0);
        cr.line_to(280.0, 40.0);
        cr.curve_to(340.0, 40.0, 370.0, 70.0, 385.0, 150.0);
        cr.curve_to(395.0, 210.0, 380.0, 250.0, 350.0, 248.0);
        cr.curve_to(325.0, 246.0, 310.0, 215.0, 290.0, 197.0);
        cr.line_to(110.0, 197.0);
        cr.curve_to(90.0, 215.0, 75.0, 246.0, 50.0, 248.0);
        cr.curve_to(20.0, 250.0, 5.0, 210.0, 15.0, 150.0);
        cr.curve_to(30.0, 70.0, 60.0, 40.0, 120.0, 40.0);
        cr.close_path();
        cr.set_source_rgba(self.fg.0, self.fg.1, self.fg.2, 0.04);
        let _ = cr.fill_preserve();
        cr.set_line_width(2.0);
        self.stroke(0.55);
        cr.set_line_width(1.5);
    }

    fn shoulders(&self, s: &PadState, labels: [&str; 4]) {
        let [lb, rb, lt, rt] = labels;
        let cr = self.cr;
        for (x, key, axis, digital, blabel, tlabel) in
            [(78.0, BTN_TL, ABS_Z, BTN_TL2, lb, lt), (252.0, BTN_TR, ABS_RZ, BTN_TR2, rb, rt)]
        {
            // trigger: fills from the bottom with the analog value; outline lights on the digital click
            let val = s.axes.get(&axis).copied().unwrap_or(0).clamp(0, 255) as f64 / 255.0;
            self.round_rect(x + 10.0, 2.0, 50.0, 22.0, 6.0);
            if s.pressed.contains(&digital) {
                cr.set_source_rgba(self.accent.0, self.accent.1, self.accent.2, 1.0);
                cr.set_line_width(2.5);
                let _ = cr.stroke();
                cr.set_line_width(1.5);
            } else {
                self.stroke(0.65);
            }
            if val > 0.0 {
                let _ = cr.save();
                self.round_rect(x + 10.0, 2.0, 50.0, 22.0, 6.0);
                cr.clip();
                cr.rectangle(x + 10.0, 2.0 + 22.0 * (1.0 - val), 50.0, 22.0 * val);
                cr.set_source_rgba(self.accent.0, self.accent.1, self.accent.2, 0.35 + 0.65 * val);
                let _ = cr.fill();
                let _ = cr.restore();
            }
            self.text(tlabel, x + 35.0, 13.0, 8.5, val > 0.5, true);
            let on = s.pressed.contains(&key);
            self.round_rect(x, 28.0, 70.0, 13.0, 6.0);
            self.fill_or_stroke(on);
            self.text(blabel, x + 35.0, 34.5, 8.0, on, true);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn stick(&self, s: &PadState, (cx, cy): (f64, f64), ax: u16, ay: u16, thumb: u16, label: &str) {
        let cr = self.cr;
        cr.new_sub_path();
        cr.arc(cx, cy, 24.0, 0.0, 2.0 * PI);
        self.stroke(0.5);
        let norm = |code: u16| (s.axes.get(&code).copied().unwrap_or(0) as f64 / 32767.0).clamp(-1.0, 1.0);
        let (x, y) = (norm(ax), norm(ay));
        let (kx, ky) = (cx + x * 13.0, cy + y * 13.0);
        if x.abs() > 0.08 || y.abs() > 0.08 {
            cr.move_to(cx, cy);
            cr.line_to(kx, ky);
            cr.set_source_rgba(self.accent.0, self.accent.1, self.accent.2, 0.8);
            let _ = cr.stroke();
        }
        let on = s.pressed.contains(&thumb);
        cr.new_sub_path();
        cr.arc(kx, ky, 12.0, 0.0, 2.0 * PI);
        self.fill_or_stroke(on);
        self.text(label, kx, ky, 7.5, on, true);
    }

    fn dpad(&self, s: &PadState, (cx, cy): (f64, f64)) {
        let hx = s.axes.get(&ABS_HAT0X).copied().unwrap_or(0);
        let hy = s.axes.get(&ABS_HAT0Y).copied().unwrap_or(0);
        let (arm, half, length) = (7.0, 7.0, 17.0);
        for ((x, y, w, h), on) in [
            ((cx - half, cy - half - length, arm * 2.0, length), hy < 0),
            ((cx - half, cy + half, arm * 2.0, length), hy > 0),
            ((cx - half - length, cy - half, length, arm * 2.0), hx < 0),
            ((cx + half, cy - half, length, arm * 2.0), hx > 0),
        ] {
            self.round_rect(x, y, w, h, 3.0);
            self.fill_or_stroke(on);
        }
        self.cr.rectangle(cx - half, cy - half, half * 2.0, half * 2.0);
        self.stroke(0.3);
    }

    fn face(&self, s: &PadState, (cx, cy): (f64, f64), labels: [(u16, &str); 4]) {
        let offset = |code: u16| match code {
            SOUTH => (0.0, 23.0),
            EAST => (23.0, 0.0),
            WEST => (-23.0, 0.0),
            _ => (0.0, -23.0),
        };
        for code in [SOUTH, EAST, WEST, NORTH] {
            let (dx, dy) = offset(code);
            let (x, y) = (cx + dx, cy + dy);
            let on = s.pressed.contains(&code);
            self.cr.new_sub_path();
            self.cr.arc(x, y, 11.0, 0.0, 2.0 * PI);
            self.fill_or_stroke(on);
            let label = labels.iter().find(|(c, _)| *c == code).map_or("", |(_, l)| l);
            if matches!(label, "cross" | "circle" | "square" | "triangle") {
                self.sony_symbol(label, x, y, on);
            } else {
                self.text(label, x, y, 9.5, on, true);
            }
        }
    }

    fn sony_symbol(&self, kind: &str, x: f64, y: f64, on: bool) {
        let cr = self.cr;
        let _ = cr.save();
        cr.set_line_width(1.6);
        match kind {
            "cross" => {
                cr.move_to(x - 4.5, y - 4.5);
                cr.line_to(x + 4.5, y + 4.5);
                cr.move_to(x + 4.5, y - 4.5);
                cr.line_to(x - 4.5, y + 4.5);
            }
            "circle" => {
                cr.new_sub_path();
                cr.arc(x, y, 5.0, 0.0, 2.0 * PI);
            }
            "square" => cr.rectangle(x - 4.5, y - 4.5, 9.0, 9.0),
            _ => {
                cr.move_to(x, y - 5.5);
                cr.line_to(x + 5.5, y + 4.0);
                cr.line_to(x - 5.5, y + 4.0);
                cr.close_path();
            }
        }
        if on {
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.95);
            let _ = cr.stroke();
        } else {
            self.stroke(0.85);
        }
        let _ = cr.restore();
    }

    fn small(&self, s: &PadState, lay: &Layout) {
        let [sel, start, mode] = lay.small;
        for (key, (x, y), label) in [(BTN_SELECT, lay.select, sel), (BTN_START, lay.start, start)] {
            let on = s.pressed.contains(&key);
            self.round_rect(x - 10.0, y - 5.0, 20.0, 10.0, 5.0);
            self.fill_or_stroke(on);
            self.text(label, x, y + 13.0, 6.5, false, false);
        }
        let (x, y) = lay.mode;
        let on = s.pressed.contains(&BTN_MODE);
        self.cr.new_sub_path();
        self.cr.arc(x, y, 10.0, 0.0, 2.0 * PI);
        self.fill_or_stroke(on);
        self.text(mode, x, y, 6.5, on, true);
    }
}

fn draw(area: &gtk::DrawingArea, cr: &cairo::Context, width: f64, height: f64, s: &mut PadState) {
    let (fg, accent) = colors(area);
    let active = s.active;
    if let Some(art) = s.art.as_mut() {
        art.maybe_reload();
        if art.error.is_none() {
            let _ = cr.save();
            let result = art.render(cr, width, height, &s.pressed, &s.axes, fg, accent, active);
            let _ = cr.restore();
            match result {
                Ok(()) => {
                    if !active {
                        let p = Painter { cr, fg, accent, active };
                        p.text("press Start to watch input", width / 2.0, height - 8.0, 12.0, false, false);
                    }
                    return;
                }
                Err(e) => {
                    // fall back to the built-in drawing
                    art.error = Some(e);
                    cr.new_path();
                }
            }
        }
    }
    let lay = layout(s.family);
    let scale = (width / W).min(height / H);
    cr.translate((width - W * scale) / 2.0, (height - H * scale) / 2.0);
    cr.scale(scale, scale);
    cr.set_line_width(1.5);
    let p = Painter { cr, fg, accent, active };
    p.body();
    if let Some((tx, ty)) = lay.touchpad {
        p.round_rect(tx - 42.0, ty - 26.0, 84.0, 50.0, 8.0);
        p.stroke(0.3);
    }
    p.shoulders(s, lay.shoulder);
    let sony = s.family == Family::Sony;
    p.stick(s, lay.ls, ABS_X, ABS_Y, BTN_THUMBL, if sony { "L3" } else { "LS" });
    p.stick(s, lay.rs, ABS_RX, ABS_RY, BTN_THUMBR, if sony { "R3" } else { "RS" });
    p.dpad(s, lay.dpad);
    p.face(s, lay.face_pos, lay.face);
    p.small(s, &lay);
    if !active {
        p.text("press Start to watch input", W / 2.0, H - 6.0, 9.0, false, false);
    }
}
