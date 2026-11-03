//! Makes the Linux helper find `libcef.so` beside itself (`$ORIGIN`), where it ships and
//! where `cef-dll-sys` copies it in a build folder (ADR-0095).
//!
//! Docs: apps/jstream-browser-helper/README.md#purpose

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo::rustc-link-arg-bins=-Wl,-rpath,$ORIGIN");
    }
}
