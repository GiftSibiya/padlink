//! JSONL session logs for later analysis.
//!
//! Each line is one JSON object with at least `{"t": <unix time>, "kind": <str>}`;
//! the kinds are listed in docs/padlink/formats.md. Files are interchangeable
//! with the Python implementation's.

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

use crate::{paths, py};

pub const KEEP_LOGS: usize = 100;

fn local_tm(t: f64) -> Option<libc::tm> {
    let secs = t.floor() as libc::time_t;
    // SAFETY: localtime_r only writes the tm we hand it.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    (!unsafe { libc::localtime_r(&secs, &mut tm) }.is_null()).then_some(tm)
}

/// Unix time as `HH:MM:SS` in local time.
pub fn local_hms(t: f64) -> String {
    local_tm(t).map_or_else(|| "??:??:??".into(), |tm| format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec))
}

/// `time.strftime("%Y%m%d-%H%M%S")`.
pub fn file_stamp(t: f64) -> String {
    local_tm(t).map_or_else(
        || "00000000-000000".into(),
        |tm| {
            format!(
                "{:04}{:02}{:02}-{:02}{:02}{:02}",
                tm.tm_year + 1900,
                tm.tm_mon + 1,
                tm.tm_mday,
                tm.tm_hour,
                tm.tm_min,
                tm.tm_sec
            )
        },
    )
}

/// `time.strftime("%Y-%m-%d %H:%M")`.
pub fn local_date_minute(t: f64) -> String {
    local_tm(t).map_or_else(String::new, |tm| {
        format!("{:04}-{:02}-{:02} {:02}:{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min)
    })
}

pub fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

/// `round(time.time(), 6)`.
fn rounded_now() -> f64 {
    format!("{:.6}", now()).parse().unwrap_or(0.0)
}

/// Called with every record written, e.g. to show live traffic.
pub type Listener = Box<dyn Fn(&Value) + Send>;

struct Inner {
    file: Option<File>,
    listeners: Vec<Listener>,
}

/// A session log. `Log::null()` keeps notifying listeners but writes nothing.
/// Clones share the same file.
#[derive(Clone)]
pub struct Log {
    pub path: Option<PathBuf>,
    inner: Arc<Mutex<Inner>>,
}

impl Log {
    pub fn null() -> Log {
        Log { path: None, inner: Arc::new(Mutex::new(Inner { file: None, listeners: Vec::new() })) }
    }

    /// Start `<dir>/<YYYYmmdd-HHMMSS>-<command>-<label>.jsonl` (dir defaults to the
    /// log dir) with a `session` record, then rotate old logs.
    pub fn create(label: &str, command: &str, extra: Map<String, Value>, dir: Option<&Path>) -> io::Result<Log> {
        let dir = dir.map_or_else(paths::log_dir, Path::to_path_buf);
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}-{command}-{}.jsonl", file_stamp(now()), safe_label(label)));
        let file = fs::OpenOptions::new().create(true).append(true).open(&path)?;
        let log =
            Log { path: Some(path), inner: Arc::new(Mutex::new(Inner { file: Some(file), listeners: Vec::new() })) };
        log.write("session", session_fields(label, command, extra));
        rotate(&dir);
        Ok(log)
    }

    /// Like `create`, but at an exact path (appending) and without rotation.
    pub fn create_at(path: &Path, label: &str, command: &str, extra: Map<String, Value>) -> io::Result<Log> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir)?;
        }
        let file = fs::OpenOptions::new().create(true).append(true).open(path)?;
        let log = Log {
            path: Some(path.to_path_buf()),
            inner: Arc::new(Mutex::new(Inner { file: Some(file), listeners: Vec::new() })),
        };
        log.write("session", session_fields(label, command, extra));
        Ok(log)
    }

    pub fn add_listener(&self, listener: Listener) {
        self.inner.lock().expect("log lock").listeners.push(listener);
    }

    /// Append one record; returns it. Write errors are dropped: logging must
    /// never take the bridge down.
    pub fn write(&self, kind: &str, fields: Map<String, Value>) -> Value {
        let mut record = Map::new();
        record.insert("t".into(), json!(rounded_now()));
        record.insert("kind".into(), json!(kind));
        record.extend(fields);
        let record = Value::Object(record);
        let mut inner = self.inner.lock().expect("log lock");
        if let Some(f) = inner.file.as_mut() {
            let _ = writeln!(f, "{}", py::json_dumps(&record, None));
        }
        for l in &inner.listeners {
            l(&record);
        }
        record
    }

    pub fn close(&self) {
        self.inner.lock().expect("log lock").file = None;
    }
}

/// `log.write(kind, k=v, ...)` with a JSON object literal.
#[macro_export]
macro_rules! log_write {
    ($log:expr, $kind:expr, { $($body:tt)* }) => {
        $log.write($kind, match ::serde_json::json!({ $($body)* }) {
            ::serde_json::Value::Object(m) => m,
            _ => unreachable!(),
        })
    };
}

fn session_fields(label: &str, command: &str, extra: Map<String, Value>) -> Map<String, Value> {
    let mut fields = Map::new();
    fields.insert("version".into(), json!(env!("CARGO_PKG_VERSION")));
    fields.insert("command".into(), json!(command));
    fields.insert("label".into(), json!(label));
    fields.extend(extra);
    fields
}

fn safe_label(label: &str) -> String {
    let mut out = String::new();
    let mut in_run = false;
    for c in label.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
            out.push(c);
            in_run = false;
        } else if !in_run {
            out.push('_');
            in_run = true;
        }
    }
    let out = out.trim_matches('_');
    if out.is_empty() { "session".into() } else { out.to_string() }
}

fn rotate(dir: &Path) {
    let mut logs = jsonl_files(dir);
    logs.sort();
    if logs.len() > KEEP_LOGS {
        for old in &logs[..logs.len() - KEEP_LOGS] {
            let _ = fs::remove_file(old);
        }
    }
}

fn jsonl_files(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .map(|it| it.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "jsonl")).collect())
        .unwrap_or_default()
}

/// Logs in `dir` (default: the log dir), newest first.
pub fn list_logs(dir: Option<&Path>) -> Vec<PathBuf> {
    let dir = dir.map_or_else(paths::log_dir, Path::to_path_buf);
    let mut logs = jsonl_files(&dir);
    logs.sort_by(|a, b| b.cmp(a));
    logs
}

/// Every record of a log; unparseable lines become `{"kind": "corrupt", "line": ...}`.
pub fn read_log(path: &Path) -> io::Result<Vec<Map<String, Value>>> {
    let mut out = Vec::new();
    for line in BufReader::new(File::open(path)?).lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(Value::Object(m)) => out.push(m),
            _ => {
                let mut m = Map::new();
                m.insert("kind".into(), json!("corrupt"));
                m.insert("line".into(), json!(line));
                out.push(m);
            }
        }
    }
    Ok(out)
}

/// One human-readable line for a record: relative to `t0` when given
/// (`+    1.250s`), else local wall-clock time.
pub fn format_record(rec: &Map<String, Value>, t0: Option<f64>) -> String {
    let t = rec.get("t").and_then(Value::as_f64).unwrap_or(0.0);
    let ts = match t0 {
        Some(t0) => format!("+{:9.3}s", t - t0),
        None => local_hms(t),
    };
    let kind = rec.get("kind").map_or_else(|| "?".to_string(), py::str_of);
    let get = |key: &str| rec.get(key).map_or_else(String::new, py::str_of);
    let body = match kind.as_str() {
        "input" => {
            let value = rec.get("value").map_or_else(|| "None".to_string(), py::str_of);
            format!("{} {} = {value}", get("node"), get("name"))
        }
        "hid_rx" | "hid_tx" => {
            let ok = rec.get("ok").is_none_or(py::truthy);
            let error = rec.get("error").map_or_else(|| "None".to_string(), py::str_of);
            format!("{} {}{}", get("node"), get("hex"), if ok { String::new() } else { format!("  ERROR {error}") })
        }
        "ff_play" => {
            let value = rec.get("value").filter(|v| py::truthy(v));
            let id = rec.get("id").map_or_else(|| "None".to_string(), py::str_of);
            format!("effect {id} {}", value.map_or_else(|| "STOP".to_string(), |v| format!("PLAY x{}", py::str_of(v))))
        }
        "rumble_out" => {
            let field = |k: &str| rec.get(k).map_or_else(|| "None".to_string(), py::str_of);
            format!("strong={} weak={}", field("strong"), field("weak"))
        }
        _ => {
            let rest: Map<String, Value> =
                rec.iter().filter(|(k, _)| *k != "t" && *k != "kind").map(|(k, v)| (k.clone(), v.clone())).collect();
            py::json_dumps(&Value::Object(rest), None)
        }
    };
    format!("{ts} {kind:10} {body}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_python_compatible_jsonl() {
        let dir = std::env::temp_dir().join(format!("padlink-log-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut extra = Map::new();
        extra.insert("device".into(), json!({"id": "3537:1041"}));
        let log = Log::create("3537:1041 ✓", "bridge", extra, Some(&dir)).unwrap();
        let path = log.path.clone().unwrap();
        assert!(path.file_name().unwrap().to_str().unwrap().ends_with("-bridge-3537_1041.jsonl"));
        crate::log_write!(log, "ff_play", {"id": 0, "value": 1});
        log.close();
        let recs = read_log(&path).unwrap();
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0]["kind"], "session");
        assert_eq!(recs[0]["label"], "3537:1041 ✓");
        assert_eq!(recs[0]["device"]["id"], "3537:1041");
        assert_eq!(recs[1]["value"], 1);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#""label": "3537:1041 \u2713""#), "{text}");
        assert_eq!(list_logs(Some(&dir)), vec![path]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn labels() {
        assert_eq!(safe_label("3537:1041"), "3537_1041");
        assert_eq!(safe_label("::"), "session");
        assert_eq!(safe_label("a  b/c"), "a_b_c");
    }
}
