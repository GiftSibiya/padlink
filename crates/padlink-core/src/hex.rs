//! Hex helpers for raw HID reports, and sysfs capability bitmasks.

use std::collections::BTreeSet;

/// Parse bytes written by a person: `"05 ff 00"`, `"05ff00"`, `"0x05,0xff,0x00"`, `"5:f:0"`.
pub fn parse_hex(text: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for tok in text.replace([',', ':'], " ").split_whitespace() {
        let tok = tok.to_lowercase();
        let tok = tok.strip_prefix("0x").unwrap_or(&tok);
        if tok.len() > 2 && tok.len() % 2 == 0 {
            for pair in tok.as_bytes().chunks(2) {
                let pair = std::str::from_utf8(pair).map_err(|_| format!("invalid hex {tok:?}"))?;
                out.push(u8::from_str_radix(pair, 16).map_err(|_| format!("invalid hex {tok:?}"))?);
            }
        } else {
            let value = i64::from_str_radix(tok, 16).map_err(|_| format!("invalid hex {tok:?}"))?;
            out.push(u8::try_from(value).map_err(|_| "byte must be in range(0, 256)".to_string())?);
        }
    }
    Ok(out)
}

/// `"05 ff 00"`.
pub fn hexstr(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
}

/// Parse a sysfs capability bitmask (`"1f 0 ffff"`): unpadded unsigned longs,
/// most significant word first.
pub fn parse_bitmask(text: Option<&str>) -> BTreeSet<u32> {
    let mut bits = BTreeSet::new();
    let Some(text) = text else { return bits };
    let word_bits = libc::c_ulong::BITS;
    for (idx, word) in text.split_whitespace().rev().enumerate() {
        let Ok(mut value) = u128::from_str_radix(word, 16) else { continue };
        let mut bit = 0;
        while value != 0 {
            if value & 1 == 1 {
                bits.insert(idx as u32 * word_bits + bit);
            }
            value >>= 1;
            bit += 1;
        }
    }
    bits
}

/// The width of a sysfs bitmask word (the platform `unsigned long`).
pub fn long_bits() -> u32 {
    libc::c_ulong::BITS
}
