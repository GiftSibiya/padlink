//! Virtual pad personas: what the bridge's uinput device pretends to be.
//!
//! The bridge translates the physical controller into canonical xpad events
//! (Xbox 360 codes and ranges). A persona then re-encodes those events the way
//! the *real* Linux driver for that controller would emit them: same name,
//! vendor/product/version, button codes, axis ranges, trigger and d-pad style.
//! That way SDL, Wine and games pick their built-in mapping for that controller.
//!
//! Version numbers follow what the kernel drivers report over USB (these end up
//! in SDL's joystick GUID and select its controller mapping).

use crate::codes::*;
use crate::py;

pub const XPAD_STICK: AbsInfo = AbsInfo::new(0, -32768, 32767, 16, 128, 0);
pub const XPAD_TRIGGER: AbsInfo = AbsInfo::new(0, 0, 255, 0, 0, 0);
pub const HAT: AbsInfo = AbsInfo::new(0, -1, 1, 0, 0, 0);

pub const STICK_AXES: [u16; 4] = [ABS_X, ABS_Y, ABS_RX, ABS_RY];
/// Analog trigger axis -> the digital button some drivers also emit.
pub const TRIGGER_AXES: [(u16, u16); 2] = [(ABS_Z, BTN_TL2), (ABS_RZ, BTN_TR2)];
pub const DIGITAL_TRIGGER_THRESHOLD: i32 = 64; // of 255

pub const COMMON_KEYS: [u16; 7] = [BTN_TL, BTN_TR, BTN_SELECT, BTN_START, BTN_MODE, BTN_THUMBL, BTN_THUMBR];

/// Canonical (xpad) face keys: 0x133 = BTN_X = left face button, 0x134 = BTN_Y = top.
const X_LEFT: u16 = 0x133;
const Y_TOP: u16 = 0x134;

#[derive(Debug, Clone, PartialEq)]
pub struct Persona {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub name: &'static str,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
    pub bustype: u16,
    /// Canonical face key -> persona key.
    pub face: &'static [(u16, u16)],
    pub stick: AbsInfo,
    /// None: digital triggers only.
    pub trigger: Option<AbsInfo>,
    /// Also emit BTN_TL2/BTN_TR2 (hid-playstation does).
    pub trigger_buttons: bool,
    /// Follows Documentation/input/gamepad.rst (BTN_NORTH = top).
    pub spec_layout: bool,
}

/// What the persona's uinput device advertises.
#[derive(Debug, Clone, PartialEq)]
pub struct Capabilities {
    pub keys: Vec<u16>,
    pub abs: Vec<(u16, AbsInfo)>,
}

impl Persona {
    pub fn id_str(&self) -> String {
        format!("{:04x}:{:04x}", self.vendor, self.product)
    }

    fn digital_triggers(&self) -> bool {
        self.trigger.is_none() || self.trigger_buttons
    }

    pub fn capabilities(&self) -> Capabilities {
        let mut keys: Vec<u16> = self.face.iter().map(|&(_, k)| k).chain(COMMON_KEYS).collect();
        if self.digital_triggers() {
            keys.extend([BTN_TL2, BTN_TR2]);
        }
        keys.sort_unstable();
        keys.dedup();
        let mut abs: Vec<(u16, AbsInfo)> = STICK_AXES.iter().map(|&a| (a, self.stick)).collect();
        if let Some(t) = self.trigger {
            abs.extend([(ABS_Z, t), (ABS_RZ, t)]);
        }
        abs.extend([(ABS_HAT0X, HAT), (ABS_HAT0Y, HAT)]);
        Capabilities { keys, abs }
    }

    fn rescale(value: i32, src: AbsInfo, dst: AbsInfo) -> i32 {
        let frac = (value as i64 - src.min as i64) as f64 / (src.max as i64 - src.min as i64) as f64;
        let out = py::round(dst.min as f64 + frac * (dst.max as i64 - dst.min as i64) as f64);
        out.clamp(dst.min as i64, dst.max as i64) as i32
    }

    /// Canonical xpad event -> this persona's events.
    pub fn convert(&self, etype: u16, code: u16, value: i32) -> Vec<Event> {
        if etype == EV_KEY {
            if let Some(&(_, key)) = self.face.iter().find(|&&(from, _)| from == code) {
                return vec![(EV_KEY, key, value)];
            }
            if COMMON_KEYS.contains(&code) {
                return vec![(EV_KEY, code, value)];
            }
            return vec![];
        }
        if etype != EV_ABS {
            return vec![];
        }
        if STICK_AXES.contains(&code) {
            return vec![(EV_ABS, code, Self::rescale(value, XPAD_STICK, self.stick))];
        }
        if let Some(&(_, button)) = TRIGGER_AXES.iter().find(|&&(axis, _)| axis == code) {
            let mut out = Vec::new();
            if let Some(t) = self.trigger {
                out.push((EV_ABS, code, Self::rescale(value, XPAD_TRIGGER, t)));
            }
            if self.digital_triggers() {
                out.push((EV_KEY, button, i32::from(value >= DIGITAL_TRIGGER_THRESHOLD)));
            }
            return out;
        }
        if code == ABS_HAT0X || code == ABS_HAT0Y {
            return vec![(EV_ABS, code, value)];
        }
        vec![]
    }
}

const XBOX_FACE: &[(u16, u16)] = &[(BTN_A, BTN_A), (BTN_B, BTN_B), (X_LEFT, X_LEFT), (Y_TOP, Y_TOP)];
/// gamepad.rst positional: south, east, west (0x134), north (0x133).
const SPEC_FACE: &[(u16, u16)] = &[(BTN_A, BTN_SOUTH), (BTN_B, BTN_EAST), (X_LEFT, BTN_WEST), (Y_TOP, BTN_NORTH)];

const XBOX_ONE_TRIGGER: AbsInfo = AbsInfo::new(0, 0, 1023, 0, 0, 0);
const PS_STICK: AbsInfo = AbsInfo::new(128, 0, 255, 0, 0, 0);
const PS_TRIGGER: AbsInfo = AbsInfo::new(0, 0, 255, 0, 0, 0);

const fn xbox(
    id: &'static str,
    label: &'static str,
    description: &'static str,
    name: &'static str,
    product: u16,
    version: u16,
    trigger: AbsInfo,
) -> Persona {
    Persona {
        id,
        label,
        description,
        name,
        vendor: 0x045E,
        product,
        version,
        bustype: BUS_USB,
        face: XBOX_FACE,
        stick: XPAD_STICK,
        trigger: Some(trigger),
        trigger_buttons: false,
        spec_layout: false,
    }
}

pub static PERSONAS: [Persona; 6] = [
    xbox(
        "xbox360",
        "Xbox 360",
        "Most compatible: every game, Wine and SDL know it (xpad)",
        "Microsoft X-Box 360 pad",
        0x028E,
        0x0114,
        XPAD_TRIGGER,
    ),
    xbox(
        "xboxone",
        "Xbox One S",
        "Xbox One S controller over USB (xpad)",
        "Microsoft X-Box One S pad",
        0x02EA,
        0x0301,
        XBOX_ONE_TRIGGER,
    ),
    xbox(
        "xboxseries",
        "Xbox Series X|S",
        "Xbox Series controller over USB (xpad)",
        "Microsoft Xbox Series S|X Controller",
        0x0B12,
        0x0509,
        XBOX_ONE_TRIGGER,
    ),
    Persona {
        id: "ds4",
        label: "DualShock 4",
        description: "PlayStation 4 controller over USB (hid-playstation)",
        name: "Sony Interactive Entertainment Wireless Controller",
        vendor: 0x054C,
        product: 0x09CC,
        version: 0x8111,
        bustype: BUS_USB,
        face: SPEC_FACE,
        stick: PS_STICK,
        trigger: Some(PS_TRIGGER),
        trigger_buttons: true,
        spec_layout: true,
    },
    Persona {
        id: "dualsense",
        label: "DualSense",
        description: "PlayStation 5 controller over USB (hid-playstation)",
        name: "Sony Interactive Entertainment DualSense Wireless Controller",
        vendor: 0x054C,
        product: 0x0CE6,
        version: 0x8111,
        bustype: BUS_USB,
        face: SPEC_FACE,
        stick: PS_STICK,
        trigger: Some(PS_TRIGGER),
        trigger_buttons: true,
        spec_layout: true,
    },
    Persona {
        id: "switchpro",
        label: "Switch Pro",
        description: "Nintendo Switch Pro Controller (hid-nintendo); digital triggers",
        name: "Nintendo Switch Pro Controller",
        vendor: 0x057E,
        product: 0x2009,
        version: 0x8111,
        bustype: BUS_USB,
        face: SPEC_FACE,
        stick: AbsInfo::new(0, -32767, 32767, 250, 500, 0),
        trigger: None,
        trigger_buttons: false,
        spec_layout: true,
    },
];

pub const ALIASES: [(&str, &str); 1] = [("xpad", "xbox360")];
pub const DEFAULT: &str = "xbox360";

pub fn get(persona_id: &str) -> Result<&'static Persona, String> {
    let id = ALIASES.iter().find(|&&(alias, _)| alias == persona_id).map_or(persona_id, |&(_, id)| id);
    PERSONAS.iter().find(|p| p.id == id).ok_or_else(|| {
        let ids = PERSONAS.iter().map(|p| p.id).collect::<Vec<_>>().join(", ");
        format!("unknown persona '{persona_id}'; choose from {ids}")
    })
}

/// padlink tags its virtual pads' phys as `padlink/virtual/<source>/<persona>`.
pub fn from_phys(phys: &str) -> Option<&'static Persona> {
    let tail = phys.rsplit('/').next().unwrap_or(phys);
    PERSONAS.iter().find(|p| p.id == tail)
}
