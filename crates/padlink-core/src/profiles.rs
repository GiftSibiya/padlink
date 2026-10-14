//! Per-device profiles: how to make a device rumble, and how to map its input.
//!
//! Stored as JSON in `~/.config/padlink/profiles/<vid>_<pid>.json`; the format
//! is documented in docs/padlink/formats.md. Files written here and by the
//! Python implementation are interchangeable.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::{paths, py};

pub const TOKENS: [&str; 8] = ["strong", "weak", "strong_hi", "strong_lo", "weak_hi", "weak_lo", "max", "any"];

/// How to turn (strong, weak) motor levels into a device's output report.
///
/// `report` and `stop` keep their JSON items as written (numbers and template
/// tokens) so a profile round-trips unchanged.
#[derive(Debug, Clone, PartialEq)]
pub struct RumbleSpec {
    pub transport: String,
    /// USB interface number of the hidraw node as sysfs prints it (`"00"`); None = first node.
    pub interface: Option<String>,
    pub report: Vec<Value>,
    pub stop: Option<Vec<Value>>,
    pub scale: i64,
    pub keepalive_ms: i64,
    pub min_interval_ms: i64,
    pub swap: bool,
}

impl Default for RumbleSpec {
    fn default() -> Self {
        RumbleSpec {
            transport: "hidraw".into(),
            interface: None,
            report: Vec::new(),
            stop: None,
            scale: 255,
            keepalive_ms: 0,
            min_interval_ms: 8,
            swap: false,
        }
    }
}

fn byte_of(item: &Value) -> Result<u8, String> {
    // Python's int(item) & 0xFF: numbers truncate, bools count as 0/1
    let n = match item {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f.trunc() as i64)),
        Value::Bool(b) => Some(*b as i64),
        _ => None,
    };
    n.map(|n| (n & 0xFF) as u8).ok_or_else(|| format!("invalid report byte {item}"))
}

impl RumbleSpec {
    /// Build the output report for 16-bit magnitudes (0..65535).
    pub fn render(&self, strong: u16, weak: u16) -> Result<Vec<u8>, String> {
        let (strong, weak) = if self.swap { (weak, strong) } else { (strong, weak) };
        if strong == 0 && weak == 0 {
            if let Some(stop) = self.stop.as_ref().filter(|s| !s.is_empty()) {
                return stop.iter().map(byte_of).collect();
            }
        }
        let s = py::round((strong as i64 * self.scale) as f64 / 65535.0);
        let w = py::round((weak as i64 * self.scale) as f64 / 65535.0);
        let token = |name: &str| -> Option<i64> {
            Some(match name {
                "strong" => s,
                "weak" => w,
                "strong_hi" => s >> 8,
                "strong_lo" => s & 0xFF,
                "weak_hi" => w >> 8,
                "weak_lo" => w & 0xFF,
                "max" => s.max(w),
                "any" => {
                    if s != 0 || w != 0 {
                        self.scale
                    } else {
                        0
                    }
                }
                _ => return None,
            })
        };
        self.report
            .iter()
            .map(|item| match item {
                Value::String(name) => {
                    token(name).map(|v| (v & 0xFF) as u8).ok_or_else(|| format!("unknown template token '{name}'"))
                }
                other => byte_of(other),
            })
            .collect()
    }

    pub fn to_value(&self) -> Value {
        json!({
            "transport": self.transport,
            "interface": self.interface,
            "report": self.report,
            "stop": self.stop,
            "scale": self.scale,
            "keepalive_ms": self.keepalive_ms,
            "min_interval_ms": self.min_interval_ms,
            "swap": self.swap,
        })
    }

    /// Missing keys take their defaults. Unknown keys are ignored (Python rejects them).
    pub fn from_value(v: &Value) -> Result<Self, String> {
        let obj = v.as_object().ok_or("rumble must be an object")?;
        let d = RumbleSpec::default();
        let int = |key: &str, default: i64| -> Result<i64, String> {
            match obj.get(key) {
                None => Ok(default),
                Some(v) => v.as_i64().ok_or_else(|| format!("rumble.{key} must be an integer")),
            }
        };
        let list = |key: &str| -> Result<Option<Vec<Value>>, String> {
            match obj.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::Array(a)) => Ok(Some(a.clone())),
                Some(_) => Err(format!("rumble.{key} must be a list")),
            }
        };
        Ok(RumbleSpec {
            transport: match obj.get("transport") {
                None => d.transport,
                Some(v) => v.as_str().ok_or("rumble.transport must be a string")?.to_string(),
            },
            interface: match obj.get("interface") {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) => Some(s.clone()),
                Some(_) => return Err("rumble.interface must be a string like \"00\" or null".into()),
            },
            report: list("report")?.unwrap_or_default(),
            stop: list("stop")?,
            scale: int("scale", d.scale)?,
            keepalive_ms: int("keepalive_ms", d.keepalive_ms)?,
            min_interval_ms: int("min_interval_ms", d.min_interval_ms)?,
            swap: match obj.get("swap") {
                None => d.swap,
                Some(v) => py::truthy(v),
            },
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub vendor: u16,
    pub product: u16,
    pub name: String,
    pub rumble: Option<RumbleSpec>,
    /// Kept as JSON, like Python; `translate::InputMap` interprets it.
    pub input_map: Option<Value>,
    pub notes: String,
}

/// `int(str(value), 16)` for the vendor/product fields.
fn parse_id(v: Option<&Value>, key: &str) -> Result<u16, String> {
    let text = match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => return Err(format!("profile has no {key}")),
    };
    let t = text.trim();
    let t = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")).unwrap_or(t);
    u16::from_str_radix(&t.replace('_', ""), 16).map_err(|_| format!("bad {key} id {text:?}"))
}

impl Profile {
    pub fn new(vendor: u16, product: u16) -> Self {
        Profile { vendor, product, name: String::new(), rumble: None, input_map: None, notes: String::new() }
    }

    pub fn to_value(&self) -> Value {
        json!({
            "name": self.name,
            "vendor": format!("{:04x}", self.vendor),
            "product": format!("{:04x}", self.product),
            "rumble": self.rumble.as_ref().map(RumbleSpec::to_value),
            "input_map": self.input_map,
            "notes": self.notes,
        })
    }

    pub fn from_value(data: &Value) -> Result<Self, String> {
        let obj: &Map<String, Value> = data.as_object().ok_or("profile must be a JSON object")?;
        let text = |key: &str| obj.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
        Ok(Profile {
            vendor: parse_id(obj.get("vendor"), "vendor")?,
            product: parse_id(obj.get("product"), "product")?,
            name: text("name"),
            rumble: match obj.get("rumble") {
                Some(v) if py::truthy(v) => Some(RumbleSpec::from_value(v)?),
                _ => None,
            },
            input_map: obj.get("input_map").filter(|v| !v.is_null()).cloned(),
            notes: text("notes"),
        })
    }

    /// The file contents Python's `Profile.save()` writes.
    pub fn to_json(&self) -> String {
        py::json_dumps(&self.to_value(), Some(2)) + "\n"
    }

    pub fn path(&self) -> PathBuf {
        profile_path(self.vendor, self.product, None)
    }

    pub fn save(&self, dir: Option<&Path>) -> io::Result<PathBuf> {
        let path = profile_path(self.vendor, self.product, dir);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, self.to_json())?;
        Ok(path)
    }
}

pub fn profile_path(vendor: u16, product: u16, dir: Option<&Path>) -> PathBuf {
    let dir = dir.map_or_else(paths::profile_dir, Path::to_path_buf);
    dir.join(format!("{vendor:04x}_{product:04x}.json"))
}

/// The saved profile for vendor:product, if there is one.
pub fn load(vendor: u16, product: u16, dir: Option<&Path>) -> io::Result<Option<Profile>> {
    let path = profile_path(vendor, product, dir);
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let invalid = |e: String| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e}", path.display()));
    let value: Value = serde_json::from_str(&text).map_err(|e| invalid(e.to_string()))?;
    Profile::from_value(&value).map(Some).map_err(invalid)
}

pub fn load_or_new(vendor: u16, product: u16, name: &str) -> io::Result<Profile> {
    Ok(load(vendor, product, None)?
        .unwrap_or_else(|| Profile { name: name.to_string(), ..Profile::new(vendor, product) }))
}
