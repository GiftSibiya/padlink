# padlink

A companion app for wireless game controllers on Linux. It detects controllers, diagnoses them, logs what they do, and **bridges rumble** for controllers whose kernel driver can't vibrate them.

Its first target is the **GameSir T4N Lite** on its Zikway 2.4G dongle (`3537:1041`). In HID mode, Linux binds `hid-generic`, which has no force feedback. Games in Lutris/Wine/Proton therefore see a gamepad with no rumble, and their rumble requests are dropped.

## How the fix works

```
 game (Wine/Proton/SDL)                    padlink bridge                      real controller
 ───────────────────────     uinput FF     ─────────────────────   hidraw      ────────────────
 sees "Xbox 360 pad"  ── upload/play ──▶   effect table + mixer ── output ──▶  dongle motors
 (045e:028e, FF_RUMBLE) ◀─ input events ── input map (translate) ◀── evdev ──  (grabbed)
```

1. `padlink probe` finds which HID output report makes the controller vibrate. It saves the result as a profile.
2. `padlink bridge` creates a virtual Xbox 360 pad with force feedback. It grabs the physical pad and forwards its input. Rumble the game sends to the virtual pad becomes the profile's output report.
3. `padlink lutris-env` hides the physical pad from SDL so games only use the virtual pad.

Every step writes JSONL logs, so you can study what a game sends and how the controller responds later.

## Install

```sh
./install.sh            # wrappers in ~/.local/bin + desktop entry, uses system python
```

Arch needs these packages: `python-evdev python-pyudev python-yaml python-gobject gtk4 libadwaita`.

Rumble on the HID-mode dongle needs write access to its hidraw node:

```sh
sudo install -m644 data/70-padlink.rules /etc/udev/rules.d/
sudo udevadm control --reload && sudo udevadm trigger      # then replug the dongle
```

For other devices, generate a rule with `padlink udev-rule <vid:pid>`.

## GameSir T4N Lite quick start

```sh
padlink list                          # is it there? which driver? kernel rumble?
padlink doctor 3537:1041              # every check, with fixes
padlink info 3537:1041                # parsed HID descriptor: output reports = rumble candidates
padlink probe 3537:1041               # interactive: "did it vibrate?" -> saves a profile
padlink rumble 3537:1041              # test the profile
padlink map 3537:1041                 # optional: teach the button/axis layout
padlink bridge 3537:1041 --log -v     # run the bridge and watch the game's FF requests
padlink lutris-env 3537:1041 --apply need-for-speed-most-wanted   # hide the physical pad in that game
```

Then launch the game from Lutris with the bridge running. To start the bridge automatically:

```sh
padlink service install 3537:1041
systemctl --user daemon-reload && systemctl --user enable --now padlink-bridge
```

> **Try XInput mode first.** Many GameSir dongles have a PC/XInput mode in which the dongle shows up as `045e:028e`. In that mode the kernel's `xpad` driver handles rumble natively and no bridge is needed. `padlink doctor` tells you which mode the controller is in. The button combo for switching is in the T4N Lite manual.

## GUI

Run `padlink-gtk`. The sidebar lists controllers and updates live on hotplug. It has these tabs:

| Tab | What it does |
|---|---|
| Device | Identity, driver, nodes and permissions, battery, FF capabilities, parsed HID descriptors |
| Input | Switch between **Drawing** and **Raw values**. Drawing is a controller illustration (Xbox, PlayStation or Nintendo layout) where pressed buttons light up, sticks move and triggers fill. It shows input through the bridge's input map, so it's also how games will see the pad. Raw evdev values and HID reports are below it. Can log to JSONL |
| Rumble | Strength and duration test. Uses kernel FF or the hidraw profile |
| Probe | The probe wizard (stage 1: find the report, stage 2: find each motor's byte) |
| Bridge | Choose which controller the virtual pad pretends to be, start and stop the bridge, see FF traffic from games live, apply Lutris env vars, install autostart |
| Doctor | All checks with copyable fixes |
| Logs | Browse session logs and filter by kind |

## CLI reference

| Command | Purpose |
|---|---|
| `list [--all] [--json]` | Controllers. `--all` also lists HID devices udev didn't flag as joysticks |
| `info <dev> [--json]` | Full dump, including raw and parsed report descriptors |
| `monitor <dev> [--raw]` | Live evdev events, or raw hidraw reports with changed bytes highlighted |
| `rumble <dev> [--strong % --weak % --ms N]` | Rumble test |
| `hid-send <dev> "05 ff 00"` | Write a raw output report. The first byte is the report ID, or 00 if the device uses none |
| `hid-feature get/set <dev> ...` | Read or write feature reports |
| `probe <dev> [--blind]` | Rumble discovery wizard |
| `map <dev>` | Input-map wizard for the bridge |
| `bridge <dev> [--as PERSONA] [--wait] [--log] [-v]` | The rumble bridge. `--as` picks which controller to spoof |
| `personas` | List the controllers the bridge can spoof |
| `doctor [<dev>] [--json] [--log]` | Diagnostics |
| `lutris-env <dev> [--apply SLUG] [--remove]` | Hide the physical pad in Lutris game configs. A backup is kept |
| `udev-rule <dev>` | Print a hidraw access rule |
| `service show/install <vid:pid> [--as PERSONA]` | systemd user unit |
| `logs list/show/export/path` | Session logs |
| `capture <dev>` (root) | usbmon capture of the dongle's USB traffic |

`<dev>` can be an index from `list`, `vid:pid`, a `/dev/...` node, or part of the name.

## Spoofing other controllers (personas)

Some games only accept certain controllers: Xbox only, or PlayStation prompts only. The bridge's virtual pad can impersonate:

| `--as` | Appears as | IDs |
|---|---|---|
| `xbox360` (default) | Microsoft X-Box 360 pad | `045e:028e` |
| `xboxone` | Microsoft X-Box One S pad | `045e:02ea` |
| `xboxseries` | Microsoft Xbox Series S\|X Controller | `045e:0b12` |
| `ds4` | Sony DualShock 4 (USB) | `054c:09cc` |
| `dualsense` | Sony DualSense (USB) | `054c:0ce6` |
| `switchpro` | Nintendo Switch Pro Controller | `057e:2009` |
| `mirror` | A copy of the real controller | same as source |

A persona copies more than the name and IDs. It also matches how that controller's Linux driver behaves:
- **Button numbering:** Xbox drivers put X on `0x133`. The Sony and Nintendo drivers follow the kernel gamepad spec, where `0x133` is the top button.
- **Axis ranges:** for example, DS4 sticks run 0..255 and Xbox One triggers 0..1023.
- **Triggers:** analog only, analog plus a click like hid-playstation, or digital only like Switch Pro.

The version numbers are the ones the real drivers report, so SDL selects its built-in mapping for that controller. Rumble works with every persona.

Pick a persona whose IDs differ from your physical controller's. Otherwise the variable that hides the physical controller from games would hide the virtual pad too.

```sh
padlink bridge 3537:1041 --as ds4      # GameSir shows up to games as a DualShock 4
```

## Profiles

Profiles live in `~/.config/padlink/profiles/<vid>_<pid>.json`:

```json
{
  "name": "GameSir T4N Lite",
  "vendor": "3537", "product": "1041",
  "rumble": {
    "transport": "hidraw", "interface": "0",
    "report": [5, 0, "strong", "weak", 0, 0, 0, 0],
    "stop": null, "scale": 255, "keepalive_ms": 0, "min_interval_ms": 8, "swap": false
  },
  "input_map": {
    "keys": {"BTN_SOUTH": "BTN_A"},
    "axes": {"ABS_Z": {"to": "ABS_RX", "invert": false}},
    "key_to_axis": {"BTN_TL2": "ABS_Z"},
    "dpad_buttons": false
  }
}
```

The `report` template accepts these tokens: `strong`, `weak` (scaled to `0..scale`), `strong_hi`/`strong_lo`/`weak_hi`/`weak_lo` (16-bit split), `max`, and `any` (`scale` while either motor is on). Set `keepalive_ms` for controllers that stop vibrating after a timeout.

## Logs

Logs are JSONL files in `~/.local/state/padlink/logs/`. The newest 100 are kept. Each line is `{"t": unix_time, "kind": ..., ...}`:

| kind | content |
|---|---|
| `session` | First line: version, command, full device snapshot |
| `input` | evdev event (`node, type, code, value, name`) |
| `hid_rx` / `hid_tx` | Raw HID report hex. `ok`/`error` on writes |
| `ff_upload` / `ff_erase` / `ff_play` / `ff_gain` | Force-feedback requests from games, through the virtual pad |
| `rumble_out` | Mixed motor levels actually sent to the controller (0..65535) |
| `diag` | Doctor results |
| `usb` | usbmon capture records |
| `note` | Free text and state changes |

Run `padlink logs export` to bundle the logs and profiles into a tarball for sharing.

## Reverse-engineering notes

- Probe stage 1 sends each declared output report filled with `0xFF`, then known gamepad layouts that fit the report's length: Xbox One BT, Xbox 360, DragonRise/GreenAsia, SmartJoy, DS4 and simple two-byte formats. Probing never sends feature reports.
- Probe stage 2 tests one byte at a time to learn which byte drives which motor, which allows variable strength.
- If no candidate vibrates, the controller probably needs a vendor command first, or rumbles only in another mode. To find out, pass the dongle through to a Windows VM where the official app (or any game) can make it rumble, and capture the traffic on the host:
  ```sh
  sudo modprobe usbmon
  sudo -E padlink capture 3537:1041      # make it rumble, then look for interrupt OUT reports
  ```
  Replay a suspect report with `padlink hid-send`, then put it in the profile.

## Development

```sh
python3 -m unittest -v      # unit tests + a live bridge test using a fake uinput "dongle"
```

The live test creates a fake source pad and runs the bridge. It checks input translation and has a separate "game" process upload and play a rumble effect, which must arrive as `ff_upload`/`ff_play`/`rumble_out`. The test needs `/dev/uinput` access and skips itself otherwise.

Code layout: `padlink/core/` holds the logic and has no GTK dependency. `padlink/cli.py` is the command line, and `padlink/gtk/` is the libadwaita UI.

## Contributing

Issues and pull requests are welcome, especially rumble profiles and probe results for controllers other than the T4N Lite. `padlink logs export` bundles your logs and profiles so you can attach them to an issue.

## License

[MIT](LICENSE)
