//! Force-feedback logic, without device access.
//!
//! - `RawEffect` / `Effect::from_raw()`: normalise a kernel `ff_effect` into the
//!   effect record padlink logs and mixes (Python's `effect_to_dict`)
//! - `RumbleMixer`: tracks uploaded effects and their playback, mixes them
//!   into a (strong, weak) pair like the kernel's ff-memless
//!
//! Timing uses the same float operations as Python so playback windows match
//! to the last bit (docs/padlink/formats.md, porting rule 2).

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::codes::*;

pub const MAX: u32 = 0xFFFF;

pub fn type_name(etype: u16) -> String {
    match etype {
        FF_RUMBLE => "rumble".into(),
        FF_PERIODIC => "periodic".into(),
        FF_CONSTANT => "constant".into(),
        FF_RAMP => "ramp".into(),
        FF_SPRING => "spring".into(),
        FF_FRICTION => "friction".into(),
        FF_DAMPER => "damper".into(),
        FF_INERTIA => "inertia".into(),
        other => format!("0x{other:02x}"),
    }
}

/// The waveform as logged: a name, or the raw number if unknown.
pub fn waveform_value(waveform: u16) -> Value {
    let name = match waveform {
        FF_SQUARE => "square",
        FF_TRIANGLE => "triangle",
        FF_SINE => "sine",
        FF_SAW_UP => "saw_up",
        FF_SAW_DOWN => "saw_down",
        FF_CUSTOM => "custom",
        other => return json!(other),
    };
    json!(name)
}

/// The parts of a kernel `struct ff_effect` padlink uses.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawEffect {
    pub etype: u16,
    pub id: i16,
    pub direction: u16,
    pub length: u16,
    pub delay: u16,
    pub params: RawParams,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RawParams {
    Rumble {
        strong: u16,
        weak: u16,
    },
    Periodic {
        waveform: u16,
        period: u16,
        magnitude: i16,
        offset: i16,
    },
    Constant {
        level: i16,
    },
    Ramp {
        start_level: i16,
        end_level: i16,
    },
    /// Condition effects and anything else: no motor levels.
    Other,
}

fn s16_to_mag(v: i16) -> u32 {
    MAX.min((v as i32).unsigned_abs() * 2)
}

/// An uploaded effect: identity, timing and the two motor levels it asks for.
#[derive(Debug, Clone, PartialEq)]
pub struct Effect {
    pub id: i32,
    pub length: u32,
    pub delay: u32,
    /// Rumble motor levels (0..65535) this effect should produce on a two-motor pad.
    pub strong: u32,
    pub weak: u32,
    /// The full record as Python logs it (`ff_upload` effect dict).
    pub record: Value,
}

impl Effect {
    pub fn from_raw(raw: &RawEffect) -> Effect {
        let mut record = json!({
            "type": type_name(raw.etype),
            "id": raw.id,
            "direction": raw.direction,
            "length": raw.length,
            "delay": raw.delay,
            "strong": 0,
            "weak": 0,
        });
        let (strong, weak) = match (raw.etype, raw.params) {
            (FF_RUMBLE, RawParams::Rumble { strong, weak }) => (strong as u32, weak as u32),
            (FF_PERIODIC, RawParams::Periodic { waveform, period, magnitude, offset }) => {
                let obj = record.as_object_mut().expect("object");
                obj.insert("waveform".into(), waveform_value(waveform));
                obj.insert("period".into(), json!(period));
                obj.insert("magnitude".into(), json!(magnitude));
                obj.insert("offset".into(), json!(offset));
                let mag = MAX.min(s16_to_mag(magnitude) + s16_to_mag(offset) / 2);
                (mag, mag)
            }
            (FF_CONSTANT, RawParams::Constant { level }) => {
                record["level"] = json!(level);
                (s16_to_mag(level), s16_to_mag(level))
            }
            (FF_RAMP, RawParams::Ramp { start_level, end_level }) => {
                record["start_level"] = json!(start_level);
                record["end_level"] = json!(end_level);
                let m = s16_to_mag(start_level).max(s16_to_mag(end_level));
                (m, m)
            }
            _ => (0, 0),
        };
        record["strong"] = json!(strong);
        record["weak"] = json!(weak);
        Effect { id: raw.id as i32, length: raw.length as u32, delay: raw.delay as u32, strong, weak, record }
    }

    /// A plain rumble effect (what `RumbleDriver.direct()` uploads).
    pub fn rumble(id: i32, strong: u32, weak: u32, length: u32) -> Effect {
        Effect {
            id,
            length,
            delay: 0,
            strong,
            weak,
            record: json!({"id": id, "type": "rumble", "strong": strong, "weak": weak, "length": length, "delay": 0}),
        }
    }
}

/// Effect table + playback state. Time (seconds, monotonic) is passed in explicitly.
#[derive(Debug, Clone)]
pub struct RumbleMixer {
    pub effects: BTreeMap<i32, Effect>,
    /// id -> (start, end); end is infinite for length 0
    pub playing: BTreeMap<i32, (f64, f64)>,
    pub gain: u32,
}

impl Default for RumbleMixer {
    fn default() -> Self {
        RumbleMixer { effects: BTreeMap::new(), playing: BTreeMap::new(), gain: MAX }
    }
}

impl RumbleMixer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn upload(&mut self, effect: Effect) {
        self.effects.insert(effect.id, effect);
    }

    pub fn erase(&mut self, id: i32) {
        self.effects.remove(&id);
        self.playing.remove(&id);
    }

    pub fn play(&mut self, id: i32, count: i32, now: f64) {
        let Some(eff) = self.effects.get(&id) else { return };
        if count <= 0 {
            self.playing.remove(&id);
            return;
        }
        let start = now + eff.delay as f64 / 1000.0;
        let end =
            if eff.length == 0 { f64::INFINITY } else { start + (count as i64 * eff.length as i64) as f64 / 1000.0 };
        self.playing.insert(id, (start, end));
    }

    pub fn set_gain(&mut self, gain: i64) {
        self.gain = gain.clamp(0, MAX as i64) as u32;
    }

    pub fn stop_all(&mut self) {
        self.playing.clear();
    }

    /// The mixed (strong, weak) at `now`. Drops effects that have finished.
    pub fn levels(&mut self, now: f64) -> (u32, u32) {
        self.playing.retain(|_, &mut (_, end)| now < end);
        let (mut strong, mut weak) = (0u64, 0u64);
        for (id, &(start, _)) in &self.playing {
            if now < start {
                continue;
            }
            if let Some(eff) = self.effects.get(id) {
                strong += eff.strong as u64;
                weak += eff.weak as u64;
            }
        }
        let scale = |v: u64| (v.min(MAX as u64) * self.gain as u64 / MAX as u64) as u32;
        (scale(strong), scale(weak))
    }

    /// When the mix will next change on its own (an effect starting or ending).
    pub fn next_deadline(&self, now: f64) -> Option<f64> {
        self.playing
            .values()
            .flat_map(|&(start, end)| [(start > now).then_some(start), end.is_finite().then_some(end)])
            .flatten()
            .min_by(f64::total_cmp)
    }
}
