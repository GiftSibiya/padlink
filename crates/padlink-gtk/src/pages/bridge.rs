//! Bridge tab: start the in-process rumble bridge and apply Lutris env vars.

use std::cell::{Cell, RefCell};
use std::io::Read;
use std::path::Path;
use std::rc::{Rc, Weak};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use adw::gtk::{self, glib};
use adw::prelude::*;
use padlink_core::bridge::{Bridge, BridgeHandle, BridgeOptions};
use padlink_core::discovery::Controller;
use padlink_core::logger::{self, Log};
use padlink_core::{doctor, gamesetup, integration, personas, settings};
use serde_json::{Map, Value, json};

use super::Page;
use crate::util::{Toast, append_text, copy_button, esc, mono_view, row, set_sub, status_icon};

pub struct BridgePage {
    root: gtk::Box,
    mode: adw::ComboRow,
    mode_ids: Vec<String>,
    grab: adw::SwitchRow,
    logsw: adw::SwitchRow,
    state_row: adw::ActionRow,
    toggle_btn: gtk::Button,
    lutris: adw::PreferencesGroup,
    lutris_rows: RefCell<Vec<gtk::Widget>>,
    me: RefCell<Weak<BridgePage>>,
    view: gtk::TextView,
    ctrl: RefCell<Option<Controller>>,
    handle: RefCell<Option<BridgeHandle>>,
    log: RefCell<Log>,
    traffic_tx: Sender<String>,
    traffic_rx: RefCell<Receiver<String>>,
    poll: RefCell<Option<glib::SourceId>>,
    toast: Toast,
    applying_mode: Cell<bool>,
}

impl BridgePage {
    pub fn new(toast: Toast) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let page = adw::PreferencesPage::new();
        root.append(&page);
        let g = adw::PreferencesGroup::builder()
            .title("Rumble bridge")
            .description(
                "Creates a virtual controller with force feedback. Games see it, and their rumble is forwarded to this controller. It can impersonate other controllers for games that expect one.",
            )
            .build();
        page.add(&g);

        let mut mode_ids: Vec<String> = personas::PERSONAS.iter().map(|p| p.id.to_string()).collect();
        let mut labels: Vec<&str> = personas::PERSONAS.iter().map(|p| p.label).collect();
        mode_ids.push("mirror".into());
        labels.push("Mirror real controller");
        let mode = adw::ComboRow::builder().title("Appear to games as").model(&gtk::StringList::new(&labels)).build();
        let saved = settings::get("bridge_persona").and_then(|v| v.as_str().map(str::to_string));
        let selected = saved.as_deref().and_then(|s| mode_ids.iter().position(|id| id == s)).unwrap_or(0);
        mode.set_selected(selected as u32);

        let grab = adw::SwitchRow::builder()
            .title("Grab physical device")
            .subtitle("so games only get input via the virtual pad")
            .active(true)
            .build();
        let logsw = adw::SwitchRow::builder().title("Write JSONL log").active(true).build();
        let state_row = row("Status", "stopped");
        let toggle_btn = gtk::Button::builder().label("Start").valign(gtk::Align::Center).build();
        toggle_btn.add_css_class("suggested-action");
        state_row.add_suffix(&toggle_btn);
        for r in [
            &mode.clone().upcast::<gtk::Widget>(),
            &grab.clone().upcast(),
            &logsw.clone().upcast(),
            &state_row.clone().upcast(),
        ] {
            g.add(r);
        }

        let lutris = adw::PreferencesGroup::builder()
            .title("Lutris")
            .description("Hide the physical controller from games so they pick the virtual pad.")
            .build();
        page.add(&lutris);

        let g3 = adw::PreferencesGroup::builder().title("Autostart").build();
        page.add(&g3);
        let svc = row("systemd user service", "Run the bridge in the background whenever the controller is present");
        let install = gtk::Button::builder().label("Install").valign(gtk::Align::Center).build();
        svc.add_suffix(&install);
        g3.add(&svc);

        let heading = gtk::Label::builder()
            .label("Force-feedback traffic from games")
            .xalign(0.0)
            .margin_start(12)
            .margin_top(6)
            .build();
        heading.add_css_class("heading");
        root.append(&heading);
        let (sw, view) = mono_view(160);
        sw.set_margin_start(12);
        sw.set_margin_end(12);
        sw.set_margin_bottom(12);
        root.append(&sw);

        let (traffic_tx, traffic_rx) = mpsc::channel();
        let this = Rc::new(BridgePage {
            root,
            mode,
            mode_ids,
            grab,
            logsw,
            state_row,
            toggle_btn,
            lutris,
            lutris_rows: RefCell::default(),
            me: RefCell::default(),
            view,
            ctrl: RefCell::default(),
            handle: RefCell::default(),
            log: RefCell::new(Log::null()),
            traffic_tx,
            traffic_rx: RefCell::new(traffic_rx),
            poll: RefCell::default(),
            toast,
            applying_mode: Cell::new(false),
        });
        *this.me.borrow_mut() = Rc::downgrade(&this);
        this.apply_mode(false);
        let t = Rc::clone(&this);
        this.mode.connect_selected_notify(move |_| t.apply_mode(true));
        let t = Rc::clone(&this);
        this.toggle_btn.connect_clicked(move |_| t.toggle());
        let t = Rc::clone(&this);
        install.connect_clicked(move |_| t.install_service());
        let t = Rc::clone(&this);
        let poll = glib::timeout_add_local(Duration::from_millis(200), move || {
            t.poll();
            glib::ControlFlow::Continue
        });
        *this.poll.borrow_mut() = Some(poll);
        this
    }

    fn mode_id(&self) -> String {
        let i = self.mode.selected() as usize;
        self.mode_ids.get(i).cloned().unwrap_or_else(|| personas::DEFAULT.to_string())
    }

    fn apply_mode(&self, save: bool) {
        if self.applying_mode.get() {
            return;
        }
        let mid = self.mode_id();
        let sub = if mid == "mirror" {
            "Same buttons, axes and IDs as the real controller, plus rumble support".to_string()
        } else {
            match personas::get(&mid) {
                Ok(p) => {
                    let mut sub = format!("{}. Shows up as “{}” [{}]", p.description, p.name, p.id_str());
                    if let Some(ctrl) = self.ctrl.borrow().as_ref() {
                        if (p.vendor, p.product) == (ctrl.vendor, ctrl.product) {
                            sub.push_str(
                                ". ⚠ Same IDs as the real controller, so hiding it from games would hide this too",
                            );
                        }
                    }
                    sub
                }
                Err(e) => e,
            }
        };
        self.mode.set_subtitle(&crate::util::esc(&sub));
        if save {
            settings::set("bridge_persona", json!(mid));
        }
    }

    fn alive(&self) -> bool {
        self.handle.borrow().as_ref().is_some_and(BridgeHandle::alive)
    }

    fn toggle(&self) {
        if self.alive() {
            self.stop();
        } else {
            self.start();
        }
    }

    fn start(&self) {
        let Some(ctrl) = self.ctrl.borrow().clone() else { return };
        let want_file = self.logsw.is_active();
        let log = if want_file {
            let mut extra = Map::new();
            extra.insert("device".into(), ctrl.to_value());
            match Log::create(&ctrl.id_str(), "bridge", extra, None) {
                Ok(log) => log,
                Err(e) => {
                    (self.toast)(&format!("Cannot open log: {e}"));
                    return;
                }
            }
        } else {
            Log::null()
        };
        let tx = self.traffic_tx.clone();
        let t0 = logger::now();
        log.add_listener(Box::new(move |rec: &Value| {
            let kind = rec.get("kind").and_then(Value::as_str).unwrap_or("");
            if matches!(kind, "ff_upload" | "ff_play" | "ff_erase" | "ff_gain" | "note" | "rumble_out") {
                if let Some(obj) = rec.as_object() {
                    let _ = tx.send(logger::format_record(obj, Some(t0)));
                }
            }
        }));
        let opts = BridgeOptions { mode: self.mode_id(), grab: self.grab.is_active(), ..BridgeOptions::default() };
        let bridge = match Bridge::new(ctrl, log.clone(), opts) {
            Ok(b) => b,
            Err(e) => {
                (self.toast)(&format!("Bridge failed to start: {e}"));
                return;
            }
        };
        *self.log.borrow_mut() = log;
        *self.handle.borrow_mut() = Some(bridge.spawn());
        self.mode.set_sensitive(false);
        self.toggle_btn.set_label("Stop");
        self.toggle_btn.remove_css_class("suggested-action");
        self.toggle_btn.add_css_class("destructive-action");
        set_sub(&self.state_row, "starting…");
    }

    fn stop(&self) {
        if let Some(mut handle) = self.handle.borrow_mut().take() {
            handle.stop();
        }
        self.log.borrow().close();
        *self.log.borrow_mut() = Log::null();
        self.mode.set_sensitive(true);
        self.toggle_btn.set_label("Start");
        self.toggle_btn.remove_css_class("destructive-action");
        self.toggle_btn.add_css_class("suggested-action");
    }

    fn poll(&self) {
        while let Ok(line) = self.traffic_rx.borrow().try_recv() {
            append_text(&self.view, &line);
        }
        let (alive, error, virtual_path, stats) = {
            let handle = self.handle.borrow();
            let Some(handle) = handle.as_ref() else { return };
            let status = handle.status.lock().expect("status");
            (handle.alive(), status.error.clone(), status.virtual_path.clone(), status.stats)
        };
        if alive {
            let log_name =
                self.log.borrow().path.as_ref().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
            let mut text = format!(
                "running · virtual pad {} · uploads {} · plays {} · inputs {}",
                virtual_path.as_deref().unwrap_or("?"),
                stats.uploads,
                stats.plays,
                stats.inputs
            );
            if let Some(name) = log_name {
                text.push_str(&format!(" · log {name}"));
            }
            set_sub(&self.state_row, &text);
        } else {
            let text = match error {
                Some(err) => format!("stopped: {err}"),
                None => "stopped".to_string(),
            };
            set_sub(&self.state_row, &text);
            if self.toggle_btn.label().as_deref() == Some("Stop") {
                self.stop();
            }
        }
    }

    fn install_service(&self) {
        let Some(ctrl) = self.ctrl.borrow().clone() else { return };
        let exe = match cli_binary() {
            Ok(exe) => exe,
            Err(e) => {
                (self.toast)(&format!("Failed: {e}"));
                return;
            }
        };
        match integration::install_service(&exe, &ctrl.id_str(), &self.mode_id()) {
            Ok(path) => {
                (self.toast)(&format!("Wrote {}. Enable: systemctl --user enable --now padlink-bridge", path.display()))
            }
            Err(e) => (self.toast)(&format!("Failed: {e}")),
        }
    }

    /// The Lutris group: the env vars to copy, then each game with what it still needs.
    fn fill_lutris(&self, ctrl: &Controller) {
        for r in self.lutris_rows.borrow_mut().drain(..) {
            self.lutris.remove(&r);
        }
        let env = integration::lutris_env(ctrl);
        let text = env.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ");
        let r = row("Environment variables", &text);
        r.add_suffix(&copy_button(text, self.toast.clone()));
        self.lutris.add(&r);
        self.lutris_rows.borrow_mut().push(r.upcast());
        let dir = integration::lutris_games_dir();
        for (slug, name) in integration::lutris_games(&dir) {
            let row = self.game_row(ctrl, &dir, &slug, &name);
            self.lutris.add(&row);
            self.lutris_rows.borrow_mut().push(row.upcast());
        }
    }

    /// One game: ready or what to fix, a Set up button, and the DirectInput switch.
    fn game_row(&self, ctrl: &Controller, dir: &Path, slug: &str, name: &str) -> adw::ExpanderRow {
        let cfg = integration::find_game_config(dir, slug).ok().and_then(|p| integration::read_game_config(&p).ok());
        let problems = cfg.as_ref().map(|c| gamesetup::env_problems(c, ctrl)).unwrap_or_default();
        let prefix = cfg.as_ref().and_then(gamesetup::game_prefix);
        let mapped = prefix.as_deref().and_then(gamesetup::map_controllers);
        let summary = if problems.is_empty() {
            "Ready: the game only sees the virtual pad".to_string()
        } else {
            format!("{} thing{} to fix", problems.len(), if problems.len() == 1 { "" } else { "s" })
        };
        let exp = adw::ExpanderRow::builder().title(esc(name)).subtitle(esc(&summary)).build();
        exp.add_prefix(&status_icon(if problems.is_empty() { "ok" } else { "warn" }));
        for p in &problems {
            exp.add_row(&row("", p));
        }

        let setup = gtk::Button::builder()
            .label("Set up")
            .valign(gtk::Align::Center)
            .sensitive(!problems.is_empty())
            .tooltip_text(
                "Hide this controller from the game (SDL and Proton) and fix env vars Lutris ignores. Backups are kept",
            )
            .build();
        setup.add_css_class("suggested-action");
        let (me, ctrl2, slug2) = (self.me.borrow().clone(), ctrl.clone(), slug.to_string());
        setup.connect_clicked(move |_| {
            let Some(t) = me.upgrade() else { return };
            match gamesetup::setup(&integration::lutris_games_dir(), &slug2, &ctrl2, None) {
                Ok(out) if out.changes.is_empty() => (t.toast)("Nothing to change"),
                Ok(out) => (t.toast)(&format!("Set up: {}", out.changes.join("; "))),
                Err(e) => (t.toast)(&format!("Failed: {e}")),
            }
            t.fill_lutris(&ctrl2);
        });
        exp.add_suffix(&setup);

        let dinput = adw::SwitchRow::builder()
            .title("DirectInput game")
            .subtitle(
                "Older games (e.g. NFS Most Wanted 2005) use DirectInput: Wine only gives them rumble with its XInput \
                 mapping off. Close the game first",
            )
            .active(mapped == Some(false))
            .sensitive(mapped.is_some())
            .build();
        if mapped.is_none() {
            dinput.set_subtitle("Run the game once so its Wine prefix exists");
        }
        let (me, ctrl2) = (self.me.borrow().clone(), ctrl.clone());
        dinput.connect_active_notify(move |sw| {
            let Some(t) = me.upgrade() else { return };
            let Some(prefix) = prefix.as_deref() else { return };
            let want_on = !sw.is_active();
            if gamesetup::map_controllers(prefix) == Some(want_on) {
                return; // already there (e.g. after a refresh)
            }
            match gamesetup::set_map_controllers(prefix, want_on) {
                Ok(_) => (t.toast)(if want_on {
                    "Wine's XInput mapping is back on for this game"
                } else {
                    "DirectInput rumble enabled: start the game again"
                }),
                Err(e) => (t.toast)(&format!("Failed: {e}")),
            }
            t.fill_lutris(&ctrl2);
        });
        exp.add_row(&dinput);
        exp
    }
}

fn cli_binary() -> Result<String, String> {
    let elf = |path: &Path| -> bool {
        let mut f = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(_) => return false,
        };
        let mut magic = [0u8; 4];
        f.read_exact(&mut magic).is_ok() && magic == [0x7f, b'E', b'L', b'F']
    };
    if let Some(path) = doctor::which("padlink") {
        if elf(&path) {
            return Ok(path.display().to_string());
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("padlink");
            if sibling.is_file() && elf(&sibling) {
                return Ok(sibling.display().to_string());
            }
        }
    }
    Err("padlink binary not found; run ./install.sh first".into())
}

impl Page for BridgePage {
    fn name(&self) -> &'static str {
        "BridgePage"
    }
    fn title(&self) -> &'static str {
        "Bridge"
    }
    fn icon(&self) -> &'static str {
        "network-transmit-receive-symbolic"
    }
    fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
    fn busy(&self) -> bool {
        self.alive()
    }
    fn keep_on_refresh(&self) -> bool {
        self.alive()
    }

    fn set_controller(&self, ctrl: Option<&Controller>) {
        *self.ctrl.borrow_mut() = ctrl.cloned();
        self.apply_mode(false);
        for r in self.lutris_rows.borrow_mut().drain(..) {
            self.lutris.remove(&r);
        }
        let running = self.alive();
        let can_start = ctrl.is_some_and(|c| running || !c.is_padlink());
        self.toggle_btn.set_sensitive(can_start);
        if let Some(ctrl) = ctrl.filter(|c| !c.is_padlink()) {
            self.fill_lutris(ctrl);
        }
    }

    fn shutdown(&self) {
        if self.alive() {
            self.stop();
        }
        if let Some(id) = self.poll.borrow_mut().take() {
            id.remove();
        }
    }
}
