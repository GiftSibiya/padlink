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
/// (Proton hands Sony pads to games through hidraw, which the SDL list doesn't cover.)
pub fn lutris_env(ctrl: &Controller) -> Vec<(String, String)> {
    let id = format!("0x{:04x}/0x{:04x}", ctrl.vendor, ctrl.product);
    vec![
        ("SDL_GAMECONTROLLER_IGNORE_DEVICES".into(), id.clone()),
        ("PROTON_DISABLE_HIDRAW".into(), id.to_uppercase().replace("0X", "0x")),
    ]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_games() {
        let dir = std::env::temp_dir().join(format!("padlink-lutris-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("nfs-123.yml");
        fs::write(&cfg, "game:\n  exe: /x.exe\nsystem:\n  env:\n    FOO: bar\n").unwrap();
        fs::write(dir.join("other-9.yml"), "name: Other Game\n").unwrap();
        assert_eq!(find_game_config(&dir, "nfs").unwrap(), cfg);
        assert_eq!(read_game_config(&cfg).unwrap()["system"]["env"]["FOO"], "bar");
        assert_eq!(
            lutris_games(&dir),
            vec![("nfs-123".to_string(), "nfs-123".to_string()), ("other-9".to_string(), "Other Game".to_string())]
        );
        assert!(find_game_config(&dir, "zzz").is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
