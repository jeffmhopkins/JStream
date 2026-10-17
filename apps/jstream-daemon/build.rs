//! Delay-loads the Media Foundation DLLs on Windows with MSVC, so the daemon starts on a
//! Windows without Media Foundation (an N edition without the Media Feature Pack) and
//! reports that itself.
//!
//! The `windows` crate imports Media Foundation as ordinary load-time imports, so the
//! loader would refuse to start the binary before `main` runs, with a system error box
//! that names a DLL. Delay-loaded, each DLL is loaded on the first call into it, which
//! comes only after the startup check has passed.
//!
//! On macOS it embeds `Info.plist` in the binary's `__TEXT,__info_plist` section, so the
//! camera, microphone and system audio recording prompts have the usage strings macOS
//! requires, with no app bundle (ADR-0063 §11).
//!
//! Docs: apps/jstream-daemon/README.md#design

use std::env;

/// The Media Foundation DLLs the daemon imports from: `mfplat.dll` (`MfCodecs`,
/// `MfVideoDevices`, `MfMediaDecoders`), `mf.dll` (`MfVideoDevices`), `mfreadwrite.dll`
/// (`MfVideoDevices`, `MfMediaDecoders`), and `mfsensorgroup.dll` (`WindowsVcam`),
/// which N editions also lack and Windows 10 has without `MFCreateVirtualCamera`
/// (`WindowsVcam::start` checks for it before calling it). Only DLLs the binary imports
/// from belong here. The linker ignores one it doesn't with warning LNK4199, and that
/// warning doesn't fail CI: rustc reports it through the `linker_messages` lint, which
/// ignores `-D warnings`. What catches a stale entry is `tests/delay_load.rs`, which
/// requires each DLL in its own copy of this list to be in the built binary's
/// delay-import table and none in its import table.
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
        // Only the daemon binary and its unit-test harness (`-bins` reaches both), not
        // the integration tests, which run on machines that have Media Foundation and
        // which `rustc-link-arg` would reach too.
        for dll in MEDIA_FOUNDATION {
            println!("cargo:rustc-link-arg-bins=/DELAYLOAD:{dll}");
        }
        // The delay-load helper that resolves the imports on first call.
        println!("cargo:rustc-link-arg-bins=delayimp.lib");
    }
    if os == "macos" {
        println!("cargo:rerun-if-changed=Info.plist");
        let dir = env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
        // The binary and its unit-test harness, as above; `-sectcreate` copies the file
        // into the section as is. The path can't hold a comma, which `-Wl` splits on.
        // The README's Purpose describes this embedding; Design, the delay-load above.
        println!("cargo:rustc-link-arg-bins=-Wl,-sectcreate,__TEXT,__info_plist,{dir}/Info.plist");
    }
}
