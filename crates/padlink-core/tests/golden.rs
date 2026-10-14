//! Replays the golden fixtures: expected values recorded from the original Python
//! implementation, now frozen. A failure here means behaviour (and so file formats or
//! what games see) changed; update a fixture only on purpose.
//! Every expected value in `fixtures/golden/*.json` must be reproduced exactly.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use padlink_core::codes::{AbsInfo, EV_SYN, code_name};
use padlink_core::ff::{Effect, RawEffect, RawParams, RumbleMixer};
use padlink_core::profiles::{Profile, RumbleSpec};
use padlink_core::translate::{Translator, heuristic_map};
use padlink_core::{art, hex, hiddesc, logger, personas, py};
use serde_json::{Value, json};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn load(name: &str) -> Value {
    let path = root().join("fixtures/golden").join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn arr(v: &Value) -> &Vec<Value> {
    v.as_array().unwrap_or_else(|| panic!("not an array: {v}"))
}

fn int(v: &Value) -> i64 {
    v.as_i64().unwrap_or_else(|| panic!("not an int: {v}"))
}

fn absinfo(v: &Value) -> AbsInfo {
    let f = |k: &str| int(&v[k]) as i32;
    AbsInfo::new(f("value"), f("min"), f("max"), f("fuzz"), f("flat"), f("resolution"))
}

fn absinfo_json(a: &AbsInfo) -> Value {
    json!({"value": a.value, "min": a.min, "max": a.max, "fuzz": a.fuzz, "flat": a.flat, "resolution": a.resolution})
}

fn event(v: &Value) -> (u16, u16, i32) {
    let e = arr(v);
    (int(&e[0]) as u16, int(&e[1]) as u16, int(&e[2]) as i32)
}

fn events_json(events: &[(u16, u16, i32)]) -> Value {
    events.iter().map(|&(t, c, v)| json!([t, c, v])).collect()
}

#[test]
fn hiddesc() {
    let fx = load("hiddesc");
    for case in arr(&fx["cases"]) {
        let name = case["name"].as_str().unwrap();
        let raw = hex::parse_hex(case["hex"].as_str().unwrap()).unwrap();
        let d = hiddesc::parse(&raw);
        assert_eq!(json!(d.applications), case["applications"], "{name}: applications");
        assert_eq!(json!(d.uses_report_ids), case["uses_report_ids"], "{name}");
        assert_eq!(json!(d.has_pid()), case["has_pid"], "{name}");
        assert_eq!(json!(d.vendor_pages()), case["vendor_pages"], "{name}");
        let reports: Vec<Value> = d
            .reports
            .iter()
            .map(|r| {
                json!({
                    "kind": r.kind.as_str(), "report_id": r.report_id, "bits": r.bits,
                    "payload_len": r.payload_len(), "wire_len": r.wire_len(), "usage_pages": r.usage_pages(),
                    "fields": r.fields.iter().map(|f| json!({
                        "kind": f.kind.as_str(), "report_id": f.report_id, "bit_offset": f.bit_offset,
                        "size": f.size, "count": f.count, "usage_page": f.usage_page, "usages": f.usages,
                        "usage_range": f.usage_range.map(|(a, b)| json!([a, b])), "logical_min": f.logical_min,
                        "logical_max": f.logical_max, "flags": f.flags, "collection": f.collection,
                        "describe": f.describe(),
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        assert_eq!(json!(reports), case["reports"], "{name}: reports");
        assert_eq!(json!(d.summary()), case["summary"], "{name}: summary");
    }
}

#[test]
fn profiles() {
    let fx = load("profiles");
    for case in arr(&fx["render"]) {
        let spec = RumbleSpec::from_value(&case["spec"]).unwrap();
        assert_eq!(spec.to_value(), case["spec"], "spec round-trip");
        for row in arr(&case["rows"]) {
            let (s, w) = (int(&row["strong"]) as u16, int(&row["weak"]) as u16);
            let got = hex::hexstr(&spec.render(s, w).unwrap());
            assert_eq!(got, row["hex"].as_str().unwrap(), "{} strong={s} weak={w}", case["spec"]);
        }
    }
    let bad = RumbleSpec { report: vec![json!("strong"), json!("loud")], ..Default::default() };
    assert_eq!(bad.render(1, 1).unwrap_err(), fx["unknown_token_error"].as_str().unwrap());

    for case in arr(&fx["roundtrip"]) {
        let p = Profile::from_value(&case["input"]).unwrap();
        assert_eq!((p.vendor as i64, p.product as i64), (int(&case["vendor"]), int(&case["product"])));
        assert_eq!(p.to_value(), case["to_dict"]);
        assert_eq!(p.to_json(), case["saved_text"].as_str().unwrap());
        let name = padlink_core::profiles::profile_path(p.vendor, p.product, Some(Path::new(".")));
        assert_eq!(name.file_name().unwrap().to_str().unwrap(), case["filename"].as_str().unwrap());
    }
}

#[test]
fn personas() {
    let fx = load("personas");
    assert_eq!(personas::DEFAULT, fx["default"].as_str().unwrap());
    assert_eq!(arr(&fx["personas"]).len(), personas::PERSONAS.len());
    for (p, want) in personas::PERSONAS.iter().zip(arr(&fx["personas"])) {
        let id = p.id;
        let face: serde_json::Map<String, Value> = p.face.iter().map(|&(k, v)| (k.to_string(), json!(v))).collect();
        let caps = p.capabilities();
        let got = json!({
            "id": p.id, "label": p.label, "description": p.description, "name": p.name, "vendor": p.vendor,
            "product": p.product, "version": p.version, "bustype": p.bustype, "id_str": p.id_str(),
            "spec_layout": p.spec_layout, "trigger_buttons": p.trigger_buttons, "face": face,
            "stick": absinfo_json(&p.stick), "trigger": p.trigger.as_ref().map(absinfo_json),
            "capabilities": {"keys": caps.keys, "abs": caps.abs.iter().map(|(c, a)| json!([c, absinfo_json(a)])).collect::<Vec<_>>()},
        });
        for key in got.as_object().unwrap().keys() {
            assert_eq!(got[key], want[key], "{id}: {key}");
        }
        for case in arr(&want["convert"]) {
            let (t, c, v) = event(&case["in"]);
            assert_eq!(events_json(&p.convert(t, c, v)), case["out"], "{id}: convert {}", case["in"]);
        }
    }
    for (query, want) in fx["get"].as_object().unwrap() {
        assert_eq!(personas::get(query).ok().map(|p| p.id), want.as_str(), "get({query})");
    }
    for (phys, want) in fx["from_phys"].as_object().unwrap() {
        assert_eq!(personas::from_phys(phys).map(|p| p.id), want.as_str(), "from_phys({phys})");
    }
}

#[test]
fn translator() {
    let fx = load("translator");
    for case in arr(&fx["cases"]) {
        let name = case["name"].as_str().unwrap();
        let info: HashMap<u16, AbsInfo> =
            case["absinfo"].as_object().unwrap().iter().map(|(code, a)| (code.parse().unwrap(), absinfo(a))).collect();
        let mapping = if let Some(want) = case.get("heuristic_map") {
            let keys: BTreeSet<u16> = arr(&case["keys"]).iter().map(|k| int(k) as u16).collect();
            let abses: BTreeSet<u16> = info.keys().copied().collect();
            let got = heuristic_map(&keys, &abses, case["driver"].as_str());
            assert_eq!(&got, want, "{name}: heuristic_map");
            // key order matters too: it ends up in saved profiles
            assert_eq!(py::json_dumps(&got, None), py::json_dumps(want, None), "{name}: heuristic_map order");
            got
        } else {
            case["map"].clone()
        };
        let mut t = Translator::new(&mapping, &info).unwrap();
        for step in arr(&case["events"]) {
            let (ty, c, v) = event(&step["in"]);
            assert_eq!(events_json(&t.translate(ty, c, v)), step["out"], "{name}: translate {}", step["in"]);
        }
    }
    for (key, want) in fx["code_names"].as_object().unwrap() {
        let (t, c) = key.split_once(':').unwrap();
        assert_eq!(code_name(t.parse().unwrap(), c.parse().unwrap()), want.as_str().unwrap(), "code_name {key}");
    }
    assert_eq!(code_name(EV_SYN, 0), "SYN_REPORT");
}

fn raw_effect(kind: &str, p: &Value) -> RawEffect {
    use padlink_core::codes::*;
    let g = |k: &str, d: i64| p.get(k).map_or(d, int);
    let (etype, params) = match kind {
        "rumble" => (FF_RUMBLE, RawParams::Rumble { strong: g("strong", 0) as u16, weak: g("weak", 0) as u16 }),
        "periodic" => (
            FF_PERIODIC,
            RawParams::Periodic {
                waveform: g("waveform", FF_SINE as i64) as u16,
                period: g("period", 100) as u16,
                magnitude: g("magnitude", 0) as i16,
                offset: g("offset", 0) as i16,
            },
        ),
        "constant" => (FF_CONSTANT, RawParams::Constant { level: g("level", 0) as i16 }),
        "ramp" => (FF_RAMP, RawParams::Ramp { start_level: g("start", 0) as i16, end_level: g("end", 0) as i16 }),
        "spring" => (FF_SPRING, RawParams::Other),
        other => panic!("unknown effect kind {other}"),
    };
    RawEffect {
        etype,
        id: g("id", 0) as i16,
        direction: g("direction", 0) as u16,
        length: g("length", 0) as u16,
        delay: g("delay", 0) as u16,
        params,
    }
}

#[test]
fn ff() {
    let fx = load("ff");
    for case in arr(&fx["effect_to_dict"]) {
        let eff = Effect::from_raw(&raw_effect(case["kind"].as_str().unwrap(), &case["params"]));
        assert_eq!(eff.record, case["dict"], "effect_to_dict {}", case["params"]);
        assert_eq!(py::json_dumps(&eff.record, None), py::json_dumps(&case["dict"], None), "key order");
        assert_eq!((eff.strong as i64, eff.weak as i64), (int(&case["dict"]["strong"]), int(&case["dict"]["weak"])));
    }
    for (name, trace) in fx["mixer"].as_object().unwrap() {
        let mut m = RumbleMixer::new();
        for op in arr(trace) {
            let now = op.get("now").and_then(Value::as_f64).unwrap_or(0.0);
            match op["op"].as_str().unwrap() {
                "upload" => {
                    let e = &op["effect"];
                    m.upload(Effect {
                        id: int(&e["id"]) as i32,
                        length: int(&e["length"]) as u32,
                        delay: int(&e["delay"]) as u32,
                        strong: int(&e["strong"]) as u32,
                        weak: int(&e["weak"]) as u32,
                        record: e.clone(),
                    })
                }
                "play" => m.play(int(&op["id"]) as i32, int(&op["count"]) as i32, now),
                "erase" => m.erase(int(&op["id"]) as i32),
                "gain" => {
                    m.set_gain(int(&op["gain"]));
                    assert_eq!(m.gain as i64, int(&op["result"]), "{name}: gain {}", op["gain"]);
                }
                "stop_all" => m.stop_all(),
                "levels" => {
                    let (s, w) = m.levels(now);
                    assert_eq!(json!([s, w]), op["result"], "{name}: levels at {now}");
                }
                "deadline" => {
                    let got = m.next_deadline(now);
                    assert_eq!(got, op["result"].as_f64(), "{name}: next_deadline at {now}");
                }
                other => panic!("unknown op {other}"),
            }
        }
    }
}

#[test]
fn misc() {
    let fx = load("misc");
    for (input, want) in fx["parse_hex"].as_object().unwrap() {
        match want.as_str() {
            Some(hexstr) => assert_eq!(hex::hexstr(&hex::parse_hex(input).unwrap()), hexstr, "parse_hex({input:?})"),
            None => assert!(hex::parse_hex(input).is_err(), "parse_hex({input:?}) should fail"),
        }
    }
    assert_eq!(hex::long_bits() as i64, int(&fx["long_bits"]));
    for (input, want) in fx["parse_bitmask"].as_object().unwrap() {
        let got: Vec<u32> = hex::parse_bitmask(Some(input)).into_iter().collect();
        assert_eq!(json!(got), *want, "parse_bitmask({input:?})");
    }
    for case in arr(&fx["format_record"]) {
        let rec = case["record"].as_object().unwrap();
        assert_eq!(logger::format_record(rec, Some(1000.0)), case["relative"].as_str().unwrap());
    }
}

#[test]
fn art() {
    let fx = load("art");
    assert_eq!(json!(art::FAMILIES), fx["families"]);
    assert_eq!(json!(art::required_ids()), fx["required_ids"]);
    assert_eq!(json!(art::OPTIONAL_IDS), fx["optional_ids"]);
    for (id, want) in fx["candidates"].as_object().unwrap() {
        let (v, p) = id.split_once(':').unwrap();
        let got = art::candidates(u16::from_str_radix(v, 16).unwrap(), u16::from_str_radix(p, 16).unwrap());
        assert_eq!(json!(got), *want, "candidates({id})");
    }
    for (family, want) in fx["templates"].as_object().unwrap() {
        let file = std::fs::read_to_string(root().join(want["file"].as_str().unwrap())).unwrap();
        assert_eq!(art::template_svg(family).unwrap() == file, want["matches_template_svg"].as_bool().unwrap());
        assert_eq!(json!(art::viewbox(&file)), want["viewbox"], "{family}: viewbox");
        let checks: Vec<Value> = art::check_svg(&file).iter().map(|(s, m)| json!([s.as_str(), m])).collect();
        assert_eq!(json!(checks), want["check"], "{family}: check");
    }
    for (name, case) in fx["check"].as_object().unwrap() {
        let got: Vec<Value> =
            art::check_svg(case["svg"].as_str().unwrap()).iter().map(|(s, m)| json!([s.as_str(), m])).collect();
        assert_eq!(json!(got), case["check"], "check {name}");
    }
    for (svg, want) in fx["viewbox"].as_object().unwrap() {
        assert_eq!(json!(art::viewbox(svg)), *want, "viewbox {svg}");
    }
    for case in arr(&fx["stick_travel"]) {
        let b = |k: &str| -> [f64; 4] { serde_json::from_value(case[k].clone()).unwrap() };
        assert_eq!(art::stick_travel(b("ring"), b("knob")), case["result"].as_f64().unwrap());
    }
}

fn candidate_json(c: &padlink_core::prober::Candidate) -> Value {
    json!({
        "label": c.label, "report_id": c.report_id, "template": c.template, "scale": c.scale, "stop": null,
        "meta": c.meta(), "on": hex::hexstr(&c.on_bytes()), "off": hex::hexstr(&c.off_bytes()),
        "spec": c.spec().to_value(),
    })
}

#[test]
fn prober() {
    use padlink_core::discovery::{Controller, HidrawNode};
    use padlink_core::prober::{self, Motor};
    use std::collections::BTreeMap;

    let fx = load("prober");
    let tmp = std::env::temp_dir().join(format!("padlink-prober-{}", std::process::id()));
    for case in arr(&fx["cases"]) {
        let name = case["descriptor"].as_str().unwrap();
        let hid = hiddesc_hex(name);
        let dir = tmp.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("report_descriptor"), &hid).unwrap();
        let node = HidrawNode {
            path: "/dev/hidraw99".into(),
            sys_path: dir.display().to_string(),
            hid_sys_path: dir.display().to_string(),
            interface: Some("01".into()),
        };
        let ctrl = Controller {
            key: "/sys/x".into(),
            name: "x".into(),
            vendor: 1,
            product: 2,
            version: 0,
            bustype: 3,
            driver: None,
            uniq: String::new(),
            phys: String::new(),
            virtual_: false,
            events: vec![],
            joysticks: vec![],
            hidraw: vec![node],
            battery: None,
            index: 0,
        };
        let blind = case["blind"].as_bool().unwrap();
        let cands = prober::stage1_candidates(&ctrl, blind);
        let got: Vec<Value> = cands.iter().map(candidate_json).collect();
        assert_eq!(json!(got), case["stage1"], "{name} blind={blind}: stage1");
        let Some(hit) = cands.iter().find(|c| c.kind == prober::Kind::AllFf) else {
            assert!(case.get("stage2").is_none());
            continue;
        };
        let got: Vec<Value> = prober::stage2_byte_candidates(hit).iter().map(candidate_json).collect();
        assert_eq!(json!(got), case["stage2"], "{name}: stage2");
        for b in arr(&case["build"]) {
            let motors: BTreeMap<usize, Motor> = b["motors"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| {
                    let m = match v.as_str().unwrap() {
                        "strong" => Motor::Strong,
                        "weak" => Motor::Weak,
                        _ => Motor::Both,
                    };
                    (k.parse().unwrap(), m)
                })
                .collect();
            assert_eq!(
                candidate_json(&prober::build_from_bytes(hit, &motors)),
                b["result"],
                "{name}: build {}",
                b["motors"]
            );
        }
    }
    for f in arr(&fx["fit"]) {
        let got = prober::fit(arr(&f["payload"]), int(&f["length"]) as usize);
        assert_eq!(json!(got), f["result"], "fit {} {}", f["payload"], f["length"]);
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The descriptor bytes a fixture case was built from.
fn hiddesc_hex(name: &str) -> Vec<u8> {
    let fx = load("hiddesc");
    let case = arr(&fx["cases"]).iter().find(|c| c["name"] == name).unwrap_or_else(|| panic!("no descriptor {name}"));
    hex::parse_hex(case["hex"].as_str().unwrap()).unwrap()
}

#[test]
fn capture() {
    let fx = load("capture");
    for case in arr(&fx["lines"]) {
        let line = case["line"].as_str().unwrap();
        let got = padlink_core::capture::parse_line(line).map(Value::Object);
        assert_eq!(json!(got), case["record"], "parse_line({line:?})");
    }
}

#[test]
fn winebus() {
    let fx = load("winebus");
    for (name, case) in fx["cases"].as_object().unwrap() {
        let got = padlink_core::doctor::parse_winebus(case["text"].as_str().unwrap());
        assert_eq!(Value::Object(got), case["keys"], "winebus {name}");
    }
}
