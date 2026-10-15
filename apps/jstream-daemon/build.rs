//! Delay-loads the Media Foundation DLLs on Windows with MSVC, so the daemon starts on a
//! Windows without Media Foundation (an N edition without the Media Feature Pack) and
//! reports that itself.
//!
//! The `windows` crate imports Media Foundation as ordinary load-time imports, so the
//! loader would refuse to start the binary before `main` runs, with a system error box
//! that names a DLL. Delay-loaded, each DLL is loaded on the first call into it, which
//! comes only after the startup check has passed.
//!
//! Docs: apps/jstream-daemon/README.md#design

use std::env;

/// The Media Foundation DLLs the daemon imports from: `mfplat.dll` (`MfCodecs`,
/// `MfVideoDevices`), `mf.dll` and `mfreadwrite.dll` (`MfVideoDevices`), and
/// `mfsensorgroup.dll` (`WindowsVcam`), which N editions also lack and Windows 10 has
/// without `MFCreateVirtualCamera` (`WindowsVcam::start` checks for it before calling
/// it). Only DLLs the binary imports from belong here: delay-loading one it doesn't is
/// linker warning LNK4199, which CI's `-D warnings` turns into an error
/// (`linker_messages`). `tests/delay_load.rs` checks the built binary.
const MEDIA_FOUNDATION: [&str; 4] = [
    "mfplat.dll",
    "mf.dll",
    "mfreadwrite.dll",
    "mfsensorgroup.dll",
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // The target's, not the host's: build scripts run on the host.
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let toolchain = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os == "windows" && toolchain == "msvc" {
        // Only the daemon binary: its test binaries run on machines that have Media
        // Foundation, and `rustc-link-arg` would reach them too.
        for dll in MEDIA_FOUNDATION {
            println!("cargo:rustc-link-arg-bins=/DELAYLOAD:{dll}");
        }
        // The delay-load helper that resolves the imports on first call.
        println!("cargo:rustc-link-arg-bins=delayimp.lib");
    }
}
