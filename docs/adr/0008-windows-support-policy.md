+++
id = "ADR-0008"
title = "Windows support policy"
kind = "adr"
status = "accepted"
+++

# ADR-0008: Windows support policy

<a id="context"></a>
## Context
Windows 10 Home/Pro reached end of support on 2025-10-14
([lifecycle](https://learn.microsoft.com/en-us/lifecycle/products/windows-10-home-and-pro)).
The OS virtual camera API `MFCreateVirtualCamera` requires Windows build 22000
([docs](https://learn.microsoft.com/en-us/windows/win32/api/mfvirtualcamera/nf-mfvirtualcamera-mfcreatevirtualcamera)).
Windows 11 feature versions leave consumer support on a yearly cadence
([lifecycle](https://learn.microsoft.com/en-us/lifecycle/products/windows-11-home-and-pro)),
so any fixed version floor goes stale within a year.

<a id="decision"></a>
## Decision
- **Runs on:** Windows 11, build 22000 or later (the API floor). Older Windows 11 builds are
  not blocked.
- **Supported and tested:** the Windows 11 versions Microsoft currently services. That set
  is a fact (`win11-serviced`) that the docs sweep re-checks against the lifecycle page.
- Capture uses [Windows.Graphics.Capture](https://learn.microsoft.com/en-us/windows/uwp/audio-video-camera/screen-capture).
  Audio uses WASAPI, including
  [process loopback](https://learn.microsoft.com/en-us/windows/win32/api/audioclientactivationparams/ns-audioclientactivationparams-audioclient_process_loopback_params).
- The virtual camera design is in [ADR-0012](0012-virtual-camera-windows.md).

<a id="consequences"></a>
## Consequences
- The policy text never needs editing. Only the fact changes.
- Windows 10 is not supported.
