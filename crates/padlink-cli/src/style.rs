//! Terminal colours, only when stdout is a terminal (like the Python CLI).

use std::io::IsTerminal;
use std::sync::OnceLock;

fn tty() -> bool {
    static TTY: OnceLock<bool> = OnceLock::new();
    *TTY.get_or_init(|| std::io::stdout().is_terminal())
}

fn c(text: &str, code: &str) -> String {
    if tty() { format!("\x1b[{code}m{text}\x1b[0m") } else { text.to_string() }
}

pub fn bold(t: &str) -> String {
    c(t, "1")
}
pub fn dim(t: &str) -> String {
    c(t, "2")
}
pub fn green(t: &str) -> String {
    c(t, "32")
}
pub fn yellow(t: &str) -> String {
    c(t, "33")
}
pub fn red(t: &str) -> String {
    c(t, "31")
}
pub fn cyan(t: &str) -> String {
    c(t, "36")
}

pub fn status(s: &str) -> String {
    match s {
        "ok" => green("  OK "),
        "info" => cyan(" INFO"),
        "warn" => yellow(" WARN"),
        "fail" => red(" FAIL"),
        other => other.to_string(),
    }
}

pub fn stdout_is_tty() -> bool {
    tty()
}
