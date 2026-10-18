<!-- @doc
id = "CRATE-jstream-vcam-windows"
title = "jstream-vcam-windows"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-vcam-windows

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-vcam-windows/src/**", "crates/jstream-vcam-windows/Cargo.toml"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-vcam-windows", "docs/ARCHITECTURE.md#packaging", "docs/adr/0066-portable-windows-distribution-v2.md", "docs/RULES.md#unsafe", "crates/jstream-vcam-proto/README.md#purpose"]
-->

The virtual camera's media source: a COM DLL (`jstream_vcam_windows.dll`) that the
Windows camera Frame Server loads, which serves the frames the engine publishes through
the [`jstream-vcam-proto`](../jstream-vcam-proto/README.md#purpose) contract. Layer 2,
and one of the crates allowed `unsafe` ([R-UNSAFE-001](../../docs/RULES.md#R-UNSAFE-001));
it depends only on layer-0 crates and Microsoft's `windows` bindings
([ADR-0066](../../docs/adr/0066-portable-windows-distribution-v2.md)). On other platforms it
builds only its pixel helpers, the sample pacer and the control-block trust rule,
which are tested everywhere.

Ported from spike S2 (a Rust port of VCamSample's design), which proved this path on
Windows 11 ([ARCHITECTURE › Packaging](../../docs/ARCHITECTURE.md#packaging)):
- **COM objects.** The registered class is an `IMFActivate`. Its `ActivateObject`
  creates an `IMFMediaSourceEx` (also `IMFGetService`, `IKsControl` and `IMFAttributes`)
  with one `IMFMediaStream2` (also `IKsControl` and `IMFAttributes`). The stream offers
  the contract's 1920x1080 at 30 fps as NV12 (current) and RGB32, with the device-stream
  attributes Frame Server expects and a legacy and a high-frame-rate sensor profile.
- **Frames.** The DLL finds the control block (`Global\`, then the console session, then
  sessions 1 to 4, as the contract orders them), opens the ring textures and the fence by
  name on the producer's adapter, and per sample copies the newest frame whose fence
  signal already completed (the contract's `choose_frame`), so it never waits on a
  producer that died. Samples are system memory. When no frame is safe to copy but the
  producer is still live and fresh, it repeats the last frame it copied; with no live
  producer, or after an error on the shared path, it serves its own colour bars. A
  producer that stops drops the opened objects at once, so one that restarts in the same
  process is reopened. It writes its identity and counters into the block's consumer half.
- **Pacing.** Each `RequestSample` takes the next slot on a 30 fps schedule (`pace`). A
  request whose slot is due is answered at once; a later one is answered from a Media
  Foundation timer work item (`MFScheduleWorkItem`), so the caller's thread never blocks
  and clients that ask faster than the frame rate get samples no faster. A timed sample
  whose stream has since stopped, restarted or shut down is dropped.
- **Trust.** The block is written by another process. A block found in a session's
  namespace is used only if that session is active (at the console, or an active remote
  connection) and the mapping's owner is the session's logged-on user, SYSTEM or
  Administrators (`trust`), so another user's engine, say in a session switched away
  from, can't feed this camera. While several sessions are active at once (Windows
  Server, several remote sessions), only the console session's block is used; another
  session's block is used only when it is the only active session, as when one remote
  session has taken over a workstation (#204). A block in use is checked again every 5 s
  (`RECHECK`) and dropped once it fails. The DLL runs as LOCAL SERVICE, which can't call
  `WTSQueryUserToken`, so it reads the owner with `GetSecurityInfo`, the active
  sessions with `WTSEnumerateSessionsW` and the session's user with `WTSQuerySessionInformationW` and `LookupAccountNameW`; anything unreadable
  refuses the block. `Global\` blocks are not owner-checked: only administrators and
  services can create them. The DLL then requires the contract's format
  (`format_matches`), requires the scope and session the block names to be the
  namespace it was found in, and after opening the shared objects re-checks that the
  block is still live for the same producer. The ring textures and the fence of a
  session block pass the same owner rule (#356): each is opened by name as an NT handle
  (`ID3D12Device::OpenSharedHandleByName`), its owner read with `GetSecurityInfo`, and
  the D3D11 object opened from that handle (`OpenSharedResource1`, `OpenSharedFence`),
  so the object checked is the object used.
- **Panics.** Every COM method and export runs inside a panic guard that returns
  `E_UNEXPECTED` and logs ([R-UNSAFE-005](../../docs/RULES.md#R-UNSAFE-005)).
- **Log.** Frame Server gives the DLL no `tracing` subscriber, so it writes short lines
  to the debugger (`OutputDebugString`) and to `JStream\vcam\vcam.log` in the host
  account's own local app data folder (for Frame Server, LOCAL SERVICE's profile, which
  no interactive user can write), capped at 1 MiB with one older file.
- **Registration.** `DllRegisterServer` (`regsvr32`, elevated) writes
  `HKLM\Software\Classes\CLSID\{…}` with `InprocServer32` pointing at the DLL and
  `ThreadingModel = Both`; `DllUnregisterServer` removes it. That registers the DLL
  where it is, so it is for development and the [hardware check](#hardware-check) only.
  The product's optional step, `jstream-daemon --register-vcam` through
  `jstream-platform-windows`, writes the same keys but points them at an
  administrators-only copy under `%ProgramFiles%\JStream\vcam\`, never at the DLL in
  the user-writable portable folder, and `--unregister-vcam` removes the keys and the
  copy ([ADR-0066](../../docs/adr/0066-portable-windows-distribution-v2.md),
  [ARCHITECTURE › Packaging](../../docs/ARCHITECTURE.md#packaging)).

The DLL only opens the shared objects. The engine creates them, with the contract's DACL
(`sddl_objects`), in `jstream-platform-windows`.

**Tests.** The pixel helpers (test pattern, strided copies, BGRA to NV12), the pacer and
the session-block trust rule are unit-tested on every platform. The COM path, and the
Windows queries behind the trust rule, need Frame Server, so they are checked by hand
with the `vcam_smoke` example ([Hardware check](#hardware-check)). CI compiles the crate
and the example on Windows.

<a id="hardware-check"></a>
## Hardware check
<!-- @section
covers = ["crates/jstream-vcam-windows/examples/**"]
depends_on = ["docs/ARCHITECTURE.md#packaging"]
-->

`examples/vcam_smoke` stands in for the engine side: it publishes the ring, the fence and
the control block as the contract describes, in this session's namespace with the
contract's DACL, creates the camera with `MFCreateVirtualCamera`, renders dark teal with
a moving white bar, and prints what the DLL recorded. On Windows 11, from the repository
root:

1. Build: `cargo build --release -p jstream-vcam-windows --example vcam_smoke`.
2. Copy `target\release\jstream_vcam_windows.dll` to a folder outside your user profile,
   such as `C:\JStreamVcamTest\`, because Frame Server runs as LOCAL SERVICE and can't
   read your profile.
3. In an elevated prompt, `net stop FrameServer`, then
   `regsvr32 C:\JStreamVcamTest\jstream_vcam_windows.dll`.
4. In a normal prompt, run `target\release\examples\vcam_smoke.exe --seconds 60`, open
   the Camera app and pick "JStream Virtual Camera". `--no-producer` shows the DLL's
   colour bars instead; `--no-camera` only publishes frames.
5. Clean up in an elevated prompt with
   `regsvr32 /u C:\JStreamVcamTest\jstream_vcam_windows.dll`.

The report's `verdict` line says whether shared frames reached the camera, and
`samples per producer frame` should stay near 1 (the stream is paced to 30 fps). The first
run on hardware passed without step 2's folder needing any extra grant
([ARCHITECTURE › Packaging](../../docs/ARCHITECTURE.md#packaging)). The DLL's log
is `C:\Windows\ServiceProfiles\LocalService\AppData\Local\JStream\vcam\vcam.log`, readable
from an elevated prompt. Run from a normal, non-elevated session, it should have no
`refused` line: one means the session-block trust rule turned the smoke test's own block
away.
