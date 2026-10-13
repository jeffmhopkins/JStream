<!-- @doc
id = "ADR-0004"
title = "wgpu compositor with native interop"
kind = "adr"
status = "accepted"
-->

# ADR-0004: wgpu compositor with native interop

<a id="context"></a>
## Context
Capture → composite → encode must avoid CPU copies at 1080p60 and above, across three OSes.

<a id="decision"></a>
## Decision
`jstream-render` uses [wgpu](https://github.com/gfx-rs/wgpu) (D3D12 / Metal / Vulkan) with
WGSL shaders. Platform crates use wgpu-hal escape hatches to import shared capture
textures and export NV12 textures to encoders, synchronized with shared fences. The
Windows sync protocol is specified in [ARCH › GPU pipeline](../ARCHITECTURE.md#gpu-pipeline)
and must be proven by spike S1 in M0 before M1 depends on it. wgpu is pinned exactly.

<a id="consequences"></a>
## Consequences
- One shader language and portable effects.
- Interop is platform-specific `unsafe` code that needs testing on real hardware.
  Upgrading wgpu means re-running the interop spike tests.
- WGC textures can't be shared, so capture costs one GPU copy per frame.
