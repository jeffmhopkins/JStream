<!-- @doc
id = "ARCH"
title = "Architecture"
kind = "normative"
status = "draft"
review_every = "90d"
-->

# Architecture

This document is the map. The reasons behind each choice are in the ADRs under
[`adr/`](adr/). If this document and the code disagree, fix one of them in the same PR
([R-DOC-004](RULES.md#R-DOC-004)).

Status is `draft` until M0 creates the crates. Until then the `covers` globs below are
provisional, and globs that match nothing are only warnings.

<a id="shape"></a>
## Shape of the system
<!-- @section
covers = ["apps/**"]
depends_on = ["docs/adr/0001-library-first-engine.md", "docs/adr/0002-web-ui-everywhere.md"]
verified = { hash = "b3:d75fd5a7179de0f1", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Review round 1: CLI changes (mock behind an opt-in feature, JSON flag parsing, request IDs on action output, README) keep the jstream-cli row true: a remote client through Control only, no engine link; jstream mcp still an open question" }
-->

```
   clients — depend only on jstream-control + jstream-protocol (R-ARCH-003)
   ┌──────────────────────────────────────────────────────────────────────────────┐
   │ jstream-ui (Leptos→WASM)   jstream-mcp (MCP tools)   jstream-cli   3rd party │
   └───────────────┬──────────────────────────────────────────────────────────────┘
                   │  Control trait: in-process (InProcessControl), or WebSocket + token
                   │  (JSON messages + binary frames: snapshots, preview, meters)
                   ▼
┌───────────────────────────── jstream-core :: Engine ─────────────────────────────┐
│ command bus → state (rev N) → events      per-client undo · audit log · persist │
│                                                                                  │
│ VIDEO  sources → render (wgpu: transform/crop/effects) → video mixes ─┐          │
│ AUDIO  sources → resample → audio (sends → buses → filters) → mixes  ─┤          │
│                                                                      ▼          │
│              shared encoder sessions (encode) → packet streams → outputs        │
│              outputs: RTMP(S) [generic | YouTube-bound] · record · virtual cam  │
└──────────────────────────────────────────────────────────────────────────────────┘
                   ▲ platform traits (capture, audio I/O, encode, decode, vcam sink,
                   │ clock, GPU interop); concrete backends injected by core
     ┌─────────────┴──────────────┬────────────────────────┬──────────────────────┐
     │ jstream-platform-windows   │ jstream-platform-macos │ jstream-platform-linux│
     └─────────────┬──────────────┴────────────────────────┴──────────────────────┘
                   │ cross-process frames (shared texture + fence)
     ┌─────────────┴────────────────────────────────────┐
     │ jstream-vcam-windows (COM media source DLL,      │  loaded by the Windows
     │ registered by the app on first use)              │  Frame Server service
     └──────────────────────────────────────────────────┘
```

| Host (`apps/`) | What it is |
|---|---|
| `jstream-desktop` | Tauri shell. The engine runs in-process. The webview UI connects to it over the same authenticated localhost WebSocket used remotely, so there is one code path. |
| `jstream-daemon` | Headless engine. Serves the UI, the WebSocket API and MCP over HTTP. Can start from a collection file (`--collection`). |
| `jstream-cli` | Lightweight remote client: drives a running engine, and `jstream mcp` runs a **stdio** MCP proxy to it. Does not link the engine. |

<a id="principles"></a>
## Core principles
<!-- @section
depends_on = ["docs/adr/0001-library-first-engine.md", "docs/adr/0003-media-stack-and-codecs.md", "docs/adr/0013-command-model.md"]
verified = { hash = "b3:4c21fdca01d95b25", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Re-read: all 8 principles hold against ADR-0001/0003/0013 and the linked rules (R-ARCH-001/003, R-RT-001/002, R-UNSAFE-001, R-SEC-002 all exist)" }
-->

1. **One API.** Every capability is part of `jstream-protocol` ([R-ARCH-001](RULES.md#R-ARCH-001)).
2. **Library first, thin hosts.** Interfaces (server, MCP, CLI, UI) never link the engine.
   The apps wire them together ([R-ARCH-003](RULES.md#R-ARCH-003)).
3. **Outputs are decoupled from scenes.** Outputs consume *video mixes* and *audio buses*.
4. **Edits are separate from actions.** State edits are undoable and batchable; actions are
   neither ([ADR-0013](adr/0013-command-model.md)).
5. **Real-time paths are protected** ([R-RT-001](RULES.md#R-RT-001), [R-RT-002](RULES.md#R-RT-002)).
6. **Platform code is quarantined.** `unsafe` code lives only in the crates that declare it
   ([R-UNSAFE-001](RULES.md#R-UNSAFE-001)).
7. **Headless and deterministic.** The whole engine runs with a software GPU adapter, a fake
   clock and fake devices.
8. **Authenticated by default.** Every control listener requires a token and checks
   `Origin` ([R-SEC-002](RULES.md#R-SEC-002)).

<a id="repo-layout"></a>
## Repository layout
<!-- @section
covers = ["xtask/src/main.rs"]
verified = { hash = "b3:fa786f1fcb4dcfc6", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Removed installer/ line (ADR-0020: no MSI); tree otherwise matches repo and xtask command set" }
-->

```
JStream/
├── Cargo.toml              # workspace: members, shared deps, [workspace.lints]
├── rust-toolchain.toml  deny.toml  rustfmt.toml  clippy.toml  .editorconfig
├── CLAUDE.md               # builder-agent entry point; points to RULES/DOCS
├── REVIEW.md               # reviewer-agent prompt; its verdict drives review-gate
├── ROADMAP.md
├── crates/                 # all library crates, flat, every name prefixed `jstream-`
├── apps/                   # host binaries (thin)
├── effects/                # built-in WGSL effect packs (data)
├── assets/                 # icons, fonts, test media
├── tests/                  # cross-crate scenario + golden tests
├── spikes/                 # M0 technical spikes (throwaway, not workspace members)
├── xtask/                  # docs, lint-deps, governance, gen-schema, golden, release
├── docs/
│   ├── ARCHITECTURE.md  RULES.md  DOCS.md
│   ├── external.toml       # external resources we rely on (URL + what we rely on)
│   ├── facts.toml          # pinned facts, one source each
│   ├── adr/                # NNNN-title.md (+ TEMPLATE.md)
│   └── guides/
└── .github/                # workflows (CI, governance, review-gate), Dependabot config, PR template
```

<a id="crate-map"></a>
## Crate map
<!-- @section
covers = ["crates/*/Cargo.toml", "apps/*/Cargo.toml"]
depends_on = ["docs/RULES.md#R-ARCH-002", "docs/RULES.md#R-ARCH-003"]
verified = { hash = "b3:d8c08c8f007f5fa2", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Review round 1: apps/jstream-cli/Cargo.toml gains an opt-in mock feature and a jstream-control dev-dependency; both are jstream-control (L2, strictly below L6), still no jstream-core; layer 6, summary and unsafe_allowed unchanged; table unchanged" }
-->

A crate may depend only on crates in **strictly lower** layers ([R-ARCH-002](RULES.md#R-ARCH-002)).
Once the `xtask docs` generator lands (planned for M0), this table is generated from
`[package.metadata.jstream]` ([DOCS › Generated content](DOCS.md#generated)). Until then
it is maintained by hand. `xtask` is repository tooling and sits outside the layers.

The client stack follows from the rule: `jstream-control` builds on `jstream-protocol`, so
it sits one layer above it (L2). `jstream-mcp` is written against `dyn Control` (L3), and
`jstream-server` mounts MCP over HTTP (L4).

| L | Crate | Responsibility | unsafe |
|---|---|---|---|
| 0 | `jstream-types` | IDs, math (Vec/Mat/Transform3D), color, rational time, units | no |
| 0 | `jstream-media` | Frame, packet and audio-buffer types, pixel formats, timestamps | no |
| 0 | `jstream-vcam-proto` | Shared-memory layout and versioned contract between the engine and the vcam DLL (`repr(C)` types and constants only) | no |
| 1 | `jstream-protocol` | **The API.** Commands (edit/action), queries, events, binary streams, errors, JSON Schema | no |
| 1 | `jstream-platform` | Backend traits: `Capture`, `AudioIo`, `VideoEncoder`, `AudioEncoder`, `VideoDecoder`, `VcamSink`, `MediaClock`, `GpuInterop` (uses `wgpu` types). The **single owner** of the encoder and decoder traits. | no |
| 2 | `jstream-control` | The `Control` trait plus the WebSocket client. WASM-safe: no engine, no tokio requirement in WASM builds | no |
| 2 | `jstream-graph` | Scene graph model, edit application and inverse edits (for undo), persistence and migrations. No GPU. | no |
| 2 | `jstream-render` | Owns the wgpu device; compositor, 3D transform/crop, WGSL effects, color conversion, snapshot readback | no |
| 2 | `jstream-audio` | Real-time mixer: adaptive resampling, send matrix, buses, filters, metering | no |
| 2 | `jstream-mux` | Pure-Rust muxers: FLV (with enhanced RTMP), MP4/fMP4 | no |
| 2 | `jstream-youtube` | OAuth plus the YouTube Live API client (broadcasts, streams, health, chat). No media code. | no |
| 2 | `jstream-platform-windows` | WGC capture, WASAPI (incl. process loopback), MF encode/decode, MF hardware encoders, D3D11↔D3D12 interop, vcam sink side, CEF browser backend (ADR-0021) | **yes** |
| 2 | `jstream-platform-macos` | ScreenCaptureKit, CoreAudio, VideoToolbox/AudioToolbox, IOSurface↔Metal (M3) | **yes** |
| 2 | `jstream-platform-linux` | PipeWire, VAAPI, dmabuf↔Vulkan (M3; codec policy per future ADR) | **yes** |
| 2 | `jstream-vcam-windows` | `cdylib` COM media source loaded by Frame Server ([ADR-0020](adr/0020-portable-windows-distribution.md)). Depends only on L0 and `windows`. | **yes** |
| 3 | `jstream-encode` | Shared encoder sessions keyed by (mix, settings); rate control; fan-out of packet streams | no |
| 3 | `jstream-mcp` | MCP server over `dyn Control`: generated grouped tools, stdio and streamable-HTTP transports | no |
| 3 | `jstream-plugin-wasm` | wasmtime host for automation plugins (calls the API through `Control`) | no |
| 4 | `jstream-server` | axum HTTP/WebSocket over `dyn Control`; serves UI assets; mounts MCP over HTTP; token and Origin checks | no |
| 4 | `jstream-sources` | `Source` trait plus built-ins (display, window, webcam, image, color, text, browser, media file, scene). Uses platform traits; backends are injected. | no |
| 4 | `jstream-outputs` | `Output` trait; RTMP(S) client with reconnect, recording, vcam output. Uses `encode`, `mux` and platform traits. | no |
| 5 | `jstream-core` | `Engine`: executes commands, owns threads and the clock, injects platform backends, implements `InProcessControl` | no |
| — | `jstream-ui` | Leptos app compiled to WASM; depends on `jstream-protocol` and `jstream-control` only | no |
| 6 | `apps/*` | `jstream-desktop` and `jstream-daemon` (link core and server); `jstream-cli` (control and mcp only) | no |

Splitting rule: start with these crates. Split one only when compile time, `unsafe`
isolation or a dependency boundary requires it, and record the split in an ADR
([R-ARCH-005](RULES.md#R-ARCH-005)).

<a id="data-model"></a>
## Data model
<!-- @section
covers = ["crates/jstream-graph/src/model/**"]
depends_on = ["docs/adr/0007-bus-matrix-audio.md"]
verified = { hash = "b3:92b0462b389879d4", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Re-read merged section: shared-source/duplicate, scenes-as-sources (merged with main's Nesting bullet, no duplicate), filters vs effects, and main's Built-so-far graph paragraph all consistent" }
-->

```
Collection (JSON on disk, schema_version, rev: u64)
├── sources        Source { id, kind, settings, filters[] }   video and/or audio; independent of scenes
├── scenes         Scene { id, name, items[] }
│                  SceneItem { id, source_id, transform: Transform3D, crop, visible, blend, effects[] }
├── studio         { program_scene, preview_scene, transition }   (studio-mode state)
├── video_mixes    VideoMix { id, input: Scene(id) | Program | Preview, resolution, fps_divisor }
├── audio          sends: (source_id, bus_id) → gain_db
│                  Bus { id, name, filters[], monitor_device? }
├── encoders       EncoderProfile { id, codec, rate_control, … }
└── outputs        Output { id, kind, video_mix, audio_buses: [bus_id], encoder_profile }
                   kind: Rtmp { target: Generic { url } | YouTube { broadcast_binding } } | Record { path, container } | VirtualCam
```

- `Program` and `Preview` are mixes driven by the studio state. `Program` shows
  `program_scene`, including any transition in progress. A `VideoMix` with input
  `Program` follows it.
- Each source is **one** instance (one capture, one decoder), however many scenes use it.
  Adding an *existing* source to a scene adds a `SceneItem` that references it. Changing
  the source or its `filters[]` changes it everywhere. **Duplicate** creates a new
  `Source` with its own id.
- **Scenes as sources:** a `Source` of kind `Scene(scene_id)` renders that scene, so
  scenes nest. A scene never contains itself at any depth, so the graph rejects the edit
  that would close a cycle.
- **Filters and effects:** `Source.filters[]` belong to the source and apply wherever it
  is used. `SceneItem.effects[]` and `crop` belong to that one placement.
- The preview shown in the UI is a protocol stream, not an `Output`.
- **Encoder sharing:** outputs with the same `(video_mix, encoder_profile)` share one
  encoder session. Hardware session caps differ by GPU vendor, so the engine reports the
  count.
- **Color:** compositing is done in linear light in an FP16 working format. Output is
  BT.709 limited range by default. HDR (FP16) capture is tone-mapped to SDR. HDR output is later.
- **Undo** primitives (an edit plus its inverse) live in `jstream-graph`. The per-client
  undo stacks live in `jstream-core`.
- **Built so far** (`jstream-graph`): sources (`kind`: a builtin kind name or a nested
  scene; no settings or filters yet), scenes in display order, and items in stacking
  order with `source_id`, a 2D transform, `crop` and `visible`. Items refer to sources;
  they never copy them. Deleting a source an item still places, or a scene a source
  still shows, is rejected. Not yet: settings, filters, effects, blend, the 3D
  transform, and everything outside `sources` and `scenes`. Nothing is persisted yet.

<a id="threading"></a>
## Threads, clock and sync
<!-- @section
covers = ["crates/jstream-core/src/runtime/**", "crates/jstream-audio/src/rt/**", "crates/jstream-render/src/frame_loop.rs"]
depends_on = ["docs/adr/0014-media-clock.md"]
verified = { hash = "b3:c12d1d1e17c62c75", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Re-read: media clock and resampling match ADR-0014; thread table rules match R-RT-001/002/003 and R-UNSAFE-005 as written" }
-->

**Clock** ([ADR-0014](adr/0014-media-clock.md)): one monotonic **media clock** derived from
the OS high-resolution counter (QPC on Windows). Every device timestamp (WGC, MF,
WASAPI) is mapped into it. No device is the master, so unplugging a device never
breaks the timeline. Each audio device is resampled adaptively to the engine sample
rate, with drift estimated from its timestamps. Video has one **base frame rate**, and
every mix runs at the base rate divided by an integer.

| Thread | Runs | Rules |
|---|---|---|
| Control (tokio) | Commands, server, MCP, YouTube API, network outputs | May allocate and await. Talks to RT threads only through lock-free channels. |
| Audio (one per device) | Device I/O, resampling into the engine's ring | [R-RT-001](RULES.md#R-RT-001). Registered with MMCSS "Pro Audio". |
| Mixer | Sends, buses, filters, meters on the engine clock | [R-RT-001](RULES.md#R-RT-001) |
| Render | Composites every mix on each base-rate tick | [R-RT-002](RULES.md#R-RT-002). A missing source frame repeats the last one. |
| Encode (per session) | Encodes frames and samples, fans packets out to outputs | [R-RT-002](RULES.md#R-RT-002). Bounded queues; drops are counted. |
| Capture (per source) | Platform callbacks into a single-slot "latest frame" mailbox | [R-UNSAFE-005](RULES.md#R-UNSAFE-005) |
| Collector | Frees objects whose last reference was released by RT threads | [R-RT-003](RULES.md#R-RT-003) |

**State to RT threads:** each subsystem (render, audio, outputs) gets its own immutable
snapshot through `arc-swap`, rebuilt only when that subsystem's state changes.
Continuous parameters (fader moves, transform drags) bypass snapshots and go through
lock-free parameter channels. GPU resources are owned by `render`, keyed by ID, created
when a snapshot references them and destroyed after the frame fence that last used them.

<a id="protocol"></a>
## Control protocol
<!-- @section
covers = ["crates/jstream-protocol/**", "crates/jstream-control/**", "crates/jstream-server/**"]
depends_on = ["docs/adr/0013-command-model.md", "docs/adr/0006-mcp-full-access.md"]
verified = { hash = "b3:db7b4bb2a05a41d5", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Merged main (#27): section text is main's (control client and mock), unchanged by this PR; re-flagged only by the crate-map rows this PR edits (browser built-in, CEF backend)" }
-->

The wire format, versioning, error codes and compatibility policy are designed in
[jstream-protocol › Design](../crates/jstream-protocol/README.md#design). In short:

- **Envelope:** JSON messages tagged by `type`. A `hello`/`welcome` handshake exchanges
  the protocol version, then each `request` (with a client-chosen `id`) gets exactly one
  `response` or `error`, and `event`s arrive at any time.
- **Edits** (`scene.create`, `item.set_transform`, `audio.set_send`, …): typed, undoable,
  always sent in an atomic batch (of one or more), which carries the optional
  `expected_rev` and idempotency key ([R-API-004](RULES.md#R-API-004)). Each applied
  batch increments `rev` by one.
- **Actions** (`output.start`, `studio.transition`, `youtube.go_live`, …): side effects,
  never undoable or batchable. A *script* runs actions in sequence and returns a result
  for each step.
- **Queries** (`state.get` → state + `rev`, `scene.list`, `video.snapshot`, `audio.levels`).
- **Events** (`state.changed{rev}`, `output.state_changed`, `stats.tick`, …).
- **Errors** carry a stable machine-readable `code`, detail fields and a message.
- **Binary streams** (WebSocket binary frames): snapshots (PNG/JPEG), preview video,
  meters. Not designed yet.
- **Schema:** the JSON Schema is generated from the Rust types, and its snapshot is
  the API review ([R-TEST-002](RULES.md#R-TEST-002)).
- **Transport:** the `Control` trait is implemented in-process by core and over WebSocket
  by `jstream-control` (`/api/v1/ws`, token required, `Origin`/`Host` validated).
- **Versioning:** [R-API-001](RULES.md#R-API-001).

<a id="mcp"></a>
## MCP
<!-- @section
covers = ["crates/jstream-mcp/**"]
depends_on = ["docs/adr/0006-mcp-full-access.md", "docs/RULES.md#R-API-002"]
verified = { hash = "b3:ff3144be4aced675", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Re-read: grouped tools, R-API-002 name pattern, snapshot cap, token + Origin (MCP transports spec link registered) match ADR-0006 and ADR-0013 per-client undo" }
-->

- **Tools are grouped by noun**, not one per command (`scene`, `item`, `audio`, `output`,
  `studio`, `youtube`, `state`, plus `batch` and `script`). Each takes an `op` argument whose
  schema is generated from the protocol. `describe_api` returns detailed schemas on demand
  (progressive discovery), so the agent's context stays small.
- **Names** follow the tool-name pattern in [R-API-002](RULES.md#R-API-002).
- **Snapshots** are returned as images, capped in size (default longest side 1280 px), with
  the state `rev` they show.
- **Access:** full access ([ADR-0006](adr/0006-mcp-full-access.md)), always authenticated.
  The stdio proxy (`jstream mcp`) reads the token from the user-only token file. The
  HTTP transport validates `Origin`, as the
  [MCP transport spec](https://modelcontextprotocol.io/specification/2025-06-18/basic/transports)
  requires.
- Every agent action goes to the audit log with the client's name. Undo is per client,
  so an agent can never undo the user's work.

<a id="gpu-pipeline"></a>
## GPU pipeline
<!-- @section
covers = ["crates/jstream-render/**", "crates/jstream-platform-windows/src/gpu/**", "effects/**"]
depends_on = ["docs/adr/0004-wgpu-with-native-interop.md"]
verified = { hash = "b3:d885d82c122deac5", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Re-read: WGC -> shared texture + fence -> wgpu D3D12 -> NV12 -> MF path matches ADR-0004; still gated on spike S1, no render code yet" }
-->

The Windows path is validated by **spike S1 in M0** before M1 builds on it:

1. **Capture:** WGC delivers D3D11 textures from its frame pool. They can't be shared, so
   capture copies each frame into a **shared NT-handle texture** that JStream allocates
   (one GPU copy).
2. **Import:** the shared texture is opened in D3D12 and wrapped into wgpu through
   wgpu-hal (`texture_from_raw` → `create_texture_from_hal`, both `unsafe`, so they live in
   `jstream-platform-windows`). Sync uses a **shared fence**: D3D11 signals and the wgpu
   queue waits, then the reverse for release. D3D12 has no keyed mutex, so fences are
   the only sync mechanism.
3. **Composite** in FP16 linear light (WGSL).
4. **Convert** to NV12/P010 in compute shaders, writing R8 + RG8 plane targets, then copied
   into an NV12 resource through hal (or a D3D11 video processor if S1 shows that's simpler).
5. **Encode:** the NV12 texture is shared back to D3D11 for the Media Foundation encoders
   (fence-synchronized).
6. **Fallbacks:** CPU readback/upload, counted in stats with a stated budget. On
   hybrid-GPU laptops, capture and render must use the same adapter, or the engine falls
   back and reports it.

wgpu is pinned exactly, because hal interop can change between releases. Effects are WGSL
plus an `effect.toml` manifest.

<a id="persistence"></a>
## Persistence, config and secrets
<!-- @section
covers = ["crates/jstream-graph/src/persist/**", "crates/jstream-core/src/settings/**"]
depends_on = ["docs/RULES.md#R-API-003", "docs/RULES.md#R-SEC-001"]
verified = { hash = "b3:233514b2be0b9df3", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Re-read: collections+migrations, jstream.toml, OS credential store, token file and local minidumps match R-API-003, R-SEC-001 and R-SEC-004" }
-->

- **Collections:** JSON with a `schema_version` field and forward migrations in
  `jstream-graph`.
- **App settings:** `jstream.toml` in the platform config directory.
- **Secrets** (stream keys, OAuth tokens) are kept in the OS credential store.
- **Control token:** a random token in a user-only file in the config directory
  (Windows ACL restricted to the user). It is rotated on demand.
- **Logs and crash dumps:** rotating logs and local minidumps in the data directory
  ([R-SEC-004](RULES.md#R-SEC-004)).

<a id="packaging"></a>
## Packaging
<!-- @section
depends_on = ["docs/adr/0020-portable-windows-distribution.md"]
verified = { hash = "b3:bb081c51d8417c30", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Portable zip contents now include the CEF runtime folder for browser sources (ADR-0021, CEF); state location and vcam registration text unchanged and still match ADR-0020" }
-->

Windows ships as a **portable zip**: unzip and run `jstream.exe`, with no installer and no
admin ([ADR-0020](adr/0020-portable-windows-distribution.md)). The folder also holds
`jstream-daemon.exe`, `jstream-cli.exe`, the `jstream-vcam-windows` DLL, and the CEF
runtime for browser sources ([ADR-0021](adr/0021-browser-source.md)), in its own folder. Release builds
link the C runtime statically and need only OS components (WebView2, Media Foundation,
Direct3D 12).

- **State:** in `%APPDATA%\JStream`, or beside the exe when a `portable` marker file is
  present.
- **Virtual camera:** the app registers the COM media source from its own folder the first
  time the camera is enabled. That is silent if per-user registration works with Frame
  Server, and otherwise one UAC prompt. Spike S2 decides which, and its result is recorded
  here. The app can unregister the source and detects a moved folder.
