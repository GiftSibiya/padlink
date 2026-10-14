//! Rumble sinks: deliver (strong, weak) motor levels to hardware.
//!
//! - `HidrawSink`: a profile-defined output report on a hidraw node
//!   (devices without kernel force feedback)
//! - `EvdevSink`: a device that already has kernel FF (xpad, hid-playstation...)
//! - `NullSink`: log only

use std::io;
use std::path::Path;
use std::thread;
use std::time::Duration;

use evdev::{Device, FFEffect, FFEffectData, FFEffectKind, FFReplay, FFTrigger};

use crate::hex::hexstr;
use crate::hidraw::HidrawDevice;
use crate::log_write;
use crate::logger::Log;
use crate::profiles::RumbleSpec;

pub trait RumbleSink: Send {
    fn name(&self) -> String;
    /// Motor levels 0..65535.
    fn set(&mut self, strong: u16, weak: u16) -> io::Result<()>;
    /// Stop the motors and release the device.
    fn close(&mut self) {}
}

/// Write a profile-defined output report to a hidraw node.
pub struct HidrawSink {
    dev: HidrawDevice,
    spec: RumbleSpec,
    log: Log,
}

impl HidrawSink {
    pub fn open(path: &Path, spec: RumbleSpec, log: Log) -> io::Result<Self> {
        Ok(HidrawSink { dev: HidrawDevice::open(path, true)?, spec, log })
    }
}

impl RumbleSink for HidrawSink {
    fn name(&self) -> String {
        format!("hidraw:{}", self.dev.path.display())
    }

    fn set(&mut self, strong: u16, weak: u16) -> io::Result<()> {
        let data = self.spec.render(strong, weak).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let node = self.dev.path.display().to_string();
        match self.dev.write(&data) {
            Ok(_) => {
                log_write!(self.log, "hid_tx", {"node": node, "hex": hexstr(&data), "ok": true});
                Ok(())
            }
            Err(e) => {
                log_write!(self.log, "hid_tx", {"node": node, "hex": hexstr(&data), "ok": false, "error": e.to_string()});
                Err(e)
            }
        }
    }

    fn close(&mut self) {
        let _ = self.set(0, 0);
    }
}

fn rumble_effect(strong: u16, weak: u16, length: u16) -> FFEffectData {
    FFEffectData {
        direction: 0,
        trigger: FFTrigger { button: 0, interval: 0 },
        replay: FFReplay { length, delay: 0 },
        kind: FFEffectKind::Rumble { strong_magnitude: strong, weak_magnitude: weak },
    }
}

/// Drive a device that already has kernel FF support.
pub struct EvdevSink {
    path: String,
    dev: Device,
    effect: Option<FFEffect>,
}

impl EvdevSink {
    pub fn open(path: &Path) -> io::Result<Self> {
        Ok(EvdevSink { path: path.display().to_string(), dev: Device::open(path)?, effect: None })
    }
}

impl RumbleSink for EvdevSink {
    fn name(&self) -> String {
        format!("evdev:{}", self.path)
    }

    fn set(&mut self, strong: u16, weak: u16) -> io::Result<()> {
        if strong == 0 && weak == 0 {
            if let Some(e) = self.effect.as_mut() {
                e.stop()?;
            }
            return Ok(());
        }
        let data = rumble_effect(strong, weak, 0);
        match self.effect.as_mut() {
            Some(e) => e.update(data)?,
            None => self.effect = Some(self.dev.upload_ff_effect(data)?),
        }
        self.effect.as_mut().expect("uploaded").play(1)
    }

    fn close(&mut self) {
        if let Some(mut e) = self.effect.take() {
            let _ = e.stop();
            // dropping the handle erases the effect
        }
    }
}

/// Log-only: the bridge still records what the game asked for.
pub struct NullSink;

impl RumbleSink for NullSink {
    fn name(&self) -> String {
        "log-only".into()
    }

    fn set(&mut self, _strong: u16, _weak: u16) -> io::Result<()> {
        Ok(())
    }
}

/// Upload and play a single rumble effect through the kernel FF API.
pub fn evdev_rumble(path: &Path, strong: u16, weak: u16, ms: u16) -> io::Result<()> {
    let mut dev = Device::open(path)?;
    let mut effect = dev.upload_ff_effect(rumble_effect(strong, weak, ms))?;
    effect.play(1)?;
    thread::sleep(Duration::from_millis(ms as u64));
    effect.stop()
}
