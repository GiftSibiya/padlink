//! Input tab: a live drawing (or raw evdev values) of the selected controller.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::os::fd::AsRawFd;
use std::rc::{Rc, Weak};
use std::time::SystemTime;

use adw::gtk::{self, glib};
use adw::prelude::*;
use evdev::raw_stream::RawDevice;
use padlink_core::codes::{self, AbsInfo, BTN_TL2, BTN_TR2, EV_ABS, EV_KEY};
use padlink_core::discovery::Controller;
use padlink_core::hex::hexstr;
use padlink_core::hidraw::{self, HidrawDevice};
use padlink_core::logger::Log;
use padlink_core::translate::{self, Translator};
use padlink_core::{personas, profiles, settings};
use serde_json::{Map, Value, json};

use super::Page;
use crate::padview::{self, PadView};
use crate::util::{Toast, append_text, mono_view};

enum OpenDev {
    Evdev { path: String, dev: RawDevice },
    Hid { path: String, dev: HidrawDevice },
}

pub struct LivePage {
    me: Weak<LivePage>,
    root: gtk::Box,
    start_btn: gtk::Button,
    raw_switch: gtk::Switch,
    log_switch: gtk::Switch,
    status: gtk::Label,
    pad: PadView,
    map_label: gtk::Label,
    buttons: gtk::FlowBox,
    axes_box: gtk::Grid,
    view: gtk::TextView,
    ctrl: RefCell<Option<Controller>>,
    translator: RefCell<Option<Translator>>,
    watches: RefCell<Vec<glib::SourceId>>,
    devs: RefCell<Vec<OpenDev>>,
    btn_widgets: RefCell<HashMap<u16, gtk::Label>>,
    axis_widgets: RefCell<HashMap<u16, (gtk::LevelBar, gtk::Label, AbsInfo)>>,
    last_raw: RefCell<HashMap<(String, u8), Vec<u8>>>,
    log: RefCell<Log>,
    toast: Toast,
    /// Bumped on stop so a watch that fires during teardown exits.
    generation: Cell<u64>,
}

impl LivePage {
    pub fn new(toast: Toast) -> Rc<Self> {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        let bar = gtk::Box::builder().spacing(8).build();
        let start_btn = gtk::Button::with_label("Start");
        start_btn.add_css_class("suggested-action");
        let raw_switch =
            gtk::Switch::builder().valign(gtk::Align::Center).tooltip_text("Also show raw HID reports").build();
        let log_switch = gtk::Switch::builder().valign(gtk::Align::Center).tooltip_text("Write a JSONL log").build();
        let status = gtk::Label::builder().xalign(0.0).hexpand(true).ellipsize(gtk::pango::EllipsizeMode::End).build();
        status.add_css_class("dim-label");
        bar.append(&start_btn);
        bar.append(&gtk::Label::new(Some("Raw HID")));
        bar.append(&raw_switch);
        bar.append(&gtk::Label::new(Some("Log to file")));
        bar.append(&log_switch);
        bar.append(&status);

        let views = gtk::Stack::builder().transition_type(gtk::StackTransitionType::Crossfade).build();
        // linked toggle buttons rather than adw::ToggleGroup, which needs libadwaita 1.7
        let view_toggle = gtk::Box::builder().valign(gtk::Align::Center).build();
        view_toggle.add_css_class("linked");
        let drawing_btn = gtk::ToggleButton::builder().label("Drawing").tooltip_text("Controller illustration").build();
        let raw_btn = gtk::ToggleButton::builder()
            .label("Raw values")
            .tooltip_text("Every evdev button and axis")
            .group(&drawing_btn)
            .build();
        view_toggle.append(&drawing_btn);
        view_toggle.append(&raw_btn);
        bar.append(&view_toggle);
        root.append(&bar);

        let drawing = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build();
        let pad = PadView::new();
        drawing.append(&pad.area);
        let map_label = gtk::Label::builder().xalign(0.5).wrap(true).build();
        map_label.add_css_class("dim-label");
        map_label.add_css_class("caption");
        drawing.append(&map_label);
        views.add_named(&drawing, Some("drawing"));

        let raw = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).build();
        let buttons = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .max_children_per_line(12)
            .column_spacing(6)
            .row_spacing(6)
            .valign(gtk::Align::Start)
            .build();
        raw.append(&buttons);
        let axes_box = gtk::Grid::builder().column_spacing(12).row_spacing(4).build();
        raw.append(&axes_box);
        views.add_named(&raw, Some("raw"));
        root.append(&views);

        let initial = match settings::get("input_view").as_ref().and_then(Value::as_str) {
            Some("raw") => "raw",
            _ => "drawing",
        };
        views.set_visible_child_name(initial);
        if initial == "raw" { &raw_btn } else { &drawing_btn }.set_active(true);
        for (btn, name) in [(&drawing_btn, "drawing"), (&raw_btn, "raw")] {
            let views2 = views.clone();
            btn.connect_toggled(move |b| {
                if b.is_active() {
                    views2.set_visible_child_name(name);
                    settings::set("input_view", json!(name));
                }
            });
        }

        let (sw, view) = mono_view(120);
        root.append(&sw);

        let this = Rc::new_cyclic(|me| LivePage {
            me: me.clone(),
            root,
            start_btn,
            raw_switch,
            log_switch,
            status,
            pad,
            map_label,
            buttons,
            axes_box,
            view,
            ctrl: RefCell::default(),
            translator: RefCell::default(),
            watches: RefCell::default(),
            devs: RefCell::default(),
            btn_widgets: RefCell::default(),
            axis_widgets: RefCell::default(),
            last_raw: RefCell::default(),
            log: RefCell::new(Log::null()),
            toast,
            generation: Cell::new(0),
        });
        let t = Rc::clone(&this);
        this.start_btn.connect_clicked(move |_| t.toggle());
        this
    }

    fn running(&self) -> bool {
        !self.watches.borrow().is_empty()
    }

    fn toggle(&self) {
        if self.running() {
            self.stop();
        } else {
            self.start();
        }
    }

    fn start(&self) {
        let Some(ctrl) = self.ctrl.borrow().clone() else { return };
        let Some(node) = ctrl.primary_event().map(|n| n.path.clone()) else { return };
        let dev = match RawDevice::open(&node) {
            Ok(d) => d,
            Err(e) => {
                (self.toast)(&format!("Cannot open {node}: {e}"));
                return;
            }
        };
        if let Err(e) = hidraw::set_nonblocking(dev.as_raw_fd()) {
            (self.toast)(&format!("Cannot watch {node}: {e}"));
            return;
        }
        let log = if self.log_switch.is_active() {
            let mut extra = Map::new();
            extra.insert("device".into(), ctrl.to_value());
            match Log::create(&ctrl.id_str(), "live", extra, None) {
                Ok(log) => log,
                Err(e) => {
                    (self.toast)(&format!("Cannot open log: {e}"));
                    return;
                }
            }
        } else {
            Log::null()
        };
        self.build_widgets(&dev);
        if let Err(e) = self.build_translator(&ctrl, &dev) {
            (self.toast)(&e);
            return;
        }
        if let Ok(held) = dev.get_key_state() {
            for key in held.iter() {
                let code = key.code();
                if let Some(lbl) = self.btn_widgets.borrow().get(&code) {
                    lbl.add_css_class("pressed");
                }
                if let Some(tr) = self.translator.borrow_mut().as_mut() {
                    for (t, c, v) in tr.translate(EV_KEY, code, 1) {
                        self.pad.apply(t, c, v);
                    }
                }
            }
        }
        let path = node.clone();
        self.devs.borrow_mut().push(OpenDev::Evdev { path: path.clone(), dev });
        self.me.upgrade().expect("page").watch_evdev(path);
        if self.raw_switch.is_active() {
            for h in &ctrl.hidraw {
                match HidrawDevice::open(&h.path, false) {
                    Ok(hd) => {
                        let path = h.path.clone();
                        self.devs.borrow_mut().push(OpenDev::Hid { path: path.clone(), dev: hd });
                        self.me.upgrade().expect("page").watch_hid(path);
                    }
                    Err(e) => append_text(&self.view, &format!("{}: {e}", h.path)),
                }
            }
        }
        self.start_btn.set_label("Stop");
        self.start_btn.remove_css_class("suggested-action");
        self.start_btn.add_css_class("destructive-action");
        if let Some(p) = &self.log_path_of(&log) {
            self.status.set_text(&format!("logging to {p}"));
        }
        *self.log.borrow_mut() = log;
        self.pad.queue_draw();
    }

    fn log_path_of(&self, log: &Log) -> Option<String> {
        log.path.as_ref().map(|p| p.display().to_string())
    }

    fn watch_evdev(self: &Rc<Self>, path: String) {
        let fd = {
            let devs = self.devs.borrow();
            let Some(OpenDev::Evdev { dev, .. }) =
                devs.iter().find(|d| matches!(d, OpenDev::Evdev { path: p, .. } if p == &path))
            else {
                return;
            };
            dev.as_raw_fd()
        };
        let epoch = self.generation.get();
        let this = Rc::clone(self);
        let id = glib_unix::unix_fd_add_local(
            fd,
            glib::IOCondition::IN | glib::IOCondition::HUP | glib::IOCondition::ERR,
            move |_, cond| {
                if this.generation.get() != epoch {
                    return glib::ControlFlow::Break;
                }
                if cond.contains(glib::IOCondition::HUP) || cond.contains(glib::IOCondition::ERR) {
                    append_text(&this.view, "device disconnected");
                    this.stop();
                    return glib::ControlFlow::Break;
                }
                let fetched = {
                    let mut devs = this.devs.borrow_mut();
                    let Some(OpenDev::Evdev { dev, .. }) =
                        devs.iter_mut().find(|d| matches!(d, OpenDev::Evdev { path: p, .. } if p == &path))
                    else {
                        return glib::ControlFlow::Break;
                    };
                    match dev.fetch_events() {
                        Ok(evs) => Ok(evs.collect::<Vec<_>>()),
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Err(false),
                        Err(e) => {
                            append_text(&this.view, &format!("read error: {e}"));
                            Err(true)
                        }
                    }
                };
                match fetched {
                    Ok(events) => this.on_events(&path, &events),
                    Err(false) => return glib::ControlFlow::Continue,
                    Err(true) => {
                        this.stop();
                        return glib::ControlFlow::Break;
                    }
                }
                glib::ControlFlow::Continue
            },
        );
        self.watches.borrow_mut().push(id);
    }

    fn watch_hid(self: &Rc<Self>, path: String) {
        let fd = {
            let devs = self.devs.borrow();
            let Some(OpenDev::Hid { dev, .. }) =
                devs.iter().find(|d| matches!(d, OpenDev::Hid { path: p, .. } if p == &path))
            else {
                return;
            };
            dev.as_raw_fd()
        };
        let epoch = self.generation.get();
        let this = Rc::clone(self);
        let id = glib_unix::unix_fd_add_local(
            fd,
            glib::IOCondition::IN | glib::IOCondition::HUP | glib::IOCondition::ERR,
            move |_, cond| {
                if this.generation.get() != epoch {
                    return glib::ControlFlow::Break;
                }
                if cond.contains(glib::IOCondition::HUP) || cond.contains(glib::IOCondition::ERR) {
                    return glib::ControlFlow::Break;
                }
                let data = {
                    let mut devs = this.devs.borrow_mut();
                    let Some(OpenDev::Hid { dev, .. }) =
                        devs.iter_mut().find(|d| matches!(d, OpenDev::Hid { path: p, .. } if p == &path))
                    else {
                        return glib::ControlFlow::Break;
                    };
                    dev.read(Some(std::time::Duration::ZERO)).unwrap_or_default()
                };
                if let Some(data) = data.filter(|d| !d.is_empty()) {
                    let key = (path.clone(), data[0]);
                    if this.last_raw.borrow().get(&key) != Some(&data) {
                        this.last_raw.borrow_mut().insert(key, data.clone());
                        let mut fields = Map::new();
                        fields.insert("node".into(), json!(path));
                        fields.insert("hex".into(), json!(hexstr(&data)));
                        this.log.borrow().write("hid_rx", fields);
                        append_text(&this.view, &format!("{path} [{}] {}", data.len(), hexstr(&data)));
                    }
                }
                glib::ControlFlow::Continue
            },
        );
        self.watches.borrow_mut().push(id);
    }

    fn on_events(&self, path: &str, events: &[evdev::InputEvent]) {
        for ev in events {
            let (t, code, value) = (ev.event_type().0, ev.code(), ev.value());
            if let Some(tr) = self.translator.borrow_mut().as_mut() {
                if t == EV_KEY || t == EV_ABS {
                    let mut out = tr.translate(t, code, value);
                    if out.is_empty() && t == EV_KEY && (code == BTN_TL2 || code == BTN_TR2) {
                        out = vec![(t, code, value)];
                    }
                    for (t, c, v) in out {
                        self.pad.apply(t, c, v);
                    }
                }
            }
            if t == EV_KEY {
                if let Some(lbl) = self.btn_widgets.borrow().get(&code) {
                    if value != 0 {
                        lbl.add_css_class("pressed");
                    } else {
                        lbl.remove_css_class("pressed");
                    }
                }
            } else if t == EV_ABS {
                self.set_axis(code, value);
            }
            if t == EV_KEY || t == EV_ABS {
                let name = codes::code_name(t, code);
                let mut fields = Map::new();
                fields.insert("node".into(), json!(path));
                fields.insert("type".into(), json!(t));
                fields.insert("code".into(), json!(code));
                fields.insert("value".into(), json!(value));
                fields.insert("name".into(), json!(name));
                self.log.borrow().write("input", fields);
                if t == EV_KEY {
                    let ts = ev.timestamp().duration_since(SystemTime::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
                    let word = if value != 0 { "press" } else { "release" };
                    append_text(&self.view, &format!("{ts:.3} {} {word}", codes::code_name(t, code)));
                }
            }
        }
        self.pad.queue_draw();
    }

    fn stop(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
        for id in self.watches.borrow_mut().drain(..) {
            id.remove();
        }
        self.devs.borrow_mut().clear();
        self.log.borrow().close();
        *self.log.borrow_mut() = Log::null();
        *self.translator.borrow_mut() = None;
        self.pad.reset();
        self.pad.set_active(false);
        self.start_btn.set_label("Start");
        self.start_btn.remove_css_class("destructive-action");
        self.start_btn.add_css_class("suggested-action");
    }

    fn build_widgets(&self, dev: &RawDevice) {
        while let Some(child) = self.buttons.first_child() {
            self.buttons.remove(&child);
        }
        while let Some(child) = self.axes_box.first_child() {
            self.axes_box.remove(&child);
        }
        self.btn_widgets.borrow_mut().clear();
        self.axis_widgets.borrow_mut().clear();
        let mut keys: Vec<u16> = dev.supported_keys().map(|s| s.iter().map(|k| k.code()).collect()).unwrap_or_default();
        keys.sort_unstable();
        for code in keys {
            let name = codes::code_name(EV_KEY, code);
            let label = name.strip_prefix("BTN_").unwrap_or(&name).to_string();
            let lbl = gtk::Label::new(Some(&label));
            lbl.add_css_class("pad-btn");
            self.buttons.append(&lbl);
            self.btn_widgets.borrow_mut().insert(code, lbl);
        }
        let mut axes = Vec::new();
        if let Ok(iter) = dev.get_absinfo() {
            for (code, info) in iter {
                axes.push((
                    code.0,
                    AbsInfo::new(
                        info.value(),
                        info.minimum(),
                        info.maximum(),
                        info.fuzz(),
                        info.flat(),
                        info.resolution(),
                    ),
                ));
            }
        }
        axes.sort_by_key(|(c, _)| *c);
        for (i, (code, info)) in axes.into_iter().enumerate() {
            let name = gtk::Label::builder().label(codes::code_name(EV_ABS, code)).xalign(0.0).build();
            let bar =
                gtk::LevelBar::builder().min_value(0.0).max_value(1.0).hexpand(true).valign(gtk::Align::Center).build();
            let val = gtk::Label::builder().label(info.value.to_string()).xalign(1.0).width_chars(7).build();
            val.add_css_class("numeric");
            let row = i as i32;
            self.axes_box.attach(&name, 0, row, 1, 1);
            self.axes_box.attach(&bar, 1, row, 1, 1);
            self.axes_box.attach(&val, 2, row, 1, 1);
            self.axis_widgets.borrow_mut().insert(code, (bar, val, info));
            self.set_axis(code, info.value);
        }
    }

    fn build_translator(&self, ctrl: &Controller, dev: &RawDevice) -> Result<(), String> {
        let keys: BTreeSet<u16> =
            dev.supported_keys().map(|s| s.iter().map(|k| k.code()).collect()).unwrap_or_default();
        let mut absinfo = HashMap::new();
        if let Ok(iter) = dev.get_absinfo() {
            for (code, info) in iter {
                absinfo.insert(
                    code.0,
                    AbsInfo::new(
                        info.value(),
                        info.minimum(),
                        info.maximum(),
                        info.fuzz(),
                        info.flat(),
                        info.resolution(),
                    ),
                );
            }
        }
        self.pad.reset();
        let (mapping, source) = if ctrl.is_padlink() {
            let driver = personas::from_phys(&ctrl.phys).filter(|p| p.spec_layout).map(|_| "playstation");
            (translate::heuristic_map(&keys, &absinfo.keys().copied().collect(), driver), "virtual pad")
        } else {
            mapping_for(ctrl, &keys, &absinfo.keys().copied().collect())
        };
        let mut translator = Translator::new(&mapping, &absinfo)?;
        for (&code, info) in &absinfo {
            for (t, c, v) in translator.translate(EV_ABS, code, info.value) {
                self.pad.apply(t, c, v);
            }
        }
        let unmapped: Vec<String> = keys
            .iter()
            .copied()
            .filter(|&k| k != BTN_TL2 && k != BTN_TR2 && translator.translate(EV_KEY, k, 0).is_empty())
            .map(|k| codes::code_name(EV_KEY, k))
            .collect();
        let hint = match source {
            "profile" => "from your profile".to_string(),
            "heuristic" => format!("guessed for driver {}", ctrl.driver.as_deref().unwrap_or("?")),
            _ => "padlink virtual pad".to_string(),
        };
        let mut text = format!("Drawing uses the bridge's input map ({hint}).");
        if !unmapped.is_empty() {
            text.push_str(&format!(" Not shown on the drawing: {}.", unmapped.join(", ")));
        }
        if source != "virtual pad" {
            text.push_str(&format!(" Wrong buttons? Fix it with: padlink map {}", ctrl.id_str()));
        }
        self.map_label.set_text(&format!("{text}\n{}", self.pad.art_status()));
        self.pad.set_active(true);
        *self.translator.borrow_mut() = Some(translator);
        Ok(())
    }

    fn set_axis(&self, code: u16, value: i32) {
        let axes = self.axis_widgets.borrow();
        let Some((bar, val, info)) = axes.get(&code) else { return };
        let span = (info.max - info.min).max(1) as f64;
        bar.set_value(((value - info.min) as f64 / span).clamp(0.0, 1.0));
        val.set_text(&value.to_string());
    }
}

fn mapping_for(ctrl: &Controller, keys: &BTreeSet<u16>, abses: &BTreeSet<u16>) -> (Value, &'static str) {
    if let Ok(Some(profile)) = profiles::load(ctrl.vendor, ctrl.product, None) {
        if let Some(map) = profile.input_map {
            return (map, "profile");
        }
    }
    (translate::heuristic_map(keys, abses, ctrl.driver.as_deref()), "heuristic")
}

impl Page for LivePage {
    fn name(&self) -> &'static str {
        "LivePage"
    }
    fn title(&self) -> &'static str {
        "Input"
    }
    fn icon(&self) -> &'static str {
        "media-record-symbolic"
    }
    fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
    fn keep_on_refresh(&self) -> bool {
        self.running()
    }

    fn set_controller(&self, ctrl: Option<&Controller>) {
        self.stop();
        let node = ctrl.and_then(|c| c.primary_event());
        // keyboards, mice and touchpads are root:input on purpose; say so instead of failing on Start
        match node {
            Some(n) if !n.readable() => {
                self.status.set_text(&format!(
                    "no read access to {} ({}); only game controllers are readable without root",
                    n.path,
                    n.role()
                ));
                self.start_btn.set_sensitive(false);
            }
            Some(n) => {
                self.status.set_text(&n.path);
                self.start_btn.set_sensitive(true);
            }
            None => {
                self.status.set_text("");
                self.start_btn.set_sensitive(false);
            }
        }
        match ctrl {
            Some(c) => self.pad.configure(padview::family_for(c.vendor), Some((c.vendor, c.product))),
            None => self.pad.configure(padview::Family::Xbox, None),
        }
        self.map_label.set_text("");
        *self.ctrl.borrow_mut() = ctrl.cloned();
    }

    fn shutdown(&self) {
        self.stop();
    }
}
