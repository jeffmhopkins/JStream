# 0004: wgpu compositor with native interop

- Status: Accepted
- Date: 2026-10-13

## Context
Capture → composite → encode must be zero-copy at 1080p60/4K60, on three OSes.

## Decision
`jstream-render` uses wgpu (D3D12 / Metal / Vulkan) with WGSL shaders. Platform crates
use wgpu-hal escape hatches to import capture surfaces and export textures to
hardware encoders without CPU copies. A CPU path exists only as a fallback.

## Consequences
- One shader language, portable effects.
- Interop code is platform-specific `unsafe` and must be well tested on real hardware.
- We track wgpu releases; hal APIs may change between versions.
