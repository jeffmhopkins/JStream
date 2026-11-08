//! Where the app has a window (Windows, and Linux with the `webkitgtk` feature):
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
    println!("cargo::rustc-check-cfg=cfg(webview)");
    // The target's, not the host's: build scripts run on the host.
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let webkitgtk = std::env::var_os("CARGO_FEATURE_WEBKITGTK").is_some();
    if os == "windows" || (os == "linux" && webkitgtk) {
        println!("cargo:rustc-cfg=webview");
    }
    #[cfg(any(windows, all(target_os = "linux", feature = "webkitgtk")))]
    {
        // Declares `pick_path`, `set_network` and `set_jtunnel` so tauri-build writes
        // `allow-pick-path`, `allow-set-network` and `allow-set-jtunnel` (the `browse`,
        // `network` and `jtunnel` capabilities grant those and no other). Without this the Windows build rejects
        // the capabilities as unknown permissions.
        let built = tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
            tauri_build::AppManifest::new().commands(&["pick_path", "set_network", "set_jtunnel"]),
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
