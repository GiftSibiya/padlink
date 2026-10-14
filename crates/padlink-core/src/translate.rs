//! Input maps: physical controller events -> canonical Xbox 360 (xpad) events.
//!
//! An input map is JSON (it lives in profiles, see docs/padlink/formats.md):
//! `{"keys": {...}, "axes": {...}, "key_to_axis": {...}, "dpad_buttons": bool}`
//! with evdev code names. `heuristic_map()` guesses one from a device's
//! capabilities; `Translator` applies one.

use std::collections::{BTreeSet, HashMap};

use serde_json::{Map, Value, json};

use crate::codes::*;
use crate::personas::{HAT, XPAD_STICK, XPAD_TRIGGER};
use crate::py;

/// The ranges of the canonical (xpad) axes.
pub fn xpad_abs(code: u16) -> Option<AbsInfo> {
    match code {
        ABS_X | ABS_Y | ABS_RX | ABS_RY => Some(XPAD_STICK),
        ABS_Z | ABS_RZ => Some(XPAD_TRIGGER),
        ABS_HAT0X | ABS_HAT0Y => Some(HAT),
        _ => None,
    }
}

/// D-pad buttons -> (hat axis, direction).
pub const DPAD_BUTTONS: [(u16, (u16, i32)); 4] = [
    (BTN_DPAD_LEFT, (ABS_HAT0X, -1)),
    (BTN_DPAD_RIGHT, (ABS_HAT0X, 1)),
    (BTN_DPAD_UP, (ABS_HAT0Y, -1)),
    (BTN_DPAD_DOWN, (ABS_HAT0Y, 1)),
];

/// Drivers that follow Documentation/input/gamepad.rst: BTN_NORTH (0x133) is the
/// *top* face button and BTN_WEST (0x134) the left one. xpad (and SDL's mapping
/// for 045e:028e) and Android-layout HID dongles use 0x133 for the *left* (X) button.
pub const SPEC_LAYOUT_DRIVERS: [&str; 6] =
    ["playstation", "hid-playstation", "sony", "hid-sony", "nintendo", "hid-nintendo"];

/// Guess an xpad mapping from the source's evdev capabilities.
///
/// Matches the Linux gamepad convention and the Android HID layout most
/// 2.4G dongles use (buttons 1..15 -> BTN_SOUTH.., right stick on Z/RZ,
/// analog triggers on BRAKE/GAS).
pub fn heuristic_map(keys: &BTreeSet<u16>, abses: &BTreeSet<u16>, driver: Option<&str>) -> Value {
    let mut map_keys = Map::new();
    let mut axes = Map::new();
    let mut key_to_axis = Map::new();
    let spec = SPEC_LAYOUT_DRIVERS.contains(&driver.unwrap_or(""));
    let gamepad_keys: [(u16, &str); 11] = [
        (BTN_A, "BTN_A"),
        (BTN_B, "BTN_B"),
        (BTN_X, if spec { "BTN_Y" } else { "BTN_X" }),
        (BTN_Y, if spec { "BTN_X" } else { "BTN_Y" }),
        (BTN_TL, "BTN_TL"),
        (BTN_TR, "BTN_TR"),
        (BTN_SELECT, "BTN_SELECT"),
        (BTN_START, "BTN_START"),
        (BTN_MODE, "BTN_MODE"),
        (BTN_THUMBL, "BTN_THUMBL"),
        (BTN_THUMBR, "BTN_THUMBR"),
    ];
    if keys.range(0x130..0x13F).next().is_some() {
        for (code, target) in gamepad_keys {
            if keys.contains(&code) {
                map_keys.insert(code_name(EV_KEY, code), json!(target));
            }
        }
    } else {
        // Generic joystick buttons (BTN_TRIGGER..). Common DirectInput order.
        let order = [
            "BTN_X",
            "BTN_A",
            "BTN_B",
            "BTN_Y",
            "BTN_TL",
            "BTN_TR",
            "@ABS_Z",
            "@ABS_RZ",
            "BTN_SELECT",
            "BTN_START",
            "BTN_THUMBL",
            "BTN_THUMBR",
            "BTN_MODE",
        ];
        let src = keys.iter().filter(|&&k| (0x120..0x130).contains(&k) || (0x2C0..0x2E8).contains(&k));
        for (&code, target) in src.zip(order) {
            match target.strip_prefix('@') {
                Some(axis) => key_to_axis.insert(code_name(EV_KEY, code), json!(axis)),
                None => map_keys.insert(code_name(EV_KEY, code), json!(target)),
            };
        }
    }

    let mut axis = |src: u16, dst: &str| {
        if abses.contains(&src) {
            axes.insert(code_name(EV_ABS, src), json!({"to": dst, "invert": false}));
        }
    };
    axis(ABS_X, "ABS_X");
    axis(ABS_Y, "ABS_Y");
    if abses.contains(&ABS_RX) && abses.contains(&ABS_RY) {
        axis(ABS_RX, "ABS_RX");
        axis(ABS_RY, "ABS_RY");
        axis(ABS_Z, "ABS_Z");
        axis(ABS_RZ, "ABS_RZ");
    } else {
        axis(ABS_Z, "ABS_RX");
        axis(ABS_RZ, "ABS_RY");
    }
    axis(ABS_BRAKE, "ABS_Z");
    axis(ABS_GAS, "ABS_RZ");
    axis(ABS_HAT0X, "ABS_HAT0X");
    axis(ABS_HAT0Y, "ABS_HAT0Y");

    let has_trigger_axes = axes.values().any(|v| matches!(v["to"].as_str(), Some("ABS_Z" | "ABS_RZ")));
    if !has_trigger_axes {
        if keys.contains(&BTN_TL2) {
            key_to_axis.insert("BTN_TL2".into(), json!("ABS_Z"));
        }
        if keys.contains(&BTN_TR2) {
            key_to_axis.insert("BTN_TR2".into(), json!("ABS_RZ"));
        }
    }
    let dpad = DPAD_BUTTONS.iter().any(|(k, _)| keys.contains(k));
    json!({"keys": map_keys, "axes": axes, "key_to_axis": key_to_axis, "dpad_buttons": dpad})
}

fn code16(name: &str) -> Result<u16, String> {
    code_of(name).and_then(|c| u16::try_from(c).ok()).ok_or_else(|| format!("unknown evdev code '{name}'"))
}

/// The canonical axis a mapping targets; must be one of the xpad axes.
fn xpad_target(name: &str) -> Result<(u16, AbsInfo), String> {
    let code = code16(name)?;
    xpad_abs(code).map(|info| (code, info)).ok_or_else(|| format!("{name} is not an Xbox 360 axis"))
}

#[derive(Debug, Clone)]
struct AxisMap {
    dst: u16,
    dst_info: AbsInfo,
    invert: bool,
    src_info: Option<AbsInfo>,
}

/// Applies an input map to evdev events. Pure logic; the d-pad buttons keep state.
#[derive(Debug, Clone)]
pub struct Translator {
    keys: HashMap<u16, u16>,
    key_to_axis: HashMap<u16, (u16, AbsInfo)>,
    axes: HashMap<u16, AxisMap>,
    dpad: bool,
    /// [negative, positive] pressed, per hat axis
    hat_x: [i32; 2],
    hat_y: [i32; 2],
}

impl Translator {
    /// Python raises on an unknown code name at construction and on a non-xpad
    /// target axis at the first event; both are rejected here, up front.
    pub fn new(mapping: &Value, absinfo: &HashMap<u16, AbsInfo>) -> Result<Self, String> {
        let section = |name: &str| mapping.get(name).and_then(Value::as_object).cloned().unwrap_or_default();
        let mut keys = HashMap::new();
        for (k, v) in section("keys") {
            keys.insert(code16(&k)?, code16(v.as_str().ok_or("key targets must be names")?)?);
        }
        let mut key_to_axis = HashMap::new();
        for (k, v) in section("key_to_axis") {
            key_to_axis.insert(code16(&k)?, xpad_target(v.as_str().ok_or("axis targets must be names")?)?);
        }
        let mut axes = HashMap::new();
        for (src, spec) in section("axes") {
            let scode = code16(&src)?;
            let (dst, dst_info) = xpad_target(spec.get("to").and_then(Value::as_str).ok_or("axis map needs \"to\"")?)?;
            let invert = spec.get("invert").is_some_and(py::truthy);
            axes.insert(scode, AxisMap { dst, dst_info, invert, src_info: absinfo.get(&scode).copied() });
        }
        let dpad = mapping.get("dpad_buttons").is_some_and(py::truthy);
        Ok(Translator { keys, key_to_axis, axes, dpad, hat_x: [0, 0], hat_y: [0, 0] })
    }

    pub fn scale(value: i32, src: Option<AbsInfo>, dst: AbsInfo, invert: bool) -> i32 {
        let out = match src {
            Some(src) if src.max != src.min => {
                let mut frac = (value as i64 - src.min as i64) as f64 / (src.max as i64 - src.min as i64) as f64;
                if invert {
                    frac = 1.0 - frac;
                }
                py::round(dst.min as f64 + frac * (dst.max as i64 - dst.min as i64) as f64)
            }
            _ => value as i64,
        };
        out.clamp(dst.min as i64, dst.max as i64) as i32
    }

    pub fn translate(&mut self, etype: u16, code: u16, value: i32) -> Vec<Event> {
        if etype == EV_KEY {
            if let Some(&dst) = self.keys.get(&code) {
                return vec![(EV_KEY, dst, value)];
            }
            if let Some(&(dst, info)) = self.key_to_axis.get(&code) {
                return vec![(EV_ABS, dst, if value != 0 { info.max } else { 0 })];
            }
            if self.dpad {
                if let Some(&(_, (axis, direction))) = DPAD_BUTTONS.iter().find(|(k, _)| *k == code) {
                    let state = if axis == ABS_HAT0X { &mut self.hat_x } else { &mut self.hat_y };
                    state[usize::from(direction > 0)] = i32::from(value != 0);
                    return vec![(EV_ABS, axis, state[1] - state[0])];
                }
            }
            return vec![];
        }
        if etype == EV_ABS {
            if let Some(a) = self.axes.get(&code) {
                if a.dst == ABS_HAT0X || a.dst == ABS_HAT0Y {
                    return vec![(EV_ABS, a.dst, value.clamp(-1, 1))];
                }
                return vec![(EV_ABS, a.dst, Self::scale(value, a.src_info, a.dst_info, a.invert))];
            }
        }
        vec![]
    }
}
