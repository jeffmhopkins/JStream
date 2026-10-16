//! The browser helper (ADR-0048): `jstream-browser-helper.dll`, which CEF's bootstrap exe,
//! shipped renamed as `jstream-browser-helper.exe`, loads and calls through its
//! `RunWinMain` export. The bootstrap sets up Chromium's sandbox first, so the process
//! is CEF's browser process for the engine's browser pages, and every renderer, GPU and
//! utility process starts as the same exe, sandboxed. All it does is hand the process to
//! `jstream_browser::run_helper`.
//!
//! Docs: docs/ARCHITECTURE.md#shape, docs/ARCHITECTURE.md#crate-map, apps/jstream-browser-helper/README.md#purpose, docs/adr/0048-sandboxed-browser-helper.md

/// CEF's bootstrap exe calls this with the sandbox it set up (`sandbox_info`) and
/// CEF's version (unused: the helper is built against the CEF it ships with). Returns
/// the process's exit code.
///
/// # Safety
/// Only CEF's bootstrap exe calls it, with its own sandbox information.
#[cfg(all(windows, feature = "cef"))]
#[unsafe(no_mangle)]
#[allow(non_snake_case, reason = "the name CEF's bootstrap exe looks up")]
pub unsafe extern "C" fn RunWinMain(
    _instance: *mut std::ffi::c_void,
    _command_line: *mut u16,
    _show: i32,
    sandbox_info: *mut std::ffi::c_void,
    _version_info: *mut std::ffi::c_void,
) -> i32 {
    // SAFETY: the caller is the bootstrap exe (this function's contract), and its
    // sandbox information goes to CEF unchanged.
    unsafe { jstream_browser::run_helper(sandbox_info) }
}
