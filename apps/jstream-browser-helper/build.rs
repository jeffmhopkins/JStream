//! Embeds `jstream-browser-helper.exe.manifest` when linking with MSVC's linker and the
//! `cef` feature is on. Chromium reads the Windows version through the manifest's
//! compatibility section; without it Windows reports itself to the process as 8.0
//! (spike S4, and CEF's own sample apps, do the same).
//!
//! Docs: apps/jstream-browser-helper/README.md#purpose

use std::env;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=jstream-browser-helper.exe.manifest");
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let toolchain = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let cef = env::var_os("CARGO_FEATURE_CEF").is_some();
    if os == "windows" && toolchain == "msvc" && cef {
        let dir = env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
        let manifest = Path::new(&dir).join("jstream-browser-helper.exe.manifest");
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
            manifest.display()
        );
    }
}
