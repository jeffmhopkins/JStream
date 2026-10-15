//! Embeds `s4-browser.exe.manifest` in both exes when linking with MSVC's linker.

fn main() {
    println!("cargo::rerun-if-changed=s4-browser.exe.manifest");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|e| e == "msvc");
    if !msvc {
        return;
    }
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
    let manifest = std::path::Path::new(&dir).join("s4-browser.exe.manifest");
    println!("cargo::rustc-link-arg-bins=/MANIFEST:EMBED");
    println!(
        "cargo::rustc-link-arg-bins=/MANIFESTINPUT:{}",
        manifest.display()
    );
}
