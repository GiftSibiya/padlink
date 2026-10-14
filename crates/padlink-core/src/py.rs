//! Python-compatible formatting and rounding.
//!
//! The Rust port reads and writes the same files and prints the same text as
//! the Python implementation, so a few Python behaviours are reproduced
//! exactly here (see docs/padlink/formats.md, "Rules for porting numeric code").

use serde_json::Value;

/// Python's `round(x)`: round half to even (`round(-0.5) == 0`, `round(2.5) == 2`).
pub fn round(x: f64) -> i64 {
    x.round_ties_even() as i64
}

/// Python's `format(x, "g")`: 6 significant digits, trailing zeros removed.
pub fn fmt_g(x: f64) -> String {
    const P: i32 = 6;
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    // the exponent after rounding to P significant digits decides the style
    let e = format!("{:.*e}", (P - 1) as usize, x);
    let (mantissa, exp) = e.split_once('e').expect("{:e} has an exponent");
    let exp: i32 = exp.parse().expect("integer exponent");
    if (-4..P).contains(&exp) {
        trim_zeros(format!("{:.*}", (P - 1 - exp) as usize, x))
    } else {
        let m = trim_zeros(mantissa.to_string());
        format!("{m}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
    }
}

fn trim_zeros(s: String) -> String {
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
}

/// Python's `repr(float)`: shortest round-trip digits, always with a `.0` or exponent.
pub fn float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    let e = format!("{x:e}"); // shortest round-trip digits, e.g. "-1.2345e3"
    let (mantissa, exp) = e.split_once('e').expect("{:e} has an exponent");
    let exp: i32 = exp.parse().expect("integer exponent");
    let (sign, mantissa) = mantissa.strip_prefix('-').map_or(("", mantissa), |m| ("-", m));
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    if (-4..16).contains(&exp) {
        let point = exp + 1; // digits before the decimal point
        let body = if point <= 0 {
            format!("0.{}{digits}", "0".repeat((-point) as usize))
        } else if point as usize >= digits.len() {
            format!("{digits}{}.0", "0".repeat(point as usize - digits.len()))
        } else {
            format!("{}.{}", &digits[..point as usize], &digits[point as usize..])
        };
        format!("{sign}{body}")
    } else {
        let m = if digits.len() > 1 { format!("{}.{}", &digits[..1], &digits[1..]) } else { digits };
        format!("{sign}{m}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
    }
}

/// Python's `json.dumps(value)` (compact, `", "` and `": "` separators) or
/// `json.dumps(value, indent=n)`, both with the default `ensure_ascii=True`.
pub fn json_dumps(value: &Value, indent: Option<usize>) -> String {
    let mut out = String::new();
    write_json(&mut out, value, indent, 0);
    out
}

fn write_json(out: &mut String, value: &Value, indent: Option<usize>, level: usize) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) => out.push_str(&i.to_string()),
            (_, Some(u), _) => out.push_str(&u.to_string()),
            (_, _, Some(f)) => out.push_str(&float_repr(f)),
            _ => out.push_str(&n.to_string()),
        },
        Value::String(s) => write_json_str(out, s),
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                separator(out, i, indent, level + 1);
                write_json(out, item, indent, level + 1);
            }
            close(out, indent, level);
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, (k, v)) in map.iter().enumerate() {
                separator(out, i, indent, level + 1);
                write_json_str(out, k);
                out.push_str(": ");
                write_json(out, v, indent, level + 1);
            }
            close(out, indent, level);
            out.push('}');
        }
    }
}

fn separator(out: &mut String, index: usize, indent: Option<usize>, level: usize) {
    match indent {
        Some(n) => {
            if index > 0 {
                out.push(',');
            }
            out.push('\n');
            out.push_str(&" ".repeat(n * level));
        }
        None if index > 0 => out.push_str(", "),
        None => {}
    }
}

fn close(out: &mut String, indent: Option<usize>, level: usize) {
    if let Some(n) = indent {
        out.push('\n');
        out.push_str(&" ".repeat(n * level));
    }
}

fn write_json_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Python's `str(value)` for a JSON value (`None`, `True`, `[1, 'a']`, ...).
pub fn str_of(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => repr(other),
    }
}

/// Python's `repr(value)` for a JSON value.
pub fn repr(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) => i.to_string(),
            (_, Some(u), _) => u.to_string(),
            (_, _, Some(f)) => float_repr(f),
            _ => n.to_string(),
        },
        Value::String(s) => str_repr(s),
        Value::Array(items) => format!("[{}]", items.iter().map(repr).collect::<Vec<_>>().join(", ")),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter().map(|(k, v)| format!("{}: {}", str_repr(k), repr(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

fn str_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::from(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python truthiness of a JSON value.
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rounding_is_half_even() {
        assert_eq!([round(-0.5), round(0.5), round(1.5), round(2.5), round(-1.5)], [0, 0, 2, 2, -2]);
    }

    #[test]
    fn g_format() {
        for (x, want) in [
            (250.0, "250"),
            (79.5, "79.5"),
            (-18.0, "-18"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (123456.0, "123456"),
            (1234567.0, "1.23457e+06"),
            (0.1 + 0.2, "0.3"),
            (2.5e-7, "2.5e-07"),
        ] {
            assert_eq!(fmt_g(x), want, "{x}");
        }
    }

    #[test]
    fn float_repr_matches_python() {
        for (x, want) in [
            (1000.5, "1000.5"),
            (1.0, "1.0"),
            (0.1, "0.1"),
            (1.1400000000000001, "1.1400000000000001"),
            (1e16, "1e+16"),
            (1.5e-7, "1.5e-07"),
            (0.0001, "0.0001"),
            (123456789012345.0, "123456789012345.0"),
            (-2.5, "-2.5"),
        ] {
            assert_eq!(float_repr(x), want);
        }
    }

    #[test]
    fn json_like_python() {
        let v = json!({"text": "unicode ✓ \"q\"", "data": {"b": [1, 2.5, null, true]}, "e": [], "o": {}});
        assert_eq!(
            json_dumps(&v, None),
            r#"{"text": "unicode \u2713 \"q\"", "data": {"b": [1, 2.5, null, true]}, "e": [], "o": {}}"#
        );
        assert_eq!(
            json_dumps(&json!({"a": [1, {}], "b": "x"}), Some(2)),
            "{\n  \"a\": [\n    1,\n    {}\n  ],\n  \"b\": \"x\"\n}"
        );
        assert_eq!(json_dumps(&json!("😀"), None), r#""\ud83d\ude00""#);
    }

    #[test]
    fn str_and_repr() {
        assert_eq!(str_of(&json!(null)), "None");
        assert_eq!(str_of(&json!("x")), "x");
        assert_eq!(repr(&json!(["a", 1, true, {"k": "it's"}])), r#"['a', 1, True, {'k': "it's"}]"#);
    }
}
