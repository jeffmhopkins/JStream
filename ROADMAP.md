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
verified = { hash = "b3:0d73b3ac048bd6ca", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Edited: gate-test added to the required-check list (R-GIT-003); uses:-pin-insensitive hashing moved from the governance item to the docs MVP item; tracks A/B otherwise unchanged" }
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
  through a shared texture. Also answers whether MSIX can register it, and what registration
  needs on dev machines.
- **S3 Clock:** WASAPI mic + loopback timestamps mapped to QPC, drift measured over an hour,
  adaptive resampler validated.

**Demo:** CI green with all required checks. A PR that changes covered code without
re-verifying is rejected. S1–S3 each have a written result.

<a id="m1"></a>
## M1: Headless engine, driven by agents (Windows)
<!-- @section
depends_on = ["docs/adr/0003-media-stack-and-codecs.md", "docs/adr/0012-virtual-camera-windows.md"]
verified = { hash = "b3:ed1273cced3d5218", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Re-read: M1a-M1d thin slice, scenes/mixing, vcam + MSI, hardening still match ADR-0003 codec baseline and ADR-0012 vcam design" }
-->

- **M1a Thin slice to YouTube:** WGC display capture → compositor (one scene) → MF H.264 +
  MF AAC (WASAPI mic + desktop loopback) → FLV → RTMPS to YouTube with a stream key. Minimal
  `jstream-core`, `jstream-server`, `jstream-mcp`, `jstream-cli`, `video.snapshot`.
- **M1b Scenes and mixing:** scene graph, sources (window, webcam incl. MJPEG decode, image,
  color, text), 2D transform + crop, golden tests; bus-matrix mixer with filters and meters;
  per-client undo, `expected_rev`, audit log; recording to fMP4; encoder sharing.
- **M1c Virtual camera:** `jstream-vcam-windows` + signed MSI installer (from spike S2).
- **M1d Hardening:** RTMP reconnect with backoff, log rotation, local minidumps, `stats.tick`,
  N-edition detection, `multiple-versions = "deny"` in `deny.toml`, the weekly `sweep` job.

**Demo:** an agent builds a two-scene setup over MCP, checks it with snapshots, starts the
virtual camera, then goes live on YouTube, all without a GUI.

<a id="m2"></a>
## M2: UI
<!-- @section
depends_on = ["docs/adr/0002-web-ui-everywhere.md"]
verified = { hash = "b3:4e872137e6946ac1", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Re-read: Leptos confirmation, preview transport (WebCodecs + JPEG fallback), Tauri/daemon and studio mode still match ADR-0002" }
-->

- Confirm Leptos, or supersede [ADR-0002](docs/adr/0002-web-ui-everywhere.md).
- `jstream-ui`: scenes, sources, properties, transform gizmo, mixer, outputs.
- Preview as low-res H.264 over binary WebSocket frames → WebCodecs → `<canvas>`, with a JPEG fallback.
- `jstream-desktop` (Tauri) and `jstream-daemon` serving the same UI. Token pairing for
  remote browsers.
- Studio mode (Preview/Program) and transitions.

**Demo:** everything from M1, done by hand in the desktop app and in a browser.

<a id="m3"></a>
## M3: Depth and reach
<!-- @section
-->

- Full 3D transforms; WGSL effect packs with hot reload.
- `jstream-youtube`: OAuth, broadcasts, metadata, health, chat, all exposed over MCP.
- Direct NVENC/AMF encoders where measurements justify them; HEVC/AV1 via enhanced RTMP.
- WASM automation plugins.
- macOS platform crate (VideoToolbox/AudioToolbox; a signed camera extension needs its own ADR).
- Linux: codec-path ADR first, then the platform crate (virtual camera via v4l2loopback,
  which needs root, so it gets its own ADR too).

<a id="later"></a>
## Later
<!-- @section
-->

- Media-file source, browser source (needs an ADR), replay buffer.
- Virtual microphone (needs a kernel-driver ADR), SRT/WHIP outputs, OBS scene-collection import.
