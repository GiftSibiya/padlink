//! Live bridge tests through the real kernel: a fake source pad (uinput) is
//! bridged, input is checked on the virtual pad and a "game" rumbles it.
//! Port of tests/test_bridge_live.py. Skipped without write access to /dev/uinput.

use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use evdev::raw_stream::RawDevice;
use evdev::uinput::VirtualDevice;
use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, BusType, EventType, InputEvent, InputId, KeyCode, UinputAbsSetup,
};
use padlink_core::bridge::{Bridge, BridgeOptions};
use padlink_core::codes::*;
use padlink_core::discovery::{self, Controller};
use padlink_core::hidraw::poll_readable;
use padlink_core::logger::Log;
use padlink_core::{personas, sink};
use serde_json::Value;

/// Tests share the kernel's device list; run them one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn uinput_ok() -> bool {
    std::fs::OpenOptions::new().write(true).open("/dev/uinput").is_ok()
}

fn wait<T>(timeout: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if let Some(v) = f() {
            return Some(v);
        }
        thread::sleep(Duration::from_millis(50));
    }
    None
}

/// An Android-style 2.4G dongle: gamepad buttons, sticks on X/Y/Z/RZ, triggers on BRAKE/GAS.
fn fake_source(product: u16) -> VirtualDevice {
    let keys: AttributeSet<KeyCode> = [
        BTN_SOUTH, BTN_EAST, BTN_NORTH, BTN_WEST, BTN_TL, BTN_TR, BTN_SELECT, BTN_START, BTN_MODE, BTN_THUMBL,
        BTN_THUMBR,
    ]
    .into_iter()
    .map(KeyCode)
    .collect();
    let stick = AbsInfo::new(128, 0, 255, 0, 15, 0);
    let pedal = AbsInfo::new(0, 0, 255, 0, 0, 0);
    let hat = AbsInfo::new(0, -1, 1, 0, 0, 0);
    let mut b = VirtualDevice::builder()
        .unwrap()
        .name("padlink test dongle")
        .input_id(InputId::new(BusType::BUS_USB, 0x1234, product, 1))
        .with_phys(c"padlink-test/source")
        .unwrap()
        .with_keys(&keys)
        .unwrap();
    for (code, info) in [
        (ABS_X, stick),
        (ABS_Y, stick),
        (ABS_Z, stick),
        (ABS_RZ, stick),
        (ABS_GAS, pedal),
        (ABS_BRAKE, pedal),
        (ABS_HAT0X, hat),
        (ABS_HAT0Y, hat),
    ] {
        b = b.with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode(code), info)).unwrap();
    }
    b.build().unwrap()
}

fn find(pred: impl Fn(&Controller) -> bool) -> Option<Controller> {
    discovery::scan(false).ok()?.into_iter().find(|c| pred(c))
}

fn source_ctrl(product: u16) -> Option<Controller> {
    let id = format!("1234:{product:04x}");
    wait(Duration::from_secs(5), || {
        find(|c| c.id_str() == id && !c.is_padlink() && c.primary_event().is_some_and(|n| n.readable()))
    })
}

fn virtual_for(product: u16, persona: &str) -> Option<Controller> {
    let tag = format!("padlink/virtual/1234:{product:04x}/{persona}");
    wait(Duration::from_secs(5), || find(|c| c.phys == tag && c.primary_event().is_some_and(|n| n.writable())))
}

fn recording_log() -> (Log, Arc<Mutex<Vec<Value>>>) {
    let records = Arc::new(Mutex::new(Vec::new()));
    let log = Log::null();
    let sink = Arc::clone(&records);
    log.add_listener(Box::new(move |r| sink.lock().unwrap().push(r.clone())));
    (log, records)
}

/// Read non-SYN events from the virtual pad until `n` arrived or 2 s passed.
fn read_events(pad: &mut RawDevice, n: usize) -> Vec<Event> {
    let mut got = Vec::new();
    let end = Instant::now() + Duration::from_secs(2);
    while Instant::now() < end && got.len() < n {
        if poll_readable(pad.as_raw_fd(), Some(Duration::from_millis(100))).unwrap() {
            got.extend(
                pad.fetch_events()
                    .unwrap()
                    .filter(|e| e.event_type().0 != EV_SYN)
                    .map(|e| (e.event_type().0, e.code(), e.value())),
            );
        }
    }
    got
}

fn emit(src: &mut VirtualDevice, events: &[Event]) {
    let evs: Vec<InputEvent> = events.iter().map(|&(t, c, v)| InputEvent::new(EventType(t).0, c, v)).collect();
    src.emit(&evs).unwrap();
}

#[test]
fn rumble_and_input_flow() {
    if !uinput_ok() {
        eprintln!("skipped: no write access to /dev/uinput");
        return;
    }
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let product = 0x5701;
    let mut src = fake_source(product);
    let Some(ctrl) = source_ctrl(product) else {
        eprintln!("skipped: fake source not visible/readable (udev uaccess?)");
        return;
    };
    assert!(!ctrl.has_ff());

    let (log, records) = recording_log();
    let mut bridge = Bridge::new(ctrl, log, BridgeOptions::default()).unwrap().spawn();
    let virt = virtual_for(product, "xbox360").unwrap_or_else(|| panic!("no virtual pad; error={:?}", bridge.error()));
    assert_eq!(virt.id_str(), "045e:028e");
    assert!(virt.has_ff());

    // input: source BTN_SOUTH -> virtual BTN_A ; source GAS -> virtual RZ trigger
    let vpath = virt.primary_event().unwrap().path.clone();
    let mut pad = wait(Duration::from_secs(3), || RawDevice::open(&vpath).ok()).expect("open virtual pad");
    emit(&mut src, &[(EV_KEY, BTN_SOUTH, 1), (EV_ABS, ABS_GAS, 255)]);
    let got = read_events(&mut pad, 2);
    assert!(got.contains(&(EV_KEY, BTN_A, 1)), "{got:?}");
    assert!(got.contains(&(EV_ABS, ABS_RZ, 255)), "{got:?}");
    drop(pad);

    // rumble: a "game" uploads + plays an effect on the virtual pad
    sink::evdev_rumble(Path::new(&vpath), 0xFFFF, 0x4000, 150).unwrap();
    let kind_seen = |k: &str| records.lock().unwrap().iter().any(|r| r["kind"] == k);
    wait(Duration::from_secs(2), || kind_seen("ff_erase").then_some(()));
    // the motors are switched off after the effect ends
    wait(Duration::from_secs(1), || {
        records
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|r| r["kind"] == "rumble_out")
            .filter(|r| r["strong"] == 0)
            .map(|_| ())
    });
    let recs = records.lock().unwrap().clone();
    // a running game (Wine/SDL hotplug) may pick up the test's virtual pad and rumble it too
    let foreign = recs.iter().any(|r| r["kind"] == "ff_upload" && r["effect"]["strong"] != 0xFFFF);
    if foreign {
        eprintln!("skipped: another program (a running game?) is sending force feedback to the virtual pad");
        bridge.stop();
        return;
    }
    let kinds: Vec<&str> = recs.iter().filter_map(|r| r["kind"].as_str()).collect();
    assert!(kinds.contains(&"ff_upload") && kinds.contains(&"ff_play") && kinds.contains(&"ff_erase"), "{kinds:?}");
    let up = recs.iter().find(|r| r["kind"] == "ff_upload").unwrap();
    assert_eq!((up["effect"]["strong"].as_u64(), up["effect"]["weak"].as_u64()), (Some(0xFFFF), Some(0x4000)));
    let outs: Vec<(u64, u64)> = recs
        .iter()
        .filter(|r| r["kind"] == "rumble_out")
        .map(|r| (r["strong"].as_u64().unwrap(), r["weak"].as_u64().unwrap()))
        .collect();
    assert!(outs.contains(&(0xFFFF, 0x4000)), "{outs:?}");
    assert_eq!(outs.last(), Some(&(0, 0)));

    bridge.stop();
    assert_eq!(bridge.error(), None);
    let recs = records.lock().unwrap();
    let down = recs.iter().rev().find(|r| r["text"] == "bridge down").expect("bridge down note");
    assert_eq!(down["stats"]["uploads"], 1);
}

#[test]
fn personas_spoof() {
    if !uinput_ok() {
        eprintln!("skipped: no write access to /dev/uinput");
        return;
    }
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let product = 0x5702;
    let mut src = fake_source(product);
    let Some(ctrl) = source_ctrl(product) else {
        eprintln!("skipped: fake source not visible/readable (udev uaccess?)");
        return;
    };
    // the fake source is Android/xpad-style: BTN_NORTH (0x133) is the LEFT button
    let expect: [(&str, u16, Vec<Event>); 3] = [
        ("xbox360", 0x133, vec![(EV_ABS, ABS_RZ, 255)]),
        ("ds4", BTN_WEST, vec![(EV_ABS, ABS_RZ, 255), (EV_KEY, BTN_TR2, 1)]),
        ("switchpro", BTN_WEST, vec![(EV_KEY, BTN_TR2, 1)]),
    ];
    for (id, left_code, trigger_events) in expect {
        let persona = personas::get(id).unwrap();
        let opts = BridgeOptions { mode: id.into(), ..Default::default() };
        let mut bridge = Bridge::new(ctrl.clone(), Log::null(), opts).unwrap().spawn();
        let virt = virtual_for(product, id).unwrap_or_else(|| panic!("{id}: no virtual pad ({:?})", bridge.error()));
        assert_eq!(virt.id_str(), persona.id_str(), "{id}");
        assert_eq!(virt.name, persona.name, "{id}");
        let vpath = virt.primary_event().unwrap().path.clone();
        let mut pad = wait(Duration::from_secs(3), || RawDevice::open(&vpath).ok()).expect("open virtual pad");
        emit(&mut src, &[(EV_KEY, BTN_NORTH, 1), (EV_ABS, ABS_GAS, 255)]);
        let got = read_events(&mut pad, 1 + trigger_events.len());
        assert!(got.contains(&(EV_KEY, left_code, 1)), "{id}: {got:?}");
        for ev in &trigger_events {
            assert!(got.contains(ev), "{id}: missing {ev:?} in {got:?}");
        }
        drop(pad);
        emit(&mut src, &[(EV_KEY, BTN_NORTH, 0), (EV_ABS, ABS_GAS, 0)]);
        bridge.stop();
        wait(Duration::from_secs(3), || virtual_for_now(product).is_none().then_some(()));
    }
}

fn virtual_for_now(product: u16) -> Option<Controller> {
    let prefix = format!("padlink/virtual/1234:{product:04x}/");
    find(|c| c.phys.starts_with(&prefix))
}

#[test]
fn source_unplug_ends_the_bridge() {
    if !uinput_ok() {
        eprintln!("skipped: no write access to /dev/uinput");
        return;
    }
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let product = 0x5703;
    let src = fake_source(product);
    let Some(ctrl) = source_ctrl(product) else {
        eprintln!("skipped: fake source not visible/readable (udev uaccess?)");
        return;
    };
    let (log, records) = recording_log();
    let mut bridge = Bridge::new(ctrl, log, BridgeOptions::default()).unwrap().spawn();
    virtual_for(product, "xbox360").expect("virtual pad");
    drop(src); // unplug
    wait(Duration::from_secs(3), || (!bridge.alive()).then_some(())).expect("bridge should end on unplug");
    assert_eq!(bridge.error().as_deref(), Some("controller disconnected"));
    bridge.stop();
    assert!(
        virtual_for_now(product).is_none()
            || wait(Duration::from_secs(3), || virtual_for_now(product).is_none().then_some(())).is_some()
    );
    let recs = records.lock().unwrap();
    assert!(recs.iter().any(|r| r["text"] == "bridge stopped: controller disconnected"));
    assert!(recs.iter().any(|r| r["text"] == "bridge down"));
}

/// A game that opens the pad and plays but never sends force feedback gets one hint in the log;
/// a game that does send effects gets none.
#[test]
fn silent_game_hint() {
    if !uinput_ok() {
        eprintln!("skipped: no write access to /dev/uinput");
        return;
    }
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let product = 0x5704;
    let mut src = fake_source(product);
    let Some(ctrl) = source_ctrl(product) else {
        eprintln!("skipped: fake source not visible/readable (udev uaccess?)");
        return;
    };
    for sends_rumble in [false, true] {
        let (log, records) = recording_log();
        let opts = BridgeOptions { silent_game_after: Duration::from_millis(800), ..Default::default() };
        let mut bridge = Bridge::new(ctrl.clone(), log, opts).unwrap().spawn();
        let virt = virtual_for(product, "xbox360").expect("virtual pad");
        let vpath = virt.primary_event().unwrap().path.clone();
        // the "game": opens the pad and sets the gain, like SDL does
        let mut game = wait(Duration::from_secs(3), || evdev::Device::open(&vpath).ok()).expect("open virtual pad");
        game.set_ff_gain(0xFFFF).unwrap();
        // play only once the bridge has seen the game open the pad (in real life the game
        // opens it long before anyone presses anything)
        let gain_seen = || records.lock().unwrap().iter().any(|r| r["kind"] == "ff_gain").then_some(());
        wait(Duration::from_secs(2), gain_seen).expect("bridge never saw the game's gain write");
        if sends_rumble {
            sink::evdev_rumble(Path::new(&vpath), 0x8000, 0x8000, 50).unwrap();
        }
        // the player plays
        for i in 0..60 {
            emit(&mut src, &[(EV_ABS, ABS_X, 100 + (i % 50))]);
        }
        thread::sleep(Duration::from_millis(2000));
        bridge.stop();
        let hints = records
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["text"].as_str().is_some_and(|t| t.contains("has sent no force feedback")))
            .count();
        let kinds: Vec<String> = records
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["kind"] != "input")
            .map(|r| format!("{}@{:.2}", r["kind"].as_str().unwrap_or("?"), r["t"].as_f64().unwrap_or(0.0) % 100.0))
            .collect();
        assert_eq!(hints, usize::from(!sends_rumble), "sends_rumble={sends_rumble}; records: {kinds:?}");
        drop(game);
        wait(Duration::from_secs(3), || virtual_for_now(product).is_none().then_some(()));
    }
}
