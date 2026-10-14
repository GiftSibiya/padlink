//! Diagnostics: why doesn't my controller rumble?

use std::collections::BTreeMap;
use std::ffi::CStr;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::discovery::{self, Controller};
use crate::{gamesetup, hiddesc, integration, profiles, py};

pub const OK: &str = "ok";
pub const INFO: &str = "info";
pub const WARN: &str = "warn";
pub const FAIL: &str = "fail";

pub const PADLINK_RULE: &str = "/etc/udev/rules.d/70-padlink.rules";
const CONFLICTING_PROCS: [&str; 7] =
    ["ds4drv", "xboxdrv", "antimicrox", "inputplumber", "sc-controller", "joycond", "steam"];

#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub id: String,
    pub title: String,
    pub status: &'static str,
    pub detail: String,
    pub fix: String,
    pub scope: String,
}

impl Check {
    fn new(
        id: &str,
        title: impl Into<String>,
        status: &'static str,
        detail: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        Check {
            id: id.into(),
            title: title.into(),
            status,
            detail: detail.into(),
            fix: fix.into(),
            scope: "system".into(),
        }
    }

    fn scoped(mut self, scope: &str) -> Self {
        self.scope = scope.into();
        self
    }

    pub fn to_value(&self) -> Value {
        json!({"id": self.id, "title": self.title, "status": self.status, "detail": self.detail, "fix": self.fix,
               "scope": self.scope})
    }
}

fn access_w(path: &str) -> bool {
    let c = std::ffi::CString::new(path).expect("path");
    // SAFETY: c is a valid NUL-terminated path.
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

fn procs() -> BTreeMap<u32, String> {
    let mut out = BTreeMap::new();
    if let Ok(it) = fs::read_dir("/proc") {
        for e in it.flatten() {
            if let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) {
                if let Ok(comm) = fs::read_to_string(e.path().join("comm")) {
                    out.insert(pid, comm.trim().to_string());
                }
            }
        }
    }
    out
}

fn kernel_release() -> String {
    // SAFETY: uname fills the struct we pass; release is NUL-terminated.
    let mut u: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut u) } != 0 {
        return String::new();
    }
    unsafe { CStr::from_ptr(u.release.as_ptr()) }.to_string_lossy().into_owned()
}

/// Is there a file named `prefix*` anywhere under `dir`? Symlinked dirs are not followed.
fn find_file(dir: &Path, prefix: &str) -> bool {
    let Ok(it) = fs::read_dir(dir) else { return false };
    for e in it.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        if e.file_name().to_string_lossy().starts_with(prefix) && !ft.is_dir() {
            return true;
        }
        if ft.is_dir() && find_file(&e.path(), prefix) {
            return true;
        }
    }
    false
}

/// An executable named `name` on PATH.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| {
        let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).ok();
        // SAFETY: c is a valid NUL-terminated path.
        p.is_file() && c.is_some_and(|c| unsafe { libc::access(c.as_ptr(), libc::X_OK) == 0 })
    })
}

pub fn system_checks() -> Vec<Check> {
    let mut checks = Vec::new();
    let uinput_ok = access_w("/dev/uinput");
    checks.push(Check::new(
        "uinput",
        "/dev/uinput writable (needed for the virtual pad)",
        if uinput_ok { OK } else { FAIL },
        if uinput_ok { "" } else { "padlink bridge cannot create a virtual controller" },
        if uinput_ok { "" } else { "sudo modprobe uinput; add a udev rule: KERNEL==\"uinput\", TAG+=\"uaccess\"" },
    ));

    let mut running: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for (pid, comm) in procs() {
        let base = comm.split('/').next().unwrap_or_default().to_string();
        if CONFLICTING_PROCS.contains(&base.as_str()) {
            running.entry(base).or_default().push(pid);
        }
    }
    if running.contains_key("steam") {
        checks.push(Check::new(
            "steam",
            "Steam is running",
            WARN,
            "Steam Input can open controllers (including generic HID pads) and re-expose them as its own virtual pad. \
             For Lutris games this can swallow rumble or duplicate controllers.",
            "Close Steam while testing, or in Steam: Settings > Controller > disable 'Enable Steam Input for Generic \
             controllers'.",
        ));
    }
    let others: Vec<&str> = running.keys().map(String::as_str).filter(|k| *k != "steam").collect();
    if !others.is_empty() {
        checks.push(Check::new(
            "remappers",
            "Other controller tools running",
            WARN,
            others.join(", "),
            "Stop them while debugging; they may grab the device.",
        ));
    }

    let leftovers: Vec<&str> = ["/etc/udev/rules.d/50-ds4drv.rules", "/etc/udev/rules.d/99-uinput-ds4-x360.rules"]
        .into_iter()
        .filter(|p| Path::new(p).exists())
        .collect();
    if !leftovers.is_empty() {
        checks.push(Check::new(
            "leftover_rules",
            "Leftover udev rules from other tools",
            INFO,
            leftovers.join(", "),
            "Harmless unless you see duplicate controllers. Remove if unused.",
        ));
    }

    let rule = Path::new(PADLINK_RULE).exists();
    checks.push(Check::new(
        "padlink_rule",
        "padlink udev rule installed",
        if rule { OK } else { INFO },
        if rule { PADLINK_RULE } else { "not installed (needed for hidraw rumble on HID-mode dongles)" },
        if rule {
            ""
        } else {
            "padlink udev-rule <dev> | sudo tee /etc/udev/rules.d/70-padlink.rules && sudo udevadm control --reload \
             && sudo udevadm trigger"
        },
    ));

    let xpad = find_file(&Path::new("/lib/modules").join(kernel_release()), "xpad.ko");
    checks.push(Check::new(
        "xpad",
        "xpad kernel module available",
        if xpad { OK } else { WARN },
        if xpad { "XInput-mode dongles (045e:028e) get native rumble from xpad" } else { "not found" },
        "",
    ));
    checks
}

pub fn device_checks(ctrl: &Controller) -> Vec<Check> {
    let scope = format!("{} [{}]", ctrl.name, ctrl.id_str());
    let mut checks = Vec::new();
    let mut add = |id: &str, title: String, status: &'static str, detail: String, fix: String| {
        checks.push(Check::new(id, title, status, detail, fix).scoped(&scope));
    };
    let s = |x: &str| x.to_string();

    if ctrl.is_padlink() {
        add("virtual", s("padlink virtual pad"), INFO, format!("created by padlink bridge ({})", ctrl.phys), s(""));
    } else if (ctrl.vendor, ctrl.product) == (0x3537, 0x1041) {
        add(
            "mode",
            s("GameSir/Zikway dongle in HID (Android/DInput) mode"),
            WARN,
            s("In this mode Linux uses hid-generic, which has no rumble support. Two fixes: switch the controller to \
               XInput/PC mode so it shows up as 045e:028e and xpad drives rumble natively (check the T4N Lite manual \
               for the mode button combo), or keep HID mode and use `padlink probe` + `padlink bridge`."),
            s(""),
        );
    } else if ctrl.vendor == 0x045E && ctrl.driver.as_deref() == Some("xpad") {
        add("mode", s("XInput device driven by xpad"), OK, s("the kernel provides rumble natively"), s(""));
    }
    if let Some(known) = ctrl.known_as() {
        add("identity", s("Recognised device"), INFO, s(known), s(""));
    }
    add("driver", s("Kernel driver"), INFO, ctrl.driver.clone().unwrap_or_else(|| s("unknown")), s(""));

    match ctrl.ff_node() {
        Some(node) => add("ff", s("Kernel force feedback (EV_FF)"), OK, node.ff_names().join(", "), s("")),
        None => add(
            "ff",
            s("Kernel force feedback (EV_FF)"),
            WARN,
            s("missing: games will see a controller without rumble"),
            s("use `padlink bridge` (needs a rumble profile from `padlink probe`)"),
        ),
    }

    if let Some(prim) = ctrl.primary_event() {
        let (r, w) = (prim.readable(), prim.writable());
        let st = if r && w { OK } else { FAIL };
        let yn = |b: bool| if b { "yes" } else { "no" };
        add(
            "event_perm",
            format!("Access to {}", prim.path),
            st,
            format!("read={} write={}", yn(r), yn(w)),
            if st == OK {
                s("")
            } else {
                format!("joystick nodes normally get uaccess ACLs from systemd; check `getfacl {}`", prim.path)
            },
        );
    }

    for node in &ctrl.hidraw {
        let raw = match node.descriptor() {
            Ok(raw) => raw,
            Err(e) => {
                add("hid_desc", format!("HID descriptor of {}", node.path), WARN, e.to_string(), s(""));
                continue;
            }
        };
        let desc = hiddesc::parse(&raw);
        let outs: Vec<String> = desc
            .by_kind(hiddesc::ReportKind::Output)
            .map(|r| format!("{:#04x}({}B)", r.report_id, r.payload_len()))
            .collect();
        let apps = desc.applications.join(", ");
        let mut detail = format!(
            "apps: {}; output reports: {}",
            if apps.is_empty() { "-" } else { &apps },
            if outs.is_empty() { "none".to_string() } else { outs.join(", ") }
        );
        if desc.has_pid() {
            detail.push_str("; has PID force-feedback page (hid-pidff could drive it if usbhid binds)");
        }
        let intf = node.interface.clone().unwrap_or_else(|| s("-"));
        add("hid_desc", format!("HID descriptor of {} (if {intf})", node.path), INFO, detail, s(""));
        let w = node.writable();
        add(
            "hid_perm",
            format!("Write access to {}", node.path),
            if w { OK } else { WARN },
            if w { s("") } else { s("needed to send rumble reports") },
            if w { s("") } else { s("install the padlink udev rule (see system checks)") },
        );
    }

    if !ctrl.is_padlink() {
        let prof = profiles::load(ctrl.vendor, ctrl.product, None).ok().flatten();
        match prof.as_ref().and_then(|p| p.rumble.as_ref().map(|r| (p, r))) {
            Some((p, r)) => add(
                "profile",
                s("Rumble profile"),
                OK,
                format!("{}: {}", p.path().display(), py::repr(&Value::Array(r.report.clone()))),
                s(""),
            ),
            None if !ctrl.has_ff() => {
                add("profile", s("Rumble profile"), WARN, s("none yet"), format!("padlink probe {}", ctrl.id_str()))
            }
            None => {}
        }
    }

    let held = discovery::holders(&ctrl.node_paths());
    if !held.is_empty() {
        let txt: Vec<String> = held.iter().map(|h| format!("{}({}): {}", h.comm, h.pid, h.nodes.join(", "))).collect();
        let steamish = held.iter().any(|h| h.comm.to_lowercase().contains("steam"));
        add(
            "holders",
            s("Processes with the device open"),
            if steamish { WARN } else { INFO },
            txt.join("; "),
            if steamish {
                s("Steam holding the device can intercept rumble; close it or disable Steam Input for it")
            } else {
                s("")
            },
        );
    }

    if let Some(b) = &ctrl.battery {
        let none = || s("None");
        add(
            "battery",
            s("Battery"),
            INFO,
            format!("{}% {}", b.capacity.clone().unwrap_or_else(none), b.status.clone().unwrap_or_else(none)),
            s(""),
        );
    }
    checks
}

/// The winebus registry values of a Wine prefix (None if it has no system.reg yet).
pub fn winebus_keys(prefix: &Path) -> Option<(PathBuf, Map<String, Value>)> {
    for reg in [prefix.join("system.reg"), prefix.join("pfx").join("system.reg")] {
        if !reg.exists() {
            continue;
        }
        let text = String::from_utf8_lossy(&fs::read(&reg).unwrap_or_default()).into_owned();
        return Some((reg, parse_winebus(&text)));
    }
    None
}

/// The `"name"=value` lines of the `[System\\ControlSet001\\Services\\winebus]` section.
pub fn parse_winebus(text: &str) -> Map<String, Value> {
    // Wine stores the key under ControlSet001 (CurrentControlSet is a link to it)
    const HEADERS: [&str; 2] =
        [r"[System\\ControlSet001\\Services\\winebus]", r"[System\\CurrentControlSet\\Services\\winebus]"];
    let mut keys = Map::new();
    let mut line_start = 0;
    let body = loop {
        if line_start > text.len() {
            return keys;
        }
        let rest = &text[line_start..];
        if HEADERS.iter().any(|h| rest.starts_with(h)) {
            // the rest of the header line, then the body up to a blank line or the end
            let Some(nl) = rest.find('\n') else { return keys };
            let body = &rest[nl + 1..];
            break &body[..body.find("\n\n").unwrap_or(body.len())];
        }
        match rest.find('\n') {
            Some(nl) => line_start += nl + 1,
            None => return keys,
        }
    };
    for line in body.lines() {
        let Some(after) = line.strip_prefix('"') else { continue };
        let Some(q) = after.find('"') else { continue };
        if q == 0 {
            continue;
        }
        if let Some(value) = after[q + 1..].strip_prefix('=') {
            keys.insert(after[..q].to_string(), json!(value));
        }
    }
    keys
}

pub fn lutris_checks(ctrl: Option<&Controller>, games_dir: &Path) -> Vec<Check> {
    let mut checks = Vec::new();
    if !games_dir.exists() {
        return checks;
    }
    let mut ymls: Vec<PathBuf> = fs::read_dir(games_dir)
        .map(|it| it.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "yml")).collect())
        .unwrap_or_default();
    ymls.sort();
    for yml in ymls {
        let stem = yml.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let cfg = match integration::read_game_config(&yml) {
            Ok(c) => c,
            Err(e) => {
                checks.push(
                    Check::new("lutris_cfg", stem.clone(), WARN, format!("unreadable: {e}"), "").scoped("lutris"),
                );
                continue;
            }
        };
        let get = |v: &Value, k: &str| v.get(k).filter(|x| py::truthy(x)).cloned().unwrap_or(json!({}));
        let game = get(&cfg, "game");
        let env = get(&get(&cfg, "system"), "env");
        let prefix = game.get("prefix").filter(|p| !p.is_null()).map(py::str_of);
        let mut parts = Vec::new();
        let mut hints: Vec<String> = Vec::new();
        let mut status = INFO;
        let mut fixes: Vec<String> = Vec::new();
        let pad = ctrl.filter(|c| !c.is_padlink());
        if let Some(c) = pad {
            let problems = gamesetup::env_problems(&cfg, c);
            if problems.is_empty() {
                parts.push("physical pad hidden from SDL and Proton (ok for bridge)".to_string());
            } else {
                parts.extend(problems);
                fixes.push(format!("padlink lutris-env {} --apply {stem}", c.id_str()));
                status = WARN;
            }
        }
        if let Some(prefix) = prefix.filter(|p| !p.is_empty()) {
            match winebus_keys(Path::new(&prefix)) {
                None => parts.push(format!("prefix {prefix}: no system.reg (not created yet?)")),
                Some((_, keys)) => {
                    // only the options that change how controllers reach the game
                    let opts: Vec<String> = WINEBUS_OPTIONS
                        .iter()
                        .filter_map(|k| keys.get(*k).map(|v| format!("{k}={}", dword(v))))
                        .collect();
                    let shown = if opts.is_empty() { "defaults".to_string() } else { opts.join(", ") };
                    parts.push(format!("winebus: {shown}"));
                    if keys.get("DisableInput").and_then(Value::as_str).is_some_and(|v| v.ends_with('1')) {
                        status = WARN;
                    }
                    if keys.get("Map Controllers").and_then(Value::as_str).is_none_or(|v| !v.ends_with('0')) {
                        parts.push(
                            "pads are XInput-mapped (Map Controllers on), so DirectInput-only games get no force \
                             feedback"
                                .to_string(),
                        );
                        if let Some(c) = pad {
                            hints.push(format!(
                                "if the game uses DirectInput (no rumble even through the bridge): padlink lutris-env \
                                 {} --apply {stem} --dinput",
                                c.id_str()
                            ));
                        }
                    }
                }
            }
        }
        if let Some(env) = env.as_object().filter(|e| !e.is_empty()) {
            let vars: Vec<String> = env.iter().map(|(k, v)| format!("{k}={}", py::str_of(v))).collect();
            parts.push(format!("env: {}", vars.join(", ")));
        }
        let name = cfg.get("name").filter(|n| py::truthy(n)).map_or_else(|| stem.clone(), py::str_of);
        // the --dinput hint rides along with the main fix, or stands alone
        let fix = match (fixes.first(), hints.first()) {
            (Some(f), Some(_)) => format!("{f} (add --dinput if the game uses DirectInput)"),
            (Some(f), None) => f.clone(),
            (None, Some(h)) => h.clone(),
            (None, None) => String::new(),
        };
        checks
            .push(Check::new("lutris_game", format!("Lutris: {name}"), status, parts.join("; "), fix).scoped("lutris"));
    }
    checks
}

/// winebus options (HKLM\\System\\CurrentControlSet\\Services\\winebus) that affect controllers.
const WINEBUS_OPTIONS: [&str; 5] = ["DisableHidraw", "DisableInput", "Enable SDL", "Map Controllers", "Enable Hidraw"];

/// `dword:00000001` -> `1`; other registry values unchanged.
fn dword(v: &Value) -> String {
    let s = py::str_of(v);
    match s.strip_prefix("dword:").and_then(|h| u32::from_str_radix(h, 16).ok()) {
        Some(n) => n.to_string(),
        None => s,
    }
}

/// Run a short command and return its stdout; None if it's missing, fails or takes over 3 s.
fn command_output(program: &str, args: &[&str]) -> Option<String> {
    use std::process::{Command, Stdio};
    let mut child = Command::new(program).args(args).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        match child.try_wait().ok()? {
            Some(status) if status.success() => break,
            Some(_) => return None,
            None if std::time::Instant::now() > deadline => {
                let _ = child.kill();
                return None;
            }
            None => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    Some(out)
}

/// A Bluetooth gamepad bluez reports as connected.
#[derive(Debug, Clone, PartialEq)]
pub struct BtPad {
    pub mac: String,
    pub name: String,
}

/// Connected gamepads (icon `input-gaming`) according to bluez; empty without bluetoothctl.
pub fn bluetooth_pads() -> Vec<BtPad> {
    let Some(list) = command_output("bluetoothctl", &["devices", "Connected"]) else { return Vec::new() };
    list.lines()
        .filter_map(|l| {
            let mut parts = l.splitn(3, ' ');
            (parts.next()? == "Device").then_some(())?;
            Some((parts.next()?.to_string(), parts.next().unwrap_or("").to_string()))
        })
        .filter(|(mac, _)| {
            command_output("bluetoothctl", &["info", mac])
                .is_some_and(|info| info.lines().any(|l| l.trim() == "Icon: input-gaming"))
        })
        .map(|(mac, name)| BtPad { mac, name })
        .collect()
}

/// Pads bluez calls connected but that have no input device: bluez dropped their HID
/// channel while the link stayed up (e.g. after "BT socket write error" on rumble output).
pub fn half_connected<'a>(pads: &'a [BtPad], controllers: &[Controller]) -> Vec<&'a BtPad> {
    pads.iter().filter(|p| !controllers.iter().any(|c| c.uniq.eq_ignore_ascii_case(&p.mac))).collect()
}

pub fn bluetooth_checks() -> Vec<Check> {
    let pads = bluetooth_pads();
    if pads.is_empty() {
        return Vec::new();
    }
    let controllers = discovery::scan(true).unwrap_or_default();
    half_connected(&pads, &controllers)
        .into_iter()
        .map(|p| {
            Check::new(
                "bt_half_connected",
                format!("{} is connected over Bluetooth but has no input device", p.name),
                FAIL,
                "bluez dropped its HID channel and kept the link up, so no program can see it (this happens when \
                 rumble output overflows the link: \"BT socket write error\" in `journalctl -u bluetooth`)",
                format!("bluetoothctl disconnect {}, then press the controller's home/PS button", p.mac),
            )
        })
        .collect()
}

/// Every check: system, then each controller (or just `ctrl`), then Lutris.
pub fn run(ctrl: Option<&Controller>, all_devices: bool) -> Vec<Check> {
    let mut checks = system_checks();
    checks.extend(bluetooth_checks());
    let controllers = discovery::scan(false).unwrap_or_default();
    if controllers.is_empty() {
        checks.push(Check::new(
            "none",
            "No game controllers detected",
            FAIL,
            "Is the dongle plugged in and the controller switched on?",
            "padlink list --all   # shows every HID device in case udev misclassifies it",
        ));
    }
    match ctrl {
        Some(c) => checks.extend(device_checks(c)),
        None if all_devices => controllers.iter().for_each(|c| checks.extend(device_checks(c))),
        None => {}
    }
    let lutris_ctrl = ctrl.or_else(|| controllers.iter().find(|c| !c.is_padlink()));
    checks.extend(lutris_checks(lutris_ctrl, &integration::lutris_games_dir()));
    if which("lutris").is_none() {
        checks.push(Check::new("lutris_bin", "lutris not on PATH", INFO, "", "").scoped("lutris"));
    }
    checks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ds4() -> Controller {
        Controller {
            key: "/sys/x".into(),
            name: "Wireless Controller".into(),
            vendor: 0x054C,
            product: 0x09CC,
            version: 0,
            bustype: 5,
            driver: Some("playstation".into()),
            uniq: String::new(),
            phys: String::new(),
            virtual_: false,
            events: vec![],
            joysticks: vec![],
            hidraw: vec![],
            battery: None,
            index: 0,
        }
    }

    /// (status, detail, fix) of the one Lutris check for a game with this env block.
    fn check_env(env: &str) -> (&'static str, String, String) {
        let dir = std::env::temp_dir().join(format!("padlink-doctor-{}-{}", std::process::id(), env.len()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("nfs.yml"), format!("game:\n  exe: /x.exe\nsystem:\n  env:\n{env}")).unwrap();
        let c = lutris_checks(Some(&ds4()), &dir).pop().unwrap();
        fs::remove_dir_all(&dir).unwrap();
        (c.status, c.detail, c.fix)
    }

    #[test]
    fn sdl_ignore_alone_is_enough_for_non_sony_pads_only() {
        let (status, detail, fix) = check_env("    SDL_GAMECONTROLLER_IGNORE_DEVICES: 0x054c/0x09cc\n");
        assert_eq!(status, WARN, "{detail}");
        assert!(detail.contains("through hidraw"), "{detail}");
        assert_eq!(fix, "padlink lutris-env 054c:09cc --apply nfs");
    }

    #[test]
    fn proton_sdl_mode_undoes_the_ignore_list() {
        let (status, detail, fix) = check_env(
            "    PROTON_PREFER_SDL: '1'\n    SDL_GAMECONTROLLER_IGNORE_DEVICES: 0x054c/0x09cc\n    PROTON_DISABLE_HIDRAW: 0x054C/0x09CC\n",
        );
        assert_eq!(status, WARN, "{detail}");
        assert!(detail.contains("PROTON_PREFER_SDL"), "{detail}");
        assert_eq!(fix, "padlink lutris-env 054c:09cc --apply nfs");
        // "0" means off in Proton
        let (status, _, _) = check_env(
            "    PROTON_PREFER_SDL: '0'\n    SDL_GAMECONTROLLER_IGNORE_DEVICES: 0x054c/0x09cc\n    PROTON_DISABLE_HIDRAW: 0x054C/0x09CC\n",
        );
        assert_eq!(status, INFO);
    }

    #[test]
    fn half_connected_pads() {
        let pads = vec![
            BtPad { mac: "DA:71:77:4C:5F:95".into(), name: "Wireless Controller".into() },
            BtPad { mac: "AA:BB:CC:DD:EE:FF".into(), name: "Other Pad".into() },
        ];
        let mut c = ds4();
        c.uniq = "da:71:77:4c:5f:95".into();
        let missing = half_connected(&pads, &[c]);
        assert_eq!(missing, vec![&pads[1]]);
        assert_eq!(half_connected(&pads, &[]).len(), 2);
    }

    #[test]
    fn hidden_everywhere_is_ok() {
        for hidraw in ["0x054C/0x09CC", "1", "0x045E/0x028E,0x054c/0x09cc"] {
            let (status, detail, fix) = check_env(&format!(
                "    SDL_GAMECONTROLLER_IGNORE_DEVICES: 0x054c/0x09cc\n    PROTON_DISABLE_HIDRAW: '{hidraw}'\n"
            ));
            assert_eq!((status, fix.as_str()), (INFO, ""), "{hidraw}: {detail}");
            assert!(detail.contains("ok for bridge"), "{detail}");
        }
    }
}

#[cfg(test)]
mod bt_live {
    /// Needs bluez with a gamepad connected: `cargo test -p padlink-core bt_live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn lists_connected_pads() {
        let pads = super::bluetooth_pads();
        println!("connected pads: {pads:?}");
        assert!(!pads.is_empty());
    }
}
