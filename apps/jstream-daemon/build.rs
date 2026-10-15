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

/// The Media Foundation DLLs the daemon imports from: `mfplat.dll` (`MfCodecs`). N
/// editions also lack the ones other backends import from: `mf.dll` and
/// `mfreadwrite.dll` (`MfVideoDevices`) and `mfsensorgroup.dll` (`WindowsVcam`). Add
/// each here when the daemon links its backend, not before: delay-loading a DLL the
/// binary doesn't import from is linker warning LNK4199, which CI's `-D warnings`
/// turns into an error (`linker_messages`). `tests/delay_load.rs` fails if any of
/// them is a load-time import.
const MEDIA_FOUNDATION: [&str; 1] = ["mfplat.dll"];

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
