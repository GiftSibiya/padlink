//! Doctor tab: system, device and Lutris checks, with copyable fixes.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use adw::gtk;
use adw::prelude::*;
use padlink_core::discovery::Controller;
use padlink_core::doctor::{self, Check};
use padlink_core::logger::Log;
use serde_json::{Map, Value};

use super::Page;
use crate::util::{Toast, copy_button, row, run_bg, status_icon};

pub struct DoctorPage {
    me: Weak<DoctorPage>,
    root: gtk::Box,
    run_btn: gtk::Button,
    page: adw::PreferencesPage,
    groups: RefCell<Vec<adw::PreferencesGroup>>,
    checks: RefCell<Vec<Check>>,
    ctrl: RefCell<Option<Controller>>,
    toast: Toast,
}

impl DoctorPage {
    pub fn new(toast: Toast) -> Rc<Self> {
        let this = Rc::new_cyclic(|me| DoctorPage {
            me: me.clone(),
            root: gtk::Box::new(gtk::Orientation::Vertical, 0),
            run_btn: gtk::Button::with_label("Run checks"),
            page: adw::PreferencesPage::builder().vexpand(true).build(),
            groups: RefCell::default(),
            checks: RefCell::default(),
            ctrl: RefCell::default(),
            toast,
        });
        this.run_btn.add_css_class("suggested-action");
        let bar = gtk::Box::builder().spacing(8).margin_top(12).margin_start(12).margin_end(12).build();
        let save = gtk::Button::with_label("Save report to log");
        bar.append(&this.run_btn);
        bar.append(&save);
        this.root.append(&bar);
        this.root.append(&this.page);
        let t = Rc::clone(&this);
        this.run_btn.connect_clicked(move |_| t.run());
        let t = Rc::clone(&this);
        save.connect_clicked(move |_| t.save());
        this
    }

    fn run(&self) {
        let Some(this) = self.me.upgrade() else { return };
        self.run_btn.set_sensitive(false);
        let ctrl = self.ctrl.borrow().clone();
        let all = ctrl.is_none();
        run_bg(move || doctor::run(ctrl.as_ref(), all), move |checks| this.show(checks));
    }

    fn show(&self, checks: Vec<Check>) {
        self.run_btn.set_sensitive(true);
        for g in self.groups.borrow_mut().drain(..) {
            self.page.remove(&g);
        }
        let mut by_scope: Vec<(String, adw::PreferencesGroup)> = Vec::new();
        for ch in &checks {
            let g = if let Some((_, g)) = by_scope.iter().find(|(s, _)| s == &ch.scope) {
                g.clone()
            } else {
                let g = adw::PreferencesGroup::builder().title(&ch.scope).build();
                self.page.add(&g);
                self.groups.borrow_mut().push(g.clone());
                by_scope.push((ch.scope.clone(), g.clone()));
                g
            };
            let detail = if ch.fix.is_empty() { ch.detail.clone() } else { format!("{}\nfix: {}", ch.detail, ch.fix) };
            let r = row(&ch.title, &detail);
            r.add_prefix(&status_icon(ch.status));
            if !ch.fix.is_empty() {
                r.add_suffix(&copy_button(ch.fix.clone(), self.toast.clone()));
            }
            g.add(&r);
        }
        *self.checks.borrow_mut() = checks;
    }

    fn save(&self) {
        let label = self.ctrl.borrow().as_ref().map(|c| c.id_str()).unwrap_or_else(|| "system".into());
        let log = match Log::create(&label, "doctor", Map::new(), None) {
            Ok(log) => log,
            Err(e) => {
                (self.toast)(&format!("Cannot save: {e}"));
                return;
            }
        };
        let checks: Vec<Value> = self.checks.borrow().iter().map(Check::to_value).collect();
        let mut fields = Map::new();
        fields.insert("checks".into(), Value::Array(checks));
        log.write("diag", fields);
        let name =
            log.path.as_ref().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_default();
        log.close();
        (self.toast)(&format!("Saved {name}"));
    }
}

impl Page for DoctorPage {
    fn name(&self) -> &'static str {
        "DoctorPage"
    }
    fn title(&self) -> &'static str {
        "Doctor"
    }
    fn icon(&self) -> &'static str {
        "dialog-information-symbolic"
    }
    fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    fn set_controller(&self, ctrl: Option<&Controller>) {
        *self.ctrl.borrow_mut() = ctrl.cloned();
        self.run();
    }
}
