//! Minimal HID report descriptor parser.
//!
//! Good enough to answer the questions padlink cares about: which input, output
//! and feature reports exist, how long they are, which usage pages they carry,
//! and whether the device speaks the PID (force feedback) usage page.

use std::collections::BTreeSet;

fn usage_page_table(page: u32) -> Option<&'static str> {
    Some(match page {
        0x01 => "Generic Desktop",
        0x02 => "Simulation",
        0x05 => "Game",
        0x06 => "Generic Device",
        0x07 => "Keyboard",
        0x08 => "LED",
        0x09 => "Button",
        0x0C => "Consumer",
        0x0F => "PID (force feedback)",
        _ => return None,
    })
}

fn generic_desktop(usage: u32) -> Option<&'static str> {
    Some(match usage {
        0x01 => "Pointer",
        0x02 => "Mouse",
        0x04 => "Joystick",
        0x05 => "Gamepad",
        0x06 => "Keyboard",
        0x08 => "Multi-axis",
        0x30 => "X",
        0x31 => "Y",
        0x32 => "Z",
        0x33 => "Rx",
        0x34 => "Ry",
        0x35 => "Rz",
        0x36 => "Slider",
        0x37 => "Dial",
        0x38 => "Wheel",
        0x39 => "Hat switch",
        0x3D => "Start",
        0x3E => "Select",
        0x90 => "D-pad up",
        0x91 => "D-pad down",
        0x92 => "D-pad right",
        0x93 => "D-pad left",
        _ => return None,
    })
}

fn simulation(usage: u32) -> Option<&'static str> {
    Some(match usage {
        0xBA => "Rudder",
        0xBB => "Throttle",
        0xC4 => "Accelerator",
        0xC5 => "Brake",
        _ => return None,
    })
}

fn pid(usage: u32) -> Option<&'static str> {
    Some(match usage {
        0x21 => "Set Effect Report",
        0x22 => "Effect Block Index",
        0x25 => "Effect Type",
        0x26 => "ET Constant Force",
        0x27 => "ET Ramp",
        0x30 => "ET Square",
        0x31 => "ET Sine",
        0x50 => "Duration",
        0x70 => "Magnitude",
        0x77 => "Effect Operation",
        0x7D => "Device Gain",
        0x96 => "Device Control",
        0x97 => "DC Enable Actuators",
        _ => return None,
    })
}

fn collection_type(kind: u32) -> String {
    match kind {
        0 => "Physical".into(),
        1 => "Application".into(),
        2 => "Logical".into(),
        3 => "Report".into(),
        4 => "Named Array".into(),
        5 => "Usage Switch".into(),
        6 => "Usage Modifier".into(),
        _ => format!("0x{kind:02x}"),
    }
}

pub fn usage_page_name(page: u32) -> String {
    if page >= 0xFF00 {
        return format!("Vendor 0x{page:04x}");
    }
    usage_page_table(page).map_or_else(|| format!("0x{page:02x}"), str::to_string)
}

pub fn usage_name(page: u32, usage: u32) -> String {
    if page == 0x09 {
        return format!("Button {usage}");
    }
    let name = match page {
        0x01 => generic_desktop(usage),
        0x02 => simulation(usage),
        0x0F => pid(usage),
        _ => None,
    };
    name.map_or_else(|| format!("0x{usage:02x}"), str::to_string)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReportKind {
    Input,
    Output,
    Feature,
}

impl ReportKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ReportKind::Input => "input",
            ReportKind::Output => "output",
            ReportKind::Feature => "feature",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub kind: ReportKind,
    pub report_id: u32,
    pub bit_offset: u64,
    pub size: u32,
    pub count: u32,
    pub usage_page: u32,
    pub usages: Vec<u32>,
    pub usage_range: Option<(u32, u32)>,
    pub logical_min: i64,
    pub logical_max: i64,
    pub flags: u32,
    pub collection: String,
}

impl Field {
    pub fn constant(&self) -> bool {
        self.flags & 0x01 != 0
    }

    pub fn variable(&self) -> bool {
        self.flags & 0x02 != 0
    }

    pub fn bits(&self) -> u64 {
        self.size as u64 * self.count as u64
    }

    pub fn describe(&self) -> String {
        let page = self.usage_page;
        let what = if self.constant() {
            "padding".to_string()
        } else if let Some((lo, hi)) = self.usage_range {
            format!("{}: {}..{}", usage_page_name(page), usage_name(page, lo), usage_name(page, hi))
        } else if !self.usages.is_empty() {
            let mut names = self.usages.iter().take(8).map(|&u| usage_name(page, u)).collect::<Vec<_>>().join(", ");
            if self.usages.len() > 8 {
                names.push_str(", ...");
            }
            format!("{}: {names}", usage_page_name(page))
        } else {
            usage_page_name(page)
        };
        format!(
            "byte {}.{} {}x{}bit [{}..{}] {} {what}",
            self.bit_offset / 8,
            self.bit_offset % 8,
            self.count,
            self.size,
            self.logical_min,
            self.logical_max,
            if self.variable() { "var" } else { "array" },
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub kind: ReportKind,
    pub report_id: u32,
    pub fields: Vec<Field>,
    pub bits: u64,
}

impl Report {
    pub fn payload_len(&self) -> u64 {
        self.bits.div_ceil(8)
    }

    /// Length including the report ID byte, as written to / read from hidraw.
    pub fn wire_len(&self) -> u64 {
        self.payload_len() + u64::from(self.report_id != 0)
    }

    pub fn usage_pages(&self) -> BTreeSet<u32> {
        self.fields.iter().filter(|f| !f.constant()).map(|f| f.usage_page).collect()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Descriptor {
    pub raw: Vec<u8>,
    pub reports: Vec<Report>,
    pub applications: Vec<String>,
    pub uses_report_ids: bool,
}

impl Descriptor {
    pub fn by_kind(&self, kind: ReportKind) -> impl Iterator<Item = &Report> {
        self.reports.iter().filter(move |r| r.kind == kind)
    }

    pub fn has_pid(&self) -> bool {
        self.reports.iter().any(|r| r.usage_pages().contains(&0x0F))
    }

    pub fn vendor_pages(&self) -> BTreeSet<u32> {
        self.reports.iter().flat_map(|r| r.usage_pages()).filter(|&p| p >= 0xFF00).collect()
    }

    pub fn summary(&self) -> Vec<String> {
        let vendor = self.vendor_pages().iter().map(|p| format!("0x{p:04x}")).collect::<Vec<_>>().join(", ");
        let apps = self.applications.join(", ");
        let mut lines = vec![
            format!("applications: {}", if apps.is_empty() { "-" } else { &apps }),
            format!(
                "report ids: {}  PID force feedback page: {}  vendor pages: {}",
                if self.uses_report_ids { "yes" } else { "no" },
                if self.has_pid() { "YES" } else { "no" },
                if vendor.is_empty() { "-" } else { &vendor },
            ),
        ];
        for r in &self.reports {
            lines.push(format!(
                "{:7} report id {:#04x}  {} bytes payload ({} on the wire)",
                r.kind.as_str(),
                r.report_id,
                r.payload_len(),
                r.wire_len()
            ));
            for f in &r.fields {
                lines.push(format!("    {}", f.describe()));
            }
        }
        lines
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Globals {
    page: u32,
    lmin: i64,
    lmax: i64,
    size: u32,
    count: u32,
    rid: u32,
}

fn signed(value: u32, nbytes: usize) -> i64 {
    let bits = nbytes * 8;
    if nbytes > 0 && value as u64 & (1 << (bits - 1)) != 0 { value as i64 - (1i64 << bits) } else { value as i64 }
}

pub fn parse(raw: &[u8]) -> Descriptor {
    let mut g = Globals::default();
    let mut stack: Vec<Globals> = Vec::new();
    let mut usages: Vec<u32> = Vec::new();
    let (mut umin, mut umax): (Option<u32>, Option<u32>) = (None, None);
    let mut local_page: Option<u32> = None; // page carried by a 32-bit extended usage
    let mut collections: Vec<String> = Vec::new();
    let mut applications: Vec<String> = Vec::new();
    let mut reports: Vec<Report> = Vec::new();
    let mut uses_ids = false;

    let mut i = 0;
    while i < raw.len() {
        let prefix = raw[i];
        if prefix == 0xFE {
            // long item
            if i + 1 >= raw.len() {
                break;
            }
            i += 3 + raw[i + 1] as usize;
            continue;
        }
        let size = [0, 1, 2, 4][(prefix & 3) as usize];
        let typ = (prefix >> 2) & 3;
        let tag = prefix >> 4;
        let data = &raw[(i + 1).min(raw.len())..(i + 1 + size).min(raw.len())];
        i += 1 + size;
        let uval = data.iter().rev().fold(0u32, |acc, &b| (acc << 8) | b as u32);
        let sval = signed(uval, data.len());

        match typ {
            0 => {
                // main
                match tag {
                    0x8 | 0x9 | 0xB => {
                        let kind = match tag {
                            0x8 => ReportKind::Input,
                            0x9 => ReportKind::Output,
                            _ => ReportKind::Feature,
                        };
                        let idx = match reports.iter().position(|r| r.kind == kind && r.report_id == g.rid) {
                            Some(idx) => idx,
                            None => {
                                reports.push(Report { kind, report_id: g.rid, fields: Vec::new(), bits: 0 });
                                reports.len() - 1
                            }
                        };
                        let rep = &mut reports[idx];
                        let field = Field {
                            kind,
                            report_id: g.rid,
                            bit_offset: rep.bits,
                            size: g.size,
                            count: g.count,
                            usage_page: local_page.filter(|&p| p != 0).unwrap_or(g.page),
                            usages: usages.iter().map(|u| u & 0xFFFF).collect(),
                            usage_range: umin.zip(umax),
                            logical_min: g.lmin,
                            logical_max: g.lmax,
                            flags: uval,
                            collection: collections.join("/"),
                        };
                        rep.bits += field.bits();
                        rep.fields.push(field);
                    }
                    0xA => {
                        let uname = usages.first().map_or_else(|| "?".to_string(), |&u| usage_name(g.page, u));
                        let label = format!("{}({}:{uname})", collection_type(uval), usage_page_name(g.page));
                        if uval == 1 {
                            applications.push(label.clone());
                        }
                        collections.push(label);
                    }
                    0xC => {
                        collections.pop();
                    }
                    _ => {}
                }
                usages.clear();
                (umin, umax, local_page) = (None, None, None);
            }
            1 => match tag {
                // global
                0x0 => g.page = uval,
                0x1 => g.lmin = sval,
                0x2 => g.lmax = if g.lmin < 0 { sval } else { uval as i64 },
                0x7 => g.size = uval,
                0x8 => {
                    g.rid = uval;
                    uses_ids = true;
                }
                0x9 => g.count = uval,
                0xA => stack.push(g),
                0xB => {
                    if let Some(prev) = stack.pop() {
                        g = prev;
                    }
                }
                _ => {}
            },
            2 => match tag {
                // local
                0x0 => {
                    if size == 4 && uval >> 16 != 0 {
                        // extended usage: page in the high word
                        local_page = Some(uval >> 16);
                    }
                    usages.push(uval & 0xFFFF);
                }
                0x1 => umin = Some(uval & 0xFFFF),
                0x2 => umax = Some(uval & 0xFFFF),
                _ => {}
            },
            _ => {}
        }
    }

    reports.sort_by_key(|r| (r.kind, r.report_id)); // stable, like Python's sorted()
    Descriptor { raw: raw.to_vec(), reports, applications, uses_report_ids: uses_ids }
}
