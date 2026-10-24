<!-- @doc
id = "README"
title = "JStream"
kind = "normative"
status = "active"
review_every = "90d"
retired_sections = { "today" = [] }
-->

# JStream

A live-streaming and compositing engine written in Rust: a ground-up rewrite in the spirit of
OBS, built to be driven equally well by a human or an AI agent.

> **In development.** JStream is not ready for use yet. [Progress](#progress) shows how
> far each milestone has got, and [ROADMAP.md](ROADMAP.md) has the plan.

- **Stream** to YouTube (RTMPS + YouTube Live API) or any RTMP target
- **Virtual camera** (an OS-native Windows camera that every app can see)
- **Scenes, sources, effects**, 3D transforms and crop on a wgpu compositor
- **Independent video and audio routing**: outputs are not tied to scenes, and audio runs
  through a bus-matrix mixer
- **Full agentic control over MCP**: anything the UI can do, an agent can do, except
  YouTube's account features, which stay with the person
  ([ADR-0058](docs/adr/0058-youtube-sign-in-with-a-jstream-client.md))
- **Headless or desktop**: the engine is a library, embedded in a Tauri app or run as a
  daemon that serves the same web UI to any browser

JStream is built to be driven by a person or by an AI agent. The engine is a library with
one protocol; the web UI, the desktop app, the CLI and MCP are all clients of it, so
nothing the UI can do is out of an agent's reach, YouTube's account features aside
([ADR-0001](docs/adr/0001-library-first-engine.md),
[ADR-0006](docs/adr/0006-mcp-full-access.md)). Windows comes first, and the headless
engine and MCP come before the GUI ([ADR-0010](docs/adr/0010-headless-first-milestone.md)).

<a id="progress"></a>
## Progress
<!-- @section
-->

Generated from [`docs/progress.toml`](docs/progress.toml) by `cargo xtask docs generate`
and drift-checked by `docs check`, so it changes in the same PR as the work.

JStream is done when every item below is merged and has passed its Windows run, where it
needs one.
[ROADMAP › Endgame](ROADMAP.md#endgame) defines that and gives the order the rest lands
in, and **What's left** lists each unfinished item with the steps it still needs.

<!-- @generated begin progress -->
| Milestone | Progress | Merged | In review | Planned |
|---|---|---|---|---|
| [M0: Foundation and risk spikes](ROADMAP.md#m0) | `██████████` 100% | 11 | 0 | 0 |
| [M1a: Thin slice to YouTube](ROADMAP.md#m1) | `██████████` 100% | 18 | 0 | 0 |
| [M1b: Scenes and mixing](ROADMAP.md#m1) | `█████████░` 95% | 35 | 2 | 0 |
| [M1c: Virtual camera](ROADMAP.md#m1) | `██████████` 100% | 7 | 0 | 0 |
| [M1d: Hardening](ROADMAP.md#m1) | `██████████` 100% | 9 | 0 | 0 |
| [M2: UI](ROADMAP.md#m2) | `████████░░` 80% | 20 | 4 | 1 |
| [M3: Depth and reach](ROADMAP.md#m3) | `████░░░░░░` 38% | 5 | 1 | 7 |

105 of 120 items are merged; 11 of those still wait on a run on Windows hardware.

<details open>
<summary><b>What's left</b>: 26 items, by next step: 🔨 build 13, 🧪 Windows run 10, 👤 maintainer 1, ❔ not set 2</summary>

The order these land in, and what done means, is in [ROADMAP › Endgame](ROADMAP.md#endgame).

| ID | Item | Milestone | Needs, in order | Note | PRs |
|---|---|---|---|---|---|
| A3 | Daemon goes live on Windows | M1a | 🧪 Windows run | Windows 11 Pro N run (#333) | #226 |
| A4 | Go-live runbook and smoke | M1a | 🧪 Windows run | smoke script run | #234 |
| A6b | Daemon YouTube wiring | M1a | 👤 maintainer, then 🧪 Windows run | #312 landed with wave/m1-2 (#545); sign-in needs the OAuth client (ADR-0058); Windows run #337 | #312 |
| C4c | Webcam source (CPU path, MJPEG on Windows, paired audio) | M1b | 🔨 build, then 🌊 its wave lands, then 🧪 Windows run |  | #654, #658, #726 |
| C5 | Audio sources | M1b | 🧪 Windows run | audio hardware tests | #238 |
| C3 | Display and window capture sources | M1b | 🧪 Windows run | #365 landed with wave/m1-2 (#545); Windows run #378 | #365 |
| C8 | Browser source (CEF) | M1b | 🧪 Windows run | #341, #335, #336 | #283, #289, #298, #326 |
| V0 | Video filter chain runtime | M1b | 🔨 build | part 1 runs the built-in kinds, part 2 loads apply_lut and image_mask assets, part 3 gives scene_mask its scene, and #730 uploads LUT tables directly; then effect-pack kinds and scratch pooling. No bounds inset: on a source chain shadow, outline and glow draw within the frame (ADR-0065 §5) | #595, #607, #626, #717, #730 |
| G2 | vcam media source DLL | M1c | 🧪 Windows run | pacing rerun | #174, #195, #200, #229 |
| G3c | --register-vcam | M1c | 🧪 Windows run | Windows run (#563) | #235, #565 |
| G3d | Virtual camera in the UI and the desktop app | M1c | 🧪 Windows run | Windows run (#390) | #316 |
| T1 | Studio mode and transitions | M2 | 🔨 build |  | #260 |
| T1u | Studio-mode UI | M2 | 🔨 build |  | #304 |
| E2b | Privacy screen and encoder sharing key | M2 | ❔ not set |  | #317 |
| C9 | Media file source | M2 | 🧪 Windows run | media file hardware tests (#382) | #245, #322 |
| B11 | Folders and labels | M2 | ❔ not set |  | #323 |
| K1 | Keyframe scenes, folders, organisation metadata and hotkeys | M2 | 🔨 build | hotkeys #319 landed with wave/m1-2 (#545); organisation metadata #323 in review into wave/m1-3 | #319 |
| F6 | HEVC and AV1 encode | M3 | 🧪 Windows run | #364 landed with wave/m1-2 (#545); Windows run #366; choosing HEVC or AV1 from a client is F6b | #364 |
| X1 | Full 3D transforms and WGSL effect packs | M3 | 🔨 build | effect packs #368 landed with wave/m1-2 (#545); full 3D transforms to build | #359, #368 |
| Y4 | YouTube chat, metadata, health and audience stats | M3 | 🔨 build, then 👤 maintainer | chat reading and the featured message (#363, landed with wave/m1-2, #545); chat preset of the browser source (#375, landed with wave/m1); audience stats in jstream-youtube (#594, landed with wave/m3-2, #663); signed-in features need the OAuth client (ADR-0058); the engine's stats polling, youtube.stats event and query, and quota accounting are still to come | #363, #375, #594 |
| CL1 | CLAP plugin host | M3 | 🔨 build, then 🧪 Windows run | parts 1 to 3 merged (wave/m3 landed in #477): the shared-memory contract, the CLAP wrapper, host app, launcher, supervisor and CLAP filter kinds; the host's Windows run passed in #342; CLAP filters to run; then a live-audio Windows run (new issue once CLAP filters run) | #272, #281, #407 |
| WA1 | WASM automation plugins | M3 | 🔨 build, then 🌊 its wave lands | WASM host #486 landed with wave/m3-2 (#663); the wire half #642 is in review into wave/m3-3; the engine side to build | #486, #642 |
| MAC1 | macOS platform crate | M3 | 🔨 build | ADR-0063 accepted (#511); crate part 1 (#513); VideoToolbox H.264/HEVC encoder, part 2 (#533); AudioToolbox AAC-LC encoder, part 3 (#543); ScreenCaptureKit display and window capture with the Screen Recording permission check, part 4 (#554); Keychain store for stream keys and OAuth tokens, part 5 (#566); Core Audio devices and input recording, part 6 (#570); Microphone permission check and the daemon's embedded Info.plist, part 7 (#583); Core Audio output playback, part 8 (#586); all landed with wave/m3-2 (#663) | #511, #513, #533, #543, #554, #566, #570, #583, #586 |
| LNX1 | Linux codec path and platform crate | M3 | 🔨 build | ADR-0060 accepted (#481); crate part 1 (#490); portal and PipeWire screen capture (#509); VA-API H.264 encode (#510); these landed with wave/m3-2 (#663); the rest of the codec path to build | #481, #490, #509, #510 |
| LNX2 | Linux OpenH264 download (ADR-0061) | M3 | 🔨 build | store, check, loader and encoder (#536, landed with wave/m3-2, #663); downloading stays off until the pin's two SHA-256 values are recorded from Cisco's server, and the UI's download offer, with Cisco's notice re-read, is still to come | #536 |
| LNX3 | AAC-LC encoder in Rust, patent check first (ADR-0061) | M3 | 🔨 build | patent check ADR-0062 accepted (#508); encoder and Linux wiring (#518), landed with wave/m3-2 (#663); TNS and intensity stereo later; before the Linux app ships the encoder its licence information must say JStream holds no AAC patent licence and name Via LA (ADR-0062) | #508, #518 |

</details>

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
| C7 | Spike S4: CEF browser source | ✅ merged | #178, #190 |
| I3 | docs log and docs review | ✅ merged | #145 |
| H7 | Docs stamp conflicts (merge-main, ADR-0042) | ✅ merged | #181, #182, #187 |
| H6 | Doc follow-ups (#102) | ✅ merged | #230 |

</details>

<details>
<summary><b>M1a</b> Thin slice to YouTube: 18 of 18 merged</summary>

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
| A3 | Daemon goes live on Windows | 🧪 merged, Windows run pending: Windows 11 Pro N run (#333) | #226 |
| A6 | Daemon --collection and audit log | ✅ merged | #232 |
| A4 | Go-live runbook and smoke | 🧪 merged, Windows run pending: smoke script run | #234 |
| E3a | GPU capture frames | ✅ merged | #173 |
| E3b | Render takes GPU frames | ✅ merged | #219 |
| Y1 | jstream-youtube crate | ✅ merged | #167 |
| Y2 | OAuth token store | ✅ merged | #169 |
| YA | ADR-0041 one sign-in | ✅ merged | #214 |
| Y3 | YouTube in the engine | ✅ merged | #223 |
| A6b | Daemon YouTube wiring | 🧪 merged, Windows run pending: #312 landed with wave/m1-2 (#545); sign-in needs the OAuth client (ADR-0058); Windows run #337 | #312 |

</details>

<details>
<summary><b>M1b</b> Scenes and mixing: 35 of 37 merged</summary>

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
| B7 | Opacity, blend, filters | ✅ merged | #252 |
| C6 | Nested scene rendering | ✅ merged | #147 |
| C4a | Video device trait | ✅ merged | #158 |
| C4b | Media Foundation webcam backend | ✅ merged | #177, #225, #227 |
| C4c | Webcam source (CPU path, MJPEG on Windows, paired audio) | 🔍 in review | #654, #658, #726 |
| C1a | Sources crate: color, image | ✅ merged | #179, #217 |
| C1b | Sources render live | ✅ merged | #222 |
| C2 | Text source | ✅ merged | #221 |
| C5 | Audio sources | 🧪 merged, Windows run pending: audio hardware tests | #238 |
| C3 | Display and window capture sources | 🧪 merged, Windows run pending: #365 landed with wave/m1-2 (#545); Windows run #378 | #365 |
| C8 | Browser source (CEF) | 🧪 merged, Windows run pending: #341, #335, #336 | #283, #289, #298, #326 |
| D1 | Live mixer inputs | ✅ merged | #149, #199 |
| D3 | Loudness meters | ✅ merged | #152 |
| D7a | Filter kernels | ✅ merged | #159 |
| D2 | Buses and send matrix | ✅ merged | #192 |
| D4 | No-allocation test | ✅ merged | #191 |
| D7b | Filter chains in the mixer | ✅ merged | #231 |
| V0 | Video filter chain runtime | 🔍 in review: part 1 runs the built-in kinds, part 2 loads apply_lut and image_mask assets, part 3 gives scene_mask its scene, and #730 uploads LUT tables directly; then effect-pack kinds and scratch pooling. No bounds inset: on a source chain shadow, outline and glow draw within the frame (ADR-0065 §5) | #595, #607, #626, #717, #730 |
| D5 | Audio in the protocol | ✅ merged | #240 |
| D6 | when_shown activity gain | ✅ merged | #299 |
| E1 | Render snapshots, GPU lifetime | ✅ merged | #172, #215 |
| F1 | fMP4 muxer | ✅ merged | #151 |
| F2a | Recording file sink | ✅ merged | #156 |
| F2b | Record output kind | ✅ merged | #170 |
| F3 | Encoder profiles and sharing | ✅ merged | #208 |
| I2 | Live values | ✅ merged | #244 |

</details>

<details>
<summary><b>M1c</b> Virtual camera: 7 of 7 merged</summary>

| ID | Item | Status | PRs |
|---|---|---|---|
| G1 | vcam protocol crate | ✅ merged | #144 |
| G2 | vcam media source DLL | 🧪 merged, Windows run pending: pacing rerun | #174, #195, #200, #229 |
| G3a | vcam producer (engine side) | ✅ merged | #194 |
| G3b | VirtualCam output kind | ✅ merged | #213, #297 |
| G3c | --register-vcam | 🧪 merged, Windows run pending: Windows run (#563) | #235, #565 |
| G3d | Virtual camera in the UI and the desktop app | 🧪 merged, Windows run pending: Windows run (#390) | #316 |
| P1 | Portable Windows zip | ✅ merged | #271 |

</details>

<details>
<summary><b>M1d</b> Hardening: 9 of 9 merged</summary>

| ID | Item | Status | PRs |
|---|---|---|---|
| H0 | RTMP reconnect, rotating logs and crash reports | ✅ merged | #126, #127, #131 |
| H1 | N-edition detection | ✅ merged | #148 |
| H2 | Deny duplicate crate versions | ✅ merged | #157 |
| H3 | Weekly sweep job | ✅ merged | #160, #165 |
| H4 | Flaky shutdown tests | ✅ merged | #150 |
| H5 | Server hardening | ✅ merged | #140 |
| F4 | Keyframe on demand | ✅ merged | #146 |
| Q1 | Fuzz and property tests | ✅ merged | #277 |
| Q2 | Headless end-to-end test | ✅ merged | #269 |

</details>

<details>
<summary><b>M2</b> UI: 20 of 25 merged</summary>

| ID | Item | Status | PRs |
|---|---|---|---|
| U0 | Leptos UI skeleton | ✅ merged | #250 |
| U2a | ADR-0044 pairing | ✅ merged | #248 |
| U2 | Serve the UI | ✅ merged | #246, #247 |
| U2b | Device pairing | ✅ merged | #265, #288 |
| U3a | Scenes, sources, properties | ✅ merged | #256 |
| U3b | Outputs and go-live panel | ✅ merged | #254 |
| U3c | Mixer panel | ✅ merged | #308 |
| U4 | Canvas editing | ✅ merged | #255 |
| U4p | Live preview on canvas | ✅ merged | #302 |
| U1 | Preview stream | ✅ merged | #251 |
| U5 | Tauri desktop app | ✅ merged | #258 |
| Q3 | UI end-to-end tests | ✅ merged | #309 |
| Q4 | Performance benchmarks | ✅ merged | #361 |
| Q5 | Threat model | ✅ merged | #358 |
| T1 | Studio mode and transitions | 🔍 in review | #260 |
| T1u | Studio-mode UI | 🔍 in review | #304 |
| E2 | Video mixes | ✅ merged | #259 |
| E2b | Privacy screen and encoder sharing key | 🔍 in review | #317 |
| D8 | Master and output audio chains | ✅ merged | #249 |
| D8p | Audio chain protocol fields | ✅ merged | #303 |
| C9 | Media file source | 🧪 merged, Windows run pending: media file hardware tests (#382) | #245, #322 |
| W1 | Audio waveform and spectrum sources | ✅ merged | #315, #460, #463, #466 |
| B11 | Folders and labels | 🔍 in review | #323 |
| K1 | Keyframe scenes, folders, organisation metadata and hotkeys | ⏳ planned: hotkeys #319 landed with wave/m1-2 (#545); organisation metadata #323 in review into wave/m1-3 | #319 |
| K2 | Keyframe scenes: model and evaluator | ✅ merged | #350 |

</details>

<details>
<summary><b>M3</b> Depth and reach: 5 of 13 merged</summary>

| ID | Item | Status | PRs |
|---|---|---|---|
| F5 | Enhanced RTMP (HEVC/AV1) | ✅ merged | #163 |
| E3c | Encode from GPU frames | ✅ merged | #325 |
| E3d | Composite to NV12 on the GPU | ✅ merged | #280 |
| F6 | HEVC and AV1 encode | 🧪 merged, Windows run pending: #364 landed with wave/m1-2 (#545); Windows run #366; choosing HEVC or AV1 from a client is F6b | #364 |
| X1 | Full 3D transforms and WGSL effect packs | ⏳ planned: effect packs #368 landed with wave/m1-2 (#545); full 3D transforms to build | #359, #368 |
| 3D1 | 3D transforms: rendering ADR and spike | ✅ merged | #360 |
| Y4 | YouTube chat, metadata, health and audience stats | ⏳ planned: chat reading and the featured message (#363, landed with wave/m1-2, #545); chat preset of the browser source (#375, landed with wave/m1); audience stats in jstream-youtube (#594, landed with wave/m3-2, #663); signed-in features need the OAuth client (ADR-0058); the engine's stats polling, youtube.stats event and query, and quota accounting are still to come | #363, #375, #594 |
| CL1 | CLAP plugin host | ⏳ planned: parts 1 to 3 merged (wave/m3 landed in #477): the shared-memory contract, the CLAP wrapper, host app, launcher, supervisor and CLAP filter kinds; the host's Windows run passed in #342; CLAP filters to run; then a live-audio Windows run (new issue once CLAP filters run) | #272, #281, #407 |
| WA1 | WASM automation plugins | 🔍 in review: WASM host #486 landed with wave/m3-2 (#663); the wire half #642 is in review into wave/m3-3; the engine side to build | #486, #642 |
| MAC1 | macOS platform crate | ⏳ planned: ADR-0063 accepted (#511); crate part 1 (#513); VideoToolbox H.264/HEVC encoder, part 2 (#533); AudioToolbox AAC-LC encoder, part 3 (#543); ScreenCaptureKit display and window capture with the Screen Recording permission check, part 4 (#554); Keychain store for stream keys and OAuth tokens, part 5 (#566); Core Audio devices and input recording, part 6 (#570); Microphone permission check and the daemon's embedded Info.plist, part 7 (#583); Core Audio output playback, part 8 (#586); all landed with wave/m3-2 (#663) | #511, #513, #533, #543, #554, #566, #570, #583, #586 |
| LNX1 | Linux codec path and platform crate | ⏳ planned: ADR-0060 accepted (#481); crate part 1 (#490); portal and PipeWire screen capture (#509); VA-API H.264 encode (#510); these landed with wave/m3-2 (#663); the rest of the codec path to build | #481, #490, #509, #510 |
| LNX2 | Linux OpenH264 download (ADR-0061) | ⏳ planned: store, check, loader and encoder (#536, landed with wave/m3-2, #663); downloading stays off until the pin's two SHA-256 values are recorded from Cisco's server, and the UI's download offer, with Cisco's notice re-read, is still to come | #536 |
| LNX3 | AAC-LC encoder in Rust, patent check first (ADR-0061) | ⏳ planned: patent check ADR-0062 accepted (#508); encoder and Linux wiring (#518), landed with wave/m3-2 (#663); TNS and intensity stereo later; before the Linux app ships the encoder its licence information must say JStream holds no AAC patent licence and name Via LA (ADR-0062) | #508, #518 |

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
depends_on = ["docs/adr/0008-windows-support-policy.md", "docs/adr/0063-macos-platform-support.md"]
-->

| Platform | Status | Support |
|---|---|---|
| Windows | Primary target | Runs on Windows 11 build 22000<!--fact:min-windows-build--> and later; supported on the versions Microsoft currently services ([ADR-0008](docs/adr/0008-windows-support-policy.md)) |
| macOS | Later (M3) | Per [ADR-0063](docs/adr/0063-macos-platform-support.md): macOS 14 and later, Apple Silicon (Intel best effort) |
| Linux | Later (M3) | Per [ADR-0060](docs/adr/0060-linux-platform-support.md): GNOME or KDE Plasma on Wayland, current Ubuntu LTS and Fedora |

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
- [docs/runbooks/waves.md](docs/runbooks/waves.md): which wave branch a PR targets, and
  how a wave lands in `main`
- [docs/runbooks/windows-hardware-pass.md](docs/runbooks/windows-hardware-pass.md): every
  open Windows hardware run as one checklist, with the issue each one closes
- [docs/security/threat-model.md](docs/security/threat-model.md): what JStream protects, from
  whom, the controls at each trust boundary, and the open gaps
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
