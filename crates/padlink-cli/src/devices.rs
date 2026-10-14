//! list / info / monitor

use std::collections::HashMap;
use std::os::fd::AsRawFd;
use std::time::{Duration, UNIX_EPOCH};

use evdev::raw_stream::RawDevice;
use padlink_core::codes::{EV_ABS, EV_FF, EV_KEY, EV_MSC, EV_SYN, code_name};
use padlink_core::hex::hexstr;
use padlink_core::hidraw::{HidrawDevice, poll_many, set_nonblocking};
use padlink_core::{discovery, hiddesc, log_write, profiles, py};

use crate::style::{bold, cyan, dim, green, red, yellow};
use crate::{DevArgs, Result, catch_sigint, die, get_ctrl, open_log, print_json, stopped};

fn yes(b: bool) -> &'static str {
    if b { "True" } else { "False" }
}

pub fn list(all: bool, json: bool) -> Result {
    let ctrls = discovery::scan(all)?;
    if json {
        print_json(&discovery::controllers_value(&ctrls));
        return Ok(0);
    }
    if ctrls.is_empty() {
        println!("No controllers found.{}", if all { "" } else { " Try `padlink list --all`." });
        return Ok(1);
    }
    for x in &ctrls {
        let mut ff = if x.has_ff() { green("rumble: kernel FF") } else { yellow("rumble: none (no EV_FF)") };
        let prof = profiles::load(x.vendor, x.product, None).ok().flatten();
        if !x.has_ff() && prof.as_ref().is_some_and(|p| p.rumble.is_some()) {
            ff = green("rumble: via padlink profile");
        }
        let tag = if x.is_padlink() { cyan(" [padlink virtual]") } else { String::new() };
        println!("{}  {}{tag}", bold(&x.index.to_string()), bold(&x.name));
        println!("   {}  bus={}  driver={}  {ff}", x.id_str(), x.bus(), x.driver.as_deref().unwrap_or("-"));
        if let Some(k) = x.known_as() {
            println!("   {}", dim(k));
        }
        let nodes: Vec<String> = x
            .events
            .iter()
            .map(|n| format!("{}({}{})", n.path, n.role(), if n.readable() { "" } else { ",no-access" }))
            .collect();
        println!("   events: {}", nodes.join(", "));
        if !x.hidraw.is_empty() {
            let nodes: Vec<String> = x
                .hidraw
                .iter()
                .map(|h| {
                    let access = if h.writable() {
                        ""
                    } else if h.readable() {
                        "(ro)"
                    } else {
                        "(no-access)"
                    };
                    format!("{}{access}", h.path)
                })
                .collect();
            println!("   hidraw: {}", nodes.join(", "));
        }
        if let Some(b) = &x.battery {
            println!(
                "   battery: {}% {}",
                b.capacity.as_deref().unwrap_or("None"),
                b.status.as_deref().unwrap_or("None")
            );
        }
    }
    Ok(0)
}

pub fn info(dev: &DevArgs, json: bool) -> Result {
    let x = get_ctrl(dev)?;
    if json {
        print_json(&discovery::info_value(&x));
        return Ok(0);
    }
    println!("{}", bold(&format!("{}  [{}]", x.name, x.id_str())));
    let or_dash = |s: &str| if s.is_empty() { "-".to_string() } else { s.to_string() };
    for (k, v) in [
        ("bus", x.bus()),
        ("driver", x.driver.clone().unwrap_or_default()),
        ("uniq", x.uniq.clone()),
        ("phys", x.phys.clone()),
        ("key", x.key.clone()),
    ] {
        println!("  {k:8} {}", or_dash(&v));
    }
    println!("  version  0x{:04x}", x.version);
    if let Some(k) = x.known_as() {
        println!("  known as {k}");
    }
    if let Some(b) = &x.battery {
        let opt = |v: &Option<String>| v.as_ref().map_or_else(|| "None".to_string(), |s| format!("'{s}'"));
        println!("  battery  {{'name': '{}', 'capacity': {}, 'status': {}}}", b.name, opt(&b.capacity), opt(&b.status));
    }
    for n in &x.events {
        println!("{}", bold(&format!("\n  {}  {}  ({})", n.path, n.name, n.role())));
        println!("    access: read={} write={}", yes(n.readable()), yes(n.writable()));
        let keys: Vec<u32> = n.caps.get("key").map(|s| s.iter().copied().collect()).unwrap_or_default();
        if !keys.is_empty() {
            let names: Vec<String> = keys.iter().take(40).map(|&k| code_name(EV_KEY, k as u16)).collect();
            println!("    keys ({}): {}", keys.len(), names.join(" "));
        }
        let abses: Vec<String> =
            n.caps.get("abs").map(|s| s.iter().map(|&a| code_name(EV_ABS, a as u16)).collect()).unwrap_or_default();
        if !abses.is_empty() {
            println!("    axes: {}", abses.join(" "));
        }
        let ff = if n.has_ff() { n.ff_names().join(", ") } else { "none".into() };
        println!("    force feedback: {ff}");
    }
    for h in &x.hidraw {
        println!(
            "{}",
            bold(&format!(
                "\n  {}  (usb interface {})  read={} write={}",
                h.path,
                h.interface.as_deref().unwrap_or("-"),
                yes(h.readable()),
                yes(h.writable())
            ))
        );
        let raw = match h.descriptor() {
            Ok(raw) => raw,
            Err(e) => {
                println!("    descriptor: {e}");
                continue;
            }
        };
        println!("{}", dim(&format!("    descriptor ({} bytes): {}", raw.len(), hexstr(&raw))));
        for line in hiddesc::parse(&raw).summary() {
            println!("    {line}");
        }
    }
    let prof = profiles::load(x.vendor, x.product, None)?;
    let where_ = prof.as_ref().map_or_else(|| "none".to_string(), |p| p.path().display().to_string());
    println!("{}{where_}", bold("\n  profile: "));
    if let Some(p) = prof {
        println!("    {}", py::json_dumps(&p.to_value(), None));
    }
    Ok(0)
}

#[derive(clap::Args)]
pub struct MonitorArgs {
    #[command(flatten)]
    dev: DevArgs,
    /// read hidraw instead of evdev
    #[arg(long)]
    raw: bool,
    /// hidraw node to read with --raw
    #[arg(long)]
    node: Option<String>,
    /// also read motion/touchpad/etc. event nodes
    #[arg(long)]
    all_nodes: bool,
    /// show MSC events / unchanged reports
    #[arg(short, long)]
    verbose: bool,
    #[arg(long)]
    no_log: bool,
}

fn hms_now() -> String {
    padlink_core::logger::local_hms(padlink_core::logger::now())
}

pub fn monitor(a: &MonitorArgs) -> Result {
    let x = get_ctrl(&a.dev)?;
    let log = open_log(a.no_log, "monitor", Some(&x))?;
    catch_sigint();
    let tick = Some(Duration::from_millis(500));

    if a.raw {
        let nodes: Vec<_> = x.hidraw.iter().filter(|h| a.node.as_ref().is_none_or(|n| *n == h.path)).collect();
        if nodes.is_empty() {
            return Ok(die("no hidraw nodes (or none matching --node)"));
        }
        let mut devs = Vec::new();
        for h in nodes {
            match HidrawDevice::open(&h.path, false) {
                Ok(d) => devs.push(d),
                Err(e) => println!("{}", yellow(&format!("{e}  (install the udev rule: padlink udev-rule)"))),
            }
        }
        if devs.is_empty() {
            return Ok(1);
        }
        let mut last: HashMap<(String, u8), Vec<u8>> = HashMap::new();
        println!("{}", dim("raw HID input reports; changed bytes highlighted. Ctrl+C to stop."));
        while !stopped() {
            let fds: Vec<_> = devs.iter().map(AsRawFd::as_raw_fd).collect();
            for i in poll_many(&fds, tick)? {
                let d = &mut devs[i];
                let Some(data) = d.read(Some(Duration::ZERO))? else { continue };
                if data.is_empty() {
                    continue;
                }
                let path = d.path.display().to_string();
                let prev = last.insert((path.clone(), data[0]), data.clone());
                if prev.as_ref() == Some(&data) && !a.verbose {
                    continue;
                }
                log_write!(log, "hid_rx", {"node": path, "hex": hexstr(&data)});
                let parts: Vec<String> = data
                    .iter()
                    .enumerate()
                    .map(|(i, b)| {
                        let s = format!("{b:02x}");
                        let changed = prev.as_ref().is_some_and(|p| i >= p.len() || p[i] != *b);
                        if changed { yellow(&s) } else { s }
                    })
                    .collect();
                println!("{} {path} [{:3}] {}", hms_now(), data.len(), parts.join(" "));
            }
        }
        log.close();
        return Ok(0);
    }

    let primary = x.primary_event().map(|n| n.path.clone());
    let mut devs = Vec::new();
    for n in x.events.iter().filter(|n| a.all_nodes || n.is_joystick() || Some(&n.path) == primary.as_ref()) {
        match RawDevice::open(&n.path) {
            Ok(d) => devs.push((n.path.clone(), d)),
            Err(e) => println!("{}", yellow(&format!("{}: {e}", n.path))),
        }
    }
    if devs.is_empty() {
        return Ok(1);
    }
    for (_, d) in &devs {
        set_nonblocking(d.as_raw_fd())?;
    }
    let paths: Vec<&str> = devs.iter().map(|(p, _)| p.as_str()).collect();
    println!("{}", dim(&format!("evdev events from {}. Ctrl+C to stop.", paths.join(", "))));
    'outer: while !stopped() {
        let fds: Vec<_> = devs.iter().map(|(_, d)| d.as_raw_fd()).collect();
        for i in poll_many(&fds, tick)? {
            let (path, d) = &mut devs[i];
            let events: Vec<_> = match d.fetch_events() {
                Ok(evs) => evs.collect(),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(e) => {
                    println!("{}", red(&format!("{path}: {e}")));
                    break 'outer;
                }
            };
            for ev in events {
                let (t, code, value) = (ev.event_type().0, ev.code(), ev.value());
                if t == EV_SYN || (t == EV_MSC && !a.verbose) {
                    continue;
                }
                let name = code_name(t, code);
                log_write!(log, "input", {"node": path, "type": t, "code": code, "value": value, "name": name});
                let tname = match t {
                    EV_KEY => "KEY".to_string(),
                    EV_ABS => "ABS".into(),
                    EV_MSC => "MSC".into(),
                    EV_FF => "FF".into(),
                    other => other.to_string(),
                };
                let val = if t == EV_KEY {
                    match value {
                        1 => green("press"),
                        0 => dim("release"),
                        _ => "repeat".into(),
                    }
                } else {
                    value.to_string()
                };
                let ts = ev.timestamp().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
                println!("{ts:.3} {path} {tname:3} {name:16} {val}");
            }
        }
    }
    log.close();
    Ok(0)
}
