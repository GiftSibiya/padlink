//! USB traffic capture through usbmon (text interface), for reverse-engineering.
//!
//! Typical use: pass the dongle through to a Windows VM running the official app
//! (or any game with working rumble), run `sudo -E padlink capture 3537:1041`
//! on the host, make it rumble, then look for the OUT reports in the log.
//!
//! usbmon text line format (Documentation/usb/usbmon.rst):
//!   `<urb tag> <timestamp us> <S|C|E> <type+dir>:<bus>:<dev>:<ep> <status> <len> [= data words]`

use std::fs::{self, File};
use std::io;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::discovery::Controller;
use crate::logger::{self, Log};

pub const DEBUGFS: &str = "/sys/kernel/debug/usb/usbmon";

#[derive(Debug)]
pub enum CaptureError {
    /// A message for the user.
    Msg(String),
    Io(io::Error),
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaptureError::Msg(m) => f.write_str(m),
            CaptureError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for CaptureError {}

impl From<io::Error> for CaptureError {
    fn from(e: io::Error) -> Self {
        CaptureError::Io(e)
    }
}

/// (bus, device) numbers of a USB controller.
pub fn usb_address(ctrl: &Controller) -> Result<(u32, u32), CaptureError> {
    let root = Path::new(&ctrl.key);
    let read = |name: &str| fs::read_to_string(root.join(name)).ok().and_then(|s| s.trim().parse().ok());
    match (read("busnum"), read("devnum")) {
        (Some(bus), Some(dev)) => Ok((bus, dev)),
        _ => Err(CaptureError::Msg("capture only works for USB devices (the dongle), not Bluetooth/virtual".into())),
    }
}

/// One usbmon text line as a record (`event, xfer, bus, dev, ep, dir, type`, `hex?`, `setup?`).
pub fn parse_line(line: &str) -> Option<Map<String, Value>> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 4 {
        return None;
    }
    let addr: Vec<&str> = parts[3].split(':').collect();
    let (xfer, bus, dev, ep) = match addr.as_slice() {
        [x, b, d, e, ..] => (*x, b.parse::<i64>().ok()?, d.parse::<i64>().ok()?, e.parse::<i64>().ok()?),
        _ => return None,
    };
    let first = xfer.chars().next()?;
    let kind = match first {
        'C' => "control".to_string(),
        'I' => "interrupt".into(),
        'B' => "bulk".into(),
        'Z' => "iso".into(),
        other => other.to_string(),
    };
    let mut rec = Map::new();
    rec.insert("event".into(), json!(parts[2]));
    rec.insert("xfer".into(), json!(xfer));
    rec.insert("bus".into(), json!(bus));
    rec.insert("dev".into(), json!(dev));
    rec.insert("ep".into(), json!(ep));
    rec.insert("dir".into(), json!(if xfer.ends_with('i') { "in" } else { "out" }));
    rec.insert("type".into(), json!(kind));
    if let Some(idx) = parts.iter().position(|p| *p == "=") {
        let bytes: Vec<String> = parts[idx + 1..]
            .iter()
            .flat_map(|w| w.as_bytes().chunks(2).map(|c| String::from_utf8_lossy(c).into_owned()))
            .collect();
        rec.insert("hex".into(), json!(bytes.join(" ")));
    }
    if xfer.starts_with('C') && parts[2] == "S" && parts.len() > 9 && parts[4] == "s" {
        rec.insert("setup".into(), json!(parts[5..10].join(" ")));
    }
    Some(rec)
}

/// Lines from a blocking file, ending early when `stop()` turns true. Uses raw
/// read(2) so a signal (installed without SA_RESTART) interrupts the wait.
fn lines_until<'a>(fh: File, stop: &'a dyn Fn() -> bool) -> impl Iterator<Item = String> + 'a {
    let mut pending: Vec<u8> = Vec::new();
    let mut ready: std::collections::VecDeque<String> = Default::default();
    std::iter::from_fn(move || {
        loop {
            if let Some(line) = ready.pop_front() {
                return Some(line);
            }
            if stop() {
                return None;
            }
            let mut buf = [0u8; 8192];
            // SAFETY: reading into a stack buffer of the given length from an fd we own.
            let n = unsafe { libc::read(fh.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return None;
            }
            if n == 0 {
                return None;
            }
            pending.extend_from_slice(&buf[..n as usize]);
            while let Some(nl) = pending.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = pending.drain(..=nl).collect();
                ready.push_back(String::from_utf8_lossy(&line[..line.len() - 1]).into_owned());
            }
        }
    })
}

/// Under sudo, logs go to the invoking user's log dir.
fn default_log_dir() -> Option<PathBuf> {
    let user = std::env::var("SUDO_USER").ok()?;
    let c = std::ffi::CString::new(user).ok()?;
    // SAFETY: getpwnam returns a pointer to static storage or NULL; we copy the string out immediately.
    let pw = unsafe { libc::getpwnam(c.as_ptr()) };
    if pw.is_null() {
        return None;
    }
    let home = unsafe { std::ffi::CStr::from_ptr((*pw).pw_dir) }.to_string_lossy().into_owned();
    Some(PathBuf::from(home).join(".local/state/padlink/logs"))
}

/// Capture until `stop()` returns true or the stream ends. `show` prints OUT
/// reports and completed transfers as they happen.
pub fn run(ctrl: &Controller, out: Option<&Path>, show: bool, stop: &dyn Fn() -> bool) -> Result<i32, CaptureError> {
    let (bus, dev) = usb_address(ctrl)?;
    let src = Path::new(DEBUGFS).join(format!("{bus}u"));
    if !Path::new(DEBUGFS).exists() {
        return Err(CaptureError::Msg(
            "usbmon not available: sudo modprobe usbmon (and debugfs mounted at /sys/kernel/debug)".into(),
        ));
    }
    let fh = File::open(&src)?; // PermissionDenied propagates
    let mut extra = Map::new();
    extra.insert("device".into(), ctrl.to_value());
    extra.insert("usb".into(), json!({"bus": bus, "dev": dev}));
    let log = match out {
        Some(path) => Log::create_at(path, &ctrl.id_str(), "capture", extra)?,
        None => Log::create(&ctrl.id_str(), "capture", extra, default_log_dir().as_deref())?,
    };
    let log_path = log.path.clone().expect("file log");
    println!("capturing bus {bus} device {dev} from {} -> {}  (Ctrl+C to stop)", src.display(), log_path.display());
    for line in lines_until(fh, stop) {
        let Some(rec) = parse_line(&line) else { continue };
        if rec["bus"] != bus || rec["dev"] != dev {
            continue;
        }
        log.write("usb", rec.clone());
        let event = rec["event"].as_str().unwrap_or_default();
        if show && (event == "S" || event == "C") && rec.contains_key("hex") {
            let out_dir = rec["dir"] == "out";
            if out_dir || event == "C" {
                println!(
                    "{} {:9} ep{} {} {}",
                    logger::local_hms(logger::now()),
                    rec["type"].as_str().unwrap_or_default(),
                    rec["ep"],
                    if out_dir { "->" } else { "<-" },
                    rec["hex"].as_str().unwrap_or_default()
                );
            }
        }
    }
    log.close();
    // hand the file to the user who ran sudo
    if let (Ok(uid), gid) = (std::env::var("SUDO_UID"), std::env::var("SUDO_GID")) {
        if let Ok(uid) = uid.parse::<u32>() {
            let gid = gid.ok().and_then(|g| g.parse().ok()).unwrap_or(uid);
            let _ = std::os::unix::fs::chown(&log_path, Some(uid), Some(gid));
        }
    }
    Ok(0)
}
