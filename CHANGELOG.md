# Changelog

## [0.1.0] - 2026-10-14

First release. padlink detects, debugs and logs game controllers on Linux, and **bridges rumble**
to controllers whose kernel driver can't vibrate them. Everything is written in Rust (the original
Python version was ported and removed).

### Rumble bridge
- Virtual controller with force feedback that games use instead of the physical pad. Rumble goes to
  the real controller through a probed hidraw report, or its kernel force feedback.
- Personas: appear as an Xbox 360 / One S / Series, DualShock 4, DualSense or Switch Pro
  controller, or mirror the real one.
- Rumble writes to Bluetooth controllers are capped at 30/s and changes under 2% are skipped.
  Faster bursts made bluez drop the controller mid-game.
- Logs a hint when a game opens the virtual pad but never sends force feedback (the usual
  DirectInput-under-Wine case).
- `--wait` re-bridges after reconnects; `padlink service install` keeps it running as a systemd user service.

### Games under Lutris / Wine / Proton
- `padlink lutris-env <dev> --apply <game>` sets a game up in one step. It hides the physical pad
  from SDL and from Proton's hidraw path, removes `PROTON_PREFER_SDL`, and moves
  `WINEDLLOVERRIDES` / `WINEDEBUG` to where Lutris reads them. Backups are kept.
- `--dinput` turns off winebus "Map Controllers" in the game's prefix, so DirectInput games
  (e.g. NFS Most Wanted 2005) get force feedback.
- Verified with NFS Most Wanted on GE-Proton and a DualShock 4 bridged as an Xbox 360 pad.

### Tools
- `list`, `info` (parsed HID descriptors), `monitor` (evdev or raw hidraw), `rumble`, `hid-send`,
  `hid-feature`, `probe` (finds a controller's rumble report), `map` (input-map wizard), `capture`
  (usbmon), `art`, `logs`.
- `doctor`: system, device, Bluetooth and Lutris/Wine checks with fixes. It flags Steam holding the
  pad, Proton settings that undo hiding, and controllers bluez lists as connected but has no input
  device for.

### GTK app
- Device, Input (live drawing or raw values, custom SVG artwork), Rumble, Probe, Bridge (with
  per-game Lutris setup and a DirectInput switch), Doctor and Logs tabs, with hotplug.
- Needs GTK 4.12+ and libadwaita 1.4+ (Ubuntu 24.04, Debian 13, Arch).
