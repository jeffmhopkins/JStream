//! CEF's subprocess exe for the separate-helper layout (`--helper separate`): renderer,
//! GPU, utility and other processes start as this small exe instead of `s4-browser.exe`
//! (ADR-0021, Packaging). It does nothing but hand control to CEF.

#[cfg(windows)]
fn main() {
    // Fixes the API version the bindings use, as in the browser process.
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
    let args = cef::args::Args::new();
    let code = cef::execute_process(Some(args.as_main_args()), None, std::ptr::null_mut());
    std::process::exit(code);
}

#[cfg(not(windows))]
fn main() {
    eprintln!("the S4 helper runs on Windows only");
    std::process::exit(1);
}
