//! Small persistent UI/user preferences (`~/.config/padlink/settings.json`).
//!
//! Keys this program doesn't know are kept on write, so the Python and Rust
//! implementations can share the file.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::{paths, py};

pub fn path() -> PathBuf {
    paths::config_dir().join("settings.json")
}

/// The whole file; empty when missing or unreadable.
pub fn load_from(path: &Path) -> Map<String, Value> {
    fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| match v {
            Value::Object(m) => Some(m),
            _ => None,
        })
        .unwrap_or_default()
}

/// Write one key. Errors are ignored like in Python: preferences are best effort.
pub fn set_in(path: &Path, key: &str, value: Value) {
    let mut data = load_from(path);
    data.insert(key.to_string(), value);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, py::json_dumps(&Value::Object(data), Some(2)) + "\n");
}

pub fn load() -> Map<String, Value> {
    load_from(&path())
}

pub fn get(key: &str) -> Option<Value> {
    load().remove(key)
}

pub fn set(key: &str, value: Value) {
    set_in(&path(), key, value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keeps_unknown_keys_and_order() {
        let dir = std::env::temp_dir().join(format!("padlink-settings-{}", std::process::id()));
        let p = dir.join("settings.json");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&p, "{\"bridge_persona\": \"ds4\", \"future\": [1]}").unwrap();
        set_in(&p, "input_view", json!("raw"));
        set_in(&p, "bridge_persona", json!("xbox360"));
        assert_eq!(
            fs::read_to_string(&p).unwrap(),
            "{\n  \"bridge_persona\": \"xbox360\",\n  \"future\": [\n    1\n  ],\n  \"input_view\": \"raw\"\n}\n"
        );
        assert_eq!(load_from(&dir.join("missing.json")), Map::new());
        fs::remove_dir_all(&dir).unwrap();
    }
}
