# Roadmap

Windows first. Engine and MCP before GUI (ADR-0010). Each milestone ends with a
demo that runs from a clean checkout.

## M0: Foundation
- Cargo workspace, toolchain pin, lints, `cargo-deny`, CI on Windows/Linux/macOS
- `xtask`: `lint-deps`, `gen-schema`, `golden`
- `jstream-types`, `jstream-media`, first cut of `jstream-protocol` (+ schema snapshot tests)
- **Demo:** CI green, `cargo xtask gen-schema` prints the API schema

## M1: Headless engine driven by agents (Windows)
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
- `jstream-ui` (Leptos→WASM): scenes, sources, properties, transform gizmo, mixer, outputs
- Low-latency preview stream into `<canvas>`
- `jstream-desktop` (Tauri, engine in-process) and `jstream-daemon` serving the same UI
- Studio mode (Preview/Program) + transitions
- **Demo:** do everything from M1 by hand in the desktop app and in a browser

## M3: Depth and reach
- Full 3D transforms (perspective, rotation about X/Y), effect packs (WGSL + manifest, hot reload)
- `jstream-youtube`: OAuth, create/schedule broadcasts, title/description, health, chat → MCP tools
- HEVC/AV1 via enhanced RTMP
- WASM automation plugins
- macOS and Linux platform crates reach M1 parity

## Later
- Media file source, browser source (needs an ADR: embedded webview vs CEF-free approach)
- Replay buffer, multi-output (simultaneous stream + record at different settings)
- SRT / WHIP outputs, OBS scene-collection import
