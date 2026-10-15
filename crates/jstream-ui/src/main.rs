//! The web UI's entry point, built for `wasm32` by `trunk` (see the crate README).
//!
//! Docs: crates/jstream-ui/README.md#build

#![forbid(unsafe_code)]

fn main() {
    jstream_ui::start();
}
