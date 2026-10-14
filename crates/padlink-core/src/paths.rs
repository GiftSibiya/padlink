//! XDG locations used by padlink.

use std::env;
use std::path::PathBuf;

fn xdg(var: &str, default: &str) -> PathBuf {
    match env::var_os(var).filter(|v| !v.is_empty()) {
        Some(v) => PathBuf::from(v),
        None => home().join(default),
    }
}

fn home() -> PathBuf {
    env::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// `$XDG_CONFIG_HOME/padlink` (default `~/.config/padlink`).
pub fn config_dir() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config").join("padlink")
}

/// `$XDG_STATE_HOME/padlink` (default `~/.local/state/padlink`).
pub fn state_dir() -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state").join("padlink")
}

pub fn profile_dir() -> PathBuf {
    config_dir().join("profiles")
}

pub fn log_dir() -> PathBuf {
    state_dir().join("logs")
}
