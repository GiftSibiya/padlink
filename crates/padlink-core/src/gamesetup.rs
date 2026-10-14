//! Get a Lutris/Wine game ready for the bridge, in one step.
//!
//! What a game needs so it sees only padlink's virtual pad, and gets its rumble
//! (found getting NFS Most Wanted to rumble through a DualShock 4; the recipe is
//! in docs/padlink/setup.md):
//! - the physical pad hidden from SDL: `SDL_GAMECONTROLLER_IGNORE_DEVICES`
//! - and from Proton's hidraw path: `PROTON_DISABLE_HIDRAW` (Proton hands Sony pads
//!   to games through hidraw; the SDL list doesn't cover that)
//! - no `PROTON_PREFER_SDL`/`PROTON_USE_SDL`: in SDL mode Proton deletes the SDL list
//! - `WINEDLLOVERRIDES`/`WINEDEBUG` where Lutris reads them (`wine: overrides:` /
//!   `wine: show_debug: inherit`); Lutris overwrites both env vars
//! - for DirectInput games, winebus "Map Controllers" off in the prefix: otherwise Wine
//!   presents every pad as an XInput controller, which has no DirectInput force feedback

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value as Json;
use serde_norway::{Mapping, Value};

use crate::discovery::Controller;
use crate::{integration, logger, py};

const SDL_IGNORE: &str = "SDL_GAMECONTROLLER_IGNORE_DEVICES";
const NO_HIDRAW: &str = "PROTON_DISABLE_HIDRAW";
const SDL_MODE: [&str; 2] = ["PROTON_PREFER_SDL", "PROTON_USE_SDL"];

/// `0x054c/0x09cc`, as SDL writes it.
pub fn pad_id(ctrl: &Controller) -> String {
    format!("0x{:04x}/0x{:04x}", ctrl.vendor, ctrl.product)
}

/// Proton's `nonzero()`: set to anything but empty or "0".
fn proton_nonzero(v: &str) -> bool {
    !v.is_empty() && v != "0"
}

fn in_list(list: &str, id: &str) -> bool {
    list.split(',').any(|item| item.trim().eq_ignore_ascii_case(id))
}

/// What keeps a game from seeing only the virtual pad, given its config (as JSON).
pub fn env_problems(cfg: &Json, ctrl: &Controller) -> Vec<String> {
    let env = cfg.get("system").and_then(|s| s.get("env")).cloned().unwrap_or(Json::Null);
    let var = |k: &str| env.get(k).filter(|v| !v.is_null()).map(py::str_of);
    let id = pad_id(ctrl);
    let mut out = Vec::new();
    if !var(SDL_IGNORE).is_some_and(|v| in_list(&v, &id)) {
        out.push("physical pad NOT hidden from SDL (game may pick the rumble-less device)".to_string());
    }
    if SDL_MODE.iter().any(|k| var(k).is_some_and(|v| proton_nonzero(&v))) {
        out.push(
            "PROTON_PREFER_SDL/PROTON_USE_SDL is set: Proton (umu) then drops SDL_GAMECONTROLLER_IGNORE_DEVICES, so \
             the physical pad is NOT hidden"
                .to_string(),
        );
    }
    let hidraw_off = var(NO_HIDRAW).is_some_and(|v| v.trim() == "1" || in_list(&v, &id));
    if ctrl.vendor == 0x054C && !hidraw_off {
        out.push("Proton (umu) still passes Sony pads to the game through hidraw (PROTON_DISABLE_HIDRAW)".to_string());
    }
    // Lutris always rebuilds WINEDLLOVERRIDES, and WINEDEBUG unless wine: show_debug is "inherit"
    let inherit_debug = cfg.get("wine").and_then(|w| w.get("show_debug")).is_some_and(|v| v == "inherit");
    if var("WINEDLLOVERRIDES").is_some() {
        out.push("WINEDLLOVERRIDES in the env is overwritten by Lutris; it belongs in wine: overrides:".to_string());
    }
    if var("WINEDEBUG").is_some() && !inherit_debug {
        out.push("WINEDEBUG in the env is overwritten by Lutris unless wine: show_debug is inherit".to_string());
    }
    out
}

/// The game's prefix registry with the winebus options (`pfx/system.reg` for Proton prefixes).
pub fn system_reg(prefix: &Path) -> Option<PathBuf> {
    [prefix.join("pfx/system.reg"), prefix.join("system.reg")].into_iter().find(|p| p.is_file())
}

/// The game's Wine prefix from its config.
pub fn game_prefix(cfg: &Json) -> Option<PathBuf> {
    cfg.get("game")?.get("prefix").filter(|p| !p.is_null()).map(py::str_of).filter(|p| !p.is_empty()).map(PathBuf::from)
}

/// winebus "Map Controllers": Some(true) on (the default), Some(false) off, None without a registry yet.
pub fn map_controllers(prefix: &Path) -> Option<bool> {
    let reg = system_reg(prefix)?;
    let text = String::from_utf8_lossy(&fs::read(reg).ok()?).into_owned();
    let keys = crate::doctor::parse_winebus(&text);
    Some(keys.get("Map Controllers").and_then(Json::as_str).is_none_or(|v| !v.ends_with('0')))
}

/// Processes running in this prefix (Wine keeps the registry in memory and rewrites
/// it on exit, so it must not be edited then).
pub fn prefix_processes(prefix: &Path) -> Vec<(u32, String)> {
    let want =
        [prefix.to_path_buf(), prefix.join("pfx")].map(|p| p.to_string_lossy().trim_end_matches('/').to_string());
    let mut out = Vec::new();
    let Ok(procs) = fs::read_dir("/proc") else { return out };
    for e in procs.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
        let Ok(env) = fs::read(e.path().join("environ")) else { continue };
        let in_prefix = env.split(|&b| b == 0).filter_map(|kv| kv.strip_prefix(b"WINEPREFIX=")).any(|v| {
            let v = String::from_utf8_lossy(v);
            want.iter().any(|w| v.trim_end_matches('/') == w)
        });
        if in_prefix {
            let comm = fs::read_to_string(e.path().join("comm")).map(|s| s.trim().to_string()).unwrap_or_default();
            out.push((pid, comm));
        }
    }
    out
}

fn backup(path: &Path, ext: &str) -> io::Result<PathBuf> {
    let stamp = logger::file_stamp(logger::now());
    let backup = path.with_extension(format!("{ext}.padlink-{stamp}.bak"));
    fs::copy(path, &backup)?;
    if let Ok(modified) = fs::metadata(path).and_then(|m| m.modified()) {
        let _ = fs::File::options().write(true).open(&backup).and_then(|f| f.set_modified(modified));
    }
    Ok(backup)
}

/// Set (`Some`) or remove (`None`, back to Wine's default) a dword option in the
/// winebus section of a prefix's system.reg. Returns the backup's path.
pub fn set_winebus_dword(prefix: &Path, name: &str, value: Option<u32>) -> io::Result<PathBuf> {
    let reg = system_reg(prefix).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("{}: no system.reg (run the game once first)", prefix.display()),
        )
    })?;
    let running = prefix_processes(prefix);
    if !running.is_empty() {
        let names: Vec<String> = running.iter().take(4).map(|(pid, c)| format!("{c}({pid})")).collect();
        return Err(io::Error::new(
            io::ErrorKind::ResourceBusy,
            format!("Wine is running in this prefix ({}): close the game first", names.join(", ")),
        ));
    }
    let bytes = fs::read(&reg)?;
    let mut lines: Vec<Vec<u8>> = bytes.split(|&b| b == b'\n').map(<[u8]>::to_vec).collect();
    let headers: [&[u8]; 2] =
        [br"[System\\ControlSet001\\Services\\winebus]", br"[System\\CurrentControlSet\\Services\\winebus]"];
    let key = format!("\"{name}\"=");
    let new_line = value.map(|v| format!("{key}dword:{v:08x}").into_bytes());
    match lines.iter().position(|l| headers.iter().any(|h| l.starts_with(h))) {
        Some(start) => {
            let end = (start + 1..lines.len()).find(|&i| lines[i].is_empty()).unwrap_or(lines.len());
            let existing = (start + 1..end).find(|&i| lines[i].starts_with(key.as_bytes()));
            match (existing, new_line) {
                (Some(i), Some(l)) => lines[i] = l,
                (Some(i), None) => {
                    lines.remove(i);
                }
                (None, Some(l)) => lines.insert(end, l),
                (None, None) => {}
            }
        }
        None => {
            if let Some(l) = new_line {
                // Wine fills in the rest of the service key itself
                let header = format!("[System\\\\ControlSet001\\\\Services\\\\winebus] {}", logger::now() as u64);
                if lines.last().is_some_and(|l| l.is_empty()) {
                    lines.pop();
                }
                lines.extend([Vec::new(), header.into_bytes(), l, Vec::new()]);
            }
        }
    }
    let backup = backup(&reg, "reg")?;
    fs::write(&reg, lines.join(&b'\n'))?;
    Ok(backup)
}

/// Turn winebus "Map Controllers" off (DirectInput games get force feedback) or back to the default.
pub fn set_map_controllers(prefix: &Path, on: bool) -> io::Result<PathBuf> {
    set_winebus_dword(prefix, "Map Controllers", if on { None } else { Some(0) })
}

/// What `setup`/`undo` did.
#[derive(Debug, Default)]
pub struct Outcome {
    pub config: PathBuf,
    pub backup: Option<PathBuf>,
    pub registry_backup: Option<PathBuf>,
    pub changes: Vec<String>,
}

fn mapping<'a>(parent: &'a mut Mapping, key: &str) -> &'a mut Mapping {
    let v = parent.entry(key.into()).or_insert(Value::Null);
    if !v.is_mapping() {
        *v = Value::Mapping(Mapping::new());
    }
    v.as_mapping_mut().expect("mapping")
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// Add `id` to a comma list (`None` = unset), unless it is there already.
fn list_add(list: Option<String>, id: &str) -> Option<String> {
    match list.filter(|l| !l.trim().is_empty()) {
        Some(l) if in_list(&l, id) => None,
        Some(l) => Some(format!("{l},{id}")),
        None => Some(id.to_string()),
    }
}

fn list_remove(list: &str, id: &str) -> String {
    list.split(',')
        .map(str::trim)
        .filter(|i| !i.is_empty() && !i.eq_ignore_ascii_case(id))
        .collect::<Vec<_>>()
        .join(",")
}

fn load(path: &Path) -> io::Result<Value> {
    let invalid =
        |e: serde_norway::Error| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e}", path.display()));
    let src = fs::read_to_string(path)?;
    let cfg: Value = if src.trim().is_empty() { Value::Null } else { serde_norway::from_str(&src).map_err(invalid)? };
    Ok(if cfg.is_mapping() { cfg } else { Value::Mapping(Mapping::new()) })
}

fn save(path: &Path, cfg: &Value) -> io::Result<()> {
    let out = serde_norway::to_string(cfg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    fs::write(path, out)
}

/// Fix everything `env_problems` reports, plus winebus "Map Controllers" when `dinput`
/// is given. Writes nothing (and keeps no backup) if the config is already right.
pub fn setup(dir: &Path, slug: &str, ctrl: &Controller, dinput: Option<bool>) -> io::Result<Outcome> {
    let path = integration::find_game_config(dir, slug)?;
    let mut cfg = load(&path)?;
    let mut changes = Vec::new();
    let root = cfg.as_mapping_mut().expect("mapping");
    let id = pad_id(ctrl);
    {
        let env = mapping(mapping(root, "system"), "env");
        let get = |env: &Mapping, k: &str| env.get(k).map(text);
        if let Some(v) = list_add(get(env, SDL_IGNORE), &id) {
            changes.push(format!("{SDL_IGNORE}={v}"));
            env.insert(SDL_IGNORE.into(), v.into());
        }
        let current = get(env, NO_HIDRAW);
        if current.as_deref().map(str::trim) != Some("1") {
            // harmless for pads Proton doesn't use hidraw for; required for Sony ones
            if let Some(v) = list_add(current, &id.to_uppercase().replace("0X", "0x")) {
                changes.push(format!("{NO_HIDRAW}={v}"));
                env.insert(NO_HIDRAW.into(), v.into());
            }
        }
        for k in SDL_MODE {
            if env.remove(k).is_some() {
                changes.push(format!("removed {k} (it made Proton drop {SDL_IGNORE})"));
            }
        }
    }
    // Lutris overwrites these env vars from its wine options: move them there
    let moved_overrides = mapping(mapping(root, "system"), "env").remove("WINEDLLOVERRIDES");
    if let Some(v) = moved_overrides {
        let overrides = mapping(mapping(root, "wine"), "overrides");
        for part in text(&v).split(';').filter(|p| !p.trim().is_empty()) {
            let (dlls, mode) = part.split_once('=').unwrap_or((part, ""));
            for dll in dlls.split(',').map(str::trim).filter(|d| !d.is_empty()) {
                if !overrides.contains_key(dll) {
                    overrides.insert(dll.into(), mode.trim().into());
                }
            }
        }
        changes.push(format!("moved WINEDLLOVERRIDES={} to wine: overrides:", text(&v)));
    }
    if mapping(mapping(root, "system"), "env").contains_key("WINEDEBUG") {
        let wine = mapping(root, "wine");
        if wine.get("show_debug").map(text).as_deref() != Some("inherit") {
            wine.insert("show_debug".into(), "inherit".into());
            changes.push("wine: show_debug: inherit (so the env WINEDEBUG takes effect)".into());
        }
    }

    let mut outcome = Outcome { config: path.clone(), ..Outcome::default() };
    if !changes.is_empty() {
        outcome.backup = Some(backup(&path, "yml")?);
        save(&path, &cfg)?;
    }
    if let Some(dinput) = dinput {
        let json = integration::read_game_config(&path).map_err(io::Error::other)?;
        let prefix = game_prefix(&json)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the game config has no wine prefix"))?;
        let want_on = !dinput;
        if map_controllers(&prefix) != Some(want_on) {
            outcome.registry_backup = Some(set_map_controllers(&prefix, want_on)?);
            changes.push(if dinput {
                "winebus \"Map Controllers\"=0 (DirectInput games get force feedback)".into()
            } else {
                "winebus \"Map Controllers\" back to Wine's default (XInput mapping)".into()
            });
        }
    }
    outcome.changes = changes;
    Ok(outcome)
}

/// Undo `setup` for this pad: unhide it, and with `restore_dinput` put "Map Controllers" back.
pub fn undo(dir: &Path, slug: &str, ctrl: &Controller, restore_dinput: bool) -> io::Result<Outcome> {
    let path = integration::find_game_config(dir, slug)?;
    let mut cfg = load(&path)?;
    let mut changes = Vec::new();
    let id = pad_id(ctrl);
    {
        let env = mapping(mapping(cfg.as_mapping_mut().expect("mapping"), "system"), "env");
        for k in [SDL_IGNORE, NO_HIDRAW] {
            let Some(cur) = env.get(k).map(text) else { continue };
            if !in_list(&cur, &id) {
                continue;
            }
            let rest = list_remove(&cur, &id);
            if rest.is_empty() {
                env.remove(k);
                changes.push(format!("removed {k}"));
            } else {
                changes.push(format!("{k}={rest}"));
                env.insert(k.into(), rest.into());
            }
        }
    }
    let mut outcome = Outcome { config: path.clone(), ..Outcome::default() };
    if !changes.is_empty() {
        outcome.backup = Some(backup(&path, "yml")?);
        save(&path, &cfg)?;
    }
    if restore_dinput {
        let json = integration::read_game_config(&path).map_err(io::Error::other)?;
        if let Some(prefix) = game_prefix(&json).filter(|p| map_controllers(p) == Some(false)) {
            outcome.registry_backup = Some(set_map_controllers(&prefix, true)?);
            changes.push("winebus \"Map Controllers\" back to Wine's default".into());
        }
    }
    outcome.changes = changes;
    Ok(outcome)
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

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("padlink-gamesetup-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// The NFS MW config as it was before the fix, with a fake prefix.
    fn nfs(dir: &Path) -> PathBuf {
        let prefix = dir.join("prefix");
        fs::create_dir_all(prefix.join("pfx")).unwrap();
        fs::write(
            prefix.join("pfx/system.reg"),
            "WINE REGISTRY Version 2\n\n[System\\\\ControlSet001\\\\Services\\\\winebus] 1790702630\n#time=1dd\n\
             \"DisableHidraw\"=dword:00000000\n\"Start\"=dword:00000003\n\n[System\\\\ControlSet001\\\\Services\\\\winehid] 1\n\
             \"DisplayName\"=\"Wine HID\"\n",
        )
        .unwrap();
        let cfg = dir.join("nfs-1.yml");
        fs::write(
            &cfg,
            format!(
                "game:\n  exe: /x/speed.exe\n  prefix: {}\nwine:\n  dxvk: true\nsystem:\n  env:\n    \
                 PROTON_PREFER_SDL: '1'\n    WINEDLLOVERRIDES: dinput8=n,b;xinput1_3,xinput9_1_0=\n    \
                 WINEDEBUG: +dinput\n    SDL_GAMECONTROLLER_IGNORE_DEVICES: 0x045e/0x0b12\n",
                prefix.display()
            ),
        )
        .unwrap();
        cfg
    }

    #[test]
    fn setup_applies_the_whole_recipe_and_is_idempotent() {
        let dir = tmpdir("setup");
        let cfg_path = nfs(&dir);
        let before = integration::read_game_config(&cfg_path).unwrap();
        assert_eq!(env_problems(&before, &ds4()).len(), 5, "{:?}", env_problems(&before, &ds4()));

        let out = setup(&dir, "nfs", &ds4(), Some(true)).unwrap();
        assert!(out.backup.is_some() && out.registry_backup.is_some(), "{out:?}");
        let cfg = integration::read_game_config(&cfg_path).unwrap();
        assert_eq!(env_problems(&cfg, &ds4()), Vec::<String>::new());
        let env = &cfg["system"]["env"];
        assert_eq!(env["SDL_GAMECONTROLLER_IGNORE_DEVICES"], "0x045e/0x0b12,0x054c/0x09cc");
        assert_eq!(env["PROTON_DISABLE_HIDRAW"], "0x054C/0x09CC");
        assert!(env.get("PROTON_PREFER_SDL").is_none() && env.get("WINEDLLOVERRIDES").is_none());
        assert_eq!(cfg["wine"]["overrides"], serde_json::json!({"dinput8": "n,b", "xinput1_3": "", "xinput9_1_0": ""}));
        assert_eq!(cfg["wine"]["show_debug"], "inherit");
        assert_eq!(cfg["wine"]["dxvk"], true);
        let prefix = dir.join("prefix");
        assert_eq!(map_controllers(&prefix), Some(false));
        let reg = fs::read_to_string(prefix.join("pfx/system.reg")).unwrap();
        assert!(reg.contains("\"Start\"=dword:00000003\n\"Map Controllers\"=dword:00000000\n\n[System"), "{reg}");

        // second run: nothing to do, no new backups
        let again = setup(&dir, "nfs", &ds4(), Some(true)).unwrap();
        assert!(again.changes.is_empty() && again.backup.is_none() && again.registry_backup.is_none(), "{again:?}");

        let undone = undo(&dir, "nfs", &ds4(), true).unwrap();
        let cfg = integration::read_game_config(&cfg_path).unwrap();
        assert_eq!(cfg["system"]["env"]["SDL_GAMECONTROLLER_IGNORE_DEVICES"], "0x045e/0x0b12");
        assert!(cfg["system"]["env"].get("PROTON_DISABLE_HIDRAW").is_none(), "{undone:?}");
        assert_eq!(map_controllers(&prefix), Some(true));
        assert!(!fs::read_to_string(prefix.join("pfx/system.reg")).unwrap().contains("Map Controllers"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn registry_section_is_created_when_missing() {
        let dir = tmpdir("reg");
        fs::write(dir.join("system.reg"), "WINE REGISTRY Version 2\n\n[Software\\\\Wine] 1\n\"Version\"=\"win10\"\n")
            .unwrap();
        set_map_controllers(&dir, false).unwrap();
        assert_eq!(map_controllers(&dir), Some(false));
        assert!(
            fs::read_to_string(dir.join("system.reg")).unwrap().starts_with("WINE REGISTRY Version 2\n\n[Software")
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn lists() {
        assert!(in_list("0x045e/0x028e, 0x054C/0x09CC", "0x054c/0x09cc"));
        assert_eq!(list_add(Some("0x054c/0x09cc".into()), "0x054C/0x09CC"), None);
        assert_eq!(list_remove("a,0x054c/0x09cc,b", "0x054C/0x09CC"), "a,b");
    }
}
