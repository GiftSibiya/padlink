//! RumbleDriver: a thread that applies RumbleMixer output to a sink on time,
//! honouring effect timing, keepalive and a minimum write interval.

use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::ff::{Effect, RumbleMixer};
use crate::log_write;
use crate::logger::Log;
use crate::sink::RumbleSink;

/// Effect id used by `direct()`, out of the kernel's range.
const DIRECT_ID: i32 = -100;

struct State {
    mixer: RumbleMixer,
    running: bool,
}

struct Shared {
    state: Mutex<State>,
    cond: Condvar,
    sink: Mutex<Box<dyn RumbleSink>>,
    started: Instant,
}

impl Shared {
    /// Seconds on a monotonic clock (the mixer's time base).
    fn now(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }
}

pub struct RumbleDriver {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    sink_name: String,
}

impl RumbleDriver {
    pub fn start(sink: Box<dyn RumbleSink>, log: Log, keepalive_ms: u64, min_interval_ms: u64) -> RumbleDriver {
        let sink_name = sink.name();
        let shared = Arc::new(Shared {
            state: Mutex::new(State { mixer: RumbleMixer::new(), running: true }),
            cond: Condvar::new(),
            sink: Mutex::new(sink),
            started: Instant::now(),
        });
        let worker = Arc::clone(&shared);
        let keepalive = keepalive_ms as f64 / 1000.0;
        let min_interval = min_interval_ms as f64 / 1000.0;
        let thread = thread::Builder::new()
            .name("padlink-rumble".into())
            .spawn(move || run(&worker, &log, keepalive, min_interval))
            .expect("spawn rumble thread");
        RumbleDriver { shared, thread: Some(thread), sink_name }
    }

    pub fn sink_name(&self) -> &str {
        &self.sink_name
    }

    fn with_mixer(&self, f: impl FnOnce(&mut RumbleMixer, f64)) {
        let mut st = self.shared.state.lock().expect("rumble state");
        let now = self.shared.now();
        f(&mut st.mixer, now);
        self.shared.cond.notify_all();
    }

    pub fn upload(&self, effect: Effect) {
        self.with_mixer(|m, _| m.upload(effect));
    }

    pub fn erase(&self, id: i32) {
        self.with_mixer(|m, _| m.erase(id));
    }

    pub fn play(&self, id: i32, count: i32) {
        self.with_mixer(|m, now| m.play(id, count, now));
    }

    pub fn set_gain(&self, gain: i64) {
        self.with_mixer(|m, _| m.set_gain(gain));
    }

    /// Play a one-off rumble without the caller managing effect ids.
    pub fn direct(&self, strong: u32, weak: u32, ms: u32) {
        self.upload(Effect::rumble(DIRECT_ID, strong, weak, ms));
        self.play(DIRECT_ID, i32::from(strong != 0 || weak != 0));
    }

    /// Stop the thread, silence the motors and close the sink.
    pub fn stop(&mut self) {
        {
            let mut st = self.shared.state.lock().expect("rumble state");
            st.running = false;
            st.mixer.stop_all();
            self.shared.cond.notify_all();
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let mut sink = self.shared.sink.lock().expect("sink");
        let _ = sink.set(0, 0);
        sink.close();
    }
}

impl Drop for RumbleDriver {
    fn drop(&mut self) {
        if self.thread.is_some() {
            self.stop();
        }
    }
}

fn run(sh: &Shared, log: &Log, keepalive: f64, min_interval: f64) {
    let mut current = (0u32, 0u32);
    let mut last_write = f64::NEG_INFINITY;
    let mut errors = 0;
    loop {
        let target = {
            let mut st = sh.state.lock().expect("rumble state");
            if !st.running {
                return;
            }
            let now = sh.now();
            let target = st.mixer.levels(now);
            let mut due = target != current;
            let active = target != (0, 0);
            if !due && active && keepalive > 0.0 && now - last_write >= keepalive {
                due = true;
            }
            if due && now - last_write < min_interval {
                let wait = min_interval - (now - last_write);
                let _ = sh.cond.wait_timeout(st, Duration::from_secs_f64(wait)).expect("rumble state");
                continue;
            }
            if !due {
                let mut timeouts = Vec::new();
                if let Some(deadline) = st.mixer.next_deadline(now) {
                    timeouts.push(deadline - now);
                }
                if active && keepalive > 0.0 {
                    timeouts.push(keepalive - (now - last_write));
                }
                match timeouts.into_iter().reduce(f64::min) {
                    Some(t) => drop(sh.cond.wait_timeout(st, Duration::from_secs_f64(t.max(0.001))).expect("state")),
                    None => drop(sh.cond.wait(st).expect("rumble state")),
                }
                continue;
            }
            target
        };
        let result = sh.sink.lock().expect("sink").set(target.0 as u16, target.1 as u16);
        match result {
            Ok(()) => errors = 0,
            Err(e) => {
                errors += 1;
                log_write!(log, "note", {"text": format!("rumble sink error: {e}")});
                if errors > 20 {
                    return;
                }
            }
        }
        current = target;
        last_write = sh.now();
        let sink = sh.sink.lock().expect("sink").name();
        log_write!(log, "rumble_out", {"strong": target.0, "weak": target.1, "sink": sink});
    }
}
