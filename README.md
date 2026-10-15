<!-- @doc
id = "README"
title = "JStream"
kind = "normative"
status = "active"
review_every = "90d"
-->

# JStream

A live-streaming and compositing engine written in Rust: a ground-up rewrite in the spirit of
OBS, built to be driven equally well by a human or an AI agent.

- **Stream** to YouTube (RTMPS + YouTube Live API) or any RTMP target
- **Virtual camera** (an OS-native Windows camera that every app can see)
- **Scenes, sources, effects**, 3D transforms and crop on a wgpu compositor
- **Independent video and audio routing**: outputs are not tied to scenes, and audio runs
  through a bus-matrix mixer
- **Full agentic control over MCP**: anything the UI can do, an agent can do
- **Headless or desktop**: the engine is a library, embedded in a Tauri app or run as a
  daemon that serves the same web UI to any browser

JStream is built to be driven by a person or by an AI agent. The engine is a library with
one protocol; the web UI, the desktop app, the CLI and MCP are all clients of it, so
nothing the UI can do is out of an agent's reach
([ADR-0001](docs/adr/0001-library-first-engine.md),
[ADR-0006](docs/adr/0006-mcp-full-access.md)). Windows comes first, and the headless
engine and MCP come before the GUI ([ADR-0010](docs/adr/0010-headless-first-milestone.md)).

<a id="today"></a>
## What works today
<!-- @section
-->

M0 is done apart from spike S4's Windows run, M1 (the headless engine) is most of the way there, and M2 (the UI) has
started. Item by item status is in [Progress](#progress); the plan is in
[ROADMAP.md](ROADMAP.md).

- **Going live from the headless daemon on Windows.** `jstream-daemon` captures the
  display, composites the program scene on the GPU (wgpu), encodes H.264 and AAC with
  Media Foundation, and publishes FLV over RTMP or RTMPS to any server, YouTube's ingest
  included, reconnecting with backoff. Stream keys live in Windows Credential Manager,
  never in the collection or the logs. The [go-live runbook](docs/runbooks/go-live.md)
  walks through it, and `scripts/smoke.ps1` runs the same steps unattended.
- **Agent and script control.** The daemon serves an authenticated WebSocket API and MCP
  over streamable HTTP; `jstream-cli` drives it from a shell, and `jstream-cli mcp` is a
  stdio MCP proxy. `video.snapshot` returns the program as a PNG, and `stats.tick`
  reports drops once a second.
- **Scenes.** Scenes with placed items (transform and crop), groups, scenes inside
  scenes, shared sources and clones, program-scene switching, per-client undo and redo,
  an audit log, and a collection file that persists across runs. Color, image (PNG,
  JPEG, WebP, animated GIF) and text sources render live.
- **Audio and recording.** Microphone and desktop audio go through a bus-matrix mixer
  with filter chains, loudness and true-peak meters. A record output writes fragmented
  MP4, and outputs share encoder sessions.
- **UI, early.** A Leptos web UI with an outputs and go-live panel and canvas editing,
  and a Tauri desktop app that runs the engine in-process (Windows).
- **Built, not yet wired end to end:** the virtual camera's media source and
  `--register-vcam`, YouTube sign-in and broadcast control, and the zero-copy GPU
  capture path. Their remaining pieces are in review; several need a run on Windows
  hardware.

<a id="progress"></a>
## Progress
<!-- @section
-->

Generated from [`docs/progress.toml`](docs/progress.toml) by `cargo xtask docs generate`,
and drift-checked by `docs check`, so it changes in the same PR as the work. Item IDs
match the PR titles that deliver them.

<!-- @generated begin progress -->
| Milestone | Progress | Merged | In review | Planned |
|---|---|---|---|---|
| [M0: Foundation and risk spikes](ROADMAP.md#m0) | `██████████` 100% | 11 | 0 | 0 |
| [M1a: Thin slice to YouTube](ROADMAP.md#m1) | `████████░░` 83% | 15 | 2 | 1 |
| [M1b: Scenes and mixing](ROADMAP.md#m1) | `████████░░` 82% | 29 | 3 | 3 |
| [M1c: Virtual camera](ROADMAP.md#m1) | `████████░░` 83% | 5 | 0 | 1 |
| [M1d: Hardening](ROADMAP.md#m1) | `████████░░` 77% | 7 | 1 | 1 |
| [M2: UI](ROADMAP.md#m2) | `███░░░░░░░` 31% | 6 | 7 | 6 |
| [M3: Depth and reach](ROADMAP.md#m3) | `█░░░░░░░░░` 12% | 1 | 0 | 7 |

74 of 106 items are merged; 7 of those still wait on a run on Windows hardware.

<details>
<summary><b>M0</b> Foundation and risk spikes: 11 of 11 merged</summary>

| ID | Item | Status | PRs |
|---|---|---|---|
| R1 | Workspace, CI checks and the review gate | ✅ merged | #3, #6, #7, #8 |
| R2 | Docs checker: check, verify, for, back-references | ✅ merged | #9, #10, #11, #13, #14, #15 |
| R3 | lint-deps and governance checks | ✅ merged | #16, #17 |
| R4 | Foundation crates: types, media, protocol, control | ✅ merged | #19, #23, #27 |
| S1 | Spike S1: GPU interop | ✅ merged | #18, #52 |
| S2 | Spike S2: virtual camera | ✅ merged | #25, #52 |
| S3 | Spike S3: clock and drift | ✅ merged | #24, #53 |
| C7 | Spike S4: CEF browser source | 🧪 merged, Windows run pending: S4 spike run | #178, #190 |
| I3 | docs log and docs review | ✅ merged | #145 |
| H7 | Docs stamp conflicts (merge-main, ADR-0042) | ✅ merged | #181, #182, #187 |
| H6 | Doc follow-ups (#102) | ✅ merged | #230 |

</details>

<details>
<summary><b>M1a</b> Thin slice to YouTube: 15 of 18 merged</summary>

| ID | Item | Status | PRs |
|---|---|---|---|
| L1 | Capture, compositor, MF encode, FLV and RTMPS pipeline | ✅ merged | #58, #61, #66, #68, #111, #118 |
| L2 | Engine, daemon, CLI and MCP first cut | ✅ merged | #33, #35, #47, #65, #70 |
| L3 | video.snapshot and stats.tick | ✅ merged | #113, #121 |
| I1 | MCP over HTTP | ✅ merged | #153 |
| A1 | Outputs in the collection | ✅ merged | #142 |
| A2a | Credential store (Windows) | ✅ merged | #141 |
| A2b | Stream key actions | ✅ merged | #142 |
| A5 | Outputs in CLI and mock | ✅ merged | #166 |
| A3 | Daemon goes live on Windows | ✅ merged | #226 |
| A6 | Daemon --collection and audit log | ✅ merged | #232 |
| A4 | Go-live runbook and smoke | 🧪 merged, Windows run pending: smoke script run | #234 |
| E3a | GPU capture frames | ✅ merged | #173 |
| E3b | Render takes GPU frames | 🔍 in review | #219 |
| Y1 | jstream-youtube crate | ✅ merged | #167 |
| Y2 | OAuth token store | ✅ merged | #169 |
| YA | ADR-0041 one sign-in | ✅ merged | #214 |
| Y3 | YouTube in the engine | 🔍 in review | #223 |
| A6b | Daemon YouTube wiring | ⏳ planned |  |

</details>

<details>
<summary><b>M1b</b> Scenes and mixing: 29 of 35 merged</summary>

| ID | Item | Status | PRs |
|---|---|---|---|
| B4 | Kind descriptors and settings | ✅ merged | #155 |
| B3 | Canvas and item geometry | ✅ merged | #183 |
| B8 | Audit log | ✅ merged | #171 |
| B2 | Per-client undo and redo | ✅ merged | #176, #198, #203, #239 |
| B5 | References and clones | ✅ merged | #196 |
| B9 | delete.plan and blockers | ✅ merged | #197 |
| B1 | Collection persistence | ✅ merged | #205 |
| B6 | Groups and item.move | ✅ merged | #184 |
| B10 | Program scene switching | ✅ merged | #201 |
| B6-f | Groups in mock and CLI | ✅ merged | #210 |
| B6-r | scene.create role | ✅ merged | #211 |
| B7 | Opacity, blend, filters | 🔍 in review | #252 |
| C6 | Nested scene rendering | ✅ merged | #147 |
| C4a | Video device trait | ✅ merged | #158 |
| C4b | Media Foundation webcam backend | ✅ merged | #177, #225, #227 |
| C1a | Sources crate: color, image | ✅ merged | #179, #217 |
| C1b | Sources render live | ✅ merged | #222 |
| C2 | Text source | ✅ merged | #221 |
| C5 | Audio sources | 🧪 merged, Windows run pending: audio hardware tests | #238 |
| C3 | Display and window capture sources | ⏳ planned |  |
| C8 | Browser source (CEF) | ⏳ planned |  |
| D1 | Live mixer inputs | ✅ merged | #149, #199 |
| D3 | Loudness meters | ✅ merged | #152 |
| D7a | Filter kernels | ✅ merged | #159 |
| D2 | Buses and send matrix | ✅ merged | #192 |
| D4 | No-allocation test | ✅ merged | #191 |
| D7b | Filter chains in the mixer | ✅ merged | #231 |
| D5 | Audio in the protocol | 🔍 in review | #240 |
| D6 | when_shown activity gain | ⏳ planned |  |
| E1 | Render snapshots, GPU lifetime | ✅ merged | #172, #215 |
| F1 | fMP4 muxer | ✅ merged | #151 |
| F2a | Recording file sink | ✅ merged | #156 |
| F2b | Record output kind | ✅ merged | #170 |
| F3 | Encoder profiles and sharing | ✅ merged | #208 |
| I2 | Live values | 🔍 in review | #244 |

</details>

<details>
<summary><b>M1c</b> Virtual camera: 5 of 6 merged</summary>

| ID | Item | Status | PRs |
|---|---|---|---|
| G1 | vcam protocol crate | ✅ merged | #144 |
| G2 | vcam media source DLL | 🧪 merged, Windows run pending: pacing rerun | #174, #195, #200, #229 |
| G3a | vcam producer (engine side) | ✅ merged | #194 |
| G3b | VirtualCam output kind | 🧪 merged, Windows run pending: WindowsVcam in the daemon's backends, then a camera run | #213 |
| G3c | --register-vcam | 🧪 merged, Windows run pending: registration check with UAC | #235 |
| P1 | Portable Windows zip | ⏳ planned |  |

</details>

<details>
<summary><b>M1d</b> Hardening: 7 of 9 merged</summary>

| ID | Item | Status | PRs |
|---|---|---|---|
| H0 | RTMP reconnect, rotating logs and crash reports | ✅ merged | #126, #127, #131 |
| H1 | N-edition detection | ✅ merged | #148 |
| H2 | Deny duplicate crate versions | ✅ merged | #157 |
| H3 | Weekly sweep job | ✅ merged | #160, #165 |
| H4 | Flaky shutdown tests | ✅ merged | #150 |
| H5 | Server hardening | ✅ merged | #140 |
| F4 | Keyframe on demand | ✅ merged | #146 |
| Q1 | Fuzz and property tests | ⏳ planned |  |
| Q2 | Headless end-to-end test | 🔍 in review | #269 |

</details>

<details>
<summary><b>M2</b> UI: 6 of 19 merged</summary>

| ID | Item | Status | PRs |
|---|---|---|---|
| U0 | Leptos UI skeleton | ✅ merged | #250 |
| U2a | ADR-0044 pairing | ✅ merged | #248 |
| U2 | Serve the UI | 🔍 in review | #246, #247 |
| U2b | Device pairing | 🔍 in review | #265 |
| U3a | Scenes, sources, properties | 🔍 in review | #256 |
| U3b | Outputs and go-live panel | ✅ merged | #254 |
| U3c | Mixer panel | ⏳ planned |  |
| U4 | Canvas editing | ✅ merged | #255 |
| U4p | Live preview on canvas | ⏳ planned |  |
| U1 | Preview stream | 🔍 in review | #251 |
| U5 | Tauri desktop app | 🧪 merged, Windows run pending: desktop app check | #258 |
| T1 | Studio mode and transitions | 🔍 in review | #260 |
| T1u | Studio-mode UI | ⏳ planned |  |
| E2 | Video mixes | 🔍 in review | #259 |
| D8 | Master and output audio chains | ✅ merged | #249 |
| D8p | Audio chain protocol fields | ⏳ planned |  |
| C9 | Media file source | 🔍 in review | #245 |
| W1 | Audio waveform and spectrum sources | ⏳ planned |  |
| K1 | Keyframe scenes, folders, organisation metadata and hotkeys | ⏳ planned |  |

</details>

<details>
<summary><b>M3</b> Depth and reach: 1 of 8 merged</summary>

| ID | Item | Status | PRs |
|---|---|---|---|
| F5 | Enhanced RTMP (HEVC/AV1) | ✅ merged | #163 |
| E3c | Encode from GPU frames | ⏳ planned |  |
| X1 | Full 3D transforms and WGSL effect packs | ⏳ planned |  |
| Y4 | YouTube chat, metadata, health and audience stats | ⏳ planned |  |
| CL1 | CLAP plugin host | ⏳ planned |  |
| WA1 | WASM automation plugins | ⏳ planned |  |
| MAC1 | macOS platform crate | ⏳ planned |  |
| LNX1 | Linux codec path and platform crate | ⏳ planned |  |

</details>
<!-- @generated end progress -->

<a id="build"></a>
## Build and run
<!-- @section
-->

You need Rust through `rustup`; `rust-toolchain.toml` pins the version and
adds the Windows and `wasm32` targets. The media backends are Windows only: on Linux and
macOS everything builds and the tests run on fakes, and the daemon serves the API with
placeholder outputs.

```sh
cargo build --release -p jstream-daemon -p jstream-cli
cargo test --workspace        # on Linux, wgpu tests need Vulkan (mesa-vulkan-drivers)
```

Start the daemon, then drive it with the CLI (or point an MCP client at the `mcp
endpoint` line it prints, with the token as a bearer token):

```sh
target/release/jstream-daemon --token-file ~/.config/jstream/token --port 4460 \
    --collection ~/.config/jstream/collection.json
JSTREAM_TOKEN_FILE=~/.config/jstream/token \
    target/release/jstream-cli --url ws://127.0.0.1:4460 scene list
```

- Going live on Windows, step by step: [docs/runbooks/go-live.md](docs/runbooks/go-live.md).
- Every daemon option: [jstream-daemon › Usage](apps/jstream-daemon/README.md#usage);
  every CLI command: [jstream-cli › Usage](apps/jstream-cli/README.md#usage).
- The web UI and the desktop app (Windows): [jstream-ui › Build](crates/jstream-ui/README.md#build)
  and [jstream-desktop › Usage](apps/jstream-desktop/README.md#usage).

<a id="platforms"></a>
## Platforms
<!-- @section
depends_on = ["docs/adr/0008-windows-support-policy.md"]
verified = { hash = "b3:174bb43ca018529b", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Linux row's link moved from superseded ADR-0003 to ADR-0031, which restates the Linux codec-path deferral unchanged; Windows fact marker and ADR-0008 policy, macOS row unchanged" }
-->

| Platform | Status | Support |
|---|---|---|
| Windows | Primary target | Runs on Windows 11 build 22000<!--fact:min-windows-build--> and later; supported on the versions Microsoft currently services ([ADR-0008](docs/adr/0008-windows-support-policy.md)) |
| macOS | Later (M3) | To be decided in the macOS ADR |
| Linux | Later (M3) | Codec path to be decided in an ADR first ([ADR-0031](docs/adr/0031-source-types-and-decoding-scope.md)) |

<a id="documentation"></a>
## Documentation
<!-- @section
-->

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): how the system fits together
- [docs/RULES.md](docs/RULES.md): engineering rules, each with a permanent ID
- [docs/DOCS.md](docs/DOCS.md): how docs stay accurate (hash-checked sections, history from git)
- [docs/adr/](docs/adr/): decision records
- [ROADMAP.md](ROADMAP.md): milestones, and [`docs/progress.toml`](docs/progress.toml):
  the item status the [Progress](#progress) table is generated from
- [docs/runbooks/go-live.md](docs/runbooks/go-live.md): taking the daemon live on Windows,
  and the smoke script that checks it
- [CLAUDE.md](CLAUDE.md): the entry point for builder agents
- [REVIEW.md](REVIEW.md): the reviewer agent's instructions; its verdict gates merges
- [CONTRIBUTING.md](CONTRIBUTING.md), [SECURITY.md](SECURITY.md) and
  [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md): who works on JStream, how to report bugs and
  vulnerabilities, and the conduct expected in its spaces

<a id="license"></a>
## License
<!-- @section
-->

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option. Unless you explicitly state otherwise, any
contribution intentionally submitted for inclusion in this project, as defined in the
Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
