//! evdev event types, codes and their names, as python-evdev's `ecodes` has them.

use crate::codes_table::{CODE_NAMES, NAMED_CODES};

/// An input event as (type, code, value).
pub type Event = (u16, u16, i32);

pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_REL: u16 = 0x02;
pub const EV_ABS: u16 = 0x03;
pub const EV_MSC: u16 = 0x04;
pub const EV_FF: u16 = 0x15;

pub const BUS_USB: u16 = 0x03;

pub const BTN_A: u16 = 0x130; // BTN_SOUTH
pub const BTN_B: u16 = 0x131; // BTN_EAST
pub const BTN_X: u16 = 0x133; // BTN_NORTH: the *left* button on xpad, the top one per gamepad.rst
pub const BTN_Y: u16 = 0x134; // BTN_WEST
pub const BTN_SOUTH: u16 = BTN_A;
pub const BTN_EAST: u16 = BTN_B;
pub const BTN_NORTH: u16 = BTN_X;
pub const BTN_WEST: u16 = BTN_Y;
pub const BTN_TL: u16 = 0x136;
pub const BTN_TR: u16 = 0x137;
pub const BTN_TL2: u16 = 0x138;
pub const BTN_TR2: u16 = 0x139;
pub const BTN_SELECT: u16 = 0x13a;
pub const BTN_START: u16 = 0x13b;
pub const BTN_MODE: u16 = 0x13c;
pub const BTN_THUMBL: u16 = 0x13d;
pub const BTN_THUMBR: u16 = 0x13e;
pub const BTN_DPAD_UP: u16 = 0x220;
pub const BTN_DPAD_DOWN: u16 = 0x221;
pub const BTN_DPAD_LEFT: u16 = 0x222;
pub const BTN_DPAD_RIGHT: u16 = 0x223;

pub const ABS_X: u16 = 0x00;
pub const ABS_Y: u16 = 0x01;
pub const ABS_Z: u16 = 0x02;
pub const ABS_RX: u16 = 0x03;
pub const ABS_RY: u16 = 0x04;
pub const ABS_RZ: u16 = 0x05;
pub const ABS_GAS: u16 = 0x09;
pub const ABS_BRAKE: u16 = 0x0a;
pub const ABS_HAT0X: u16 = 0x10;
pub const ABS_HAT0Y: u16 = 0x11;

pub const FF_RUMBLE: u16 = 0x50;
pub const FF_PERIODIC: u16 = 0x51;
pub const FF_CONSTANT: u16 = 0x52;
pub const FF_SPRING: u16 = 0x53;
pub const FF_FRICTION: u16 = 0x54;
pub const FF_DAMPER: u16 = 0x55;
pub const FF_INERTIA: u16 = 0x56;
pub const FF_RAMP: u16 = 0x57;
pub const FF_SQUARE: u16 = 0x58;
pub const FF_TRIANGLE: u16 = 0x59;
pub const FF_SINE: u16 = 0x5a;
pub const FF_SAW_UP: u16 = 0x5b;
pub const FF_SAW_DOWN: u16 = 0x5c;
pub const FF_CUSTOM: u16 = 0x5d;
pub const FF_GAIN: u16 = 0x60;

/// `struct input_absinfo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbsInfo {
    pub value: i32,
    pub min: i32,
    pub max: i32,
    pub fuzz: i32,
    pub flat: i32,
    pub resolution: i32,
}

impl AbsInfo {
    pub const fn new(value: i32, min: i32, max: i32, fuzz: i32, flat: i32, resolution: i32) -> Self {
        AbsInfo { value, min, max, fuzz, flat, resolution }
    }
}

/// The name python-evdev gives a code (its first alias), or the number if it has none.
/// Only key, abs, ff, rel, msc and syn codes are named, like `bridge.code_name()`.
pub fn code_name(etype: u16, code: u16) -> String {
    match CODE_NAMES.binary_search_by(|&(t, c, _)| (t, c).cmp(&(etype, code))) {
        Ok(i) => CODE_NAMES[i].2.to_string(),
        Err(_) => code.to_string(),
    }
}

/// The value of any `ecodes` constant by name (`"BTN_SOUTH"`, `"ABS_RZ"`, `"EV_KEY"`, ...).
pub fn code_of(name: &str) -> Option<u32> {
    NAMED_CODES.binary_search_by(|&(n, _)| n.cmp(name)).ok().map(|i| NAMED_CODES[i].1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_ecodes() {
        for (name, value) in [
            ("EV_SYN", EV_SYN),
            ("EV_KEY", EV_KEY),
            ("EV_REL", EV_REL),
            ("EV_ABS", EV_ABS),
            ("EV_MSC", EV_MSC),
            ("EV_FF", EV_FF),
            ("BUS_USB", BUS_USB),
            ("BTN_A", BTN_A),
            ("BTN_B", BTN_B),
            ("BTN_X", BTN_X),
            ("BTN_Y", BTN_Y),
            ("BTN_SOUTH", BTN_SOUTH),
            ("BTN_EAST", BTN_EAST),
            ("BTN_NORTH", BTN_NORTH),
            ("BTN_WEST", BTN_WEST),
            ("BTN_TL", BTN_TL),
            ("BTN_TR", BTN_TR),
            ("BTN_TL2", BTN_TL2),
            ("BTN_TR2", BTN_TR2),
            ("BTN_SELECT", BTN_SELECT),
            ("BTN_START", BTN_START),
            ("BTN_MODE", BTN_MODE),
            ("BTN_THUMBL", BTN_THUMBL),
            ("BTN_THUMBR", BTN_THUMBR),
            ("BTN_DPAD_UP", BTN_DPAD_UP),
            ("BTN_DPAD_DOWN", BTN_DPAD_DOWN),
            ("BTN_DPAD_LEFT", BTN_DPAD_LEFT),
            ("BTN_DPAD_RIGHT", BTN_DPAD_RIGHT),
            ("ABS_X", ABS_X),
            ("ABS_Y", ABS_Y),
            ("ABS_Z", ABS_Z),
            ("ABS_RX", ABS_RX),
            ("ABS_RY", ABS_RY),
            ("ABS_RZ", ABS_RZ),
            ("ABS_GAS", ABS_GAS),
            ("ABS_BRAKE", ABS_BRAKE),
            ("ABS_HAT0X", ABS_HAT0X),
            ("ABS_HAT0Y", ABS_HAT0Y),
            ("FF_RUMBLE", FF_RUMBLE),
            ("FF_PERIODIC", FF_PERIODIC),
            ("FF_CONSTANT", FF_CONSTANT),
            ("FF_SPRING", FF_SPRING),
            ("FF_FRICTION", FF_FRICTION),
            ("FF_DAMPER", FF_DAMPER),
            ("FF_INERTIA", FF_INERTIA),
            ("FF_RAMP", FF_RAMP),
            ("FF_SQUARE", FF_SQUARE),
            ("FF_TRIANGLE", FF_TRIANGLE),
            ("FF_SINE", FF_SINE),
            ("FF_SAW_UP", FF_SAW_UP),
            ("FF_SAW_DOWN", FF_SAW_DOWN),
            ("FF_CUSTOM", FF_CUSTOM),
            ("FF_GAIN", FF_GAIN),
        ] {
            assert_eq!(code_of(name), Some(value as u32), "{name}");
        }
    }

    #[test]
    fn names() {
        assert_eq!(code_name(EV_KEY, 0x130), "BTN_A");
        assert_eq!(code_name(EV_KEY, 0x133), "BTN_NORTH");
        assert_eq!(code_name(EV_KEY, 0x2ff), "767");
        assert_eq!(code_name(99, 1), "1");
        assert_eq!(code_of("NOPE"), None);
    }
}
