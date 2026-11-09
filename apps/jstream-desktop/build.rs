//! Where the app has a window (Windows, macOS, and Linux with the `webkitgtk` feature):
//! generates the Tauri context (`tauri.conf.json`, the icon) and sets the `webview` cfg
//! the window's code is gated on. On Windows with MSVC it also delay-loads the Media
//! Foundation DLLs, so the app starts on a Windows without Media Foundation and says so
//! in its window, as the daemon does (its `build.rs`).
//!
//! Docs: apps/jstream-desktop/README.md#design

// `MEDIA_FOUNDATION`: the DLLs to delay-load, shared with `tests/delay_load.rs`.
#[cfg(windows)]
include!("media_foundation.rs");

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=media_foundation.rs");
    // Tauri's context macro embeds it on macOS (`Info.plist`'s own comment).
    println!("cargo:rerun-if-changed=Info.plist");
    println!("cargo::rustc-check-cfg=cfg(webview)");
    // The target's, not the host's: build scripts run on the host.
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let webkitgtk = std::env::var_os("CARGO_FEATURE_WEBKITGTK").is_some();
    if os == "windows" || os == "macos" || (os == "linux" && webkitgtk) {
        println!("cargo:rustc-cfg=webview");
    }
    // `tauri-build` is a build dependency only there, and this script is compiled for the
    // host, so it runs on a native build (a check from another OS has no context to
    // generate).
    #[cfg(any(
        windows,
        target_os = "macos",
        all(target_os = "linux", feature = "webkitgtk")
    ))]
    {
        // Declares `pick_path` and `set_network` so tauri-build writes
        // `allow-pick-path` and `allow-set-network` (the `browse` and `network`
        // capabilities grant those and no other). Without this the Windows build rejects
        // the capabilities as unknown permissions.
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
