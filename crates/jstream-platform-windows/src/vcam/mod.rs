//! The engine side of the virtual camera (`VcamSink`, ADR-0020): it publishes frames into
//! the `jstream-vcam-proto` contract that the `jstream-vcam-windows` media source reads
//! inside Frame Server, and creates the camera with `MFCreateVirtualCamera`.
//!
//! The contract fixes the format: 1920x1080 BGRA at 30 fps, in a ring of shared textures.
//! The checks on formats and frames (`format`) make no Windows calls, so they are tested
//! on every platform; [`WindowsVcam`] is the Windows sink.
//!
//! # Behaviour
//!
//! **`WindowsVcam`** (`VcamSink`), the virtual camera's engine side
//! (ADR-0066). It is the producer of
//! the `jstream-vcam-proto` contract that the
//! `jstream-vcam-windows` media source reads
//! inside Frame Server:
//! - **Format:** only the contract's, 1920x1080 BGRA at 30 fps (`offered_format`); any
//!   other `VcamFormat` is `Unsupported`. Frames are system-memory BGRA
//!   (`VideoSurface::Cpu`, any stride); GPU frames are `Unsupported` for now (a GPU
//!   path would copy the engine's texture into the ring on the D3D11 side).
//! - **Start:** `start` first checks that `mfsensorgroup.dll` loads from System32 and
//!   exports `MFCreateVirtualCamera` (Windows 11), and is `Unsupported` without it, so a
//!   binary that delay-loads the DLL gets an error rather than a delay-load exception on
//!   Windows 10 or an N edition. The media source must already be registered
//!   machine-wide, so `start` then checks for its `HKLM` class key and is `Unsupported` without it (registering
//!   is the optional `jstream-daemon --register-vcam`, through `register_vcam` in `crate::vcam_register`). It then creates, on a D3D11 device of its own
//!   on the default hardware adapter, a ring of three shared BGRA textures and a shared
//!   fence, plus the control block. They are named in **this session's namespace**,
//!   because a non-elevated engine can't create `Global\` objects
//!   (ARCH › Packaging), and secured with the
//!   contract's DACL for this user. Last, it creates the camera with
//!   `MFCreateVirtualCamera` for the current user and this session, and starts it.
//! - **One producer per session:** a live control block with a fresh heartbeat in this
//!   session (another engine, or another sink in this process) makes `start`
//!   `InvalidState`. A stale one, left by a producer that died, is re-initialised as the
//!   contract says: marked empty, rewritten, then marked live, but only if its owner and
//!   DACL are ones the sink would have set (#356): `CreateFileMappingW` keeps an existing
//!   mapping's security, so the sink reads it back (`GetSecurityInfo`, as SDDL text) and
//!   `start` is `InvalidState` unless the owner is this user, SYSTEM or Administrators and
//!   no allow entry grants more than `READ_CONTROL`, `SYNCHRONIZE` or
//!   `FILE_READ_ATTRIBUTES` to anyone but them and LOCAL SERVICE. That rule (`security`)
//!   works on the text, so it is tested on every platform.
//! - **Publish:** each frame is copied into its ring slot (`UpdateSubresource`), the
//!   fence is signalled with the frame's number, and the number is published as
//!   `latest_frame`. It never waits on the DLL, which may have died.
//! - **Liveness:** a heartbeat thread refreshes the block's heartbeat every 200 ms while
//!   the sink is started, so a pause in `publish` freezes the camera on its last frame
//!   instead of switching to the DLL's colour bars. Stopping, dropping the sink or the
//!   process ending lets the DLL fall back to them.
//! - **Stop:** removes the camera, stops the heartbeat, marks the block stopped and
//!   releases the objects. Dropping a started sink does the same.
//! - **Threads:** like the codecs, a started sink keeps Media Foundation started and
//!   the process MTA alive, so it may be driven from threads that never initialised COM.
//!   Start it from a thread that isn't an STA.

// Platform-independent, so it is tested on every platform.
#[cfg(any(windows, test))]
mod format;
#[cfg(windows)]
pub(crate) mod shared;
#[cfg(windows)]
mod sink;

#[cfg(windows)]
pub use sink::WindowsVcam;
