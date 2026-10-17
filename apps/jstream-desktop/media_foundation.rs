// The DLLs `build.rs` delay-loads. Both `build.rs` and `tests/delay_load.rs` `include!`
// this file, so the test checks exactly the list the build passes to the linker.

/// The Media Foundation DLLs the backends import from, as in the daemon's build script,
/// including `mfsensorgroup.dll`, which `WindowsVcam` imports `MFCreateVirtualCamera`
/// from; `WindowsVcam::start` probes for that export before calling it. Only DLLs the
/// binary imports from belong here: the linker ignores one it doesn't with warning
/// LNK4199, which doesn't fail CI, so `tests/delay_load.rs` is what catches a stale entry.
const MEDIA_FOUNDATION: [&str; 4] = [
    "mfplat.dll",
    "mf.dll",
    "mfreadwrite.dll",
    "mfsensorgroup.dll",
];
