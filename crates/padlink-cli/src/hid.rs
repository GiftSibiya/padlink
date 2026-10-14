//! rumble / hid-send / hid-feature

use std::io;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use padlink_core::bridge::pick_hidraw;
use padlink_core::discovery::Controller;
use padlink_core::hex::{hexstr, parse_hex};
use padlink_core::hidraw::HidrawDevice;
use padlink_core::profiles;
use padlink_core::sink::{HidrawSink, RumbleSink, evdev_rumble};
use padlink_core::{log_write, py};

use crate::{DevArgs, Result, die, get_ctrl, open_log};

#[derive(clap::Args)]
pub struct RumbleArgs {
    #[command(flatten)]
    dev: DevArgs,
    /// heavy/left motor percent (default 100)
    #[arg(long, default_value_t = 100.0)]
    strong: f64,
    /// light/right motor percent (default 100)
    #[arg(long, default_value_t = 100.0)]
    weak: f64,
    #[arg(long, default_value_t = 800)]
    ms: u16,
    /// force the hidraw profile path
    #[arg(long)]
    hid: bool,
    #[arg(long)]
    no_log: bool,
}

fn pct(v: f64) -> u16 {
    py::round(v / 100.0 * 65535.0).clamp(0, 0xFFFF) as u16
}

pub fn rumble(a: &RumbleArgs) -> Result {
    let x = get_ctrl(&a.dev)?;
    let (strong, weak) = (pct(a.strong), pct(a.weak));
    let log = open_log(a.no_log, "rumble", Some(&x))?;
    let prof = profiles::load(x.vendor, x.product, None)?;
    let rumble_spec = prof.as_ref().and_then(|p| p.rumble.clone());
    let use_hid = a.hid || (!x.has_ff() && rumble_spec.is_some());
    let result = (|| -> io::Result<i32> {
        if !use_hid {
            let Some(node) = x.ff_node() else {
                return Ok(die(&format!(
                    "{} has no kernel force feedback and no padlink rumble profile. Run: padlink probe {}",
                    x.name,
                    x.id_str()
                )));
            };
            println!(
                "evdev FF on {}: strong={}% weak={}% for {} ms",
                node.path,
                py::float_repr(a.strong),
                py::float_repr(a.weak),
                a.ms
            );
            log_write!(log, "note", {"text": "evdev rumble test", "node": node.path, "strong": strong, "weak": weak, "ms": a.ms});
            evdev_rumble(Path::new(&node.path), strong, weak, a.ms)?;
            return Ok(0);
        }
        let Some(spec) = rumble_spec.clone() else {
            return Ok(die("no rumble profile; run `padlink probe` first"));
        };
        let Some(path) = pick_hidraw(&x, &spec) else {
            return Ok(die("controller has no hidraw node"));
        };
        let report = spec.render(strong, weak).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let mut sink = HidrawSink::open(Path::new(path), spec.clone(), log.clone())?;
        println!("hidraw {path}: {} for {} ms", hexstr(&report), a.ms);
        let end = Instant::now() + Duration::from_millis(a.ms as u64);
        let keepalive = Duration::from_millis(spec.keepalive_ms.max(0) as u64);
        let run = (|| -> io::Result<()> {
            sink.set(strong, weak)?;
            while !keepalive.is_zero() && Instant::now() + keepalive < end {
                thread::sleep(keepalive);
                sink.set(strong, weak)?;
            }
            thread::sleep(end.saturating_duration_since(Instant::now()));
            Ok(())
        })();
        sink.close();
        run.map(|_| 0)
    })();
    log.close();
    match result {
        Ok(code) => Ok(code),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            Ok(die(&format!("{e}. For hidraw: install the udev rule (padlink udev-rule {}).", x.id_str())))
        }
        Err(e) => Err(e.into()),
    }
}

fn pick_node(x: &Controller, node: &Option<String>) -> std::result::Result<String, i32> {
    match node {
        Some(n) => Ok(n.clone()),
        None => x.hidraw.first().map(|h| h.path.clone()).ok_or_else(|| die("controller has no hidraw node")),
    }
}

#[derive(clap::Args)]
pub struct HidSendArgs {
    #[arg(help = crate::DEV)]
    device: String,
    /// e.g. "05 ff 00 00"
    hex: String,
    #[arg(long)]
    all: bool,
    /// hidraw node (default: first of the device)
    #[arg(long)]
    node: Option<String>,
    /// seconds to print input reports afterwards
    #[arg(long, default_value_t = 0.0)]
    read: f64,
    #[arg(long)]
    no_log: bool,
}

pub fn hid_send(a: &HidSendArgs) -> Result {
    let x = get_ctrl(&DevArgs { device: Some(a.device.clone()), all: a.all })?;
    let path = match pick_node(&x, &a.node) {
        Ok(p) => p,
        Err(code) => return Ok(code),
    };
    let data = parse_hex(&a.hex).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let log = open_log(a.no_log, "hid-send", Some(&x))?;
    let result = (|| -> io::Result<()> {
        let mut d = HidrawDevice::open(&path, true)?;
        let n = d.write(&data)?;
        log_write!(log, "hid_tx", {"node": path, "hex": hexstr(&data), "ok": true});
        println!("wrote {n} bytes to {path}: {}", hexstr(&data));
        let end = Instant::now() + Duration::from_secs_f64(a.read.max(0.0));
        while Instant::now() < end {
            if let Some(resp) = d.read(Some(end.saturating_duration_since(Instant::now())))? {
                log_write!(log, "hid_rx", {"node": path, "hex": hexstr(&resp)});
                println!("  <- {}", hexstr(&resp));
            }
        }
        Ok(())
    })();
    let code = match result {
        Ok(()) => 0,
        Err(e) => {
            log_write!(log, "hid_tx", {"node": path, "hex": hexstr(&data), "ok": false, "error": e.to_string()});
            die(&format!("{path}: {e}"))
        }
    };
    log.close();
    Ok(code)
}

#[derive(clap::Args)]
pub struct HidFeatureArgs {
    #[arg(value_parser = ["get", "set"])]
    action: String,
    #[arg(help = crate::DEV)]
    device: String,
    /// get: report id (e.g. 0x05); set: hex bytes
    value: String,
    /// buffer length for get
    #[arg(long, default_value_t = 64)]
    length: usize,
    #[arg(long)]
    all: bool,
    #[arg(long)]
    node: Option<String>,
    #[arg(long)]
    no_log: bool,
}

/// Python's `int(text, 0)`: 0x/0o/0b prefixes, otherwise decimal (no leading zeros).
fn int_auto(text: &str) -> Option<i64> {
    let t = text.trim().replace('_', "");
    let (neg, t) = t.strip_prefix('-').map_or((false, t.as_str()), |r| (true, r));
    let lower = t.to_lowercase();
    let v = if let Some(h) = lower.strip_prefix("0x") {
        i64::from_str_radix(h, 16).ok()?
    } else if let Some(o) = lower.strip_prefix("0o") {
        i64::from_str_radix(o, 8).ok()?
    } else if let Some(b) = lower.strip_prefix("0b") {
        i64::from_str_radix(b, 2).ok()?
    } else if lower.len() > 1 && lower.starts_with('0') && lower.bytes().any(|b| b != b'0') {
        return None;
    } else {
        lower.parse().ok()?
    };
    Some(if neg { -v } else { v })
}

pub fn hid_feature(a: &HidFeatureArgs) -> Result {
    let x = get_ctrl(&DevArgs { device: Some(a.device.clone()), all: a.all })?;
    let path = match pick_node(&x, &a.node) {
        Ok(p) => p,
        Err(code) => return Ok(code),
    };
    let log = open_log(a.no_log, "hid-feature", Some(&x))?;
    let result = (|| -> io::Result<()> {
        let d = HidrawDevice::open(&path, true)?;
        if a.action == "get" {
            let id = int_auto(&a.value)
                .and_then(|v| u8::try_from(v).ok())
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("bad report id {:?}", a.value)))?;
            let data = d.get_feature(id, a.length)?;
            log_write!(log, "hid_rx", {"node": path, "hex": hexstr(&data), "feature": true});
            println!("{}", hexstr(&data));
        } else {
            let data = parse_hex(&a.value).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
            d.set_feature(&data)?;
            log_write!(log, "hid_tx", {"node": path, "hex": hexstr(&data), "ok": true, "feature": true});
            println!("set feature: {}", hexstr(&data));
        }
        Ok(())
    })();
    log.close();
    Ok(match result {
        Ok(()) => 0,
        Err(e) => die(&format!("{path}: {e}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_int_base_0() {
        assert_eq!(int_auto("0x05"), Some(5));
        assert_eq!(int_auto("5"), Some(5));
        assert_eq!(int_auto("0"), Some(0));
        assert_eq!(int_auto("0b11"), Some(3));
        assert_eq!(int_auto("05"), None);
        assert_eq!(int_auto("x"), None);
    }

    #[test]
    fn percent() {
        assert_eq!([pct(100.0), pct(0.0), pct(50.0), pct(150.0), pct(-1.0)], [65535, 0, 32768, 65535, 0]);
    }
}
