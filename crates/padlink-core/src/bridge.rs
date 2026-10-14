//! Rumble bridge: physical controller -> virtual uinput pad with force feedback.
//!
//! Games (through Wine/Proton/SDL) see the virtual pad. It has FF_RUMBLE, so
//! rumble requests reach padlink through the uinput FF upload/play protocol, and
//! padlink forwards them to the real controller:
//!   - with a profile:   hidraw output report (devices without kernel FF)
//!   - native FF device: evdev FF on the real node (useful for testing with a DS4)
//!   - otherwise:        log only, which still shows what the game asked for
//!
//! What the virtual pad looks like (mode):
//!   `<persona>`  spoof a known controller (see personas.rs). Input is translated
//!                with an input map into canonical Xbox 360 events, then
//!                re-encoded the way that controller's Linux driver would.
//!   `mirror`     clone the source's capabilities 1:1 and add FF.
//!
//! Hide the physical device from games (SDL_GAMECONTROLLER_IGNORE_DEVICES, see
//! `padlink lutris-env`) so they only see the virtual pad. The bridge also grabs
//! the physical event node so no other reader receives its input.

use std::collections::HashMap;
use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use evdev::raw_stream::RawDevice;
use evdev::uinput::VirtualDevice;
use evdev::{
    AbsoluteAxisCode, AttributeSet, BusType, EventSummary, FFEffectCode, FFEffectData, FFEffectKind, InputId, KeyCode,
    UInputCode, UinputAbsSetup,
};
use serde_json::Value;

use crate::codes::*;
use crate::discovery::{self, Controller, PADLINK_VIRTUAL_PHYS};
use crate::ff::{Effect, RawEffect, RawParams};
use crate::hidraw::{poll_many, set_nonblocking};
use crate::personas::{self, Persona};
use crate::profiles::{self, Profile, RumbleSpec};
use crate::sink::{EvdevSink, HidrawSink, NullSink, RumbleSink};
use crate::translate::{Translator, heuristic_map};
use crate::{driver::RumbleDriver, log_write, logger::Log};

/// The force-feedback capabilities every virtual pad advertises.
pub const FF_CAPS: [u16; 8] = [FF_RUMBLE, FF_PERIODIC, FF_SQUARE, FF_TRIANGLE, FF_SINE, FF_CONSTANT, FF_RAMP, FF_GAIN];
const MAX_EFFECTS: u32 = 16;

/// The hidraw node a rumble profile targets.
pub fn pick_hidraw<'a>(ctrl: &'a Controller, spec: &RumbleSpec) -> Option<&'a str> {
    if let Some(intf) = &spec.interface {
        if let Some(n) = ctrl.hidraw.iter().find(|n| n.interface.as_ref() == Some(intf)) {
            return Some(&n.path);
        }
    }
    ctrl.hidraw.first().map(|n| n.path.as_str())
}

/// Where rumble goes, and the profile's timing if it is a hidraw profile.
pub fn make_sink(
    ctrl: &Controller,
    profile: Option<&Profile>,
    log: &Log,
) -> io::Result<(Box<dyn RumbleSink>, Option<RumbleSpec>)> {
    if let Some(spec) = profile.and_then(|p| p.rumble.as_ref()).filter(|s| s.transport == "hidraw") {
        if let Some(path) = pick_hidraw(ctrl, spec) {
            match HidrawSink::open(Path::new(path), spec.clone(), log.clone()) {
                Ok(sink) => return Ok((Box::new(sink), Some(spec.clone()))),
                Err(e) => {
                    log_write!(log, "note", {"text": format!(
                        "cannot open {path} for rumble ({e}); FF requests will only be logged. \
                         Install the udev rule: padlink udev-rule {}", ctrl.id_str())});
                    return Ok((Box::new(NullSink), None));
                }
            }
        }
    }
    if let Some(node) = ctrl.ff_node() {
        return Ok((Box::new(EvdevSink::open(Path::new(&node.path))?), None));
    }
    Ok((Box::new(NullSink), None))
}

/// How to build the bridge.
#[derive(Debug, Clone)]
pub struct BridgeOptions {
    /// A persona id, or "mirror".
    pub mode: String,
    pub grab: bool,
    /// Log every input event (verbose).
    pub log_input: bool,
    /// Input map overriding the profile/heuristic one.
    pub mapping: Option<Value>,
    pub vendor: Option<u16>,
    pub product: Option<u16>,
    pub name: Option<String>,
}

impl Default for BridgeOptions {
    fn default() -> Self {
        BridgeOptions {
            mode: personas::DEFAULT.into(),
            grab: true,
            log_input: false,
            mapping: None,
            vendor: None,
            product: None,
            name: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Stats {
    pub uploads: u64,
    pub plays: u64,
    pub inputs: u64,
}

/// What a running bridge exposes to other threads.
#[derive(Debug, Default)]
pub struct Status {
    pub error: Option<String>,
    pub virtual_path: Option<String>,
    pub stats: Stats,
}

struct Running {
    node: String,
    src: RawDevice,
    ui: VirtualDevice,
    driver: RumbleDriver,
    translator: Option<Translator>,
    persona: Option<&'static Persona>,
}

/// One bridge session. `run()` blocks; `spawn()` runs it on a thread.
pub struct Bridge {
    ctrl: Controller,
    log: Log,
    opts: BridgeOptions,
    persona: Option<&'static Persona>,
    status: Arc<Mutex<Status>>,
    stop: Stopper,
}

/// Wakes a bridge's `run()` loop and makes it return. Cheap to clone.
///
/// It owns both ends of the wake-up pipe, so writing never hits a pipe whose
/// read end is gone (which would raise SIGPIPE).
#[derive(Clone)]
pub struct Stopper(Arc<(OwnedFd, OwnedFd)>);

impl Stopper {
    fn read_fd(&self) -> std::os::fd::RawFd {
        self.0.0.as_raw_fd()
    }

    pub fn stop(&self) {
        // SAFETY: writing one byte from a valid buffer to our own non-blocking pipe.
        unsafe { libc::write(self.0.1.as_raw_fd(), b"x".as_ptr().cast(), 1) };
    }
}

/// Handle to a bridge running on its own thread.
pub struct BridgeHandle {
    thread: Option<JoinHandle<()>>,
    stopper: Stopper,
    pub status: Arc<Mutex<Status>>,
}

impl BridgeHandle {
    pub fn stop(&mut self) {
        if let Some(t) = self.thread.take() {
            self.stopper.stop();
            let _ = t.join();
        }
    }

    pub fn alive(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }

    pub fn error(&self) -> Option<String> {
        self.status.lock().expect("status").error.clone()
    }
}

impl Drop for BridgeHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Write events to a uinput device exactly as given (no implicit SYN_REPORT).
fn write_events(ui: &VirtualDevice, events: &[Event]) -> io::Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    let raw: Vec<libc::input_event> = events
        .iter()
        .map(|&(t, c, v)| libc::input_event {
            time: libc::timeval { tv_sec: 0, tv_usec: 0 },
            type_: t,
            code: c,
            value: v,
        })
        .collect();
    let bytes = std::mem::size_of_val(raw.as_slice());
    // SAFETY: raw is a contiguous array of input_event, `bytes` long.
    let n = unsafe { libc::write(ui.as_raw_fd(), raw.as_ptr().cast(), bytes) };
    if n < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

/// FFEffectData from the evdev crate -> the kernel struct fields padlink uses.
fn raw_effect(id: i16, data: &FFEffectData) -> RawEffect {
    let (etype, params) = match data.kind {
        FFEffectKind::Rumble { strong_magnitude, weak_magnitude } => {
            (FF_RUMBLE, RawParams::Rumble { strong: strong_magnitude, weak: weak_magnitude })
        }
        FFEffectKind::Periodic { waveform, period, magnitude, offset, .. } => {
            (FF_PERIODIC, RawParams::Periodic { waveform: FFEffectCode::from(waveform).0, period, magnitude, offset })
        }
        FFEffectKind::Constant { level, .. } => (FF_CONSTANT, RawParams::Constant { level }),
        FFEffectKind::Ramp { start_level, end_level, .. } => (FF_RAMP, RawParams::Ramp { start_level, end_level }),
        FFEffectKind::Spring { .. } => (FF_SPRING, RawParams::Other),
        FFEffectKind::Friction { .. } => (FF_FRICTION, RawParams::Other),
        FFEffectKind::Damper => (FF_DAMPER, RawParams::Other),
        FFEffectKind::Inertia => (FF_INERTIA, RawParams::Other),
    };
    RawEffect { etype, id, direction: data.direction, length: data.replay.length, delay: data.replay.delay, params }
}

impl Bridge {
    pub fn new(ctrl: Controller, log: Log, opts: BridgeOptions) -> Result<Bridge, String> {
        let persona = if opts.mode == "mirror" { None } else { Some(personas::get(&opts.mode)?) };
        let mut fds = [0; 2];
        // SAFETY: fds has room for the two descriptors pipe2 writes.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error().to_string());
        }
        // SAFETY: pipe2 just returned these two fresh descriptors; nothing else owns them.
        let (stop_r, stop_w) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        Ok(Bridge { ctrl, log, opts, persona, status: Arc::default(), stop: Stopper(Arc::new((stop_r, stop_w))) })
    }

    pub fn status(&self) -> Arc<Mutex<Status>> {
        Arc::clone(&self.status)
    }

    /// Run on a thread named padlink-bridge.
    pub fn spawn(self) -> BridgeHandle {
        let status = self.status();
        let stopper = self.stopper();
        let thread = thread::Builder::new()
            .name("padlink-bridge".into())
            .spawn(move || {
                let mut b = self;
                b.run();
            })
            .expect("spawn bridge thread");
        BridgeHandle { thread: Some(thread), stopper, status }
    }

    /// Something that stops `run()` from another thread.
    pub fn stopper(&self) -> Stopper {
        self.stop.clone()
    }

    fn open(&self) -> Result<Running, Box<dyn std::error::Error>> {
        let node = self.ctrl.primary_event().ok_or("controller has no event node")?.path.clone();
        let mut src = RawDevice::open(&node).map_err(|e| io::Error::new(e.kind(), format!("{node}: {e}")))?;
        let keys: Vec<u16> = src.supported_keys().map(|k| k.iter().map(|k| k.0).collect()).unwrap_or_default();
        let absinfo: HashMap<u16, AbsInfo> = src
            .get_absinfo()?
            .map(|(code, a)| {
                (code.0, AbsInfo::new(a.value(), a.minimum(), a.maximum(), a.fuzz(), a.flat(), a.resolution()))
            })
            .collect();
        let profile = profiles::load(self.ctrl.vendor, self.ctrl.product, None)?;
        let source_tag = self.persona.map_or("mirror", |p| p.id);
        let tag = CString::new(format!("{PADLINK_VIRTUAL_PHYS}/{}/{source_tag}", self.ctrl.id_str()))?;
        let ff: AttributeSet<FFEffectCode> = FF_CAPS.iter().map(|&c| FFEffectCode(c)).collect();

        let (ui, translator) = match self.persona {
            None => {
                let name = self.opts.name.clone().unwrap_or_else(|| format!("{} (padlink)", src.name().unwrap_or("?")));
                let id = InputId::new(
                    BusType::BUS_USB,
                    self.opts.vendor.unwrap_or(self.ctrl.vendor),
                    self.opts.product.unwrap_or(self.ctrl.product),
                    0x0110,
                );
                let mut b = VirtualDevice::builder()?.name(name.as_bytes()).input_id(id).with_phys(&tag)?;
                if let Some(k) = src.supported_keys() {
                    b = b.with_keys(k)?;
                }
                for (code, info) in src.get_absinfo()? {
                    b = b.with_absolute_axis(&UinputAbsSetup::new(code, info))?;
                }
                if let Some(r) = src.supported_relative_axes() {
                    b = b.with_relative_axes(r)?;
                }
                if let Some(m) = src.misc_properties() {
                    b = b.with_msc(m)?;
                }
                if let Some(s) = src.supported_switches() {
                    b = b.with_switches(s)?;
                }
                (b.with_ff(&ff)?.with_ff_effects_max(MAX_EFFECTS).build()?, None)
            }
            Some(p) => {
                let (mapping, source) = match (&self.opts.mapping, profile.as_ref().and_then(|p| p.input_map.clone())) {
                    (Some(m), _) => (m.clone(), "override"),
                    (None, Some(m)) => (m, "profile"),
                    (None, None) => (
                        heuristic_map(
                            &keys.iter().copied().collect(),
                            &absinfo.keys().copied().collect(),
                            self.ctrl.driver.as_deref(),
                        ),
                        "heuristic",
                    ),
                };
                let translator = Translator::new(&mapping, &absinfo)?;
                let caps = p.capabilities();
                let key_set: AttributeSet<KeyCode> = caps.keys.iter().map(|&k| KeyCode(k)).collect();
                let name = self.opts.name.clone().unwrap_or_else(|| p.name.to_string());
                let (vendor, product) = (self.opts.vendor.unwrap_or(p.vendor), self.opts.product.unwrap_or(p.product));
                let id = InputId::new(BusType(p.bustype), vendor, product, p.version);
                let mut b = VirtualDevice::builder()?
                    .name(name.as_bytes())
                    .input_id(id)
                    .with_phys(&tag)?
                    .with_keys(&key_set)?;
                for (code, a) in &caps.abs {
                    let info = evdev::AbsInfo::new(a.value, a.min, a.max, a.fuzz, a.flat, a.resolution);
                    b = b.with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode(*code), info))?;
                }
                let ui = b.with_ff(&ff)?.with_ff_effects_max(MAX_EFFECTS).build()?;
                if (vendor, product) == (self.ctrl.vendor, self.ctrl.product) {
                    log_write!(self.log, "note", {"text": "virtual pad has the same vendor:product as the physical one; \
                        SDL_GAMECONTROLLER_IGNORE_DEVICES would hide both. Pick another persona."});
                }
                log_write!(self.log, "note", {"text": "input map", "mapping": mapping, "source": source});
                (ui, Some(translator))
            }
        };
        let mut ui = ui;
        let virtual_path = virtual_node(&mut ui);
        self.status.lock().expect("status").virtual_path = virtual_path.clone();
        let (spoof_v, spoof_p) = match self.persona {
            Some(p) => (self.opts.vendor.unwrap_or(p.vendor), self.opts.product.unwrap_or(p.product)),
            None => (self.opts.vendor.unwrap_or(self.ctrl.vendor), self.opts.product.unwrap_or(self.ctrl.product)),
        };

        let (sink, spec) = make_sink(&self.ctrl, profile.as_ref(), &self.log)?;
        let (keepalive, min_interval) =
            spec.map_or((0, 8), |s| (s.keepalive_ms.max(0) as u64, s.min_interval_ms.max(0) as u64));
        let driver = RumbleDriver::start(sink, self.log.clone(), keepalive, min_interval);
        if self.opts.grab {
            if let Err(e) = src.grab() {
                log_write!(self.log, "note", {"text": format!("could not grab {node}: {e}")});
            }
        }
        log_write!(self.log, "note", {
            "text": "bridge up", "source": node, "virtual": virtual_path, "mode": self.opts.mode,
            "persona": self.persona.map_or("mirror", |p| p.label),
            "spoofing": format!("{spoof_v:04x}:{spoof_p:04x}"), "sink": driver.sink_name(),
        });
        set_nonblocking(src.as_raw_fd())?;
        set_nonblocking(ui.as_raw_fd())?;
        Ok(Running { node, src, ui, driver, translator, persona: self.persona })
    }

    fn handle_source(&self, r: &mut Running) -> io::Result<()> {
        let events: Vec<_> = match r.src.fetch_events() {
            Ok(evs) => evs.collect(),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(e) => return Err(e),
        };
        let mut out: Vec<Event> = Vec::new();
        let mut inputs = 0;
        for ev in events {
            let (t, c, v) = (ev.event_type().0, ev.code(), ev.value());
            if t == EV_SYN {
                out.push((EV_SYN, 0, 0));
                continue;
            }
            if self.opts.log_input && (t == EV_KEY || t == EV_ABS) {
                log_write!(self.log, "input", {"node": r.node, "type": t,
                    "code": c, "value": v, "name": code_name(t, c)});
            }
            inputs += 1;
            match (&mut r.translator, r.persona) {
                (Some(tr), Some(p)) => {
                    for (t1, c1, v1) in tr.translate(t, c, v) {
                        out.extend(p.convert(t1, c1, v1));
                    }
                }
                _ if t != EV_FF => out.push((t, c, v)),
                _ => {}
            }
        }
        self.status.lock().expect("status").stats.inputs += inputs;
        write_events(&r.ui, &out)
    }

    fn handle_uinput(&self, r: &mut Running) -> io::Result<()> {
        let events: Vec<_> = match r.ui.fetch_events() {
            Ok(evs) => evs.collect(),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(e) => return Err(e),
        };
        for ev in events {
            match ev.destructure() {
                EventSummary::UInput(ev, UInputCode::UI_FF_UPLOAD, _) => {
                    let mut upload = match r.ui.process_ff_upload(ev) {
                        Ok(u) => u,
                        Err(e) => {
                            // the requester gave up (timeout) before we got to it
                            log_write!(self.log, "note", {"text": format!("stale FF upload request: {e}")});
                            continue;
                        }
                    };
                    // effect() panics on waveforms the crate doesn't model; never leave the game's ioctl hanging
                    match catch_unwind(AssertUnwindSafe(|| upload.effect())) {
                        Ok(data) => {
                            let eff = Effect::from_raw(&raw_effect(upload.effect_id(), &data));
                            let (id, record) = (eff.id, eff.record.clone());
                            r.driver.upload(eff);
                            self.status.lock().expect("status").stats.uploads += 1;
                            log_write!(self.log, "ff_upload", {"id": id, "effect": record});
                            upload.set_retval(0);
                        }
                        Err(_) => {
                            upload.set_retval(-libc::EINVAL);
                            log_write!(self.log, "note", {"text": "upload failed: unsupported effect"});
                        }
                    }
                    // dropping `upload` sends UI_END_FF_UPLOAD
                }
                EventSummary::UInput(ev, UInputCode::UI_FF_ERASE, _) => {
                    let mut erase = match r.ui.process_ff_erase(ev) {
                        Ok(e) => e,
                        Err(e) => {
                            log_write!(self.log, "note", {"text": format!("stale FF erase request: {e}")});
                            continue;
                        }
                    };
                    let id = erase.effect_id() as i32;
                    r.driver.erase(id);
                    log_write!(self.log, "ff_erase", {"id": id});
                    erase.set_retval(0);
                }
                EventSummary::ForceFeedback(_, FFEffectCode::FF_GAIN, value) => {
                    r.driver.set_gain(value as i64);
                    log_write!(self.log, "ff_gain", {"gain": value});
                }
                EventSummary::ForceFeedback(_, code, value) => {
                    r.driver.play(code.0 as i32, value);
                    self.status.lock().expect("status").stats.plays += 1;
                    log_write!(self.log, "ff_play", {"id": code.0, "value": value});
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Bridge until stopped or the controller goes away. Errors end up in `status().error`.
    pub fn run(&mut self) {
        let mut running: Option<Running> = None;
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            let r = running.insert(self.open()?);
            loop {
                let fds = [r.src.as_raw_fd(), r.ui.as_raw_fd(), self.stop.read_fd()];
                let ready = poll_many(&fds, None)?;
                if ready.contains(&2) {
                    return Ok(());
                }
                if ready.contains(&0) {
                    self.handle_source(r)?;
                }
                if ready.contains(&1) {
                    self.handle_uinput(r)?;
                }
            }
        })();
        if let Err(e) = result {
            let error = match e.downcast_ref::<io::Error>() {
                Some(io) if io.raw_os_error() == Some(libc::ENODEV) => "controller disconnected".to_string(),
                _ => e.to_string(),
            };
            log_write!(self.log, "note", {"text": format!("bridge stopped: {error}")});
            self.status.lock().expect("status").error = Some(error);
        }
        if let Some(mut r) = running {
            r.driver.stop();
            if self.opts.grab {
                let _ = r.src.ungrab();
            }
        }
        let s = self.status.lock().expect("status").stats;
        log_write!(self.log, "note", {"text": "bridge down", "stats": {"uploads": s.uploads, "plays": s.plays, "inputs": s.inputs}});
    }
}

fn virtual_node(ui: &mut VirtualDevice) -> Option<String> {
    for _ in 0..50 {
        if let Ok(nodes) = ui.enumerate_dev_nodes_blocking() {
            let found = nodes
                .filter_map(Result::ok)
                .find(|p: &PathBuf| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("event")));
            if let Some(p) = found {
                return Some(p.display().to_string());
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
    None
}

/// Block until a controller matching `spec` appears (ignores padlink's own pads)
/// and its event node is readable.
pub fn wait_for(spec: Option<&str>, include_all: bool, poll: Duration, stop: &dyn Fn() -> bool) -> Option<Controller> {
    while !stop() {
        let ctrls: Vec<Controller> =
            discovery::scan(include_all).unwrap_or_default().into_iter().filter(|c| !c.is_padlink()).collect();
        if let Ok(c) = discovery::resolve(spec, ctrls) {
            if c.primary_event().is_some_and(|n| n.readable()) {
                return Some(c);
            }
        }
        let end = std::time::Instant::now() + poll;
        while std::time::Instant::now() < end && !stop() {
            thread::sleep(Duration::from_millis(50));
        }
    }
    None
}
