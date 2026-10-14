//! padlink core library.
//!
//! Ported from the original Python implementation (see docs/padlink/rust-migration.md
//! in Dirah), which has since been removed. Its behaviour is pinned by the frozen golden
//! fixtures in `fixtures/golden/`, which tests/golden.rs replays.

pub mod art;
pub mod bridge;
pub mod capture;
pub mod codes;
mod codes_table;
pub mod discovery;
pub mod doctor;
pub mod driver;
pub mod ff;
pub mod gamesetup;
pub mod hex;
pub mod hiddesc;
pub mod hidraw;
pub mod integration;
pub mod logger;
pub mod paths;
pub mod personas;
pub mod prober;
pub mod profiles;
pub mod py;
pub mod settings;
pub mod sink;
pub mod translate;
