"""Glue to the rest of the system: udev rules, Lutris game configs, systemd."""

from __future__ import annotations

import shutil
import time
from pathlib import Path

from padlink.core.discovery import Controller

LUTRIS_GAMES = Path.home() / ".local/share/lutris/games"
SYSTEMD_USER = Path.home() / ".config/systemd/user"


def udev_rule(ctrl: Controller) -> str:
    v, p = f"{ctrl.vendor:04x}", f"{ctrl.product:04x}"
    return (
        f"# padlink: let the logged-in user send rumble reports to {ctrl.name} ({v}:{p})\n"
        f'KERNEL=="hidraw*", SUBSYSTEM=="hidraw", ATTRS{{idVendor}}=="{v}", ATTRS{{idProduct}}=="{p}", TAG+="uaccess"\n'
        f'KERNEL=="hidraw*", SUBSYSTEM=="hidraw", KERNELS=="*:{v.upper()}:{p.upper()}.*", TAG+="uaccess"\n'
        f"# virtual pads created by padlink are plain uinput joysticks and get uaccess from systemd already\n"
    )


def lutris_env(ctrl: Controller) -> dict[str, str]:
    """Env vars so games see only padlink's virtual pad, not the physical one."""
    return {"SDL_GAMECONTROLLER_IGNORE_DEVICES": f"0x{ctrl.vendor:04x}/0x{ctrl.product:04x}"}


def lutris_games() -> list[tuple[str, str]]:
    import yaml

    out = []
    for yml in sorted(LUTRIS_GAMES.glob("*.yml")):
        try:
            cfg = yaml.safe_load(yml.read_text()) or {}
        except Exception:
            continue
        out.append((yml.stem, cfg.get("name") or yml.stem))
    return out


def find_game_config(slug: str) -> Path:
    exact = LUTRIS_GAMES / f"{slug}.yml"
    if exact.exists():
        return exact
    hits = [p for p in LUTRIS_GAMES.glob("*.yml") if p.stem.startswith(slug)]
    if len(hits) == 1:
        return hits[0]
    raise FileNotFoundError(f"no unique Lutris game config matching {slug!r} in {LUTRIS_GAMES}"
                            + (f" (candidates: {', '.join(h.stem for h in hits)})" if hits else ""))


def apply_lutris_env(slug: str, env: dict[str, str], remove: bool = False) -> tuple[Path, Path]:
    """Merge (or remove) env vars into a Lutris game config. Returns (config, backup)."""
    import yaml

    path = find_game_config(slug)
    backup = path.with_suffix(f".yml.padlink-{time.strftime('%Y%m%d-%H%M%S')}.bak")
    shutil.copy2(path, backup)
    cfg = yaml.safe_load(path.read_text()) or {}
    system = cfg.setdefault("system", {}) or {}
    cfg["system"] = system
    cur = system.get("env") or {}
    for k, v in env.items():
        if remove:
            cur.pop(k, None)
        else:
            cur[k] = v
    system["env"] = cur
    path.write_text(yaml.safe_dump(cfg, default_flow_style=False, sort_keys=False))
    return path, backup


def systemd_unit(spec: str, mode: str = "xbox360") -> str:
    """The unit runs the Rust `padlink` binary: the installed one if there is one."""
    from padlink.core import rsbridge

    installed = shutil.which("padlink")
    cmd = installed if installed and rsbridge._is_native(Path(installed)) else rsbridge.find_binary()
    if cmd is None:
        raise FileNotFoundError("padlink binary not found; run ./install.sh first")
    return f"""[Unit]
Description=padlink rumble bridge for {spec}
After=graphical-session.target

[Service]
ExecStart={cmd} bridge {spec} --as {mode} --wait --log
Restart=on-failure
RestartSec=3

[Install]
WantedBy=default.target
"""


def install_service(spec: str, mode: str = "xbox360") -> Path:
    SYSTEMD_USER.mkdir(parents=True, exist_ok=True)
    path = SYSTEMD_USER / "padlink-bridge.service"
    path.write_text(systemd_unit(spec, mode))
    return path
