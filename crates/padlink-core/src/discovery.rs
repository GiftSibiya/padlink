//! Controller discovery via udev/sysfs.
//!
//! Everything here works without access to the device nodes themselves: names,
//! IDs, capability bitmasks and HID report descriptors all come from sysfs, which
//! is world-readable. Node permissions are reported so the doctor can explain
//! what is missing.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{CString, OsStr};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::codes::{EV_FF, FF_GAIN, code_name};
use crate::hex::parse_bitmask;
use crate::hidraw::HidrawDevice;

const FF_AUTOCENTER: u32 = 0x61;

pub const PADLINK_VIRTUAL_PHYS: &str = "padlink/virtual";

pub fn bus_name(bustype: u32) -> String {
    match bustype {
        0x03 => "usb".into(),
        0x05 => "bluetooth".into(),
        0x06 => "virtual".into(),
        0x18 => "i2c".into(),
        0x19 => "host".into(),
        other => format!("0x{other:02x}"),
    }
}

/// Vendor/product pairs worth recognising by name in the UI and doctor.
pub fn known_device(vendor: u16, product: u16) -> Option<&'static str> {
    Some(match (vendor, product) {
        (0x3537, 0x1041) => "GameSir / Zikway 2.4G dongle (HID mode)",
        (0x045E, 0x028E) => "Xbox 360 controller (or XInput-mode clone)",
        (0x054C, 0x05C4) => "Sony DualShock 4 (v1)",
        (0x054C, 0x09CC) => "Sony DualShock 4 (v2)",
        (0x054C, 0x0CE6) => "Sony DualSense",
        _ => return None,
    })
}

fn access(path: &Path, mode: libc::c_int) -> bool {
    let Ok(c) = CString::new(path.as_os_str().as_bytes()) else { return false };
    // SAFETY: c is a valid NUL-terminated path.
    unsafe { libc::access(c.as_ptr(), mode) == 0 }
}

fn text(s: Option<&OsStr>) -> Option<String> {
    s.map(|s| s.to_string_lossy().trim().to_string())
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// The number at the end of a node name (`/dev/input/event12` -> 12).
fn node_number(path: &str, stem: &str) -> u64 {
    path.rsplit(stem).next().and_then(|n| n.parse().ok()).unwrap_or(u64::MAX)
}

#[derive(Debug, Clone)]
pub struct EventNode {
    pub path: String,
    pub sys_path: String,
    pub name: String,
    pub input_sys_path: String,
    /// "ev", "key", "abs", "ff", "rel" capability bits.
    pub caps: BTreeMap<String, BTreeSet<u32>>,
    /// udev ID_INPUT* properties.
    pub props: BTreeMap<String, String>,
}

impl EventNode {
    pub fn readable(&self) -> bool {
        access(Path::new(&self.path), libc::R_OK)
    }

    pub fn writable(&self) -> bool {
        access(Path::new(&self.path), libc::W_OK)
    }

    pub fn is_joystick(&self) -> bool {
        self.props.get("ID_INPUT_JOYSTICK").is_some_and(|v| v == "1")
    }

    fn cap(&self, kind: &str) -> BTreeSet<u32> {
        self.caps.get(kind).cloned().unwrap_or_default()
    }

    pub fn has_ff(&self) -> bool {
        // EV_FF alone isn't enough: uinput sets it whenever ff_effects_max > 0
        let ff = self.cap("ff");
        let effects = ff.iter().any(|&c| c != FF_GAIN as u32 && c != FF_AUTOCENTER);
        self.cap("ev").contains(&(EV_FF as u32)) && effects
    }

    pub fn ff_names(&self) -> Vec<String> {
        self.cap("ff").iter().map(|&c| code_name(EV_FF, c as u16)).collect()
    }

    pub fn role(&self) -> &'static str {
        if self.is_joystick() {
            return "joystick";
        }
        for (prop, role) in [
            ("ID_INPUT_ACCELEROMETER", "motion"),
            ("ID_INPUT_TOUCHPAD", "touchpad"),
            ("ID_INPUT_MOUSE", "mouse"),
            ("ID_INPUT_KEYBOARD", "keyboard"),
            ("ID_INPUT_KEY", "keys"),
        ] {
            if self.props.get(prop).is_some_and(|v| v == "1") {
                return role;
            }
        }
        "other"
    }

    pub fn to_value(&self) -> Value {
        json!({
            "path": self.path,
            "name": self.name,
            "role": self.role(),
            "readable": self.readable(),
            "writable": self.writable(),
            "ff": self.ff_names(),
            "buttons": self.cap("key").len(),
            "axes": self.cap("abs").len(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct HidrawNode {
    pub path: String,
    pub sys_path: String,
    pub hid_sys_path: String,
    /// USB interface number as sysfs prints it (`"00"`).
    pub interface: Option<String>,
}

impl HidrawNode {
    pub fn readable(&self) -> bool {
        access(Path::new(&self.path), libc::R_OK)
    }

    pub fn writable(&self) -> bool {
        access(Path::new(&self.path), libc::W_OK)
    }

    /// The report descriptor from sysfs (world-readable), else through the node.
    pub fn descriptor(&self) -> io::Result<Vec<u8>> {
        std::fs::read(Path::new(&self.hid_sys_path).join("report_descriptor"))
            .or_else(|_| HidrawDevice::open(&self.path, false)?.report_descriptor())
    }

    pub fn to_value(&self) -> Value {
        json!({
            "path": self.path,
            "interface": self.interface,
            "readable": self.readable(),
            "writable": self.writable(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct Battery {
    pub name: String,
    pub capacity: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Controller {
    /// sysfs path of the physical root (USB device, HID device or input device).
    pub key: String,
    pub name: String,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
    pub bustype: u32,
    pub driver: Option<String>,
    pub uniq: String,
    pub phys: String,
    pub virtual_: bool,
    pub events: Vec<EventNode>,
    pub joysticks: Vec<String>,
    pub hidraw: Vec<HidrawNode>,
    pub battery: Option<Battery>,
    pub index: usize,
}

impl Controller {
    pub fn bus(&self) -> String {
        if self.virtual_ { "virtual".into() } else { bus_name(self.bustype) }
    }

    pub fn id_str(&self) -> String {
        format!("{:04x}:{:04x}", self.vendor, self.product)
    }

    pub fn known_as(&self) -> Option<&'static str> {
        known_device(self.vendor, self.product)
    }

    pub fn is_padlink(&self) -> bool {
        self.phys.starts_with(PADLINK_VIRTUAL_PHYS)
    }

    pub fn primary_event(&self) -> Option<&EventNode> {
        self.events.iter().find(|n| n.is_joystick()).or(self.events.first())
    }

    pub fn has_ff(&self) -> bool {
        self.events.iter().any(EventNode::has_ff)
    }

    pub fn ff_node(&self) -> Option<&EventNode> {
        self.events.iter().find(|n| n.has_ff())
    }

    /// Every device node of the controller (event, hidraw, js).
    pub fn node_paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self.events.iter().map(|n| n.path.clone()).collect();
        paths.extend(self.hidraw.iter().map(|n| n.path.clone()));
        paths.extend(self.joysticks.iter().cloned());
        paths
    }

    pub fn to_value(&self) -> Value {
        json!({
            "index": self.index,
            "name": self.name,
            "id": self.id_str(),
            "known_as": self.known_as(),
            "bus": self.bus(),
            "driver": self.driver,
            "uniq": self.uniq,
            "phys": self.phys,
            "virtual": self.virtual_,
            "padlink_virtual": self.is_padlink(),
            "sys_path": self.key,
            "has_ff": self.has_ff(),
            "events": self.events.iter().map(EventNode::to_value).collect::<Vec<_>>(),
            "joysticks": self.joysticks,
            "hidraw": self.hidraw.iter().map(HidrawNode::to_value).collect::<Vec<_>>(),
            "battery": self.battery.as_ref().map(|b| json!({"name": b.name, "capacity": b.capacity, "status": b.status})),
        })
    }
}

fn list(subsystem: &str) -> io::Result<Vec<udev::Device>> {
    let mut e = udev::Enumerator::new()?;
    e.match_subsystem(subsystem)?;
    Ok(e.scan_devices()?.collect())
}

fn parent(dev: &udev::Device, subsystem: &str, devtype: Option<&str>) -> Option<udev::Device> {
    match devtype {
        Some(t) => dev.parent_with_subsystem_devtype(subsystem, t).ok().flatten(),
        None => dev.parent_with_subsystem(subsystem).ok().flatten(),
    }
}

fn attr(dev: &udev::Device, name: &str) -> Option<String> {
    text(dev.attribute_value(name))
}

fn root_for(dev: &udev::Device) -> Option<udev::Device> {
    parent(dev, "usb", Some("usb_device")).or_else(|| parent(dev, "hid", None)).or_else(|| dev.parent())
}

fn driver_for(dev: &udev::Device) -> Option<String> {
    let driver = |d: Option<udev::Device>| d.and_then(|d| text(d.driver())).filter(|s| !s.is_empty());
    driver(parent(dev, "hid", None)).or_else(|| driver(parent(dev, "usb", Some("usb_interface"))))
}

fn event_node(dev: &udev::Device) -> Option<EventNode> {
    let inp = dev.parent()?;
    let caps = ["ev", "key", "abs", "ff", "rel"]
        .into_iter()
        .map(|k| (k.to_string(), parse_bitmask(attr(&inp, &format!("capabilities/{k}")).as_deref())))
        .collect();
    let props = dev
        .properties()
        .filter_map(|p| {
            let name = p.name().to_string_lossy();
            name.starts_with("ID_INPUT").then(|| (name.into_owned(), p.value().to_string_lossy().into_owned()))
        })
        .collect();
    Some(EventNode {
        path: path_str(dev.devnode()?),
        sys_path: path_str(dev.syspath()),
        name: attr(&inp, "name").filter(|s| !s.is_empty()).unwrap_or_else(|| "?".into()),
        input_sys_path: path_str(inp.syspath()),
        caps,
        props,
    })
}

fn battery_for(root: &str) -> Option<Battery> {
    let prefix = format!("{root}/");
    list("power_supply").ok()?.into_iter().find(|ps| path_str(ps.syspath()).starts_with(&prefix)).map(|ps| Battery {
        name: ps.sysname().to_string_lossy().into_owned(),
        capacity: attr(&ps, "capacity"),
        status: attr(&ps, "status"),
    })
}

fn is_node(dev: &udev::Device, stem: &str) -> bool {
    dev.sysname().to_string_lossy().starts_with(stem) && dev.devnode().is_some()
}

/// Find game controllers.
///
/// A controller is everything under one physical root (USB device, HID device,
/// or virtual input device) that has at least one joystick event node. With
/// `include_all`, any root owning a HID-backed or virtual event node is listed,
/// which helps when a dongle is misclassified by udev.
pub fn scan(include_all: bool) -> io::Result<Vec<Controller>> {
    let inputs = list("input")?;
    let events: Vec<&udev::Device> = inputs.iter().filter(|d| is_node(d, "event")).collect();
    let mut roots: BTreeMap<String, ()> = BTreeMap::new();
    for dev in &events {
        let is_joy = dev.property_value("ID_INPUT_JOYSTICK").is_some_and(|v| v == "1");
        let hid_backed = parent(dev, "hid", None).is_some() || parent(dev, "usb", None).is_some();
        if is_joy || (include_all && hid_backed) {
            if let Some(root) = root_for(dev) {
                roots.insert(path_str(root.syspath()), ());
            }
        }
    }
    let hidraws = list("hidraw")?;
    let joysticks: Vec<&udev::Device> = inputs.iter().filter(|d| is_node(d, "js")).collect();

    let mut controllers = Vec::new();
    for root_path in roots.keys() {
        let prefix = format!("{root_path}/");
        let under = |d: &udev::Device| path_str(d.syspath()).starts_with(&prefix);
        let mut nodes: Vec<(EventNode, &udev::Device)> =
            events.iter().filter(|d| under(d)).filter_map(|d| event_node(d).map(|n| (n, *d))).collect();
        nodes.sort_by_key(|(n, _)| (!n.is_joystick(), node_number(&n.path, "event")));
        let Some((primary, pdev)) = nodes.iter().find(|(n, _)| n.is_joystick()).or(nodes.first()) else {
            continue;
        };
        let inp = pdev.parent();
        let inp_attr = |name: &str| inp.as_ref().and_then(|i| attr(i, name));
        let hexattr = |name: &str| {
            u32::from_str_radix(&inp_attr(&format!("id/{name}")).unwrap_or_else(|| "0".into()), 16).unwrap_or(0)
        };

        let mut hid_nodes: Vec<HidrawNode> = hidraws
            .iter()
            .filter(|h| under(h))
            .filter_map(|h| {
                let hid = parent(h, "hid", None);
                let intf = parent(h, "usb", Some("usb_interface"));
                Some(HidrawNode {
                    path: path_str(h.devnode()?),
                    sys_path: path_str(h.syspath()),
                    hid_sys_path: path_str(hid.as_ref().map_or(h.syspath(), |d| d.syspath())),
                    interface: intf.and_then(|i| attr(&i, "bInterfaceNumber")),
                })
            })
            .collect();
        hid_nodes.sort_by_key(|n| node_number(&n.path, "hidraw"));

        let mut js: Vec<String> =
            joysticks.iter().filter(|d| under(d)).filter_map(|d| d.devnode().map(path_str)).collect();
        js.sort();

        controllers.push(Controller {
            key: root_path.clone(),
            name: primary.name.clone(),
            vendor: hexattr("vendor") as u16,
            product: hexattr("product") as u16,
            version: hexattr("version") as u16,
            bustype: hexattr("bustype"),
            driver: driver_for(pdev),
            uniq: inp_attr("uniq").unwrap_or_default(),
            phys: inp_attr("phys").unwrap_or_default(),
            virtual_: root_path.starts_with("/sys/devices/virtual/input"),
            events: nodes.iter().map(|(n, _)| n.clone()).collect(),
            joysticks: js,
            hidraw: hid_nodes,
            battery: battery_for(root_path),
            index: 0,
        });
    }
    for (i, c) in controllers.iter_mut().enumerate() {
        c.index = i;
    }
    Ok(controllers)
}

#[derive(Debug)]
pub struct NoSuchController(pub String);

impl std::fmt::Display for NoSuchController {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NoSuchController {}

/// Pick a controller by index, vid:pid, device node path, or name substring.
/// With no spec, the first non-padlink controller is chosen.
pub fn resolve(spec: Option<&str>, controllers: Vec<Controller>) -> Result<Controller, NoSuchController> {
    if controllers.is_empty() {
        return Err(NoSuchController("no controllers found (try --all, or check `padlink doctor`)".into()));
    }
    let Some(spec) = spec else {
        let first_real = controllers.iter().position(|c| !c.is_padlink()).unwrap_or(0);
        return Ok(controllers.into_iter().nth(first_real).expect("non-empty"));
    };
    if !spec.is_empty() && spec.bytes().all(|b| b.is_ascii_digit()) {
        if let Ok(i) = spec.parse::<usize>() {
            if i < controllers.len() {
                return Ok(controllers.into_iter().nth(i).expect("in range"));
            }
        }
    }
    let low = spec.to_lowercase();
    let pick = |pred: &dyn Fn(&Controller) -> bool| controllers.iter().position(pred);
    let found = pick(&|c| low == c.id_str())
        .or_else(|| pick(&|c| c.node_paths().iter().any(|p| p == spec)))
        .or_else(|| pick(&|c| c.name.to_lowercase().contains(&low)));
    match found {
        Some(i) => Ok(controllers.into_iter().nth(i).expect("found")),
        None => Err(NoSuchController(format!("no controller matches '{spec}'"))),
    }
}

/// A process holding some of the given nodes open.
#[derive(Debug, Clone)]
pub struct Holder {
    pub pid: u32,
    pub comm: String,
    pub nodes: Vec<String>,
}

/// Processes (that we are allowed to inspect) holding any of the given nodes open.
pub fn holders(paths: &[String]) -> Vec<Holder> {
    let targets: BTreeSet<u64> = paths.iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.rdev()).collect();
    let me = std::process::id();
    let mut found = Vec::new();
    let Ok(procs) = std::fs::read_dir("/proc") else { return found };
    for entry in procs.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
        if pid == me {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else { continue };
        let mut hits = BTreeSet::new();
        for fd in fds.flatten() {
            let Ok(st) = std::fs::metadata(fd.path()) else { continue };
            if st.rdev() != 0 && targets.contains(&st.rdev()) {
                if let Ok(link) = std::fs::read_link(fd.path()) {
                    hits.insert(path_str(&link));
                }
            }
        }
        if !hits.is_empty() {
            let comm = std::fs::read_to_string(entry.path().join("comm"))
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| "?".into());
            found.push(Holder { pid, comm, nodes: hits.into_iter().collect() });
        }
    }
    found
}

/// Python's `list --json` output shape.
pub fn controllers_value(ctrls: &[Controller]) -> Value {
    Value::Array(ctrls.iter().map(Controller::to_value).collect())
}

/// Info dict with each hidraw node's descriptor, as `padlink info --json` prints it.
pub fn info_value(c: &Controller) -> Value {
    let mut v = c.to_value();
    let descriptors: Map<String, Value> = c
        .hidraw
        .iter()
        .map(|h| {
            let d = match h.descriptor() {
                Ok(raw) => crate::hex::hexstr(&raw),
                Err(e) => format!("error: {e}"),
            };
            (h.path.clone(), Value::String(d))
        })
        .collect();
    v.as_object_mut().expect("object").insert("descriptors".into(), Value::Object(descriptors));
    v
}
