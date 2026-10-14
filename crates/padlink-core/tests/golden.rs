//! Golden fixtures from the Python implementation (`tools/gen_fixtures.py`).
//!
//! Phase 0 only checks they are present and well-formed; each module ported in
//! Phase 1 adds a test here that replays its fixture file.

use std::path::PathBuf;

use serde_json::Value;

fn load(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/golden").join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn fixtures_are_present() {
    for name in ["hiddesc", "profiles", "personas", "translator", "ff", "misc", "art"] {
        assert!(load(name).is_object(), "{name}.json is not a JSON object");
    }
    assert!(!load("hiddesc")["cases"].as_array().unwrap().is_empty());
    assert_eq!(load("personas")["personas"].as_array().unwrap().len(), 6);
}
