"""Diagnostics: why doesn't my controller rumble?"""

from __future__ import annotations

import os
import re
import shutil
from dataclasses import asdict, dataclass
from pathlib import Path

from padlink.core import discovery, hiddesc, profiles

OK, INFO, WARN, FAIL = "ok", "info", "warn", "fail"

PADLINK_RULE = Path("/etc/udev/rules.d/70-padlink.rules")
CONFLICTING_PROCS = {"ds4drv", "xboxdrv", "antimicrox", "inputplumber", "sc-controller", "joycond", "steam"}
LUTRIS_GAMES = Path.home() / ".local/share/lutris/games"


@dataclass
class Check:
    id: str
    title: str
    status: str
    detail: str = ""
    fix: str = ""
    scope: str = "system"

    def to_dict(self) -> dict:
        return asdict(self)


def _procs() -> dict[int, str]:
    out = {}
    for d in Path("/proc").iterdir():
        if d.name.isdigit():
            try:
                out[int(d.name)] = (d / "comm").read_text().strip()
            except OSError:
                pass
    return out


def system_checks() -> list[Check]:
    checks = []
    uinput_ok = os.access("/dev/uinput", os.W_OK)
    checks.append(Check(
        "uinput", "/dev/uinput writable (needed for the virtual pad)", OK if uinput_ok else FAIL,
        "" if uinput_ok else "padlink bridge cannot create a virtual controller",
        "" if uinput_ok else "sudo modprobe uinput; add a udev rule: KERNEL==\"uinput\", TAG+=\"uaccess\"",
    ))

    running = {}
    for pid, comm in _procs().items():
        base = comm.split("/")[0]
        if base in CONFLICTING_PROCS:
            running.setdefault(base, []).append(pid)
    if "steam" in running:
        checks.append(Check(
            "steam", "Steam is running", WARN,
            "Steam Input can open controllers (including generic HID pads) and re-expose them as its "
            "own virtual pad. For Lutris games this can swallow rumble or duplicate controllers.",
            "Close Steam while testing, or in Steam: Settings > Controller > disable "
            "'Enable Steam Input for Generic controllers'.",
        ))
    others = sorted(k for k in running if k != "steam")
    if others:
        checks.append(Check("remappers", "Other controller tools running", WARN, ", ".join(others),
                            "Stop them while debugging; they may grab the device."))

    leftovers = [p for p in ("/etc/udev/rules.d/50-ds4drv.rules", "/etc/udev/rules.d/99-uinput-ds4-x360.rules")
                 if Path(p).exists()]
    if leftovers:
        checks.append(Check("leftover_rules", "Leftover udev rules from other tools", INFO, ", ".join(leftovers),
                            "Harmless unless you see duplicate controllers. Remove if unused."))

    checks.append(Check(
        "padlink_rule", "padlink udev rule installed", OK if PADLINK_RULE.exists() else INFO,
        str(PADLINK_RULE) if PADLINK_RULE.exists() else "not installed (needed for hidraw rumble on HID-mode dongles)",
        "" if PADLINK_RULE.exists() else "padlink udev-rule <dev> | sudo tee /etc/udev/rules.d/70-padlink.rules && "
        "sudo udevadm control --reload && sudo udevadm trigger",
    ))

    xpad = any(Path("/lib/modules", os.uname().release).rglob("xpad.ko*"))
    checks.append(Check("xpad", "xpad kernel module available", OK if xpad else WARN,
                        "XInput-mode dongles (045e:028e) get native rumble from xpad" if xpad else "not found"))
    return checks


def device_checks(ctrl: discovery.Controller) -> list[Check]:
    scope = f"{ctrl.name} [{ctrl.id_str}]"
    checks: list[Check] = []

    def add(*args, **kw):
        checks.append(Check(*args, scope=scope, **kw))

    if ctrl.is_padlink:
        add("virtual", "padlink virtual pad", INFO, f"created by padlink bridge ({ctrl.phys})")
    elif (ctrl.vendor, ctrl.product) == (0x3537, 0x1041):
        add("mode", "GameSir/Zikway dongle in HID (Android/DInput) mode", WARN,
            "In this mode Linux uses hid-generic, which has no rumble support. Two fixes: switch the "
            "controller to XInput/PC mode so it shows up as 045e:028e and xpad drives rumble natively "
            "(check the T4N Lite manual for the mode button combo), or keep HID mode and use "
            "`padlink probe` + `padlink bridge`.")
    elif ctrl.vendor == 0x045E and ctrl.driver == "xpad":
        add("mode", "XInput device driven by xpad", OK, "the kernel provides rumble natively")
    if ctrl.known_as:
        add("identity", "Recognised device", INFO, ctrl.known_as)

    add("driver", "Kernel driver", INFO, ctrl.driver or "unknown")

    if ctrl.has_ff:
        add("ff", "Kernel force feedback (EV_FF)", OK, ", ".join(ctrl.ff_node.ff_names))
    else:
        add("ff", "Kernel force feedback (EV_FF)", WARN,
            "missing: games will see a controller without rumble",
            "use `padlink bridge` (needs a rumble profile from `padlink probe`)")

    prim = ctrl.primary_event
    if prim:
        st = OK if prim.readable and prim.writable else FAIL
        add("event_perm", f"Access to {prim.path}", st,
            f"read={'yes' if prim.readable else 'no'} write={'yes' if prim.writable else 'no'}",
            "" if st == OK else "joystick nodes normally get uaccess ACLs from systemd; check `getfacl " + prim.path + "`")

    for node in ctrl.hidraw:
        try:
            desc = hiddesc.parse(node.descriptor())
        except OSError as exc:
            add("hid_desc", f"HID descriptor of {node.path}", WARN, str(exc))
            continue
        outs = [r for r in desc.by_kind("output")]
        detail = (f"apps: {', '.join(desc.applications) or '-'}; output reports: "
                  + (", ".join(f"{r.report_id:#04x}({r.payload_len}B)" for r in outs) or "none"))
        if desc.has_pid:
            detail += "; has PID force-feedback page (hid-pidff could drive it if usbhid binds)"
        add("hid_desc", f"HID descriptor of {node.path} (if {node.interface or '-'})", INFO, detail)
        add("hid_perm", f"Write access to {node.path}", OK if node.writable else WARN,
            "" if node.writable else "needed to send rumble reports",
            "" if node.writable else "install the padlink udev rule (see system checks)")

    if not ctrl.is_padlink:
        prof = profiles.load(ctrl.vendor, ctrl.product)
        if prof and prof.rumble:
            add("profile", "Rumble profile", OK, f"{prof.path}: {prof.rumble.report}")
        elif not ctrl.has_ff:
            add("profile", "Rumble profile", WARN, "none yet", f"padlink probe {ctrl.id_str}")

    paths = [n.path for n in ctrl.events] + [n.path for n in ctrl.hidraw] + ctrl.joysticks
    held = discovery.holders(paths)
    if held:
        txt = "; ".join(f"{h['comm']}({h['pid']}): {', '.join(h['nodes'])}" for h in held)
        steamish = any("steam" in h["comm"].lower() for h in held)
        add("holders", "Processes with the device open", WARN if steamish else INFO, txt,
            "Steam holding the device can intercept rumble; close it or disable Steam Input for it" if steamish else "")

    if ctrl.battery:
        add("battery", "Battery", INFO, f"{ctrl.battery.get('capacity')}% {ctrl.battery.get('status')}")
    return checks


def _winebus_keys(prefix: Path) -> dict | None:
    for reg in (prefix / "system.reg", prefix / "pfx" / "system.reg"):
        if reg.exists():
            text = reg.read_text(errors="replace")
            m = re.search(r"^\[System\\\\CurrentControlSet\\\\Services\\\\winebus\][^\n]*\n(.*?)(?:\n\n|\Z)",
                          text, re.M | re.S)
            keys = {}
            if m:
                for line in m.group(1).splitlines():
                    kv = re.match(r'"([^"]+)"=(.*)', line)
                    if kv:
                        keys[kv.group(1)] = kv.group(2)
            return {"reg": str(reg), "keys": keys}
    return None


def lutris_checks(ctrl: discovery.Controller | None = None) -> list[Check]:
    checks: list[Check] = []
    if not LUTRIS_GAMES.exists():
        return checks
    try:
        import yaml
    except ImportError:
        return [Check("lutris", "Lutris configs", INFO, "python-yaml missing; skipped", scope="lutris")]
    for yml in sorted(LUTRIS_GAMES.glob("*.yml")):
        try:
            cfg = yaml.safe_load(yml.read_text()) or {}
        except Exception as exc:
            checks.append(Check("lutris_cfg", yml.stem, WARN, f"unreadable: {exc}", scope="lutris"))
            continue
        game = cfg.get("game") or {}
        env = (cfg.get("system") or {}).get("env") or {}
        prefix = game.get("prefix")
        parts = []
        status = INFO
        ignore = env.get("SDL_GAMECONTROLLER_IGNORE_DEVICES", "")
        if ctrl and not ctrl.is_padlink:
            if f"0x{ctrl.vendor:04x}/0x{ctrl.product:04x}" in ignore.lower():
                parts.append("physical pad hidden from SDL (ok for bridge)")
            else:
                parts.append("physical pad NOT hidden (game may pick the rumble-less device)")
                status = WARN
        if prefix:
            wb = _winebus_keys(Path(prefix))
            if wb is None:
                parts.append(f"prefix {prefix}: no system.reg (not created yet?)")
            else:
                parts.append(f"winebus keys: {wb['keys'] or 'defaults'}")
                if wb["keys"].get("DisableInput", "").endswith("1"):
                    status = WARN
        runner_env = ", ".join(f"{k}={v}" for k, v in env.items())
        if runner_env:
            parts.append(f"env: {runner_env}")
        checks.append(Check("lutris_game", f"Lutris: {cfg.get('name') or yml.stem}", status, "; ".join(parts),
                            "" if status != WARN or not ctrl else f"padlink lutris-env {ctrl.id_str} --apply {yml.stem}",
                            scope="lutris"))
    return checks


def run(ctrl: discovery.Controller | None = None, all_devices: bool = True) -> list[Check]:
    checks = system_checks()
    controllers = discovery.scan()
    if not controllers:
        checks.append(Check("none", "No game controllers detected", FAIL,
                            "Is the dongle plugged in and the controller switched on?",
                            "padlink list --all   # shows every HID device in case udev misclassifies it"))
    targets = [ctrl] if ctrl else (controllers if all_devices else [])
    for c in targets:
        checks.extend(device_checks(c))
    checks.extend(lutris_checks(ctrl or next((c for c in controllers if not c.is_padlink), None)))
    if shutil.which("lutris") is None:
        checks.append(Check("lutris_bin", "lutris not on PATH", INFO, scope="lutris"))
    return checks
