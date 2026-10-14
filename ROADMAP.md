<!-- @doc
id = "ROADMAP"
title = "Roadmap"
kind = "plan"
status = "active"
review_every = "30d"
-->

# Roadmap

Windows first. Engine and MCP before the GUI ([ADR-0010](docs/adr/0010-headless-first-milestone.md)).
Risks are retired by spikes before anything depends on them. Each milestone ends with a demo
that runs from a clean checkout.

<a id="m0"></a>
## M0: Foundation and risk spikes
<!-- @section
depends_on = ["docs/adr/0016-docs-as-checked-graph-v2.md", "docs/adr/0019-review-gate-v2.md"]
verified = { hash = "b3:82028f4d88d590e3", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Review N1 on #26: Demo now says S1-S3 have results and S4 may finish in M1b but needs a written result before the browser source is built, matching ADR-0021's 'prove it first'; Track B S4 bullet unchanged" }
-->

Two tracks run in parallel.

**Track A: repo machinery**
- Cargo workspace, toolchain, `[workspace.lints]`, `cargo-deny`, and the required CI checks
  from [R-GIT-003](docs/RULES.md#R-GIT-003): `fmt`, `clippy`, `test`, `doc`, `deny`,
  `lint-deps`, `docs`, `governance`, `pr-title`, `gate-test`. `macos` runs but isn't required.
  (`review-gate` exists already; see [REVIEW.md](REVIEW.md).)
- `xtask docs` **MVP**: `@doc`/`@section`/`@rule` block and anchor parsing, link resolution (internal plus the
  external registry), hash-bound verification, unmatched-glob errors, the uncovered-crate
  report, generated-block drift, `docs for`, `docs verify`. Hashing ignores `uses:` pin
  changes in covered workflow files, so Dependabot bumps don't stale REVIEW#output. It
  replaces every `bootstrap` hash with a real one.
- `xtask lint-deps` (layers, `unsafe` declarations, `links=` allowlist, workspace deps),
  `xtask governance` (must encode ADR-0018's pre-approval for Dependabot pin-only PRs).
- `jstream-types`, `jstream-media`, first cut of `jstream-protocol` (edit/action
  classification, schema snapshot tests), `jstream-control`.

**Track B: technical spikes** (in `spikes/`, throwaway code; each ends with an ADR or ARCH update)
- **S1 GPU interop:** WGC capture → shared texture + fence → wgpu (D3D12) → NV12 → Media
  Foundation H.264 encode, at 1080p60 with no CPU copy. Measured on named hardware.
- **S2 Virtual camera:** Rust COM media source loaded by Frame Server, fed cross-process
  through a shared texture. Also answers whether Frame Server accepts a per-user (HKCU)
  registration, or needs a one-time elevated machine-wide one
  ([ADR-0020](docs/adr/0020-portable-windows-distribution.md)).
- **S3 Clock:** WASAPI mic + loopback timestamps mapped to QPC, drift measured over an hour,
  adaptive resampler validated.
- **S4 Browser source:** a CEF off-screen page delivered as a shared GPU texture
  through accelerated paint (transparent, 1080p60, no CPU copy) into wgpu, with its audio
  from CEF's audio handler. Also picks the Rust CEF bindings and the helper-process layout
  ([ADR-0021](docs/adr/0021-browser-source.md)).

**Demo:** CI green with all required checks. A PR that changes covered code without
re-verifying is rejected. S1–S3 each have a written result. S4 may finish in M1b, but
must have a written result before the browser source is built.

<a id="m1"></a>
## M1: Headless engine, driven by agents (Windows)
<!-- @section
depends_on = ["docs/adr/0020-portable-windows-distribution.md"]
verified = { hash = "b3:3c78f71437377d43", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "M1b source-kinds bullet now lists ADR-0031's kinds (display, window, video capture device with MJPEG and audio, image formats, color, text, audio input, desktop and application audio, browser) beside main's ADR-0023/0024/0028 bullets; M1a/c/d unchanged" }
-->

- **M1a Thin slice to YouTube:** WGC display capture → compositor (one scene) → MF H.264 +
  MF AAC (WASAPI mic + desktop loopback) → FLV → RTMPS to YouTube with a stream key. Minimal
  `jstream-core`, `jstream-server`, `jstream-mcp`, `jstream-cli`, `video.snapshot`.
- **M1b Scenes and mixing:**
  - Scene graph and source kinds
    ([ADR-0031](docs/adr/0031-source-types-and-decoding-scope.md)): display, window,
    video capture device (webcam or capture card, incl. MJPEG decode, with its audio),
    image (PNG, JPEG, WebP, animated GIF), color, text, audio input, desktop audio,
    application audio, and **browser** ([ADR-0021](docs/adr/0021-browser-source.md);
    after spike S4).
  - **Scenes as sources**: a scene placed inside another scene, with cycles rejected.
  - **Groups as scenes** ([ADR-0024](docs/adr/0024-scene-structure.md)): a scene `role`
    (`scene` or `group`), `item.move` between scenes keeping the item's id, and the
    optional `scene_id` addressing of placement edits (own scene only until keyframe
    scenes are built).
  - **Shared sources:** adding an existing source places the *same* instance (one capture,
    one set of source filters) in another scene. **Duplicate** makes an independent copy.
    An item converts between reference and clone and keeps its id
    ([ADR-0023](docs/adr/0023-placements-references-and-clones.md)).
  - Place, move, resize, rotate and **crop** scene items (2D transform + crop), with golden
    tests.
  - **Filters and effects:** source filters apply everywhere the source appears. Each
    placement has its own opacity and blend; per-placement effects are not in v1, and the
    model reserves a slot for them.
  - Bus-matrix mixer ([ADR-0028](docs/adr/0028-audio-model-v2.md)): a master bus,
    per-source and per-bus faders and mute, audio that follows the program
    (`when_shown`), filters, and peak, true-peak and loudness meters.
  - Per-client undo, `expected_rev`, audit log, recording to fMP4, and encoder sharing.
    The `Output` model reserves its video and audio chains, empty by default
    ([ADR-0027](docs/adr/0027-program-master-and-output-chains.md)).
- **M1c Virtual camera:** `jstream-vcam-windows` with in-app registration, shipped in the
  portable zip ([ADR-0020](docs/adr/0020-portable-windows-distribution.md); from spike S2).
- **M1d Hardening:** RTMP reconnect with backoff, log rotation, local minidumps, `stats.tick`,
  N-edition detection, `multiple-versions = "deny"` in `deny.toml`, the weekly `sweep` job.

**Demo:** an agent builds a two-scene setup over MCP, checks it with snapshots, starts the
virtual camera, then goes live on YouTube, all without a GUI.

<a id="m2"></a>
## M2: UI
<!-- @section
depends_on = ["docs/adr/0002-web-ui-everywhere.md"]
verified = { hash = "b3:eed1c82a752d1079", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "M2 gains ADR-0031's media file, audio waveform and audio spectrum bullet between main's ADR-0029 transitions and ADR-0024 UI-built bullets; other M2 bullets unchanged" }
-->

- Confirm Leptos, or supersede [ADR-0002](docs/adr/0002-web-ui-everywhere.md).
- `jstream-ui`: scenes, sources, properties, mixer, outputs.
- Canvas editing: **drag to place**, resize, rotate and **crop by dragging** handles
  (Alt-drag crop, OBS style), snapping, multi-select, and **add existing** versus
  **duplicate** when adding a source.
- Preview as low-res H.264 over binary WebSocket frames → WebCodecs → `<canvas>`, with a JPEG fallback.
- `jstream-desktop` (Tauri) and `jstream-daemon` serving the same UI. Token pairing for
  remote browsers.
- Studio mode (Preview/Program) and transitions
  ([ADR-0029](docs/adr/0029-transitions.md)): transition presets (Cut, Fade, Move) with a
  default, per-scene "into" and "out of" overrides, Move pairing items by source, and
  per-item delay, easing, glide and appear and disappear effects.
- The program master chain, per-output chains with their cost shown, and the privacy
  screen ([ADR-0027](docs/adr/0027-program-master-and-output-chains.md)).
- Source kinds ([ADR-0031](docs/adr/0031-source-types-and-decoding-scope.md)): **media
  file** (video or audio, looping; OS demux and decode, A/V sync on the media clock),
  **audio waveform** and **audio spectrum**.
- Built with the UI that uses them ([ADR-0024](docs/adr/0024-scene-structure.md)):
  keyframe scenes (variants with per-item overrides), scene folders, organisation
  metadata (labels, favourites, scene icons and colours, show in multiview, scene
  background colour) and hotkey bindings.

**Demo:** everything from M1, done by hand in the desktop app and in a browser.

<a id="m3"></a>
## M3: Depth and reach
<!-- @section
-->

- Full 3D transforms; WGSL effect packs with hot reload.
- `jstream-youtube`: OAuth, broadcasts, metadata, health, chat, all exposed over MCP.
- Direct NVENC/AMF encoders where measurements justify them; HEVC/AV1 via enhanced RTMP.
- WASM automation plugins.
- CLAP audio effect filters, hosted out of process
  ([ADR-0035](docs/adr/0035-clap-plugins-out-of-process.md)).
- macOS platform crate (VideoToolbox/AudioToolbox; a signed camera extension needs its own ADR).
  A Syphon receiver, the macOS follow-up to Spout2, is considered with it.
- Linux: codec-path ADR first, then the platform crate (virtual camera via v4l2loopback,
  which needs root, so it gets its own ADR too).

<a id="later"></a>
## Later
<!-- @section
-->

- Spout2 receiver source (Windows), when wanted; replay buffer.
- Virtual microphone (needs a kernel-driver ADR), SRT/WHIP outputs, OBS scene-collection import.
