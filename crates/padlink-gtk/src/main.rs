//! padlink GTK4/libadwaita app.

mod padview;
mod pages;
mod svgart;
mod util;

use std::cell::{Cell, RefCell};
use std::os::fd::AsRawFd;
use std::rc::Rc;
use std::time::Duration;

use adw::gtk::{self, gdk, glib};
use adw::prelude::*;
use padlink_core::discovery::{self, Controller};

use crate::pages::Page;
use crate::util::{CSS, Toast};

const APP_ID: &str = "io.github.GiftSibiya.padlink";

struct Window {
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    split: adw::NavigationSplitView,
    listbox: gtk::ListBox,
    show_all: gtk::ToggleButton,
    content_stack: gtk::Stack,
    pages: Vec<Rc<dyn Page>>,
    controllers: RefCell<Vec<Controller>>,
    row_keys: RefCell<Vec<String>>,
    selected_key: RefCell<Option<String>>,
    rescan_pending: Cell<bool>,
    /// Keeps the udev socket open for the lifetime of the window.
    _monitor: RefCell<Option<udev::MonitorSocket>>,
}

impl Window {
    fn new(app: &adw::Application) -> Rc<Self> {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("padlink")
            .default_width(1100)
            .default_height(760)
            .build();
        let toasts = adw::ToastOverlay::new();
        let split = adw::NavigationSplitView::builder().min_sidebar_width(240.0).max_sidebar_width(320.0).build();
        toasts.set_child(Some(&split));
        window.set_content(Some(&toasts));

        // sidebar
        let side = adw::ToolbarView::new();
        let sh = adw::HeaderBar::new();
        sh.set_title_widget(Some(&adw::WindowTitle::new("padlink", "controllers")));
        let refresh = gtk::Button::builder().icon_name("view-refresh-symbolic").tooltip_text("Rescan").build();
        sh.pack_start(&refresh);
        let show_all = gtk::ToggleButton::builder()
            .icon_name("view-list-symbolic")
            .tooltip_text("Show every HID input device, not only gamepads")
            .build();
        sh.pack_end(&show_all);
        side.add_top_bar(&sh);
        let listbox = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).build();
        listbox.add_css_class("navigation-sidebar");
        side.set_content(Some(&gtk::ScrolledWindow::builder().vexpand(true).child(&listbox).build()));
        split.set_sidebar(Some(&adw::NavigationPage::new(&side, "Controllers")));

        // content
        let toasts_ref = toasts.clone();
        let toast: Toast = Rc::new(move |text: &str| {
            let t = adw::Toast::builder().title(util::esc(text)).timeout(4).build();
            toasts_ref.add_toast(t);
        });
        let stack = adw::ViewStack::builder().vexpand(true).build();
        let pages = pages::all(&toast);
        for p in &pages {
            stack.add_titled_with_icon(&p.widget(), Some(p.name()), p.title(), p.icon());
        }
        let content = adw::ToolbarView::new();
        let ch = adw::HeaderBar::new();
        let switcher = adw::ViewSwitcher::builder().stack(&stack).policy(adw::ViewSwitcherPolicy::Wide).build();
        ch.set_title_widget(Some(&switcher));
        content.add_top_bar(&ch);
        let empty = adw::StatusPage::builder()
            .icon_name("input-gaming-symbolic")
            .title("No controller selected")
            .description("Plug in the dongle / switch the controller on. The list updates automatically.")
            .build();
        let content_stack = gtk::Stack::new();
        content_stack.add_named(&empty, Some("empty"));
        content_stack.add_named(&stack, Some("pages"));
        content.set_content(Some(&content_stack));
        let bottom = adw::ViewSwitcherBar::builder().stack(&stack).build();
        content.add_bottom_bar(&bottom);
        split.set_content(Some(&adw::NavigationPage::new(&content, "Controller")));

        let bp = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 760sp").expect("valid condition"));
        bp.add_setter(&split, "collapsed", Some(&true.to_value()));
        bp.add_setter(&bottom, "reveal", Some(&true.to_value()));
        window.add_breakpoint(bp);

        let this = Rc::new(Window {
            window,
            toasts,
            split,
            listbox,
            show_all,
            content_stack,
            pages,
            controllers: RefCell::default(),
            row_keys: RefCell::default(),
            selected_key: RefCell::default(),
            rescan_pending: Cell::new(false),
            _monitor: RefCell::default(),
        });

        let t = Rc::clone(&this);
        refresh.connect_clicked(move |_| t.rescan());
        let t = Rc::clone(&this);
        this.show_all.connect_toggled(move |_| t.rescan());
        let t = Rc::clone(&this);
        this.listbox.connect_row_selected(move |_, r| {
            if let Some(r) = r {
                t.on_select(r.index(), false);
            }
        });
        this.watch_hotplug();
        let t = Rc::clone(&this);
        this.window.connect_close_request(move |_| {
            for p in &t.pages {
                p.shutdown();
            }
            glib::Propagation::Proceed
        });
        this.rescan();
        this
    }

    fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::builder().title(util::esc(text)).timeout(4).build());
    }

    /// Rescan on input/hidraw/power_supply hotplug, debounced.
    fn watch_hotplug(self: &Rc<Self>) {
        let socket = udev::MonitorBuilder::new()
            .and_then(|b| b.match_subsystem("input"))
            .and_then(|b| b.match_subsystem("hidraw"))
            .and_then(|b| b.match_subsystem("power_supply"))
            .and_then(|b| b.listen());
        let socket = match socket {
            Ok(s) => s,
            Err(e) => {
                self.toast(&format!("hotplug disabled: {e}"));
                return;
            }
        };
        let fd = socket.as_raw_fd();
        *self._monitor.borrow_mut() = Some(socket);
        let weak = Rc::downgrade(self);
        glib_unix::unix_fd_add_local(fd, glib::IOCondition::IN, move |_, _| {
            let Some(t) = weak.upgrade() else { return glib::ControlFlow::Break };
            if let Some(sock) = t._monitor.borrow().as_ref() {
                for _ in sock.iter() {}
            }
            if !t.rescan_pending.replace(true) {
                let t2 = Rc::clone(&t);
                glib::timeout_add_local_once(Duration::from_millis(400), move || {
                    t2.rescan_pending.set(false);
                    t2.rescan();
                });
            }
            glib::ControlFlow::Continue
        });
    }

    fn rescan(&self) {
        let ctrls = match discovery::scan(self.show_all.is_active()) {
            Ok(c) => c,
            Err(e) => {
                self.toast(&format!("scan failed: {e}"));
                return;
            }
        };
        let keep = self.selected_key.borrow().clone();
        self.listbox.remove_all();
        let mut keys = Vec::new();
        let mut sel_index = None;
        for c in &ctrls {
            let bx = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(2)
                .margin_top(6)
                .margin_bottom(6)
                .build();
            let title =
                gtk::Label::builder().label(&c.name).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
            title.add_css_class("heading");
            let rumble = if c.is_padlink() {
                "padlink virtual pad"
            } else if c.has_ff() {
                "rumble ✓"
            } else {
                "no kernel rumble"
            };
            let sub = gtk::Label::builder()
                .label(format!("{} · {} · {rumble}", c.id_str(), c.bus()))
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build();
            sub.add_css_class("dim-label");
            sub.add_css_class("caption");
            bx.append(&title);
            bx.append(&sub);
            self.listbox.append(&gtk::ListBoxRow::builder().child(&bx).build());
            if Some(&c.key) == keep.as_ref() {
                sel_index = Some(keys.len());
            }
            keys.push(c.key.clone());
        }
        if sel_index.is_none() && !ctrls.is_empty() {
            sel_index = Some(ctrls.iter().position(|c| !c.is_padlink()).unwrap_or(0));
        }
        *self.controllers.borrow_mut() = ctrls;
        *self.row_keys.borrow_mut() = keys;
        match sel_index.and_then(|i| self.listbox.row_at_index(i as i32)) {
            Some(r) => {
                self.listbox.select_row(Some(&r));
                // re-deliver updated controller data (permissions/battery may have changed)
                self.on_select(r.index(), true);
            }
            None => {
                *self.selected_key.borrow_mut() = None;
                self.content_stack.set_visible_child_name("empty");
                for p in &self.pages {
                    if !p.busy() {
                        p.set_controller(None);
                    }
                }
            }
        }
    }

    fn on_select(&self, index: i32, force: bool) {
        let Some(key) = self.row_keys.borrow().get(index as usize).cloned() else { return };
        let Some(ctrl) = self.controllers.borrow().iter().find(|c| c.key == key).cloned() else { return };
        let changed = self.selected_key.borrow().as_ref() != Some(&key);
        if !force && !changed {
            return;
        }
        *self.selected_key.borrow_mut() = Some(key);
        self.content_stack.set_visible_child_name("pages");
        for p in &self.pages {
            if p.busy() && changed {
                continue; // keep a running bridge attached to its controller
            }
            if !changed && p.keep_on_refresh() {
                continue; // don't interrupt running sessions on mere hotplug refreshes
            }
            p.set_controller(Some(&ctrl));
        }
        self.split.set_show_content(true);
    }
}

fn main() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    let window: Rc<RefCell<Option<Rc<Window>>>> = Rc::default();
    app.connect_activate(move |app| {
        if let Some(w) = window.borrow().as_ref() {
            w.window.present();
            return;
        }
        let provider = gtk::CssProvider::new();
        provider.load_from_string(CSS);
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        }
        let w = Window::new(app);
        w.window.present();
        *window.borrow_mut() = Some(w);
    });
    app.run()
}
