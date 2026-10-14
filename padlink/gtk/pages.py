"""The per-controller pages of the GTK app."""

from __future__ import annotations

import json
import time

from evdev import InputDevice, ecodes as E

from padlink.core import doctor, hiddesc, integration, profiles, prober
from padlink.core.bridge import Bridge, code_name
from padlink.core.discovery import Controller
from padlink.core.hidraw import HidrawDevice, hexstr
from padlink.core.logger import NullLog, SessionLog, format_record, list_logs, read_log
from padlink.core.paths import LOG_DIR
from padlink.gtk.util import (Adw, Gio, GLib, Gtk, append_text, clear_group, copy_button, esc, mono_view,
                              run_bg, status_icon)


def row(title: str, subtitle: str = "", **kw) -> Adw.ActionRow:
    r = Adw.ActionRow(title=esc(title), subtitle=esc(subtitle), **kw)
    r.set_subtitle_selectable(True)
    return r


def set_sub(r: Adw.ActionRow, text: str) -> None:
    r.set_subtitle(esc(text))


class Page(Gtk.Box):
    """Base: receives the selected controller and a toast callback."""

    title = "page"
    icon = "applications-games-symbolic"

    def __init__(self, toast):
        super().__init__(orientation=Gtk.Orientation.VERTICAL)
        self.toast = toast
        self.ctrl: Controller | None = None

    def set_controller(self, ctrl: Controller | None) -> None:
        self.ctrl = ctrl

    def shutdown(self) -> None:
        pass


# --------------------------------------------------------------------------- overview
class OverviewPage(Page):
    title, icon = "Device", "input-gaming-symbolic"

    def __init__(self, toast):
        super().__init__(toast)
        self.page = Adw.PreferencesPage(vexpand=True)
        self.append(self.page)
        self.groups: list = []

    def set_controller(self, ctrl):
        super().set_controller(ctrl)
        clear_group(self.page, self.groups)
        if ctrl is None:
            return

        def group(title, desc=""):
            g = Adw.PreferencesGroup(title=title, description=desc)
            self.page.add(g)
            self.groups.append(g)
            return g

        g = group(ctrl.name, ctrl.known_as or "")
        g.add(row("ID", f"{ctrl.id_str}  (version 0x{ctrl.version:04x})"))
        g.add(row("Bus / driver", f"{ctrl.bus} / {ctrl.driver or 'unknown'}"))
        if ctrl.uniq:
            g.add(row("Unique ID / MAC", ctrl.uniq))
        if ctrl.phys:
            g.add(row("Physical path", ctrl.phys))
        g.add(row("sysfs", ctrl.key))
        if ctrl.battery:
            g.add(row("Battery", f"{ctrl.battery.get('capacity')}%  {ctrl.battery.get('status')}"))

        g = group("Rumble")
        prof = profiles.load(ctrl.vendor, ctrl.product)
        if ctrl.has_ff:
            r = row("Kernel force feedback", ", ".join(ctrl.ff_node.ff_names))
            r.add_prefix(status_icon("ok"))
        elif prof and prof.rumble:
            r = row("Rumble via padlink profile", f"{prof.path}: {prof.rumble.report}")
            r.add_prefix(status_icon("ok"))
        else:
            r = row("No rumble path yet",
                    "The kernel exposes no force feedback for this device. Use the Probe tab to find its "
                    "rumble report, then run the Bridge.")
            r.add_prefix(status_icon("warn"))
        g.add(r)

        g = group("Input nodes")
        for n in ctrl.events:
            r = row(f"{n.path}  ·  {n.role}", f"{n.name}  ·  {len(n.caps.get('key', ()))} buttons, "
                    f"{len(n.caps.get('abs', ()))} axes  ·  FF: {', '.join(n.ff_names) or 'none'}")
            r.add_suffix(status_icon("ok" if n.readable and n.writable else "warn"))
            g.add(r)
        for j in ctrl.joysticks:
            g.add(row(j, "legacy joystick API"))

        if ctrl.hidraw:
            g = group("HID", "Raw HID interfaces and their report descriptors")
            for h in ctrl.hidraw:
                exp = Adw.ExpanderRow(title=esc(f"{h.path}  ·  interface {h.interface or '-'}"),
                                      subtitle=f"read {'yes' if h.readable else 'no'} · write "
                                               f"{'yes' if h.writable else 'no'}")
                try:
                    raw = h.descriptor()
                    text = "\n".join(hiddesc.parse(raw).summary()) + f"\n\nraw ({len(raw)} bytes):\n{raw.hex(' ')}"
                except OSError as exc:
                    text = f"descriptor unavailable: {exc}"
                lbl = Gtk.Label(label=text, xalign=0, selectable=True, wrap=True, margin_start=12, margin_end=12,
                                margin_top=8, margin_bottom=8)
                lbl.add_css_class("mono-small")
                exp.add_row(lbl)
                exp.add_suffix(copy_button(self, lambda t=text: t, self.toast))
                g.add(exp)


# --------------------------------------------------------------------------- live input
class LivePage(Page):
    title, icon = "Input", "media-record-symbolic"

    def __init__(self, toast):
        super().__init__(toast)
        self.set_spacing(12)
        for m in ("top", "bottom", "start", "end"):
            getattr(self, f"set_margin_{m}")(12)
        bar = Gtk.Box(spacing=8)
        self.start_btn = Gtk.Button(label="Start")
        self.start_btn.add_css_class("suggested-action")
        self.start_btn.connect("clicked", self.toggle)
        self.raw_switch = Gtk.Switch(valign=Gtk.Align.CENTER, tooltip_text="Also show raw HID reports")
        self.log_switch = Gtk.Switch(valign=Gtk.Align.CENTER, tooltip_text="Write a JSONL log")
        bar.append(self.start_btn)
        bar.append(Gtk.Label(label="Raw HID"))
        bar.append(self.raw_switch)
        bar.append(Gtk.Label(label="Log to file"))
        bar.append(self.log_switch)
        self.status = Gtk.Label(xalign=0, hexpand=True)
        self.status.add_css_class("dim-label")
        bar.append(self.status)
        self.append(bar)

        self.buttons = Gtk.FlowBox(selection_mode=Gtk.SelectionMode.NONE, max_children_per_line=12,
                                   column_spacing=6, row_spacing=6)
        self.append(self.buttons)
        self.axes_box = Gtk.Grid(column_spacing=12, row_spacing=4)
        self.append(self.axes_box)
        sw, self.view = mono_view(200)
        self.append(sw)
        self.watches: list[int] = []
        self.devs: list = []
        self.btn_widgets: dict[int, Gtk.Label] = {}
        self.axis_widgets: dict[int, tuple[Gtk.LevelBar, Gtk.Label, object]] = {}
        self.last_raw: dict = {}
        self.log = NullLog()

    def set_controller(self, ctrl):
        self.stop()
        super().set_controller(ctrl)
        self.status.set_text(f"{ctrl.primary_event.path}" if ctrl and ctrl.primary_event else "")

    def toggle(self, _b):
        if self.watches:
            self.stop()
        else:
            self.start()

    def start(self):
        ctrl = self.ctrl
        if not ctrl or not ctrl.primary_event:
            return
        try:
            dev = InputDevice(ctrl.primary_event.path)
        except OSError as exc:
            self.toast(f"Cannot open {ctrl.primary_event.path}: {exc}")
            return
        self.devs = [dev]
        self.log = SessionLog(ctrl.id_str, "live", {"device": ctrl.to_dict()}) if self.log_switch.get_active() \
            else NullLog()
        self._build_widgets(dev)
        self.watches.append(GLib.io_add_watch(dev.fd, GLib.PRIORITY_DEFAULT, GLib.IO_IN | GLib.IO_HUP | GLib.IO_ERR,
                                              self._on_evdev, dev))
        if self.raw_switch.get_active():
            for h in ctrl.hidraw:
                try:
                    hd = HidrawDevice(h.path)
                except OSError as exc:
                    append_text(self.view, f"{h.path}: {exc}")
                    continue
                self.devs.append(hd)
                self.watches.append(GLib.io_add_watch(hd.fd, GLib.PRIORITY_DEFAULT,
                                                      GLib.IO_IN | GLib.IO_HUP | GLib.IO_ERR, self._on_hid, hd))
        self.start_btn.set_label("Stop")
        self.start_btn.remove_css_class("suggested-action")
        self.start_btn.add_css_class("destructive-action")
        if self.log.path:
            self.status.set_text(f"logging to {self.log.path}")

    def stop(self):
        for w in self.watches:
            GLib.source_remove(w)
        self.watches.clear()
        for d in self.devs:
            d.close()
        self.devs.clear()
        self.log.close()
        self.log = NullLog()
        self.start_btn.set_label("Start")
        self.start_btn.remove_css_class("destructive-action")
        self.start_btn.add_css_class("suggested-action")

    def shutdown(self):
        self.stop()

    def _build_widgets(self, dev: InputDevice):
        while (child := self.buttons.get_first_child()) is not None:
            self.buttons.remove(child)
        while (child := self.axes_box.get_first_child()) is not None:
            self.axes_box.remove(child)
        self.btn_widgets.clear()
        self.axis_widgets.clear()
        caps = dev.capabilities(absinfo=True)
        for code in caps.get(E.EV_KEY, []):
            lbl = Gtk.Label(label=code_name(E.EV_KEY, code).removeprefix("BTN_"))
            lbl.add_css_class("pad-btn")
            self.buttons.append(lbl)
            self.btn_widgets[code] = lbl
        for i, (code, info) in enumerate(caps.get(E.EV_ABS, [])):
            name = Gtk.Label(label=code_name(E.EV_ABS, code), xalign=0)
            bar = Gtk.LevelBar(min_value=0, max_value=1, hexpand=True, valign=Gtk.Align.CENTER)
            val = Gtk.Label(label=str(info.value), xalign=1, width_chars=7)
            val.add_css_class("numeric")
            self.axes_box.attach(name, 0, i, 1, 1)
            self.axes_box.attach(bar, 1, i, 1, 1)
            self.axes_box.attach(val, 2, i, 1, 1)
            self.axis_widgets[code] = (bar, val, info)
            self._set_axis(code, info.value)

    def _set_axis(self, code, value):
        bar, val, info = self.axis_widgets[code]
        span = max(1, info.max - info.min)
        bar.set_value(min(1.0, max(0.0, (value - info.min) / span)))
        val.set_text(str(value))

    def _on_evdev(self, _fd, cond, dev):
        if cond & (GLib.IO_HUP | GLib.IO_ERR):
            append_text(self.view, "device disconnected")
            self.stop()
            return False
        try:
            events = list(dev.read())
        except BlockingIOError:
            return True
        except OSError as exc:
            append_text(self.view, f"read error: {exc}")
            self.stop()
            return False
        for ev in events:
            if ev.type == E.EV_KEY and ev.code in self.btn_widgets:
                (self.btn_widgets[ev.code].add_css_class if ev.value else
                 self.btn_widgets[ev.code].remove_css_class)("pressed")
            elif ev.type == E.EV_ABS and ev.code in self.axis_widgets:
                self._set_axis(ev.code, ev.value)
            if ev.type in (E.EV_KEY, E.EV_ABS):
                self.log.write("input", node=dev.path, type=ev.type, code=ev.code, value=ev.value,
                               name=code_name(ev.type, ev.code))
                if ev.type == E.EV_KEY:
                    append_text(self.view, f"{ev.timestamp():.3f} {code_name(ev.type, ev.code)} "
                                           f"{'press' if ev.value else 'release'}")
        return True

    def _on_hid(self, _fd, cond, hd):
        if cond & (GLib.IO_HUP | GLib.IO_ERR):
            return False
        data = hd.read(0)
        if data:
            key = (hd.path, data[0])
            if self.last_raw.get(key) != data:
                self.last_raw[key] = data
                self.log.write("hid_rx", node=hd.path, hex=hexstr(data))
                append_text(self.view, f"{hd.path} [{len(data)}] {hexstr(data)}")
        return True


# --------------------------------------------------------------------------- rumble test
class RumblePage(Page):
    title, icon = "Rumble", "audio-volume-high-symbolic"

    def __init__(self, toast):
        super().__init__(toast)
        page = Adw.PreferencesPage(vexpand=True)
        self.append(page)
        g = Adw.PreferencesGroup(title="Rumble test")
        page.add(g)
        self.path_row = row("Path", "")
        g.add(self.path_row)
        self.strong = Adw.SpinRow.new_with_range(0, 100, 5)
        self.strong.set_title("Strong / left motor %")
        self.strong.set_value(100)
        self.weak = Adw.SpinRow.new_with_range(0, 100, 5)
        self.weak.set_title("Weak / right motor %")
        self.weak.set_value(100)
        self.ms = Adw.SpinRow.new_with_range(50, 5000, 50)
        self.ms.set_title("Duration (ms)")
        self.ms.set_value(800)
        for r in (self.strong, self.weak, self.ms):
            g.add(r)
        g2 = Adw.PreferencesGroup()
        page.add(g2)
        box = Gtk.Box(spacing=8, halign=Gtk.Align.CENTER)
        self.go = Gtk.Button(label="Rumble")
        self.go.add_css_class("suggested-action")
        self.go.add_css_class("pill")
        self.go.connect("clicked", lambda _b: self.fire(pattern=False))
        self.pat = Gtk.Button(label="Left → right → both")
        self.pat.add_css_class("pill")
        self.pat.connect("clicked", lambda _b: self.fire(pattern=True))
        box.append(self.go)
        box.append(self.pat)
        g2.add(box)

    def set_controller(self, ctrl):
        super().set_controller(ctrl)
        if not ctrl:
            return
        prof = profiles.load(ctrl.vendor, ctrl.product)
        if ctrl.has_ff:
            set_sub(self.path_row, f"kernel FF on {ctrl.ff_node.path}")
        elif prof and prof.rumble:
            set_sub(self.path_row, f"hidraw via profile {prof.path}")
        else:
            set_sub(self.path_row, "none: run the Probe first")
        sensitive = ctrl.has_ff or bool(prof and prof.rumble)
        self.go.set_sensitive(sensitive)
        self.pat.set_sensitive(sensitive)

    def fire(self, pattern: bool):
        ctrl = self.ctrl
        if not ctrl:
            return
        s = round(self.strong.get_value() / 100 * 0xFFFF)
        w = round(self.weak.get_value() / 100 * 0xFFFF)
        ms = int(self.ms.get_value())
        seq = [(0xFFFF, 0, 500), (0, 0xFFFF, 500), (0xFFFF, 0xFFFF, 500)] if pattern else [(s, w, ms)]

        def work():
            log = SessionLog(ctrl.id_str, "rumble", {"device": ctrl.to_dict()})
            try:
                prof = profiles.load(ctrl.vendor, ctrl.product)
                if ctrl.has_ff:
                    from padlink.core.ff import evdev_rumble
                    for a, b, d in seq:
                        log.write("note", text="evdev rumble", strong=a, weak=b, ms=d)
                        evdev_rumble(ctrl.ff_node.path, a, b, d)
                        time.sleep(0.15)
                else:
                    from padlink.core.bridge import pick_hidraw
                    from padlink.core.ff import HidrawSink
                    sink = HidrawSink(pick_hidraw(ctrl, prof.rumble), prof.rumble, log)
                    try:
                        for a, b, d in seq:
                            sink.set(a, b)
                            time.sleep(d / 1000)
                            sink.set(0, 0)
                            time.sleep(0.15)
                    finally:
                        sink.close()
            finally:
                log.close()

        self.go.set_sensitive(False)
        run_bg(work, lambda _r: self.go.set_sensitive(True),
               lambda e: (self.go.set_sensitive(True), self.toast(f"Rumble failed: {e}")))


# --------------------------------------------------------------------------- probe wizard
class ProbePage(Page):
    title, icon = "Probe", "system-search-symbolic"

    def __init__(self, toast):
        super().__init__(toast)
        self.status = Adw.StatusPage(icon_name="system-search-symbolic", title="Find the rumble report",
                                     vexpand=True)
        self.append(self.status)
        self.hex = Gtk.Label(selectable=True, wrap=True)
        self.hex.add_css_class("mono-small")
        self.buttons = Gtk.Box(spacing=8, halign=Gtk.Align.CENTER)
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12)
        box.append(self.hex)
        box.append(self.buttons)
        self.status.set_child(box)
        self.prober: prober.Prober | None = None
        self.log = None
        self.reset()

    def set_controller(self, ctrl):
        self.reset()
        super().set_controller(ctrl)
        if not ctrl:
            return
        if ctrl.has_ff:
            self.status.set_description("This controller already has kernel force feedback. Use the Rumble tab. "
                                        "(You can still probe its hidraw interface.)")
        elif not ctrl.hidraw:
            self.status.set_description("No hidraw interface: nothing to probe.")
        elif not any(h.writable for h in ctrl.hidraw):
            self.status.set_description(
                "No write access to the hidraw node(s). Install the padlink udev rule (Doctor tab), then replug.")
        else:
            self.status.set_description("padlink will send candidate output reports. Each one vibrates for "
                                        "~0.7 s if it is right. Hold the controller and answer each question.")

    def _set_buttons(self, specs):
        while (c := self.buttons.get_first_child()) is not None:
            self.buttons.remove(c)
        for label, cb, style in specs:
            b = Gtk.Button(label=label)
            b.add_css_class("pill")
            if style:
                b.add_css_class(style)
            b.connect("clicked", lambda _b, f=cb: f())
            self.buttons.append(b)

    def _busy(self, busy: bool):
        c = self.buttons.get_first_child()
        while c is not None:
            c.set_sensitive(not busy)
            c = c.get_next_sibling()

    def reset(self):
        if self.prober:
            self.prober.close()
            self.prober = None
        if self.log:
            self.log.close()
            self.log = None
        self.stage = 0
        self.cands = []
        self.idx = 0
        self.hit = None
        self.motors = {}
        self.final = None
        self.status.set_title("Find the rumble report")
        self.hex.set_text("")
        self._set_buttons([("Start probe", self.start, "suggested-action")])

    def shutdown(self):
        self.reset()

    def start(self):
        ctrl = self.ctrl
        if not ctrl:
            return
        self.cands = prober.stage1_candidates(ctrl)
        if not self.cands:
            self.cands = prober.stage1_candidates(ctrl, blind=True)
            self.toast("No output reports declared; trying common report IDs blindly")
        self.log = SessionLog(ctrl.id_str, "probe", {"device": ctrl.to_dict()})
        self.prober = prober.Prober(self.log)
        self.stage, self.idx = 1, 0
        self._pulse()

    def _current(self):
        return self.cands[self.idx]

    def _pulse(self):
        if self.idx >= len(self.cands):
            if self.stage == 1:
                self.status.set_title("No candidate vibrated")
                self.status.set_description("Check the controller mode (XInput mode may work natively), or capture "
                                            "the official app's USB traffic with `padlink capture`.")
                self._set_buttons([("Start over", self.reset, None)])
            else:
                self._finish_stage2()
            return
        cand = self._current()
        self.status.set_title(f"Test {self.idx + 1} of {len(self.cands)}")
        self.status.set_description(cand.label)
        self.hex.set_text(hexstr(cand.on_bytes()))
        if self.stage == 1:
            self._set_buttons([("It vibrated", self._yes, "suggested-action"), ("Nothing", self._no, None),
                               ("Repeat", self._pulse, None), ("Cancel", self.reset, "destructive-action")])
        else:
            self._set_buttons([("Left / heavy", lambda: self._motor("strong"), None),
                               ("Right / light", lambda: self._motor("weak"), None),
                               ("Both", lambda: self._motor("both"), None), ("Nothing", self._no, None),
                               ("Skip rest", self._finish_stage2, None)])
        self._busy(True)
        p = self.prober

        def done(err):
            self._busy(False)
            if err:
                self.toast(f"Write failed: {err}")

        run_bg(lambda: p.pulse(cand), done, lambda e: (self._busy(False), self.toast(str(e))))

    def _yes(self):
        self.hit = self._current()
        self.log.write("note", text="probe hit", label=self.hit.label, template=self.hit.template)
        if self.hit.meta.get("kind") == "all_ff":
            self.stage, self.cands, self.idx = 2, prober.stage2_byte_candidates(self.hit), 0
            self.toast("Found it. Now: which byte drives which motor?")
            self._pulse()
        else:
            self.final = self.hit
            self._offer_save()

    def _no(self):
        self.idx += 1
        self._pulse()

    def _motor(self, which):
        self.motors[self._current().meta["pos"]] = which
        self.idx += 1
        self._pulse()

    def _finish_stage2(self):
        self.final = prober.build_from_bytes(self.hit, self.motors)
        self._offer_save()

    def _offer_save(self):
        self.status.set_title("Rumble report found")
        self.status.set_description(f"{self.final.label}\ntemplate: {self.final.template}")
        self.hex.set_text(hexstr(self.final.on_bytes()))
        self._set_buttons([("Save profile", self._save, "suggested-action"), ("Test again", self._test_final, None),
                           ("Start over", self.reset, None)])

    def _test_final(self):
        self._busy(True)
        run_bg(lambda: self.prober.pulse(self.final), lambda _e: self._busy(False))

    def _save(self):
        ctrl = self.ctrl
        prof = profiles.load_or_new(ctrl.vendor, ctrl.product, ctrl.name)
        prof.rumble = self.final.spec()
        prof.notes = (prof.notes + f"\nprobed {time.strftime('%Y-%m-%d %H:%M')} via {self.final.label}").strip()
        path = prof.save()
        self.log.write("note", text="profile saved", path=str(path), profile=prof.to_dict())
        self.toast(f"Saved {path}")
        self.reset()
        self.status.set_title("Profile saved")
        self.status.set_description(f"{path}\nTest it in the Rumble tab, then start the Bridge.")


# --------------------------------------------------------------------------- bridge
class BridgePage(Page):
    title, icon = "Bridge", "network-transmit-receive-symbolic"

    def __init__(self, toast):
        super().__init__(toast)
        self.bridge: Bridge | None = None
        self.log = None
        page = Adw.PreferencesPage()
        self.append(page)
        g = Adw.PreferencesGroup(title="Rumble bridge",
                                 description="Creates a virtual Xbox 360 pad with force feedback. Games see it, "
                                             "and their rumble is forwarded to this controller.")
        page.add(g)
        self.mode = Adw.ComboRow(title="Virtual pad", model=Gtk.StringList.new(["Xbox 360 (xpad)", "Mirror source"]))
        self.grab = Adw.SwitchRow(title="Grab physical device", subtitle="so games only get input via the virtual pad",
                                  active=True)
        self.logsw = Adw.SwitchRow(title="Write JSONL log", active=True)
        self.state = row("Status", "stopped")
        self.toggle_btn = Gtk.Button(label="Start", valign=Gtk.Align.CENTER)
        self.toggle_btn.add_css_class("suggested-action")
        self.toggle_btn.connect("clicked", self.toggle)
        self.state.add_suffix(self.toggle_btn)
        for r in (self.mode, self.grab, self.logsw, self.state):
            g.add(r)

        self.lutris = Adw.PreferencesGroup(title="Lutris",
                                           description="Hide the physical controller from games so they pick the "
                                                       "virtual pad.")
        page.add(self.lutris)
        self.lutris_rows: list = []

        g3 = Adw.PreferencesGroup(title="Autostart")
        page.add(g3)
        svc = row("systemd user service", "Run the bridge in the background whenever the controller is present")
        b = Gtk.Button(label="Install", valign=Gtk.Align.CENTER)
        b.connect("clicked", self.install_service)
        svc.add_suffix(b)
        g3.add(svc)

        lbl = Gtk.Label(label="Force-feedback traffic from games", xalign=0, margin_start=12, margin_top=6)
        lbl.add_css_class("heading")
        self.append(lbl)
        sw, self.view = mono_view(160)
        sw.set_margin_start(12)
        sw.set_margin_end(12)
        sw.set_margin_bottom(12)
        self.append(sw)
        self.poll = GLib.timeout_add(1000, self._poll)

    def set_controller(self, ctrl):
        super().set_controller(ctrl)
        for r in self.lutris_rows:
            self.lutris.remove(r)
        self.lutris_rows.clear()
        busy = self.bridge is not None and self.bridge.alive
        self.toggle_btn.set_sensitive(bool(ctrl) and (busy or not ctrl.is_padlink))
        if not ctrl or ctrl.is_padlink:
            return
        env = integration.lutris_env(ctrl)
        text = " ".join(f"{k}={v}" for k, v in env.items())
        r = row("Environment variable", text)
        r.add_suffix(copy_button(self, lambda: text, self.toast))
        self.lutris.add(r)
        self.lutris_rows.append(r)
        try:
            games = integration.lutris_games()
        except Exception:
            games = []
        for slug, name in games:
            r = row(name, slug)
            b = Gtk.Button(label="Apply", valign=Gtk.Align.CENTER, tooltip_text="Add the variable to this game "
                                                                                 "(a backup is kept)")
            b.connect("clicked", lambda _b, s=slug: self._apply(s, env))
            r.add_suffix(b)
            self.lutris.add(r)
            self.lutris_rows.append(r)

    def _apply(self, slug, env):
        try:
            cfg, backup = integration.apply_lutris_env(slug, env)
            self.toast(f"Updated {cfg.name} (backup {backup.name})")
        except Exception as exc:
            self.toast(f"Failed: {exc}")

    def install_service(self, _b):
        if not self.ctrl:
            return
        path = integration.install_service(self.ctrl.id_str)
        self.toast(f"Wrote {path}. Enable: systemctl --user enable --now padlink-bridge")

    def toggle(self, _b):
        if self.bridge and self.bridge.alive:
            self.stop()
        else:
            self.start()

    def start(self):
        ctrl = self.ctrl
        if not ctrl:
            return
        self.log = SessionLog(ctrl.id_str, "bridge", {"device": ctrl.to_dict()}) if self.logsw.get_active() \
            else NullLog()
        t0 = time.time()
        self.log.listeners.append(lambda rec: GLib.idle_add(self._on_record, rec, t0))
        mode = "mirror" if self.mode.get_selected() == 1 else "xpad"
        self.bridge = Bridge(ctrl, self.log, mode=mode, grab=self.grab.get_active())
        self.bridge.start()
        self.toggle_btn.set_label("Stop")
        self.toggle_btn.remove_css_class("suggested-action")
        self.toggle_btn.add_css_class("destructive-action")
        set_sub(self.state, "starting…")

    def stop(self):
        if self.bridge:
            self.bridge.stop()
        if self.log:
            self.log.close()
        self.toggle_btn.set_label("Start")
        self.toggle_btn.remove_css_class("destructive-action")
        self.toggle_btn.add_css_class("suggested-action")

    def _on_record(self, rec, t0):
        if rec["kind"] in ("ff_upload", "ff_play", "ff_erase", "ff_gain", "note", "rumble_out"):
            append_text(self.view, format_record(rec, t0))
        return False

    def _poll(self):
        b = self.bridge
        if b is None:
            return True
        if b.alive:
            s = b.stats
            set_sub(self.state, f"running · virtual pad {b.virtual_path or '?'} · uploads {s['uploads']} · "
                                    f"plays {s['plays']} · inputs {s['inputs']}"
                                    + (f" · log {self.log.path.name}" if self.log and self.log.path else ""))
        else:
            set_sub(self.state, f"stopped{': ' + b.error if b.error else ''}")
            if self.toggle_btn.get_label() == "Stop":
                self.stop()
        return True

    def shutdown(self):
        if self.bridge and self.bridge.alive:
            self.stop()
        GLib.source_remove(self.poll)


# --------------------------------------------------------------------------- doctor
class DoctorPage(Page):
    title, icon = "Doctor", "dialog-information-symbolic"

    def __init__(self, toast):
        super().__init__(toast)
        bar = Gtk.Box(spacing=8, margin_top=12, margin_start=12, margin_end=12)
        self.run_btn = Gtk.Button(label="Run checks")
        self.run_btn.add_css_class("suggested-action")
        self.run_btn.connect("clicked", lambda _b: self.run())
        save = Gtk.Button(label="Save report to log")
        save.connect("clicked", self.save)
        bar.append(self.run_btn)
        bar.append(save)
        self.append(bar)
        self.page = Adw.PreferencesPage(vexpand=True)
        self.append(self.page)
        self.groups: list = []
        self.checks: list = []

    def set_controller(self, ctrl):
        super().set_controller(ctrl)
        self.run()

    def run(self):
        self.run_btn.set_sensitive(False)
        ctrl = self.ctrl
        run_bg(lambda: doctor.run(ctrl), self._show,
               lambda e: (self.run_btn.set_sensitive(True), self.toast(f"doctor failed: {e}")))

    def _show(self, checks):
        self.run_btn.set_sensitive(True)
        self.checks = checks
        clear_group(self.page, self.groups)
        by_scope: dict[str, Adw.PreferencesGroup] = {}
        for ch in checks:
            g = by_scope.get(ch.scope)
            if g is None:
                g = Adw.PreferencesGroup(title=ch.scope)
                self.page.add(g)
                self.groups.append(g)
                by_scope[ch.scope] = g
            r = row(ch.title, ch.detail + (f"\nfix: {ch.fix}" if ch.fix else ""))
            r.add_prefix(status_icon(ch.status))
            if ch.fix:
                r.add_suffix(copy_button(self, lambda f=ch.fix: f, self.toast))
            g.add(r)

    def save(self, _b):
        log = SessionLog(self.ctrl.id_str if self.ctrl else "system", "doctor")
        log.write("diag", checks=[c.to_dict() for c in self.checks])
        log.close()
        self.toast(f"Saved {log.path.name}")


# --------------------------------------------------------------------------- logs
class LogsPage(Page):
    title, icon = "Logs", "document-open-recent-symbolic"

    def __init__(self, toast):
        super().__init__(toast)
        paned = Gtk.Paned(orientation=Gtk.Orientation.HORIZONTAL, vexpand=True, position=330)
        left = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=6, margin_top=12, margin_start=12,
                       margin_bottom=12)
        tools = Gtk.Box(spacing=6)
        refresh = Gtk.Button(icon_name="view-refresh-symbolic", tooltip_text="Refresh")
        refresh.connect("clicked", lambda _b: self.refresh())
        folder = Gtk.Button(icon_name="folder-open-symbolic", tooltip_text="Open log folder")
        folder.connect("clicked", self._open_folder)
        tools.append(refresh)
        tools.append(folder)
        left.append(tools)
        self.listbox = Gtk.ListBox(selection_mode=Gtk.SelectionMode.SINGLE)
        self.listbox.add_css_class("navigation-sidebar")
        self.listbox.connect("row-selected", self._selected)
        sw = Gtk.ScrolledWindow(vexpand=True, min_content_width=300)
        sw.set_child(self.listbox)
        left.append(sw)
        paned.set_start_child(left)
        right = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=6, margin_top=12, margin_end=12,
                        margin_bottom=12, margin_start=6)
        self.filter = Gtk.SearchEntry(placeholder_text="filter kinds, e.g. ff_upload,ff_play")
        self.filter.connect("search-changed", lambda _e: self._selected(self.listbox, self.listbox.get_selected_row()))
        right.append(self.filter)
        sw2, self.view = mono_view(300)
        right.append(sw2)
        paned.set_end_child(right)
        self.append(paned)
        self.paths: list = []
        self.refresh()

    def _open_folder(self, _b):
        LOG_DIR.mkdir(parents=True, exist_ok=True)
        Gtk.FileLauncher.new(Gio.File.new_for_path(str(LOG_DIR))).launch(self.get_root(), None, None)

    def refresh(self):
        self.listbox.remove_all()
        self.paths = list_logs()
        for p in self.paths:
            lbl = Gtk.Label(label=p.name, xalign=0, ellipsize=3)
            lbl.set_tooltip_text(str(p))
            self.listbox.append(lbl)

    def _selected(self, _lb, r):
        if r is None:
            return
        path = self.paths[r.get_index()]
        recs = read_log(path)
        kinds = {k.strip() for k in self.filter.get_text().split(",") if k.strip()}
        t0 = recs[0]["t"] if recs and "t" in recs[0] else None
        lines = []
        for rec in recs[:20000]:
            if kinds and rec.get("kind") not in kinds:
                continue
            if rec.get("kind") == "session":
                lines.append(json.dumps(rec, indent=1)[:4000])
            else:
                lines.append(format_record(rec, t0))
        self.view.get_buffer().set_text("\n".join(lines))

    def set_controller(self, ctrl):
        super().set_controller(ctrl)
        self.refresh()


PAGES = [OverviewPage, LivePage, RumblePage, ProbePage, BridgePage, DoctorPage, LogsPage]
