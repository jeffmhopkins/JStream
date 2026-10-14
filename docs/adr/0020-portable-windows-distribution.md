<!-- @doc
id = "ADR-0020"
title = "Portable Windows distribution: run the exe, no installer"
kind = "adr"
status = "accepted"
tier = "maintainer"
-->

# ADR-0020: Portable Windows distribution: run the exe, no installer

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

Supersedes [ADR-0012](0012-virtual-camera-windows.md). It keeps ADR-0012's virtual-camera
design and replaces its installer decision.

<a id="context"></a>
## Context
The maintainer requires that JStream on Windows is an `.exe` you can launch without
installing anything. ADR-0012 instead ships a signed MSI. The MSI exists for one reason:
the virtual camera's media source is a COM object that the Windows camera Frame Server
loads, so it has to be registered
([`MFCreateVirtualCamera`](https://learn.microsoft.com/en-us/windows/win32/api/mfvirtualcamera/nf-mfvirtualcamera-mfcreatevirtualcamera)).
Nothing else in JStream needs an installer:

- The UI runs in WebView2 ([Tauri](https://v2.tauri.app/)), which Windows 11 ships as part
  of the OS.
- Media Foundation, Direct3D 12, Windows.Graphics.Capture and WASAPI are OS components.
  Windows "N" editions lack Media Foundation, and ADR-0008's support policy already covers
  them.
- Rust can link the C runtime statically, so no Visual C++ redistributable is needed.

What isn't known yet is whether Frame Server accepts a media source registered **per user**
(`HKCU`, no admin), or only one registered **machine-wide** (`HKLM`, admin). The reference
implementation ([VCamSample](https://github.com/smourier/VCamSample)) registers it
machine-wide. Spike S2 answers this.

<a id="decision"></a>
## Decision
- **Deliverable:** Windows ships as a portable zip. You unzip it anywhere and run
  `jstream.exe`, the desktop app with the engine in-process. The same folder holds the
  headless `jstream-daemon.exe`, `jstream-cli.exe` and the virtual-camera DLL. Running the
  app needs no installer, no admin rights, and no registry writes.
- **Self-contained binaries:** release builds link the C runtime statically (`crt-static`).
  At run time they depend only on OS components. If WebView2 or Media Foundation is
  missing, the app shows a clear message instead of failing obscurely.
- **Where state lives:** settings, scenes and logs go in `%APPDATA%\JStream` by default.
  When a `portable` marker file sits next to the exe, they go in a folder beside the exe
  instead, so the whole app can live on a USB stick.
- **Virtual camera (the design is unchanged from ADR-0012):**
  - `jstream-vcam-windows` is a `cdylib` media source written in Rust. It depends only on
    L0 crates.
  - Frames cross processes through a shared NT-handle texture plus a shared fence, with a
    versioned shared-memory control block (`jstream-vcam-proto`). Its security descriptors
    allow Frame Server's service account.
  - On the engine side, `VcamSink` creates the camera for the session.
  - The DLL must not panic across FFI, and it must survive the engine disappearing.
- **Virtual camera registration (new):** there is no installer, so the app registers the
  DLL from its own folder the first time the user turns the camera on.
  - If S2 shows per-user registration works, registration is silent and needs no admin.
  - Otherwise the app asks once for elevation (one UAC prompt) to register machine-wide,
    using a small mode of the same exe (`jstream.exe --register-vcam`).
  - The app offers **Unregister** in its settings and as a CLI command.
  - If the folder has moved since registration, the app detects that and offers to
    re-register.
  - Streaming, recording and everything else never need the virtual camera, and so never
    need admin.
- **Signing:** release exes and the DLL are code-signed when a certificate is available, so
  SmartScreen trusts them. Unsigned development builds work the same way; SmartScreen just
  warns first.
- **Out of scope:** an optional MSI or winget package for people who want one. It can be
  added later without changing this decision. Auto-update needs its own ADR.

<a id="consequences"></a>
## Consequences
- No installer project in M1. M1c becomes "virtual camera with in-app registration". The
  `installer/` tree in the repo layout is dropped.
- The first time the camera is enabled it may cost one UAC prompt, depending on S2. Nothing
  else ever does.
- The app must cope with running from any folder, including read-only or removable media.
  It must never write next to the exe unless the portable marker is present.
- A registered camera points at the DLL's path. Moving or deleting the folder breaks the
  camera until the user re-registers or unregisters, so the app has to detect and explain
  this.
- Spike S2 must now answer two questions, and record the answers in ARCHITECTURE#packaging:
  1. Does per-user (`HKCU`) registration work with Frame Server?
  2. What does elevated registration need?
