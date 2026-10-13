---
id: ARCH
title: Architecture
kind: normative
status: draft
rev: 2
review_every: 90d
depends_on: [ADR-0001, ADR-0002, ADR-0003, ADR-0004, ADR-0005, ADR-0006, ADR-0007, ADR-0008, ADR-0009, ADR-0010, ADR-0011]
verified: {date: 2026-10-13, by: "agent:claude", rev: 2, note: "Converted to tracked sections; covers paths are provisional until M0 creates the crates"}
---

# Architecture

This document is the map. The *why* behind each choice lives in the ADRs ([[ADR-0001]] onward).
If code and this document disagree, fix one of them in the same PR ([[RULES#R-DOC-013]]).

`covers` paths are provisional until M0 creates the crates. While this doc is `status: draft`, unmatched globs are warnings.

## Shape of the system
<!-- @section
id: shape
rev: 1
covers: [crates/jstream-core/src/lib.rs, apps/**]
depends_on: [ADR-0001, ADR-0002]
verified: {date: 2026-10-13, by: "agent:claude", rev: 1, note: "Initial version"}
-->

```
                ┌───────────────────────── clients (speak only jstream-protocol) ──────────────────────────┐
                │  jstream-ui (Leptos→WASM)     jstream-mcp (MCP tools)     jstream-cli     3rd-party     │
                └───────────────┬──────────────────────┬─────────────────────┬────────────────────────────┘
                                │      Control trait (in-process)  or  WebSocket (remote)
                                ▼
┌──────────────────────────────────────────── jstream-core :: Engine ──────────────────────────────────────────────┐
│  command bus ─► state (scene graph, mixer, outputs) ─► events                    undo/redo · audit log · persist │
│                                                                                                                  │
│  VIDEO   sources ─► jstream-render (wgpu compositor: transform/crop/effects) ─► video mixes ─► encoders ─┐       │
│  AUDIO   sources ─► jstream-audio (send matrix ─► buses ─► filters)          ─► audio mixes ─► encoders ─┤       │
│                                                                                                          ▼       │
│                                                         outputs: RTMPS/YouTube · recording · virtual cam · preview│
└──────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
                                ▲ platform traits (capture, audio devices, HW encoders, vcam, GPU interop)
                ┌───────────────┴──────────────────────────────────────────────┐
                │ jstream-platform-windows │ jstream-platform-macos │ -linux   │   ← the only place FFI/unsafe lives
                └──────────────────────────────────────────────────────────────┘
```

**Hosts** embed the engine:

| Host | What it is |
|---|---|
| `apps/jstream-desktop` | Tauri shell. Engine runs **in-process**, UI is `jstream-ui` in the webview. |
| `apps/jstream-daemon` | Headless engine. Serves `jstream-ui` + WebSocket API over HTTP, optional MCP over HTTP. |
| `apps/jstream-cli` | `jstream` command: drive a running engine, run headless from a scene file, or run `jstream mcp` as a **stdio** MCP server that connects to a running engine. |

## Core principles
<!-- @section
id: principles
rev: 1
depends_on: [ADR-0001, ADR-0003, ADR-0007]
-->

1. **One API.** Every capability is a command in `jstream-protocol`. The UI, MCP,
   CLI and tests all use it. There are no UI-only features and no private back doors.
2. **Library first.** `jstream-core` is a library. Hosts are thin.
3. **Outputs are decoupled from scenes.** A *video mix* (e.g. Program, Preview, or
   any scene) and an *audio mix* (a bus) are what outputs consume. Any mix can feed
   any output.
4. **Real-time paths are sacred.** Audio, render and encode threads never block on
   the control plane, never allocate in steady state, and never take contended locks.
5. **Platform code is quarantined.** OS APIs and `unsafe` live only in
   `jstream-platform-*` crates behind traits defined in `jstream-platform`.
6. **Deterministic and testable headless.** The whole engine runs without a window,
   with a software GPU adapter, a fake clock and fake devices.

## Repository layout
<!-- @section
id: repo-layout
rev: 2
covers: [Cargo.toml, xtask/**, .github/workflows/**]
depends_on: [DOCS]
verified: {date: 2026-10-13, by: "agent:claude", rev: 2, note: "Added docs registries and generated INDEX to layout"}
-->

```
JStream/
├── Cargo.toml              # workspace: members, shared deps, lints
├── rust-toolchain.toml     # pinned stable toolchain
├── deny.toml               # license/advisory/duplicate policy (cargo-deny)
├── rustfmt.toml  clippy.toml  .editorconfig
├── CLAUDE.md               # rules for AI agents
├── ROADMAP.md
├── crates/                 # all library crates, flat, every name prefixed `jstream-`
├── apps/                   # binaries (thin hosts)
├── effects/                # built-in WGSL effect packs (data, not code)
├── assets/                 # icons, test media, fonts
├── tests/                  # cross-crate integration + golden-frame tests
├── xtask/                  # cargo xtask: docs, lint-deps, gen-schema, golden, release
├── docs/
│   ├── ARCHITECTURE.md     # id ARCH
│   ├── RULES.md            # id RULES: every rule has a permanent R-<AREA>-NNN id
│   ├── DOCS.md             # id DOCS: how docs are tracked, referenced, verified
│   ├── INDEX.md            # generated: every doc/section/rule id → link, rev, verified
│   ├── external.toml       # registry of external URLs we rely on
│   ├── facts.toml          # pinned facts (min OS, toolchain, …), one source each
│   ├── adr/                # NNNN-title.md, immutable once accepted
│   └── guides/             # how-tos: adding a source, an effect, a command
└── .github/workflows/      # CI (incl. `docs` check and weekly docs sweep)
```

## Crate map
<!-- @section
id: crate-map
rev: 1
covers: [Cargo.toml, crates/*/Cargo.toml, apps/*/Cargo.toml]
depends_on: [RULES#R-ARCH-002]
verified: {date: 2026-10-13, by: "agent:claude", rev: 1, note: "Hand-written until M0; then generated from package.metadata.jstream"}
-->

Layers: a crate may depend only on crates in **strictly lower** layers ([[RULES#R-ARCH-002]]).
`cargo xtask lint-deps` enforces this in CI. From M0 this table is generated from
`[package.metadata.jstream]` ([[DOCS#generated]]); until then it is hand-maintained.

| L | Crate | Responsibility |
|---|---|---|
| 0 | `jstream-types` | IDs, math (Vec/Mat/Transform3D), color, rational time, units. No heavy deps. |
| 0 | `jstream-media` | Frame/packet/audio-buffer types, pixel formats, clocks, A/V sync primitives. |
| 1 | `jstream-protocol` | **The API.** Commands, queries, events, errors (serde + JSON Schema). Versioned. |
| 1 | `jstream-platform` | Traits: `CaptureBackend`, `AudioDeviceBackend`, `HwEncoder`, `VirtualCamera`, `GpuInterop`. |
| 2 | `jstream-graph` | Scene graph model: scenes, items, sources registry, transforms, groups, undo/redo, persistence + migrations. Pure logic, no GPU. |
| 2 | `jstream-render` | wgpu device ownership, compositor, 3D transform/crop, WGSL effect runtime, readback for snapshots. |
| 2 | `jstream-audio` | Real-time mixer: send matrix, buses, filters (gain, gate, compressor, EQ, limiter), metering. |
| 2 | `jstream-encode` | Encoder abstraction, settings, rate control, encoder selection/fallback. |
| 2 | `jstream-mux` | Pure-Rust muxers: FLV (incl. enhanced RTMP), MP4 / fragmented MP4. |
| 3 | `jstream-platform-windows` | WGC capture, WASAPI (+ process loopback), MF virtual camera, NVENC/AMF/QSV/MF encoders, D3D11↔D3D12 interop. |
| 3 | `jstream-platform-macos` | ScreenCaptureKit, CoreAudio, VideoToolbox, IOSurface↔Metal, CoreMediaIO camera extension. |
| 3 | `jstream-platform-linux` | PipeWire (screen + audio), V4L2, VAAPI, dmabuf↔Vulkan, v4l2loopback. |
| 3 | `jstream-sources` | Built-in sources: display, window, webcam, image, color, text, media file, scene-as-source. One module per source. |
| 3 | `jstream-outputs` | RTMP/RTMPS client, recording, virtual camera output, preview stream. |
| 3 | `jstream-youtube` | OAuth device/loopback flow, Live API (broadcasts, streams, health, chat). |
| 3 | `jstream-plugin-wasm` | wasmtime host for sandboxed automation/logic plugins. |
| 4 | `jstream-core` | `Engine`: wires everything, executes commands, emits events, owns threads. |
| 5 | `jstream-client` | `Control` trait + `InProcess` and `Remote` (WebSocket) implementations. |
| 5 | `jstream-server` | axum HTTP/WebSocket server, serves `jstream-ui` assets, auth token. |
| 5 | `jstream-mcp` | MCP server; tools **generated from** `jstream-protocol`; uses `Control`, so it works in-process or remote. |
| — | `jstream-ui` | Leptos app compiled to WASM. Depends **only** on `jstream-protocol` (+ a WS client). Never on core. |
| 6 | `apps/*` | `jstream-desktop`, `jstream-daemon`, `jstream-cli`. |

Splitting rule: start with the crates above. Split a crate only when compile time,
`unsafe` isolation or a dependency boundary demands it, and record that in an ADR ([[RULES#R-ARCH-005]]).

## Data model
<!-- @section
id: data-model
rev: 1
covers: [crates/jstream-graph/**]
depends_on: [ADR-0007]
verified: {date: 2026-10-13, by: "agent:claude", rev: 1, note: "Initial version"}
-->

```
Collection (a saved setup, JSON on disk, schema-versioned)
├── sources        Source { id, kind, settings, filters[] }      — video and/or audio, exist independently of scenes
├── scenes         Scene  { id, name, items[] }
│                  SceneItem { id, source_id, transform: Transform3D, crop, visible, blend, effects[] }
├── video_mixes    VideoMix { id, input: Scene(id) | Program | Preview, resolution, fps }
├── audio          sends: (source_id, bus_id) → gain_db
│                  Bus { id, name, filters[], monitor: bool }
└── outputs        Output { id, kind: Rtmp|Youtube|Record|VirtualCam, video_mix, audio_tracks: [bus_id], encoder }
```

- `Program` and `Preview` are two special video mixes (studio mode). Transitions act on Program.
- A source used in several scenes is **one** instance (one capture, one decoder).
- Effects are per scene item (video) or per source/bus (audio filters).

## Threads and timing
<!-- @section
id: threading
rev: 1
covers: [crates/jstream-core/src/runtime/**, crates/jstream-audio/src/rt/**, crates/jstream-render/src/frame_loop.rs]
depends_on: [RULES#R-RT-001, RULES#R-RT-002]
verified: {date: 2026-10-13, by: "agent:claude", rev: 1, note: "Initial version"}
-->

| Thread | Runs | Rules |
|---|---|---|
| Control (tokio) | Command handling, server, MCP, YouTube API, network outputs | May allocate/await. Talks to RT threads only via lock-free queues and triple buffers. |
| Render | Composites every video mix once per frame tick | Fixed-rate clock. Missing source frame → reuse last. Never waits on control. |
| Audio | Mixer, one per audio clock (device-driven or virtual) | No alloc, no locks, no syscalls besides the device API. `assert_no_alloc` in debug builds. |
| Encode (per output) | Pulls frames and samples, encodes, hands packets to muxers/network | Bounded queues; drops are counted and surfaced as events. |
| Capture (per source) | Platform callbacks | Publishes into a single-slot "latest frame" mailbox. |

Master clock: the audio clock drives A/V sync. Video frames are timestamped against it.
State changes reach RT threads as immutable snapshots (`arc-swap`), so RT threads never see half-applied commands.

## Control protocol
<!-- @section
id: protocol
rev: 1
covers: [crates/jstream-protocol/**, crates/jstream-client/**, crates/jstream-server/**]
depends_on: [ADR-0001, RULES#R-API-001]
verified: {date: 2026-10-13, by: "agent:claude", rev: 1, note: "Initial version"}
-->

- **Commands** (`scene.create`, `item.set_transform`, `audio.set_send`, `output.start`, …):
  typed request/response, each with a JSON Schema.
- **Queries** (`state.get`, `scene.list`, `video.snapshot` → PNG/JPEG, `audio.levels`).
- **Events** (`scene.switched`, `output.state_changed`, `stats.tick`, `audio.levels`), pushed to subscribers.
- **Batches**: a list of commands applied atomically with a single undo step (agents rely on this).
- Transport: in-process `Control` trait, or JSON over WebSocket (`/api/v1/ws`).
- Versioning: [[RULES#R-API-001]].

## MCP
<!-- @section
id: mcp
rev: 1
covers: [crates/jstream-mcp/**]
depends_on: [ADR-0006, ext:mcp-spec]
verified: {date: 2026-10-13, by: "agent:claude", rev: 1, note: "Initial version"}
-->

`jstream-mcp` builds its tool list from the protocol schema at build time: one tool
per command/query, with a curated description per tool, plus a few agent-friendly
composite tools (`describe_setup`, `snapshot`, `apply_batch`). Agents have full access
([[ADR-0006]]). Two defaults stay on regardless: the endpoint binds to localhost only, and
every agent action goes to the audit log with its MCP client name.

Transports: stdio (`jstream mcp`, for desktop agent apps) and streamable HTTP (served by the daemon/desktop).

## GPU pipeline
<!-- @section
id: gpu-pipeline
rev: 1
covers: [crates/jstream-render/**, crates/jstream-platform-*/src/gpu/**, effects/**]
depends_on: [ADR-0004, ADR-0005]
verified: {date: 2026-10-13, by: "agent:claude", rev: 1, note: "Initial version"}
-->

- `jstream-render` owns one `wgpu::Device` (D3D12 on Windows, Metal on macOS, Vulkan on Linux).
- Capture textures are imported zero-copy through `GpuInterop` (shared handles / IOSurface / dmabuf).
  If import fails, the fallback is a CPU upload path, which is logged and surfaced in stats.
- Compositing works in linear light, in a 16-bit float working format; output conversion to NV12/P010 runs in a compute shader.
- Encoded frames are handed to the HW encoder as GPU textures via interop; CPU readback is a fallback only.
- Effects are WGSL with a manifest (`effect.toml`: parameters, types, ranges, defaults). Built-ins live in `effects/`, and user effects hot-reload from the user data dir.

## Persistence and config
<!-- @section
id: persistence
rev: 1
covers: [crates/jstream-graph/src/persist/**, crates/jstream-core/src/settings/**]
depends_on: [RULES#R-API-003, RULES#R-SEC-001]
verified: {date: 2026-10-13, by: "agent:claude", rev: 1, note: "Initial version"}
-->

- Collections: JSON, `schema_version` field, forward migrations in `jstream-graph::migrate`.
- App settings: `jstream.toml` in the platform config dir.
- Secrets (stream keys, OAuth tokens): OS credential store (Windows Credential Manager / Keychain / Secret Service), never in collection files or logs.

## Revision history

<!-- @generated begin history by="xtask docs sync" -->
| rev | date | by | sections | note |
|---|---|---|---|---|
| 1 | 2026-10-13 | agent:claude | all | Initial version |
| 2 | 2026-10-13 | agent:claude | all | Tracked sections with ids/covers; unnumbered headings; docs registries in layout |
<!-- @generated end history -->
