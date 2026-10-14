//! Logs tab: browse JSONL session logs.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::gtk;
use adw::prelude::*;
use padlink_core::logger::{format_record, list_logs, read_log};
use padlink_core::paths;
use padlink_core::py;
use serde_json::Value;

use super::Page;
use crate::util::mono_view;

pub struct LogsPage {
    root: gtk::Box,
    listbox: gtk::ListBox,
    filter: gtk::SearchEntry,
    view: gtk::TextView,
    paths: RefCell<Vec<PathBuf>>,
}

impl LogsPage {
    pub fn new(_toast: crate::util::Toast) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let paned = gtk::Paned::builder().orientation(gtk::Orientation::Horizontal).vexpand(true).position(330).build();
        let left = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .margin_top(12)
            .margin_start(12)
            .margin_bottom(12)
            .build();
        let tools = gtk::Box::builder().spacing(6).build();
        let refresh = gtk::Button::builder().icon_name("view-refresh-symbolic").tooltip_text("Refresh").build();
        let folder = gtk::Button::builder().icon_name("folder-open-symbolic").tooltip_text("Open log folder").build();
        tools.append(&refresh);
        tools.append(&folder);
        left.append(&tools);
        let listbox = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).build();
        listbox.add_css_class("navigation-sidebar");
        let sw = gtk::ScrolledWindow::builder().vexpand(true).min_content_width(300).child(&listbox).build();
        left.append(&sw);
        paned.set_start_child(Some(&left));

        let right = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .margin_top(12)
            .margin_end(12)
            .margin_bottom(12)
            .margin_start(6)
            .build();
        let filter = gtk::SearchEntry::builder().placeholder_text("filter kinds, e.g. ff_upload,ff_play").build();
        right.append(&filter);
        let (sw2, view) = mono_view(300);
        right.append(&sw2);
        paned.set_end_child(Some(&right));
        root.append(&paned);

        let this = Rc::new(LogsPage { root, listbox, filter, view, paths: RefCell::default() });
        let t = Rc::clone(&this);
        refresh.connect_clicked(move |_| t.refresh());
        let t = Rc::clone(&this);
        folder.connect_clicked(move |b| t.open_folder(b));
        let t = Rc::clone(&this);
        this.listbox.connect_row_selected(move |_, row| t.show_selected(row));
        let t = Rc::clone(&this);
        this.filter.connect_search_changed(move |_| {
            let row = t.listbox.selected_row();
            t.show_selected(row.as_ref());
        });
        this.refresh();
        this
    }

    fn open_folder(&self, widget: &gtk::Button) {
        let dir = paths::log_dir();
        let _ = std::fs::create_dir_all(&dir);
        let file = gtk::gio::File::for_path(&dir);
        let parent = widget.root().and_then(|r| r.downcast::<gtk::Window>().ok());
        gtk::FileLauncher::new(Some(&file)).launch(parent.as_ref(), None::<&gtk::gio::Cancellable>, |_| {});
    }

    fn refresh(&self) {
        self.listbox.remove_all();
        let paths = list_logs(None);
        for p in &paths {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let lbl = gtk::Label::builder().label(&name).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
            lbl.set_tooltip_text(Some(&p.display().to_string()));
            self.listbox.append(&lbl);
        }
        *self.paths.borrow_mut() = paths;
    }

    fn show_selected(&self, row: Option<&gtk::ListBoxRow>) {
        let Some(row) = row else { return };
        let index = row.index();
        if index < 0 {
            return;
        }
        let Some(path) = self.paths.borrow().get(index as usize).cloned() else { return };
        let recs = read_log(&path).unwrap_or_default();
        let kinds: Vec<String> =
            self.filter.text().split(',').map(|k| k.trim().to_string()).filter(|k| !k.is_empty()).collect();
        let t0 = recs.first().and_then(|r| r.get("t")).and_then(Value::as_f64);
        let mut lines = Vec::new();
        for rec in recs.iter().take(20_000) {
            let kind = rec.get("kind").and_then(Value::as_str).unwrap_or("");
            if !kinds.is_empty() && !kinds.iter().any(|k| k == kind) {
                continue;
            }
            if kind == "session" {
                let text = py::json_dumps(&Value::Object(rec.clone()), Some(1));
                lines.push(text.chars().take(4000).collect::<String>());
            } else {
                lines.push(format_record(rec, t0));
            }
        }
        self.view.buffer().set_text(&lines.join("\n"));
    }
}

impl Page for LogsPage {
    fn name(&self) -> &'static str {
        "LogsPage"
    }
    fn title(&self) -> &'static str {
        "Logs"
    }
    fn icon(&self) -> &'static str {
        "document-open-recent-symbolic"
    }
    fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    fn set_controller(&self, _ctrl: Option<&padlink_core::discovery::Controller>) {
        self.refresh();
    }
}
