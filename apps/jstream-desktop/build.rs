//! Says where the app has its window (`desktop_window`: Windows and macOS, ADR-0063),
//! generates the Tauri context there (`tauri.conf.json`, the icon) and, with MSVC,
//! delay-loads the Media Foundation DLLs, so the app starts on a Windows without Media
//! Foundation and says so in its window, as the daemon does (its `build.rs`).
//!
//! Docs: apps/jstream-desktop/README.md#design

// `MEDIA_FOUNDATION`: the DLLs to delay-load, shared with `tests/delay_load.rs`.
#[cfg(windows)]
include!("media_foundation.rs");

/// The target OSes with a window, where `tauri` is a dependency (the manifest's target
/// tables). Elsewhere the binary says to use the daemon.
const WINDOWED: &[&str] = &["windows", "macos"];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=media_foundation.rs");
    // Tauri's context macro embeds it on macOS (`Info.plist`'s own comment).
    println!("cargo:rerun-if-changed=Info.plist");
    println!("cargo::rustc-check-cfg=cfg(desktop_window)");
    // The target's, not the host's: build scripts run on the host.
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if WINDOWED.contains(&os.as_str()) {
        println!("cargo::rustc-cfg=desktop_window");
    }
    // `tauri-build` is a build dependency only for a windowed target, and this script is
    // compiled for the host, so it runs on a native build there (a cross check from
    // another OS has no context to generate).
    #[cfg(any(windows, target_os = "macos"))]
    {
        // Declares `pick_path` and `set_network` so tauri-build writes
        // `allow-pick-path` and `allow-set-network` (the `browse` and `network`
        // capabilities grant those and no other). Without this the build rejects the
        // capabilities as unknown permissions.
        let built =
            tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
                tauri_build::AppManifest::new().commands(&["pick_path", "set_network"]),
            ));
        if let Err(error) = built {
            panic!("could not build the Tauri context: {error:#}");
        }
    }
    #[cfg(windows)]
    {
        let toolchain = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
        if toolchain == "msvc" {
            for dll in MEDIA_FOUNDATION {
                println!("cargo:rustc-link-arg-bins=/DELAYLOAD:{dll}");
            }
            println!("cargo:rustc-link-arg-bins=delayimp.lib");
        }
    }
}
