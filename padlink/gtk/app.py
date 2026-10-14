"""padlink GTK4/libadwaita app."""

from __future__ import annotations

import sys

from padlink.core import discovery
from padlink.gtk.pages import PAGES
from padlink.gtk.util import CSS, Adw, Gdk, GLib, Gtk

APP_ID = "io.github.GiftSibiya.padlink"


class Window(Adw.ApplicationWindow):
    def __init__(self, app):
        super().__init__(application=app, title="padlink", default_width=1100, default_height=760)
        self.controllers: list[discovery.Controller] = []
        self.selected_key: str | None = None
        self._rescan_pending = 0

        self.toasts = Adw.ToastOverlay()
        split = Adw.NavigationSplitView(min_sidebar_width=240, max_sidebar_width=320)
        self.toasts.set_child(split)
        self.set_content(self.toasts)

        # sidebar
        side = Adw.ToolbarView()
        sh = Adw.HeaderBar()
        sh.set_title_widget(Adw.WindowTitle(title="padlink", subtitle="controllers"))
        refresh = Gtk.Button(icon_name="view-refresh-symbolic", tooltip_text="Rescan")
        refresh.connect("clicked", lambda _b: self.rescan())
        sh.pack_start(refresh)
        self.show_all = Gtk.ToggleButton(icon_name="view-list-symbolic",
                                         tooltip_text="Show every HID input device, not only gamepads")
        self.show_all.connect("toggled", lambda _b: self.rescan())
        sh.pack_end(self.show_all)
        side.add_top_bar(sh)
        self.listbox = Gtk.ListBox(selection_mode=Gtk.SelectionMode.SINGLE)
        self.listbox.add_css_class("navigation-sidebar")
        self.listbox.connect("row-selected", self.on_select)
        sw = Gtk.ScrolledWindow(vexpand=True)
        sw.set_child(self.listbox)
        side.set_content(sw)
        split.set_sidebar(Adw.NavigationPage(title="Controllers", child=side))

        # content
        self.stack = Adw.ViewStack(vexpand=True)
        self.pages = []
        for cls in PAGES:
            page = cls(self.toast)
            self.pages.append(page)
            self.stack.add_titled_with_icon(page, cls.__name__, cls.title, cls.icon)
        content = Adw.ToolbarView()
        ch = Adw.HeaderBar()
        self.switcher = Adw.ViewSwitcher(stack=self.stack, policy=Adw.ViewSwitcherPolicy.WIDE)
        ch.set_title_widget(self.switcher)
        content.add_top_bar(ch)
        self.empty = Adw.StatusPage(icon_name="input-gaming-symbolic", title="No controller selected",
                                    description="Plug in the dongle / switch the controller on. "
                                                "The list updates automatically.")
        self.content_stack = Gtk.Stack()
        self.content_stack.add_named(self.empty, "empty")
        self.content_stack.add_named(self.stack, "pages")
        content.set_content(self.content_stack)
        bottom = Adw.ViewSwitcherBar(stack=self.stack)
        content.add_bottom_bar(bottom)
        split.set_content(Adw.NavigationPage(title="Controller", child=content))

        bp = Adw.Breakpoint.new(Adw.BreakpointCondition.parse("max-width: 760sp"))
        bp.add_setter(split, "collapsed", True)
        bp.add_setter(bottom, "reveal", True)
        self.add_breakpoint(bp)
        self.split = split

        # hotplug
        self.monitor = discovery.udev_monitor()
        self.monitor.start()
        GLib.io_add_watch(self.monitor.fileno(), GLib.PRIORITY_DEFAULT, GLib.IO_IN, self.on_udev)
        self.connect("close-request", self.on_close)
        self.rescan()

    def toast(self, text: str) -> None:
        self.toasts.add_toast(Adw.Toast(title=GLib.markup_escape_text(text), timeout=4))

    def on_udev(self, *_):
        while self.monitor.poll(timeout=0) is not None:
            pass
        if not self._rescan_pending:
            self._rescan_pending = GLib.timeout_add(400, self._do_rescan)
        return True

    def _do_rescan(self):
        self._rescan_pending = 0
        self.rescan()
        return False

    def rescan(self):
        try:
            self.controllers = discovery.scan(include_all=self.show_all.get_active())
        except Exception as exc:  # noqa: BLE001
            self.toast(f"scan failed: {exc}")
            return
        keep = self.selected_key
        self.listbox.remove_all()
        sel_row = None
        for c in self.controllers:
            box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=2, margin_top=6, margin_bottom=6)
            title = Gtk.Label(label=c.name, xalign=0, ellipsize=3)
            title.add_css_class("heading")
            rumble = "rumble ✓" if c.has_ff else "no kernel rumble"
            if c.is_padlink:
                rumble = "padlink virtual pad"
            sub = Gtk.Label(label=f"{c.id_str} · {c.bus} · {rumble}", xalign=0, ellipsize=3)
            sub.add_css_class("dim-label")
            sub.add_css_class("caption")
            box.append(title)
            box.append(sub)
            r = Gtk.ListBoxRow(child=box)
            r.ctrl_key = c.key
            self.listbox.append(r)
            if c.key == keep:
                sel_row = r
        if sel_row is None and self.controllers:
            real = [i for i, c in enumerate(self.controllers) if not c.is_padlink]
            sel_row = self.listbox.get_row_at_index(real[0] if real else 0)
        if sel_row is not None:
            self.listbox.select_row(sel_row)
            # re-deliver updated controller data (permissions/battery may have changed)
            self.on_select(self.listbox, sel_row, force=True)
        else:
            self.selected_key = None
            self.content_stack.set_visible_child_name("empty")
            for p in self.pages:
                if p.__class__.__name__ == "BridgePage" and getattr(p, "bridge", None) and p.bridge.alive:
                    continue
                p.set_controller(None)

    def on_select(self, _lb, r, force: bool = False):
        if r is None:
            return
        ctrl = next((c for c in self.controllers if c.key == r.ctrl_key), None)
        if ctrl is None:
            return
        if not force and ctrl.key == self.selected_key:
            return
        changed = ctrl.key != self.selected_key
        self.selected_key = ctrl.key
        self.content_stack.set_visible_child_name("pages")
        for p in self.pages:
            busy = getattr(p, "bridge", None) is not None and p.bridge.alive
            if busy and changed:
                continue  # keep a running bridge attached to its controller
            if not changed and p.__class__.__name__ in ("LivePage", "ProbePage", "DoctorPage", "LogsPage"):
                continue  # don't interrupt running sessions on mere hotplug refreshes
            p.set_controller(ctrl)
        self.split.set_show_content(True)

    def on_close(self, *_):
        for p in self.pages:
            try:
                p.shutdown()
            except Exception:  # noqa: BLE001
                pass
        return False


class App(Adw.Application):
    def __init__(self):
        super().__init__(application_id=APP_ID)

    def do_activate(self):
        win = self.props.active_window
        if not win:
            provider = Gtk.CssProvider()
            provider.load_from_data(CSS)
            Gtk.StyleContext.add_provider_for_display(Gdk.Display.get_default(), provider,
                                                      Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)
            win = Window(self)
        win.present()


def main() -> int:
    return App().run(sys.argv)


if __name__ == "__main__":
    raise SystemExit(main())
