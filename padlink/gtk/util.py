"""Small GTK helpers shared by the pages."""

from __future__ import annotations

import threading

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
from gi.repository import Adw, Gdk, Gio, GLib, Gtk  # noqa: E402

STATUS_ICONS = {
    "ok": ("emblem-ok-symbolic", "success"),
    "info": ("dialog-information-symbolic", "accent"),
    "warn": ("dialog-warning-symbolic", "warning"),
    "fail": ("dialog-error-symbolic", "error"),
}

CSS = b"""
.pad-btn { padding: 4px 8px; border-radius: 6px; background: alpha(currentColor, 0.08); font-size: 0.9em; }
.pad-btn.pressed { background: @accent_bg_color; color: @accent_fg_color; }
.mono-small { font-family: monospace; font-size: 0.85em; }
.big-status { font-size: 1.2em; font-weight: bold; }
"""


def run_bg(fn, on_done=None, on_error=None):
    """Run fn() in a thread; deliver result/error on the GTK main loop."""

    def worker():
        try:
            result = fn()
        except Exception as exc:  # noqa: BLE001 - surfaced to the UI
            if on_error:
                GLib.idle_add(on_error, exc)
            return
        if on_done:
            GLib.idle_add(on_done, result)

    threading.Thread(target=worker, daemon=True).start()


def status_icon(status: str) -> Gtk.Image:
    name, css = STATUS_ICONS.get(status, STATUS_ICONS["info"])
    img = Gtk.Image.new_from_icon_name(name)
    img.add_css_class(css)
    return img


def copy_button(widget_for_clipboard: Gtk.Widget, text_fn, toast_fn=None) -> Gtk.Button:
    btn = Gtk.Button(icon_name="edit-copy-symbolic", valign=Gtk.Align.CENTER, tooltip_text="Copy")
    btn.add_css_class("flat")

    def on_click(_b):
        widget_for_clipboard.get_clipboard().set(text_fn())
        if toast_fn:
            toast_fn("Copied to clipboard")

    btn.connect("clicked", on_click)
    return btn


def mono_view(height: int = 240) -> tuple[Gtk.ScrolledWindow, Gtk.TextView]:
    view = Gtk.TextView(editable=False, monospace=True, wrap_mode=Gtk.WrapMode.WORD_CHAR,
                        top_margin=6, bottom_margin=6, left_margin=8, right_margin=8)
    sw = Gtk.ScrolledWindow(min_content_height=height, vexpand=True)
    sw.set_child(view)
    sw.add_css_class("card")
    return sw, view


def append_text(view: Gtk.TextView, text: str, max_lines: int = 2000) -> None:
    buf = view.get_buffer()
    buf.insert(buf.get_end_iter(), text + "\n")
    extra = buf.get_line_count() - max_lines
    if extra > 0:
        buf.delete(buf.get_start_iter(), buf.get_iter_at_line(extra)[1])
    mark = buf.create_mark(None, buf.get_end_iter(), False)
    view.scroll_to_mark(mark, 0, False, 0, 0)
    buf.delete_mark(mark)


def clear_group(page: Adw.PreferencesPage, groups: list) -> None:
    for g in groups:
        page.remove(g)
    groups.clear()


def esc(text) -> str:
    return GLib.markup_escape_text(str(text))
