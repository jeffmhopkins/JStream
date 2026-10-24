<!-- @doc
id = "SPIKE-S2"
title = "Spike S2: Windows virtual camera"
kind = "guide"
status = "draft"
review_every = "180d"
-->

# Spike S2: Windows virtual camera

Throwaway code for spike S2 in [ROADMAP M0](../../ROADMAP.md#m0), Track B. It checks,
on a real Windows 11 machine, the path the virtual-camera ADRs fix: a **Rust COM media
source** that the Windows camera **Frame Server** loads, fed **cross-process** through a
shared NT-handle texture and a shared fence.

It also answers the registration question from ADR-0020 (portable Windows
distribution, since superseded by
[ADR-0066](../../docs/adr/0066-portable-windows-distribution-v2.md)):
does Frame Server accept a media source registered **per user** (`HKCU`, no admin), or
does it need a **machine-wide** registration (`HKLM`, one elevation)? The results PR must
record the answer in [ARCHITECTURE › Packaging](../../docs/ARCHITECTURE.md#packaging),
as ADR-0020 requires. ADR-0020 also drops the MSI, so the MSIX question in ROADMAP M0 is
out of scope here.

The crate is not a workspace member. It has its own `[workspace]` table and its own
`Cargo.lock`. Results don't go in this file. They come from the maintainer's run, and a
later PR records them.

<a id="what"></a>
## What it does
<!-- @section
-->

`cargo build --release` produces two files in `target\release`:

- **`s2_vcam.dll`**, the media source. It ports the design of VCamSample
  (`smourier/VCamSample` on GitHub) to Rust with the `windows` crate:
  - The registered class is an `IMFActivate`. Its `ActivateObject` creates an
    `IMFMediaSourceEx` (also `IMFGetService`, `IKsControl` and `IMFAttributes`) with one
    `IMFMediaStream2` (also `IKsControl` and `IMFAttributes`).
  - The stream offers 1920x1080 at 30 fps as NV12 and RGB32, with the device-stream
    attributes Frame Server expects (video capture pin, Frame Server shared, colour
    source) and VCamSample's two sensor profiles.
  - Every COM method and export runs inside a panic guard that returns `E_UNEXPECTED`
    and logs, so no panic crosses into Frame Server.
  - The DLL exports `DllGetClassObject`, `DllCanUnloadNow`, `DllRegisterServer` (HKLM,
    for `regsvr32`) and `DllUnregisterServer`.
- **`s2-vcam.exe`**, the producer and registration tool:
  - `--register hkcu|hklm` writes `Software\Classes\CLSID\{491711D3-…}\InprocServer32`
    (the DLL next to the exe, `ThreadingModel = Both`) in that hive. It also grants
    LOCAL SERVICE and ALL APPLICATION PACKAGES read and execute on the DLL file with
    `icacls`. Frame Server runs as LOCAL SERVICE, which can't read files in your user
    profile by default. Pass `--no-acl` to skip the grant.
  - `--unregister` removes both registrations. `--status` shows what is registered where.
  - With no mode, it **runs the producer**. It creates three shared BGRA textures and a
    shared fence, named and with an explicit SDDL DACL that allows SYSTEM,
    Administrators, LOCAL SERVICE and interactive users. It also creates a small
    shared-memory control block with the format, adapter LUID, newest frame number, a
    timestamp and a liveness heartbeat. It renders a moving test pattern with plain D3D11
    clears and copies. It then calls `MFCreateVirtualCamera` (software camera source,
    session lifetime, current-user access) with the DLL's CLSID and starts the camera.
    It runs until you press Enter, or for `--seconds N`, and then prints a report.

**The hand-off.** For frame `n`, the producer renders into ring slot `n % 3`, queues
`Signal(fence, n)` and publishes `n` in the control block. Inside Frame Server, the DLL
reads `n`, queues `Wait(fence, n)` on its own D3D11 device (on the producer's adapter),
copies the slot into a staging texture and maps it. It then converts to NV12 on the CPU
and delivers a system-memory sample. The readback is deliberate: the question is the
cross-process path, not zero-copy delivery.

**Where the objects live.** The producer first tries the global namespace (`Global\…`).
A non-elevated user usually can't create a global file mapping, so it falls back to its
session's namespace. The DLL runs in session 0 and opens those objects through
`Session\<id>\…`. The report says which namespace was used.

**What you see.** The producer's picture is **dark teal with a white bar** moving right,
plus a row of 32 squares that count frames in binary. If the DLL is loaded but can't
reach a live producer, it shows its own **colour bars** with a moving black-and-white
line instead.

**Did Frame Server load it?** The report's `frame_server` object answers this, and
`decided_by` says which evidence decided:

- **`control_block`** (preferred). When the DLL attaches to the control block, it
  records its host's session id and flags (session 0, `svchost.exe`, LOCAL SERVICE by
  SID). It counts attaches, created sources and served samples there. The producer
  snapshots these at start and compares them at the end, so a DLL that stayed loaded
  in Frame Server from an earlier run still counts.
- **`dll_log`**, used when the DLL never reached the control block. The DLL logs to
  `C:\ProgramData\JStreamS2\vcam-<pid>.log` (one file per host process) and to
  `OutputDebugString`. Every new log file starts with a `LOADED` line naming the host
  process, account and session, even when the DLL was already loaded before the
  producer cleared the old logs. Every `ActivateObject` also logs it.
- **`none`**: no sign of the DLL at all.

If the two sources disagree, `frame_server.inconsistency` explains how, and the report
lists it under `errors`.

<a id="run"></a>
## How to run it (Windows)
<!-- @section
-->

You need Windows 11 (build 22000<!--fact:min-windows-build--> or later, for
`MFCreateVirtualCamera`), a GPU with a D3D11 driver, and the Rust toolchain from
`rust-toolchain.toml`. Use a normal PowerShell prompt unless a step says elevated.

**1. Build.**

```powershell
cd spikes\s2-vcam
cargo build --release
```

**2. Try per-user registration (no admin).**

```powershell
.\target\release\s2-vcam.exe --register hkcu
.\target\release\s2-vcam.exe --json | Tee-Object s2-hkcu.json
```

While it runs, open the **Camera** app, switch cameras until you find **"JStream S2"**,
and watch it for a few seconds. Optionally, also check that a browser lists it, such as
Edge on any page that asks for the camera. Then close the Camera app, go back to the
prompt and press **Enter**. The JSON report is printed and saved to `s2-hkcu.json`.

**3. If step 2 failed, try machine-wide registration.** Step 2 failed if "JStream S2"
isn't listed, if selecting it gives an error or a black picture, or if the report has
`"frame_server": {"loaded": false, …}`.

```powershell
.\target\release\s2-vcam.exe --unregister
```

Then, in an **elevated** PowerShell (Run as administrator), in the same folder, restart
Frame Server so the DLL from step 2 is unloaded, and register machine-wide:

```powershell
net stop FrameServer
.\target\release\s2-vcam.exe --register hklm
```

Back in the **normal** prompt, repeat the run:

```powershell
.\target\release\s2-vcam.exe --json | Tee-Object s2-hklm.json
```

Look for "JStream S2" again, and press Enter when done.

**4. Clean up.** Run `.\target\release\s2-vcam.exe --unregister`, from an elevated prompt
if you used `hklm`. The camera itself disappears when the exe exits, because it has
session lifetime.

Other options: `--seconds N` stops after N seconds instead of waiting for Enter. `--fps N`
sets the producer's rate (default 30). `--no-vcam` feeds the textures without creating
the camera. `--status` shows the registration. Run `s2-vcam.exe --help` to list them.

<a id="paste-back"></a>
## What to paste back
<!-- @section
-->

Paste these into the PR or issue:

1. The **JSON report** from each attempt (`s2-hkcu.json`, and `s2-hklm.json` if you got
   that far). It holds:
   - the Windows build and whether the prompt was elevated;
   - the registration mode and paths;
   - the namespace used for the shared objects;
   - the `MFCreateVirtualCamera`, `Start`, `Stop` and `Shutdown` HRESULTs, with their
     error text;
   - the DLL's counters for this run (attaches, sources created, samples served and how
     many came from the producer), its host's identity, and its last error;
   - whether Frame Server loaded the DLL (`frame_server.loaded`), which evidence
     decided (`frame_server.decided_by`), and the hosts named in the logs
     (`dll_log.loaded_by`);
   - the DLL's log lines and any errors.
2. For each attempt, **what the Camera app showed**: whether "JStream S2" was listed, and
   whether the picture was teal with a moving bar (the full path works), colour bars (the
   DLL works but can't reach the producer), black, or an error. Say the same for a browser
   if you tried one.
3. Anything unusual, such as a UAC prompt, a SmartScreen warning, an antivirus alert, or
   a `cargo build` that failed because the DLL was in use.

How to read the report:

- **`vcam.create.hr` is not `0x00000000`:** `MFCreateVirtualCamera` refused the source.
  Check the registration and the error text.
- **`create` and `start` are OK, `frame_server.loaded` is false and `decided_by` is
  `none`:** Frame Server never activated the DLL. With `hkcu`, this is the expected sign
  that per-user registration isn't seen.
- **`frame_server.loaded` is false but `decided_by` is `control_block` or `dll_log`:**
  the DLL ran, but in another process (see `consumer.pid`, `consumer.session_id` and
  `dll_log.loaded_by`), such as an app loading it in-process. That is not Frame Server.
- **`frame_server.loaded` is true, and `consumer.frames_shared_this_run` is greater
  than 0:** the whole path works in that registration mode.
- **`frame_server.loaded` is true, but no samples came from the producer:** the DLL ran
  but couldn't open the shared objects. If `decided_by` is `dll_log`, it didn't even find
  the control block. The log lines say which name or call failed.
- **`frame_server.inconsistency` is set:** the two kinds of evidence disagree. Paste the
  report as it is; the control block's answer is the one used.

<a id="risks"></a>
## Known risks and uncertainties
<!-- @section
-->

This code was cross-checked from Linux with `cargo check` and `cargo clippy` for
`x86_64-pc-windows-msvc`, and the platform-independent parts were unit-tested. **It has
never been linked or run.** These are the parts most likely to fail at the first run,
roughly in order:

- **HKCU visibility.** Frame Server runs as LOCAL SERVICE. Whether it resolves the CLSID
  through the calling user's `HKCU\Software\Classes` is exactly what the spike asks.
  Failing in `hkcu` mode and working in `hklm` mode is a valid result, not a bug.
- **File access to the DLL.** The DLL sits in your user profile, which LOCAL SERVICE
  can't read. `--register` grants it read and execute on the file itself. Every
  `cargo build` replaces the file and loses the grant, so re-run `--register` after
  rebuilding. To learn whether the grant is needed at all, run `--register … --no-acl`.
- **Cross-session names.** Opening the session-namespace objects from session 0 through
  `Session\<id>\…` is the least certain step. If it fails, you'll see colour bars, and
  the log will say "no control block" or name the failing call. The fallback order is:
  `Global\`, then the console session, then sessions 1 to 4.
- **Opening the shared objects by name.** The DLL opens the textures with
  `ID3D11Device1::OpenSharedResourceByName`. It opens the fence with D3D12's
  `OpenSharedHandleByName`, because D3D11 has no by-name call for fences, and then with
  `ID3D11Device5::OpenSharedFence`. A driver could reject either step.
- **Activation details ported from VCamSample.** These are: the `IMFActivate` wrapper,
  the attributes it sets, the sensor profiles, `IKsControl` answering "not found" to
  everything, no `IMFSampleAllocatorControl`, and an ignored `SetD3DManager` (so samples
  are always system memory). If Frame Server is stricter than this, the log shows the
  last COM call it made.
- **Pacing and tearing.** `RequestSample` answers right away, with no pacing. The ring
  has 3 slots and no back-pressure, so a DLL more than 3 frames behind could copy a slot
  while the producer rewrites it. Look for torn bars.
- **The DLL stays loaded.** While Frame Server holds the DLL, `cargo build` can't
  overwrite it. Close every camera app, run `--unregister`, and wait, or restart the
  "Windows Camera Frame Server" service from an elevated prompt (`net stop FrameServer`).
- **Hybrid GPUs.** The DLL opens a device on the producer's adapter, by LUID. That
  adapter must be usable from session 0.
- **A dying producer.** The DLL only copies a frame whose fence signal has already
  completed (`GetCompletedValue`), so it never blocks inside Frame Server on a signal
  that a dead producer can't deliver. It shows colour bars instead.
- **Security is spike-grade.** Interactive users get full control of the log folder and
  of the shared objects. So a local user could plant a link where Frame Server writes its
  log, or write into the control block. That's fine on the maintainer's machine. The M1
  crates (`jstream-vcam-windows`, `jstream-vcam-proto`) must not copy it: they need a
  service-owned log location or ETW, and DACLs narrowed to the producing user's SID plus
  LOCAL SERVICE.
