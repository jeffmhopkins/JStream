//! On Windows: generates the Tauri context (`tauri.conf.json`, the icon) and, with MSVC,
//! delay-loads the Media Foundation DLLs, so the app starts on a Windows without Media
//! Foundation and says so in its window, as the daemon does (its `build.rs`).
//!
//! Docs: apps/jstream-desktop/README.md#design

// `MEDIA_FOUNDATION`: the DLLs to delay-load, shared with `tests/delay_load.rs`.
#[cfg(windows)]
include!("media_foundation.rs");

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=media_foundation.rs");
    #[cfg(windows)]
    {
        // Declares `pick_path` so tauri-build writes `allow-pick-path` (the `browse`
        // capability grants that permission and no other). Without this the Windows
        // build rejects the capability as an unknown permission.
        let built = tauri_build::try_build(
            tauri_build::Attributes::new()
                .app_manifest(tauri_build::AppManifest::new().commands(&["pick_path"])),
        );
        if let Err(error) = built {
            panic!("could not build the Tauri context: {error:#}");
        }
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
