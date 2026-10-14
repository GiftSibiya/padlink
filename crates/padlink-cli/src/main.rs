//! `padlink`: the command line. Ported from the Python CLI (docs/padlink/rust-migration.md
//! in Dirah); its output matched the Python version line for line at cut-over.

mod bridge;
mod devices;
mod hid;
mod misc;
mod style;
mod tools;

use std::error::Error;
use std::io;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::{Parser, Subcommand};
use padlink_core::discovery::{self, Controller, NoSuchController};
use padlink_core::logger::Log;
use serde_json::Map;

use crate::style::{dim, red};

pub type Result<T = i32> = std::result::Result<T, Box<dyn Error>>;

const DEV: &str = "controller: index from `list`, vid:pid, /dev node, or name substring (default: first)";

#[derive(Parser)]
#[command(
    name = "padlink",
    version,
    about = "Wireless controller companion: detect, debug, log, and bridge rumble for controllers the kernel can't vibrate."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(clap::Args)]
pub struct DevArgs {
    #[arg(help = DEV)]
    pub device: Option<String>,
    /// consider every HID input device, not only joysticks
    #[arg(long)]
    pub all: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// list controllers
    List {
        /// include every HID/virtual input device
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// full dump of a controller incl. parsed HID descriptor
    Info {
        #[command(flatten)]
        dev: DevArgs,
        #[arg(long)]
        json: bool,
    },
    /// live input events (evdev) or raw HID reports (--raw)
    Monitor(devices::MonitorArgs),
    /// test rumble (kernel FF, or hidraw via profile)
    Rumble(hid::RumbleArgs),
    /// write a raw output report (first byte = report id, 00 if none)
    HidSend(hid::HidSendArgs),
    /// get/set a feature report
    HidFeature(hid::HidFeatureArgs),
    /// interactively discover the rumble report, save a profile
    Probe(tools::ProbeArgs),
    /// virtual pad with rumble, forwarded to the real controller
    Bridge(bridge::BridgeArgs),
    /// interactive input-map wizard for the bridge (xpad mode)
    Map {
        #[command(flatten)]
        dev: DevArgs,
    },
    /// diagnose why rumble does not work
    Doctor(tools::DoctorArgs),
    /// env vars to hide the physical pad from games; can edit Lutris configs
    LutrisEnv(tools::LutrisEnvArgs),
    /// print a udev rule granting hidraw access for this controller
    UdevRule {
        #[command(flatten)]
        dev: DevArgs,
    },
    /// systemd --user unit that keeps the bridge running
    Service(bridge::ServiceArgs),
    /// controller artwork (SVG) for the Input tab: templates, checks
    Art(misc::ArtArgs),
    /// list the controllers the bridge can spoof
    Personas,
    /// list/show/export session logs
    Logs(misc::LogsArgs),
    /// (root) sniff the controller's USB traffic via usbmon, for reverse-engineering
    Capture(tools::CaptureArgs),
}

pub fn die(msg: &str) -> i32 {
    eprintln!("{}", red(&format!("error: {msg}")));
    1
}

pub fn get_ctrl(dev: &DevArgs) -> Result<Controller> {
    Ok(discovery::resolve(dev.device.as_deref(), discovery::scan(dev.all)?)?)
}

/// A session log for a command, or a null log with --no-log.
pub fn open_log(no_log: bool, command: &str, ctrl: Option<&Controller>) -> Result<Log> {
    if no_log {
        return Ok(Log::null());
    }
    let mut extra = Map::new();
    if let Some(c) = ctrl {
        extra.insert("device".into(), c.to_value());
    }
    let label = ctrl.map_or_else(|| "system".to_string(), Controller::id_str);
    let log = Log::create(&label, command, extra, None)?;
    if let Some(p) = &log.path {
        eprintln!("{}", dim(&format!("logging to {}", p.display())));
    }
    Ok(log)
}

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigint(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

/// Route `sig` to the stop flag. No SA_RESTART: a blocking read returns EINTR,
/// so loops notice the flag instead of waiting for more input.
fn install(sig: libc::c_int) {
    // SAFETY: a zeroed sigaction with our handler; the handler only stores to an atomic.
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_sigint as *const () as libc::sighandler_t;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(sig, &sa, std::ptr::null_mut());
    }
}

/// Make Ctrl+C end the current loop instead of the process.
pub fn catch_sigint() {
    install(libc::SIGINT);
}

/// Ctrl+C and SIGTERM (systemd stop) both end the current loop.
pub fn catch_signals() {
    install(libc::SIGINT);
    install(libc::SIGTERM);
}

pub fn stopped() -> bool {
    STOP.load(Ordering::SeqCst)
}

fn run(cli: Cli) -> Result {
    match cli.cmd {
        Cmd::List { all, json } => devices::list(all, json),
        Cmd::Info { dev, json } => devices::info(&dev, json),
        Cmd::Monitor(a) => devices::monitor(&a),
        Cmd::Rumble(a) => hid::rumble(&a),
        Cmd::HidSend(a) => hid::hid_send(&a),
        Cmd::HidFeature(a) => hid::hid_feature(&a),
        Cmd::Probe(a) => tools::probe(&a),
        Cmd::Bridge(a) => bridge::bridge(&a),
        Cmd::Map { dev } => tools::map(&dev),
        Cmd::Doctor(a) => tools::doctor(&a),
        Cmd::LutrisEnv(a) => tools::lutris_env(&a),
        Cmd::UdevRule { dev } => misc::udev_rule(&dev),
        Cmd::Service(a) => bridge::service(&a),
        Cmd::Art(a) => misc::art(&a),
        Cmd::Personas => misc::personas(),
        Cmd::Logs(a) => misc::logs(&a),
        Cmd::Capture(a) => tools::capture(&a),
    }
}

fn main() -> ExitCode {
    // like Python's BrokenPipeError handling: `padlink list | head` just stops
    // SAFETY: restoring the default disposition of a signal.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    let code = match run(Cli::parse()) {
        Ok(code) => code,
        Err(e) if e.is::<NoSuchController>() => die(&e.to_string()),
        Err(e) => match e.downcast_ref::<io::Error>() {
            Some(io) if io.kind() == io::ErrorKind::PermissionDenied => die(&format!("{e}  (see `padlink doctor`)")),
            _ => die(&e.to_string()),
        },
    };
    ExitCode::from(code.clamp(0, 255) as u8)
}

/// `json.dumps(value, indent=2)`.
pub fn print_json(value: &serde_json::Value) {
    println!("{}", padlink_core::py::json_dumps(value, Some(2)));
}
