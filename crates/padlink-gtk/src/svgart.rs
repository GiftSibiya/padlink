//! Render controller artwork SVGs (see padlink_core::art for the contract).
//!
//! Each frame is drawn in passes, each with a fresh librsvg handle carrying a
//! generated user stylesheet (stylesheets can't be reliably reset on a handle):
//!   1. each pulled trigger, filled with the accent colour and clipped to the
//!      pulled fraction (bottom-up)
//!   2. the whole document on top: theme recolouring, pressed buttons filled,
//!      stick knobs hidden
//!   3. each stick knob, translated by the stick position within its ring

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::SystemTime;

use adw::gtk::cairo;
use padlink_core::art;
use padlink_core::codes::*;
use rsvg::prelude::*;

fn key_id(code: u16) -> Option<&'static str> {
    Some(match code {
        BTN_A => "btn-south",
        BTN_B => "btn-east",
        0x133 => "btn-west",
        0x134 => "btn-north",
        BTN_TL => "btn-lb",
        BTN_TR => "btn-rb",
        BTN_SELECT => "btn-select",
        BTN_START => "btn-start",
        BTN_MODE => "btn-home",
        BTN_THUMBL => "ls-knob",
        BTN_THUMBR => "rs-knob",
        _ => return None,
    })
}

const BLACKS: [&str; 5] = ["#000", "#000000", "black", "#231f20", "#231F20"];

type Rgb = (f64, f64, f64);

fn rgb(c: Rgb) -> String {
    let ch = |v: f64| padlink_core::py::round(v * 255.0);
    format!("rgb({},{},{})", ch(c.0), ch(c.1), ch(c.2))
}

pub struct SvgArt {
    pub path: PathBuf,
    mtime: Option<SystemTime>,
    data: Vec<u8>,
    vb: Option<[f64; 4]>,
    pub error: Option<String>,
}

impl SvgArt {
    pub fn new(path: PathBuf) -> Self {
        let mut a = SvgArt { path, mtime: None, data: Vec::new(), vb: None, error: None };
        a.reload();
        a
    }

    fn reload(&mut self) {
        let result = (|| -> Result<(), String> {
            self.mtime = std::fs::metadata(&self.path).and_then(|m| m.modified()).ok();
            self.data = std::fs::read(&self.path).map_err(|e| e.to_string())?;
            self.vb = art::viewbox(&String::from_utf8_lossy(&self.data));
            rsvg::Handle::from_data(&self.data).map_err(|e| e.to_string())?; // parse check
            if self.vb.is_none() {
                return Err("no viewBox".into());
            }
            Ok(())
        })();
        self.error = result.err();
    }

    /// Hot reload: pick up edits saved from Illustrator/Inkscape.
    pub fn maybe_reload(&mut self) -> bool {
        let mtime = std::fs::metadata(&self.path).and_then(|m| m.modified()).ok();
        if mtime.is_some() && mtime != self.mtime {
            self.reload();
            return true;
        }
        false
    }

    fn handle(&self, css: &str) -> Result<rsvg::Handle, String> {
        let h = rsvg::Handle::from_data(&self.data).map_err(|e| e.to_string())?;
        h.set_stylesheet(css.as_bytes()).map_err(|e| e.to_string())?;
        Ok(h)
    }

    /// Map get_geometry_for_layer() output to widget pixels.
    ///
    /// librsvg (2.63) offsets the result by the viewport origin but does not
    /// scale it to the viewport size, so undo the offset, scale, re-offset.
    fn rect(r: &rsvg::Rectangle, vp: &rsvg::Rectangle, vb: [f64; 4], scale: f64) -> [f64; 4] {
        [
            vp.x() + (r.x() - vp.x() - vb[0]) * scale,
            vp.y() + (r.y() - vp.y() - vb[1]) * scale,
            r.width() * scale,
            r.height() * scale,
        ]
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &self,
        cr: &cairo::Context,
        width: f64,
        height: f64,
        pressed: &HashSet<u16>,
        axes: &HashMap<u16, i32>,
        fg: Rgb,
        accent: Rgb,
        active: bool,
    ) -> Result<(), String> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        let vb = self.vb.ok_or("no viewBox")?;
        let scale = (width / vb[2]).min(height / vb[3]);
        let (vw, vh) = (vb[2] * scale, vb[3] * scale);
        let vp = rsvg::Rectangle::new((width - vw) / 2.0, (height - vh) / 2.0, vw, vh);
        let (fgc, acc) = (rgb(fg), rgb(accent));
        let axis = |code: u16| axes.get(&code).copied().unwrap_or(0);

        let mut common = vec!["#guides { display: none; }".to_string()];
        for b in BLACKS {
            common.push(format!("[stroke=\"{b}\"] {{ stroke: {fgc}; }}"));
            common.push(format!("[fill=\"{b}\"] {{ fill: {fgc}; }}"));
        }
        let mut on_ids: Vec<&str> = pressed.iter().filter_map(|&k| key_id(k)).collect();
        let (hx, hy) = (axis(ABS_HAT0X), axis(ABS_HAT0Y));
        for (id, cond) in [("dpad-left", hx < 0), ("dpad-right", hx > 0), ("dpad-up", hy < 0), ("dpad-down", hy > 0)] {
            if cond {
                on_ids.push(id);
            }
        }
        on_ids.sort_unstable();
        on_ids.dedup();
        let fill_on = |i: &str| format!("#{i} {{ fill: {acc}; fill-opacity: 0.9; }}");
        let trig_css: Vec<String> = [("lt", BTN_TL2), ("rt", BTN_TR2)]
            .iter()
            .filter(|(_, k)| pressed.contains(k))
            .map(|(i, _)| format!("#{i} {{ stroke: {acc}; }}"))
            .collect();
        let err = |e: rsvg::glib::Error| e.to_string();

        cr.push_group();
        let drawn = (|| -> Result<(), String> {
            // triggers first: accent fill clipped to the pulled fraction, so the artwork
            // (outline, labels) drawn afterwards stays on top of it
            let mut pulled = Vec::new();
            for (tid, code) in [("lt", ABS_Z), ("rt", ABS_RZ)] {
                let val = axis(code).clamp(0, 255) as f64 / 255.0;
                if val <= 0.0 {
                    continue;
                }
                pulled.push(tid);
                let mut css = common.clone();
                css.push(format!("#{tid} {{ fill: {acc}; fill-opacity: {:.2}; stroke: none; }}", 0.35 + 0.65 * val));
                let h = self.handle(&css.join("\n"))?;
                let Ok((ink, _)) = h.geometry_for_layer(Some(&format!("#{tid}")), &vp) else { continue };
                let [x, y, w, hgt] = Self::rect(&ink, &vp, vb, scale);
                let _ = cr.save();
                cr.rectangle(x - 2.0, y + hgt * (1.0 - val), w + 4.0, hgt * val + 2.0);
                cr.clip();
                h.render_layer(cr, Some(&format!("#{tid}")), &vp).map_err(err)?;
                let _ = cr.restore();
            }

            let mut css = common.clone();
            css.extend(on_ids.iter().filter(|i| !i.ends_with("knob")).map(|i| fill_on(i)));
            css.extend(trig_css);
            css.extend(pulled.iter().map(|t| format!("#{t} {{ fill-opacity: 0; }}")));
            css.push("#ls-knob, #rs-knob { display: none; }".into());
            self.handle(&css.join("\n"))?.render_document(cr, &vp).map_err(err)?;

            // sticks: move the knob inside its ring
            let mut css = common;
            css.extend(on_ids.iter().filter(|i| i.ends_with("knob")).map(|i| fill_on(i)));
            let knobs = self.handle(&css.join("\n"))?;
            for (stick, ax, ay) in [("ls", ABS_X, ABS_Y), ("rs", ABS_RX, ABS_RY)] {
                let ring = knobs.geometry_for_layer(Some(&format!("#{stick}-ring")), &vp);
                let knob = knobs.geometry_for_layer(Some(&format!("#{stick}-knob")), &vp);
                let (Ok((ring, _)), Ok((knob, _))) = (ring, knob) else { continue };
                let ring_px = Self::rect(&ring, &vp, vb, scale);
                let travel = art::stick_travel(ring_px, Self::rect(&knob, &vp, vb, scale));
                let x = (axis(ax) as f64 / 32767.0).clamp(-1.0, 1.0);
                let y = (axis(ay) as f64 / 32767.0).clamp(-1.0, 1.0);
                let (dx, dy) = (x * travel, y * travel);
                if x.abs() > 0.08 || y.abs() > 0.08 {
                    let (cx, cy) = (ring_px[0] + ring_px[2] / 2.0, ring_px[1] + ring_px[3] / 2.0);
                    cr.move_to(cx, cy);
                    cr.line_to(cx + dx, cy + dy);
                    cr.set_source_rgba(accent.0, accent.1, accent.2, 0.8);
                    cr.set_line_width((1.5 * scale).max(1.0));
                    let _ = cr.stroke();
                }
                let _ = cr.save();
                cr.translate(dx, dy);
                knobs.render_layer(cr, Some(&format!("#{stick}-knob")), &vp).map_err(err)?;
                let _ = cr.restore();
            }
            Ok(())
        })();
        if let Err(e) = drawn {
            let _ = cr.pop_group(); // leave the context as we found it for the fallback drawing
            return Err(e);
        }
        cr.pop_group_to_source().map_err(|e| e.to_string())?;
        cr.paint_with_alpha(if active { 1.0 } else { 0.6 }).map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    const W: i32 = 600;
    const H: i32 = 450;
    const WHITE: Rgb = (1.0, 1.0, 1.0);
    const RED: Rgb = (1.0, 0.0, 0.0);

    struct ArtFile {
        art: SvgArt,
        dir: PathBuf,
    }

    impl ArtFile {
        fn xbox() -> Self {
            let dir = std::env::temp_dir().join(format!("padlink-svgart-{}-{}", std::process::id(), unique()));
            fs_create(&dir);
            let path = dir.join("xbox.svg");
            std::fs::write(&path, art::template_svg("xbox").expect("xbox template")).expect("write svg");
            let art = SvgArt::new(path);
            assert!(art.error.is_none(), "{:?}", art.error);
            ArtFile { art, dir }
        }
    }

    impl Drop for ArtFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn unique() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        N.fetch_add(1, Ordering::Relaxed)
    }

    fn fs_create(dir: &std::path::Path) {
        std::fs::create_dir_all(dir).expect("temp dir");
    }

    fn render(art: &SvgArt, pressed: &[u16], axes: &[(u16, i32)]) -> cairo::ImageSurface {
        let surf = cairo::ImageSurface::create(cairo::Format::ARgb32, W, H).expect("surface");
        let cr = cairo::Context::new(&surf).expect("context");
        art.render(
            &cr,
            W as f64,
            H as f64,
            &pressed.iter().copied().collect(),
            &axes.iter().copied().collect(),
            WHITE,
            RED,
            true,
        )
        .expect("render");
        drop(cr);
        surf.flush();
        surf
    }

    /// RGB at template user coordinates. Cairo ARGB32 is stored B,G,R,A.
    fn pixel(surf: &cairo::ImageSurface, ux: f64, uy: f64) -> (u8, u8, u8, u8) {
        let scale = (W as f64 / 800.0).min(H as f64 / 520.0);
        let ox = (W as f64 - 800.0 * scale) / 2.0;
        let oy = (H as f64 - 520.0 * scale) / 2.0;
        let x = (ox + ux * scale) as i32;
        let y = (oy + uy * scale) as i32;
        let stride = surf.stride();
        let mut px = (0, 0, 0, 0);
        surf.with_data(|data| {
            let i = (y * stride + x * 4) as usize;
            px = (data[i + 2], data[i + 1], data[i], data[i + 3]);
        })
        .expect("surface data");
        px
    }

    fn is_red((r, g, b, a): (u8, u8, u8, u8)) -> bool {
        a > 150 && r > 150 && g < 90 && b < 90
    }

    #[test]
    fn pressed_button_filled() {
        let fix = ArtFile::xbox();
        // btn-south centre in the xbox template: face (576, 200) + (0, 46); sample beside the label
        assert!(!is_red(pixel(&render(&fix.art, &[], &[]), 576.0 - 14.0, 246.0)));
        assert!(is_red(pixel(&render(&fix.art, &[BTN_A], &[]), 576.0 - 14.0, 246.0)));
    }

    #[test]
    fn trigger_fill_is_partial_and_bottom_up() {
        let fix = ArtFile::xbox();
        // rt rect: x 534..634, y 4..48; half pulled -> bottom half red, top half not
        let surf = render(&fix.art, &[], &[(ABS_RZ, 128)]);
        assert!(is_red(pixel(&surf, 545.0, 44.0)));
        assert!(!is_red(pixel(&surf, 545.0, 8.0)));
    }

    #[test]
    fn knob_moves_within_ring() {
        let fix = ArtFile::xbox();
        // ls ring centre (250, 200) r48, knob r24 -> travel 24 user units; full right
        let surf = render(&fix.art, &[BTN_THUMBL], &[(ABS_X, 32767)]);
        assert!(is_red(pixel(&surf, 250.0 + 24.0, 200.0)));
        assert!(!is_red(pixel(&surf, 250.0 - 20.0, 200.0)));
        assert!(!is_red(pixel(&surf, 250.0 + 24.0 + 28.0, 200.0)));
    }
}
