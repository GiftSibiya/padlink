//! Probe tab: send candidate rumble reports and save the one that vibrates.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::{Rc, Weak};

use adw::gtk;
use adw::prelude::*;
use padlink_core::discovery::Controller;
use padlink_core::hex::hexstr;
use padlink_core::logger::{self, Log};
use padlink_core::prober::{self, Candidate, Kind, Motor, Prober};
use padlink_core::{log_write, profiles};
use serde_json::{Map, Value};

use super::Page;
use crate::util::{Toast, run_bg};

#[derive(Clone, Copy)]
enum Act {
    Start,
    Yes,
    No,
    Repeat,
    Cancel,
    Strong,
    Weak,
    Both,
    Skip,
    Save,
    Test,
}

struct State {
    ctrl: Option<Controller>,
    prober: Option<Prober>,
    log: Option<Log>,
    stage: u8,
    cands: Vec<Candidate>,
    idx: usize,
    hit: Option<Candidate>,
    motors: BTreeMap<usize, Motor>,
    final_cand: Option<Candidate>,
}

pub struct ProbePage {
    me: Weak<ProbePage>,
    root: gtk::Box,
    status: adw::StatusPage,
    hex: gtk::Label,
    buttons: gtk::Box,
    state: RefCell<State>,
    /// Ignores a pulse that returns after the user cancelled.
    generation: Cell<u64>,
    toast: Toast,
}

impl ProbePage {
    pub fn new(toast: Toast) -> Rc<Self> {
        let this = Rc::new_cyclic(|me| ProbePage {
            me: me.clone(),
            root: gtk::Box::new(gtk::Orientation::Vertical, 0),
            status: adw::StatusPage::builder()
                .icon_name("system-search-symbolic")
                .title("Find the rumble report")
                .vexpand(true)
                .build(),
            hex: gtk::Label::builder().selectable(true).wrap(true).build(),
            buttons: gtk::Box::builder().spacing(8).halign(gtk::Align::Center).build(),
            state: RefCell::new(State {
                ctrl: None,
                prober: None,
                log: None,
                stage: 0,
                cands: Vec::new(),
                idx: 0,
                hit: None,
                motors: BTreeMap::new(),
                final_cand: None,
            }),
            generation: Cell::new(0),
            toast,
        });
        this.hex.add_css_class("mono-small");
        let inner = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).build();
        inner.append(&this.hex);
        inner.append(&this.buttons);
        this.status.set_child(Some(&inner));
        this.root.append(&this.status);
        this.reset();
        this
    }

    fn rc(&self) -> Rc<Self> {
        self.me.upgrade().expect("page still alive")
    }

    fn set_buttons(&self, specs: &[(&str, Act, Option<&str>)]) {
        while let Some(child) = self.buttons.first_child() {
            self.buttons.remove(&child);
        }
        for &(label, act, style) in specs {
            let b = gtk::Button::with_label(label);
            b.add_css_class("pill");
            if let Some(style) = style {
                b.add_css_class(style);
            }
            let this = self.rc();
            b.connect_clicked(move |_| this.on_act(act));
            self.buttons.append(&b);
        }
    }

    fn on_act(&self, act: Act) {
        match act {
            Act::Start => self.start(),
            Act::Yes => self.yes(),
            Act::No => self.no(),
            Act::Repeat => self.pulse(),
            Act::Cancel => self.reset(),
            Act::Strong => self.motor(Motor::Strong),
            Act::Weak => self.motor(Motor::Weak),
            Act::Both => self.motor(Motor::Both),
            Act::Skip => self.finish_stage2(),
            Act::Save => self.save(),
            Act::Test => self.test_final(),
        }
    }

    fn set_busy(&self, busy: bool) {
        let mut child = self.buttons.first_child();
        while let Some(c) = child {
            c.set_sensitive(!busy);
            child = c.next_sibling();
        }
    }

    fn reset(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
        {
            let mut st = self.state.borrow_mut();
            st.prober = None;
            if let Some(log) = st.log.take() {
                log.close();
            }
            st.stage = 0;
            st.cands.clear();
            st.idx = 0;
            st.hit = None;
            st.motors.clear();
            st.final_cand = None;
        }
        self.status.set_title("Find the rumble report");
        self.hex.set_text("");
        self.set_buttons(&[("Start probe", Act::Start, Some("suggested-action"))]);
    }

    fn start(&self) {
        let Some(ctrl) = self.state.borrow().ctrl.clone() else { return };
        let mut cands = prober::stage1_candidates(&ctrl, false);
        if cands.is_empty() {
            cands = prober::stage1_candidates(&ctrl, true);
            (self.toast)("No output reports declared; trying common report IDs blindly");
        }
        let mut extra = Map::new();
        extra.insert("device".into(), ctrl.to_value());
        let log = match Log::create(&ctrl.id_str(), "probe", extra, None) {
            Ok(log) => log,
            Err(e) => {
                (self.toast)(&format!("Cannot open log: {e}"));
                return;
            }
        };
        let prober = Prober::new(log.clone());
        {
            let mut st = self.state.borrow_mut();
            st.cands = cands;
            st.log = Some(log);
            st.prober = Some(prober);
            st.stage = 1;
            st.idx = 0;
        }
        self.pulse();
    }

    fn pulse(&self) {
        let (idx, len, stage) = {
            let st = self.state.borrow();
            (st.idx, st.cands.len(), st.stage)
        };
        if idx >= len {
            if stage == 1 {
                self.status.set_title("No candidate vibrated");
                self.status.set_description(Some(
                    "Check the controller mode (XInput mode may work natively), or capture the official app's USB traffic with `padlink capture`.",
                ));
                self.set_buttons(&[("Start over", Act::Cancel, None)]);
            } else {
                self.finish_stage2();
            }
            return;
        }
        let Some(cand) = self.state.borrow().cands.get(idx).cloned() else { return };
        self.status.set_title(&format!("Test {} of {len}", idx + 1));
        self.status.set_description(Some(&cand.label));
        self.hex.set_text(&hexstr(&cand.on_bytes()));
        if stage == 1 {
            self.set_buttons(&[
                ("It vibrated", Act::Yes, Some("suggested-action")),
                ("Nothing", Act::No, None),
                ("Repeat", Act::Repeat, None),
                ("Cancel", Act::Cancel, Some("destructive-action")),
            ]);
        } else {
            self.set_buttons(&[
                ("Left / heavy", Act::Strong, None),
                ("Right / light", Act::Weak, None),
                ("Both", Act::Both, None),
                ("Nothing", Act::No, None),
                ("Skip rest", Act::Skip, None),
            ]);
        }
        self.set_busy(true);
        let Some(mut prober) = self.state.borrow_mut().prober.take() else {
            self.set_busy(false);
            return;
        };
        let epoch = self.generation.get();
        let this = self.rc();
        run_bg(
            move || {
                let err = prober.pulse(&cand, 700);
                (prober, err)
            },
            move |(prober, err)| {
                if this.generation.get() != epoch {
                    return;
                }
                this.state.borrow_mut().prober = Some(prober);
                this.set_busy(false);
                if let Some(err) = err {
                    (this.toast)(&format!("Write failed: {err}"));
                }
            },
        );
    }

    fn yes(&self) {
        let Some(hit) = self.state.borrow().cands.get(self.state.borrow().idx).cloned() else { return };
        if let Some(log) = &self.state.borrow().log {
            log_write!(log, "note", {"text": "probe hit", "label": hit.label, "template": hit.template});
        }
        if matches!(hit.kind, Kind::AllFf) {
            let next = prober::stage2_byte_candidates(&hit);
            {
                let mut st = self.state.borrow_mut();
                st.hit = Some(hit);
                st.stage = 2;
                st.cands = next;
                st.idx = 0;
            }
            (self.toast)("Found it. Now: which byte drives which motor?");
            self.pulse();
        } else {
            self.state.borrow_mut().final_cand = Some(hit);
            self.offer_save();
        }
    }

    fn no(&self) {
        self.state.borrow_mut().idx += 1;
        self.pulse();
    }

    fn motor(&self, which: Motor) {
        let idx = self.state.borrow().idx;
        if let Some(cand) = self.state.borrow().cands.get(idx).cloned() {
            if let Kind::Byte(pos) = cand.kind {
                self.state.borrow_mut().motors.insert(pos, which);
            }
        }
        self.state.borrow_mut().idx += 1;
        self.pulse();
    }

    fn finish_stage2(&self) {
        let built = {
            let st = self.state.borrow();
            let Some(hit) = st.hit.clone() else { return };
            prober::build_from_bytes(&hit, &st.motors)
        };
        self.state.borrow_mut().final_cand = Some(built);
        self.offer_save();
    }

    fn offer_save(&self) {
        let Some(final_cand) = self.state.borrow().final_cand.clone() else { return };
        self.status.set_title("Rumble report found");
        let template = padlink_core::py::json_dumps(&Value::Array(final_cand.template.clone()), None);
        self.status.set_description(Some(&format!("{}\ntemplate: {template}", final_cand.label)));
        self.hex.set_text(&hexstr(&final_cand.on_bytes()));
        self.set_buttons(&[
            ("Save profile", Act::Save, Some("suggested-action")),
            ("Test again", Act::Test, None),
            ("Start over", Act::Cancel, None),
        ]);
    }

    fn test_final(&self) {
        let Some(cand) = self.state.borrow().final_cand.clone() else { return };
        self.set_busy(true);
        let Some(mut prober) = self.state.borrow_mut().prober.take() else {
            self.set_busy(false);
            return;
        };
        let epoch = self.generation.get();
        let this = self.rc();
        run_bg(
            move || {
                prober.pulse(&cand, 700);
                prober
            },
            move |prober| {
                if this.generation.get() != epoch {
                    return;
                }
                this.state.borrow_mut().prober = Some(prober);
                this.set_busy(false);
            },
        );
    }

    fn save(&self) {
        let (ctrl, final_cand, log) = {
            let st = self.state.borrow();
            (st.ctrl.clone(), st.final_cand.clone(), st.log.clone())
        };
        let (Some(ctrl), Some(final_cand)) = (ctrl, final_cand) else { return };
        let mut prof = match profiles::load_or_new(ctrl.vendor, ctrl.product, &ctrl.name) {
            Ok(p) => p,
            Err(e) => {
                (self.toast)(&format!("Cannot load profile: {e}"));
                return;
            }
        };
        prof.rumble = Some(final_cand.spec());
        let stamp = logger::local_date_minute(logger::now());
        let line = format!("probed {stamp} via {}", final_cand.label);
        prof.notes = match prof.notes.trim() {
            "" => line,
            notes => format!("{notes}\n{line}"),
        };
        let path = match prof.save(None) {
            Ok(p) => p,
            Err(e) => {
                (self.toast)(&format!("Cannot save profile: {e}"));
                return;
            }
        };
        if let Some(log) = &log {
            log_write!(log, "note", {"text": "profile saved", "path": path.display().to_string(), "profile": prof.to_value()});
        }
        (self.toast)(&format!("Saved {}", path.display()));
        let desc = format!("{}\nTest it in the Rumble tab, then start the Bridge.", path.display());
        self.reset();
        self.status.set_title("Profile saved");
        self.status.set_description(Some(&desc));
    }
}

impl Page for ProbePage {
    fn name(&self) -> &'static str {
        "ProbePage"
    }
    fn title(&self) -> &'static str {
        "Probe"
    }
    fn icon(&self) -> &'static str {
        "system-search-symbolic"
    }
    fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
    fn keep_on_refresh(&self) -> bool {
        self.state.borrow().prober.is_some()
    }
    fn shutdown(&self) {
        self.reset();
    }

    fn set_controller(&self, ctrl: Option<&Controller>) {
        self.reset();
        self.state.borrow_mut().ctrl = ctrl.cloned();
        let Some(ctrl) = ctrl else { return };
        let desc = if ctrl.has_ff() {
            "This controller already has kernel force feedback. Use the Rumble tab. (You can still probe its hidraw interface.)"
        } else if ctrl.hidraw.is_empty() {
            "No hidraw interface: nothing to probe."
        } else if !ctrl.hidraw.iter().any(|h| h.writable()) {
            "No write access to the hidraw node(s). Install the padlink udev rule (Doctor tab), then replug."
        } else {
            "padlink will send candidate output reports. Each one vibrates for ~0.7 s if it is right. Hold the controller and answer each question."
        };
        self.status.set_description(Some(desc));
    }
}
