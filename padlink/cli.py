"""padlink command line interface."""

from __future__ import annotations

import argparse
import json
import select
import signal
import sys
import time

from padlink import __version__
from padlink.core import discovery, doctor, hiddesc, integration, profiles
from padlink.core.hidraw import HidrawDevice, hexstr, parse_hex
from padlink.core.logger import NullLog, SessionLog, format_record, list_logs, read_log
from padlink.core.paths import LOG_DIR, PROFILE_DIR

TTY = sys.stdout.isatty()


def c(text: str, code: str) -> str:
    return f"\033[{code}m{text}\033[0m" if TTY else text


def bold(t): return c(t, "1")
def dim(t): return c(t, "2")
def green(t): return c(t, "32")
def yellow(t): return c(t, "33")
def red(t): return c(t, "31")
def cyan(t): return c(t, "36")


STATUS_STYLE = {"ok": green("  OK "), "info": cyan(" INFO"), "warn": yellow(" WARN"), "fail": red(" FAIL")}


def die(msg: str, code: int = 1) -> int:
    print(red(f"error: {msg}"), file=sys.stderr)
    return code


def get_ctrl(args) -> discovery.Controller:
    return discovery.resolve(getattr(args, "device", None), include_all=getattr(args, "all", False))


def open_log(args, command: str, ctrl: discovery.Controller | None):
    if getattr(args, "no_log", False):
        return NullLog()
    extra = {"device": ctrl.to_dict()} if ctrl else {}
    log = SessionLog(ctrl.id_str if ctrl else "system", command, extra)
    print(dim(f"logging to {log.path}"), file=sys.stderr)
    return log


# ---------------------------------------------------------------- list / info
def cmd_list(args) -> int:
    ctrls = discovery.scan(include_all=args.all)
    if args.json:
        print(json.dumps([x.to_dict() for x in ctrls], indent=2))
        return 0
    if not ctrls:
        print("No controllers found." + ("" if args.all else " Try `padlink list --all`."))
        return 1
    for x in ctrls:
        ff = green("rumble: kernel FF") if x.has_ff else yellow("rumble: none (no EV_FF)")
        prof = profiles.load(x.vendor, x.product)
        if not x.has_ff and prof and prof.rumble:
            ff = green("rumble: via padlink profile")
        tag = cyan(" [padlink virtual]") if x.is_padlink else ""
        print(f"{bold(str(x.index))}  {bold(x.name)}{tag}")
        print(f"   {x.id_str}  bus={x.bus}  driver={x.driver or '-'}  {ff}")
        if x.known_as:
            print(f"   {dim(x.known_as)}")
        nodes = ", ".join(f"{n.path}({n.role}{'' if n.readable else ',no-access'})" for n in x.events)
        print(f"   events: {nodes}")
        if x.hidraw:
            print("   hidraw: " + ", ".join(f"{h.path}{'' if h.writable else '(ro)' if h.readable else '(no-access)'}"
                                         for h in x.hidraw))
        if x.battery:
            print(f"   battery: {x.battery.get('capacity')}% {x.battery.get('status')}")
    return 0


def cmd_info(args) -> int:
    x = get_ctrl(args)
    if args.json:
        d = x.to_dict()
        d["descriptors"] = {}
        for h in x.hidraw:
            try:
                d["descriptors"][h.path] = h.descriptor().hex(" ")
            except OSError as exc:
                d["descriptors"][h.path] = f"error: {exc}"
        print(json.dumps(d, indent=2))
        return 0
    print(bold(f"{x.name}  [{x.id_str}]"))
    for k in ("bus", "driver", "uniq", "phys", "key"):
        print(f"  {k:8s} {getattr(x, k) or '-'}")
    print(f"  version  0x{x.version:04x}")
    if x.known_as:
        print(f"  known as {x.known_as}")
    if x.battery:
        print(f"  battery  {x.battery}")
    for n in x.events:
        print(bold(f"\n  {n.path}  {n.name}  ({n.role})"))
        print(f"    access: read={n.readable} write={n.writable}")
        keys = sorted(n.caps.get("key", ()))
        from padlink.core.bridge import code_name
        from evdev import ecodes as E
        if keys:
            print(f"    keys ({len(keys)}): " + " ".join(code_name(E.EV_KEY, k) for k in keys[:40]))
        abses = sorted(n.caps.get("abs", ()))
        if abses:
            print(f"    axes: " + " ".join(code_name(E.EV_ABS, a) for a in abses))
        print(f"    force feedback: {', '.join(n.ff_names) if n.has_ff else 'none'}")
    for h in x.hidraw:
        print(bold(f"\n  {h.path}  (usb interface {h.interface or '-'})  read={h.readable} write={h.writable}"))
        try:
            raw = h.descriptor()
        except OSError as exc:
            print(f"    descriptor: {exc}")
            continue
        print(dim(f"    descriptor ({len(raw)} bytes): {raw.hex(' ')}"))
        for line in hiddesc.parse(raw).summary():
            print("    " + line)
    prof = profiles.load(x.vendor, x.product)
    print(bold("\n  profile: ") + (str(prof.path) if prof else "none"))
    if prof:
        print("    " + json.dumps(prof.to_dict()))
    return 0


# ---------------------------------------------------------------- monitor
def cmd_monitor(args) -> int:
    x = get_ctrl(args)
    log = open_log(args, "monitor", x)
    stop = []
    signal.signal(signal.SIGINT, lambda *_: stop.append(1))
    if args.raw:
        nodes = [h for h in x.hidraw if (not args.node or h.path == args.node)]
        if not nodes:
            return die("no hidraw nodes (or none matching --node)")
        devs = []
        for h in nodes:
            try:
                devs.append(HidrawDevice(h.path))
            except OSError as exc:
                print(yellow(f"{h.path}: {exc}  (install the udev rule: padlink udev-rule)"))
        if not devs:
            return 1
        last: dict[tuple[str, int], bytes] = {}
        print(dim("raw HID input reports; changed bytes highlighted. Ctrl+C to stop."))
        while not stop:
            r, _, _ = select.select(devs, [], [], 0.5)
            for d in r:
                data = d.read(0)
                if not data:
                    continue
                key = (d.path, data[0])
                prev = last.get(key)
                last[key] = data
                if prev == data and not args.verbose:
                    continue
                log.write("hid_rx", node=d.path, hex=hexstr(data))
                parts = []
                for i, b in enumerate(data):
                    s = f"{b:02x}"
                    parts.append(yellow(s) if prev is not None and (i >= len(prev) or prev[i] != b) else s)
                print(f"{time.strftime('%H:%M:%S')} {d.path} [{len(data):3d}] {' '.join(parts)}")
        for d in devs:
            d.close()
        log.close()
        return 0

    from evdev import InputDevice, ecodes as E
    from padlink.core.bridge import code_name

    nodes = [n for n in x.events if args.all_nodes or n.is_joystick or n is x.primary_event]
    devs = []
    for n in nodes:
        try:
            devs.append(InputDevice(n.path))
        except OSError as exc:
            print(yellow(f"{n.path}: {exc}"))
    if not devs:
        return 1
    print(dim(f"evdev events from {', '.join(d.path for d in devs)}. Ctrl+C to stop."))
    while not stop:
        r, _, _ = select.select(devs, [], [], 0.5)
        for d in r:
            try:
                events = list(d.read())
            except BlockingIOError:
                continue
            except OSError as exc:
                print(red(f"{d.path}: {exc}"))
                stop.append(1)
                break
            for ev in events:
                if ev.type == E.EV_SYN or (ev.type == E.EV_MSC and not args.verbose):
                    continue
                name = code_name(ev.type, ev.code)
                log.write("input", node=d.path, type=ev.type, code=ev.code, value=ev.value, name=name)
                tname = {E.EV_KEY: "KEY", E.EV_ABS: "ABS", E.EV_MSC: "MSC", E.EV_FF: "FF"}.get(ev.type, str(ev.type))
                val = (green("press") if ev.value == 1 else dim("release") if ev.value == 0 else "repeat") \
                    if ev.type == E.EV_KEY else str(ev.value)
                print(f"{ev.timestamp():.3f} {d.path} {tname:3s} {name:16s} {val}")
    for d in devs:
        d.close()
    log.close()
    return 0


# ---------------------------------------------------------------- rumble / hid
def pct(v: float) -> int:
    return max(0, min(0xFFFF, round(v / 100.0 * 0xFFFF)))


def cmd_rumble(args) -> int:
    x = get_ctrl(args)
    strong, weak = pct(args.strong), pct(args.weak)
    log = open_log(args, "rumble", x)
    prof = profiles.load(x.vendor, x.product)
    use_hid = args.hid or (not x.has_ff and prof and prof.rumble)
    try:
        if not use_hid:
            node = x.ff_node
            if node is None:
                return die(f"{x.name} has no kernel force feedback and no padlink rumble profile. "
                           f"Run: padlink probe {x.id_str}")
            from padlink.core.ff import evdev_rumble
            print(f"evdev FF on {node.path}: strong={args.strong}% weak={args.weak}% for {args.ms} ms")
            log.write("note", text="evdev rumble test", node=node.path, strong=strong, weak=weak, ms=args.ms)
            evdev_rumble(node.path, strong, weak, args.ms)
        else:
            if not prof or not prof.rumble:
                return die("no rumble profile; run `padlink probe` first")
            from padlink.core.bridge import pick_hidraw
            from padlink.core.ff import HidrawSink
            path = pick_hidraw(x, prof.rumble)
            sink = HidrawSink(path, prof.rumble, log)
            print(f"hidraw {path}: {hexstr(prof.rumble.render(strong, weak))} for {args.ms} ms")
            try:
                end = time.monotonic() + args.ms / 1000.0
                ka = (prof.rumble.keepalive_ms or 0) / 1000.0
                sink.set(strong, weak)
                while ka and time.monotonic() + ka < end:
                    time.sleep(ka)
                    sink.set(strong, weak)
                time.sleep(max(0.0, end - time.monotonic()))
            finally:
                sink.close()
    except PermissionError as exc:
        return die(f"{exc}. For hidraw: install the udev rule (padlink udev-rule {x.id_str}).")
    finally:
        log.close()
    return 0


def _pick_hidraw_node(x: discovery.Controller, node: str | None) -> str:
    if node:
        return node
    if not x.hidraw:
        raise SystemExit(die("controller has no hidraw node"))
    return x.hidraw[0].path


def cmd_hid_send(args) -> int:
    x = get_ctrl(args)
    path = _pick_hidraw_node(x, args.node)
    data = parse_hex(args.hex)
    log = open_log(args, "hid-send", x)
    try:
        with HidrawDevice(path, write=True) as d:
            n = d.write(data)
            log.write("hid_tx", node=path, hex=hexstr(data), ok=True)
            print(f"wrote {n} bytes to {path}: {hexstr(data)}")
            end = time.monotonic() + args.read
            while time.monotonic() < end:
                resp = d.read(max(0.0, end - time.monotonic()))
                if resp:
                    log.write("hid_rx", node=path, hex=hexstr(resp))
                    print(f"  <- {hexstr(resp)}")
    except OSError as exc:
        log.write("hid_tx", node=path, hex=hexstr(data), ok=False, error=str(exc))
        return die(f"{path}: {exc}")
    finally:
        log.close()
    return 0


def cmd_hid_feature(args) -> int:
    x = get_ctrl(args)
    path = _pick_hidraw_node(x, args.node)
    log = open_log(args, "hid-feature", x)
    try:
        with HidrawDevice(path, write=True) as d:
            if args.action == "get":
                data = d.get_feature(int(args.value, 0), args.length)
                log.write("hid_rx", node=path, hex=hexstr(data), feature=True)
                print(hexstr(data))
            else:
                data = parse_hex(args.value)
                d.set_feature(data)
                log.write("hid_tx", node=path, hex=hexstr(data), ok=True, feature=True)
                print(f"set feature: {hexstr(data)}")
    except OSError as exc:
        return die(f"{path}: {exc}")
    finally:
        log.close()
    return 0


# ---------------------------------------------------------------- probe
def ask(prompt: str, choices: str) -> str:
    while True:
        try:
            ans = input(f"{prompt} [{'/'.join(choices)}] ").strip().lower()
        except EOFError:
            return "q"
        if ans and ans[0] in choices:
            return ans[0]


def cmd_probe(args) -> int:
    from padlink.core import prober

    x = get_ctrl(args)
    if x.has_ff and not args.force:
        print(f"{x.name} already has kernel force feedback ({', '.join(x.ff_node.ff_names)}).")
        print(f"Test it with: padlink rumble {x.id_str}   (use --force to probe hidraw anyway)")
        return 0
    if not x.hidraw:
        return die("no hidraw nodes for this controller; nothing to probe")
    unwritable = [h.path for h in x.hidraw if not h.writable]
    if len(unwritable) == len(x.hidraw):
        print(red(f"No write access to {', '.join(unwritable)}."))
        print("Install the udev rule first:")
        print(f"  padlink udev-rule {x.id_str} | sudo tee /etc/udev/rules.d/70-padlink.rules")
        print("  sudo udevadm control --reload && sudo udevadm trigger")
        print("then replug the dongle. (Or run this probe once with sudo.)")
        return 1
    cands = prober.stage1_candidates(x, blind=args.blind)
    if not cands:
        print(yellow("The HID descriptors declare no output reports."))
        print("Rerun with --blind to try common report IDs anyway, or capture the official app's traffic "
              "(see `padlink capture --help`).")
        return 1
    log = open_log(args, "probe", x)
    p = prober.Prober(log)
    print(bold(f"Rumble probe for {x.name} [{x.id_str}]"))
    print("Hold the controller. Each test vibrates for ~0.7 s if it works.")
    print("Answer y = vibrated, n = nothing, r = repeat, q = quit.\n")
    hit = None
    try:
        for i, cand in enumerate(cands, 1):
            while True:
                print(f"[{i}/{len(cands)}] {cand.label}  {dim(hexstr(cand.on_bytes()))}")
                err = p.pulse(cand, args.ms)
                if err:
                    print(yellow(f"   write failed: {err}"))
                    ans = "n"
                    break
                ans = ask("   did it vibrate?", "ynrq")
                if ans != "r":
                    break
            if ans == "q":
                return 1
            if ans == "y":
                hit = cand
                log.write("note", text="probe hit", label=cand.label, template=cand.template)
                break
        if hit is None:
            print(yellow("\nNo candidate made it vibrate."))
            print("Next steps: try `--blind`, check the controller mode (XInput mode may just work), or capture "
                  "traffic from the official GameSir app with `padlink capture` on a Windows VM/Android host.")
            return 1

        final = hit
        if hit.meta.get("kind") == "all_ff":
            print(bold("\nFound a working report. Now let's find which byte drives which motor."))
            print("Answer: l = left/heavy motor, r = right/light motor, b = both/can't tell, n = nothing, "
                  "q = stop narrowing")
            motors: dict[int, str] = {}
            for cand in prober.stage2_byte_candidates(hit):
                print(f"  {cand.label}  {dim(hexstr(cand.on_bytes()))}")
                err = p.pulse(cand, args.ms)
                if err:
                    print(yellow(f"   write failed: {err}"))
                    continue
                ans = ask("   vibrated?", "lrbnq")
                if ans == "q":
                    break
                if ans in "lrb":
                    motors[cand.meta["pos"]] = {"l": "strong", "r": "weak", "b": "both"}[ans]
            final = prober.build_from_bytes(hit, motors)
            if not motors:
                print(yellow("No single byte worked alone; the profile will use the all-0xFF report as on/off."))

        print(bold("\nVerifying the final layout: weak motor 50%, then strong 100%."))
        spec = final.spec()
        for s, w in ((0, 0x7FFF), (0xFFFF, 0)):
            data = spec.render(s, w)
            p.send(final.node.path, data)
            time.sleep(0.6)
            p.send(final.node.path, spec.render(0, 0))
            time.sleep(0.3)
        prof = profiles.load_or_new(x.vendor, x.product, x.name)
        prof.rumble = spec
        prof.notes = (prof.notes + f"\nprobed {time.strftime('%Y-%m-%d %H:%M')} via {final.label}").strip()
        path = prof.save()
        log.write("note", text="profile saved", path=str(path), profile=prof.to_dict())
        print(green(f"Saved {path}"))
        print(f"Test:   padlink rumble {x.id_str}")
        print(f"Bridge: padlink bridge {x.id_str}")
    finally:
        p.close()
        log.close()
    return 0


# ---------------------------------------------------------------- bridge
def cmd_bridge(args) -> int:
    from padlink.core.bridge import Bridge, wait_for

    while True:
        if args.wait:
            print(dim(f"waiting for {args.device or 'a controller'}..."), file=sys.stderr)
            x = wait_for(args.device, include_all=args.all)
        else:
            x = get_ctrl(args)
        if x.is_padlink:
            return die("that is a padlink virtual pad; pick the physical controller")
        log = SessionLog(x.id_str, "bridge", {"device": x.to_dict()}) if args.log else NullLog()
        if args.log:
            print(dim(f"logging to {log.path}"), file=sys.stderr)

        def show(rec, t0=time.time()):
            if rec["kind"] in ("ff_upload", "ff_erase", "ff_play", "ff_gain", "note") or \
                    (args.verbose and rec["kind"] in ("rumble_out", "hid_tx", "input")):
                print(format_record(rec, t0), flush=True)

        log.listeners.append(show)
        mapping = None
        if args.map:
            with open(args.map) as fh:
                mapping = json.load(fh)
        br = Bridge(x, log, mode=args.mode, grab=not args.no_grab, log_input=args.verbose, mapping=mapping,
                    vendor=int(args.vendor, 16) if args.vendor else None,
                    product=int(args.product, 16) if args.product else None, name=args.name)
        stop = []

        def on_sig(*_):
            stop.append(1)
            br.stop()

        signal.signal(signal.SIGINT, on_sig)
        signal.signal(signal.SIGTERM, on_sig)
        print(bold(f"Bridging {x.name} [{x.id_str}] -> virtual pad ({args.mode}). Ctrl+C to stop."))
        if not args.no_grab:
            env = integration.lutris_env(x)
            print(dim("Hide the physical pad from games: " + " ".join(f"{k}={v}" for k, v in env.items())))
        br.run()
        log.close()
        if stop or not args.wait:
            return 0 if not br.error or stop else 1
        print(yellow(f"bridge ended: {br.error}; waiting for the controller again"))
        time.sleep(1)


# ---------------------------------------------------------------- map wizard
MAP_STEPS = [
    ("BTN_A", "press A (bottom face button)"), ("BTN_B", "press B (right face button)"),
    ("BTN_X", "press X (left face button)"), ("BTN_Y", "press Y (top face button)"),
    ("BTN_TL", "press LB (left bumper)"), ("BTN_TR", "press RB (right bumper)"),
    ("ABS_Z", "pull LT (left trigger) fully"), ("ABS_RZ", "pull RT (right trigger) fully"),
    ("BTN_SELECT", "press Back/Select/Minus"), ("BTN_START", "press Start/Plus"),
    ("BTN_MODE", "press Home/Guide"), ("BTN_THUMBL", "click the left stick"), ("BTN_THUMBR", "click the right stick"),
    ("ABS_X+", "push the LEFT stick fully RIGHT"), ("ABS_Y+", "push the LEFT stick fully DOWN"),
    ("ABS_RX+", "push the RIGHT stick fully RIGHT"), ("ABS_RY+", "push the RIGHT stick fully DOWN"),
    ("ABS_HAT0X+", "press d-pad RIGHT"), ("ABS_HAT0Y+", "press d-pad DOWN"),
]


def cmd_map(args) -> int:
    from evdev import InputDevice, ecodes as E
    from padlink.core.bridge import code_name

    x = get_ctrl(args)
    node = x.primary_event
    dev = InputDevice(node.path)
    absinfo = {code: info for code, info in dev.capabilities(absinfo=True).get(E.EV_ABS, [])}
    mapping: dict = {"keys": {}, "axes": {}, "key_to_axis": {}, "dpad_buttons": False}
    print(bold(f"Input mapping for {x.name} -> Xbox 360 layout"))
    print("Do each action and release. Press Enter to skip a step.\n")

    def capture() -> tuple[int, int, int] | None:
        rest = {c: dev.absinfo(c).value for c in absinfo}
        while True:
            r, _, _ = select.select([dev, sys.stdin], [], [])
            if sys.stdin in r:
                sys.stdin.readline()
                return None
            for ev in dev.read():
                if ev.type == E.EV_KEY and ev.value == 1:
                    return (E.EV_KEY, ev.code, 1)
                if ev.type == E.EV_ABS and ev.code in absinfo:
                    info = absinfo[ev.code]
                    span = max(1, info.max - info.min)
                    delta = ev.value - rest[ev.code]
                    if abs(delta) >= (1 if span <= 2 else 0.5 * span):
                        return (E.EV_ABS, ev.code, 1 if delta > 0 else -1)

    def settle():
        end = time.monotonic() + 0.6
        while time.monotonic() < end:
            if select.select([dev], [], [], 0.1)[0]:
                list(dev.read())

    try:
        for target, prompt in MAP_STEPS:
            print(f"  {prompt} ... ", end="", flush=True)
            got = capture()
            if got is None:
                print(dim("skipped"))
                continue
            etype, code, direction = got
            name = code_name(etype, code)
            tname = target.rstrip("+")
            if etype == E.EV_KEY:
                if tname.startswith("BTN_"):
                    mapping["keys"][name] = tname
                elif tname in ("ABS_Z", "ABS_RZ"):
                    mapping["key_to_axis"][name] = tname
                elif tname.startswith("ABS_HAT") and code in (E.BTN_DPAD_RIGHT, E.BTN_DPAD_DOWN):
                    mapping["dpad_buttons"] = True
                else:
                    print(yellow(f"{name} (button) can't drive {tname}; skipped"))
                    continue
            else:
                if tname.startswith("BTN_"):
                    print(yellow(f"{name} (axis) can't drive a button; skipped"))
                    continue
                mapping["axes"][name] = {"to": tname, "invert": target.endswith("+") and direction < 0}
            print(green(name))
            settle()
    finally:
        dev.close()
    prof = profiles.load_or_new(x.vendor, x.product, x.name)
    prof.input_map = mapping
    path = prof.save()
    print(green(f"\nSaved input map to {path}"))
    print(json.dumps(mapping, indent=2))
    return 0


# ---------------------------------------------------------------- doctor & co
def cmd_doctor(args) -> int:
    x = discovery.resolve(args.device) if args.device else None
    checks = doctor.run(x)
    if args.json:
        print(json.dumps([ch.to_dict() for ch in checks], indent=2))
        return 0
    scope = None
    for ch in checks:
        if ch.scope != scope:
            scope = ch.scope
            print(bold(f"\n== {scope} =="))
        print(f"{STATUS_STYLE.get(ch.status, ch.status)}  {ch.title}")
        if ch.detail:
            print(f"        {dim(ch.detail)}")
        if ch.fix:
            print(f"        fix: {ch.fix}")
    if args.log:
        log = SessionLog(x.id_str if x else "system", "doctor")
        log.write("diag", checks=[ch.to_dict() for ch in checks])
        log.close()
        print(dim(f"\nsaved to {log.path}"))
    return 1 if any(ch.status == "fail" for ch in checks) else 0


def cmd_lutris_env(args) -> int:
    x = get_ctrl(args)
    env = integration.lutris_env(x)
    if args.apply:
        cfg, backup = integration.apply_lutris_env(args.apply, env, remove=args.remove)
        print(f"{'removed from' if args.remove else 'updated'} {cfg} (backup: {backup})")
        return 0
    print("# Lutris > game > Configure > System options > Environment variables:")
    for k, v in env.items():
        print(f"{k}={v}")
    games = integration.lutris_games()
    if games:
        print("\n# or apply automatically to a game:")
        for slug, name in games:
            print(f"padlink lutris-env {x.id_str} --apply {slug}    # {name}")
    return 0


def cmd_udev_rule(args) -> int:
    x = get_ctrl(args)
    sys.stdout.write(integration.udev_rule(x))
    if sys.stdout.isatty():
        print(dim(f"\n# install: padlink udev-rule {x.id_str} | sudo tee /etc/udev/rules.d/70-padlink.rules "
                  f"&& sudo udevadm control --reload && sudo udevadm trigger"), file=sys.stderr)
    return 0


def cmd_service(args) -> int:
    if args.action == "show":
        print(integration.systemd_unit(args.device))
        return 0
    if args.action == "install":
        path = integration.install_service(args.device)
        print(f"wrote {path}")
        print("enable with: systemctl --user daemon-reload && systemctl --user enable --now padlink-bridge")
        return 0
    return 1


def cmd_logs(args) -> int:
    logs = list_logs()
    if args.action == "path":
        print(LOG_DIR)
        return 0
    if args.action == "list":
        for i, p in enumerate(logs[: args.limit]):
            print(f"{i:3d}  {p.name}  {p.stat().st_size // 1024} KiB")
        if not logs:
            print(f"no logs in {LOG_DIR}")
        return 0
    if args.action == "show":
        target = args.file
        if target is None or target.isdigit():
            idx = int(target or 0)
            if idx >= len(logs):
                return die("no such log")
            path = logs[idx]
        else:
            from pathlib import Path
            path = Path(target)
        recs = read_log(path)
        kinds = set(args.kind.split(",")) if args.kind else None
        t0 = recs[0]["t"] if recs else None
        print(dim(str(path)))
        for r in recs:
            if kinds and r.get("kind") not in kinds:
                continue
            print(format_record(r, t0))
        return 0
    if args.action == "export":
        if not logs:
            return die("no logs")
        dest = args.file or "padlink-logs.tar.gz"
        import tarfile
        with tarfile.open(dest, "w:gz") as tar:
            for p in logs[: args.limit]:
                tar.add(p, arcname=f"logs/{p.name}")
            if PROFILE_DIR.exists():
                for p in PROFILE_DIR.glob("*.json"):
                    tar.add(p, arcname=f"profiles/{p.name}")
        print(f"wrote {dest}")
        return 0
    return 1


def cmd_capture(args) -> int:
    from padlink.core import capture

    x = get_ctrl(args)
    try:
        return capture.run(x, args.out, show=True)
    except PermissionError:
        return die("usbmon needs root: sudo -E padlink capture ... (and `sudo modprobe usbmon`)")
    except capture.CaptureError as exc:
        return die(str(exc))


# ---------------------------------------------------------------- parser
def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="padlink", description="Wireless controller companion: detect, debug, log, "
                                "and bridge rumble for controllers the kernel can't vibrate.")
    p.add_argument("--version", action="version", version=f"padlink {__version__}")
    sub = p.add_subparsers(dest="cmd", required=True)
    DEV = "controller: index from `list`, vid:pid, /dev node, or name substring (default: first)"

    def dev_arg(sp, optional=True):
        sp.add_argument("device", nargs="?" if optional else None, help=DEV)
        sp.add_argument("--all", action="store_true", help="consider every HID input device, not only joysticks")

    sp = sub.add_parser("list", help="list controllers")
    sp.add_argument("--all", action="store_true", help="include every HID/virtual input device")
    sp.add_argument("--json", action="store_true")
    sp.set_defaults(func=cmd_list)

    sp = sub.add_parser("info", help="full dump of a controller incl. parsed HID descriptor")
    dev_arg(sp)
    sp.add_argument("--json", action="store_true")
    sp.set_defaults(func=cmd_info)

    sp = sub.add_parser("monitor", help="live input events (evdev) or raw HID reports (--raw)")
    dev_arg(sp)
    sp.add_argument("--raw", action="store_true", help="read hidraw instead of evdev")
    sp.add_argument("--node", help="hidraw node to read with --raw")
    sp.add_argument("--all-nodes", action="store_true", help="also read motion/touchpad/etc. event nodes")
    sp.add_argument("-v", "--verbose", action="store_true", help="show MSC events / unchanged reports")
    sp.add_argument("--no-log", action="store_true")
    sp.set_defaults(func=cmd_monitor)

    sp = sub.add_parser("rumble", help="test rumble (kernel FF, or hidraw via profile)")
    dev_arg(sp)
    sp.add_argument("--strong", type=float, default=100, help="heavy/left motor percent (default 100)")
    sp.add_argument("--weak", type=float, default=100, help="light/right motor percent (default 100)")
    sp.add_argument("--ms", type=int, default=800)
    sp.add_argument("--hid", action="store_true", help="force the hidraw profile path")
    sp.add_argument("--no-log", action="store_true")
    sp.set_defaults(func=cmd_rumble)

    sp = sub.add_parser("hid-send", help="write a raw output report (first byte = report id, 00 if none)")
    sp.add_argument("device", help=DEV)
    sp.add_argument("hex", help='e.g. "05 ff 00 00"')
    sp.add_argument("--all", action="store_true")
    sp.add_argument("--node", help="hidraw node (default: first of the device)")
    sp.add_argument("--read", type=float, default=0.0, help="seconds to print input reports afterwards")
    sp.add_argument("--no-log", action="store_true")
    sp.set_defaults(func=cmd_hid_send)

    sp = sub.add_parser("hid-feature", help="get/set a feature report")
    sp.add_argument("action", choices=["get", "set"])
    sp.add_argument("device", help=DEV)
    sp.add_argument("value", help="get: report id (e.g. 0x05); set: hex bytes")
    sp.add_argument("--length", type=int, default=64, help="buffer length for get")
    sp.add_argument("--all", action="store_true")
    sp.add_argument("--node")
    sp.add_argument("--no-log", action="store_true")
    sp.set_defaults(func=cmd_hid_feature)

    sp = sub.add_parser("probe", help="interactively discover the rumble report, save a profile")
    dev_arg(sp)
    sp.add_argument("--ms", type=int, default=700, help="pulse length per test")
    sp.add_argument("--blind", action="store_true", help="try common report IDs even if none are declared")
    sp.add_argument("--force", action="store_true", help="probe even if kernel FF exists")
    sp.add_argument("--no-log", action="store_true")
    sp.set_defaults(func=cmd_probe)

    sp = sub.add_parser("bridge", help="virtual pad with rumble, forwarded to the real controller")
    dev_arg(sp)
    sp.add_argument("--mode", choices=["xpad", "mirror"], default="xpad")
    sp.add_argument("--wait", action="store_true", help="wait for the controller and re-bridge after reconnects")
    sp.add_argument("--log", action="store_true", help="write a JSONL session log")
    sp.add_argument("-v", "--verbose", action="store_true", help="print input and output traffic too")
    sp.add_argument("--no-grab", action="store_true", help="don't grab the physical device")
    sp.add_argument("--map", help="input map JSON file (overrides profile/heuristic)")
    sp.add_argument("--vendor", help="override virtual vendor id (hex)")
    sp.add_argument("--product", help="override virtual product id (hex)")
    sp.add_argument("--name", help="override virtual device name")
    sp.set_defaults(func=cmd_bridge)

    sp = sub.add_parser("map", help="interactive input-map wizard for the bridge (xpad mode)")
    dev_arg(sp)
    sp.set_defaults(func=cmd_map)

    sp = sub.add_parser("doctor", help="diagnose why rumble does not work")
    sp.add_argument("device", nargs="?", help=DEV)
    sp.add_argument("--json", action="store_true")
    sp.add_argument("--log", action="store_true", help="save the result as a log")
    sp.set_defaults(func=cmd_doctor)

    sp = sub.add_parser("lutris-env", help="env vars to hide the physical pad from games; can edit Lutris configs")
    dev_arg(sp)
    sp.add_argument("--apply", metavar="GAME_SLUG", help="write the vars into this Lutris game config (with backup)")
    sp.add_argument("--remove", action="store_true", help="with --apply: remove the vars instead")
    sp.set_defaults(func=cmd_lutris_env)

    sp = sub.add_parser("udev-rule", help="print a udev rule granting hidraw access for this controller")
    dev_arg(sp)
    sp.set_defaults(func=cmd_udev_rule)

    sp = sub.add_parser("service", help="systemd --user unit that keeps the bridge running")
    sp.add_argument("action", choices=["show", "install"])
    sp.add_argument("device", help="vid:pid to bridge, e.g. 3537:1041")
    sp.set_defaults(func=cmd_service)

    sp = sub.add_parser("logs", help="list/show/export session logs")
    sp.add_argument("action", choices=["list", "show", "export", "path"])
    sp.add_argument("file", nargs="?", help="show: index or path; export: output .tar.gz")
    sp.add_argument("--kind", help="show: comma-separated kinds to include (e.g. ff_upload,ff_play)")
    sp.add_argument("--limit", type=int, default=30)
    sp.set_defaults(func=cmd_logs)

    sp = sub.add_parser("capture", help="(root) sniff the controller's USB traffic via usbmon, for reverse-engineering")
    dev_arg(sp)
    sp.add_argument("--out", help="JSONL output path (default: padlink log dir of the invoking user)")
    sp.set_defaults(func=cmd_capture)
    return p


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        return args.func(args) or 0
    except discovery.NoSuchController as exc:
        return die(str(exc))
    except PermissionError as exc:
        return die(f"{exc}  (see `padlink doctor`)")
    except KeyboardInterrupt:
        return 130
    except BrokenPipeError:
        return 0


if __name__ == "__main__":
    raise SystemExit(main())
