//! Device tab: identity, rumble path, input nodes, HID descriptors.

use std::cell::RefCell;
use std::rc::Rc;

use adw::gtk;
use adw::prelude::*;
use padlink_core::discovery::Controller;
use padlink_core::hex::hexstr;
use padlink_core::{hiddesc, profiles, py};

use super::Page;
use crate::util::{Toast, copy_button, esc, row, status_icon};

pub struct OverviewPage {
    root: gtk::Box,
    page: adw::PreferencesPage,
    groups: RefCell<Vec<adw::PreferencesGroup>>,
    toast: Toast,
}

impl OverviewPage {
    pub fn new(toast: Toast) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let page = adw::PreferencesPage::builder().vexpand(true).build();
        root.append(&page);
        Rc::new(OverviewPage { root, page, groups: RefCell::default(), toast })
    }

    fn group(&self, title: &str, desc: &str) -> adw::PreferencesGroup {
        let g = adw::PreferencesGroup::builder().title(esc(title)).description(esc(desc)).build();
        self.page.add(&g);
        self.groups.borrow_mut().push(g.clone());
        g
    }
}

impl Page for OverviewPage {
    fn name(&self) -> &'static str {
        "OverviewPage"
    }
    fn title(&self) -> &'static str {
        "Device"
    }
    fn icon(&self) -> &'static str {
        "input-gaming-symbolic"
    }
    fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    fn set_controller(&self, ctrl: Option<&Controller>) {
        for g in self.groups.borrow_mut().drain(..) {
            self.page.remove(&g);
        }
        let Some(ctrl) = ctrl else { return };

        let g = self.group(&ctrl.name, ctrl.known_as().unwrap_or(""));
        g.add(&row("ID", &format!("{}  (version 0x{:04x})", ctrl.id_str(), ctrl.version)));
        g.add(&row("Bus / driver", &format!("{} / {}", ctrl.bus(), ctrl.driver.as_deref().unwrap_or("unknown"))));
        if !ctrl.uniq.is_empty() {
            g.add(&row("Unique ID / MAC", &ctrl.uniq));
        }
        if !ctrl.phys.is_empty() {
            g.add(&row("Physical path", &ctrl.phys));
        }
        g.add(&row("sysfs", &ctrl.key));
        if let Some(b) = &ctrl.battery {
            let none = || "None".to_string();
            g.add(&row(
                "Battery",
                &format!("{}%  {}", b.capacity.clone().unwrap_or_else(none), b.status.clone().unwrap_or_else(none)),
            ));
        }

        let g = self.group("Rumble", "");
        let prof = profiles::load(ctrl.vendor, ctrl.product, None).ok().flatten();
        let r = if let Some(node) = ctrl.ff_node() {
            let r = row("Kernel force feedback", &node.ff_names().join(", "));
            r.add_prefix(&status_icon("ok"));
            r
        } else if let Some((p, spec)) = prof.as_ref().and_then(|p| p.rumble.as_ref().map(|s| (p, s))) {
            let report = py::repr(&serde_json::Value::Array(spec.report.clone()));
            let r = row("Rumble via padlink profile", &format!("{}: {report}", p.path().display()));
            r.add_prefix(&status_icon("ok"));
            r
        } else {
            let r = row(
                "No rumble path yet",
                "The kernel exposes no force feedback for this device. Use the Probe tab to find its rumble report, \
                 then run the Bridge.",
            );
            r.add_prefix(&status_icon("warn"));
            r
        };
        g.add(&r);

        let g = self.group("Input nodes", "");
        for n in &ctrl.events {
            let count = |k: &str| n.caps.get(k).map_or(0, |s| s.len());
            let ff = n.ff_names().join(", ");
            let r = row(
                &format!("{}  ·  {}", n.path, n.role()),
                &format!(
                    "{}  ·  {} buttons, {} axes  ·  FF: {}",
                    n.name,
                    count("key"),
                    count("abs"),
                    if ff.is_empty() { "none" } else { &ff }
                ),
            );
            r.add_suffix(&status_icon(if n.readable() && n.writable() { "ok" } else { "warn" }));
            g.add(&r);
        }
        for j in &ctrl.joysticks {
            g.add(&row(j, "legacy joystick API"));
        }

        if !ctrl.hidraw.is_empty() {
            let g = self.group("HID", "Raw HID interfaces and their report descriptors");
            for h in &ctrl.hidraw {
                let yn = |b: bool| if b { "yes" } else { "no" };
                let exp = adw::ExpanderRow::builder()
                    .title(esc(&format!("{}  ·  interface {}", h.path, h.interface.as_deref().unwrap_or("-"))))
                    .subtitle(format!("read {} · write {}", yn(h.readable()), yn(h.writable())))
                    .build();
                let text = match h.descriptor() {
                    Ok(raw) => format!(
                        "{}\n\nraw ({} bytes):\n{}",
                        hiddesc::parse(&raw).summary().join("\n"),
                        raw.len(),
                        hexstr(&raw)
                    ),
                    Err(e) => format!("descriptor unavailable: {e}"),
                };
                let lbl = gtk::Label::builder()
                    .label(&text)
                    .xalign(0.0)
                    .selectable(true)
                    .wrap(true)
                    .margin_start(12)
                    .margin_end(12)
                    .margin_top(8)
                    .margin_bottom(8)
                    .build();
                lbl.add_css_class("mono-small");
                exp.add_row(&lbl);
                exp.add_suffix(&copy_button(text, self.toast.clone()));
                g.add(&exp);
            }
        }
    }
}
