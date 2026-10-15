//! Delay-loads `mfplat.dll` on Windows with MSVC, so the daemon starts on a Windows
//! without Media Foundation (an N edition without the Media Feature Pack) and reports
//! that itself.
//!
//! The `windows` crate imports Media Foundation as ordinary load-time imports, so the
//! loader would refuse to start the binary before `main` runs, with a system error box
//! that names a DLL. Delay-loaded, `mfplat.dll` is loaded on the first call into it,
//! which comes only after `check_media_foundation` has passed.
//!
//! Docs: apps/jstream-daemon/README.md#design

use std::env;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // The target's, not the host's: build scripts run on the host.
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let toolchain = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os == "windows" && toolchain == "msvc" {
        // Only the daemon binary: its test binaries run on machines that have Media
        // Foundation, and `rustc-link-arg` would reach them too.
        println!("cargo:rustc-link-arg-bins=/DELAYLOAD:mfplat.dll");
        // The delay-load helper that resolves the imports on first call.
        println!("cargo:rustc-link-arg-bins=delayimp.lib");
    }
}
