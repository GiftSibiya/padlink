//! probe / map / doctor / lutris-env / capture

use std::collections::{BTreeMap, HashMap};
use std::io::{self, BufRead, Write};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use evdev::raw_stream::RawDevice;
use padlink_core::codes::{BTN_DPAD_DOWN, BTN_DPAD_RIGHT, EV_ABS, EV_KEY, code_name};
use padlink_core::hex::hexstr;
use padlink_core::hidraw::poll_many;
use padlink_core::logger::{self, Log};
use padlink_core::prober::{self, Kind, Motor, Prober};
use padlink_core::{capture, discovery, doctor, integration, log_write, profiles};
use serde_json::{Map, Value, json};

use crate::style::{bold, dim, green, red, status, yellow};
use crate::{DevArgs, Result, catch_sigint, die, get_ctrl, open_log, print_json, stopped};

/// Python's `input(prompt [y/n/...])` loop: the first matching letter, "q" on EOF.
fn ask(prompt: &str, choices: &str) -> char {
    let options: Vec<String> = choices.chars().map(String::from).collect();
    loop {
        print!("{prompt} [{}] ", options.join("/"));
        let _ = io::stdout().flush();
        let mut line = String::new();
        match io::stdin().lock().read_line(&mut line) {
            Ok(0) | Err(_) => return 'q',
            Ok(_) => {}
        }
        if let Some(c) = line.trim().to_lowercase().chars().next().filter(|c| choices.contains(*c)) {
            return c;
        }
    }
}

#[derive(clap::Args)]
pub struct ProbeArgs {
    #[command(flatten)]
    dev: DevArgs,
    /// pulse length per test
    #[arg(long, default_value_t = 700)]
    ms: u64,
    /// try common report IDs even if none are declared
    #[arg(long)]
    blind: bool,
    /// probe even if kernel FF exists
    #[arg(long)]
    force: bool,
    #[arg(long)]
    no_log: bool,
}

pub fn probe(a: &ProbeArgs) -> Result {
    let x = get_ctrl(&a.dev)?;
    if let Some(node) = x.ff_node().filter(|_| !a.force) {
        println!("{} already has kernel force feedback ({}).", x.name, node.ff_names().join(", "));
        println!("Test it with: padlink rumble {}   (use --force to probe hidraw anyway)", x.id_str());
        return Ok(0);
    }
    if x.hidraw.is_empty() {
        return Ok(die("no hidraw nodes for this controller; nothing to probe"));
    }
    let unwritable: Vec<&str> = x.hidraw.iter().filter(|h| !h.writable()).map(|h| h.path.as_str()).collect();
    if unwritable.len() == x.hidraw.len() {
        println!("{}", red(&format!("No write access to {}.", unwritable.join(", "))));
        println!("Install the udev rule first:");
        println!("  padlink udev-rule {} | sudo tee /etc/udev/rules.d/70-padlink.rules", x.id_str());
        println!("  sudo udevadm control --reload && sudo udevadm trigger");
        println!("then replug the dongle. (Or run this probe once with sudo.)");
        return Ok(1);
    }
    let cands = prober::stage1_candidates(&x, a.blind);
    if cands.is_empty() {
        println!("{}", yellow("The HID descriptors declare no output reports."));
        println!(
            "Rerun with --blind to try common report IDs anyway, or capture the official app's traffic (see `padlink \
             capture --help`)."
        );
        return Ok(1);
    }
    let log = open_log(a.no_log, "probe", Some(&x))?;
    let mut p = Prober::new(log.clone());
    let code = run_probe(a, &x, &cands, &mut p, &log);
    log.close();
    code
}

fn run_probe(
    a: &ProbeArgs,
    x: &discovery::Controller,
    cands: &[prober::Candidate],
    p: &mut Prober,
    log: &Log,
) -> Result {
    println!("{}", bold(&format!("Rumble probe for {} [{}]", x.name, x.id_str())));
    println!("Hold the controller. Each test vibrates for ~0.7 s if it works.");
    println!("Answer y = vibrated, n = nothing, r = repeat, q = quit.\n");
    let mut hit = None;
    for (i, cand) in cands.iter().enumerate() {
        let ans = loop {
            println!("[{}/{}] {}  {}", i + 1, cands.len(), cand.label, dim(&hexstr(&cand.on_bytes())));
            if let Some(err) = p.pulse(cand, a.ms) {
                println!("{}", yellow(&format!("   write failed: {err}")));
                break 'n';
            }
            let ans = ask("   did it vibrate?", "ynrq");
            if ans != 'r' {
                break ans;
            }
        };
        if ans == 'q' {
            return Ok(1);
        }
        if ans == 'y' {
            log_write!(log, "note", {"text": "probe hit", "label": cand.label, "template": cand.template});
            hit = Some(cand.clone());
            break;
        }
    }
    let Some(hit) = hit else {
        println!("{}", yellow("\nNo candidate made it vibrate."));
        println!(
            "Next steps: try `--blind`, check the controller mode (XInput mode may just work), or capture traffic from \
             the official GameSir app with `padlink capture` on a Windows VM/Android host."
        );
        return Ok(1);
    };

    let mut fin = hit.clone();
    if hit.kind == Kind::AllFf {
        println!("{}", bold("\nFound a working report. Now let's find which byte drives which motor."));
        println!(
            "Answer: l = left/heavy motor, r = right/light motor, b = both/can't tell, n = nothing, q = stop narrowing"
        );
        let mut motors = BTreeMap::new();
        for cand in prober::stage2_byte_candidates(&hit) {
            println!("  {}  {}", cand.label, dim(&hexstr(&cand.on_bytes())));
            if let Some(err) = p.pulse(&cand, a.ms) {
                println!("{}", yellow(&format!("   write failed: {err}")));
                continue;
            }
            let motor = match ask("   vibrated?", "lrbnq") {
                'q' => break,
                'l' => Motor::Strong,
                'r' => Motor::Weak,
                'b' => Motor::Both,
                _ => continue,
            };
            if let Kind::Byte(pos) = cand.kind {
                motors.insert(pos, motor);
            }
        }
        fin = prober::build_from_bytes(&hit, &motors);
        if motors.is_empty() {
            println!("{}", yellow("No single byte worked alone; the profile will use the all-0xFF report as on/off."));
        }
    }

    println!("{}", bold("\nVerifying the final layout: weak motor 50%, then strong 100%."));
    let spec = fin.spec();
    for (s, w) in [(0, 0x7FFF), (0xFFFF, 0)] {
        p.send(&fin.node.path, &spec.render(s, w)?);
        thread::sleep(Duration::from_millis(600));
        p.send(&fin.node.path, &spec.render(0, 0)?);
        thread::sleep(Duration::from_millis(300));
    }
    let mut prof = profiles::load_or_new(x.vendor, x.product, &x.name)?;
    prof.rumble = Some(spec);
    let note = format!("probed {} via {}", logger::local_date_minute(logger::now()), fin.label);
    prof.notes = format!("{}\n{note}", prof.notes).trim().to_string();
    let path = prof.save(None)?;
    log_write!(log, "note", {"text": "profile saved", "path": path.display().to_string(), "profile": prof.to_value()});
    println!("{}", green(&format!("Saved {}", path.display())));
    println!("Test:   padlink rumble {}", x.id_str());
    println!("Bridge: padlink bridge {}", x.id_str());
    Ok(0)
}

const MAP_STEPS: [(&str, &str); 19] = [
    ("BTN_A", "press A (bottom face button)"),
    ("BTN_B", "press B (right face button)"),
    ("BTN_X", "press X (left face button)"),
    ("BTN_Y", "press Y (top face button)"),
    ("BTN_TL", "press LB (left bumper)"),
    ("BTN_TR", "press RB (right bumper)"),
    ("ABS_Z", "pull LT (left trigger) fully"),
    ("ABS_RZ", "pull RT (right trigger) fully"),
    ("BTN_SELECT", "press Back/Select/Minus"),
    ("BTN_START", "press Start/Plus"),
    ("BTN_MODE", "press Home/Guide"),
    ("BTN_THUMBL", "click the left stick"),
    ("BTN_THUMBR", "click the right stick"),
    ("ABS_X+", "push the LEFT stick fully RIGHT"),
    ("ABS_Y+", "push the LEFT stick fully DOWN"),
    ("ABS_RX+", "push the RIGHT stick fully RIGHT"),
    ("ABS_RY+", "push the RIGHT stick fully DOWN"),
    ("ABS_HAT0X+", "press d-pad RIGHT"),
    ("ABS_HAT0Y+", "press d-pad DOWN"),
];

pub fn map(dev: &DevArgs) -> Result {
    let x = get_ctrl(dev)?;
    let node = x.primary_event().ok_or("controller has no event node")?.path.clone();
    let mut d = RawDevice::open(&node)?;
    let absinfo: HashMap<u16, (i32, i32, i32)> =
        d.get_absinfo()?.map(|(c, a)| (c.0, (a.minimum(), a.maximum(), a.value()))).collect();
    let (mut keys, mut axes, mut key_to_axis) = (Map::new(), Map::new(), Map::new());
    let mut dpad = false;
    println!("{}", bold(&format!("Input mapping for {} -> Xbox 360 layout", x.name)));
    println!("Do each action and release. Press Enter to skip a step.\n");
    let fds = [d.as_raw_fd(), io::stdin().as_raw_fd()];

    for (target, prompt) in MAP_STEPS {
        print!("  {prompt} ... ");
        io::stdout().flush()?;
        // where each axis rests now, so a move is measured from there
        let rest: HashMap<u16, i32> = d.get_absinfo()?.map(|(c, a)| (c.0, a.value())).collect();
        let got = 'capture: loop {
            let ready = poll_many(&fds, None)?;
            if ready.contains(&1) {
                let mut line = String::new();
                io::stdin().lock().read_line(&mut line)?;
                break None;
            }
            for ev in d.fetch_events()? {
                let (t, c, v) = (ev.event_type().0, ev.code(), ev.value());
                if t == EV_KEY && v == 1 {
                    break 'capture Some((EV_KEY, c, 1));
                }
                if let (EV_ABS, Some(&(min, max, _))) = (t, absinfo.get(&c)) {
                    let span = (max - min).max(1);
                    let delta = v - rest.get(&c).copied().unwrap_or(0);
                    let threshold = if span <= 2 { 1.0 } else { 0.5 * span as f64 };
                    if (delta.abs() as f64) >= threshold {
                        break 'capture Some((EV_ABS, c, if delta > 0 { 1 } else { -1 }));
                    }
                }
            }
        };
        let Some((etype, code, direction)) = got else {
            println!("{}", dim("skipped"));
            continue;
        };
        let name = code_name(etype, code);
        let tname = target.trim_end_matches('+');
        if etype == EV_KEY {
            if tname.starts_with("BTN_") {
                keys.insert(name.clone(), json!(tname));
            } else if tname == "ABS_Z" || tname == "ABS_RZ" {
                key_to_axis.insert(name.clone(), json!(tname));
            } else if tname.starts_with("ABS_HAT") && (code == BTN_DPAD_RIGHT || code == BTN_DPAD_DOWN) {
                dpad = true;
            } else {
                println!("{}", yellow(&format!("{name} (button) can't drive {tname}; skipped")));
                continue;
            }
        } else {
            if tname.starts_with("BTN_") {
                println!("{}", yellow(&format!("{name} (axis) can't drive a button; skipped")));
                continue;
            }
            axes.insert(name.clone(), json!({"to": tname, "invert": target.ends_with('+') && direction < 0}));
        }
        println!("{}", green(&name));
        // let the control settle and drop what it sends meanwhile
        let end = Instant::now() + Duration::from_millis(600);
        while Instant::now() < end {
            if poll_many(&fds[..1], Some(Duration::from_millis(100)))?.contains(&0) {
                let _ = d.fetch_events()?.count();
            }
        }
    }
    let mapping = json!({"keys": keys, "axes": axes, "key_to_axis": key_to_axis, "dpad_buttons": dpad});
    let mut prof = profiles::load_or_new(x.vendor, x.product, &x.name)?;
    prof.input_map = Some(mapping.clone());
    let path = prof.save(None)?;
    println!("{}", green(&format!("\nSaved input map to {}", path.display())));
    print_json(&mapping);
    Ok(0)
}

#[derive(clap::Args)]
pub struct DoctorArgs {
    #[arg(help = crate::DEV)]
    device: Option<String>,
    #[arg(long)]
    json: bool,
    /// save the result as a log
    #[arg(long)]
    log: bool,
}

pub fn doctor(a: &DoctorArgs) -> Result {
    let x = match &a.device {
        Some(spec) => Some(discovery::resolve(Some(spec), discovery::scan(false)?)?),
        None => None,
    };
    let checks = doctor::run(x.as_ref(), true);
    if a.json {
        print_json(&Value::Array(checks.iter().map(doctor::Check::to_value).collect()));
        return Ok(0);
    }
    let mut scope: Option<&str> = None;
    for ch in &checks {
        if scope != Some(ch.scope.as_str()) {
            scope = Some(&ch.scope);
            println!("{}", bold(&format!("\n== {} ==", ch.scope)));
        }
        println!("{}  {}", status(ch.status), ch.title);
        if !ch.detail.is_empty() {
            println!("        {}", dim(&ch.detail));
        }
        if !ch.fix.is_empty() {
            println!("        fix: {}", ch.fix);
        }
    }
    if a.log {
        let label = x.as_ref().map_or_else(|| "system".to_string(), discovery::Controller::id_str);
        let log = Log::create(&label, "doctor", Map::new(), None)?;
        log_write!(log, "diag", {"checks": checks.iter().map(doctor::Check::to_value).collect::<Vec<_>>()});
        log.close();
        println!("{}", dim(&format!("\nsaved to {}", log.path.as_ref().expect("file log").display())));
    }
    Ok(i32::from(checks.iter().any(|c| c.status == doctor::FAIL)))
}

#[derive(clap::Args)]
pub struct LutrisEnvArgs {
    #[command(flatten)]
    dev: DevArgs,
    /// write the vars into this Lutris game config (with backup)
    #[arg(long, value_name = "GAME_SLUG")]
    apply: Option<String>,
    /// with --apply: remove the vars instead
    #[arg(long)]
    remove: bool,
}

pub fn lutris_env(a: &LutrisEnvArgs) -> Result {
    let x = get_ctrl(&a.dev)?;
    let env = integration::lutris_env(&x);
    let dir = integration::lutris_games_dir();
    if let Some(slug) = &a.apply {
        let (cfg, backup) = integration::apply_lutris_env(&dir, slug, &env, a.remove)?;
        let verb = if a.remove { "removed from" } else { "updated" };
        println!("{verb} {} (backup: {})", cfg.display(), backup.display());
        return Ok(0);
    }
    println!("# Lutris > game > Configure > System options > Environment variables:");
    for (k, v) in &env {
        println!("{k}={v}");
    }
    let games = integration::lutris_games(&dir);
    if !games.is_empty() {
        println!("\n# or apply automatically to a game:");
        for (slug, name) in games {
            println!("padlink lutris-env {} --apply {slug}    # {name}", x.id_str());
        }
    }
    Ok(0)
}

#[derive(clap::Args)]
pub struct CaptureArgs {
    #[command(flatten)]
    dev: DevArgs,
    /// JSONL output path (default: padlink log dir of the invoking user)
    #[arg(long)]
    out: Option<String>,
}

pub fn capture(a: &CaptureArgs) -> Result {
    let x = get_ctrl(&a.dev)?;
    catch_sigint();
    match capture::run(&x, a.out.as_deref().map(Path::new), true, &stopped) {
        Ok(code) => Ok(code),
        Err(capture::CaptureError::Io(e)) if e.kind() == io::ErrorKind::PermissionDenied => {
            Ok(die("usbmon needs root: sudo -E padlink capture ... (and `sudo modprobe usbmon`)"))
        }
        Err(e) => Ok(die(&e.to_string())),
    }
}
