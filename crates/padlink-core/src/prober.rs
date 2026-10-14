//! Rumble discovery for devices the kernel can't drive.
//!
//! Stage 1 sends a series of candidate output reports and asks a human whether the
//! controller vibrated. Candidates come from the device's own HID descriptor
//! (every output report, filled with 0xFF) plus layouts used by known gamepad
//! families. Stage 2 narrows an all-0xFF hit down to the bytes that control each
//! motor, so the resulting profile can do variable strength.
//!
//! Only *output* reports are sent, and every "on" is followed by an "off". Each
//! write is logged.

use std::collections::{BTreeMap, HashMap};
use std::thread;
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::discovery::{Controller, HidrawNode};
use crate::hex::hexstr;
use crate::hiddesc::{self, Descriptor, Report, ReportKind};
use crate::hidraw::HidrawDevice;
use crate::log_write;
use crate::logger::Log;
use crate::profiles::RumbleSpec;

/// One payload item: a byte or a template token.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Item {
    Byte(i64),
    Token(&'static str),
}

impl Item {
    fn to_value(self) -> Value {
        match self {
            Item::Byte(b) => json!(b),
            Item::Token(t) => json!(t),
        }
    }
}

use Item::{Byte as B, Token as T};

/// (label, payload template excluding the report ID byte, scale)
pub const KNOWN_LAYOUTS: [(&str, &[Item], i64); 9] = [
    ("Xbox One/Series HID (BT style)", &[B(0x0F), B(0), B(0), T("strong"), T("weak"), B(0xFF), B(0), B(0)], 100),
    ("Xbox 360 wired style", &[B(0), B(0x08), B(0), T("strong"), T("weak"), B(0), B(0), B(0)], 255),
    ("DragonRise / GreenAsia", &[B(0x51), B(0), T("strong"), B(0), T("weak"), B(0), B(0), B(0)], 255),
    ("DragonRise (motors swapped)", &[B(0x51), B(0), T("weak"), B(0), T("strong"), B(0), B(0), B(0)], 255),
    ("SmartJoy style", &[B(0x01), T("weak"), T("strong")], 255),
    ("DualShock 4 USB style", &[B(0x07), B(0), B(0), T("weak"), T("strong"), B(0), B(0), B(0)], 255),
    ("Two bytes strong,weak", &[T("strong"), T("weak")], 255),
    ("Header 0x00 then strong,weak", &[B(0), T("strong"), T("weak")], 255),
    ("Header 0x08 then strong,weak", &[B(0x08), T("strong"), T("weak")], 255),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    AllFf,
    Layout,
    Byte(usize),
    Learned,
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub label: String,
    pub node: HidrawNode,
    pub report_id: u32,
    /// Full wire template, including the report ID byte.
    pub template: Vec<Value>,
    pub scale: i64,
    pub kind: Kind,
    /// Payload length (without the report ID).
    pub length: usize,
}

impl Candidate {
    pub fn spec(&self) -> RumbleSpec {
        RumbleSpec {
            transport: "hidraw".into(),
            interface: self.node.interface.clone(),
            report: self.template.clone(),
            stop: None,
            scale: self.scale,
            ..RumbleSpec::default()
        }
    }

    pub fn on_bytes(&self) -> Vec<u8> {
        self.spec().render(0xFFFF, 0xFFFF).expect("prober templates only use known tokens")
    }

    pub fn off_bytes(&self) -> Vec<u8> {
        self.spec().render(0, 0).expect("prober templates only use known tokens")
    }

    /// The `meta` dict as Python logs it.
    pub fn meta(&self) -> Value {
        let kind = match self.kind {
            Kind::AllFf => "all_ff",
            Kind::Layout => "layout",
            Kind::Byte(_) => "byte",
            Kind::Learned => "learned",
        };
        let mut m = Map::new();
        m.insert("kind".into(), json!(kind));
        if let Kind::Byte(pos) = self.kind {
            m.insert("pos".into(), json!(pos));
        }
        m.insert("length".into(), json!(self.length));
        Value::Object(m)
    }
}

/// Pad or trim a payload to `length` bytes; None if it doesn't fit.
pub fn fit(payload: &[Value], length: usize) -> Option<Vec<Value>> {
    if length == 0 {
        return None;
    }
    if payload.len() > length {
        let zero = |v: &Value| v.as_i64() == Some(0);
        if !payload[length..].iter().all(zero) {
            return None;
        }
        return Some(payload[..length].to_vec());
    }
    let mut out = payload.to_vec();
    out.resize(length, json!(0));
    Some(out)
}

/// The node's descriptor and its output reports, minus keyboard-LED-only ones.
pub fn output_reports(node: &HidrawNode) -> (Option<Descriptor>, Vec<Report>) {
    let Ok(raw) = node.descriptor() else { return (None, Vec::new()) };
    let desc = hiddesc::parse(&raw);
    let reports = desc
        .by_kind(ReportKind::Output)
        .filter(|r| {
            let pages = r.usage_pages();
            !(!pages.is_empty() && pages.iter().all(|&p| p == 0x08)) // keyboard LEDs only
        })
        .cloned()
        .collect();
    (Some(desc), reports)
}

pub fn stage1_candidates(ctrl: &Controller, blind: bool) -> Vec<Candidate> {
    let mut cands = Vec::new();
    for node in &ctrl.hidraw {
        let (_desc, reports) = output_reports(node);
        let mut targets: Vec<(u32, usize)> = reports.iter().map(|r| (r.report_id, r.payload_len() as usize)).collect();
        if blind && targets.is_empty() {
            targets = [0, 1, 2, 3, 5].iter().map(|&rid| (rid, 8)).collect();
        }
        for (rid, length) in targets {
            // 1) everything on: the most likely single "does anything vibrate" test
            let mut tpl = vec![json!(rid)];
            tpl.extend(std::iter::repeat_n(json!("any"), length));
            cands.push(Candidate {
                label: format!("report {rid:#04x}: all bytes 0xFF"),
                node: node.clone(),
                report_id: rid,
                template: tpl,
                scale: 255,
                kind: Kind::AllFf,
                length,
            });
            // 2) known gamepad layouts that fit this report
            for (label, payload, scale) in KNOWN_LAYOUTS {
                let payload: Vec<Value> = payload.iter().map(|i| i.to_value()).collect();
                if let Some(fitted) = fit(&payload, length) {
                    let mut tpl = vec![json!(rid)];
                    tpl.extend(fitted);
                    cands.push(Candidate {
                        label: format!("report {rid:#04x}: {label}"),
                        node: node.clone(),
                        report_id: rid,
                        template: tpl,
                        scale,
                        kind: Kind::Layout,
                        length,
                    });
                }
            }
        }
    }
    cands
}

/// Single-byte tests to find which positions drive which motor.
pub fn stage2_byte_candidates(hit: &Candidate) -> Vec<Candidate> {
    (0..hit.length)
        .map(|pos| {
            let mut tpl = vec![json!(hit.report_id)];
            tpl.extend(std::iter::repeat_n(json!(0), hit.length));
            tpl[1 + pos] = json!("any");
            Candidate {
                label: format!("byte {pos} = 0xFF"),
                node: hit.node.clone(),
                report_id: hit.report_id,
                template: tpl,
                scale: 255,
                kind: Kind::Byte(pos),
                length: hit.length,
            }
        })
        .collect()
}

/// Which motor a byte drives, as answered in stage 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motor {
    Strong,
    Weak,
    Both,
}

/// `motors`: byte position -> motor. Unmarked bytes are 0. With nothing
/// identified, the all-0xFF report stays as on/off.
pub fn build_from_bytes(hit: &Candidate, motors: &BTreeMap<usize, Motor>) -> Candidate {
    if motors.is_empty() {
        return hit.clone();
    }
    let mut tpl = vec![json!(hit.report_id)];
    tpl.extend(std::iter::repeat_n(json!(0), hit.length));
    for (&pos, motor) in motors {
        tpl[1 + pos] = json!(match motor {
            Motor::Strong => "strong",
            Motor::Weak => "weak",
            Motor::Both => "max",
        });
    }
    Candidate {
        label: format!("report {:#04x}: learned layout", hit.report_id),
        node: hit.node.clone(),
        report_id: hit.report_id,
        template: tpl,
        scale: 255,
        kind: Kind::Learned,
        length: hit.length,
    }
}

/// Sends candidates with logging. UI-agnostic: the caller asks the human.
pub struct Prober {
    log: Log,
    devs: HashMap<String, HidrawDevice>,
}

impl Prober {
    pub fn new(log: Log) -> Self {
        Prober { log, devs: HashMap::new() }
    }

    /// Write one report; returns the error text if it failed.
    pub fn send(&mut self, path: &str, data: &[u8]) -> Option<String> {
        let result = match self.devs.get_mut(path) {
            Some(d) => d.write(data).map(|_| ()),
            None => HidrawDevice::open(path, true).and_then(|mut d| {
                let r = d.write(data).map(|_| ());
                self.devs.insert(path.to_string(), d);
                r
            }),
        };
        let err = result.err().map(|e| e.to_string());
        let mut rec = Map::new();
        rec.insert("node".into(), json!(path));
        rec.insert("hex".into(), json!(hexstr(data)));
        rec.insert("ok".into(), json!(err.is_none()));
        if let Some(e) = &err {
            rec.insert("error".into(), json!(e));
        }
        self.log.write("hid_tx", rec);
        err
    }

    /// Send on, wait, send off. Returns the error text if the write failed.
    pub fn pulse(&mut self, cand: &Candidate, ms: u64) -> Option<String> {
        log_write!(self.log, "note", {"text": format!("probe: {}", cand.label), "template": cand.template});
        if let Some(err) = self.send(&cand.node.path, &cand.on_bytes()) {
            return Some(err);
        }
        thread::sleep(Duration::from_millis(ms));
        self.send(&cand.node.path, &cand.off_bytes());
        None
    }
}
