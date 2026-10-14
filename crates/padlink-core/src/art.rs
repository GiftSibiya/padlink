//! Controller artwork (SVG) for the Input tab: naming contract, lookup,
//! validation and template generation. No rendering here.
//!
//! Artwork contract (see README "Controller artwork"):
//! - one SVG per controller family (xbox, playstation, nintendo), optionally a
//!   more specific one per model (xbox360, xboxone, xboxseries, ds4, dualsense,
//!   switchpro); lookup prefers the model file
//! - interactive parts are single *shapes* (path/rect/circle/ellipse/polygon)
//!   with these ids; padlink fills them with the accent colour when pressed
//! - pure black (#000, #000000, black, Illustrator's #231F20) strokes/fills are
//!   recoloured to the theme's text colour; other colours are kept as drawn
//! - an element with id "guides" is never shown

use std::fs;
use std::path::{Path, PathBuf};

use crate::{paths, py};

pub const FAMILIES: [&str; 3] = ["xbox", "playstation", "nintendo"];

/// vid:pid -> model art name.
pub const MODELS: [((u16, u16), &str); 7] = [
    ((0x045E, 0x028E), "xbox360"),
    ((0x045E, 0x02EA), "xboxone"),
    ((0x045E, 0x0B12), "xboxseries"),
    ((0x054C, 0x05C4), "ds4"),
    ((0x054C, 0x09CC), "ds4"),
    ((0x054C, 0x0CE6), "dualsense"),
    ((0x057E, 0x2009), "switchpro"),
];

pub const BUTTON_IDS: [&str; 13] = [
    "btn-south",
    "btn-east",
    "btn-west",
    "btn-north",
    "btn-lb",
    "btn-rb",
    "btn-select",
    "btn-start",
    "btn-home",
    "dpad-up",
    "dpad-down",
    "dpad-left",
    "dpad-right",
];
pub const ANALOG_IDS: [&str; 6] = ["lt", "rt", "ls-ring", "ls-knob", "rs-ring", "rs-knob"];
pub const OPTIONAL_IDS: [&str; 2] = ["touchpad", "btn-capture"];
pub const SHAPE_TAGS: [&str; 7] = ["path", "rect", "circle", "ellipse", "polygon", "polyline", "line"];
pub const SVG_NS: &str = "http://www.w3.org/2000/svg";

pub fn required_ids() -> Vec<&'static str> {
    std::iter::once("body").chain(BUTTON_IDS).chain(ANALOG_IDS).collect()
}

pub fn user_art_dir() -> PathBuf {
    paths::config_dir().join("art")
}

pub fn family_of(vendor: u16) -> &'static str {
    match vendor {
        0x054C => "playstation",
        0x057E => "nintendo",
        _ => "xbox",
    }
}

pub fn candidates(vendor: u16, product: u16) -> Vec<&'static str> {
    let model = MODELS.iter().find(|(id, _)| *id == (vendor, product)).map(|&(_, name)| name);
    model.into_iter().chain([family_of(vendor)]).collect()
}

/// The first `<name>.svg` among the candidates, searching `dirs` in order
/// (callers pass the user art dir first so it wins over shipped art).
pub fn find(vendor: u16, product: u16, dirs: &[PathBuf]) -> Option<PathBuf> {
    candidates(vendor, product)
        .into_iter()
        .flat_map(|name| dirs.iter().map(move |d| d.join(format!("{name}.svg"))))
        .find(|p| p.is_file())
}

/// Python's `float()` on an SVG length after stripping a trailing unit (`"20px"`, `"100%"`).
fn length(text: &str) -> Option<f64> {
    let trimmed = text.trim_end_matches(|c: char| c.is_ascii_lowercase() || c == '%');
    trimmed.trim().parse().ok()
}

/// Illustrator exports carry a DOCTYPE, which roxmltree refuses by default.
fn parse_svg(svg: &str) -> Result<roxmltree::Document<'_>, roxmltree::Error> {
    roxmltree::Document::parse_with_options(svg, roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() })
}

/// (min-x, min-y, width, height) from viewBox, else from width/height.
pub fn viewbox(svg: &str) -> Option<[f64; 4]> {
    let doc = parse_svg(svg).ok()?;
    let root = doc.root_element();
    if let Some(vb) = root.attribute("viewBox") {
        let parts: Option<Vec<f64>> =
            vb.trim().split([' ', ',']).filter(|s| !s.is_empty()).map(|s| s.parse().ok()).collect();
        if let Some([x, y, w, h]) = parts.as_deref() {
            if *w > 0.0 && *h > 0.0 {
                return Some([*x, *y, *w, *h]);
            }
        }
    }
    Some([0.0, 0.0, length(root.attribute("width")?)?, length(root.attribute("height")?)?])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Warn => "warn",
            Status::Fail => "fail",
        }
    }
}

/// Validate an artwork file.
pub fn check(path: &Path) -> Vec<(Status, String)> {
    match fs::read_to_string(path) {
        Ok(text) => check_svg(&text),
        Err(e) => vec![(Status::Fail, format!("cannot parse SVG: {e}"))],
    }
}

pub fn check_svg(svg: &str) -> Vec<(Status, String)> {
    let doc = match parse_svg(svg) {
        Ok(d) => d,
        Err(e) => return vec![(Status::Fail, format!("cannot parse SVG: {e}"))],
    };
    let mut out = Vec::new();
    if viewbox(svg).is_none() {
        out.push((Status::Fail, "no viewBox (or width/height); export with 'Responsive' on".to_string()));
    }
    // id -> tag; the last element with an id wins, ordered by first appearance (a Python dict)
    let mut elements: Vec<(String, String)> = Vec::new();
    for node in doc.descendants().filter(|n| n.is_element()) {
        if let Some(id) = node.attribute("id") {
            let tag = node.tag_name().name().to_string();
            match elements.iter_mut().find(|(k, _)| k == id) {
                Some(entry) => entry.1 = tag,
                None => elements.push((id.to_string(), tag)),
            }
        }
    }
    for eid in required_ids() {
        match elements.iter().find(|(k, _)| k == eid) {
            None => {
                let mangled = elements.iter().map(|(k, _)| k).find(|k| {
                    k.replace("_x5F_", "_")
                        .trim_end_matches(|c: char| c == '_' || c.is_ascii_digit())
                        .trim_end_matches('_')
                        == eid
                });
                let hint = mangled
                    .map(|m| {
                        format!(" (found {}: rename it, Illustrator added a suffix)", py::repr(&m.as_str().into()))
                    })
                    .unwrap_or_default();
                out.push((Status::Fail, format!("missing #{eid}{hint}")));
            }
            Some((_, tag)) if eid != "body" && !SHAPE_TAGS.contains(&tag.as_str()) => {
                out.push((
                    Status::Warn,
                    format!("#{eid} is a <{tag}>: name the shape itself, not a group, or it can't be filled"),
                ));
            }
            Some(_) => {}
        }
    }
    for eid in OPTIONAL_IDS {
        if elements.iter().any(|(k, _)| k == eid) {
            out.push((Status::Ok, format!("optional #{eid} present")));
        }
    }
    let has = |tag: &str| {
        doc.root_element()
            .descendants()
            .skip(1)
            .any(|n| n.is_element() && n.tag_name().name() == tag && n.tag_name().namespace() == Some(SVG_NS))
    };
    if has("style") {
        out.push((
            Status::Warn,
            "uses a <style> block (Illustrator 'Internal CSS'); export with 'Presentation Attributes' so theme \
             recolouring works"
                .into(),
        ));
    }
    if has("image") {
        out.push((Status::Warn, "contains embedded raster images; these won't recolour or scale crisply".into()));
    }
    if has("text") {
        out.push((
            Status::Warn,
            "contains live <text>; convert text to outlines so it looks the same everywhere".into(),
        ));
    }
    if !out.iter().any(|(s, _)| *s == Status::Fail) {
        out.insert(0, (Status::Ok, format!("all {} required elements present", required_ids().len())));
    }
    out
}

// ------------------------------------------------------------------ templates
// Positions in a 400x260 design space (the built-in drawing uses the same).

struct Layout {
    ls: (f64, f64),
    dpad: (f64, f64),
    rs: (f64, f64),
    face: (f64, f64),
    select: (f64, f64),
    start: (f64, f64),
    mode: (f64, f64),
    touchpad: Option<(f64, f64)>,
}

struct Labels {
    face: [&'static str; 4],
    shoulder: [&'static str; 4],
    small: [&'static str; 3],
}

fn layout(family: &str) -> Option<(Layout, Labels)> {
    let xbox_like = |face, shoulder, small| {
        (
            Layout {
                ls: (125.0, 100.0),
                dpad: (160.0, 162.0),
                rs: (240.0, 162.0),
                face: (288.0, 100.0),
                select: (172.0, 100.0),
                start: (228.0, 100.0),
                mode: (200.0, 70.0),
                touchpad: None,
            },
            Labels { face, shoulder, small },
        )
    };
    Some(match family {
        "xbox" => xbox_like(["A", "B", "X", "Y"], ["LB", "RB", "LT", "RT"], ["View", "Menu", "Guide"]),
        "nintendo" => xbox_like(["B", "A", "Y", "X"], ["L", "R", "ZL", "ZR"], ["−", "+", "Home"]),
        "playstation" => (
            Layout {
                dpad: (112.0, 100.0),
                face: (288.0, 100.0),
                ls: (158.0, 165.0),
                rs: (242.0, 165.0),
                select: (140.0, 58.0),
                start: (260.0, 58.0),
                mode: (200.0, 165.0),
                touchpad: Some((200.0, 72.0)),
            },
            Labels {
                face: ["cross", "circle", "square", "triangle"],
                shoulder: ["L1", "R1", "L2", "R2"],
                small: ["Share", "Options", "PS"],
            },
        ),
        _ => return None,
    })
}

const BODY_PATH: &str = "M120 40 L280 40 C340 40 370 70 385 150 C395 210 380 250 350 248 C325 246 310 215 290 197 \
                         L110 197 C90 215 75 246 50 248 C20 250 5 210 15 150 C30 70 60 40 120 40 Z";

/// Multiply every number (`-?\d+(\.\d+)?`) in a path by k.
fn scale_path(d: &str, k: f64) -> String {
    let b = d.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        let neg = b[i] == b'-' && b.get(i + 1).is_some_and(u8::is_ascii_digit);
        if neg || b[i].is_ascii_digit() {
            let start = i;
            i += usize::from(neg);
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            if i + 1 < b.len() && b[i] == b'.' && b[i + 1].is_ascii_digit() {
                i += 1;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
            }
            let n: f64 = d[start..i].parse().expect("matched a number");
            out.push_str(&py::fmt_g(n * k));
        } else {
            let ch = d[i..].chars().next().expect("in bounds");
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// An 800x520 template with every required element named and placed.
pub fn template_svg(family: &str) -> Result<String, String> {
    let (lay, lab) =
        layout(family).ok_or_else(|| format!("unknown family '{family}'; choose from {}", FAMILIES.join(", ")))?;
    let k = 2.0;
    let pos = |(x, y): (f64, f64)| (x * k, y * k);
    let g = py::fmt_g;
    const S: &str = r##"fill="none" stroke="#000000" stroke-width="3" stroke-linejoin="round""##;
    const T: &str = r##"fill="#000000" font-family="sans-serif" font-weight="bold" text-anchor="middle" dominant-baseline="central""##;
    let mut a: Vec<String> = Vec::new();
    let rrect = |a: &mut Vec<String>, eid: &str, cx: f64, cy: f64, w: f64, h: f64, r: f64| {
        a.push(format!(
            r#"    <rect id="{eid}" x="{}" y="{}" width="{}" height="{}" rx="{}" {S}/>"#,
            g(cx - w / 2.0),
            g(cy - h / 2.0),
            g(w),
            g(h),
            g(r)
        ));
    };
    let circle = |a: &mut Vec<String>, eid: &str, cx: f64, cy: f64, r: f64| {
        a.push(format!(r#"    <circle id="{eid}" cx="{}" cy="{}" r="{}" {S}/>"#, g(cx), g(cy), g(r)));
    };
    let text = |a: &mut Vec<String>, t: &str, x: f64, y: f64, size: f64| {
        a.push(format!(r#"    <text x="{}" y="{}" font-size="{}" {T}>{t}</text>"#, g(x), g(y), g(size)));
    };

    a.push(format!(r#"    <path id="body" d="{}" {S}/>"#, scale_path(BODY_PATH, k)));
    if let Some(tp) = lay.touchpad {
        let (tx, ty) = pos(tp);
        rrect(&mut a, "touchpad", tx, ty, 168.0, 100.0, 16.0);
    }
    for (side, x) in [("l", 78.0 * k), ("r", 252.0 * k)] {
        rrect(&mut a, &format!("{side}t"), x + 70.0, 26.0, 100.0, 44.0, 12.0);
        rrect(&mut a, &format!("btn-{side}b"), x + 70.0, 69.0, 140.0, 26.0, 12.0);
    }
    for (stick, p) in [("ls", lay.ls), ("rs", lay.rs)] {
        let (x, y) = pos(p);
        circle(&mut a, &format!("{stick}-ring"), x, y, 48.0);
        circle(&mut a, &format!("{stick}-knob"), x, y, 24.0);
    }
    let (cx, cy) = pos(lay.dpad);
    for (eid, (dx, dy, w, h)) in [
        ("dpad-up", (0.0, -31.0, 28.0, 34.0)),
        ("dpad-down", (0.0, 31.0, 28.0, 34.0)),
        ("dpad-left", (-31.0, 0.0, 34.0, 28.0)),
        ("dpad-right", (31.0, 0.0, 34.0, 28.0)),
    ] {
        rrect(&mut a, eid, cx + dx, cy + dy, w, h, 6.0);
    }
    let (fx, fy) = pos(lay.face);
    let face_offsets = [(0.0, 46.0), (46.0, 0.0), (-46.0, 0.0), (0.0, -46.0)];
    for (eid, (dx, dy)) in ["btn-south", "btn-east", "btn-west", "btn-north"].into_iter().zip(face_offsets) {
        circle(&mut a, eid, fx + dx, fy + dy, 22.0);
    }
    for (eid, p) in [("btn-select", lay.select), ("btn-start", lay.start)] {
        let (x, y) = pos(p);
        rrect(&mut a, eid, x, y, 40.0, 20.0, 10.0);
    }
    let (mx, my) = pos(lay.mode);
    circle(&mut a, "btn-home", mx, my, 20.0);

    // labels: not interactive, free to restyle or delete
    a.push("  </g>\n  <g id=\"labels\">".into());
    for ((dx, dy), label) in face_offsets.into_iter().zip(lab.face) {
        let (x, y) = (fx + dx, fy + dy);
        match label {
            "cross" => a.push(format!(
                r#"    <path d="M{} {} L{} {} M{} {} L{} {}" {S}/>"#,
                g(x - 9.0),
                g(y - 9.0),
                g(x + 9.0),
                g(y + 9.0),
                g(x + 9.0),
                g(y - 9.0),
                g(x - 9.0),
                g(y + 9.0)
            )),
            "circle" => a.push(format!(r#"    <circle cx="{}" cy="{}" r="10" {S}/>"#, g(x), g(y))),
            "square" => {
                a.push(format!(r#"    <rect x="{}" y="{}" width="18" height="18" {S}/>"#, g(x - 9.0), g(y - 9.0)))
            }
            "triangle" => a.push(format!(
                r#"    <path d="M{} {} L{} {} L{} {} Z" {S}/>"#,
                g(x),
                g(y - 11.0),
                g(x + 11.0),
                g(y + 8.0),
                g(x - 11.0),
                g(y + 8.0)
            )),
            _ => text(&mut a, label, x, y, 19.0),
        }
    }
    let [lb, rb, lt, rt] = lab.shoulder;
    for (x, b, t) in [(78.0 * k + 70.0, lb, lt), (252.0 * k + 70.0, rb, rt)] {
        text(&mut a, t, x, 26.0, 17.0);
        text(&mut a, b, x, 69.0, 16.0);
    }
    let [sel, start, home] = lab.small;
    let (sx, sy) = pos(lay.select);
    text(&mut a, sel, sx, sy + 26.0, 13.0);
    let (tx, ty) = pos(lay.start);
    text(&mut a, start, tx, ty + 26.0, 13.0);
    text(&mut a, home, mx, my, 12.0);

    let guide_lines = [
        format!("padlink controller template ({family}). Draw your artwork over these shapes."),
        "Keep the object NAMES (they become ids). Interactive parts must be single shapes, not groups.".into(),
        "Black (#000000) strokes/fills follow the light/dark theme. Other colours are kept as drawn.".into(),
        "Export: SVG, Presentation Attributes, Convert text to outlines, Object IDs = Layer Names, Responsive on."
            .into(),
        "This 'guides' layer is hidden by padlink. Delete it or leave it.".into(),
    ];
    let guides = guide_lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            format!(
                r##"    <text x="400" y="{}" font-size="12" fill="#888888" font-family="sans-serif" text-anchor="middle">{line}</text>"##,
                430 + i * 18
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(format!(
        r##"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 800 520" width="800" height="520">
  <g id="guides">
    <rect x="0.5" y="0.5" width="799" height="519" fill="none" stroke="#cccccc" stroke-dasharray="6 6"/>
{guides}
  </g>
  <g id="artwork">
{}
  </g>
</svg>
"##,
        a.join("\n")
    ))
}

/// How far (in the same units) a knob can move from centre inside its ring.
/// Boxes are (x, y, width, height).
pub fn stick_travel(ring: [f64; 4], knob: [f64; 4]) -> f64 {
    ((ring[2].min(ring[3]) - knob[2].min(knob[3])) / 2.0).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_illustrator_doctype() {
        let svg = r#"<?xml version="1.0"?>
<!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 1.1//EN" "http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><rect id="body"/></svg>"#;
        assert_eq!(viewbox(svg), Some([0.0, 0.0, 10.0, 10.0]));
        assert!(check_svg(svg).iter().all(|(_, m)| !m.starts_with("cannot parse")));
    }

    #[test]
    fn unknown_family() {
        assert!(template_svg("sega").unwrap_err().contains("choose from xbox, playstation, nintendo"));
    }
}
