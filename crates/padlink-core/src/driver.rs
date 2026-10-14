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
    /// `deadband`: skip writes where neither motor moved by at least this much (0..65535);
    /// starting and stopping always go out.
    pub fn start(
        sink: Box<dyn RumbleSink>,
        log: Log,
        keepalive_ms: u64,
        min_interval_ms: u64,
        deadband: u32,
    ) -> RumbleDriver {
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
            .spawn(move || run(&worker, &log, keepalive, min_interval, deadband))
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

/// Worth a write: started, stopped, or a motor moved by at least `deadband`.
pub fn significant(target: (u32, u32), current: (u32, u32), deadband: u32) -> bool {
    if target == current {
        return false;
    }
    if target == (0, 0) || current == (0, 0) {
        return true;
    }
    target.0.abs_diff(current.0) >= deadband || target.1.abs_diff(current.1) >= deadband
}

fn run(sh: &Shared, log: &Log, keepalive: f64, min_interval: f64, deadband: u32) {
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
            let mut due = significant(target, current, deadband);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sink::RumbleSink;
    use std::sync::Arc;

    struct Recorder(Arc<Mutex<Vec<(u16, u16)>>>);

    impl RumbleSink for Recorder {
        fn name(&self) -> String {
            "recorder".into()
        }
        fn set(&mut self, strong: u16, weak: u16) -> std::io::Result<()> {
            self.0.lock().unwrap().push((strong, weak));
            Ok(())
        }
    }

    #[test]
    fn deadband_rule() {
        assert!(!significant((100, 100), (100, 100), 1311));
        assert!(significant((5, 0), (0, 0), 1311), "starting always goes out");
        assert!(significant((0, 0), (5, 0), 1311), "stopping always goes out");
        assert!(!significant((40_500, 1000), (40_000, 1000), 1311));
        assert!(significant((40_000, 2400), (40_000, 1000), 1311));
        assert!(significant((40_001, 1000), (40_000, 1000), 0), "no deadband: every change");
    }

    #[test]
    fn small_changes_are_skipped_but_stop_is_not() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let mut d = RumbleDriver::start(Box::new(Recorder(Arc::clone(&writes))), Log::null(), 0, 0, 1311);
        let step = || std::thread::sleep(Duration::from_millis(40));
        d.direct(40_000, 1000, 0);
        step();
        d.direct(40_500, 1000, 0); // < 2%: skipped
        step();
        d.direct(46_000, 1000, 0); // > 2%: written
        step();
        d.direct(0, 0, 0); // stop: written
        step();
        let got = writes.lock().unwrap().clone();
        assert_eq!(got, vec![(40_000, 1000), (46_000, 1000), (0, 0)]);
        d.stop();
    }
}
