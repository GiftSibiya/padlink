//! Glue to the rest of the system: udev rules, Lutris game configs, systemd.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::discovery::Controller;

/// A udev rule giving the logged-in user write access to the controller's hidraw nodes.
pub fn udev_rule(ctrl: &Controller) -> String {
    let (v, p) = (format!("{:04x}", ctrl.vendor), format!("{:04x}", ctrl.product));
    format!(
        "# padlink: let the logged-in user send rumble reports to {name} ({v}:{p})\n\
         KERNEL==\"hidraw*\", SUBSYSTEM==\"hidraw\", ATTRS{{idVendor}}==\"{v}\", ATTRS{{idProduct}}==\"{p}\", TAG+=\"uaccess\"\n\
         KERNEL==\"hidraw*\", SUBSYSTEM==\"hidraw\", KERNELS==\"*:{vu}:{pu}.*\", TAG+=\"uaccess\"\n\
         # virtual pads created by padlink are plain uinput joysticks and get uaccess from systemd already\n",
        name = ctrl.name,
        vu = v.to_uppercase(),
        pu = p.to_uppercase(),
    )
}

/// Env vars so games see only padlink's virtual pad, not the physical one.
pub fn lutris_env(ctrl: &Controller) -> Vec<(String, String)> {
    vec![("SDL_GAMECONTROLLER_IGNORE_DEVICES".into(), format!("0x{:04x}/0x{:04x}", ctrl.vendor, ctrl.product))]
}

/// A systemd --user unit that keeps the bridge running. `exe` is the padlink binary.
pub fn systemd_unit(exe: &str, spec: &str, mode: &str) -> String {
    format!(
        "[Unit]\nDescription=padlink rumble bridge for {spec}\nAfter=graphical-session.target\n\n\
         [Service]\nExecStart={exe} bridge {spec} --as {mode} --wait --log\nRestart=on-failure\nRestartSec=3\n\n\
         [Install]\nWantedBy=default.target\n"
    )
}

pub fn systemd_user_dir() -> PathBuf {
    std::env::home_dir().unwrap_or_default().join(".config/systemd/user")
}

pub fn install_service(exe: &str, spec: &str, mode: &str) -> io::Result<PathBuf> {
    let dir = systemd_user_dir();
    fs::create_dir_all(&dir)?;
    let path = dir.join("padlink-bridge.service");
    fs::write(&path, systemd_unit(exe, spec, mode))?;
    Ok(path)
}

pub fn lutris_games_dir() -> PathBuf {
    std::env::home_dir().unwrap_or_default().join(".local/share/lutris/games")
}

/// A Lutris game config as JSON (YAML maps to the same value types).
pub fn read_game_config(path: &Path) -> Result<serde_json::Value, String> {
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    if text.trim().is_empty() {
        return Ok(serde_json::Value::Object(Default::default()));
    }
    let value: serde_json::Value = serde_norway::from_str(&text).map_err(|e| e.to_string())?;
    Ok(if value.is_null() { serde_json::Value::Object(Default::default()) } else { value })
}

fn yml_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .map(|it| it.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "yml")).collect())
        .unwrap_or_default();
    files.sort();
    files
}

fn stem(p: &Path) -> String {
    p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// (slug, name) of every Lutris game config that parses.
pub fn lutris_games(dir: &Path) -> Vec<(String, String)> {
    yml_files(dir)
        .into_iter()
        .filter_map(|p| {
            let cfg = read_game_config(&p).ok()?;
            let slug = stem(&p);
            let name = cfg.get("name").and_then(|n| n.as_str()).filter(|n| !n.is_empty()).unwrap_or(&slug).to_string();
            Some((slug, name))
        })
        .collect()
}

/// The config for a slug, or a unique slug prefix.
pub fn find_game_config(dir: &Path, slug: &str) -> io::Result<PathBuf> {
    let exact = dir.join(format!("{slug}.yml"));
    if exact.exists() {
        return Ok(exact);
    }
    let hits: Vec<PathBuf> = yml_files(dir).into_iter().filter(|p| stem(p).starts_with(slug)).collect();
    if hits.len() == 1 {
        return Ok(hits.into_iter().next().expect("one hit"));
    }
    let candidates = if hits.is_empty() {
        String::new()
    } else {
        format!(" (candidates: {})", hits.iter().map(|p| stem(p)).collect::<Vec<_>>().join(", "))
    };
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("no unique Lutris game config matching '{slug}' in {}{candidates}", dir.display()),
    ))
}

/// Merge (or remove) env vars into a Lutris game config, keeping a timestamped
/// backup. Returns (config, backup).
pub fn apply_lutris_env(
    dir: &Path,
    slug: &str,
    env: &[(String, String)],
    remove: bool,
) -> io::Result<(PathBuf, PathBuf)> {
    use serde_norway::{Mapping, Value};
    let path = find_game_config(dir, slug)?;
    let stamp = crate::logger::file_stamp(crate::logger::now());
    let backup = path.with_extension(format!("yml.padlink-{stamp}.bak"));
    fs::copy(&path, &backup)?;
    if let Ok(modified) = fs::metadata(&path).and_then(|m| m.modified()) {
        let _ = fs::File::options().write(true).open(&backup).and_then(|f| f.set_modified(modified));
    }
    let invalid =
        |e: serde_norway::Error| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e}", path.display()));
    let text = fs::read_to_string(&path)?;
    let mut cfg: Value =
        if text.trim().is_empty() { Value::Null } else { serde_norway::from_str(&text).map_err(invalid)? };
    if !cfg.is_mapping() {
        cfg = Value::Mapping(Mapping::new());
    }
    let root = cfg.as_mapping_mut().expect("mapping");
    let system = root.entry("system".into()).or_insert(Value::Null);
    if !system.is_mapping() {
        *system = Value::Mapping(Mapping::new());
    }
    let system = system.as_mapping_mut().expect("mapping");
    let cur = system.entry("env".into()).or_insert(Value::Null);
    if !cur.is_mapping() {
        *cur = Value::Mapping(Mapping::new());
    }
    let cur = cur.as_mapping_mut().expect("mapping");
    for (k, v) in env {
        if remove {
            cur.remove(k.as_str());
        } else {
            cur.insert(k.clone().into(), v.clone().into());
        }
    }
    fs::write(&path, serde_norway::to_string(&cfg).map_err(invalid)?)?;
    Ok((path, backup))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_and_remove() {
        let dir = std::env::temp_dir().join(format!("padlink-lutris-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("nfs-123.yml");
        fs::write(&cfg, "game:\n  exe: /x.exe\nsystem:\n  env:\n    FOO: bar\n").unwrap();
        let var = || vec![("SDL_GAMECONTROLLER_IGNORE_DEVICES".to_string(), "0x3537/0x1041".to_string())];
        let (path, backup) = apply_lutris_env(&dir, "nfs", &var(), false).unwrap();
        assert_eq!(path, cfg);
        assert!(backup.exists());
        let v = read_game_config(&cfg).unwrap();
        assert_eq!(
            v["system"]["env"],
            serde_json::json!({"FOO": "bar", "SDL_GAMECONTROLLER_IGNORE_DEVICES": "0x3537/0x1041"})
        );
        apply_lutris_env(&dir, "nfs-123", &var(), true).unwrap();
        let v = read_game_config(&cfg).unwrap();
        assert_eq!(v["system"]["env"], serde_json::json!({"FOO": "bar"}));
        assert_eq!(v["game"]["exe"], "/x.exe");
        assert_eq!(lutris_games(&dir), vec![("nfs-123".to_string(), "nfs-123".to_string())]);
        assert!(find_game_config(&dir, "zzz").is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
