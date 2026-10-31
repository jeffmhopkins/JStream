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
covers = ["apps/*/Cargo.toml#jstream-meta", "apps/*/src/main.rs"]
depends_on = ["docs/adr/0001-library-first-engine.md", "docs/adr/0045-leptos-web-ui.md"]
-->

```
   clients — depend only on jstream-control + jstream-protocol (R-ARCH-003)
   ┌──────────────────────────────────────────────────────────────────────────────┐
   │ jstream-ui (Leptos→WASM)   jstream-mcp (MCP tools)   jstream-cli   3rd party │
   └───────────────┬──────────────────────────────────────────────────────────────┘
                   │  Control trait: in-process (InProcessControl), or WebSocket + token
                   │  (JSON messages, including video.snapshot's PNG; binary frames: preview)
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
     │ an admin-only copy, registered only on opt-in)   │  Frame Server service
     └──────────────────────────────────────────────────┘
```

| Host (`apps/`) | What it is |
|---|---|
| `jstream-desktop` | Tauri shell. The engine runs in-process. The webview UI connects to it over the same authenticated localhost WebSocket used remotely, so there is one code path. The first cut runs on Windows (WebView2), with a fresh in-memory token handed to the page at startup ([its README](../apps/jstream-desktop/README.md#design)). Its `cef` feature, off by default until the CEF runtime is packaged, opens browser sources' pages in CEF. |
| `jstream-daemon` | Headless engine. Serves the UI, the WebSocket API and MCP over HTTP. Can start from a collection file (`--collection`). The first cut serves the WebSocket API, MCP over streamable HTTP and the UI's built files (`--ui-dir`), and loads and saves `--collection`, and on Windows links `jstream-platform-windows` (and on macOS `jstream-platform-macos`, on the backends built so far) to run the media pipeline and go live over RTMP(S), on Windows registers (`--register-vcam`) and publishes (a `virtual_cam` output) the virtual camera, built with its `cef` feature, opens browser sources through CEF, and offers YouTube sign-in with JStream's own OAuth client, unavailable until that client is registered ([ADR-0058](adr/0058-youtube-sign-in-with-a-jstream-client.md), [its README](../apps/jstream-daemon/README.md#purpose)). |
| `jstream-browser-helper` | Not a host: the sandboxed browser helper, CEF's bootstrap exe renamed plus this crate's DLL, which a host's CEF backend starts and drives over pipes. Hosts that turn on browser sources ship it next to their own exe ([ADR-0048](adr/0048-sandboxed-browser-helper.md), [its README](../apps/jstream-browser-helper/README.md#purpose)). |
| `jstream-cli` | Lightweight remote client: drives a running engine, and `jstream-cli mcp` runs a **stdio** MCP proxy to it. Does not link the engine. |
| `jstream-plugin-host` | Not a host of the engine: the out-of-process CLAP plugin host, one process per CLAP filter instance, started by the engine ([ADR-0035](adr/0035-clap-plugins-out-of-process.md)). It talks to the engine only through a shared-memory block and framed messages on its stdin and stdout, never the protocol ([its README](../apps/jstream-plugin-host/README.md#purpose)). |

<a id="principles"></a>
## Core principles
<!-- @section
depends_on = ["docs/adr/0001-library-first-engine.md", "docs/adr/0061-source-types-and-decoding-scope-v2.md", "docs/adr/0013-command-model.md"]
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
   `Host`, and `Origin` when one is present ([R-SEC-002](RULES.md#R-SEC-002)).

<a id="repo-layout"></a>
## Repository layout
<!-- @section
covers = ["xtask/src/main.rs"]
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
├── design/                 # UI design references (prototype source); not normative
├── xtask/                  # docs, lint-deps, governance, dist (the portable zip), gen-schema, golden
├── docs/
│   ├── ARCHITECTURE.md  RULES.md  DOCS.md
│   ├── external.toml       # external resources we rely on (URL + what we rely on)
│   ├── facts.toml          # pinned facts, one source each
│   ├── adr/                # NNNN-title.md (+ TEMPLATE.md, generated README.md index)
│   └── guides/
└── .github/                # workflows (CI, governance, review-gate, sweep), their tests, Dependabot config,
                            # issue and PR templates
```

<a id="crate-map"></a>
## Crate map
<!-- @section
covers = ["crates/*/Cargo.toml#jstream-meta", "apps/*/Cargo.toml#jstream-meta"]
depends_on = ["docs/RULES.md#R-ARCH-002", "docs/RULES.md#R-ARCH-003"]
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
| 0 | `jstream-aac` | JStream's AAC-LC encoder, Linux's AAC path ([ADR-0061](adr/0061-source-types-and-decoding-scope-v2.md), within [ADR-0062](adr/0062-aac-lc-encoder-patent-check.md)'s limits): MPEG-2 AAC LC tools only, encode only; `f32` samples in, raw access units and the `AudioSpecificConfig` out. `jstream-platform-linux` puts it behind `AudioEncoder` | no |
| 0 | `jstream-anim` | Keyframe-scene model ([ADR-0049](adr/0049-keyframe-scene-model-and-evaluation.md)): a variant's overrides and their inverses, effective values over the live parent, and the switch plan between keyframes; generic over geometry, no dependencies | no |
| 0 | `jstream-plugin-proto` | Shared-memory contract between the mixer and the out-of-process CLAP plugin host ([ADR-0035](adr/0035-clap-plugins-out-of-process.md)): `repr(C)` audio rings, parameter-event rings and a status block, all atomics; no OS calls | no |
| 0 | `jstream-vcam-proto` | Shared-memory layout and versioned contract between the engine and the vcam DLL: `repr(C)` types, constants and pure helpers (frame choice, object names); no OS calls | no |
| 1 | `jstream-protocol` | **The API.** Commands (edit/action), queries, events, binary streams, errors, JSON Schema | no |
| 1 | `jstream-platform` | Backend traits: `Capture`, `VideoDevices`, `AudioIo`, `Codecs` (`VideoEncoder`, `AudioEncoder`, `VideoDecoder`), `VcamSink`, `MediaDecoders` (media files), `MediaClock`, `GpuInterop` (uses `wgpu` types), `StreamKeyStore`, `OAuthTokenStore`, `PluginHosts` (CLAP host processes, ADR-0035), and `paths` (the data folder, ADR-0066). The **single owner** of the encoder and decoder traits. | no |
| 2 | `jstream-control` | The `Control` trait plus the WebSocket client. WASM-safe: no engine, no tokio requirement in WASM builds | no |
| 2 | `jstream-graph` | Scene graph model, edit application and inverse edits (for undo), persistence and migrations. No GPU. | no |
| 2 | `jstream-render` | Owns the wgpu device; compositor, 3D transform/crop, WGSL effects, color conversion, snapshot readback | no |
| 2 | `jstream-audio` | Real-time mixer: adaptive resampling, send matrix, buses, filters, metering | no |
| 2 | `jstream-mux` | Pure-Rust muxers: FLV (with enhanced RTMP), MP4/fMP4 | no |
| 2 | `jstream-crash` | Local crash reports for the apps ([ADR-0067](adr/0067-shared-crash-reports.md)): the panic hook, redaction of registered secrets by BLAKE3 digest, the report format and pruning, and `Redacting`, which registers each stream key and refresh token passing through a credential store. The secrets live in a `Secrets` handle the app holds, not in global state | no |
| 2 | `jstream-youtube` | OAuth plus the YouTube Live API client (broadcasts, streams, health, chat reading and sending as the channel, audience stats; [ADR-0058](adr/0058-youtube-sign-in-with-a-jstream-client.md)). No media code. | no |
| 2 | `jstream-platform-windows` | WGC capture, MF webcam and capture-card capture (with MJPEG decode), WASAPI (incl. process loopback), MF encode/decode, MF hardware encoders, D3D11↔D3D12 interop, Credential Manager store for stream keys, vcam sink side, CLAP plugin-host launcher (job object, scrubbed environment) and shared block | **yes** |
| 2 | `jstream-platform-macos` | ScreenCaptureKit, Core Audio (incl. process taps), AVFoundation webcams, VideoToolbox/AudioToolbox, AVFoundation media files, IOSurface↔Metal, Keychain store for stream keys and OAuth tokens (M3; [ADR-0063](adr/0063-macos-platform-support.md); Apple frameworks through the objc2 crates) | **yes** |
| 2 | `jstream-platform-linux` | PipeWire, with ALSA as the audio fallback, V4L2 webcams and capture cards paired with their microphones, VAAPI, dmabuf↔Vulkan, the OpenH264 loader, AAC encoding through `jstream-aac`, secrets in the Secret Service with a `0600` file fallback (M3; [ADR-0060](adr/0060-linux-platform-support.md), [ADR-0061](adr/0061-source-types-and-decoding-scope-v2.md), [ADR-0071](adr/0071-linux-secret-store.md); a crate loading its system libraries at run time) | **yes** |
| 2 | `jstream-plugin-clap` | Safe wrapper over the CLAP plugin ABI ([ADR-0035](adr/0035-clap-plugins-out-of-process.md)): loading a plugin file, its stereo audio effects, parameters, latency and saved state, and real-time processing. Only `jstream-plugin-host` links it | **yes** |
| 2 | `jstream-vcam-windows` | `cdylib` COM media source loaded by Frame Server ([ADR-0066](adr/0066-portable-windows-distribution-v2.md)). Depends only on L0 and `windows`, so it logs through its own sink (the debugger and a capped file in the host account's profile), not `tracing`: no subscriber exists inside Frame Server. | **yes** |
| 3 | `jstream-encode` | Encoder sessions: outputs with no video chain share one per (mix, encoder profile), and an output with a video chain gets its own ([ADR-0080](adr/0080-program-master-and-output-chains-v2.md)); rate control; fan-out of packet streams | no |
| 3 | `jstream-effects` | WGSL effect packs ([ADR-0035](adr/0035-clap-plugins-out-of-process.md)): `effect.toml` manifests, WGSL validation with naga, parameters as `<pack>/<effect>` kind descriptors with a uniform layout, hot reload by polling; draws an effect through `jstream-render`'s filter harness. No GPU device of its own | no |
| 3 | `jstream-mcp` | MCP server over `dyn Control`: generated grouped tools and the stdio transport; its streamable-HTTP transport is served by `jstream-server` | no |
| 3 | `jstream-browser` | Browser sources' engine ([ADR-0021](adr/0021-browser-source.md), [ADR-0046](adr/0046-browser-crate-and-helper.md), [ADR-0048](adr/0048-sandboxed-browser-helper.md)): the `BrowserBackend` seam, pages' audio hand-off, custom CSS and request policy; the CEF backend (Windows, feature `cef`), which drives the sandboxed browser helper over pipes and copies its shared-texture frames onto the engine's device; and the CEF code the helper runs (feature `helper`) | **yes** |
| 3 | `jstream-plugin-wasm` | wasmtime host for automation plugins ([ADR-0035](adr/0035-clap-plugins-out-of-process.md)): calls the API through `Control`, limited to a per-plugin list of granted commands (empty by default) or to what that `Control` allows, with refusals as ABI statuses or `forbidden`; fuel, wall-time and memory limits; no WASI | no |
| 4 | `jstream-server` | axum HTTP/WebSocket over `dyn Control`; token and Origin checks; serves UI assets; serves MCP's streamable-HTTP transport on the control port behind the same checks | no |
| 4 | `jstream-sources` | `Source` trait plus built-ins ([ADR-0061](adr/0061-source-types-and-decoding-scope-v2.md)): display, window, video capture device, image, color, text, browser, audio input, desktop audio, application audio, scene (M1b); media file, audio waveform, audio spectrum (M2); YouTube chat message ([ADR-0058](adr/0058-youtube-sign-in-with-a-jstream-client.md)) (M3); Spout2 receiver (later). Uses platform traits; backends are injected. Images and fonts use pure-Rust crates. | no |
| 4 | `jstream-outputs` | RTMP(S) publish client with reconnect, and the recording file sink. Uses `mux`. Planned: the `Output` trait, and use of `encode` and platform traits. The virtual-camera output runs in `jstream-core`'s runtime. | no |
| 5 | `jstream-core` | `Engine`: executes commands, owns threads and the clock, injects platform backends, implements `InProcessControl`; CLAP filter kinds from plugin scans and the host supervisor (ADR-0035); runs WASM automation plugins on `jstream-plugin-wasm`'s host, behind the default `wasm-plugins` feature ([ADR-0075](adr/0075-wasm-plugin-host-in-the-engine.md)) | no |
| 3 | `jstream-ui` | Leptos app compiled to WASM ([ADR-0045](adr/0045-leptos-web-ui.md)); depends on `jstream-protocol` and `jstream-control` only | no |
| 6 | `jstream-ui-e2e` | Test-only harness at `crates/jstream-ui/tests/e2e/mock-daemon`: `MockEngine` behind `jstream-server`, serving the built UI for the Playwright tests; never shipped | no |
| 6 | `apps/*` | `jstream-desktop` and `jstream-daemon` (link core and server, and the platform crate of the OS they run on; `jstream-desktop` also links sources, and `jstream-browser` with its `cef` feature; `jstream-daemon` always links `jstream-browser` to give the engine a browser backend, CEF's with its `cef` feature); `jstream-cli` (control and mcp only; test-only dev-dependencies, such as server for its end-to-end test, are outside this); `jstream-plugin-host`, the out-of-process CLAP host (plugin-clap, which only it may link, plugin-proto, and the Windows platform crate for its shared block; [ADR-0035](adr/0035-clap-plugins-out-of-process.md)); `jstream-browser-helper`, the sandboxed browser helper's DLL ([ADR-0048](adr/0048-sandboxed-browser-helper.md); it declares `unsafe` for its one export) | no |

Splitting rule: start with these crates. Split one only when compile time, `unsafe`
isolation or a dependency boundary requires it, and record the split in an ADR
([R-ARCH-005](RULES.md#R-ARCH-005)).

<a id="data-model"></a>
## Data model
<!-- @section
covers = ["crates/jstream-graph/src/model/**"]
depends_on = ["docs/adr/0022-item-transform-and-canvas.md", "docs/adr/0023-placements-references-and-clones.md", "docs/adr/0024-scene-structure.md", "docs/adr/0025-typed-settings-kind-descriptors.md", "docs/adr/0026-source-filter-chains.md", "docs/adr/0080-program-master-and-output-chains-v2.md", "docs/adr/0028-audio-model-v2.md", "docs/adr/0030-studio-state-and-scene-switching.md", "docs/adr/0037-kind-descriptors-for-transitions-and-plugins.md", "docs/adr/0040-scene-structure-overrides-shared-groups-and-hotkey-targets.md", "docs/adr/0064-nested-scene-folders.md"]
-->

```
Collection (JSON on disk, schema_version, rev: u64, canvas: { width, height })
├── sources        Source { id, kind, settings, filters { video[], audio[] }, duplicated_from?, labels, favourite, audio? }   video and/or audio
│                  Filter { filter_id, kind, enabled, settings }   unique within its source; at most 8 per chain
│                  SourceAudio { enabled, follow: Always | WhenShown, gain_db, muted }
├── scenes         Scene { id, name, role: scene | group, variant_of?, overrides, folder?, background?,
│                          labels, icon, color, favourite, show_in_multiview, transition_in?, transition_out?,
│                          filters { video[] }, items[] }   flat display order
│                  SceneItem { id, source_id, geometry: ItemGeometry, crop, visible, opacity, blend,
│                              effects[] (video filters), transition }
├── folders        Folder { id, name, parent? }   ordered; a tree; organising only
├── labels         Label { id, name, icon?, color }   ordered
├── hotkeys        [{ chord, action }]   chord → protocol action request
├── transitions    { presets: [TransitionPreset], default }
│                  TransitionPreset { id, name, kind, duration_ms, easing, settings }   kind: cut | fade | move | …
├── video_mixes    VideoMix { id, input: Scene(id) | Program | Preview, resolution, fps_divisor }
├── audio          master_bus: bus_id   (created with the collection, never deleted)
│                  sends: (source_id, bus_id) → gain_db   (post-fader)
│                  Bus { id, name, filters[], gain_db, muted, monitor? }
│                  monitor { device, tap: PreFilters | PostFilters }
│                  an audio filter may name a key source (sidechain)
├── encoders       EncoderProfile { id, codec, rate_control, … }
├── master         Master { filters { video[], audio[] } }   applies to everything leaving JStream
└── outputs        Output { id, kind, video_mix, audio_buses: [bus_id], encoder_profile, filters { video[], audio[] } }
                   kind: Rtmp { target: Generic { url } | YouTube { broadcast_id } } | Record { path, container } | VirtualCam
```

- `Program` and `Preview` are mixes driven by the studio state: `program_scene` and
  `preview_scene`, which are runtime state beside the outputs' state, not part of the
  collection or its `rev` ([ADR-0030](adr/0030-studio-state-and-scene-switching.md)).
  `Program` shows `program_scene`, including any transition in progress. A `VideoMix`
  with input `Program` follows it. `Preview` shows `preview_scene`, studio mode's next
  scene, set by `studio.set_preview`; a switch doesn't move it. The collection's first
  scene goes on program and on preview when it is created, and a deleted program or
  preview scene gives way to the first scene left.
- Each source is **one** instance (one capture, one decoder), however many scenes use it.
  Adding an *existing* source to a scene adds a `SceneItem` that is a **reference**: it
  shares everything with the other placements of that source except its own placement,
  opacity and blend. Changing the source or its filters changes it everywhere.
  **Duplicate** creates a new `Source` with its own id and `duplicated_from` set to the
  original; an item placing it is a **clone** and changes independently. Sources have no
  owner: every placement of a shared source is equal, and a client shows how often a
  source is placed rather than marking one placement as the original.
  `item.set_source` switches an item between reference and clone and keeps its id,
  placement and stacking position ([ADR-0023](adr/0023-placements-references-and-clones.md)).
- **Canvas and geometry** ([ADR-0022](adr/0022-item-transform-and-canvas.md)): the
  collection's `canvas` is every scene's coordinate space, in pixels, and a `VideoMix`
  scales it to its `resolution`. An item's `geometry` is its box: the centre and size in
  canvas pixels, a fit mode for the source inside the box, a rotation about the centre,
  horizontal and vertical flips that mirror the content inside the box, and 3D tilt,
  turn, depth and per-item perspective, drawn in stacking order with no depth buffer. `crop` is in source pixels. A source's current size is runtime state,
  not part of the collection. Until 1.0 an item may instead hold a **legacy placement**,
  protocol 0.3's transform as written, which the compositor evaluates against the
  source's current size every frame. Only an explicit `item.set_geometry` turns it into
  a geometry.
- **Scenes as sources:** a `Source` of kind `Scene(scene_id)` renders that scene, so
  scenes nest. A scene never contains itself at any depth, so the graph rejects any edit
  that would close a cycle (`item.add`, and `item.set_source`). The graph accepts any
  acyclic depth, but the program mix (the live output and `video.snapshot` alike) draws
  at most **8 levels** of scene sources below the program scene: a scene source deeper
  than that draws nothing, and the engine logs it at `debug`. A scene with a scene
  filter is drawn once per frame as if it were the program scene, so the count restarts
  below it ([ADR-0065](adr/0065-scene-chains-and-item-effects.md)), as it does below a
  scene a `scene_mask` reads or an item with effects shows.
- **Groups and keyframe scenes** ([ADR-0024](adr/0024-scene-structure.md)):
  - A **group** is a scene with `role = group`. The role changes no engine behaviour: a
    group renders, nests and takes filters like any scene, and clients hide groups from
    the scene list. A group shares its parent's coordinate space, so its children keep
    their coordinates and a client can edit them inline. `item.move` moves an item
    to another scene and keeps its id; grouping and ungrouping are client-built batches
    of `scene.create`, `source.create`, `item.add` and `item.move`.
  - A **keyframe scene** (variant) is a real scene with `variant_of` naming its parent,
    which is not itself a variant. It has no items of its own: it shows the parent's
    items with per-item `overrides` of placement, visibility and opacity (an absent
    field follows the parent); everything else, including the background, follows the
    parent. A variant can be on program, nested and hotkeyed. The cycle check counts the
    variant → parent edge. Deleting a parent is rejected while it has variants.
  - Placement edits take an optional `scene_id`: absent or the item's own scene changes
    the item; a variant writes that variant's override; `item.clear_override` removes one.
  - **Orphaned overrides are dropped, never refused.** An edit that leaves overrides
    naming items outside their variant's tree drops those overrides in the same edit, and
    its inverse restores them, as `item.move` and `item.remove` do. That covers
    `scene.set_role` on a group (its children leave the tree), removing the item that
    places a group or retargeting it with `item.set_source`, and `scene.set_role` on a
    variant.
  - **An override follows a group everywhere it is placed.** Overrides are keyed by item
    ID and a group is one shared scene, so a variant's override on a group's child
    applies to every placement of that group in the variant's tree, including two
    placements in the same parent. To vary one copy, duplicate the group.
- **Folders and organisation are collection state:** folders, labels (which sources and
  scenes carry, [ADR-0078](adr/0078-scenes-carry-labels.md)), favourites, scene
  icons, colours and `show_in_multiview`, and hotkey bindings are edited by undoable edits
  and saved with the collection, so every client and agent sees the same thing
  ([R-ARCH-001](RULES.md#R-ARCH-001); JStream is single-user, so a favourite is not per
  person). Folders change no rendering, switching or nesting; scenes keep their flat
  display order, and a variant sits under its parent, never in a folder. A folder sits in
  at most one other (`parent`, [ADR-0064](adr/0064-nested-scene-folders.md)), never inside itself, and
  the folders in one parent show in folder order. A folder that holds anything, or a
  label a source or scene carries, can't be deleted; `delete.plan` moves what the folder
  holds up to its parent, or takes the label off its sources and scenes. Labels and
  favourites on a source belong to the source, so every placement shares them. A hotkey
  binds only an action, never an edit; which client listens for the keys is a client
  decision. Deleting the scene, source, output or transition preset a binding's action
  names also removes
  that binding, in the same undoable edit, and undo restores both; `delete.plan` lists
  the bindings it will remove, and a binding is never a delete blocker
  ([ADR-0040](adr/0040-scene-structure-overrides-shared-groups-and-hotkey-targets.md)). `Scene.background` (absent
  means transparent) is scene model, not metadata, because the compositor draws it
  wherever the scene renders.
- **Filters and effects:** a source's two filter chains, `filters.video` and
  `filters.audio`, belong to the source and apply wherever it is used
  ([ADR-0026](adr/0026-source-filter-chains.md)). A source has a video chain if its kind
  produces video, and an audio chain if it produces audio and is not a `scene` source. A
  filter's kind is a `video_filter` or `audio_filter` kind matching its chain, and the
  `filter.*` edits address it by its owner and its ID, which is unique within the source
  and kept by `source.duplicate`. A disabled filter keeps its place and settings and is
  bypassed. The audio chain runs on the source's input before its fader. The master
  stage and each output own `filters { video, audio }` too, edited with the same
  `filter.*` edits under the owners `master` and `output`
  ([ADR-0080](adr/0080-program-master-and-output-chains-v2.md)); only their audio chains
  hold filters until video mixes run the video ones, and their filters can't hold a
  reference (a scene, source, bus or asset) yet. A placement owns its geometry, `crop`, `visible`, `opacity` (an alpha
  multiplier in [0, 1]) and `blend` (default `normal`; a mode is rejected until the
  compositor draws it). A scene's `filters` and a placement's `effects[]` are video filter
  chains, the owners `scene` and `item` of `filter.*`
  ([ADR-0065](adr/0065-scene-chains-and-item-effects.md); item effects are in v1 under
  [ADR-0068](adr/0068-item-effects-in-v1.md)). A reference in them is an edge of the
  scene, so it counts for cycles and blocks deleting its target. A keyframe scene has
  none of its own. The engine runs a scene's filters on its composite, and an item's
  effects over its box in a layer grown by their outset, before its opacity and blend.
- **Settings:** a source's `settings`, each filter's, and each transition preset's are a
  map of values described by its kind's descriptor, which the engine reports and
  validates against. References to scenes, sources, buses and asset files inside
  settings are typed, so the graph tracks them in cycle checks and delete rejection
  ([ADR-0025](adr/0025-typed-settings-kind-descriptors.md)). Every edit that can add a
  dependency edge runs the cycle check, including `item.set_source`, `item.move` and
  creating a variant. Kinds are built in, come from an effect pack, or, for audio
  filters from M3, come from a CLAP plugin (`clap/<plugin id>`): its descriptor is built
  from the plugin's parameters, and its saved state is a parameter of the opaque `state`
  type, bytes stored as base64 in the settings
  ([ADR-0037](adr/0037-kind-descriptors-for-transitions-and-plugins.md)).
- **Transitions** ([ADR-0029](adr/0029-transitions.md),
  [ADR-0037](adr/0037-kind-descriptors-for-transitions-and-plugins.md)): a preset's kind
  is a transition kind with a descriptor. `cut`, `fade` and `move` are the first, so a
  new kind (a stinger, a WGSL wipe) needs no protocol change. `duration_ms` and `easing`
  stay preset fields; `unmatched` and `stagger_ms` are settings of `fade` and `move`. A
  switch to a preset whose kind is missing cuts. Exactly one preset is the
  default, and it can't be deleted. Deleting a preset clears the overrides that use it in
  the same edit. The engine picks a switch's preset: the one the switch action names, else
  the destination scene's `transition_in`, else the current scene's `transition_out`, else
  the default. `SceneItem.transition { delay_ms, easing, glide, appear, disappear }`
  belongs to the placement, keyed by item ID. **Move** pairs outgoing and incoming items
  that place the same source (the k-th placement with the k-th, in stacking order) and
  interpolates each pair; **Fade** pairs nothing; **Cut** switches on the next tick.
  Unpaired items appear or disappear with their effect. The running transition is
  runtime state beside `program_scene`, never collection state.
- The preview shown in the UI is a protocol stream, not an `Output`.
- **Master and output chains**
  ([ADR-0080](adr/0080-program-master-and-output-chains-v2.md)): the `master` stage applies
  to every output, whichever mix and buses it takes, but not to the editing canvas or
  audio monitoring. Its video chain runs once per tick on each mix some output consumes;
  its audio chain runs on each bus that feeds an output, after the bus's own chain
  (monitors tap the bus before it). Each output may add its own chains after the master
  stage; `VirtualCam` has no audio, so it has only a video chain.
- **Encoder sharing** ([ADR-0080](adr/0080-program-master-and-output-chains-v2.md)):
  outputs whose video chain is empty share one encoder session when they have the same
  `(video_mix, encoder_profile)`, the master stage counting as part of the mix. An
  output with a video chain, even one holding only disabled filters, gets its own render
  pass and session, keyed by its output ID. An output's own audio chain gives it its own
  audio encode and never affects video sharing. Hardware session caps differ by GPU
  vendor, so the engine reports, per output, whether it shares and with which outputs
  and whether its session is its own, plus the sessions in use. It does not guess the
  caps. Outputs carry no video chain until filters land, so for now every output shares
  by `(video_mix, encoder_profile)`; the engine already keys and draws an output with a
  chain on its own.
- **No privacy screen** ([ADR-0080](adr/0080-program-master-and-output-chains-v2.md)
  §4): nothing covers the outputs or mutes the microphones on a switch. Muting a source
  and switching scenes do that job.
- **Audio** ([ADR-0028](adr/0028-audio-model-v2.md)): sources feed buses through the send
  matrix; buses feed outputs and monitors, never other buses. Every audio source gets a
  send to the master bus ("Program output") at 0 dB. Per source: stream → audio filters →
  fader and mute → activity gain → sends. Per bus: sum → filters → fader and mute.
  `audio.enabled = false` closes the source's audio stream and removes its mixer channel
  but keeps its sends; mute keeps it open and metered. A nested scene source has
  `enabled` and `follow` only; its contents keep their own channels. A filter's key is the
  key source after its fader, mute and activity gain. Sources and buses, with sends and
  keys as edges, form an audio graph: an edit that closes a cycle is rejected, as is a
  keyed filter on a bus its key sends to, a source keyed on itself, or deleting a source
  that is still a key.
- **Shown and on program** (D29): "shown" is relative to the Program mix only. Its roots
  are `program_scene`, or both scenes of a running transition, weighted by the
  transition's audio curve (weights sum to 1). A source is reachable from a scene through
  `visible` items, following nested scene sources whose `audio.enabled` is on, to any
  depth. A nested scene source set to `follow = always` is an extra root of weight 1. A
  `when_shown` source's activity gain is `min(1, Σ w(r))` over the roots that reach it;
  an `always` source's is 1. The activity gain is runtime state, never an edit. Preview,
  other video mixes, projectors and editor previews never make a source shown.
- **Color:** compositing is done in linear light in an FP16 working format. Output is
  BT.709 limited range by default. HDR (FP16) capture is tone-mapped to SDR. HDR output is later.
- **Undo** primitives (an edit plus its inverse) live in `jstream-graph`. The per-client
  undo stacks live in `jstream-core`.
- **Built so far** (`jstream-graph`): sources (`kind`: a builtin kind ID or a nested
  scene, with `settings` checked against the kind's descriptor and stored complete, and
  its video and audio filter chains, each filter's settings checked the same way),
  scenes in display order, and items in stacking order with `source_id`, a placement,
  `crop`, `visible`, `opacity` and `blend` (only `normal` is accepted), and its
  `effects[]`, a scene's `filters` beside them. The placement is a `geometry` or, until 1.0, a
  legacy placement (protocol 0.3's 2D transform as written), and the collection has
  its `canvas` (protocol 0.11); any 3D geometry the compositor can draw is accepted from
  protocol 0.34. Scenes have
  a `role` (`scene` or `group`, set by `scene.create` or `scene.set_role`), and `item.move` moves an item
  to another scene keeping its id, under the same cycle check. The placement edits take
  an optional `scene_id` (protocol 0.12): the item's own scene, or one of its keyframe
  scenes (protocol 0.39). Keyframe scenes (`scene.create_variant`) are built with their
  per-item overrides of the parent's own items, `item.clear_override`, the overrides
  `item.remove` and `item.move` drop (reported in the batch result and restored by
  undo), their parent's delete blocked by them and its `delete.plan` deleting them
  first, and the switch within a family paired by item (ADR-0049 §3). Items refer to sources; they never copy them, and
  `item.set_source` retargets one, keeping its id, placement and stacking position.
  A source copied by `source.duplicate` records its original in `duplicated_from`, a
  plain link that reads as absent once the original is deleted. Scene and source
  references in settings are dependency edges with items and nested scenes, so one
  cycle check covers them all. Deleting a source an item still places or a setting
  refers to, or a scene a source or a video mix still shows or a setting refers to, is
  rejected, and so is deleting a bus a setting refers to (a `bus` reference, which never
  closes a cycle); a reference in a filter's settings is an edge of the filter's
  source.
  built-in kinds come from `jstream-sources`. The engine runs a source of each one
  whose kind is built (`browser` and its `youtube_chat` preset, through the host's
  browser backend, and `color`, `display`, `image`, `media`, `text`, `waveform`,
  `spectrum` and `window`), rebuilt when its settings change, and the program mix shows
  its frames, animations and videos advancing on the media clock, or for `display` and
  `window` what the output pipeline captures for its `target`. The audio-only kinds (`audio_input`,
  `desktop_audio`, `app_audio`) have no `Source`: while an output runs, the engine
  records each as a mixer input, routed as its audio and sends say, running the
  source's enabled audio filters of the built-in kinds (`gain`, `noise_gate`,
  `compressor`, `limiter`, `delay`). A `browser` page's audio and a `media` file's
  sound are mixer inputs of their running source, routed the same way. A source's
  enabled video filters of `jstream-render`'s built-in kinds run in order over its
  frame or capture before every placement of it, in every mix (roadmap V0,
  [core › Design](../crates/jstream-core/README.md#design)); effect-pack kinds are
  bypassed there for now.
  Outputs, ordered by ID, with their `kind`, an optional `encoder_profile` and an
  optional `video_mix`: `Rtmp {
  target: Generic { url } }`, the URL checked to carry no credential, or `Record { path,
  container: mp4 }`, the path checked to be absolute and to end in a file name, or
  `VirtualCam`, which has no settings and ignores its profile. A stream
  key is never collection state: it lives in the OS credential store
  ([R-SEC-001](RULES.md#R-SEC-001)). Encoder profiles, ordered by ID, each a name and an
  H.264, HEVC or AV1 and AAC encoding (rate control, keyframe interval, hardware preference, audio bit
  rate), checked against the ranges the protocol states. An output with no profile uses
  the engine's default; a profile an output uses can't be deleted. Video mixes
  (protocol 0.28), ordered by ID, each with a name besides the fields above: the
  resolution's sides even and 16 to 8192, the divisor 1 to 60. An output with no mix
  shows `Program` at the canvas size and base rate; a mix an output shows can't be
  deleted. Outputs share a session by mix and profile. `Preview` shows `Program`: no
  mix draws studio mode's preview scene yet.
  Transition presets (`cut`, `fade` and `move`, built-in kinds with descriptors), the
  default preset, each scene's `transition_in` and `transition_out` and each item's
  `transition`: a new collection holds one preset, `cut`, as its default, so a switch
  cuts until a client adds another.
  Audio (protocol 0.19): an audio source's `audio` (`enabled`, `follow`, fader and
  mute; a nested scene source has no fader or mute), the master bus and up to 15
  others with their faders and (protocol 0.42) each bus's optional monitor, an output
  device and a tap point before or after its filters (`bus.set_monitor`), and the
  post-fader sends, a new channel source sending to the master at 0 dB. A send always
  runs from a source to a bus, and filters have no sidechain keys yet, so no edit can
  close an audio cycle. The engine routes these into the program stage's mixer while it
  runs, and plays each bus's monitor on its device (one it can't open stays silent and
  is retried).
  Folders (a tree), labels, scenes' and sources' organisation metadata (protocol 0.31),
  and hotkey bindings (protocol 0.25), as the organisation bullet above describes.
  Not yet: settings migrations between kind versions (a source saved at another
  version is refused on load), blend modes
  other than `normal`, a keyframe scene's overrides of a group's
  children (so `scene.set_role` and removing or retargeting a group's item drop none
  yet, #702), `background`, output audio buses and chains, the `YouTube` kind, a virtual
  camera's video mix (it always shows the program), and everything outside `sources`,
  `scenes`, `outputs`, `encoders`, `video_mixes`, `audio`, `folders`, `labels`,
  `hotkeys` and `transitions`. The collection file saves everything built so far,
  encoder profiles, video mixes, each output's profile and mix, `duplicated_from`,
  buses, sends, source audio, folders, labels, organisation metadata, hotkeys and
  transitions included
  ([Persistence](#persistence)).
  The program and preview scenes and the running transition are built in `jstream-core`
  as runtime state, so the file doesn't hold them.

<a id="threading"></a>
## Threads, clock and sync
<!-- @section
depends_on = ["docs/adr/0014-media-clock.md", "docs/adr/0080-program-master-and-output-chains-v2.md", "docs/adr/0028-audio-model-v2.md", "docs/adr/0032-live-edits-and-undo.md", "docs/adr/0035-clap-plugins-out-of-process.md"]
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
| Audio (one per stream) | Device I/O, resampling into the engine's ring | [R-RT-001](RULES.md#R-RT-001). Registered with MMCSS "Pro Audio". |
| Mixer | Activity gains, sends, buses, filters with keys (in audio-graph order), latency compensation, meters on the engine clock | [R-RT-001](RULES.md#R-RT-001) |
| Render | Composites every mix on each base-rate tick | [R-RT-002](RULES.md#R-RT-002). A missing source frame repeats the last one. |
| Encode (per session) | Encodes frames and samples, fans packets out to outputs | [R-RT-002](RULES.md#R-RT-002). Bounded queues; drops are counted. |
| Preview (one per engine, while any preview stream is open) | Draws the program mix, or the stream's scene, small for each `preview.subscribe` stream and encodes it (H.264 or JPEG) into the session's bounded frame queue | Not real-time: it has its own compositor, like `video.snapshot`'s, and loads the render snapshot, `rev` and live values under a brief collection lock, never held while drawing or encoding. A stream whose queue is full is not drawn. |
| Capture (per source) | Platform callbacks into a single-slot "latest frame" mailbox | [R-UNSAFE-005](RULES.md#R-UNSAFE-005) |
| Collector | Frees objects whose last reference was released by RT threads | [R-RT-003](RULES.md#R-RT-003). One for the program stage while any output runs, fed by its render and mixer threads (snapshots, live values, GPU images, finished sessions' inputs, a dropped mix's frame buffers), and the mixer's own for the input rings the mixer and audio threads release, and removed buses' state, filter chains and filters. |

This section is the engine-wide model. Each subsystem's runtime, as built, is described
next to its code, which that section covers: the output pipeline in
[`jstream-core`'s runtime](../crates/jstream-core/README.md#runtime) and the audio and
mixer threads in [`jstream-audio`'s real-time design](../crates/jstream-audio/README.md#realtime)
([ADR-0042](adr/0042-narrow-hot-architecture-sections.md)). Neither may contradict this
model; a change that would contradict it edits it in the same PR.

**CLAP plugin hosts** ([ADR-0035](adr/0035-clap-plugins-out-of-process.md)), M3, in progress: the shared block
(`jstream-plugin-proto`), the mixer's side of it (`jstream-audio`) and the host
process (`jstream-plugin-host`) are built, and so are starting hosts
(`jstream-platform-windows`) and watching and restarting them on a control thread
(`jstream-core`'s `HostSupervisor`), and a source's, the master's and each output's
audio chain runs CLAP filters, each with its own supervisor (on the master chain, the
plugin on the master bus and a delayed-dry stand-in on the other buses, #922); the
`stats.tick` count is not yet. Each CLAP filter
instance runs in its own `jstream-plugin-host` process, on a real-time thread registered
with MMCSS "Pro Audio" and woken by its own timer on the media clock, which both
processes read from the same OS counter (not built yet: for now the host sleeps a
quarter block between passes). Audio crosses through shared-memory rings, and
the mixer never waits on the host: it writes block *n* and reads the processed block
*n − d* (by default *d* is two engine blocks), with no syscall or lock for the plugin
([R-RT-001](RULES.md#R-RT-001)). A late block is bypassed with a short crossfade, by
the dry signal delayed the same amount, and counted for `stats.tick` in the engine's
own memory beside the block (`EngineBlock`), where the host can't reset it. The filter's latency is *d* plus the plugin's own, and latency
compensation keeps audio and video in sync. A host that dies is restarted by the control
thread with backoff while the mixer bypasses it.

**S3 result**, a 20-minute run (ROADMAP M0 asks for an hour) on the S1 machine, with an
Expert Sleepers ES-9 for both the mic ("Line") and the output ("Speakers"). Both WASAPI
streams were 48 kHz with a 10 ms period and MMCSS on, and a quiet test tone played
throughout:
- **Timestamps:** both streams map cleanly onto QPC. There were no glitches, gaps,
  discontinuities, timestamp errors or wait timeouts in 120,000 packets each.
- **Drift:** the ES-9 runs 59.2 to 59.3 ppm slow against QPC, so about 71 ms over the run.
  The fit has a standard error of 0.001 ppm, with mic jitter of 0.16 ms and loopback
  jitter of 0.06 ms.
- **Resampler:** the PI-steered resampler held each stream within 0.06 ms of the engine
  time line (rms 0.01 ms). It never had to re-anchor.
- **CPU:** about 1.6% of one core for both streams.

Still open: the mic and the output share the ES-9's crystal, so the relative drift was
only −0.12 ppm. Drift between two independent devices, and events rarer than about once
in 20 minutes, haven't been measured.

**In the engine:** `jstream-audio` runs S3's drift fit and PI loop on each input's audio
thread, resampling into a per-input ring that the mixer reads on the engine clock. Inputs
are added and removed through a lock-free command queue while the mixer runs, and a ring
released by the mixer or the audio thread is freed on the collector thread. How it
differs from what S3 measured is in
[its real-time design](../crates/jstream-audio/README.md#realtime).

**Audio timestamps** ([ADR-0028](adr/0028-audio-model-v2.md)): every audio filter reports
its latency in frames, and the mixer delays the shorter paths before each sum so all inputs
line up. Every output block is stamped with `pts` = the media time of the input it was
computed from, not the time it was produced, and muxers interleave by `pts` with a bounded
wait. Filter latency therefore never shifts A/V sync and video is never delayed to match
it. An audio delay filter is a deliberate shift: its output is stamped later by its delay,
and its ring is allocated at its maximum on the control thread.

**State to RT threads:** each subsystem (render, audio, outputs) gets its own immutable
snapshot through `arc-swap`, rebuilt only when that subsystem's state changes.
Continuous parameters (fader moves, transform drags) bypass snapshots and go through
lock-free parameter channels. On the wire they are live values, committed by one batch
at the end of the gesture ([ADR-0032](adr/0032-live-edits-and-undo.md)). GPU resources are owned by `render`, keyed by ID, created
when a snapshot references them and destroyed after the frame fence that last used them.

The **render snapshot** is built. After every applied batch, undo and redo, every stored
source frame and a collection file's load, the control side re-plans the program mix
and the scene each video mix shows under the collection lock: the reachable scenes, the
frames they show and the passes that draw them. It publishes that as the next render
generation only when it draws something new, so a batch that touches nothing on program
or any mix publishes nothing. The render
thread loads it without the lock. `video.snapshot` and the preview thread read it
together with `rev` and the live values under the lock, and draw the live values over
it. The render
thread draws through a `jstream-render` resource cache keyed by source and by nested
scene and depth. A frame that shows what the last one showed allocates no texture: an
unchanged frame is not uploaded again and each nested scene is redrawn into the layer it
had. An image no frame references is retired, then freed once the GPU has finished the
last frame that used it (a `Queue::on_submitted_work_done` fence the cache polls without
blocking). The render thread hands those images, and each render snapshot and set of
live values it replaces, to the program stage's collector thread ([R-RT-003](RULES.md#R-RT-003)). The render
thread stamps its reader slot as the handover below says. When a fatal error stops its
drawing it publishes the stopped marker too: it adopts nothing more and only repeats the
frame it last drew until teardown. The audio and outputs snapshots are not built.

**Live-value handover** (decided; built for the render thread: its generation, its reader
slot, its parameter channel and the gesture release. The mixer's channel and generation
are not, so audio faders have no live values yet): the engine learns that a
real-time thread holds a committed snapshot through a **generation counter per
subsystem**. Each time the control side publishes a subsystem's snapshot it bumps that
subsystem's generation and stores it in the snapshot. Each real-time thread that reads
the snapshot (the render thread for render, the mixer for audio) publishes the last
generation it adopted with one atomic store when it picks up a new snapshot, which
allocates, locks and waits on nothing ([R-RT-001](RULES.md#R-RT-001),
[R-RT-002](RULES.md#R-RT-002)). When a gesture's committing batch applies, the control
side notes the generation of the snapshot that carries it, and releases the gesture's
live values from the parameter channels once every thread that reads them has reported a
generation at least that high. Until then the parameter channel keeps the live value, so
no frame shows the old value in between. The control side polls the published
generations; the real-time threads never wait for it. A thread that isn't adopting
snapshots counts as caught up, so a gesture's live values aren't held while it is
stopped or parked (no output running, nothing to render, no device to mix for). Before
it stops or parks it publishes a stopped marker in place of its generation, and it shows
nothing while marked. Each reader's slot starts as the stopped marker, so a gesture
committed before a thread has ever run waits on nothing. On starting or resuming it
clears the marker before it loads a snapshot, to a value below every generation (so it
counts as caught up with nothing until it adopts one), and reads the parameter channels
only after that load. So if the control side saw the marker and released a gesture, the
thread's next load comes after the committing snapshot was published and already carries
the committed value. That needs a store then load on each side (the thread clears the
marker then loads the snapshot; the control side publishes the snapshot then reads the
marker), which release and acquire alone don't order: the marker and generation stores
and reads are `SeqCst`, with a `SeqCst` fence between each side's store and its load. A
fence doesn't allocate, lock or wait, so [R-RT-001](RULES.md#R-RT-001) and
[R-RT-002](RULES.md#R-RT-002) still hold.

<a id="protocol"></a>
## Control protocol
<!-- @section
covers = ["crates/jstream-protocol/**", "crates/jstream-control/**", "crates/jstream-server/**"]
depends_on = ["docs/adr/0013-command-model.md", "docs/adr/0006-mcp-full-access.md"]
-->

The wire format, versioning, error codes and compatibility policy are designed in
[jstream-protocol › Design](../crates/jstream-protocol/README.md#design). In short:

- **Envelope:** JSON messages tagged by `type`. A `hello`/`welcome` handshake exchanges
  the protocol version, then each `request` (with a client-chosen `id`) gets exactly one
  `response` or `error`, and `event`s arrive at any time.
- **Edits** (`scene.create`, `item.set_transform`, `send.set`, …): typed, undoable,
  always sent in an atomic batch (of one or more), which carries the optional
  `expected_rev` and idempotency key ([R-API-004](RULES.md#R-API-004)). Each applied
  batch increments `rev` by one. `item.move` moves an item between scenes keeping its
  id, `scene.set_role` (or, from protocol 0.17, `scene.create`'s optional `role`) marks a scene as a group, and placement edits take an optional
  `scene_id` so they can write a keyframe scene's override
  ([ADR-0024](adr/0024-scene-structure.md), protocol 0.12). Since 0.39,
  `scene.create_variant` makes a keyframe scene, `item.clear_override` removes one
  field of its override, `state.get` reports `variant_of` and `overrides`, and a batch
  result lists the overrides it dropped (`dropped_overrides`, ADR-0040 §1).
  `output.create`, `output.set_target` and `output.delete` configure the collection's
  outputs (protocol 0.7); the `record` output kind, a file on the engine's machine, is
  0.9, and the `virtual_cam` kind, a webcam other apps on that machine see, is 0.18.
  `canvas.set_size` and `item.set_geometry` set the canvas and an item's box
  ([ADR-0022](adr/0022-item-transform-and-canvas.md), protocol 0.11), which deprecates
  `item.set_transform`; a geometry's `flip_h` and `flip_v` mirror its content (protocol
  0.32). `encoder.create`, `encoder.update` and `encoder.delete`
  configure encoder profiles, and `output.set_encoder` points an output at one or back
  at the engine's default (protocol 0.13). A profile an output uses can't be deleted.
  `video_mix.create`, `video_mix.update` and `video_mix.delete` configure video mixes
  (an input, a resolution and a divisor of the base rate), and `output.set_video_mix`
  points an output at one or back at the program (protocol 0.28). A mix an output shows
  can't be deleted, and a mix showing a scene blocks that scene's delete.
  `item.set_opacity` and `item.set_blend` set a placement's opacity and blend mode
  (`item.add` takes both too), and `filter.add`, `filter.remove`, `filter.set_enabled`,
  `filter.set_settings` and `filter.reorder` edit the filter chains of a source, the
  master stage or an output, addressed by an `owner` and a filter ID;
  `SourceInfo.filters`, `OutputInfo.filters` and `state.get`'s `master` report them
  ([ADR-0026](adr/0026-source-filter-chains.md), protocol 0.22;
  [ADR-0080](adr/0080-program-master-and-output-chains-v2.md), protocol 0.29). Since 0.43
  the owners `scene` and `item`, `SceneInfo.filters`, `ItemInfo.effects` and a
  descriptor's `max_outset_px` are in the protocol
  ([ADR-0065](adr/0065-scene-chains-and-item-effects.md)). The engine runs both.
  `source.set_audio`, `bus.create`, `bus.rename`, `bus.set_fader`, `bus.delete`,
  `send.set` and `send.remove` edit sources' audio, the buses and the send matrix
  ([ADR-0028](adr/0028-audio-model-v2.md), protocol 0.19), and `bus.set_monitor` a
  bus's monitor, an output device and a tap point (protocol 0.42).
  `hotkey.set` and `hotkey.clear` bind a key chord to an action and unbind it
  ([ADR-0024](adr/0024-scene-structure.md) §5, protocol 0.25). Deleting the scene,
  output or transition preset an action names removes its bindings in the same edit, the batch result lists
  them as `removed_hotkeys`, and undo restores them
  ([ADR-0040](adr/0040-scene-structure-overrides-shared-groups-and-hotkey-targets.md)).
  `folder.create`, `folder.rename`, `folder.set_parent`, `folder.reorder`,
  `folder.delete` and `scene.set_folder` organise the scene list, and `label.create`,
  `label.update`, `label.reorder`, `label.delete`, `scene.set_meta` and
  `source.set_meta` edit labels and organisation metadata
  ([ADR-0024](adr/0024-scene-structure.md), protocol 0.31; a scene's labels, protocol
  0.45, [ADR-0078](adr/0078-scenes-carry-labels.md)).
- **Stream keys** (protocol 0.7): the actions `output.set_key` and `output.clear_key`.
  A key is not collection state: it goes to the OS credential store and is write-only,
  never in a response, event, error, query or log ([R-SEC-001](RULES.md#R-SEC-001));
  `output.status` reports only `key_set`. A key is bound to its output's server: an
  applied change that deletes the output or points it at another scheme, host or port
  removes the key, so it never goes to a server it wasn't set for (#343). From 0.37
  `output.status` also reports `cleartext`: whether publishing sends the key across the
  network unencrypted (plain `rtmp://` to a host other than loopback, #562).
- **Actions** (`output.start`, `scene.switch`, `youtube.go_live`, …): side effects,
  never undoable or batchable. A *script* runs actions in sequence and returns a result
  for each step.
- **Program scene** (protocol 0.16, [ADR-0030](adr/0030-studio-state-and-scene-switching.md)):
  the action `scene.switch { scene_id }` puts a scene on program, with no
  `rev` change. `studio.get` reads the program scene and `studio.changed` reports every
  change of it, including the ones a batch, undo or redo causes by creating the first scene or
  deleting the program scene.
- **Pairing** (protocol 0.21, [ADR-0044](adr/0044-pairing-remote-browsers.md)): the
  action `pairing.start` answers a one-time code (an action result, the one place it is
  sent), which a device trades at `POST /api/v1/pair` for a device token of its own;
  `device.list` and the action `device.revoke` manage the paired devices. They concern
  whom a listener admits, so `jstream-server` answers them, and a session with no server
  in front, or one whose server pairs nothing (the desktop app's), answers `unsupported`
  ([ADR-0057](adr/0057-server-answers-credential-calls.md)).
- **Plugins** (protocol 0.35, [ADR-0069](adr/0069-wasm-plugin-limits-and-capabilities.md)): the actions
  `plugin.install`, `plugin.grant` and `plugin.enable` install a WASM automation plugin
  with what a person grants it, change the grant and re-enable it; they are for a person,
  so no MCP tool, plugin or hotkey carries them. The event `plugin.state_changed` reports
  a plugin enabled or disabled, with why the engine disabled it. A plugin calls the API
  through a scoped `Control` that checks each call against its grant, using
  `jstream-protocol`'s table of every command's class (`read`, `edit`, `live`, `outputs`
  or never), and the hosts and files a call's settings name against the grant's hosts
  and folders, since a class never widens network or filesystem reach; a call outside
  the grant fails with `forbidden` and never reaches the engine. The engine runs the
  plugins in process, on `jstream-plugin-wasm`'s host behind its default `wasm-plugins`
  feature, each on threads of its own
  ([ADR-0075](adr/0075-wasm-plugin-host-in-the-engine.md)). A package is a directory
  with `plugin.wasm` and an optional `manifest.json`, and a plugin that exports `on_event`
  gets the engine's events. A grant never gives more than the manifest's `classes`,
  `hosts` and `folders` ask for (`invalid_argument` on that grant field), an update whose
  manifest asks for more stays disabled as `needs_grant` until a person grants it again,
  and the installed plugins are saved in the plugin folder
  ([ADR-0076](adr/0076-saved-wasm-plugins-and-manifest-requests.md)). The mock runs no
  plugins: it answers `plugin.install` with
  `unsupported`, and the other two with `not_found`, as an engine built without the
  feature does.
- **Studio mode** (protocol 0.30): the action `studio.set_preview { scene_id }` puts a
  scene on preview, also with no `rev` change; `studio.get` and `studio.changed` carry
  `preview_scene` beside `program_scene`.
- **Transitions** (protocol 0.30, [ADR-0029](adr/0029-transitions.md)): the edits
  `transition.create`, `transition.update` and `transition.delete` manage presets (each
  with a kind and a settings map or patch,
  [ADR-0037](adr/0037-kind-descriptors-for-transitions-and-plugins.md)),
  `transition.set_default` sets the default, `scene.set_transitions` sets a scene's
  `transition_in` and `transition_out`, and `item.set_transition` an item's transition
  settings. `scene.switch` takes an optional `preset_id`, and the `studio.changed` it
  causes names the preset it ran and how long the transition lasts (0 for a cut).
  `state.get` reports the presets and the default under `transitions`, and `kind.list`
  the transition kinds.
- **Live values** (`live.set`, `live.cancel`; query `live.list`; events `live.changed`,
  `live.ended`; protocol 0.26): actions that override live-adjustable parameters during
  a drag or a live dialog, with no `rev` bump or undo step; one batch carrying `ends_gesture`
  commits the gesture ([ADR-0032](adr/0032-live-edits-and-undo.md)). Built for item
  placements and crops; audio faders follow with the mixer's parameter channel.
- **History** (`history.undo`, `history.redo`): per-client undo and redo, a fourth kind
  of call that applies a step's inverse (or the step again) as one `rev` bump
  ([ADR-0032](adr/0032-live-edits-and-undo.md)). It refuses with `undo_conflict`
  rather than overwrite another client's later change (protocol 0.10).
- **Privacy calls** (protocol 0.33, deprecated, removed in 1.0): JStream has no privacy
  screen ([ADR-0080](adr/0080-program-master-and-output-chains-v2.md) §4).
  `privacy.set` is refused with `unsupported` and still goes to the audit log as a
  refusal, `privacy.get` always reads off, and `privacy.changed` is never sent. Preview
  streams show the pre-master stage (the editing view) by default; a client asks for the
  post-master stage to see what viewers see. `preview.subscribe` has no post-master stage
  yet.
- **Encoder sessions** (protocol 0.13): `output.status` reports, per running output,
  its encoder session's profile, the other outputs sharing it and whether the session is
  dedicated to it because of its own video chain (0.33), plus the sessions and hardware
  sessions in use. A profile's settings and the output's video mix at start
  count, so an output started after `encoder.update` gets a session of its own, and
  outputs on different mixes never share one. While any output holds a hardware
  session (a stopping output does until its pipeline is joined), a new one for a codec
  the machine lists a hardware encoder for (or when it can't list them) opens with
  `require`, and when the encoder refuses it
  `output.start` fails with `hardware_session_limit` (0.44) naming the outputs that hold
  hardware sessions, with no fallback to a software encoder
  ([ADR-0080](adr/0080-program-master-and-output-chains-v2.md)).
- **Queries** (`state.get` → state + `rev`, the canvas included, `scene.list`, `source.list`, `item.list`,
  `output.list`, `output.status`, `encoder.list` (protocol 0.13), `video_mix.list`
  (protocol 0.28), `video.snapshot` → the program mix (not a video mix) as a PNG in the
  JSON result, longest side at most 1280, with the layout `rev`; `kind.list` and
  `kind.describe` (protocol 0.8) → the kinds the engine describes, each with a generated settings
  schema ([ADR-0025](adr/0025-typed-settings-kind-descriptors.md)); `delete.plan` (protocol 0.15) → the
  batch that deletes a scene or source (or, from protocol 0.24, a bus, or a folder or
  label) with everything
  that refers to it, and the `rev` it was planned at, plus the hotkey bindings it removes (protocol 0.25); `bus.list` (protocol 0.19) → the buses and sends;
  `capture.targets` (protocol 0.27) → the displays and windows a `display` or `window`
  source's `target` can name; `source.sizes` (protocol 0.40) → each source's current
  size, runtime state ([ADR-0022](adr/0022-item-transform-and-canvas.md) §4); audio
  levels are the `meters.tick` event, not a query).
- **Events** (`state.changed{rev}`, `output.state_changed`, `events.dropped`, `stats.tick{seq, drops}`, `meters.tick`,
  `source.size_changed` (protocol 0.40), …).
  `stats.tick` (protocol 0.6) carries the pipeline's running drop totals
  ([R-RT-004](RULES.md#R-RT-004)), and `meters.tick` (protocol 0.19, about ten a second
  while the mixer runs) every source's and bus's levels and the master's loudness,
  which `audio.reset_loudness` restarts; the fields are in
  [jstream-protocol › Design](../crates/jstream-protocol/README.md#design).
- **YouTube** ([ADR-0058](adr/0058-youtube-sign-in-with-a-jstream-client.md); the UI's
  only, never offered over MCP): since protocol 0.23
  the actions `youtube.sign_in` (returns Google's consent URL; the loopback redirect
  finishes the sign-in in the engine, and no token ever crosses the protocol),
  `youtube.sign_out`, `youtube.create_broadcast`, `youtube.bind_broadcast` and
  `youtube.transition_broadcast`; the queries `youtube.status`, `youtube.list_broadcasts`
  and `youtube.list_streams` (never a stream key); the events `youtube.status_changed`
  and `youtube.health_changed` (a running YouTube output's stream health); and the RTMP
  target `youtube { broadcast_id }`, which `output.start` resolves to the bound stream's
  `rtmps://` ingest and key through the Live API. All of these need a Google OAuth
  client: the one built into JStream (ADR-0058 §1, empty until the maintainer fills it
  in) or a developer's override; without one
  `youtube.status` reports `unavailable`, the rest fail with `action_failed`, and
  clients hide them. Streaming to YouTube with a stream key needs none of this. Planned
  for M3:
  the actions `youtube.chat.send { text }`, which posts to the bound broadcast's chat as
  the channel and goes to the audit log, and `youtube.chat.feature { message_id }`, which
  sets or clears the featured message (runtime state, reported by an event) that the chat
  message source draws as plain text. Since protocol 0.38 a running YouTube output's
  audience (concurrent viewers, likes, views) comes in a low-rate `youtube.stats`
  event, and the `youtube.stats` query lists the last readings with the engine's
  estimate of the day's API quota, by which audience reads slow and stop first
  (ADR-0041 §3 and §5); the chat rate follows with the engine's chat polling.
- **Errors** carry a stable machine-readable `code`, detail fields and a message. A
  `scene.delete` or `source.delete` refused because something still refers to its
  target lists every such reference in `invalid_argument`'s `blockers` (protocol 0.15,
  [ADR-0025](adr/0025-typed-settings-kind-descriptors.md)), and so does a `bus.delete`
  refused because a setting draws the bus (protocol 0.24: a `bus` reference, as the
  `waveform` and `spectrum` kinds' `audio` setting holds), and a `folder.delete` or
  `label.delete` refused because a scene or folder is in the folder or a source carries
  the label.
- **Binary streams** (WebSocket binary frames, protocol 0.20): preview video.
  `preview.subscribe` opens a stream of the program mix, or of any one scene (0.46,
  `scene_id`: the UI's projector windows and multiview), for the calling session,
  scaled down, at most `fps` pictures a second and only when the mix changes (as it
  does throughout a running transition), in H.264
  when the platform has an encoder and JPEG otherwise; `preview.unsubscribe` or the
  session's end closes it. Each frame is a fixed 32-byte header (channel, codec,
  keyframe, size, presentation time, `rev`) and the picture, and every hop that queues
  frames drops up to the next keyframe when full, so a slow client still decodes what
  it gets. The layout is in
  [jstream-protocol › Design](../crates/jstream-protocol/README.md#design). Meters
  travel in the JSON `meters.tick` event for now (protocol 0.19); a binary meter frame
  would be another kind of the same frame. The meter fields
  ([ADR-0028](adr/0028-audio-model-v2.md)): per channel, sample peak, RMS and true peak
  (4× oversampled, BS.1770); per bus, also momentary, short-term and integrated loudness
  (LUFS). Loudness is computed only for the master bus and for subscribed buses and
  sources. Integrated loudness is reset by the action `audio.reset_loudness`. A
  snapshot is the `video.snapshot` query's PNG, in the JSON result, not a binary frame.
- **Schema:** the JSON Schema is generated from the Rust types, and its snapshot is
  the API review ([R-TEST-002](RULES.md#R-TEST-002)).
- **Transport:** the `Control` trait is implemented in-process by core
  (`InProcessControl`, [jstream-core › Design](../crates/jstream-core/README.md#design))
  and over WebSocket by `jstream-control` (`WsControl`, feature `ws`), natively and in
  the browser, where the token travels as a subprotocol because a page can't set
  `Authorization` ([ADR-0045](adr/0045-leptos-web-ui.md)). `jstream-server`
  serves the WebSocket API at `/api/v1/ws` in front of any `dyn Control`, on `127.0.0.1` by
  default, with the token (the control token, or a paired device's on loopback or a TLS
  listener) required and `Origin`/`Host` validated
  ([jstream-server › Design](../crates/jstream-server/README.md#design)). Plain `ws://`
  for now; TLS is an open question there. An upgrade with no `Origin` header is allowed,
  because the token is the gate, while an `Origin` that is present must be on the allow
  list. The same server
  serves MCP's streamable-HTTP transport at `/mcp` on the control port, behind the same
  checks, when the host turns it on ([MCP](#mcp)), and the web UI's files at every other
  path, behind the same checks too, when the host names a UI directory. A browser
  presents the token as the password of HTTP `Basic` credentials, which it then sends
  on the UI's WebSocket upgrade as well; a browser client can instead offer it as the
  subprotocol `jstream.token.<token>`. On a TLS listener a browser can instead pair on
  the token-free page `GET /pair` and hold its device token, never the control token, as
  the `__Host-jstream_device` cookie
  ([ADR-0047](adr/0047-browser-pairing-page-and-device-cookie.md)).
- **Slow clients:** each connection gets a bounded event buffer, and the server keeps
  draining the engine's events while a client is slow to read. When the buffer
  overflows, the server drops the queued events and sends one `events.dropped {count}`
  notice (protocol 0.4), and the client re-reads state (`state.get` carries `rev`).
  Answers are never dropped. One slow client never slows the engine or other clients
  ([jstream-server › Design](../crates/jstream-server/README.md#design)).
- **Versioning:** [R-API-001](RULES.md#R-API-001).

<a id="mcp"></a>
## MCP
<!-- @section
covers = ["crates/jstream-mcp/**"]
depends_on = ["docs/adr/0006-mcp-full-access.md", "docs/RULES.md#R-API-002", "docs/adr/0036-mcp-events-polling-tool.md"]
-->

- **Tools are grouped by noun**, not one per command: one tool per protocol noun
  (`audio`, `bus`, `canvas`, `capture`, `delete`, `device`, `encoder`, `filter`, `folder`, `history`, `hotkey`, `item`, `kind`, `label`, `live`, `output`, `pairing`, `preview`, `scene`, `send`, `source`, `state`, `studio`, `transition`, `video` and `video_mix` so far), plus `batch` and later `script`. Each takes an `op` argument whose
  schema is generated from the protocol. The protocol's `youtube.*` ops get no tool, an
  output edit with a `youtube` target is refused, and `youtube.*` events stay out of
  the `events` feed: YouTube is for people, through
  the UI ([ADR-0058](adr/0058-youtube-sign-in-with-a-jstream-client.md) §3). The same
  holds for the `plugin.*` ops and events: only a person installs or grants a plugin
  (ADR-0069 G1).
- **Events** reach agents through the `events` tool, which returns the engine's events
  after a cursor from a bounded buffer and reports any it dropped
  ([ADR-0036](adr/0036-mcp-events-polling-tool.md)). The high-rate `stats.tick` is kept
  apart: each read returns the newest tick as `stats` (running totals), so it can't
  evict the state and output events and its drop counts stay visible. `meters.tick`,
  about ten a second while the mixer runs, is kept the same way, as `meters`. A drag's
  `live.changed` events are merged, in place, into the newest one of their gesture that
  no read has returned yet, while only `live.changed` events for other parameters follow
  it, so an unread drag holds one buffered event and `seq` has no gaps. `youtube.*`
  and `plugin.*` events are not returned to agents (ADR-0058 §3, ADR-0069 G1).
- **Schema size:** the goal is progressive discovery. Tools list only their op names, and
  `describe_api` returns detailed schemas on demand, so the agent's context stays small.
  The first cut does not do this yet: each tool carries its ops' full schemas (about 731 KB
  of `tools/list` today, as `tools.snap` stores it; `live` and `hotkey` each carry most
  of the edit schemas `item` and `batch` do, because `live.set` takes edits and
  `hotkey.set` takes any action), and `describe_api` returns the whole protocol schema. Shrinking
  the tools is an open question in
  [jstream-mcp › Open questions](../crates/jstream-mcp/README.md#open-questions), and the
  MCP revisions and transports are in
  [jstream-mcp › Design](../crates/jstream-mcp/README.md#design).
- **Preview streams** need a binary channel, which MCP doesn't have, so the `preview`
  tool's `preview.subscribe` fails with `action_failed`; an agent looks with
  `video.snapshot`.
- **Annotations and third-party text:** every tool carries MCP tool annotations derived
  from its ops (`readOnlyHint` for queries only; `destructiveHint` for actions, history
  calls and deletes; `openWorldHint` for `output`, `source`, whose `browser` kind loads
  any web page, and `batch`), so a client can ask the user before an agent retargets an
  output, sets its key, goes live, points a browser source at a page or deletes. Text
  from outside JStream sits in `remote_*` fields or in an `output.start` `action_failed`
  `reason` (the whole of which is untrusted), and the tool descriptions and server
  instructions say it is data, never instructions
  ([ADR-0059](adr/0059-mcp-annotations-and-remote-text.md)). The instructions also say
  that names and other state text can be set by any client or plugin, and are data too
  (ADR-0069's U1).
- **Names** follow the tool-name pattern in [R-API-002](RULES.md#R-API-002).
- **Snapshots** are returned as images, capped in size (default longest side 1280 px), with
  the state `rev` they show. The `video` tool's `video.snapshot` result leads with an
  image content block (`image/png`, the protocol result's base64), the only copy of the
  PNG: the protocol `Response` it also carries as text and as `structuredContent` has a
  short placeholder in `png`, so the image doesn't flood the agent's context.
- **Access:** full access ([ADR-0006](adr/0006-mcp-full-access.md)), always authenticated.
  The stdio proxy (`jstream-cli mcp`) reads the token from the user-only token file. The
  streamable-HTTP transport is served by `jstream-server` at `/mcp` on the control port,
  behind the same token, `Host` and `Origin` checks as the WebSocket API, so it validates
  `Origin`, as the
  [MCP transport spec](https://modelcontextprotocol.io/specification/2025-06-18/basic/transports)
  requires. Like the WebSocket API it lets a request without `Origin` through to the
  token check: browsers send `Origin` on a cross-origin `fetch`, and the required
  `Content-Type: application/json` forces a preflight, so only native clients send none
  ([jstream-server › Design](../crates/jstream-server/README.md#design)).
- Every agent action goes to the audit log with the client's name. Undo is per client,
  so an agent can never undo the user's work.

<a id="gpu-pipeline"></a>
## GPU pipeline
<!-- @section
covers = ["crates/jstream-render/**", "crates/jstream-platform-windows/src/gpu/**", "effects/**"]
depends_on = ["docs/adr/0004-wgpu-with-native-interop.md"]
-->

The Windows path is validated by **spike S1 in M0** before M1 builds on it:

1. **Capture:** WGC delivers D3D11 textures from its frame pool. They can't be shared, so
   capture copies each frame into a **shared NT-handle texture** that JStream allocates
   (one GPU copy).
2. **Import:** the shared texture is opened in D3D12 and wrapped into wgpu through
   wgpu-hal (`texture_from_raw` → `create_texture_from_hal`, both `unsafe`, so they live in
   `jstream-platform-windows`). Sync uses a **shared fence**: D3D11 signals and the wgpu
   queue waits, then the reverse for release. D3D12 has no keyed mutex, so fences are
   the only sync mechanism. Raw D3D12 objects taken through hal must be released before
   the wgpu device: wgpu-hal loads `d3d12.dll` itself and unloads it with the device, so a
   later `Release` crashes (S1 did, at exit).
3. **Composite** in FP16 linear light (WGSL).
4. **Convert** to NV12/P010 with two render passes that write straight into the Y and UV
   plane views of the shared NV12 texture (proven by S1). Writing R8 + RG8 targets in
   compute and copying them into NV12 through hal, or a D3D11 video processor, is the
   fallback for an adapter that rejects planar render targets.
5. **Encode:** the NV12 texture is shared back to D3D11 for the Media Foundation encoders
   (fence-synchronized).
6. **Fallbacks:** CPU readback/upload, counted in stats with a stated budget. On
   hybrid-GPU laptops, capture and render must use the same adapter, or the engine falls
   back and reports it.

**One-scene compositor** (`jstream-render`), the part of step 3 that is built. It draws
one canvas: a straight-alpha sRGB clear and a bottom-first stack of images. Placement
is evaluated with `jstream-protocol`'s shared formula in either of ADR-0022's forms: a
geometry (crop in source pixels, fitted into the box by `stretch`, `fit` or `fill`,
which is clipped to the box, mirrored by its flips, then rotated about the box centre), or a legacy 0.3
transform through `legacy_quad` (crop, scale against the current source size,
top-left at `position`, clockwise rotation about that corner). Both count in the
image's extent, its pixel size unless the caller gave another: a source drawn sharp for
an item that shows it enlarged has more pixels than the size it is placed at
([ADR-0073](adr/0073-sources-drawn-at-their-shown-size.md)). A geometry with
non-neutral 3D draws [ADR-0050](adr/0050-3d-transforms-rendering.md)'s drawn polygon,
clipped by the near plane, with a per-vertex `w` for perspective-correct texturing and
analytic edge coverage, sampled from a mip chain of its cropped source (rebuilt on the
GPU once per change to the source, so at most once per source and frame) with 16×
anisotropic filtering. The engine accepts any 3D it can draw from protocol 0.34.
Opacity is a straight multiplier in `[0, 1]` and the blend is premultiplied
src-over. The composite pass writes premultiplied linear light into `Rgba16Float`; a
resolve pass un-premultiplies into
`Rgba8UnormSrgb`, which a snapshot can read back. The device is the platform `GpuContext`
when interop is present, and otherwise a device the crate opens with a hardware adapter
first (D3D12 first on Windows); the software adapter is the fallback, so a machine with
no GPU still runs. A nested scene is composited into an `Rgba16Float` layer that stays
on the GPU
and is placed in its parent: `render_image` makes a new one, and `render_into` redraws
a layer the caller keeps, so a frame allocates no texture for it. The caller walks the
nesting, bounded in depth. `ResourceCache` keeps a compositor's uploads and layers from
frame to frame, keyed by the caller's IDs, and hands an image back for freeing only
after the GPU has finished the last frame that used it (see
[Threads › State to RT threads](#threading)). On the GPU path the engine's render
thread composites on the platform's `GpuContext`, and capture moves onto it through
`Capture::on_context`: each captured texture is imported once into the cache and
sampled with no CPU copy, its alpha read as 1 as on the CPU path, the frame's fence hold is staged on each submit that reads it
and dropped when the frame ends, and the submit hands the texture back in `PRESENT`.
Staging attaches to the queue's next submit, so render holds the context's
`SubmitLock` from staging through its submit (the composite's and the NV12
conversion's), and other threads that submit on that queue (the browser source's
copies, Linux's PipeWire capture copies) take it too.
Without a context, or a capture that can deliver on it, the engine keeps CPU frames
(step 6; [jstream-core › Runtime](../crates/jstream-core/README.md#runtime)). A
browser source's page paints on the same context and is imported the same way, read
as premultiplied sRGB (`import_premultiplied`), CEF's layout. Step 5's encoder side is built:
on Windows, `Codecs::on_context` gives H.264 encoders that copy each NV12 encode target
on D3D11 into a texture of their own, fence-synchronized both ways, and
`jstream-encode`'s `VideoSession::open_on` picks them or falls back to a CPU encoder
([jstream-platform-windows](../crates/jstream-platform-windows/README.md#purpose)).
Step 4's NV12 pass is built too: it reads the sRGB output and writes the Y and CbCr
planes, either plane views of a native NV12 texture or an `R8Unorm` and an `Rg8Unorm`
texture where planar render targets are missing, in BT.709 limited or full range with
left-sited chroma. The render thread (in `jstream-core`) runs it: a video session
opened on the context gets each composite of its video mix converted into the next of
that mix's ring of the platform's encode targets, which it is pushed as a GPU frame, in
its own submit that stages the target's fence and hands it back in `PRESENT`. Sessions
on the CPU path and the virtual cameras still get their mix read back, and a mix none
of them is fed is not read back. Built-in video filters are kinds with
descriptors whose passes `apply_filter` draws from any image (an upload, a layer or an
import, a browser page's included) into a premultiplied `Rgba16Float` layer, of the size the filter reports
([jstream-render › Filters](../crates/jstream-render/README.md#filters));
`apply_filter_into` draws into a layer the caller keeps, which is how `jstream-core`'s
chain runtime runs a source's video chain before placement, and a scene's over its
composite once per frame wherever the scene shows, and an item's effects over its box
([ADR-0065](adr/0065-scene-chains-and-item-effects.md)), without allocating per
frame. Still outside this crate: the render
thread (in `jstream-core`), P010, loading effect packs (`jstream-effects`; this
crate draws them through the filter harness), and the output path below.

**Device loss** (#349). A GPU hang that Windows resets (TDR), a driver reset or a removed
GPU takes the device with it, and with it everything made on it. The compositor watches
its device, its own or the context's, for the loss (`Compositor::lost`), and keeps the
invalid-resource errors that follow from panicking the render thread. The render thread
treats a lost device as one event: it opens the compositor on a new device at once, off
the render thread with the fallback's deadline, and the output repeats its last frame
meanwhile and stays up. On the platform's context that is step 6's fallback, so capture
moves to CPU frames. On a device of its own it is a new device, the hardware adapter
first again. Encoders on the CPU path take the readback, so they don't reopen. A session
that takes the context's encode targets (step 4) rules out leaving the context, so a lost
context device fails its outputs at once instead
([jstream-core › Runtime](../crates/jstream-core/README.md#runtime)); the NV12 pass
reports the loss as an error rather than panicking. The third loss within
60 s fails the output instead. Each loss is logged and counted in the output's stats
([jstream-core › Design](../crates/jstream-core/README.md#design)). Not built yet: the
engine event for a loss (a protocol change), attributing a loss to the effect pack that
was drawing, quarantining that pack, and the GPU-time guard (timestamp queries that
quarantine an effect whose pass runs over half the frame budget for 30 frames running,
before Windows' TDR fires), all of which wait for packs to be drawn in the engine (V0's
effect-pack kinds).

**Transitions** ([ADR-0029](adr/0029-transitions.md), with
[ADR-0052](adr/0052-transition-mechanics-and-studio-preview.md)'s mechanics). A Move is not a texture blend:
during it, the program mix is a synthetic item list whose transforms and opacity come
from the transition state, drawn over both scenes' source images. The control thread
pairs the items when the switch starts and publishes the plan in the shot, one
`jstream_render::transition::Track` per item (a pair, an appearance or a
disappearance, each with its delay, duration and easing); the render thread only
samples each track at the frame's media time. A switch mid-transition starts from what
is on screen: the control thread samples the running plan at the switch time with the
same function, so both threads agree on it without the render thread keeping any
state. Rotation, tilt and turn take the shortest way round. The easing curves are one
shared function (`jstream_protocol::Easing`) with golden tests. After easing, values
are clamped to a hard range: opacity to `[0, 1]`, crop to `[0, source size]`, size to
`≥ 0` and perspective to `≥ 1`; the spring may overshoot everything else.

**S1 result**, on a Ryzen 9 8945HS with Radeon 780M (driver 32.0.22029.9039), a
3840x2160 display at 29 Hz and the async `AMDh264Encoder` MFT. The pipeline holds 60
ticks per second with no CPU copy of frame data. A 29 Hz display delivers about 30 new
WGC frames per second, so every other tick re-encodes the latest frame. This is not a
60 Hz capture result:

| Encode size | Frames encoded | Convert p50 / p95 | Encode p50 / p95 | CPU |
|---|---|---|---|---|
| 1920x1080 | 1801 of 1801, none dropped | 2.1 / 10.3 ms | 6.5 / 14.5 ms | 11% of one core |
| 3840x2160 | 1798 of 1799, 3 ticks dropped | 3.1 / 11.1 ms | 11.2 / 19.0 ms | 10% of one core |

The encoded output decodes to the test page's colours (BT.709 limited range). Still open:
- NVIDIA and Intel encoders, and a hybrid-GPU laptop.
- Encoder selection on multi-GPU machines. AMD's MFTs don't report
  `MFT_ENUM_ADAPTER_LUID`, so the spike took the first hardware encoder, and a multi-GPU
  machine needs another way to match the encoder to the adapter.

wgpu is pinned exactly, because hal interop can change between releases. Effects are WGSL
plus an `effect.toml` manifest.

**Output path** ([ADR-0080](adr/0080-program-master-and-output-chains-v2.md)), for each
output: mix → master video chain → the output's own video chain → colour conversion
(step 4) → encoder. The render plan has a master node per consumed mix and a node per
output with a video chain. Built so far: the sharing key's output part. The master and
output video chains are not built yet (their audio chains are, protocol 0.29).

<a id="browser-source"></a>
## Browser source
<!-- @section
depends_on = ["docs/adr/0021-browser-source.md", "docs/adr/0004-wgpu-with-native-interop.md"]
-->

A browser source is a CEF off-screen browser
([ADR-0021](adr/0021-browser-source.md)), built in `jstream-browser`
([ADR-0046](adr/0046-browser-crate-and-helper.md),
[its README](../crates/jstream-browser/README.md)). The desktop app and the daemon wire it
in behind their off-by-default `cef` features. While a program stage runs on a GPU
context, the mix draws each page's newest texture there (a premultiplied import, see
[GPU pipeline](#gpu-pipeline)); otherwise a page shows its transparent stand-in.
CEF runs in the sandboxed browser helper process, never in the engine's
([ADR-0048](adr/0048-sandboxed-browser-helper.md)), which the engine drives over two named
pipes. Each `OnAcceleratedPaint`
hands over an NT handle to a D3D11 BGRA texture with premultiplied alpha, no keyed mutex
and no fence. CEF takes the texture back into its pool when the callback returns. So the
helper copies it, inside the callback, into one of three textures it shares with the
engine and waits for that copy. The engine opens that texture on its wgpu D3D12 device,
imports it through hal as in the GPU pipeline's step 2, copies it into a texture the
source owns, waits for that copy, and hands the shared texture back. The source's audio
comes from CEF's audio handler as planar float PCM, which crosses the pipe interleaved;
the engine stamps frames and audio as they arrive.

**Choices made by spike S4:**
- **Bindings:** the `cef` crate (tauri-apps/cef-rs), pinned exactly, with default
  features off. It is generated from CEF's C API and released for each CEF version. Its
  sys crate downloads the CEF binary distribution and builds CEF's C++ wrapper with CMake
  and Ninja, so it needs the lint-deps allowlist entry and governance change that
  ADR-0021 names.
- **Helper processes:** a separate small `jstream-browser-helper.exe`, set as CEF's
  `browser_subprocess_path`, so the engine exe never runs as a Chromium subprocess. Both
  layouts that ADR-0021 allows measured the same. ADR-0048 has since replaced this
  layout to get the sandbox.
- **The sandbox:** S4 ran unsandboxed and didn't test it. On Windows, CEF sandboxes only
  in its bootstrap layout, where CEF's `bootstrap.exe` is the exe and loads the
  application as a DLL, and every CEF process is that exe. ADR-0048 adopts it in a helper
  process of its own, which S4's measurements below don't cover.

**S4 result**, on the S1 machine with CEF 154.0.34 (Chromium 154.0.8037.98). Each run
measured 30 s of the built-in test page, which has a frame counter, alpha probes and a
440 Hz / 660 Hz stereo tone:

| Run | Paint rate | Intervals over 1.5 frames | Callback p50 / p99 | CPU, all processes | Private memory |
|---|---|---|---|---|---|
| 1920x1080, helper is the engine exe | 60.03 fps | 3 of 1799 | 2.6 / 3.9 ms | 22% of one core | 587 MB |
| 1920x1080, separate helper exe | 60.02 fps | 0 of 1799 | 2.2 / 3.7 ms | 21% of one core | 569 MB |
| 3840x2160, helper is the engine exe | 60.01 fps | 1 of 1799 | 4.1 / 6.6 ms | 22% of one core | 700 MB |

- **Video:** all 1800 paints per run were accelerated, with no failures. The page's frame
  counter showed every page frame once, none skipped, repeated or out of order. The
  alpha probes held on every frame: the page texture is transparent and premultiplied,
  and blending it over an opaque background gave the expected colours. CEF reused about
  250 distinct texture handles at 1080p and 430 at 4K.
- **Audio:** CEF honoured the requested 48 kHz stereo in 480-frame (10 ms) packets. Both
  tones came through at the right pitch and level on the right channels. The runs
  received 2999, 3000 and 2999 packets, as expected for 30 s.
- **Audio timestamps:** contrary to CEF's header, which documents Unix-epoch
  milliseconds, a packet's `pts` is not. It differed from the wall clock by the same offset to the millisecond in all
  three runs, which fits milliseconds since boot (Chromium's QPC-based `TimeTicks`).
  That is inferred, not checked against QPC. If it holds, packets map onto the media
  clock directly.
- **Shutdown:** every run reported one audio error, "Socket closed unexpectedly", while
  the browser closed. The source must treat it as the end of the stream, not a fault.
- **Codecs:** the page reports no H.264 or AAC support, and VP8, VP9, AV1, Opus, Vorbis,
  FLAC, MP3 and WAV playable. That is the royalty-free set ADR-0021 expects.
- **Runtime size:** the CEF runtime is about 400 MB uncompressed, 290 MB of it
  `libcef.dll`. Its compressed size in the zip wasn't measured, so ADR-0021's estimate
  of 150–250 MB added to the zip is still unchecked. The runtime includes CEF's
  `CREDITS.html`.

Still open: the CEF runtime's compressed size in the zip, other GPU vendors and
multi-GPU machines (Chromium must render on the adapter the engine imports on), several
browsers at once, pages with video, input events, and a re-run of S4's measurements in
the sandboxed helper, whose frames take a second copy.

<a id="persistence"></a>
## Persistence, config and secrets
<!-- @section
covers = ["crates/jstream-graph/src/persist/**", "crates/jstream-core/src/settings/**"]
depends_on = ["docs/RULES.md#R-API-003", "docs/RULES.md#R-SEC-001", "docs/adr/0025-typed-settings-kind-descriptors.md"]
-->

- **Collections:** JSON with a `schema_version` field and forward migrations in
  `jstream-graph`, which checks a loaded collection as it checks edits. A field added
  for new collection state (encoder profiles, hotkey bindings, transition presets and
  settings, folders, labels and organisation metadata, keyframe scenes' `variant_of` and
  `overrides`) is omitted when empty or at its default (for presets, a new collection's one `cut`), so a file without it is still
  valid at the same `schema_version`. `jstream-core`
  loads the file the host names and replaces it atomically after every change.
- **Asset files** named in settings are stored relative to the collection folder or the
  app asset folder when they are under one, so a moved or portable setup still finds them.
  Each stored source, filter and transition preset records its kind's descriptor
  version, migrated on load like the collection
  ([ADR-0025](adr/0025-typed-settings-kind-descriptors.md)). Until kinds ship settings
  migrations, a source, filter or preset saved at another version is refused on load.
  A filter whose kind the engine doesn't describe (a pack that isn't installed, or a
  CLAP plugin before the scan that runs after the load) loads with its settings and
  version verbatim and is saved back as it was; the engine bypasses it until its kind
  appears.
  A placement's `opacity` and `blend` are written only when they differ from 1 and
  `normal`, a geometry's `flip_h` and `flip_v` only when true, a disabled filter's `enabled` only when false. Schema version 2 adds a scene's
  `filters` and an item's `effects`, each omitted while empty; the migration from 1
  changes nothing, and refuses a version 1 file that holds either.
- **Installed WASM plugins:** `plugins.json` (version 0) in the plugin folder, by default
  `plugins` in the data directory: each plugin's package path, grant, manifest request
  and disabled reason. `jstream-core` loads it at startup, loading each enabled plugin
  again, and replaces it atomically after every change. A file it can't read is left as
  it is ([ADR-0076](adr/0076-saved-wasm-plugins-and-manifest-requests.md)).
- **App settings:** `jstream.toml` in the platform config directory.
- **Secrets** (stream keys, OAuth tokens) are kept in the OS credential store. On
  Linux, where no Secret Service is reachable, they are kept in a `0600` `secrets` file
  in the config directory instead ([ADR-0071](adr/0071-linux-secret-store.md)).
- **Control token:** a random token in a user-only file in the config directory
  (Windows ACL restricted to the user). It is rotated on demand.
- **Logs and crash dumps:** rotating logs and local minidumps in the data directory
  ([R-SEC-004](RULES.md#R-SEC-004)).

<a id="packaging"></a>
## Packaging
<!-- @section
covers = ["xtask/src/dist.rs", ".cargo/config.toml"]
depends_on = ["docs/adr/0066-portable-windows-distribution-v2.md"]
-->

Windows ships as a **portable zip**: unzip and run `jstream.exe`, with no installer and no
admin ([ADR-0066](adr/0066-portable-windows-distribution-v2.md)). The folder also holds
`jstream-daemon.exe`, `jstream-cli.exe`, the `jstream-vcam-windows` DLL, and, in its own
`browser` folder, the CEF runtime for browser sources
([ADR-0021](adr/0021-browser-source.md)) with the sandboxed browser helper: CEF's
`bootstrap.exe` renamed `jstream-browser-helper.exe` and `jstream-browser-helper.dll`,
signed with the same certificate if releases are signed
([ADR-0048](adr/0048-sandboxed-browser-helper.md)). The hosts look for the helper there
(`CefConfig::beside_exe`). From M3 it also holds `jstream-plugin-host.exe`, the
out-of-process CLAP plugin host ([ADR-0035](adr/0035-clap-plugins-out-of-process.md)).
Release builds link the C runtime statically and need only OS components (WebView2, Media
Foundation, Direct3D 12).

- **State:** in `%APPDATA%\JStream`, or in a `data` folder beside the exe when a
  `portable` marker file is present. Every host finds it through `jstream-platform`'s
  `paths` module.
- **Building the zip:** `cargo xtask dist`, on Windows with MSVC, CMake and Ninja. It
  builds the web UI for release with trunk (or takes a built one with `--ui-dir`), builds
  `jstream.exe`, `jstream-daemon.exe`, `jstream-cli.exe` and the virtual-camera DLL for
  release with the hosts' `cef` features on, then the browser helper's DLL with its own
  `cef` feature in a second `cargo build`: built together, Cargo would unify
  `jstream-browser`'s `helper` feature into the hosts and link CEF into the engine's
  process. The helper's build downloads the pinned CEF distribution and builds its C++
  wrapper; `dist` finds the distribution's folder from that build's JSON messages
  (`cef-dll-sys`'s link path that holds `bootstrap.exe`). It fails if any of the
  binaries imports a Visual C++ runtime DLL (`vcruntime`, `msvcp`), or if a host imports
  `libcef.dll`. The Universal CRT (`api-ms-win-crt-*`) is allowed: it is part of Windows
  10 and 11, and `jstream.exe` imports it even with `crt-static` (from a prebuilt static
  library built with `/MD`, most likely WebView2's loader). It zips them in one folder,
  `jstream-<git describe>-windows-x64` (or `--revision`), with the UI in `ui` (where
  `jstream.exe` looks), the licenses and a `README.txt`, into `target/dist`. The
  `browser` folder holds CEF's runtime files (all of its distribution's top-level files
  but import libraries, `archive.json`, `CMakeLists.txt` and the console bootstrap), its
  `locales`, the renamed bootstrap and DLL, CEF's `CREDITS.html` (its licences, FFmpeg's
  LGPL text among them) and a `CEF-VERSION.txt` naming the CEF and Chromium build, from
  `archive.json`. `--no-browser` builds the zip without browser sources (no CMake or
  Ninja needed). `.cargo/config.toml` links the C runtime statically for every MSVC
  build. A `RUSTFLAGS` variable replaces the config's flags, so `dist` adds `crt-static`
  back to it. The zip is written by `dist` itself, deflated, with fixed timestamps, so
  the same files make the same zip. The `dist` job in `after-review.yml` builds it with
  the release UI its `dist-ui` job builds, names it after the commit tested
  (`--revision`), and uploads it as an artifact, as is, on `main` and on a PR once
  review passes ([ADR-0054](adr/0054-platform-ci-after-review.md)).
- **Virtual camera:** registering the COM media source is **optional**, an explicit
  step the user chooses; nothing registers it or asks for elevation on its own
  ([ADR-0066](adr/0066-portable-windows-distribution-v2.md)). The step costs **one UAC
  prompt** (`jstream.exe --register-vcam`), because Frame Server ignores per-user
  registration. Inside it, the elevated process creates `%ProgramFiles%\JStream\vcam\`
  with a protected DACL (SYSTEM and Administrators full control, LOCAL SERVICE and Users
  read and execute, nothing inherited), copies the shipped DLL there with the same DACL,
  and points `InprocServer32` at the copy. The DLL in the app's folder, which the user
  can usually write to, is never registered
  ([THREAT-MODEL › Virtual camera](security/threat-model.md#vcam)). The app compares the
  copy with the DLL it ships by content, without elevation, and offers to register again
  when they differ, which is a new opt-in. Unregistering removes the keys and the copy,
  with one prompt. Until the desktop app has those modes, `jstream-daemon
  --register-vcam` and `--unregister-vcam` are them, and the daemon only logs the
  registration's state at startup
  ([jstream-daemon › Usage](../apps/jstream-daemon/README.md#usage)). Spike S2 (Windows
  11 25H2, build 26200) found:
  - **Per user (`HKCU`) doesn't work.** `MFCreateVirtualCamera` succeeds, but
    `IMFVirtualCamera::Start` fails with `0x80070003`, and Frame Server never loads the
    DLL.
  - **Machine-wide (`HKLM`) works.** Frame Server loaded the DLL in its `svchost` (LOCAL
    SERVICE, session 0) and served frames from a non-elevated producer.
  - **What the elevated step wrote:**
    - `HKLM\Software\Classes\CLSID\{…}` with `InprocServer32` set to the DLL's path
      and `ThreadingModel = Both`;
    - read and execute for LOCAL SERVICE on the DLL file. The DLL loaded from inside a
      user profile; whether this grant is strictly needed wasn't isolated.

    The spike also stopped Frame Server first, so a rebuilt DLL would replace the loaded
    one.
  - **Shared-object names:** a non-elevated engine can't create `Global\` objects (access
    denied). So the hand-off uses the engine's session namespace, and the DLL opens it as
    `Session\<id>\…`.
  - **`jstream-vcam-windows` on hardware** (Windows 11, AMD Radeon 780M, with the
    `vcam_smoke` producer): registered with `regsvr32` from a folder outside any user
    profile, the DLL was loaded by Frame Server (session 0, `svchost`, LOCAL SERVICE) and
    served only the producer's frames, with **no** LOCAL SERVICE grant on the file. So the
    grant matters only for a DLL inside a user profile, which is inferred from S2 rather
    than isolated. Frame Server requested about four samples per producer frame, because
    the stream didn't pace `RequestSample`; it now answers at most 30 a second (#193).
