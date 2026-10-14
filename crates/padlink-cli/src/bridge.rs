//! bridge / service

use std::thread;
use std::time::{Duration, Instant};

use padlink_core::bridge::{Bridge, BridgeHandle, BridgeOptions, wait_for};
use padlink_core::logger::{self, Log};
use padlink_core::{integration, personas, py};
use serde_json::{Value, json};

use crate::style::{bold, dim, yellow};
use crate::{DevArgs, Result, catch_signals, die, get_ctrl, stopped};

const MODES: [&str; 8] = ["xbox360", "xboxone", "xboxseries", "ds4", "dualsense", "switchpro", "mirror", "xpad"];

#[derive(clap::Args)]
pub struct BridgeArgs {
    #[command(flatten)]
    dev: DevArgs,
    /// what the virtual pad pretends to be (see `padlink personas`); default xbox360
    #[arg(long = "mode", visible_alias = "as", default_value = personas::DEFAULT, value_parser = MODES)]
    mode: String,
    /// wait for the controller and re-bridge after reconnects
    #[arg(long)]
    wait: bool,
    /// write a JSONL session log
    #[arg(long)]
    log: bool,
    /// print input and output traffic too
    #[arg(short, long)]
    verbose: bool,
    /// don't grab the physical device
    #[arg(long)]
    no_grab: bool,
    /// input map JSON file (overrides profile/heuristic)
    #[arg(long)]
    map: Option<String>,
    /// override virtual vendor id (hex)
    #[arg(long)]
    vendor: Option<String>,
    /// override virtual product id (hex)
    #[arg(long)]
    product: Option<String>,
    /// override virtual device name
    #[arg(long)]
    name: Option<String>,
    /// machine-readable output for frontends: every log record as a JSON line,
    /// plus a {"kind": "status"} line every second
    #[arg(long)]
    json: bool,
}

fn hex16(v: &Option<String>) -> Result<Option<u16>> {
    v.as_deref()
        .map(|s| {
            let s = s.trim();
            u16::from_str_radix(s.strip_prefix("0x").unwrap_or(s), 16).map_err(|_| format!("bad hex id {s:?}").into())
        })
        .transpose()
}

pub fn bridge(a: &BridgeArgs) -> Result {
    catch_signals();
    let mapping: Option<Value> = match &a.map {
        Some(path) => Some(serde_json::from_str(&std::fs::read_to_string(path)?)?),
        None => None,
    };
    let (vendor, product) = (hex16(&a.vendor)?, hex16(&a.product)?);
    loop {
        let x = if a.wait {
            eprintln!("{}", dim(&format!("waiting for {}...", a.dev.device.as_deref().unwrap_or("a controller"))));
            match wait_for(a.dev.device.as_deref(), a.dev.all, Duration::from_secs(2), &stopped) {
                Some(x) => x,
                None => return Ok(0),
            }
        } else {
            get_ctrl(&a.dev)?
        };
        if x.is_padlink() {
            return Ok(die("that is a padlink virtual pad; pick the physical controller"));
        }
        let log = if a.log {
            let mut extra = serde_json::Map::new();
            extra.insert("device".into(), x.to_value());
            let log = Log::create(&x.id_str(), "bridge", extra, None)?;
            eprintln!("{}", dim(&format!("logging to {}", log.path.as_ref().expect("file log").display())));
            log
        } else {
            Log::null()
        };
        let (t0, verbose, json) = (logger::now(), a.verbose, a.json);
        log.add_listener(Box::new(move |rec: &Value| {
            if json {
                println!("{}", py::json_dumps(rec, None));
                return;
            }
            let kind = rec["kind"].as_str().unwrap_or_default();
            let always = matches!(kind, "ff_upload" | "ff_erase" | "ff_play" | "ff_gain" | "note");
            if always || (verbose && matches!(kind, "rumble_out" | "hid_tx" | "input")) {
                println!("{}", logger::format_record(rec.as_object().expect("record"), Some(t0)));
            }
        }));
        let opts = BridgeOptions {
            mode: a.mode.clone(),
            grab: !a.no_grab,
            log_input: a.verbose,
            mapping: mapping.clone(),
            vendor,
            product,
            name: a.name.clone(),
        };
        let target = if a.mode == "mirror" {
            "mirror of the source".to_string()
        } else {
            let p = personas::get(&a.mode)?;
            format!("{} [{}]", p.label, p.id_str())
        };
        let name = x.name.clone();
        let id = x.id_str();
        let env = integration::lutris_env(&x);
        let mut handle = Bridge::new(x, log.clone(), opts)?.spawn();
        if !a.json {
            println!("{}", bold(&format!("Bridging {name} [{id}] -> virtual {target}. Ctrl+C to stop.")));
            if !a.no_grab {
                let vars: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();
                println!("{}", dim(&format!("Hide the physical pad from games: {}", vars.join(" "))));
            }
        }
        let mut next_status = Instant::now();
        while handle.alive() && !stopped() {
            if a.json && Instant::now() >= next_status {
                print_status(&handle, &log, true);
                next_status += Duration::from_secs(1);
            }
            thread::sleep(Duration::from_millis(50));
        }
        handle.stop();
        log.close();
        if a.json {
            print_status(&handle, &log, false);
        }
        let error = handle.error();
        if stopped() || !a.wait {
            return Ok(if error.is_none() || stopped() { 0 } else { 1 });
        }
        println!(
            "{}",
            yellow(&format!("bridge ended: {}; waiting for the controller again", error.unwrap_or_default()))
        );
        thread::sleep(Duration::from_secs(1));
    }
}

/// One `{"kind": "status"}` line for --json frontends.
fn print_status(handle: &BridgeHandle, log: &Log, alive: bool) {
    let st = handle.status.lock().expect("status");
    let rec = json!({
        "t": logger::now(), "kind": "status", "alive": alive, "virtual": st.virtual_path,
        "stats": {"uploads": st.stats.uploads, "plays": st.stats.plays, "inputs": st.stats.inputs},
        "error": st.error, "log": log.path.as_ref().map(|p| p.display().to_string()),
    });
    println!("{}", py::json_dumps(&rec, None));
}

#[derive(clap::Args)]
pub struct ServiceArgs {
    #[arg(value_parser = ["show", "install"])]
    action: String,
    /// vid:pid to bridge, e.g. 3537:1041
    device: String,
    /// persona for the virtual pad
    #[arg(long = "as", default_value = personas::DEFAULT, value_parser = MODES[..7].to_vec())]
    mode: String,
}

pub fn service(a: &ServiceArgs) -> Result {
    let exe = std::env::current_exe()?.display().to_string();
    if a.action == "show" {
        println!("{}", integration::systemd_unit(&exe, &a.device, &a.mode));
    } else {
        let path = integration::install_service(&exe, &a.device, &a.mode)?;
        println!("wrote {}", path.display());
        println!("enable with: systemctl --user daemon-reload && systemctl --user enable --now padlink-bridge");
    }
    Ok(0)
}
