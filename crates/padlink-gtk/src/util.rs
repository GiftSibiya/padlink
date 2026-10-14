//! Small GTK helpers shared by the pages.

use std::rc::Rc;

use adw::gtk::{self, glib};
use adw::prelude::*;

pub const CSS: &str = "
.pad-btn { padding: 4px 8px; border-radius: 6px; background: alpha(currentColor, 0.08); font-size: 0.9em; }
.pad-btn.pressed { background: @accent_bg_color; color: @accent_fg_color; }
.mono-small { font-family: monospace; font-size: 0.85em; }
.big-status { font-size: 1.2em; font-weight: bold; }
";

/// Shows a toast in the window.
pub type Toast = Rc<dyn Fn(&str)>;

pub fn esc(text: &str) -> String {
    glib::markup_escape_text(text).to_string()
}

pub fn row(title: &str, subtitle: &str) -> adw::ActionRow {
    let r = adw::ActionRow::builder().title(esc(title)).subtitle(esc(subtitle)).build();
    r.set_subtitle_selectable(true);
    r
}

pub fn set_sub(r: &adw::ActionRow, text: &str) {
    r.set_subtitle(&esc(text));
}

pub fn status_icon(status: &str) -> gtk::Image {
    let (name, css) = match status {
        "ok" => ("emblem-ok-symbolic", "success"),
        "warn" => ("dialog-warning-symbolic", "warning"),
        "fail" => ("dialog-error-symbolic", "error"),
        _ => ("dialog-information-symbolic", "accent"),
    };
    let img = gtk::Image::from_icon_name(name);
    img.add_css_class(css);
    img
}

pub fn copy_button(text: String, toast: Toast) -> gtk::Button {
    let btn =
        gtk::Button::builder().icon_name("edit-copy-symbolic").valign(gtk::Align::Center).tooltip_text("Copy").build();
    btn.add_css_class("flat");
    btn.connect_clicked(move |b| {
        b.clipboard().set_text(&text);
        toast("Copied to clipboard");
    });
    btn
}

pub fn mono_view(height: i32) -> (gtk::ScrolledWindow, gtk::TextView) {
    let view = gtk::TextView::builder()
        .editable(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(6)
        .bottom_margin(6)
        .left_margin(8)
        .right_margin(8)
        .build();
    let sw = gtk::ScrolledWindow::builder().min_content_height(height).vexpand(true).child(&view).build();
    sw.add_css_class("card");
    (sw, view)
}

/// Append a line, keep at most `max_lines`, and scroll to the end.
pub fn append_text(view: &gtk::TextView, text: &str) {
    const MAX_LINES: i32 = 2000;
    let buf = view.buffer();
    buf.insert(&mut buf.end_iter(), &format!("{text}\n"));
    let extra = buf.line_count() - MAX_LINES;
    if extra > 0 {
        if let Some(mut end) = buf.iter_at_line(extra) {
            buf.delete(&mut buf.start_iter(), &mut end);
        }
    }
    let mark = buf.create_mark(None, &buf.end_iter(), false);
    view.scroll_to_mark(&mark, 0.0, false, 0.0, 0.0);
    buf.delete_mark(&mark);
}

/// Run `work` on a worker thread and hand its result to `done` on the GTK main loop.
pub fn run_bg<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static, done: impl FnOnce(T) + 'static) {
    glib::spawn_future_local(async move {
        if let Ok(result) = gtk::gio::spawn_blocking(work).await {
            done(result);
        }
    });
}
