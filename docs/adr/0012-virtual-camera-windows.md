+++
id = "ADR-0012"
title = "Windows virtual camera as a COM media source"
kind = "adr"
status = "accepted"
+++

# ADR-0012: Windows virtual camera as a COM media source

<a id="context"></a>
## Context
`MFCreateVirtualCamera` takes the CLSID of a **custom media source**
([docs](https://learn.microsoft.com/en-us/windows/win32/api/mfvirtualcamera/nf-mfvirtualcamera-mfcreatevirtualcamera)).
That source is a COM object which the Windows camera Frame Server loads, not JStream. A
reference implementation ([VCamSample](https://github.com/smourier/VCamSample)) registers it
machine-wide and feeds it frames from another process. The original ADR-0008 claimed no
DLL was needed, and that was wrong.

<a id="decision"></a>
## Decision
- `jstream-vcam-windows` is a `cdylib` implementing the media source in Rust (`windows`
  crate, `unsafe` allowed). It depends only on L0 crates.
- Frames cross processes through a shared NT-handle texture plus a shared fence, with a
  shared-memory control block (`jstream-vcam-proto`, versioned) carrying format,
  timestamps and liveness. The security descriptors must allow the Frame Server's service
  account.
- The engine side (`VcamSink` in `jstream-platform-windows`) creates the virtual camera
  for the session and publishes frames from any video mix.
- The MSI installer registers the COM server machine-wide (admin), and the build is signed.
- **Spike S2 in M0** proves the whole path before M1 commits to it, including whether
  MSIX can register the source.

<a id="consequences"></a>
## Consequences
- Adds an installer, code signing and a second `unsafe` crate. This is the price of an
  OS-native camera that every app sees.
- The DLL runs inside a system service, so a crash there takes down the camera for every
  app. It must not panic across FFI ([R-UNSAFE-005](../RULES.md#R-UNSAFE-005)), and it
  must handle the engine disappearing.
