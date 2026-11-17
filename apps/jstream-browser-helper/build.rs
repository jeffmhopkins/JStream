//! Makes the Linux helper find `libcef.so` beside itself (`$ORIGIN`), where it ships and
//! where `cef-dll-sys` copies it in a build folder (ADR-0095). On macOS, links the C++
//! standard library: `cef-dll-sys` compiles CEF's framework loader and sandbox entry
//! (`cef_scoped_library_loader_mac.mm`, `cef_scoped_sandbox_context_mac.mm`) into its
//! rlib without naming `libc++`, and the helper is what calls them (ADR-0157). It is a
//! link argument for the executable: a `rustc-link-lib` here would reach only the
//! package's library, the Windows DLL, and never the executable's link line.
//!
//! Docs: apps/jstream-browser-helper/README.md#purpose

fn main() {
    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("linux") => println!("cargo::rustc-link-arg-bins=-Wl,-rpath,$ORIGIN"),
        Ok("macos") => println!("cargo::rustc-link-arg-bins=-lc++"),
        _ => {}
    }
}
