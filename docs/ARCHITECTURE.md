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
verified = { hash = "b3:e706591283751343", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Covered apps/ READMEs changed only by their re-verified Purpose lines (crate-map's jstream-sources row moved to ADR-0031); layer diagram and app roles unchanged" }
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
| `jstream-daemon` | Headless engine. Serves the UI, the WebSocket API and MCP over HTTP. Can start from a collection file (`--collection`). The first cut serves the WebSocket API only ([its README](../apps/jstream-daemon/README.md#purpose)). |
| `jstream-cli` | Lightweight remote client: drives a running engine, and `jstream-cli mcp` runs a **stdio** MCP proxy to it. Does not link the engine. |

<a id="principles"></a>
## Core principles
<!-- @section
depends_on = ["docs/adr/0001-library-first-engine.md", "docs/adr/0031-source-types-and-decoding-scope.md", "docs/adr/0013-command-model.md"]
verified = { hash = "b3:67a80aa8d2b1bb42", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "depends_on moved from superseded ADR-0003 to ADR-0031; the 8 principles cite no codec specifics and still hold" }
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
verified = { hash = "b3:c7717535ca16d975", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "PR #54 B1: added design/ (UI design references, prototype source; not normative); rest of the tree unchanged and still matches the repo and xtask command set" }
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
│   ├── adr/                # NNNN-title.md (+ TEMPLATE.md)
│   └── guides/
└── .github/                # workflows (CI, governance, review-gate), Dependabot config, PR template
```

<a id="crate-map"></a>
## Crate map
<!-- @section
covers = ["crates/*/Cargo.toml", "apps/*/Cargo.toml"]
depends_on = ["docs/RULES.md#R-ARCH-002", "docs/RULES.md#R-ARCH-003"]
verified = { hash = "b3:477a5e1a4581d82a", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "This PR rewrites the jstream-sources row for ADR-0031 (kinds by milestone, images and fonts via pure-Rust crates) beside main's ADR-0035 plugin rows; other rows match main; checked against each crate's [package.metadata.jstream]" }
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
| 0 | `jstream-plugin-proto` | Planned for M3 ([ADR-0035](adr/0035-clap-plugins-out-of-process.md)): the versioned `repr(C)` shared-memory contract (audio rings, parameter-event ring, status block) between the mixer and the CLAP plugin host | no |
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
| 2 | `jstream-plugin-clap` | Planned for M3 ([ADR-0035](adr/0035-clap-plugins-out-of-process.md)): safe wrapper over the CLAP plugin ABI. Only `jstream-plugin-host` links it. | **yes** |
| 2 | `jstream-vcam-windows` | `cdylib` COM media source loaded by Frame Server ([ADR-0020](adr/0020-portable-windows-distribution.md)). Depends only on L0 and `windows`. | **yes** |
| 3 | `jstream-encode` | Shared encoder sessions keyed by (mix, settings); rate control; fan-out of packet streams | no |
| 3 | `jstream-mcp` | MCP server over `dyn Control`: generated grouped tools, stdio and streamable-HTTP transports | no |
| 3 | `jstream-plugin-wasm` | wasmtime host for automation plugins (calls the API through `Control`) | no |
| 4 | `jstream-server` | axum HTTP/WebSocket over `dyn Control`; serves UI assets; mounts MCP over HTTP; token and Origin checks | no |
| 4 | `jstream-sources` | `Source` trait plus built-ins ([ADR-0031](adr/0031-source-types-and-decoding-scope.md)): display, window, video capture device, image, color, text, browser, audio input, desktop audio, application audio, scene (M1b); media file, audio waveform, audio spectrum (M2); Spout2 receiver (later). Uses platform traits; backends are injected. Images and fonts use pure-Rust crates. | no |
| 4 | `jstream-outputs` | `Output` trait; RTMP(S) client with reconnect, recording, vcam output. Uses `encode`, `mux` and platform traits. | no |
| 5 | `jstream-core` | `Engine`: executes commands, owns threads and the clock, injects platform backends, implements `InProcessControl` | no |
| — | `jstream-ui` | Leptos app compiled to WASM; depends on `jstream-protocol` and `jstream-control` only | no |
| 6 | `apps/*` | `jstream-desktop` and `jstream-daemon` (link core and server); `jstream-cli` (control and mcp only; test-only dev-dependencies, such as server for its end-to-end test, are outside this); `jstream-plugin-host`, the out-of-process CLAP host, planned for M3 ([ADR-0035](adr/0035-clap-plugins-out-of-process.md)) | no |

Splitting rule: start with these crates. Split one only when compile time, `unsafe`
isolation or a dependency boundary requires it, and record the split in an ADR
([R-ARCH-005](RULES.md#R-ARCH-005)).

<a id="data-model"></a>
## Data model
<!-- @section
covers = ["crates/jstream-graph/src/model/**"]
depends_on = ["docs/adr/0022-item-transform-and-canvas.md", "docs/adr/0023-placements-references-and-clones.md", "docs/adr/0024-scene-structure.md", "docs/adr/0025-typed-settings-kind-descriptors.md", "docs/adr/0027-program-master-and-output-chains.md", "docs/adr/0028-audio-model-v2.md"]
verified = { hash = "b3:e45efcf5eba44a05", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Joined ADR-0028 (Source.audio and SourceAudio, master bus, bus fader/mute/monitor, Audio and Shown-on-program bullets; 'independent of scenes' dropped) with main's ADR-0022 canvas/geometry, ADR-0024 scene structure and ADR-0025 Settings; depends_on lists 0022-0025 and 0028, not the superseded 0007; not-yet list adds source audio settings" }
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
├── studio         { program_scene, preview_scene }   (studio-mode state)
├── transitions    { presets: [TransitionPreset], default }
│                  TransitionPreset { id, name, kind: Cut | Fade | Move, duration_ms, easing, unmatched, stagger_ms }
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
                   kind: Rtmp { target: Generic { url } | YouTube { broadcast_binding } } | Record { path, container } | VirtualCam
```

- `Program` and `Preview` are mixes driven by the studio state. `Program` shows
  `program_scene`, including any transition in progress. A `VideoMix` with input
  `Program` follows it.
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
  that would close a cycle (`item.add`, and `item.set_source`).
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
- **Folders and organisation are collection state:** folders, labels, favourites, scene
  icons, colours and `show_in_multiview`, and hotkey bindings are edited by undoable
  edits and saved with the collection, so every client and agent sees the same thing
  ([R-ARCH-001](RULES.md#R-ARCH-001); JStream is single-user, so a favourite is not
  per person). Folders change no rendering, switching or nesting; scenes keep their flat
  display order, and a variant sits under its parent, never in a folder. Labels and
  favourites on a source belong to the source, so every placement shares them. A hotkey
  binds only an action, never an edit; which client listens for the keys is a client
  decision. `Scene.background` (absent means transparent) is scene model, not metadata,
  because the compositor draws it wherever the scene renders.
- **Filters and effects:** `Source.filters[]` belong to the source and apply wherever it
  is used. A placement owns its geometry, `crop`, `visible`, `opacity` (an alpha
  multiplier in [0, 1]) and `blend` (default `normal`; a mode is rejected until the
  compositor draws it). `SceneItem.effects[]` is reserved for per-placement effects and
  stays empty in v1, so adding them later needs no collection migration.
- **Settings:** a source's `settings`, and each filter's, are a map of values described by
  its kind's descriptor, which the engine reports and validates against. References to
  scenes, sources and asset files inside settings are typed, so the graph tracks them in
  cycle checks and delete rejection ([ADR-0025](adr/0025-typed-settings-kind-descriptors.md)).
- **Transitions** ([ADR-0029](adr/0029-transitions.md)): exactly one preset is the
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
- **Built so far** (`jstream-graph`): sources (`kind`: a builtin kind name or a nested
  scene; no settings or filters yet), scenes in display order, and items in stacking
  order with `source_id`, protocol 0.3's 2D transform (top-left corner and scale, which
  ADR-0022 replaces), `crop` and `visible`. Items refer to sources; they never copy them.
  Deleting a source an item still places, or a scene a source still shows, is rejected.
  Not yet: settings, filters, source audio settings, `opacity`, `blend`, the reserved
  `effects[]`, `duplicated_from`, `item.set_source`, the transition fields, the canvas,
  `geometry`, scene `role` and `item.move`, the `scene_id` addressing of placement edits,
  variants and overrides, folders, labels, favourites and other organisation metadata,
  `background`, hotkeys, and everything outside `sources` and `scenes`. Nothing is
  persisted yet.

<a id="threading"></a>
## Threads, clock and sync
<!-- @section
covers = ["crates/jstream-core/src/runtime/**", "crates/jstream-audio/src/rt/**", "crates/jstream-render/src/frame_loop.rs"]
depends_on = ["docs/adr/0014-media-clock.md", "docs/adr/0028-audio-model-v2.md"]
verified = { hash = "b3:eede437e183b3eb5", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "This PR edits the Mixer row (activity gains, keys in audio-graph order, latency compensation), adds the Audio timestamps paragraph (a block's pts is its input's media time; delay filter is a stamped shift) and ADR-0028 to depends_on; main's ADR-0035 CLAP host paragraph (latency d plus the plugin's own, compensated) agrees with it; none built beyond the first-cut mixer" }
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
| Collector | Frees objects whose last reference was released by RT threads | [R-RT-003](RULES.md#R-RT-003) |

**CLAP plugin hosts** ([ADR-0035](adr/0035-clap-plugins-out-of-process.md)), planned for M3 and not built: each CLAP filter
instance runs in its own `jstream-plugin-host` process, on a real-time thread registered
with MMCSS "Pro Audio" and woken by its own timer on the media clock, which both
processes read from the same OS counter. Audio crosses through shared-memory rings, and
the mixer never waits on the host: it writes block *n* and reads the processed block
*n − d* (by default *d* is two engine blocks), with no syscall or lock for the plugin
([R-RT-001](RULES.md#R-RT-001)). A late block is bypassed with a short crossfade and
counted in `stats.tick`. The filter's latency is *d* plus the plugin's own, and latency
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

**In the engine:** `jstream-audio` runs S3's drift fit and PI loop with S3's tuning on
each input's audio thread, resampling into a per-input ring indexed by engine frame
that the mixer reads on the engine clock
([its real-time design](../crates/jstream-audio/README.md#realtime)). It differs from
what S3 measured in two ways: its own windowed-sinc resampler replaces rubato, and the
timing error is absolute, so inputs line up with each other. That combination has been
simulated (within 0.05 ms after settling), not yet measured on hardware.

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
verified = { hash = "b3:8cbe42013fc01956", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Covered control/core/server/protocol READMEs changed only by re-verified Purpose lines after the ADR-0031 crate-map row; no protocol text changes in this PR; bullets match protocol 0.3 as built" }
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
  id, and placement edits take an optional `scene_id` so they can write a keyframe
  scene's override ([ADR-0024](adr/0024-scene-structure.md); not built yet).
- **Actions** (`output.start`, `studio.transition`, `youtube.go_live`, …): side effects,
  never undoable or batchable. A *script* runs actions in sequence and returns a result
  for each step.
- **Transitions** (planned, [ADR-0029](adr/0029-transitions.md)): edits create, update
  and delete presets, set the default, and set a scene's `transition_in` and
  `transition_out` and an item's transition settings. The scene-switch action takes an
  optional preset.
- **Live values** (`live.set`, `live.cancel`; events `live.changed`, `live.ended`):
  actions that override live-adjustable parameters during a drag or a live dialog, with
  no `rev` bump or undo step; one batch commits the gesture
  ([ADR-0032](adr/0032-live-edits-and-undo.md)).
- **History** (`history.undo`, `history.redo`): per-client undo and redo, a fourth kind
  of call that applies a step's inverse (or the step again) as one `rev` bump
  ([ADR-0032](adr/0032-live-edits-and-undo.md)).
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
- **Encoder sessions** (planned): `output.status` reports, per output, whether it shares
  an encoder session and with which outputs, plus the hardware sessions in use. When the
  encoder refuses a session, `output.start` fails with a stable session-limit error code
  naming the outputs that hold sessions, with no fallback to a software encoder
  ([ADR-0027](adr/0027-program-master-and-output-chains.md)).
- **Queries** (`state.get` → state + `rev`, `scene.list`, `source.list`, `item.list`,
  `output.status`; later `video.snapshot`, `audio.levels`).
- **Events** (`state.changed{rev}`, `output.state_changed`, `stats.tick`, …).
- **Errors** carry a stable machine-readable `code`, detail fields and a message.
- **Binary streams** (WebSocket binary frames): snapshots (PNG/JPEG), preview video,
  meters. Not designed yet, except the meter frame's fields
  ([ADR-0028](adr/0028-audio-model-v2.md)): per channel, sample peak, RMS and true peak
  (4× oversampled, BS.1770); per bus, also momentary, short-term and integrated loudness
  (LUFS). Loudness is computed only for the master bus and for subscribed buses and
  sources. Integrated loudness is reset by the action `audio.reset_loudness`.
- **Schema:** the JSON Schema is generated from the Rust types, and its snapshot is
  the API review ([R-TEST-002](RULES.md#R-TEST-002)).
- **Transport:** the `Control` trait is implemented in-process by core
  (`InProcessControl`, [jstream-core › Design](../crates/jstream-core/README.md#design))
  and over WebSocket by `jstream-control` (`WsControl`, feature `ws`). `jstream-server`
  serves the WebSocket API at `/api/v1/ws` in front of any `dyn Control`, on `127.0.0.1` by
  default, with the token required and `Origin`/`Host` validated
  ([jstream-server › Design](../crates/jstream-server/README.md#design)). Plain `ws://`
  for now; TLS is an open question there.
- **Versioning:** [R-API-001](RULES.md#R-API-001).

<a id="mcp"></a>
## MCP
<!-- @section
covers = ["crates/jstream-mcp/**"]
depends_on = ["docs/adr/0006-mcp-full-access.md", "docs/RULES.md#R-API-002"]
verified = { hash = "b3:e0616afe224f0007", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Covered jstream-mcp and CLI READMEs changed only by re-verified Purpose lines after the ADR-0031 crate-map row; MCP grouping, schema size and naming bullets unchanged" }
-->

- **Tools are grouped by noun**, not one per command: one tool per protocol noun
  (`item`, `output`, `scene`, `source` and `state` so far; `audio`, `studio`, `youtube`
  as those commands land), plus `batch` and later `script`. Each takes an `op` argument whose
  schema is generated from the protocol.
- **Schema size:** the goal is progressive discovery. Tools list only their op names, and
  `describe_api` returns detailed schemas on demand, so the agent's context stays small.
  The first cut does not do this yet: each tool carries its ops' full schemas (about 50 KB
  of `tools/list` today), and `describe_api` returns the whole protocol schema. Shrinking
  the tools is an open question in
  [jstream-mcp › Open questions](../crates/jstream-mcp/README.md#open-questions), and the
  MCP revisions and transports are in
  [jstream-mcp › Design](../crates/jstream-mcp/README.md#design).
- **Names** follow the tool-name pattern in [R-API-002](RULES.md#R-API-002).
- **Snapshots** are returned as images, capped in size (default longest side 1280 px), with
  the state `rev` they show.
- **Access:** full access ([ADR-0006](adr/0006-mcp-full-access.md)), always authenticated.
  The stdio proxy (`jstream-cli mcp`) reads the token from the user-only token file. The
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
verified = { hash = "b3:7d384e016afe5a8e", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Merged main (ADR-0029 transitions) into the ADR-0027 acceptance: ROADMAP M2 keeps the spelled-out transitions bullet and the master/output chains/privacy bullet; ARCH data-model, protocol and GPU pipeline carry both ADRs' additions; built-crate text unchanged" }
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

<a id="persistence"></a>
## Persistence, config and secrets
<!-- @section
covers = ["crates/jstream-graph/src/persist/**", "crates/jstream-core/src/settings/**"]
depends_on = ["docs/RULES.md#R-API-003", "docs/RULES.md#R-SEC-001", "docs/adr/0025-typed-settings-kind-descriptors.md"]
verified = { hash = "b3:0e05e58970c4134c", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "ADR-0025 dependency now links ADR-0032 for live hints (review N2); data-model Settings bullet and persistence Asset files bullet unaffected" }
-->

- **Collections:** JSON with a `schema_version` field and forward migrations in
  `jstream-graph`.
- **Asset files** named in settings are stored relative to the collection folder or the
  app asset folder when they are under one, so a moved or portable setup still finds them.
  Each stored source and filter records its kind's descriptor version, migrated on load
  like the collection ([ADR-0025](adr/0025-typed-settings-kind-descriptors.md)).
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
verified = { hash = "b3:a4757b3070f69873", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Edited per ADR-0035: the zip gains jstream-plugin-host.exe from M3; portable layout, state location and S2 vcam registration findings unchanged" }
-->

Windows ships as a **portable zip**: unzip and run `jstream.exe`, with no installer and no
admin ([ADR-0020](adr/0020-portable-windows-distribution.md)). The folder also holds
`jstream-daemon.exe`, `jstream-cli.exe`, the `jstream-vcam-windows` DLL, and the CEF
runtime for browser sources ([ADR-0021](adr/0021-browser-source.md)), in its own folder.
From M3 it also holds `jstream-plugin-host.exe`, the out-of-process CLAP plugin host
([ADR-0035](adr/0035-clap-plugins-out-of-process.md)). Release builds
link the C runtime statically and need only OS components (WebView2, Media Foundation,
Direct3D 12).

- **State:** in `%APPDATA%\JStream`, or beside the exe when a `portable` marker file is
  present.
- **Virtual camera:** the app registers the COM media source from its own folder the first
  time the camera is enabled. That costs **one UAC prompt** (`jstream.exe --register-vcam`),
  because Frame Server ignores per-user registration. The app can unregister the source and
  detects a moved folder. Spike S2 (Windows 11 25H2, build 26200) found:
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
