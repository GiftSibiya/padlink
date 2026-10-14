//! Rumble tab: fire a test effect through the kernel or the hidraw profile.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::thread;
use std::time::Duration;

use adw::gtk;
use adw::prelude::*;
use padlink_core::bridge::pick_hidraw;
use padlink_core::discovery::Controller;
use padlink_core::logger::Log;
use padlink_core::sink::{HidrawSink, RumbleSink, evdev_rumble};
use padlink_core::{log_write, profiles, py};
use serde_json::Map;

use super::Page;
use crate::util::{Toast, row, run_bg, set_sub};

pub struct RumblePage {
    root: gtk::Box,
    path_row: adw::ActionRow,
    strong: adw::SpinRow,
    weak: adw::SpinRow,
    ms: adw::SpinRow,
    go: gtk::Button,
    pat: gtk::Button,
    ctrl: RefCell<Option<Controller>>,
    toast: Toast,
}

fn spin(title: &str, lo: f64, hi: f64, step: f64, value: f64) -> adw::SpinRow {
    let r = adw::SpinRow::with_range(lo, hi, step);
    r.set_title(title);
    r.set_value(value);
    r
}

impl RumblePage {
    pub fn new(toast: Toast) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let page = adw::PreferencesPage::builder().vexpand(true).build();
        root.append(&page);
        let g = adw::PreferencesGroup::builder().title("Rumble test").build();
        page.add(&g);
        let path_row = row("Path", "");
        g.add(&path_row);
        let strong = spin("Strong / left motor %", 0.0, 100.0, 5.0, 100.0);
        let weak = spin("Weak / right motor %", 0.0, 100.0, 5.0, 100.0);
        let ms = spin("Duration (ms)", 50.0, 5000.0, 50.0, 800.0);
        for r in [&strong, &weak, &ms] {
            g.add(r);
        }
        let g2 = adw::PreferencesGroup::new();
        page.add(&g2);
        let bx = gtk::Box::builder().spacing(8).halign(gtk::Align::Center).build();
        let go = gtk::Button::with_label("Rumble");
        go.add_css_class("suggested-action");
        go.add_css_class("pill");
        let pat = gtk::Button::with_label("Left → right → both");
        pat.add_css_class("pill");
        bx.append(&go);
        bx.append(&pat);
        g2.add(&bx);
        let this = Rc::new(RumblePage { root, path_row, strong, weak, ms, go, pat, ctrl: RefCell::default(), toast });
        let t = Rc::clone(&this);
        this.go.connect_clicked(move |_| t.fire(false));
        let t = Rc::clone(&this);
        this.pat.connect_clicked(move |_| t.fire(true));
        this
    }

    fn fire(self: &Rc<Self>, pattern: bool) {
        let Some(ctrl) = self.ctrl.borrow().clone() else { return };
        let pct = |v: f64| py::round(v / 100.0 * 65535.0).clamp(0, 0xFFFF) as u16;
        let (s, w, ms) = (pct(self.strong.value()), pct(self.weak.value()), self.ms.value() as u16);
        let seq: Vec<(u16, u16, u16)> =
            if pattern { vec![(0xFFFF, 0, 500), (0, 0xFFFF, 500), (0xFFFF, 0xFFFF, 500)] } else { vec![(s, w, ms)] };
        self.go.set_sensitive(false);
        let this = Rc::clone(self);
        run_bg(
            move || -> Result<(), String> {
                let mut extra = Map::new();
                extra.insert("device".into(), ctrl.to_value());
                let log = Log::create(&ctrl.id_str(), "rumble", extra, None).map_err(|e| e.to_string())?;
                let result = (|| -> Result<(), String> {
                    if let Some(node) = ctrl.ff_node() {
                        for &(a, b, d) in &seq {
                            log_write!(log, "note", {"text": "evdev rumble", "strong": a, "weak": b, "ms": d});
                            evdev_rumble(Path::new(&node.path), a, b, d).map_err(|e| e.to_string())?;
                            thread::sleep(Duration::from_millis(150));
                        }
                        return Ok(());
                    }
                    let prof = profiles::load(ctrl.vendor, ctrl.product, None).map_err(|e| e.to_string())?;
                    let spec = prof.and_then(|p| p.rumble).ok_or("no rumble profile")?;
                    let path = pick_hidraw(&ctrl, &spec).ok_or("no hidraw node")?.to_string();
                    let mut sink = HidrawSink::open(Path::new(&path), spec, log.clone()).map_err(|e| e.to_string())?;
                    let run = (|| -> std::io::Result<()> {
                        for &(a, b, d) in &seq {
                            sink.set(a, b)?;
                            thread::sleep(Duration::from_millis(d as u64));
                            sink.set(0, 0)?;
                            thread::sleep(Duration::from_millis(150));
                        }
                        Ok(())
                    })();
                    sink.close();
                    run.map_err(|e| e.to_string())
                })();
                log.close();
                result
            },
            move |result| {
                this.go.set_sensitive(true);
                if let Err(e) = result {
                    (this.toast)(&format!("Rumble failed: {e}"));
                }
            },
        );
    }
}

impl Page for RumblePage {
    fn name(&self) -> &'static str {
        "RumblePage"
    }
    fn title(&self) -> &'static str {
        "Rumble"
    }
    fn icon(&self) -> &'static str {
        "audio-volume-high-symbolic"
    }
    fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    fn set_controller(&self, ctrl: Option<&Controller>) {
        *self.ctrl.borrow_mut() = ctrl.cloned();
        let Some(ctrl) = ctrl else { return };
        let prof = profiles::load(ctrl.vendor, ctrl.product, None).ok().flatten();
        let has_profile = prof.as_ref().is_some_and(|p| p.rumble.is_some());
        if let Some(node) = ctrl.ff_node() {
            set_sub(&self.path_row, &format!("kernel FF on {}", node.path));
        } else if let Some(p) = prof.as_ref().filter(|_| has_profile) {
            set_sub(&self.path_row, &format!("hidraw via profile {}", p.path().display()));
        } else {
            set_sub(&self.path_row, "none: run the Probe first");
        }
        let sensitive = ctrl.has_ff() || has_profile;
        self.go.set_sensitive(sensitive);
        self.pat.set_sensitive(sensitive);
    }
}
