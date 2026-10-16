//! On Windows: generates the Tauri context (`tauri.conf.json`, the icon) and, with MSVC,
//! delay-loads the Media Foundation DLLs, so the app starts on a Windows without Media
//! Foundation and says so in its window, as the daemon does (its `build.rs`).
//!
//! Docs: apps/jstream-desktop/README.md#design

/// The Media Foundation DLLs the backends import from, as in the daemon's build script,
/// including `mfsensorgroup.dll`, which `WindowsVcam` imports `MFCreateVirtualCamera`
/// from; `WindowsVcam::start` probes for that export before calling it.
#[cfg(windows)]
const MEDIA_FOUNDATION: [&str; 4] = [
    "mfplat.dll",
    "mf.dll",
    "mfreadwrite.dll",
    "mfsensorgroup.dll",
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(windows)]
    {
        tauri_build::build();
        // The target's, not the host's: build scripts run on the host.
        let toolchain = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
        if toolchain == "msvc" {
            for dll in MEDIA_FOUNDATION {
                println!("cargo:rustc-link-arg-bins=/DELAYLOAD:{dll}");
            }
            println!("cargo:rustc-link-arg-bins=delayimp.lib");
        }
    }
}
