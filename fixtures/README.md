# Fixtures

- `golden/*.json`: expected values recorded from the original Python implementation of
  padlink (removed after the Rust port). `crates/padlink-core/tests/golden.rs` replays every
  one. They are frozen: a failing golden test means behaviour changed (file formats, what
  games see through the bridge), so only edit a fixture deliberately.
- `hid/*.hex`: the raw HID report descriptors the `hiddesc` and `prober` cases came from
  (real devices plus two synthetic ones).
