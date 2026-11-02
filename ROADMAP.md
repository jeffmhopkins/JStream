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
  `xtask governance` (must encode ADR-0056's pre-approval for Dependabot pin-only PRs).
- `jstream-types`, `jstream-media`, first cut of `jstream-protocol` (edit/action
  classification, schema snapshot tests), `jstream-control`.

**Track B: technical spikes** (in `spikes/`, throwaway code; each ends with an ADR or ARCH update)
- **S1 GPU interop:** WGC capture → shared texture + fence → wgpu (D3D12) → NV12 → Media
  Foundation H.264 encode, at 1080p60 with no CPU copy. Measured on named hardware.
- **S2 Virtual camera:** Rust COM media source loaded by Frame Server, fed cross-process
  through a shared texture. Also answers whether Frame Server accepts a per-user (HKCU)
  registration, or needs a one-time elevated machine-wide one (ADR-0020, since superseded
  by [ADR-0066](docs/adr/0066-portable-windows-distribution-v2.md)).
- **S3 Clock:** WASAPI mic + loopback timestamps mapped to QPC, drift measured over an hour,
  adaptive resampler validated.
- **S4 Browser source:** a CEF off-screen page delivered as a shared GPU texture
  through accelerated paint (transparent, 1080p60, no CPU copy) into wgpu, with its audio
  from CEF's audio handler. Also picks the Rust CEF bindings and the helper-process layout
  ([ADR-0021](docs/adr/0021-browser-source.md)).

**Demo:** CI green with all required checks. A PR that changes covered code without
re-verifying is rejected. S1–S4 each have a written result (S4 was allowed to finish in M1b,
but needed one before the browser source is built).

<a id="m1"></a>
## M1: Headless engine, driven by agents (Windows)
<!-- @section
depends_on = ["docs/adr/0061-source-types-and-decoding-scope-v2.md", "docs/adr/0066-portable-windows-distribution-v2.md"]
-->

- **M1a Thin slice to YouTube:** WGC display capture → compositor (one scene) → MF H.264 +
  MF AAC (WASAPI mic + desktop loopback) → FLV → RTMPS to YouTube with a stream key. Minimal
  `jstream-core`, `jstream-server`, `jstream-mcp`, `jstream-cli`, `video.snapshot`.
- **M1b Scenes and mixing:**
  - Scene graph and source kinds
    ([ADR-0061](docs/adr/0061-source-types-and-decoding-scope-v2.md)): display, window,
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
    placement has its own opacity and blend. Per-placement effects (item effects: a
    video filter chain per placement,
    [ADR-0065](docs/adr/0065-scene-chains-and-item-effects.md)) are in v1
    ([ADR-0068](docs/adr/0068-item-effects-in-v1.md)), built after the video chain
    runtime, and the model reserves their slot.
  - Bus-matrix mixer ([ADR-0028](docs/adr/0028-audio-model-v2.md)): a master bus,
    per-source and per-bus faders and mute, audio that follows the program
    (`when_shown`), filters, and peak, true-peak and loudness meters.
  - Per-client undo, `expected_rev`, audit log, recording to fMP4, and encoder sharing.
    The `Output` model reserves its video and audio chains, empty by default
    ([ADR-0080](docs/adr/0080-program-master-and-output-chains-v2.md)).
- **M1c Virtual camera:** `jstream-vcam-windows` with in-app registration, shipped in the
  portable zip ([ADR-0066](docs/adr/0066-portable-windows-distribution-v2.md); from spike S2).
- **M1d Hardening:** RTMP reconnect with backoff, log rotation, local minidumps, `stats.tick`,
  N-edition detection, `multiple-versions = "deny"` in `deny.toml`, the weekly `sweep` job.

**Demo:** an agent builds a two-scene setup over MCP, checks it with snapshots, starts the
virtual camera, then goes live on YouTube, all without a GUI.

<a id="m2"></a>
## M2: UI
<!-- @section
depends_on = ["docs/adr/0045-leptos-web-ui.md"]
-->

- Leptos confirmed ([ADR-0045](docs/adr/0045-leptos-web-ui.md)).
- `jstream-ui`: scenes, sources, properties, mixer, outputs.
- Canvas editing: **drag to place**, resize, rotate and **crop by dragging** handles
  (Alt-drag crop, OBS style), snapping, multi-select, and **add existing** versus
  **duplicate** when adding a source.
- Preview as low-res H.264 over binary WebSocket frames → WebCodecs → `<canvas>`, with a JPEG fallback.
- `jstream-desktop` (Tauri) and `jstream-daemon` serving the same UI. Token pairing for
  remote browsers.
- Studio mode (Preview/Program) and transitions
  ([ADR-0029](docs/adr/0029-transitions.md)): transition presets (Cut, Fade, Move, the
  first transition kinds with descriptors and settings,
  [ADR-0037](docs/adr/0037-kind-descriptors-for-transitions-and-plugins.md)) with a
  default, per-scene "into" and "out of" overrides, Move pairing items by source, and
  per-item delay, easing, glide and appear and disappear effects.
- The program master chain and per-output chains with their cost shown
  ([ADR-0080](docs/adr/0080-program-master-and-output-chains-v2.md)).
- Source kinds ([ADR-0061](docs/adr/0061-source-types-and-decoding-scope-v2.md)): **media
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
- YouTube with no required OAuth client: a stream key with Studio's auto-start, and chat
  through a browser-source preset. Signed in through JStream's own OAuth client, `jstream-youtube`
  adds broadcasts, metadata, health, chat, sending chat as the channel, a featured chat
  message shown on stream by a chat message source, and audience stats, for people in the
  UI only, not agents ([ADR-0058](docs/adr/0058-youtube-sign-in-with-a-jstream-client.md)).
- Direct NVENC/AMF encoders where measurements justify them; HEVC/AV1 via enhanced RTMP.
- WASM automation plugins.
- CLAP audio effect filters, hosted out of process
  ([ADR-0035](docs/adr/0035-clap-plugins-out-of-process.md)), as plugin kinds whose
  parameters and opaque saved state are the filter's settings
  ([ADR-0037](docs/adr/0037-kind-descriptors-for-transitions-and-plugins.md)). A
  `state` may be 2 MiB, so a batch carrying two can pass the server's 4 MiB request
  limit. [ADR-0074](docs/adr/0074-too-large-requests.md) (issue #102) amends ADR-0037's
  `state` clause: such a request, read below a 16 MiB frame limit, is refused with a
  stable `too_large` error and the connection stays open, so an oversized batch fails
  cleanly (protocol 0.62).
- macOS: the platform crate per [ADR-0063](docs/adr/0063-macos-platform-support.md)
  (ScreenCaptureKit capture, Core Audio with process taps, VideoToolbox and AudioToolbox,
  the Keychain), on macOS 14 and later, Apple Silicon supported and Intel best effort.
  Signing and notarisation, and the virtual camera as a CMIO camera extension, come later,
  the camera in its own ADR. A Syphon receiver, the macOS follow-up to Spout2, is its own
  item when wanted.
- Linux: the platform crate per [ADR-0060](docs/adr/0060-linux-platform-support.md)
  (portal and PipeWire capture, VA-API encode). Per
  [ADR-0061](docs/adr/0061-source-types-and-decoding-scope-v2.md), software H.264 is Cisco's
  OpenH264 binary, downloaded on the user's request, and AAC is an AAC-LC encoder in Rust,
  built once its own patent-check ADR is accepted. The virtual camera via v4l2loopback
  needs root, so it gets its own ADR.

<a id="endgame"></a>
## Endgame: what done means
<!-- @section
depends_on = ["docs/adr/0055-wave-branches.md#decision"]
-->

The Rust implementation is **done** when every item of M0 to M3 in
[`docs/progress.toml`](docs/progress.toml) is `done`: merged on `main`, with its Windows
run passed where it needs one. Then each milestone's demo above runs from a clean checkout.
An item leaves M3 only when the maintainer moves it to [Later](#later). The README's
[What's left](README.md#progress) table lists every unfinished item with the steps it still
needs, from that file.

The remaining work lands in this order. A step doesn't wait for the one before it to
finish: builds, runs and decisions go on side by side, and the order is what lands first.

1. **Land the open waves** in milestone order, M1, then M2, then M3
   ([RUNBOOK-WAVES › Landing](docs/runbooks/waves.md#landing)). Items marked *build*
   finish through review into their wave. Items marked *its wave lands* are already in
   one.
2. **Windows runs.** Every item marked *Windows run* passes its run on Windows hardware,
   which only the maintainer can do. Items that a later build changes run again.
3. **Maintainer decisions and actions**, for items marked *maintainer*:
   - the Google Cloud project and OAuth client for YouTube sign-in
     ([ADR-0058](docs/adr/0058-youtube-sign-in-with-a-jstream-client.md)). Until then
     the daemon's sign-in and the signed-in YouTube features stay hidden;
   - which platforms besides Windows are supported (maintainer tier): Linux first, and
     whether macOS stays in M3 or moves to Later.
4. **Platform crates**: Linux, after its codec-path ADR, and macOS, if it stays, after
   its own ADR ([M3](#m3)).
5. **Release check.** With every item `done`, run the M1, M2 and M3 demos from a clean
   checkout. Whether that becomes a first release, 1.0, is the maintainer's call: 1.0
   removes the deprecated protocol forms
   ([ADR-0022](docs/adr/0022-item-transform-and-canvas.md)), a public compatibility
   promise.

<a id="later"></a>
## Later
<!-- @section
-->

- Spout2 receiver source (Windows), when wanted; replay buffer.
- Virtual microphone (needs a kernel-driver ADR), SRT/WHIP outputs, OBS scene-collection import.
