<!-- @doc
id = "README"
title = "JStream"
kind = "normative"
status = "active"
review_every = "90d"
retired_sections = { "today" = [], "humans" = ["install"], "progress" = ["status"] }
-->

# JStream

[![CI](https://github.com/jeffmhopkins/JStream/actions/workflows/ci.yml/badge.svg)](.github/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/jeffmhopkins/JStream?include_prereleases&label=release)](https://github.com/jeffmhopkins/JStream/releases)
[![Built with Rust](https://img.shields.io/badge/built%20with-Rust-dea584)](rust-toolchain.toml)
[![Agent control: MCP](https://img.shields.io/badge/agent%20control-MCP-8a2be2)](#agents)
[![Licence: MIT OR Apache-2.0](https://img.shields.io/badge/licence-MIT%20OR%20Apache--2.0-blue)](#license)

> A live-streaming and compositing studio written in Rust, built from the ground up to be
> driven equally well by a person or by an AI agent.

> [!NOTE]
> JStream is in **pre-release**. Expect rough edges, and keep a second way to stream for
> a show that matters. [Status](#status) says where it stands.

<a id="what"></a>
## What JStream is
<!-- @section
-->

JStream composes scenes from your cameras, screens, media, browser pages and graphics on
the GPU, mixes their audio, and sends the result to a stream, a recording or a virtual
camera. It does the job of a streaming studio such as OBS, written again from scratch
around one idea: **everything is an API**.

- **Stream** over RTMP or RTMPS to YouTube, or to any service that takes a stream key,
  and **record** to a file at the same time.
- **Virtual camera**: an OS-native Windows camera that every video-call app can see.
- **Scenes and sources** on a wgpu compositor: cameras, PTZ cameras, screen and window
  capture, media files, images, text, colour, timers, audio visualisers and browser
  sources, with crop, 3D transforms and blend modes.
- **Effects**: built-in filters on sources, items, scenes and outputs, and shader
  sources and filters written as WGSL effect packs.
- **3D scenes**: a scene can have a camera that its items sit in front of, moving through
  them, with depth of field.
- **Transitions and motion**: a studio preview, keyframe scenes that animate between
  layouts, and native motion, one-shot and audio-reactive, on items and cameras.
- **Independent routing**: outputs take video mixes and audio buses, not scenes, so a
  stream and a recording can show different things, and audio runs through a bus mixer
  with CLAP plugins.
- **Control your way**: hotkeys, MIDI controllers and tempo sync share one binding
  system, and pop-out projectors put any scene on another monitor.
- **Desktop or headless**: the same engine runs inside the desktop app or as a daemon that
  serves the web UI to any paired browser.

<a id="different"></a>
## How it differs from OBS
<!-- @section
-->

OBS is mature, battle-tested and has a huge plugin ecosystem; JStream is new and has
none of that. What JStream does differently is in its design, and each point below is in
the code today:

| | JStream |
|---|---|
| **Agent control** | A built-in MCP server gives an AI agent the same control a person has in the window, apart from YouTube's account features and installing or granting plugins, so it can build scenes, restyle them, take snapshots to check its work, and undo its own edits ([ADR-0006](docs/adr/0006-mcp-full-access.md)). Steps undo can't take back, such as going live, are marked so the agent asks first. |
| **One protocol** | The engine is a library behind one versioned, token-authenticated WebSocket protocol. The web UI, the desktop app, the CLI and MCP are all clients of it, so nothing is UI-only ([ADR-0001](docs/adr/0001-library-first-engine.md)). |
| **Rust core** | The engine is Rust, with `unsafe` code allowed only in the crates that declare it. The real-time audio thread never allocates, locks or blocks, and tests enforce it ([R-RT-001](docs/RULES.md#R-RT-001)). |
| **Headless first** | The whole engine runs with no window, on a software GPU, a fake clock and fake devices, which is how its tests run, and how a server can host it. |
| **Outputs apart from scenes** | Outputs consume video mixes and audio buses, so each output can carry its own picture and sound. |
| **Undo per client** | Edits are undoable per client: an agent's undo never takes back yours. The **Agent activity** panel lists what an agent changed. |
| **Effects agents can write** | An agent can write its own WGSL shader packs over MCP and use them in a scene straight away ([ADR-0097](docs/adr/0097-agents-write-effect-packs.md)). |

<a id="demos"></a>
## Demos
<!-- @section
-->

Examples of what JStream can do. Each clip links to the video file; the scenes and the
code behind them live in [StreamingScenes](https://github.com/jeffmhopkins/StreamingScenes),
not in JStream.

| | |
|---|---|
| [![A scene of stacked browser pages, recorded in JStream](https://raw.githubusercontent.com/jeffmhopkins/StreamingScenes/main/starship-cockpit/media/nebula.jpg)](https://github.com/jeffmhopkins/StreamingScenes/raw/main/starship-cockpit/media/cockpit-jstream.mp4) | **A layered scene, recorded live.** Browser pages and images stacked into one scene, with live cameras inside it; its effects and music-driven lights are the scene's own pages. Recorded on Linux with JStream's own `record` output. |
| [![Depth of field through a scene camera](https://raw.githubusercontent.com/jeffmhopkins/StreamingScenes/main/camera-demos/media/depth-of-field.jpg)](https://github.com/jeffmhopkins/StreamingScenes/raw/main/camera-demos/media/depth-of-field.mp4) | **Depth of field.** Cards at three depths seen through a scene camera; the focus pulls from one to the next and the blur grows and shrinks. |
| [![A screen capture seen up close through a moving camera](https://raw.githubusercontent.com/jeffmhopkins/StreamingScenes/main/camera-demos/media/screen-closeup.jpg)](https://github.com/jeffmhopkins/StreamingScenes/raw/main/camera-demos/media/screen-closeup.mp4) | **A 3D screen close-up.** A screen capture placed flat in a 3D scene, with the camera gliding between close, angled views of it. |

The two camera clips were rendered frame by frame by JStream's compositor, outside the app.

<a id="platforms"></a>
## Platforms
<!-- @section
depends_on = ["docs/adr/0008-windows-support-policy.md", "docs/adr/0063-macos-platform-support.md", "docs/adr/0060-linux-platform-support.md"]
-->

The first beta, `0.1.0-beta.1`, targets all three desktop platforms.

| Platform | Requirements | How you get it today |
|---|---|---|
| **Windows** | Windows 11 build 22000<!--fact:min-windows-build--> or later, 64-bit, with a Direct3D 12 GPU; supported on the versions Microsoft currently services ([ADR-0008](docs/adr/0008-windows-support-policy.md)) | A portable zip from the [releases page](https://github.com/jeffmhopkins/JStream/releases) |
| **macOS** | macOS 14 or later, Apple Silicon (Intel best effort) ([ADR-0063](docs/adr/0063-macos-platform-support.md)) | From beta.1, an unsigned disk image (`.dmg`) from the [releases page](https://github.com/jeffmhopkins/JStream/releases); before then, one command that builds and installs it from source ([mac-testing](docs/runbooks/mac-testing.md)) |
| **Linux** | 64-bit, GNOME or KDE Plasma on Wayland, current Ubuntu LTS and Fedora ([ADR-0060](docs/adr/0060-linux-platform-support.md)) | From beta.1, a `.deb` and an AppImage from the [releases page](https://github.com/jeffmhopkins/JStream/releases); before then, one command that builds and installs it from source ([linux-testing](docs/runbooks/linux-testing.md)) |

<a id="install"></a>
## Install and first steps
<!-- @section
-->

1. **Get it.** On Windows, download the latest `jstream-v<version>-windows-x64.zip` from
   the [releases page](https://github.com/jeffmhopkins/JStream/releases), unzip it and run
   `jstream.exe`. There is no installer and it needs no administrator rights. From beta.1
   the same page has an unsigned `.dmg` for Apple Silicon Macs and a `.deb` and an
   AppImage for 64-bit Linux ([Getting started](docs/GETTING-STARTED.md#install)); before
   then, run the one-line install from [Platforms](#platforms).
2. **Learn it.** [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md) walks through a first
   scene, going live, recording, the virtual camera and connecting an agent.
3. **Report what breaks.** Bugs and ideas are welcome as
   [issues](https://github.com/jeffmhopkins/JStream/issues/new); security problems go
   privately, as [SECURITY.md](SECURITY.md) says. [CONTRIBUTING.md](CONTRIBUTING.md) says
   what contributions JStream takes.

To build from source by hand, see [Build from source](#build).

<a id="agents"></a>
## For agents
<!-- @section
-->

There are two ways an agent meets JStream: **driving** a running JStream over MCP, or
**building** JStream itself in this repo.

**Driving JStream.** Start JStream with `--mcp` (or run the daemon), then point your MCP
client at it: through `jstream-cli mcp`, a stdio proxy, or straight at the streamable HTTP
endpoint with the token as a bearer token. JStream asks you before a new agent connects.
[GETTING-STARTED › Connect an agent](docs/GETTING-STARTED.md#agents) has the steps for
Claude Desktop and Claude Code. Every tool is described in
[jstream-mcp](crates/jstream-mcp/README.md), and the protocol behind them in
[ARCHITECTURE › Control protocol](docs/ARCHITECTURE.md#protocol).

**Building JStream.** JStream is built by AI builder agents and gated by a reviewer agent.
Read [CLAUDE.md](CLAUDE.md) before changing anything: it explains how the docs, rules and
ADRs that govern each file are found and kept true, and the PR loop that lands a change.
[REVIEW.md](REVIEW.md) is the reviewer's brief.

<a id="build"></a>
## Build from source
<!-- @section
-->

You need Rust through `rustup`; `rust-toolchain.toml` pins the version and
adds the Windows and `wasm32` targets. The daemon runs live on Windows, macOS and 64-bit
Linux, on each OS's media backends; elsewhere it serves the API with placeholder
outputs. Everything builds on every OS, and the tests run on fakes.

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
- The web UI and the desktop app: [jstream-ui › Build](crates/jstream-ui/README.md#build)
  and [jstream-desktop › Usage](apps/jstream-desktop/README.md#usage).

<a id="status"></a>
## Status
<!-- @section
-->

JStream ships as numbered pre-releases on the
[releases page](https://github.com/jeffmhopkins/JStream/releases), each with its notes and
known issues. The alphas built the feature set; the first beta, `0.1.0-beta.1`, adds no
new features and makes what the alphas ship dependable on Windows, macOS and Linux. The
control protocol may still change between pre-releases, and a project saved by one may not
open in the next.

- [PROGRESS.md](PROGRESS.md): the release being built now, every roadmap item by
  milestone, and what is left, generated from [`docs/progress.toml`](docs/progress.toml)
- [ROADMAP.md](ROADMAP.md): the milestones, the releases, and what done means

<a id="documentation"></a>
## Documentation
<!-- @section
-->

**Using JStream**

- [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md): a first scene, going live,
  recording, the virtual camera and connecting an agent
- [docs/runbooks/go-live.md](docs/runbooks/go-live.md): taking the daemon live on Windows,
  and the smoke script that checks it
- [docs/runbooks/mac-testing.md](docs/runbooks/mac-testing.md) and
  [docs/runbooks/linux-testing.md](docs/runbooks/linux-testing.md): installing JStream on
  macOS and Linux
- [jstream-mcp](crates/jstream-mcp/README.md): every MCP tool an agent can call

**How it works**

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): how the system fits together
- [docs/adr/](docs/adr/): decision records
- [docs/security/threat-model.md](docs/security/threat-model.md): what JStream protects, from
  whom, the controls at each trust boundary, and the open gaps

**Working on JStream**

- [CLAUDE.md](CLAUDE.md): the entry point for builder agents, and
  [REVIEW.md](REVIEW.md): the reviewer agent's brief; its verdict gates merges
- [docs/RULES.md](docs/RULES.md): engineering rules, each with a permanent ID
- [docs/DOCS.md](docs/DOCS.md): how docs stay accurate (hash-checked sections, history from git)
- [docs/runbooks/release.md](docs/runbooks/release.md): versioning and publishing a release
- [docs/runbooks/waves.md](docs/runbooks/waves.md): which wave branch a PR targets, and
  how a wave lands in `main`
- [docs/runbooks/windows-hardware-pass.md](docs/runbooks/windows-hardware-pass.md): every
  open Windows hardware run as one checklist, with the issue each one closes
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
