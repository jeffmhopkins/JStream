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
verified = { hash = "b3:bcf5d3053a8f0044", by = "agent:claude@session_01D6iWjpMkoHbWLUxVnmDfxk", note = "U5: Hosts table desktop row adds the first cut (Windows, WebView2, fresh in-memory token handed to the page) with a README link; diagram and the new apps/jstream-desktop/src/main.rs (a thin entry point) fit the shape" }
-->

```
   clients — depend only on jstream-control + jstream-protocol (R-ARCH-003)
   ┌──────────────────────────────────────────────────────────────────────────────┐
   │ jstream-ui (Leptos→WASM)   jstream-mcp (MCP tools)   jstream-cli   3rd party │
   └───────────────┬──────────────────────────────────────────────────────────────┘
                   │  Control trait: in-process (InProcessControl), or WebSocket + token
                   │  (JSON messages, including video.snapshot's PNG; binary frames later: preview, meters)
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
| `jstream-desktop` | Tauri shell. The engine runs in-process. The webview UI connects to it over the same authenticated localhost WebSocket used remotely, so there is one code path. The first cut runs on Windows (WebView2), with a fresh in-memory token handed to the page at startup ([its README](../apps/jstream-desktop/README.md#design)). |
| `jstream-daemon` | Headless engine. Serves the UI, the WebSocket API and MCP over HTTP. Can start from a collection file (`--collection`). The first cut serves the WebSocket API and MCP over streamable HTTP and loads and saves `--collection`, not the UI yet, and on Windows links `jstream-platform-windows` to run the media pipeline and go live over RTMP(S), and registers the virtual camera (`--register-vcam`) ([its README](../apps/jstream-daemon/README.md#purpose)). |
| `jstream-cli` | Lightweight remote client: drives a running engine, and `jstream-cli mcp` runs a **stdio** MCP proxy to it. Does not link the engine. |

<a id="principles"></a>
## Core principles
<!-- @section
depends_on = ["docs/adr/0001-library-first-engine.md", "docs/adr/0031-source-types-and-decoding-scope.md", "docs/adr/0013-command-model.md"]
verified = { hash = "b3:405cffd61ea37377", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Edited principle 8: listeners require a token and check Host, and Origin when present, matching R-SEC-002's new wording; other principles unchanged" }
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
verified = { hash = "b3:7f90b5a4097428aa", by = "agent:claude@session_01ApfNxzxANDHPeESRr5XsgN", note = "Re-read against main.rs, which only adds docs merge-main to the usage list: the xtask line still names docs among its commands, so the layout holds" }
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
├── xtask/                  # docs, lint-deps, governance, gen-schema, golden, release
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
verified = { hash = "b3:928b559944b79c2d", by = "agent:claude@session_01Fdcnfj3q78X58b3kEQDyAo", note = "CL1: re-read; jstream-plugin-proto row now matches its new manifest summary (layer 0, no unsafe); lint-deps passes; plugin-clap and plugin-host rows stay planned" }
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
| 0 | `jstream-plugin-proto` | Shared-memory contract between the mixer and the out-of-process CLAP plugin host ([ADR-0035](adr/0035-clap-plugins-out-of-process.md)): `repr(C)` audio rings, parameter-event rings and a status block, all atomics; no OS calls | no |
| 0 | `jstream-vcam-proto` | Shared-memory layout and versioned contract between the engine and the vcam DLL: `repr(C)` types, constants and pure helpers (frame choice, object names); no OS calls | no |
| 1 | `jstream-protocol` | **The API.** Commands (edit/action), queries, events, binary streams, errors, JSON Schema | no |
| 1 | `jstream-platform` | Backend traits: `Capture`, `VideoDevices`, `AudioIo`, `Codecs` (`VideoEncoder`, `AudioEncoder`, `VideoDecoder`), `VcamSink`, `MediaClock`, `GpuInterop` (uses `wgpu` types), `StreamKeyStore`, `OAuthTokenStore`. The **single owner** of the encoder and decoder traits. | no |
| 2 | `jstream-control` | The `Control` trait plus the WebSocket client. WASM-safe: no engine, no tokio requirement in WASM builds | no |
| 2 | `jstream-graph` | Scene graph model, edit application and inverse edits (for undo), persistence and migrations. No GPU. | no |
| 2 | `jstream-render` | Owns the wgpu device; compositor, 3D transform/crop, WGSL effects, color conversion, snapshot readback | no |
| 2 | `jstream-audio` | Real-time mixer: adaptive resampling, send matrix, buses, filters, metering | no |
| 2 | `jstream-mux` | Pure-Rust muxers: FLV (with enhanced RTMP), MP4/fMP4 | no |
| 2 | `jstream-youtube` | OAuth plus the YouTube Live API client (broadcasts, streams, health, chat reading and sending as the channel, audience stats; [ADR-0041](adr/0041-youtube-interaction-scope-v3.md)). No media code. | no |
| 2 | `jstream-platform-windows` | WGC capture, MF webcam and capture-card capture (with MJPEG decode), WASAPI (incl. process loopback), MF encode/decode, MF hardware encoders, D3D11↔D3D12 interop, Credential Manager store for stream keys, vcam sink side, CEF browser backend (ADR-0021) | **yes** |
| 2 | `jstream-platform-macos` | ScreenCaptureKit, CoreAudio, VideoToolbox/AudioToolbox, IOSurface↔Metal (M3) | **yes** |
| 2 | `jstream-platform-linux` | PipeWire, VAAPI, dmabuf↔Vulkan (M3; codec policy per future ADR) | **yes** |
| 2 | `jstream-plugin-clap` | Planned for M3 ([ADR-0035](adr/0035-clap-plugins-out-of-process.md)): safe wrapper over the CLAP plugin ABI. Only `jstream-plugin-host` links it. | **yes** |
| 2 | `jstream-vcam-windows` | `cdylib` COM media source loaded by Frame Server ([ADR-0020](adr/0020-portable-windows-distribution.md)). Depends only on L0 and `windows`, so it logs through its own sink (the debugger and a capped file in the host account's profile), not `tracing`: no subscriber exists inside Frame Server. | **yes** |
| 3 | `jstream-encode` | Encoder sessions: outputs with no video chain share one per (mix, encoder profile), and an output with a video chain gets its own ([ADR-0027](adr/0027-program-master-and-output-chains.md)); rate control; fan-out of packet streams | no |
| 3 | `jstream-mcp` | MCP server over `dyn Control`: generated grouped tools and the stdio transport; its streamable-HTTP transport is served by `jstream-server` | no |
| 3 | `jstream-plugin-wasm` | wasmtime host for automation plugins (calls the API through `Control`) | no |
| 4 | `jstream-server` | axum HTTP/WebSocket over `dyn Control`; token and Origin checks; serves UI assets; serves MCP's streamable-HTTP transport on the control port behind the same checks | no |
| 4 | `jstream-sources` | `Source` trait plus built-ins ([ADR-0031](adr/0031-source-types-and-decoding-scope.md)): display, window, video capture device, image, color, text, browser, audio input, desktop audio, application audio, scene (M1b); media file, audio waveform, audio spectrum (M2); YouTube chat message ([ADR-0041](adr/0041-youtube-interaction-scope-v3.md)) (M3); Spout2 receiver (later). Uses platform traits; backends are injected. Images and fonts use pure-Rust crates. | no |
| 4 | `jstream-outputs` | `Output` trait; RTMP(S) client with reconnect, recording, vcam output. Uses `encode`, `mux` and platform traits. | no |
| 5 | `jstream-core` | `Engine`: executes commands, owns threads and the clock, injects platform backends, implements `InProcessControl` | no |
| 3 | `jstream-ui` | Leptos app compiled to WASM ([ADR-0045](adr/0045-leptos-web-ui.md)); depends on `jstream-protocol` and `jstream-control` only | no |
| 6 | `apps/*` | `jstream-desktop` and `jstream-daemon` (link core and server, and the platform crate of the OS they run on); `jstream-cli` (control and mcp only; test-only dev-dependencies, such as server for its end-to-end test, are outside this); `jstream-plugin-host`, the out-of-process CLAP host, planned for M3 ([ADR-0035](adr/0035-clap-plugins-out-of-process.md)) | no |

Splitting rule: start with these crates. Split one only when compile time, `unsafe`
isolation or a dependency boundary requires it, and record the split in an ADR
([R-ARCH-005](RULES.md#R-ARCH-005)).

<a id="data-model"></a>
## Data model
<!-- @section
covers = ["crates/jstream-graph/src/model/**"]
depends_on = ["docs/adr/0022-item-transform-and-canvas.md", "docs/adr/0023-placements-references-and-clones.md", "docs/adr/0024-scene-structure.md", "docs/adr/0025-typed-settings-kind-descriptors.md", "docs/adr/0027-program-master-and-output-chains.md", "docs/adr/0028-audio-model-v2.md", "docs/adr/0030-studio-state-and-scene-switching.md", "docs/adr/0037-kind-descriptors-for-transitions-and-plugins.md", "docs/adr/0040-scene-structure-overrides-shared-groups-and-hotkey-targets.md"]
verified = { hash = "b3:4bdb39f3da90046c", by = "agent:claude@session_016BkTSqJ3kAfTw3EVqkJnMr", note = "Y3 merge of main at 75a18e2 (B9 delete.plan 0.15, B10 scene.switch 0.16, B6-r 0.17 and more), Y3 now protocol 0.18: kind YouTube { broadcast_id }; depends_on takes main's ADR-0030" }
-->

```
Collection (JSON on disk, schema_version, rev: u64, canvas: { width, height })
├── sources        Source { id, kind, settings, filters[], duplicated_from?, labels, favourite, audio? }   video and/or audio
│                  SourceAudio { enabled, follow: Always | WhenShown, gain_db, muted, filters[] }
├── scenes         Scene { id, name, role: scene | group, variant_of?, overrides, folder?, background?,
│                          icon, color, favourite, show_in_multiview, transition_in?, transition_out?,
│                          items[] }   flat display order
│                  SceneItem { id, source_id, geometry: ItemGeometry, crop, visible, opacity, blend,
│                              effects[] (reserved, empty in v1), transition }
├── folders        Folder { id, name }   ordered, one level; organising only
├── labels         Label { id, name, icon, color }   ordered
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
├── master         Master { video_filters[], audio_filters[] }   applies to everything leaving JStream
├── privacy        Privacy { look: Blur { strength } | Slate { color | image }, muted_sources: [source_id] }
└── outputs        Output { id, kind, video_mix, audio_buses: [bus_id], encoder_profile, video_filters[], audio_filters[] }
                   kind: Rtmp { target: Generic { url } | YouTube { broadcast_id } } | Record { path, container } | VirtualCam
```

- `Program` and `Preview` are mixes driven by the studio state: `program_scene` and
  `preview_scene`, which are runtime state beside the outputs' state, not part of the
  collection or its `rev` ([ADR-0030](adr/0030-studio-state-and-scene-switching.md)).
  `Program` shows `program_scene`, including any transition in progress. A `VideoMix`
  with input `Program` follows it. The collection's first scene goes on program when it
  is created, and a deleted program scene gives way to the first scene left.
- Each source is **one** instance (one capture, one decoder), however many scenes use it.
  Adding an *existing* source to a scene adds a `SceneItem` that is a **reference**: it
  shares everything with the other placements of that source except its own placement,
  opacity and blend. Changing the source or its `filters[]` changes it everywhere.
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
  and 3D tilt, turn, depth and per-item perspective, drawn in stacking order with no
  depth buffer. `crop` is in source pixels. A source's current size is runtime state,
  not part of the collection. Until 1.0 an item may instead hold a **legacy placement**,
  protocol 0.3's transform as written, which the compositor evaluates against the
  source's current size every frame. Only an explicit `item.set_geometry` turns it into
  a geometry.
- **Scenes as sources:** a `Source` of kind `Scene(scene_id)` renders that scene, so
  scenes nest. A scene never contains itself at any depth, so the graph rejects any edit
  that would close a cycle (`item.add`, and `item.set_source`). The graph accepts any
  acyclic depth, but the program mix (the live output and `video.snapshot` alike) draws
  at most **8 levels** of scene sources below the program scene: a scene source deeper
  than that draws nothing, and the engine logs it at `debug`.
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
- **Folders and organisation are collection state:** folders, labels, favourites, scene
  icons, colours and `show_in_multiview`, and hotkey bindings are edited by undoable edits
  and saved with the collection, so every client and agent sees the same thing
  ([R-ARCH-001](RULES.md#R-ARCH-001); JStream is single-user, so a favourite is not per
  person). Folders change no rendering, switching or nesting; scenes keep their flat
  display order, and a variant sits under its parent, never in a folder. Labels and
  favourites on a source belong to the source, so every placement shares them. A hotkey
  binds only an action, never an edit; which client listens for the keys is a client
  decision. Deleting the scene, source or output a binding's action names also removes
  that binding, in the same undoable edit, and undo restores both; `delete.plan` lists
  the bindings it will remove, and a binding is never a delete blocker
  ([ADR-0040](adr/0040-scene-structure-overrides-shared-groups-and-hotkey-targets.md)). `Scene.background` (absent
  means transparent) is scene model, not metadata, because the compositor draws it
  wherever the scene renders.
- **Filters and effects:** `Source.filters[]` belong to the source and apply wherever it
  is used. A placement owns its geometry, `crop`, `visible`, `opacity` (an alpha
  multiplier in [0, 1]) and `blend` (default `normal`; a mode is rejected until the
  compositor draws it). `SceneItem.effects[]` is reserved for per-placement effects and
  stays empty in v1, so adding them later needs no collection migration.
- **Settings:** a source's `settings`, each filter's, and each transition preset's are a
  map of values described by its kind's descriptor, which the engine reports and
  validates against. References to scenes, sources and asset files inside settings are
  typed, so the graph tracks them in cycle checks and delete rejection
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
  Unpaired items appear or disappear with their effect. The fields are reserved with
  neutral defaults while M1b builds the model; the behaviour comes before or during M2.
- The preview shown in the UI is a protocol stream, not an `Output`.
- **Master and output chains**
  ([ADR-0027](adr/0027-program-master-and-output-chains.md)): the `master` stage applies
  to every output, whichever mix and buses it takes, but not to the editing canvas or
  audio monitoring. Its video chain runs once per tick on each mix some output consumes;
  its audio chain runs on each bus that feeds an output, after the bus's own chain
  (monitors tap the bus before it). Each output may add its own chains after the master
  stage; `VirtualCam` has no audio, so it has only a video chain.
- **Encoder sharing** ([ADR-0027](adr/0027-program-master-and-output-chains.md)):
  outputs whose video chain is empty share one encoder session when they have the same
  `(video_mix, encoder_profile)`, the master stage counting as part of the mix. An
  output with a video chain, even one holding only disabled filters, gets its own render
  pass and session, keyed by its output ID. An output's own audio chain gives it its own
  audio encode and never affects video sharing. Hardware session caps differ by GPU
  vendor, so the engine reports, per output, whether it shares and with which outputs,
  plus the sessions in use. It does not guess the caps.
- **Privacy screen** ([ADR-0027](adr/0027-program-master-and-output-chains.md)): whether
  it is on is runtime state, set by an action and never stored in the collection; it
  starts off. The collection's `privacy` setting only says how it looks (a blur or a
  slate) and which sources it mutes, by default every audio input-device source,
  independently of each source's own mute. While on, it covers every output and every
  preview stream of a post-master stage.
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
  scene, with `settings` checked against the kind's descriptor and stored complete; no
  filters yet), scenes in display order, and items in stacking order with `source_id`,
  a placement, `crop` and `visible`. The placement is a `geometry` or, until 1.0, a
  legacy placement (protocol 0.3's 2D transform as written), and the collection has
  its `canvas` (protocol 0.11); non-neutral 3D geometry is still rejected. Scenes have
  a `role` (`scene` or `group`, set by `scene.create` or `scene.set_role`), and `item.move` moves an item
  to another scene keeping its id, under the same cycle check. The placement edits take
  an optional `scene_id`, accepted only when it names the item's own scene until
  variants exist (protocol 0.12). Items refer to sources; they never copy them, and
  `item.set_source` retargets one, keeping its id, placement and stacking position.
  A source copied by `source.duplicate` records its original in `duplicated_from`, a
  plain link that reads as absent once the original is deleted. Scene and source
  references in settings are dependency edges with items and nested scenes, so one
  cycle check covers them all. Deleting a source an item still places or a setting
  refers to, or a scene a source still shows or a setting refers to, is rejected. The
  built-in kinds come from `jstream-sources`. The engine runs a source of each one
  whose kind is built (`color`, `image` and `text`), rebuilt when its settings change,
  and the program mix shows its frames, animations advancing on the media clock. The
  audio-only kinds (`audio_input`, `desktop_audio`, `app_audio`) have no `Source`:
  while an output runs, the engine records each as a mixer input sent to master.
  Outputs, ordered by ID, with their `kind` and an optional `encoder_profile`: `Rtmp {
  target: Generic { url } }`, the URL checked to carry no credential, or `Record { path,
  container: mp4 }`, the path checked to be absolute and to end in a file name. A stream
  key is never collection state: it lives in the OS credential store
  ([R-SEC-001](RULES.md#R-SEC-001)). Encoder profiles, ordered by ID, each a name and an
  H.264 and AAC encoding (rate control, keyframe interval, hardware preference, audio bit
  rate), checked against the ranges the protocol states. An output with no profile uses
  the engine's default; a profile an output uses can't be deleted. The mix is always
  `Program` until video mixes exist, so outputs share a session by profile alone.
  Not yet: settings migrations between kind versions (a source saved at another
  version is refused on load), filters, source audio settings, `opacity`, `blend`, the reserved
  `effects[]`, the transition fields, 3D
  geometry, variants and overrides (so no edit drops
  any yet), folders, labels, favourites and other organisation metadata,
  `background`, hotkeys, an output's mix, buses and chains, the
  `YouTube` and `VirtualCam` kinds, and everything outside `sources`, `scenes`,
  `outputs` and `encoders`. The collection file saves everything built so
  far, encoder profiles, each output's profile and `duplicated_from` included
  ([Persistence](#persistence)).
  The program scene is built in `jstream-core` as runtime state, so the file doesn't
  hold it (no preview scene yet).

<a id="threading"></a>
## Threads, clock and sync
<!-- @section
depends_on = ["docs/adr/0014-media-clock.md", "docs/adr/0027-program-master-and-output-chains.md", "docs/adr/0028-audio-model-v2.md", "docs/adr/0032-live-edits-and-undo.md", "docs/adr/0035-clap-plugins-out-of-process.md"]
verified = { hash = "b3:6012cce52fa65794", by = "agent:claude@session_01Fdcnfj3q78X58b3kEQDyAo", note = "CL1 merge of main (#215): re-read; main's render-snapshot paragraph (render thread loads lock-free, video.snapshot reads under the lock) kept as is; this PR's CLAP hosts paragraph (shared block and mixer side built, host/restarts/stats.tick not yet, dry-signal bypass) checked against rt/host.rs" }
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
| Capture (per source) | Platform callbacks into a single-slot "latest frame" mailbox | [R-UNSAFE-005](RULES.md#R-UNSAFE-005) |
| Collector | Frees objects whose last reference was released by RT threads | [R-RT-003](RULES.md#R-RT-003). One for the program stage while any output runs, fed by its render and mixer threads (snapshots, GPU images, finished sessions' inputs), and the mixer's own for the input rings the mixer and audio threads release, and removed buses' state, filter chains and filters. |

This section is the engine-wide model. Each subsystem's runtime, as built, is described
next to its code, which that section covers: the output pipeline in
[`jstream-core`'s runtime](../crates/jstream-core/README.md#runtime) and the audio and
mixer threads in [`jstream-audio`'s real-time design](../crates/jstream-audio/README.md#realtime)
([ADR-0042](adr/0042-narrow-hot-architecture-sections.md)). Neither may contradict this
model; a change that would edits it in the same PR.

**CLAP plugin hosts** ([ADR-0035](adr/0035-clap-plugins-out-of-process.md)), M3, in progress: the shared block
(`jstream-plugin-proto`) and the mixer's side of it (`jstream-audio`) are built; the
host process, its restarts and the `stats.tick` count are not yet. Each CLAP filter
instance runs in its own `jstream-plugin-host` process, on a real-time thread registered
with MMCSS "Pro Audio" and woken by its own timer on the media clock, which both
processes read from the same OS counter. Audio crosses through shared-memory rings, and
the mixer never waits on the host: it writes block *n* and reads the processed block
*n − d* (by default *d* is two engine blocks), with no syscall or lock for the plugin
([R-RT-001](RULES.md#R-RT-001)). A late block is bypassed with a short crossfade, by
the dry signal delayed the same amount, and counted in the shared block for
`stats.tick`. The filter's latency is *d* plus the plugin's own, and latency
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

The **render snapshot** is built. After every applied batch, undo and redo, every
stored source frame and a collection file's load, the control side re-plans the program mix under the collection lock: the
reachable scenes, the frames they show and the passes that draw them. It publishes
that as the next render generation only when it draws something new, so a batch that
touches nothing on program publishes nothing. The render thread loads it without the
lock. `video.snapshot` reads it together with `rev` under the lock on the control path,
and its thread only receives the snapshot. The render thread draws through a `jstream-render`
resource cache keyed by source and by nested scene and depth. A frame that shows what
the last one showed allocates no texture: an unchanged frame is not uploaded again and
each nested scene is redrawn into the layer it had. An image no frame references is
retired, then freed once the GPU has finished the last frame that used it (a
`Queue::on_submitted_work_done` fence the cache polls without blocking). The render
thread hands those images, and each render snapshot it replaces, to the program
stage's collector thread ([R-RT-003](RULES.md#R-RT-003)). The render thread stamps its
reader slot as the handover below says. When a fatal error stops its drawing it
publishes the stopped marker too: it adopts nothing more and only repeats the frame it
last drew until teardown. The audio and outputs snapshots are not built.

**Live-value handover** (decided; the render generation and its reader slot are built,
the parameter channels and the gesture release are not): the engine learns that a real-time
thread holds a committed snapshot through a **generation counter per subsystem**. Each
time the control side publishes a subsystem's snapshot it bumps that subsystem's
generation and stores it in the snapshot. Each real-time thread that reads the snapshot
(the render thread for render, the mixer for audio) publishes the last generation it
adopted with one atomic store when it picks up a new snapshot, which allocates, locks and
waits on nothing ([R-RT-001](RULES.md#R-RT-001), [R-RT-002](RULES.md#R-RT-002)). When a
gesture's committing batch applies, the control side notes the generation of the snapshot
that carries it, and releases the gesture's live values from the parameter channels once
every thread that reads them has reported a generation at least that high. Until then the
parameter channel keeps the live value, so no frame shows the old value in between. The
control side polls the published generations; the real-time threads never wait for it.
A thread that isn't adopting snapshots counts as caught up, so a gesture's live values
aren't held while it is stopped or parked (no output running, nothing to render, no
device to mix for). Before it stops or parks it publishes a stopped marker in place of
its generation, and it shows nothing while marked. Each reader's slot starts as
the stopped marker, so a gesture committed before a thread has ever run waits on
nothing. On starting or resuming it clears the marker before it loads a snapshot, and
reads the parameter channels only after that load. So if the control side saw the
marker and released a gesture, the thread's next load comes after the committing
snapshot was published and already carries the committed value. That needs a store
then load on each side (the thread clears the marker then loads the snapshot; the
control side publishes the snapshot then reads the marker), which release and acquire
alone don't order: the marker and generation stores and reads are `SeqCst`, with a
`SeqCst` fence between each side's store and its load. A fence doesn't allocate, lock
or wait, so R-RT-001 and R-RT-002 still hold.

The privacy flag ([ADR-0027](adr/0027-program-master-and-output-chains.md)) will also
reach the render and mixer threads through a parameter channel, not a snapshot rebuild,
so it takes effect on the next tick and block. The blur or slate pass exists whenever an
output does, so turning privacy on allocates nothing on a real-time thread. Not built
yet.

<a id="protocol"></a>
## Control protocol
<!-- @section
covers = ["crates/jstream-protocol/**", "crates/jstream-control/**", "crates/jstream-server/**"]
depends_on = ["docs/adr/0013-command-model.md", "docs/adr/0006-mcp-full-access.md"]
verified = { hash = "b3:ef16d40b03157f0b", by = "agent:claude@session_01FFzJN8ifSytfHpv52HAhhR", note = "float_roundtrip r2 merge of main: text is main's (#264 verified it); this PR adds only a protocol test, nothing on number parsing" }
-->

The wire format, versioning, error codes and compatibility policy are designed in
[jstream-protocol › Design](../crates/jstream-protocol/README.md#design). In short:

- **Envelope:** JSON messages tagged by `type`. A `hello`/`welcome` handshake exchanges
  the protocol version, then each `request` (with a client-chosen `id`) gets exactly one
  `response` or `error`, and `event`s arrive at any time.
- **Edits** (`scene.create`, `item.set_transform`, `audio.set_send`, …): typed, undoable,
  always sent in an atomic batch (of one or more), which carries the optional
  `expected_rev` and idempotency key ([R-API-004](RULES.md#R-API-004)). Each applied
  batch increments `rev` by one. `item.move` moves an item between scenes keeping its
  id, `scene.set_role` (or, from protocol 0.17, `scene.create`'s optional `role`) marks a scene as a group, and placement edits take an optional
  `scene_id` so they can write a keyframe scene's override
  ([ADR-0024](adr/0024-scene-structure.md), protocol 0.12). Until keyframe scenes are
  built, `scene_id` may name only the item's own scene.
  `output.create`, `output.set_target` and `output.delete` configure the collection's
  outputs (protocol 0.7); the `record` output kind, a file on the engine's machine, is
  0.9. `canvas.set_size` and `item.set_geometry` set the canvas and an item's box
  ([ADR-0022](adr/0022-item-transform-and-canvas.md), protocol 0.11), which deprecates
  `item.set_transform`. `encoder.create`, `encoder.update` and `encoder.delete`
  configure encoder profiles, and `output.set_encoder` points an output at one or back
  at the engine's default (protocol 0.13). A profile an output uses can't be deleted.
- **Stream keys** (protocol 0.7): the actions `output.set_key` and `output.clear_key`.
  A key is not collection state: it goes to the OS credential store and is write-only,
  never in a response, event, error, query or log ([R-SEC-001](RULES.md#R-SEC-001));
  `output.status` reports only `key_set`.
- **Actions** (`output.start`, `scene.switch`, `youtube.go_live`, …): side effects,
  never undoable or batchable. A *script* runs actions in sequence and returns a result
  for each step.
- **Program scene** (protocol 0.16, [ADR-0030](adr/0030-studio-state-and-scene-switching.md)):
  the action `scene.switch { scene_id }` puts a scene on program with a cut, with no
  `rev` change. `studio.get` reads the program scene and `studio.changed` reports every
  change of it, including the ones a batch, undo or redo causes by creating the first scene or
  deleting the program scene.
- **Transitions** (planned, [ADR-0029](adr/0029-transitions.md)): edits create, update and
  delete presets (each with a kind and a settings map or patch,
  [ADR-0037](adr/0037-kind-descriptors-for-transitions-and-plugins.md)), set the default,
  and set a scene's `transition_in` and `transition_out` and an item's transition
  settings. The scene-switch action takes an optional preset.
- **Live values** (`live.set`, `live.cancel`; events `live.changed`, `live.ended`):
  actions that override live-adjustable parameters during a drag or a live dialog, with
  no `rev` bump or undo step; one batch commits the gesture
  ([ADR-0032](adr/0032-live-edits-and-undo.md)).
- **History** (`history.undo`, `history.redo`): per-client undo and redo, a fourth kind
  of call that applies a step's inverse (or the step again) as one `rev` bump
  ([ADR-0032](adr/0032-live-edits-and-undo.md)). It refuses with `undo_conflict`
  rather than overwrite another client's later change (protocol 0.10).
- **Privacy screen** (planned): the action `privacy.set { on }` on the master stage, never
  undoable or batchable and with no `rev` change; every change emits
  `privacy.changed { on }` to every client and goes to the audit log
  ([ADR-0027](adr/0027-program-master-and-output-chains.md)). A batch holds only edits,
  so one naming `privacy.set` is refused whole with the stable code `malformed`, and
  nothing in it is applied. A client that connects while privacy is on gets
  `privacy.changed { on: true }` right after its handshake, so no client shows it as
  off. Preview streams show the pre-master stage (the editing view) by
  default; a client asks for the post-master stage to see what viewers see, and only
  that stage is covered by the privacy screen.
- **Encoder sessions** (protocol 0.13): `output.status` reports, per running output,
  its encoder session's profile and the other outputs sharing it, plus the sessions and
  hardware sessions in use. A profile's settings at start count, so an output started
  after `encoder.update` gets a session of its own. Planned: when the encoder refuses a
  session, `output.start` fails with a stable session-limit error code naming the
  outputs that hold sessions, with no fallback to a software encoder
  ([ADR-0027](adr/0027-program-master-and-output-chains.md)).
- **Queries** (`state.get` → state + `rev`, the canvas included, `scene.list`, `source.list`, `item.list`,
  `output.list`, `output.status`, `encoder.list` (protocol 0.13), `video.snapshot` → the program mix as a PNG in the
  JSON result, longest side at most 1280, with the layout `rev`; `kind.list` and
  `kind.describe` (protocol 0.8) → the kinds the engine describes, each with a generated settings
  schema ([ADR-0025](adr/0025-typed-settings-kind-descriptors.md)); `delete.plan` (protocol 0.15) → the
  batch that deletes a scene or source with everything that refers to it, and the `rev`
  it was planned at; later `audio.levels`).
- **Events** (`state.changed{rev}`, `output.state_changed`, `events.dropped`, `stats.tick{seq, drops}`, …).
  `stats.tick` (protocol 0.6) carries the pipeline's running drop totals
  ([R-RT-004](RULES.md#R-RT-004)); the fields are in
  [jstream-protocol › Design](../crates/jstream-protocol/README.md#design).
- **YouTube** ([ADR-0041](adr/0041-youtube-interaction-scope-v3.md)): since protocol 0.18
  the actions `youtube.sign_in` (returns Google's consent URL; the loopback redirect
  finishes the sign-in in the engine, and no token ever crosses the protocol),
  `youtube.sign_out`, `youtube.create_broadcast`, `youtube.bind_broadcast` and
  `youtube.transition_broadcast`; the queries `youtube.status`, `youtube.list_broadcasts`
  and `youtube.list_streams` (never a stream key); the events `youtube.status_changed`
  and `youtube.health_changed` (a running YouTube output's stream health); and the RTMP
  target `youtube { broadcast_id }`, which `output.start` resolves to the bound stream's
  `rtmps://` ingest and key through the Live API. Planned for M3:
  the actions `youtube.chat.send { text }`, which posts to the bound broadcast's chat as
  the channel and goes to the audit log, and `youtube.chat.feature { message_id }`, which
  sets or clears the featured message (runtime state, reported by an event) that the chat
  message source draws as plain text. Audience stats (concurrent viewers, likes, chat
  rate) come in a low-rate `youtube.stats` event and a query.
- **Errors** carry a stable machine-readable `code`, detail fields and a message. A
  `scene.delete` or `source.delete` refused because something still refers to its
  target lists every such reference in `invalid_argument`'s `blockers` (protocol 0.15,
  [ADR-0025](adr/0025-typed-settings-kind-descriptors.md)).
- **Binary streams** (WebSocket binary frames): preview video and meters. Not designed
  yet, except the meter frame's fields
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
  default, with the token required and `Origin`/`Host` validated
  ([jstream-server › Design](../crates/jstream-server/README.md#design)). Plain `ws://`
  for now; TLS is an open question there. An upgrade with no `Origin` header is allowed,
  because the token is the gate, while an `Origin` that is present must be on the allow
  list. The same server
  serves MCP's streamable-HTTP transport at `/mcp` on the control port, behind the same
  checks, when the host turns it on ([MCP](#mcp)), and the web UI's files at every other
  path, behind the same checks too, when the host names a UI directory. A browser
  presents the token as the password of HTTP `Basic` credentials, which it then sends
  on the UI's WebSocket upgrade as well; a browser client can instead offer it as the
  subprotocol `jstream.token.<token>`.
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
verified = { hash = "b3:801a5ca4daece7b9", by = "agent:claude@session_016BkTSqJ3kAfTw3EVqkJnMr", note = "Y3 merge of main at 75a18e2 (B9 delete.plan 0.15, B10 scene.switch 0.16, B6-r 0.17 and more), Y3 now protocol 0.18: nouns add delete, studio and youtube; tools.snap measured 164260 bytes, text says about 165 KB" }
-->

- **Tools are grouped by noun**, not one per command: one tool per protocol noun
  (`canvas`, `delete`, `encoder`, `history`, `item`, `kind`, `output`, `scene`, `source`, `state`, `studio`, `video` and `youtube` so far; `audio`
  as those commands land), plus `batch` and later `script`. Each takes an `op` argument whose
  schema is generated from the protocol.
- **Events** reach agents through the `events` tool, which returns the engine's events
  after a cursor from a bounded buffer and reports any it dropped
  ([ADR-0036](adr/0036-mcp-events-polling-tool.md)). The high-rate `stats.tick` is kept
  apart: each read returns the newest tick as `stats` (running totals), so it can't
  evict the state and output events and its drop counts stay visible.
- **Schema size:** the goal is progressive discovery. Tools list only their op names, and
  `describe_api` returns detailed schemas on demand, so the agent's context stays small.
  The first cut does not do this yet: each tool carries its ops' full schemas (about 165 KB
  of `tools/list` today, as `tools.snap` stores it), and `describe_api` returns the whole protocol schema. Shrinking
  the tools is an open question in
  [jstream-mcp › Open questions](../crates/jstream-mcp/README.md#open-questions), and the
  MCP revisions and transports are in
  [jstream-mcp › Design](../crates/jstream-mcp/README.md#design).
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
verified = { hash = "b3:dc199b6560c865f5", by = "agent:claude@session_01SZXaqcgUA1wWCDzxs3hWJ6", note = "Merge of main (B3 fit modes built) into E1: dropped 'fit modes' from Still outside; E1's render_into/ResourceCache sentence and B3's geometry text both hold" }
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
which is clipped to the box, then rotated about the box centre), or a legacy 0.3
transform through `legacy_quad` (crop, scale against the current source size,
top-left at `position`, clockwise rotation about that corner). Opacity is a straight multiplier in `[0, 1]` and the blend is premultiplied
src-over. The composite pass writes premultiplied linear light into `Rgba16Float`; a
resolve pass un-premultiplies into
`Rgba8UnormSrgb`, which a snapshot can read back. The device is the platform `GpuContext`
when interop is present, and otherwise a device the crate opens with a software adapter
first. A nested scene is composited into an `Rgba16Float` layer that stays on the GPU
and is placed in its parent: `render_image` makes a new one, and `render_into` redraws
a layer the caller keeps, so a frame allocates no texture for it. The caller walks the
nesting, bounded in depth. `ResourceCache` keeps a compositor's uploads and layers from
frame to frame, keyed by the caller's IDs, and hands an image back for freeing only
after the GPU has finished the last frame that used it (see
[Threads › State to RT threads](#threading)). Still outside this crate: the render
thread (in `jstream-core`), NV12/P010 conversion (steps 4 and 5), 3D, effect packs, and
the output path below.

**Transitions** ([ADR-0029](adr/0029-transitions.md)), not built yet. A Move is not a
texture blend: during it, the program mix is a synthetic item list whose transforms and
opacity come from the transition state, drawn by a transition node that reads both
scenes' source textures. The control thread pairs the items when the switch starts and
sends the render thread a plan; the render thread only interpolates, and owns the live
interpolated state, so a switch mid-transition starts from what is on screen. Rotation,
tilt and turn take the shortest way round. The easing curves are one shared function
with golden tests. After easing, the compositor clamps values with a hard range: opacity
to `[0, 1]`, crop to `[0, source size]`, size to `≥ 0` and perspective to `> 0`; the
spring may overshoot everything else.

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

**Output path** ([ADR-0027](adr/0027-program-master-and-output-chains.md)), for each
output: mix → master video chain → the output's own video chain → privacy stage → colour
conversion (step 4) → encoder. The render plan has a master node per consumed mix, a
node per output with a video chain, and a privacy node per output. The privacy stage
comes last, so no output chain can undo it, and it fails closed: if its pass can't run,
the output gets a black frame. None of this is built yet.

<a id="browser-source"></a>
## Browser source
<!-- @section
depends_on = ["docs/adr/0021-browser-source.md", "docs/adr/0004-wgpu-with-native-interop.md"]
verified = { hash = "b3:cd6c3960b512a104", by = "agent:claude@session_01NkygLFZ2oeJS1F938ivbtc", note = "#190 review: sandbox is required where supported (bootstrap layout may replace the helper, or a superseding ADR), zip-size estimate marked unchecked and listed as open, pts wording and all three packet counts (2999/3000/2999) re-checked against the run JSON" }
-->

A browser source is a CEF off-screen browser
([ADR-0021](adr/0021-browser-source.md)), not built yet. Each `OnAcceleratedPaint`
hands over an NT handle to a D3D11 BGRA texture with premultiplied alpha, no keyed mutex
and no fence. CEF takes the texture back into its pool when the callback returns. So the
callback opens the handle on the wgpu D3D12 device, imports it through hal as in the GPU
pipeline's step 2, copies it into a texture the source owns, and waits for that copy
before it returns. The source's audio comes from CEF's audio handler as planar float PCM.

**Choices made by spike S4:**
- **Bindings:** the `cef` crate (tauri-apps/cef-rs), pinned exactly, with default
  features off. It is generated from CEF's C API and released for each CEF version. Its
  sys crate downloads the CEF binary distribution and builds CEF's C++ wrapper with CMake
  and Ninja, so it needs the lint-deps allowlist entry and governance change that
  ADR-0021 names.
- **Helper processes:** a separate small `jstream-browser-helper.exe`, set as CEF's
  `browser_subprocess_path`, so the engine exe never runs as a Chromium subprocess. Both
  layouts that ADR-0021 allows measured the same.
- **Not yet decided:** the sandbox. ADR-0021 requires it where supported. On Windows,
  CEF sandboxes subprocesses only in its bootstrap layout, where CEF's `bootstrap.exe` is
  the exe and loads the application as a DLL. S4 ran unsandboxed and didn't test that
  layout. The browser source must adopt it before it ships, which may replace the helper
  exe above, or a superseding ADR must accept running unsandboxed.

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

Still open, before the browser source is built: the CEF runtime's compressed size in
the zip, other GPU vendors and multi-GPU machines (Chromium must render on the adapter
the engine imports on), several browsers at once, pages with video, input events, and
the sandboxed layout.

<a id="persistence"></a>
## Persistence, config and secrets
<!-- @section
covers = ["crates/jstream-graph/src/persist/**", "crates/jstream-core/src/settings/**"]
depends_on = ["docs/RULES.md#R-API-003", "docs/RULES.md#R-SEC-001", "docs/adr/0025-typed-settings-kind-descriptors.md"]
verified = { hash = "b3:042c342d215b5bc4", by = "agent:claude@session_01TiAAtPXbX68C2WPpy1bKp8", note = "B6-r merge of main: rebuild passes the role on scene.create; Collections bullet holds" }
-->

- **Collections:** JSON with a `schema_version` field and forward migrations in
  `jstream-graph`, which checks a loaded collection as it checks edits. `jstream-core`
  loads the file the host names and replaces it atomically after every change.
- **Asset files** named in settings are stored relative to the collection folder or the
  app asset folder when they are under one, so a moved or portable setup still finds them.
  Each stored source and filter records its kind's descriptor version, migrated on load
  like the collection ([ADR-0025](adr/0025-typed-settings-kind-descriptors.md)). Until
  kinds ship settings migrations, a source saved at another version is refused on load.
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
verified = { hash = "b3:981e101ec9dc1d4b", by = "agent:claude@session_01D6iWjpMkoHbWLUxVnmDfxk", note = "U5: the virtual camera bullet now says the daemon is the --register-vcam mode until the desktop app has it (the app exists but lacks that mode); rest unchanged" }
-->

Windows ships as a **portable zip**: unzip and run `jstream.exe`, with no installer and no
admin ([ADR-0020](adr/0020-portable-windows-distribution.md)). The folder also holds
`jstream-daemon.exe`, `jstream-cli.exe`, the `jstream-vcam-windows` DLL, and the CEF
runtime for browser sources ([ADR-0021](adr/0021-browser-source.md)), in its own folder,
with `jstream-browser-helper.exe` for CEF's subprocesses
([ARCH › Browser source](#browser-source); the sandbox decision may replace it with
CEF's bootstrap layout).
From M3 it also holds `jstream-plugin-host.exe`, the out-of-process CLAP plugin host
([ADR-0035](adr/0035-clap-plugins-out-of-process.md)). Release builds
link the C runtime statically and need only OS components (WebView2, Media Foundation,
Direct3D 12).

- **State:** in `%APPDATA%\JStream`, or beside the exe when a `portable` marker file is
  present.
- **Virtual camera:** the app registers the COM media source from its own folder the first
  time the camera is enabled. That costs **one UAC prompt** (`jstream.exe --register-vcam`),
  because Frame Server ignores per-user registration. The app can unregister the source and
  detects a moved folder. Until the desktop app has that mode, `jstream-daemon --register-vcam` is
  that mode, and the daemon warns at startup when the registration names another folder
  ([jstream-daemon › Usage](../apps/jstream-daemon/README.md#usage)); unregistering waits
  for the app. Spike S2 (Windows 11 25H2, build 26200) found:
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
