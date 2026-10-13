---
id: ROADMAP
title: Roadmap
kind: plan
status: active
rev: 2
review_every: 30d
depends_on: [ADR-0010, ADR-0011]
verified: {date: 2026-10-13, by: "agent:claude", rev: 2, note: "M0 reordered: docs tooling first"}
---

# Roadmap

Windows first. Engine and MCP before GUI ([[ADR-0010]]). Each milestone ends with a
demo that runs from a clean checkout.

## M0: Foundation
<!-- @section
id: m0
rev: 2
-->
- **First:** `xtask docs` (check, sync, verify, for, gen, sweep) per [[DOCS]] and
  [[ADR-0011]], then re-validate all hand-written doc metadata with it
- Cargo workspace, toolchain pin, lints, `cargo-deny`, CI on Windows/Linux/macOS
  (required checks per [[RULES#R-GIT-006]]), weekly docs sweep job
- `xtask`: `lint-deps`, `gen-schema`, `golden`; generated crate map and `docs/INDEX.md`
- `jstream-types`, `jstream-media`, first cut of `jstream-protocol` (+ schema snapshot tests)
- **Demo:** CI green, including `docs`; a PR that touches covered code without verifying
  its docs is rejected; `cargo xtask gen-schema` prints the API schema

## M1: Headless engine driven by agents (Windows)
<!-- @section
id: m1
rev: 1
-->
- **M1a Compositor:** `jstream-graph` scenes/items/sources; `jstream-render` wgpu compositor
  with 2D transform + crop; color/image/text sources; display capture (WGC) and webcam (MF).
  Golden-frame tests.
- **M1b Control:** `jstream-core` Engine, `jstream-client`, `jstream-server` (WS),
  `jstream-mcp` (stdio + HTTP), `jstream-cli`, `video.snapshot` so agents can see the output.
  Undo/redo, audit log, batches.
- **M1c Virtual camera:** MF virtual camera output fed from any video mix.
- **M1d Audio + stream:** WASAPI capture + process loopback, bus-matrix mixer, metering;
  NVENC/AMF/QSV H.264 with zero-copy input; FLV mux; RTMPS out to YouTube via stream key; recording to fragmented MP4.
- **Demo:** an agent builds a two-scene setup over MCP, checks it with snapshots, starts the
  virtual cam, then goes live on YouTube, all with no GUI.

## M2: UI
<!-- @section
id: m2
rev: 1
-->
- `jstream-ui` (Leptos→WASM): scenes, sources, properties, transform gizmo, mixer, outputs
- Low-latency preview stream into `<canvas>`
- `jstream-desktop` (Tauri, engine in-process) and `jstream-daemon` serving the same UI
- Studio mode (Preview/Program) + transitions
- **Demo:** do everything from M1 by hand in the desktop app and in a browser

## M3: Depth and reach
<!-- @section
id: m3
rev: 1
-->
- Full 3D transforms (perspective, rotation about X/Y), effect packs (WGSL + manifest, hot reload)
- `jstream-youtube`: OAuth, create/schedule broadcasts, title/description, health, chat → MCP tools
- HEVC/AV1 via enhanced RTMP
- WASM automation plugins
- macOS (minimum macOS 13 [[fact:min-macos]], provisional) and Linux platform crates reach M1 parity

## Later
<!-- @section
id: later
rev: 1
-->
- Media file source, browser source (needs an ADR: embedded webview vs CEF-free approach)
- Replay buffer, multi-output (simultaneous stream + record at different settings)
- SRT / WHIP outputs, OBS scene-collection import

## Revision history

<!-- @generated begin history by="xtask docs sync" -->
| rev | date | by | sections | note |
|---|---|---|---|---|
| 1 | 2026-10-13 | agent:claude | all | Initial version |
| 2 | 2026-10-13 | agent:claude | m0, m3 | Docs tooling first in M0; facts cited |
<!-- @generated end history -->
