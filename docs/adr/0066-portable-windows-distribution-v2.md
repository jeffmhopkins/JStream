<!-- @doc
id = "ADR-0066"
title = "Portable Windows distribution v2: an optional virtual camera, registered from an administrators-only copy"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0066: Portable Windows distribution v2: an optional virtual camera, registered from an administrators-only copy

<!-- @generated begin adr-status -->
> **Status:** proposed
<!-- @generated end adr-status -->

Supersedes [ADR-0020](0020-portable-windows-distribution.md) once accepted. It keeps all
of ADR-0020 except where the virtual camera's DLL is registered from, and makes
registering it explicitly optional.

<a id="context"></a>
## Context
[ADR-0020](0020-portable-windows-distribution.md) ships Windows as a portable zip and
registers the virtual camera's media source DLL (`jstream_vcam_windows.dll`) **from the
app's own folder**. Spike S2 found that Frame Server ignores per-user (`HKCU`)
registration, so the registration is machine-wide (`HKLM`) and costs one UAC prompt
([ARCHITECTURE › Packaging](../ARCHITECTURE.md#packaging)). The media source is a COM
object that the Windows camera Frame Server loads
([`MFCreateVirtualCamera`](https://learn.microsoft.com/en-us/windows/win32/api/mfvirtualcamera/nf-mfvirtualcamera-mfcreatevirtualcamera)),
the same way the reference implementation
([VCamSample](https://github.com/smourier/VCamSample)) registers it.

The portable folder is usually one the user can write to. So any program running as that
user can replace the registered DLL without a prompt, and its code then runs in Frame
Server's `svchost` (LOCAL SERVICE, session 0) the next time the camera loads (#242). Two
other fixes don't work. A hash check can't help, because Windows loads the DLL, not
JStream. A stub loader left in the same folder can be replaced the same way.

The maintainer decided, answering a question about the one-time admin prompt: "If you
have to register it, name it optional, and then yeah, I think the program files makes
sense".

<a id="decision"></a>
## Decision
Everything in ADR-0020's decision stands, with the virtual camera's registration replaced:

- **Deliverable:** a portable zip. You unzip it anywhere and run `jstream.exe`, the
  desktop app with the engine in-process. The same folder holds `jstream-daemon.exe`,
  `jstream-cli.exe` and the virtual camera DLL. Running the app needs no installer, no
  admin rights and no registry writes.
- **Self-contained binaries, state and signing:** as in ADR-0020.
  - Release builds link the C runtime statically.
  - State lives in `%APPDATA%\JStream`, or beside the exe when a `portable` marker is
    present.
  - Exes and the DLL are code-signed when a certificate is available.
- **Virtual camera design:** as in ADR-0020 (and ADR-0012 before it).
  - A Rust `cdylib` media source.
  - Shared NT-handle textures, a fence and a versioned control block
    (`jstream-vcam-proto`).
  - `VcamSink` on the engine side.
  - The DLL never panics across FFI.
- **Virtual camera registration (changed):**
  - **Optional.** Registering is an explicit opt-in step the user chooses, named as
    optional in the app and the docs. Nothing registers the camera or asks for
    elevation on its own: not startup, and not adding or starting a camera output. An
    unregistered camera output says it needs the optional step.
  - **One prompt, an administrators-only copy.** Inside that step's single UAC prompt
    (`jstream.exe --register-vcam`, or `jstream-daemon --register-vcam` until the app
    has that mode), the elevated process does three things. It creates
    `%ProgramFiles%\JStream\vcam\` with a protected DACL: SYSTEM and Administrators get
    full control, LOCAL SERVICE and Users get read and execute, and nothing is inherited.
    It copies the shipped DLL there. It points
    `HKLM\Software\Classes\CLSID\{clsid}\InprocServer32` at the copy. The DLL in the
    portable folder is never registered.
  - **Updates.** The app compares the registered copy with the DLL it ships, without
    elevation. When they differ, it offers to re-register. That is a new opt-in with its
    own prompt. Until then, Frame Server keeps loading the old copy. If the control
    block's contract version has changed, the old copy treats the engine as absent and
    shows its own colour bars.
  - **Unregister**, in the app's settings and as a CLI command, removes the keys and
    deletes the copy, with one prompt.
  - Moving or deleting the portable folder no longer breaks a registered camera.
  - Streaming, recording and everything else never need the virtual camera, and so never
    need admin.

<a id="consequences"></a>
## Consequences
- **What it fixes.** A program running as the user can no longer change the code that
  Frame Server loads. The registered copy is as trustworthy as the portable folder was
  when the user approved the prompt; after that, only an administrator can change it.
- **New state outside the folder.** The camera leaves a folder under `%ProgramFiles%`
  besides the `HKLM` keys, and only after the user opts in. Unregister removes both. A
  user who deletes the portable folder without unregistering leaves them behind, as they
  would leave the keys under ADR-0020.
- **Updates can cost a prompt.** A new release whose DLL changed asks again, but only
  for users who registered the camera, and only when they accept the offer.
- **Moved folders.** Detecting a moved folder becomes detecting an outdated copy:
  `vcam_registration` compares content, not paths.
- **Code to write** (#561): make the step opt-in only, copy the DLL with the DACL inside
  the elevated step, point `InprocServer32` at the copy, compare by content, delete the
  copy on unregister, and check on hardware that Frame Server loads the copy.
