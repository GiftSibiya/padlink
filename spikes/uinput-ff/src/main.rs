//! Phase 0 spike: can the `evdev` crate do what `padlink bridge` needs on the
//! game-facing side? It creates the same virtual pad as the Python bridge's
//! xbox360 persona (045e:028e, FF_RUMBLE/PERIODIC/CONSTANT/RAMP/GAIN) and
//! services the uinput force-feedback protocol: upload, erase, play, gain.
//!
//!     cargo run -p uinput-ff-spike -- --self-test   # act as the game too, assert every step
//!     cargo run -p uinput-ff-spike                  # serve until Ctrl-C; point fftest/a game at it
//!
//! Not production code: no input forwarding, no mixer, no sink.

use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use evdev::uinput::VirtualDevice;
use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, BusType, Device, EventSummary, FFEffectCode, FFEffectData, FFEffectKind,
    FFEnvelope, FFReplay, FFTrigger, FFWaveform, InputId, KeyCode, UInputCode, UinputAbsSetup,
};

/// What the handler saw, in the order it saw it.
#[derive(Debug, Clone, PartialEq)]
enum Seen {
    Upload { id: i16, effect: String },
    Erase { id: u32 },
    Play { id: u16, count: i32 },
    Gain(i32),
}

fn build_pad() -> io::Result<VirtualDevice> {
    let keys: AttributeSet<KeyCode> = [
        KeyCode::BTN_SOUTH,
        KeyCode::BTN_EAST,
        KeyCode::BTN_NORTH,
        KeyCode::BTN_WEST,
        KeyCode::BTN_TL,
        KeyCode::BTN_TR,
        KeyCode::BTN_SELECT,
        KeyCode::BTN_START,
        KeyCode::BTN_MODE,
        KeyCode::BTN_THUMBL,
        KeyCode::BTN_THUMBR,
    ]
    .into_iter()
    .collect();
    let ff: AttributeSet<FFEffectCode> = [
        FFEffectCode::FF_RUMBLE,
        FFEffectCode::FF_PERIODIC,
        FFEffectCode::FF_SQUARE,
        FFEffectCode::FF_TRIANGLE,
        FFEffectCode::FF_SINE,
        FFEffectCode::FF_CONSTANT,
        FFEffectCode::FF_RAMP,
        FFEffectCode::FF_GAIN,
    ]
    .into_iter()
    .collect();
    let stick = AbsInfo::new(0, -32768, 32767, 16, 128, 0);
    let trigger = AbsInfo::new(0, 0, 255, 0, 0, 0);
    let hat = AbsInfo::new(0, -1, 1, 0, 0, 0);

    let step = |what: &'static str| move |e: io::Error| io::Error::new(e.kind(), format!("{what}: {e}"));
    let mut b = VirtualDevice::builder()
        .map_err(step("open /dev/uinput"))?
        .name("Microsoft X-Box 360 pad")
        .input_id(InputId::new(BusType::BUS_USB, 0x045E, 0x028E, 0x0114))
        .with_phys(c"padlink/virtual/spike/xbox360")
        .map_err(step("UI_SET_PHYS"))?
        .with_keys(&keys)
        .map_err(step("UI_SET_KEYBIT"))?
        .with_ff(&ff)
        .map_err(step("UI_SET_FFBIT"))?
        .with_ff_effects_max(16);
    for (axis, info) in [
        (AbsoluteAxisCode::ABS_X, stick),
        (AbsoluteAxisCode::ABS_Y, stick),
        (AbsoluteAxisCode::ABS_RX, stick),
        (AbsoluteAxisCode::ABS_RY, stick),
        (AbsoluteAxisCode::ABS_Z, trigger),
        (AbsoluteAxisCode::ABS_RZ, trigger),
        (AbsoluteAxisCode::ABS_HAT0X, hat),
        (AbsoluteAxisCode::ABS_HAT0Y, hat),
    ] {
        b = b.with_absolute_axis(&UinputAbsSetup::new(axis, info)).map_err(step("UI_ABS_SETUP"))?;
    }
    b.build().map_err(step("UI_DEV_SETUP/UI_DEV_CREATE"))
}

/// Same shape as Python's effect_to_dict(), printed instead of serialised.
fn describe(data: &FFEffectData) -> String {
    let r = data.replay;
    let kind = match data.kind {
        FFEffectKind::Rumble { strong_magnitude, weak_magnitude } => {
            format!("rumble strong={strong_magnitude} weak={weak_magnitude}")
        }
        FFEffectKind::Periodic { waveform, period, magnitude, offset, .. } => {
            format!("periodic {waveform:?} period={period} magnitude={magnitude} offset={offset}")
        }
        FFEffectKind::Constant { level, .. } => format!("constant level={level}"),
        FFEffectKind::Ramp { start_level, end_level, .. } => format!("ramp {start_level}->{end_level}"),
        other => format!("{other:?}"),
    };
    format!("{kind} length={} delay={} direction={:#06x}", r.length, r.delay, data.direction)
}

/// The bridge's `_handle_uinput`, minus the mixer.
fn serve(mut pad: VirtualDevice, tx: Option<mpsc::Sender<Seen>>) -> io::Result<()> {
    let report = |seen: Seen| {
        println!("  {seen:?}");
        if let Some(tx) = &tx {
            let _ = tx.send(seen);
        }
    };
    loop {
        let events: Vec<_> = pad.fetch_events()?.collect();
        for ev in events {
            match ev.destructure() {
                EventSummary::UInput(ev, UInputCode::UI_FF_UPLOAD, ..) => {
                    let mut upload = pad.process_ff_upload(ev)?;
                    // effect() hits unreachable!() on waveforms the crate doesn't model
                    // (FF_CUSTOM). The kernel rejects undeclared waveforms, but a daemon
                    // must never panic on game input, so guard it anyway.
                    match catch_unwind(AssertUnwindSafe(|| upload.effect())) {
                        Ok(data) => {
                            report(Seen::Upload { id: upload.effect_id(), effect: describe(&data) });
                            upload.set_retval(0);
                        }
                        Err(_) => upload.set_retval(-libc_einval()),
                    }
                    // dropping `upload` sends UI_END_FF_UPLOAD and unblocks the game
                }
                EventSummary::UInput(ev, UInputCode::UI_FF_ERASE, ..) => {
                    let erase = pad.process_ff_erase(ev)?;
                    report(Seen::Erase { id: erase.effect_id() });
                }
                EventSummary::ForceFeedback(_, FFEffectCode::FF_GAIN, value) => report(Seen::Gain(value)),
                EventSummary::ForceFeedback(_, code, value) => report(Seen::Play { id: code.0, count: value }),
                _ => {}
            }
        }
    }
}

fn libc_einval() -> i32 {
    22
}

fn event_node(pad: &mut VirtualDevice) -> io::Result<PathBuf> {
    // the node can take a moment to appear after UI_DEV_CREATE
    for _ in 0..50 {
        if let Some(p) = pad
            .enumerate_dev_nodes_blocking()?
            .filter_map(Result::ok)
            .find(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("event")))
        {
            return Ok(p);
        }
        thread::sleep(Duration::from_millis(20));
    }
    Err(io::Error::new(io::ErrorKind::NotFound, "virtual pad has no event node"))
}

fn rumble(strong: u16, weak: u16, length: u16) -> FFEffectData {
    FFEffectData {
        direction: 0,
        trigger: FFTrigger { button: 0, interval: 0 },
        replay: FFReplay { length, delay: 0 },
        kind: FFEffectKind::Rumble { strong_magnitude: strong, weak_magnitude: weak },
    }
}

/// Play the game's side through the kernel FF API and check the handler saw each step.
fn self_test(node: PathBuf, rx: mpsc::Receiver<Seen>) -> Result<(), String> {
    let next = || rx.recv_timeout(Duration::from_secs(2)).map_err(|e| format!("handler saw nothing: {e}"));
    // udev grants the uaccess ACL on joystick nodes a moment after they appear
    let mut tries = 0;
    let mut game = loop {
        match Device::open(&node) {
            Ok(d) => break d,
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied && tries < 100 => {
                tries += 1;
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("open {}: {e}", node.display())),
        }
    };
    println!("game side opened {} ({:?})", node.display(), game.name());
    let ff = game.supported_ff().ok_or("virtual pad advertises no FF")?;
    for code in [FFEffectCode::FF_RUMBLE, FFEffectCode::FF_PERIODIC, FFEffectCode::FF_GAIN] {
        if !ff.contains(code) {
            return Err(format!("{code:?} not advertised"));
        }
    }

    let mut a = game.upload_ff_effect(rumble(40000, 1000, 200)).map_err(|e| format!("upload: {e}"))?;
    let id = a.id();
    expect(
        next()?,
        Seen::Upload {
            id: id as i16,
            effect: "rumble strong=40000 weak=1000 length=200 delay=0 direction=0x0000".into(),
        },
    )?;

    a.play(1).map_err(|e| e.to_string())?;
    expect(next()?, Seen::Play { id, count: 1 })?;

    game.set_ff_gain(32768).map_err(|e| e.to_string())?;
    expect(next()?, Seen::Gain(32768))?;

    let sine = FFEffectData {
        direction: 0x4000,
        trigger: FFTrigger { button: 0, interval: 0 },
        replay: FFReplay { length: 0, delay: 20 },
        kind: FFEffectKind::Periodic {
            waveform: FFWaveform::Sine,
            period: 50,
            magnitude: 16384,
            offset: 0,
            phase: 0,
            envelope: FFEnvelope { attack_length: 0, attack_level: 0, fade_length: 0, fade_level: 0 },
        },
    };
    let b = game.upload_ff_effect(sine).map_err(|e| format!("upload periodic: {e}"))?;
    expect(
        next()?,
        Seen::Upload {
            id: b.id() as i16,
            effect: "periodic Sine period=50 magnitude=16384 offset=0 length=0 delay=20 direction=0x4000".into(),
        },
    )?;

    a.update(rumble(1, 2, 0)).map_err(|e| format!("update: {e}"))?;
    expect(
        next()?,
        Seen::Upload { id: id as i16, effect: "rumble strong=1 weak=2 length=0 delay=0 direction=0x0000".into() },
    )?;

    a.stop().map_err(|e| e.to_string())?;
    expect(next()?, Seen::Play { id, count: 0 })?;

    // ff-core stops an effect (playback 0) before erasing it
    let erased = |eid: u16| -> Result<(), String> {
        let mut got = next()?;
        if got == (Seen::Play { id: eid, count: 0 }) {
            got = next()?;
        }
        expect(got, Seen::Erase { id: eid as u32 })
    };
    let bid = b.id();
    drop(b);
    erased(bid)?;
    drop(a);
    erased(id)
}

fn expect(got: Seen, want: Seen) -> Result<(), String> {
    if got == want { Ok(()) } else { Err(format!("expected {want:?}, got {got:?}")) }
}

fn main() {
    let self_test_mode = std::env::args().any(|a| a == "--self-test");
    let mut pad = match build_pad() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("cannot create uinput device: {e}");
            std::process::exit(2);
        }
    };
    let node = event_node(&mut pad).expect("event node");
    println!(
        "virtual pad: {} ({})",
        node.display(),
        pad.get_syspath().map(|p| p.display().to_string()).unwrap_or_default()
    );

    if !self_test_mode {
        println!("serving FF requests; try: fftest {}   (Ctrl-C to stop)", node.display());
        if let Err(e) = serve(pad, None) {
            eprintln!("serve: {e}");
            std::process::exit(1);
        }
        return;
    }

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        if let Err(e) = serve(pad, Some(tx)) {
            eprintln!("serve: {e}");
        }
    });
    match self_test(node, rx) {
        Ok(()) => println!("SELF-TEST PASSED: upload, update, play, gain, stop, erase all round-tripped"),
        Err(e) => {
            eprintln!("SELF-TEST FAILED: {e}");
            std::process::exit(1);
        }
    }
}
