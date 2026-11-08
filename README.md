<!-- @doc
id = "README"
title = "JStream"
kind = "normative"
status = "active"
review_every = "90d"
retired_sections = { "today" = [] }
-->

# JStream

[![CI](https://github.com/jeffmhopkins/JStream/actions/workflows/ci.yml/badge.svg)](.github/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/jeffmhopkins/JStream?include_prereleases&label=release)](https://github.com/jeffmhopkins/JStream/releases)
[![Status: pre-release](https://img.shields.io/badge/status-pre--release-orange)](#progress)
[![Built with Rust](https://img.shields.io/badge/built%20with-Rust-dea584)](rust-toolchain.toml)
[![Agent control: MCP](https://img.shields.io/badge/agent%20control-MCP-8a2be2)](#agents)
[![Licence: MIT OR Apache-2.0](https://img.shields.io/badge/licence-MIT%20OR%20Apache--2.0-blue)](#license)

> A live-streaming and compositing engine written in Rust, built from the ground up to be
> driven equally well by a person or by an AI agent.

> [!WARNING]
> **JStream is pre-release software under heavy development.** The releases are alphas:
> expect bugs, missing features, and scenes or settings that a later version can't open.
> The control protocol still changes from release to release. Don't rely on it for a show
> that matters yet. [Progress](#progress) shows how far each milestone has got.

> [!NOTE]
> **Known issues in alpha.4.1 on Windows.** Snapshots and frame sheets draw browser pages
> and captures black (#1184). Small inset sources, animated overlays and recordings can
> still look soft (#1217). Hotkeys aren't heard while another app has the focus (#1188),
> and the follow-pointer camera can lose the cursor (#1216). alpha.4.1 fixed the rest of
> alpha.4's list: browser sources and CLAP plugins from the zip (#1213), dropped frames and
> a soft editor preview (#1193, #1233). What's left leads the alpha.5 work below.

> [!TIP]
> **Human?** Start at [For humans](#humans): download an alpha, build a first scene and go
> live with [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md).

> [!IMPORTANT]
> **Agent?** Start at [For agents](#agents). To drive JStream, connect over MCP. To change
> JStream's code, read [CLAUDE.md](CLAUDE.md) first.

<a id="what"></a>
## What JStream is
<!-- @section
-->

JStream composes scenes from your cameras, screens, media, browser pages and graphics on
the GPU, mixes their audio, and sends the result to a stream, a recording or a virtual
camera. It does the job of a streaming studio such as OBS, written again from scratch
around one idea: **everything is an API**.

- **Stream** over RTMP or RTMPS to YouTube, or to any service that takes a stream key.
  **Record** to a file at the same time.
- **Virtual camera**: an OS-native Windows camera that every video-call app can see.
- **Scenes and sources** on a wgpu compositor: cameras, screen and window capture, media
  files, images, text, colour and browser sources, with crop, 3D transforms and blend
  modes.
- **Effects**: built-in filters on sources, items, scenes and outputs, and shader
  sources written as WGSL effect packs.
- **3D scenes**: a scene can have a camera that the items sit in front of, moving through
  them, with depth of field.
- **Transitions** with a studio preview, and keyframe scenes that animate between
  layouts, and native motion, one-shot and audio-reactive, on items and cameras.
- **Independent routing**: outputs take video mixes and audio buses, not scenes, so a
  stream and a recording can show different things, and audio runs through a bus mixer.
- **Desktop or headless**: the same engine runs inside the desktop app or as a daemon that
  serves the web UI to any paired browser.

<a id="different"></a>
## How it differs from OBS
<!-- @section
-->

OBS is mature, battle-tested and has a huge plugin ecosystem; JStream is an alpha and has
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
| [![A scene of stacked browser pages, recorded in JStream](https://raw.githubusercontent.com/jeffmhopkins/StreamingScenes/main/starship-cockpit/media/nebula.jpg)](https://github.com/jeffmhopkins/StreamingScenes/raw/main/starship-cockpit/media/cockpit-jstream.mp4) | **A layered scene, recorded live.** A starship cockpit stacked from browser pages and images, with live cameras on its monitors; its effects and music-driven lights are the scene's own pages. Recorded on Linux with JStream's own `record` output. |
| [![Depth of field through a scene camera](https://raw.githubusercontent.com/jeffmhopkins/StreamingScenes/main/camera-demos/media/depth-of-field.jpg)](https://github.com/jeffmhopkins/StreamingScenes/raw/main/camera-demos/media/depth-of-field.mp4) | **Depth of field.** Cards at three depths seen through a scene camera; the focus pulls from one to the next and the blur grows and shrinks. |
| [![A screen capture seen up close through a moving camera](https://raw.githubusercontent.com/jeffmhopkins/StreamingScenes/main/camera-demos/media/screen-closeup.jpg)](https://github.com/jeffmhopkins/StreamingScenes/raw/main/camera-demos/media/screen-closeup.mp4) | **A 3D screen close-up.** A screen capture placed flat in a 3D scene, with the camera gliding between close, angled views of it. |

The two camera clips were rendered frame by frame by JStream's compositor, outside the app.

<a id="humans"></a>
## For humans
<!-- @section
-->

> [!NOTE]
> Windows 11 is the first platform. macOS and Linux builds exist for testers, but their
> platform work is still in progress; see [Platforms](#platforms).

1. **Try it.** Download the latest alpha from the
   [releases page](https://github.com/jeffmhopkins/JStream/releases), unzip it and run
   `jstream.exe`. There is no installer.
2. **Learn it.** [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md) walks through a first
   scene, going live, recording, the virtual camera and connecting an agent.
3. **Report what breaks.** Bugs and ideas are welcome as
   [issues](https://github.com/jeffmhopkins/JStream/issues/new); security problems go
   privately, as [SECURITY.md](SECURITY.md) says. Until the first release JStream takes no
   outside code contributions ([CONTRIBUTING.md](CONTRIBUTING.md)).

To build from source instead, see [Build and run](#build).

<a id="agents"></a>
## For agents
<!-- @section
-->

> [!IMPORTANT]
> There are two ways an agent meets JStream: **driving** a running JStream over MCP, or
> **building** JStream itself in this repo. They have different entry points.

**Driving JStream.** Start JStream with `--mcp` (or run the daemon), then point your MCP
client at it: through `jstream-cli mcp`, a stdio proxy, or straight at the streamable HTTP
endpoint with the token as a bearer token.
[GETTING-STARTED › Connect an agent](docs/GETTING-STARTED.md#agents) has the steps for
Claude Desktop and Claude Code. Every tool is described in
[jstream-mcp](crates/jstream-mcp/README.md), and the protocol behind them in
[ARCHITECTURE › Control protocol](docs/ARCHITECTURE.md#protocol).

**Building JStream.** JStream is built by AI builder agents and gated by a reviewer agent.
Read [CLAUDE.md](CLAUDE.md) before changing anything: it explains how the docs, rules and
ADRs that govern each file are found and kept true, and the PR loop that lands a change.
[REVIEW.md](REVIEW.md) is the reviewer's brief.

<a id="progress"></a>
## Progress
<!-- @section
-->

Generated from [`docs/progress.toml`](docs/progress.toml) by `cargo xtask docs generate`
and drift-checked by `docs check`, so it changes in the same PR as the work.

Progress is shown two ways. **Next release** is the alpha being built now: what it aims
for, in the maintainer's priority order, with the issues and PRs behind each target.
Shipped alphas aren't tracked here; their notes are on GitHub's releases page. **Whole
app** groups every roadmap item by the roadmap's milestones.

JStream is done when every roadmap item below is merged and has passed its Windows run, where it
needs one.
[ROADMAP › Endgame](ROADMAP.md#endgame) defines that and gives the order the rest lands
in, and **What's left** lists each unfinished item with the steps it still needs.

<!-- @generated begin progress -->
### Next release: `0.1.0-alpha.5`

A seamless, full-quality preview on a thread model that uses more cores, proved by a bench, then pop-out projectors, application settings and saved projects. The latest shipped release is `v0.1.0-alpha.4.2`. Its progress counts its targets and any roadmap items it carries. Priority is the maintainer's order, 1 first; a ❔ target may move to the release after, so it isn't counted.

| Progress | Done | In progress | Planned | Undecided |
|---|---|---|---|---|
| `█░░░░░░░░░` 10% | 2 | 1 | 18 | 0 |

| Priority | Target | Status | Issues | PRs |
|---|---|---|---|---|
| 1 | A thread model that uses more cores: CPU frame work off the render thread, the preview reusing the program frame, upcoming scenes kept warm, and audio threads that never wait on video | ⏳ planned: measured, then decided in an ADR; no further preview fix is built until it lands | #1221 | #1235, #1236 |
| 1 | jstream bench: standard scenes on real hardware that pass or fail on frame rate, drops, preview restarts and picture quality | ⏳ planned: each Windows run opens with it, and a fail blocks the release | #1220, #1140 | #1260, #1315 |
| 1 | A seamless, full-quality preview: no placeholder, blank or stall on a scene switch, an effectively lossless local preview, and no dropped frames in the preview, stream or recording | ⏳ planned: #1205's placeholder was fixed in alpha.4.1 (#1233); the bench checks the whole bar | #1205, #1217 |  |
| 1 | Animated sources plan once a frame, and a small still image costs what it draws | ⏳ planned | #1138, #1139 |  |
| 2 | Pop-out projectors: the program, the preview or a scene in its own window or full screen on another monitor, at full quality and frame rate | ⏳ planned: #1199 merged; the Windows check is #1208 | #1208 | #1199 |
| 3 | An Application Settings dialog: plugin management, Check for updates, a CLAP plugins page, project video settings, and one set of blend modes | ⏳ planned: #1210's check and the launch setting are in #1249; plugins, the CLAP page, project video settings and blend modes are still open | #1207, #1210, #1212, #1192, #1230 | #1249 |
| 4 | Save and load projects: File › New, Open, Save As and Open recent | ⏳ planned: #1269 is the protocol and engine part (ADR-0121); the File menu follows | #1218 | #1269 |
| 5 | A UI scrub of every panel, dialog and window, with a decluttered Inspector and Properties and Filters in one dialog | ⏳ planned: #1279 and #1373 redesign the Outputs panel (#1206); #1219, #1198, #1202 and #1204 remain | #1219, #1198, #1202, #1204, #1206 | #1279, #1373, #1389 |
|  | Snapshots and frame sheets draw browser pages and screen captures on Windows | ⏳ planned | #1184, #1194, #1215 |  |
|  | Follow pointer as a filter on a capture that keeps the cursor in view, a working 3D camera section, and hotkeys heard while another app has the focus | ⏳ planned: #1251 lays out the 3D camera section (#1214); #1270 makes Follow pointer a filter with a safe margin (#1225, #1216); #1278 hears bound hotkeys while another app has the focus (#1188), and #1329 does on Linux under X11 (#1325); #1343 adds wheel chords such as Ctrl+WheelUp, heard in the page (ADR-0112 §3); #1398 counts a notch at Chromium's measured 120 px (#1346); #1399 hears wheel chords on X11 while a follow camera is live, binds them from Set key and offers the Ctrl+wheel defaults (ADR-0112 §3–§5); #1402 hears them on Windows with a WH_MOUSE_LL hook while a follow camera is live | #1225, #1216, #1214, #1188, #1325, #1346 | #1251, #1270, #1278, #1329, #1343, #1398, #1399, #1402 |
|  | Looping media files play without a gap at the wrap | ⏳ planned | #445 |  |
|  | Small fixes: the Cut preset cuts, and the alpha.4 run's small MCP findings | ⏳ planned | #1209, #1189 | #1240, #1245, #1255, #1257 |
|  | Filters: a Glow that shows on opaque items, Bloom, a Vignette that is easy to tune, a full set of borders, and a Custom shader filter | ⏳ planned: #1281 brings Glow past the box, Bloom and the Vignette (#1226); #1296 adds the full set of borders (#1227) and #1352 its 9-slice image frame (#1294); #1289 is the first part of the Custom shader (#1232), #1331 colours its editor and #1351 adds Save as pack and setting renames (#1295) | #1226, #1227, #1232, #1295 | #1281, #1289, #1296, #1331, #1351, #1352 |
|  | Page API: audio levels and spectrum bands for HTML visualisers, and a checkbox for each kind of data a page may read | ⏳ planned | #1228, #1060 |  |
|  | Webcam Properties: a device picker, resolution, frame rate and format, and camera controls | 🔍 in review | #1195, #1196 | #1262, #1348, #1393, #1396 |
|  | Ask before a new agent connects, with always-allowed agents listed in Settings | ⏳ planned | #1229 |  |
|  | Richer 3D scenes: glTF and GLB models with materials, lights, fog and depth-aware effects | ⏳ planned: finishes roadmap item 3D3 | #1231 |  |
|  | MIDI and network audio: tempo sync, one binding system for hotkeys, MIDI and controllers, a network audio source, and MIDI visualization sources | ⏳ planned | #1222, #1223, #1224 | #1272, #1273, #1299, #1374, #1375, #1415, #1427 |
|  | PTZ cameras over VISCA-over-IP, with their presets in keyframes | ⏳ planned | #1182, #1181 |  |

<details open>
<summary><b>Roadmap items in 0.1.0-alpha.5</b>: 2 of 2 merged</summary>

A gate item (🚧) is one the release waits for.

| ID | Item | Gate | Milestone | Status | PRs |
|---|---|---|---|---|---|
| PA3 | Page API part 3: a bus's levels and bands for pages (jstream.levels) |  | M3 | 🧪 merged, Windows run pending: Merged; the Windows run's page API step (#1147) checks a granted bus with real audio | #1263 |
| PA4 | Page API read review: a checkbox and a visible warning per kind of data, and a badge (ADR-0124) |  | M3 | ✅ merged | #1287 |

</details>

### Whole app

Every roadmap item, by milestone.

| Milestone | Progress | Merged | In review | Planned |
|---|---|---|---|---|
| [M0: Foundation and risk spikes](ROADMAP.md#m0) | `██████████` 100% | 11 | 0 | 0 |
| [M1a: Thin slice to YouTube](ROADMAP.md#m1) | `██████████` 100% | 18 | 0 | 0 |
| [M1b: Scenes and mixing](ROADMAP.md#m1) | `██████████` 100% | 39 | 0 | 0 |
| [M1c: Virtual camera](ROADMAP.md#m1) | `██████████` 100% | 7 | 0 | 0 |
| [M1d: Hardening](ROADMAP.md#m1) | `█████████░` 91% | 10 | 0 | 1 |
| [M2: UI](ROADMAP.md#m2) | `██████████` 96% | 27 | 0 | 1 |
| [M3: Depth and reach](ROADMAP.md#m3) | `████████░░` 80% | 35 | 0 | 9 |

147 of 158 items are merged; 22 of those still wait on a run on Windows hardware.

<details open>
<summary><b>What's left</b>: 33 items, by next step: 🔨 build 12, 🧪 Windows run 20, 👤 maintainer 1</summary>

The order these land in, and what done means, is in [ROADMAP › Endgame](ROADMAP.md#endgame).

| ID | Item | Milestone | Needs, in order | Note | PRs |
|---|---|---|---|---|---|
| A4 | Go-live runbook and smoke | M1a | 🧪 Windows run | the go-live smoke and graceful stop (item 11 of the Windows hardware pass); its live YouTube steps are #337 | #234 |
| A6b | Daemon YouTube wiring | M1a | 👤 maintainer, then 🧪 Windows run | #312 landed with wave/m1-2 (#545); sign-in needs the OAuth client (ADR-0058); Windows run #337 | #312 |
| C4c | Webcam source (CPU path, MJPEG on Windows, paired audio) | M1b | 🧪 Windows run | landed with wave/m1-3 (#754); a Windows run with a webcam and its paired microphone is still pending (#673 covers the MJPEG webcam) | #654, #658, #726 |
| C5 | Audio sources | M1b | 🧪 Windows run | the WASAPI device tests on real devices (#778, item 18 of the Windows hardware pass) | #238 |
| C8 | Browser source (CEF) | M1b | 🔨 build, then 🧪 Windows run | the Windows runs #335 (steps 1 to 4) and #336 passed; #341 passed except its refused-request count (2, not 1), so page_check needs to name the refused URLs, then a rerun; Ctrl-C with CEF running skips the graceful stop (#436), a fix to build, then #335's step 5 runs again | #283, #289, #298, #326 |
| D9 | Audio monitoring: a bus's monitor device and tap point | M1b | 🧪 Windows run | ADR-0028 gives a bus a monitor, a device and a tap point before or after its filters (decision register D22). #801 is the mixer's monitor taps and player, #816 the protocol's bus monitor and bus.set_monitor (0.42), and #822 the engine playing it on its device, all landed with wave/m1-5 (#877); the mixer's Hear: before or after FX row (MX-05) can then be built. #822 ran only against the simulated player and the fake device, so a run on a real output device (WASAPI, Core Audio, PipeWire) is still owed: playback, drift steering and an unplug mid-stream | #801, #816, #822 |
| G2 | vcam media source DLL | M1c | 🧪 Windows run | the pacing rerun (#777, with item 6 of the Windows hardware pass) | #174, #195, #200, #229 |
| G3c | --register-vcam | M1c | 🧪 Windows run | Windows run (#563) | #235, #565 |
| H8 | Hardware acceleration by default, with GPU fallback and device-loss recovery | M1d | 🧪 Windows run | #437 on main; the encoder's software fallback (#568), the mid-run GPU capture fallback (#569, #580), device-loss recovery (#604) and sessions after a fallback (#708) landed with wave/m1-3 (#754); landed with wave/m1-5 (#877): a full hardware session limit failing output.start with hardware_session_limit instead of falling back (#829, #443 case 2, protocol 0.44); Windows runs: the mid-run fallback (#576) and a forced TDR (#620); #734's webcam restart fixed by #796 in the same landing; open follow-up #443 (#667 fixed by #672 and #694) | #437, #568, #569, #580, #604, #708, #829 |
| Q6 | Threat-model follow-ups | M1d | 🔨 build, then 🧪 Windows run | from the Q5 threat model: user-only token, devices and log files (#548) and crash reports (#662) on Windows, browser pages refused every permission and local-network requests decided by policy (#535, #549, #714, #729), the vcam's owner checks (#540), local redacted desktop crash reports (#552) and a warning on cleartext rtmp:// (#555) landed with wave/m1-3 (#754); the audit log and collection file user-only (#574, for #547) and the rtmp:// flag in output.status (#653, for #562) landed with wave/m1-4 (#810); the CLAP IPC hardening (#573, for #351) landed with wave/m3-4 (#827); #349's device-loss event and effect-pack quarantine (#976, after X1's packs in #971), its GPU-time guard and a forced TDR on Windows hardware still open; open: #344, #347 and #353; Windows runs #546 and #557 | #535, #540, #548, #549, #552, #555, #662, #714, #729, #573, #574, #653, #976 |
| U4p | Live preview on canvas | M2 | 🧪 Windows run | the canvas's H.264 preview path on Windows (#380) | #302 |
| T1 | Studio mode and transitions | M2 | 🧪 Windows run | #260 landed with wave/m1-3 (#754); the transition node's per-tick cost is still to measure on S1 hardware (#357, ADR-0029) | #260 |
| C9 | Media file source | M2 | 🧪 Windows run | the media file run passed on main (#382) except an MP3's loop seam, a 56 ms gap at each wrap (#445): #875 cuts it from the LAME tag; a rerun checks it, and the ~12 ms dropout 0.42 s into each pass | #245, #322, #875 |
| U6 | Desktop UI as a 1:1 match of the UI prototype | M2 | 🔨 build | #748; docs/UI-FEATURES.md tracks it row by row. Theme and header (#495), docking (#496), the canvas (#615, #637), the Inspector (#659) and the Transform panel (#664) landed with wave/m1-3 (#754); menus, dialogs and live drag (#766, #747), the Scenes and Scene objects panels (#770), the Filters panel and its dialogs (#785, #788) and the mixer master card (#792) landed with wave/m1-4 (#810); vertical mixer strips (#809) landed with wave/m1-5 (#877); crop on the canvas and source sizes (#869), scene folders, groups and reference ↔ clone (#871), the program output and output chains (#864), the keyframe rows, menus and banner with scene hotkeys (#865), the privacy screen's button, overlay, mic label and Pause key (#866, since removed with the privacy screen, ADR-0080), the polish batch of labels, toasts, fold memory, 3D fields and the eleven panels (#867), mixer monitoring with the Hear button (#863, MX-05), which completes the mixer rows, and studio mode, the scene transitions and the scene Properties dialog (#868) landed with wave/m1-6 (#887); copy and paste (KB-05), Duplicate scene and the canvas's live picture of any scene (CV-03) and the scene-input filter rows (OB-05) are #911; the Agent activity panel and the engine's activity log over the protocol (AG-01, D38, ADR-0083) are #938. Rows that wait on the engine follow B12, V4, D9, K1, X1 and Y4; the IBM Plex fonts are bundled under the SIL OFL 1.1 (WS-02, ADR-0077, #886, landed with wave/m1-6); every blend mode drawn (CV-15, ADR-0082, #918); fps and CPU in the header from stats.tick's load (HD-04, protocol 0.48, #930); Pop out of a source alone on an object's menu (DL-07, CM-02, CM-04, protocol 0.56, #969); the Stream stats panel's own-output half from stats.tick's per-output stats (ST-01 in part, D39, protocol 0.57, #972); video filters on the program output and each output (FL-07, protocol 0.60, #973); encoder profiles and video mixes edited in the Outputs panel, with a Video select on each output card (#992); the Stream stats chart's hover readout (ST-01, #996); the scene Properties dialog's background colour, Show in multiview and Include its audio when nested (DL-03, ADR-0089, protocol 0.65, #986) | #495, #496, #615, #637, #659, #664, #766, #770, #785, #788, #792, #809, #863, #864, #865, #866, #867, #868, #869, #871, #886, #911, #918, #930, #938, #969, #972, #973, #986, #992, #996 |
| 3D3 | 3D models with materials and live textures | M3 | 🔨 build | ADR-0107 (#1112): a `model` source kind (glTF files and built-in shapes), lit with a studio rig or unlit, drawn through the scene camera with its own depth, filters and effects over its layer, other sources as its texture and reflection, and picking by its 3D bounds | #1112, #1113, #1118, #1261 |
| Y4 | YouTube chat, metadata, health and audience stats | M3 | 🔨 build, then 👤 maintainer | chat reading and the featured message (#363, landed with wave/m1-2, #545); chat preset of the browser source (#375, landed with wave/m1); audience stats in jstream-youtube (#594, landed with wave/m3-2, #663); the engine's stats polling, the youtube.stats event and query, and the quota estimate (#696, protocol 0.38, landed with wave/m1-4, #810); the chat rate waits on the engine polling chat; signed-in features need the OAuth client (ADR-0058) | #363, #375, #594, #696 |
| CL1 | CLAP plugin host | M3 | 🔨 build, then 🧪 Windows run | parts 1 to 3 merged (wave/m3 landed in #477): the shared-memory contract, the CLAP wrapper, host app, launcher, supervisor and CLAP filter kinds; the host's Windows run passed in #342; CLAP filters in a source's audio chain, part 4 (#488), and the IPC hardening (#573, for #351) landed with wave/m3-4 (#827); CLAP filters on the master's and outputs' chains and in the UI's filter picker (FL-08) built in #913; then the live-audio Windows run (#912) | #272, #281, #407, #488, #573, #913 |
| WA1 | WASM automation plugins | M3 | 🔨 build | WASM host #486 landed with wave/m3-2 (#663); its wall-time deadlines #623, the cap of 50 commands a second #722, the 1,000-event queue #725 and the wire half #642 (protocol 0.35) landed with wave/m3-3 (#786); the engine side part 1, installing, granting, events, forbidden and disabling (#839, ADR-0075), and part 2, saving installed plugins, grants held to the manifest's classes and hosts, needs_grant, memory_limit and the MCP state-text line (#843, ADR-0076), landed with wave/m3-5 (#854); the plugin.list query and the UI's Plugins section with its grant dialog landed (#974, protocol 0.58); removing a plugin and a private data folder wait, the folder for its own design with the file imports it needs | #486, #623, #642, #722, #725, #839, #843, #974 |
| MAC1 | macOS platform crate | M3 | 🔨 build | ADR-0063 accepted (#511); crate part 1 (#513); VideoToolbox H.264/HEVC encoder, part 2 (#533); AudioToolbox AAC-LC encoder, part 3 (#543); ScreenCaptureKit display and window capture with the Screen Recording permission check, part 4 (#554); Keychain store for stream keys and OAuth tokens, part 5 (#566); Core Audio devices and input recording, part 6 (#570); Microphone permission check and the daemon's embedded Info.plist, part 7 (#583); Core Audio output playback, part 8 (#586); all landed with wave/m3-2 (#663); with wave/m3-3 (#786): the list of processes Core Audio knows, part 9 (#589); desktop and application audio through Core Audio process taps, part 10 (#599); AVFoundation webcams behind the Camera permission, part 11 (#608); VideoToolbox H.264/HEVC decoder on CPU frames (#618); media files through AVFoundation's asset reader (#625); the daemon live on macOS (#639); spike S6, IOSurface and Metal interop and its synchronisation, for ADR-0063 decision 8 (#646, its Mac run in #648); a one-command install that builds JStream on a tester's Mac and runs it from Terminal (#1093); per-thread CPU for the performance profile (#1135); the rest of the crate to build | #511, #513, #533, #543, #554, #566, #570, #583, #586, #589, #599, #608, #618, #625, #639, #646, #1093, #1135 |
| LNX1 | Linux codec path and platform crate | M3 | 🔨 build, then 🧪 Windows run | ADR-0060 accepted (#481); crate part 1 (#490); portal and PipeWire screen capture (#509); VA-API H.264 encode (#510); these landed with wave/m3-2 (#663); with wave/m3-3 (#786): ALSA audio input and output (#572); portal restore tokens per source (#598); DMA-BUF capture frames imported into Vulkan (#606); VA-API H.264 encode from the Vulkan device's NV12 encode targets (#652); PipeWire audio input, output and loopback (#593); the engine's capture per source, which starts each source with its own restore token, forgets it when the source is deleted and honours `show_cursor` (#682, protocol 0.36); with wave/m3-4 (#827): PipeWire processing on the real-time data loop (#612); V4L2 webcams and capture cards (#603); multi-planar V4L2 capture (#617); the pointer as PipeWire cursor metadata or left out, platform only (#610), on the CPU and DMA-BUF paths; the source's show_cursor setting (protocol 0.36, #682) has the OS draw the pointer when it's on and leave it out when it's off (WGC leaves it out too, with #682, and ScreenCaptureKit with #685), and the engine doesn't draw the pointer; webcams paired with their microphones and the Linux backend set choosing PipeWire or ALSA (#689); stream keys and OAuth tokens in the Secret Service, else a 0600 file, ADR-0071, not yet used by an app (#634); capture copies and NV12 submits ordered on the context's queue (#694); with wave/m3-5 (#854): VA-API HEVC encode (#830) and its hardening (#835), still to run on Intel or AMD hardware; the daemon goes live on the Linux backends, with stream keys in LinuxSecrets and OpenH264 taking the compositor's RGBA frames (#1016); a one-command install that builds JStream on a tester's Linux machine, with browser sources and an app-menu entry (#1103); the rest of the codec path to build | #481, #490, #509, #510, #572, #598, #606, #652, #593, #612, #603, #617, #610, #685, #689, #634, #694, #830, #835, #682, #1016, #1103 |
| LNX3 | AAC-LC encoder in Rust, patent check first (ADR-0061) | M3 | 🔨 build | patent check ADR-0062 accepted (#508); encoder and Linux wiring (#518), landed with wave/m3-2 (#663); TNS (#723, more tests #727) landed with wave/m3-3 (#786); intensity stereo (#799) landed with wave/m3-4 (#827); before the Linux app ships the encoder its licence information must say JStream holds no AAC patent licence and name Via LA (ADR-0062) | #508, #518, #723, #727, #799 |
| F6b | Choose HEVC or AV1 from a client | M3 | 🔨 build | F6 built the encoders; #807 names the encoder a machine lacks at output.start, landed with wave/m3-4 (#827); #814 (protocol 0.41) adds hevc and av1 to VideoCodec and lets an encoder profile take them, landed with wave/m1-5 (#877) | #807, #814 |
| F7 | Direct NVENC and AMF encoders, if measurements justify them | M3 | 🔨 build | ROADMAP M3: measure Media Foundation's hardware encoders against direct NVENC and AMF first, and build the direct ones only where the numbers say so |  |
| X2 | Shader sources: multipass WGSL packs with feedback, inputs and audio, written over MCP | M3 | 🧪 Windows run | ADR-0093 (agent tier) and the program in jstream-effects and jstream-render (#1034), and the engine wiring: kinds, stand-ins, scene slots, restart on show and audio (#1048); writing packs over the protocol and MCP is decided by ADR-0097 (maintainer tier, accepted): the quarantine follows the shader digest (#1064), and pack.write, pack.delete, pack.list and pack.read (protocol 0.72, #1085), whose Windows and macOS run is item 20 of the Windows hardware pass (#1088); for the cockpit scenes (warp, reflections, audio glow) that browser sources draw on the CPU today | #1034, #1048, #1050, #1052, #1064, #1085 |
| MO2 | Native motion: continuous wiggle, oscillate and drift on items, drawn each frame (item.set_motion) | M3 | 🧪 Windows run | Protocol 0.81; landed with wave/alpha4 (#1176), with camera motion and the follow-cursor camera (#1157, ADR-0113, 0.85); the alpha.4 Windows run's step 4 (the cockpit rebuild over MCP) checks it on hardware, and step 5e the follow camera (#1159) | #1125, #1176 |
| MO3 | A one-shot shake that a hotkey fires and that decays | M3 | 🧪 Windows run | item.trigger_motion, protocol 0.84; landed with wave/alpha4 (#1176); the alpha.4 Windows run's step 4 (the cockpit rebuild over MCP) checks it on hardware | #1156, #1176 |
| MO5 | Audio-reactive motion modifiers drawn from the render loop | M3 | 🧪 Windows run | Protocol 0.86; landed with wave/alpha4 (#1176); the alpha.4 Windows run's step 4 (the cockpit rebuild over MCP) checks it on hardware, with real audio | #1149, #1176 |
| HK1 | Hotkeys show, hide and toggle one item, with the item's own effects (ADR-0101) | M3 | 🧪 Windows run | Protocol 0.80; landed with wave/alpha4 (#1176); the alpha.4 Windows run's step 4 (the cockpit rebuild over MCP) checks it on hardware | #1082, #1176 |
| FS1 | Frame sheets, so an agent can check motion (video.frames, scene.switch_and_watch) | M3 | 🧪 Windows run | Protocol 0.82; landed with wave/alpha4 (#1176); the alpha.4 Windows run's step 4 (the cockpit rebuild over MCP) checks it on hardware | #1080, #1176 |
| PA2 | Page API part 2: window.jstream with clock, visibility, scenes and values | M3 | 🧪 Windows run | Merged; step 5f of the alpha.4 Windows run checks it on Windows (#1147) | #1146, #1178 |
| PA3 | Page API part 3: a bus's levels and bands for pages (jstream.levels) | M3 | 🧪 Windows run | Merged; the Windows run's page API step (#1147) checks a granted bus with real audio | #1263 |
| CB3 | Opacity above 1 as a gain, so additive items can add more than their source | M3 | 🧪 Windows run | Protocol 0.83; landed with wave/alpha4 (#1176); the alpha.4 Windows run's step 4 (the cockpit rebuild over MCP) checks it on hardware | #1104, #1176 |
| RL4 | Release 0.1.0-alpha.4: the version bump, then the acceptance run on Windows | M3 | 🧪 Windows run | Released as v0.1.0-alpha.4 (#1179); the acceptance run on Windows, building a layered, animated scene over MCP and reading its profile, runs on the released build | #1179 |

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
| A3 | Daemon goes live on Windows | ✅ merged: the maintainer waived the Windows 11 Pro N run: there is no N-edition VM, and #333 stays open to keep the gap on record | #226 |
| A6 | Daemon --collection and audit log | ✅ merged | #232 |
| A4 | Go-live runbook and smoke | 🧪 merged, Windows run pending: the go-live smoke and graceful stop (item 11 of the Windows hardware pass); its live YouTube steps are #337 | #234 |
| E3a | GPU capture frames | ✅ merged | #173 |
| E3b | Render takes GPU frames | ✅ merged | #219 |
| Y1 | jstream-youtube crate | ✅ merged | #167 |
| Y2 | OAuth token store | ✅ merged | #169 |
| YA | ADR-0041 one sign-in | ✅ merged | #214 |
| Y3 | YouTube in the engine | ✅ merged | #223 |
| A6b | Daemon YouTube wiring | 🧪 merged, Windows run pending: #312 landed with wave/m1-2 (#545); sign-in needs the OAuth client (ADR-0058); Windows run #337 | #312 |

</details>

<details>
<summary><b>M1b</b> Scenes and mixing: 39 of 39 merged</summary>

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
| C4c | Webcam source (CPU path, MJPEG on Windows, paired audio) | 🧪 merged, Windows run pending: landed with wave/m1-3 (#754); a Windows run with a webcam and its paired microphone is still pending (#673 covers the MJPEG webcam) | #654, #658, #726 |
| C1a | Sources crate: color, image | ✅ merged | #179, #217 |
| C1b | Sources render live | ✅ merged | #222 |
| C2 | Text source | ✅ merged | #221 |
| C5 | Audio sources | 🧪 merged, Windows run pending: the WASAPI device tests on real devices (#778, item 18 of the Windows hardware pass) | #238 |
| C3 | Display and window capture sources | ✅ merged: #365 landed with wave/m1-2 (#545); its Windows run passed (#378) | #365 |
| C8 | Browser source (CEF) | 🧪 merged, Windows run pending: the Windows runs #335 (steps 1 to 4) and #336 passed; #341 passed except its refused-request count (2, not 1), so page_check needs to name the refused URLs, then a rerun; Ctrl-C with CEF running skips the graceful stop (#436), a fix to build, then #335's step 5 runs again | #283, #289, #298, #326 |
| D1 | Live mixer inputs | ✅ merged | #149, #199 |
| D3 | Loudness meters | ✅ merged | #152 |
| D7a | Filter kernels | ✅ merged | #159 |
| D2 | Buses and send matrix | ✅ merged | #192 |
| D4 | No-allocation test | ✅ merged | #191 |
| D7b | Filter chains in the mixer | ✅ merged | #231 |
| V0 | Video filter chain runtime | ✅ merged: part 1 runs the built-in kinds, part 2 loads apply_lut and image_mask assets, part 3 gives scene_mask its scene, part 4 pools scratch layers and each draw's uniform buffers (#712, #718, landed with wave/m1-4, #810), and #730 uploads LUT tables directly; effect-pack kinds are drawn, with the packs folder watched by the engine (#971). No bounds inset: on a source chain shadow, outline and glow draw within the frame (ADR-0065 §5) | #595, #607, #626, #712, #717, #718, #730, #971 |
| D5 | Audio in the protocol | ✅ merged | #240 |
| D6 | when_shown activity gain | ✅ merged | #299 |
| E1 | Render snapshots, GPU lifetime | ✅ merged | #172, #215 |
| F1 | fMP4 muxer | ✅ merged | #151 |
| F2a | Recording file sink | ✅ merged | #156 |
| F2b | Record output kind | ✅ merged | #170 |
| F3 | Encoder profiles and sharing | ✅ merged | #208 |
| I2 | Live values | ✅ merged | #244 |
| V4 | Scene filter chains and item effects | ✅ merged: ADR-0065 designs a video chain owned by a scene or by one placement, and ADR-0068 puts item effects in v1 (#529). Landed with wave/m1-5 (#877): a vignette kind (#802), the protocol part, the scene and item owners at 0.43 (#813's change, landed through #815), the graph and mock holding them at collection schema 2 (#815), the engine running scene filters over the scene's composite (#823), and item effects over each item's box (#824). The UI's scene filters (FL-06) merged (#897), then vignette goldens at every level (#921), stats.tick's counts of scene-chain and item-effect layers (#944), and the UI's item effects with the vignette in the filter picker (FL-09, #968) | #802, #815, #823, #824, #897, #921, #944, #968 |
| D9 | Audio monitoring: a bus's monitor device and tap point | 🧪 merged, Windows run pending: ADR-0028 gives a bus a monitor, a device and a tap point before or after its filters (decision register D22). #801 is the mixer's monitor taps and player, #816 the protocol's bus monitor and bus.set_monitor (0.42), and #822 the engine playing it on its device, all landed with wave/m1-5 (#877); the mixer's Hear: before or after FX row (MX-05) can then be built. #822 ran only against the simulated player and the fake device, so a run on a real output device (WASAPI, Core Audio, PipeWire) is still owed: playback, drift steering and an unplug mid-stream | #801, #816, #822 |

</details>

<details>
<summary><b>M1c</b> Virtual camera: 7 of 7 merged</summary>

| ID | Item | Status | PRs |
|---|---|---|---|
| G1 | vcam protocol crate | ✅ merged | #144 |
| G2 | vcam media source DLL | 🧪 merged, Windows run pending: the pacing rerun (#777, with item 6 of the Windows hardware pass) | #174, #195, #200, #229 |
| G3a | vcam producer (engine side) | ✅ merged | #194 |
| G3b | VirtualCam output kind | ✅ merged | #213, #297 |
| G3c | --register-vcam | 🧪 merged, Windows run pending: Windows run (#563) | #235, #565 |
| G3d | Virtual camera in the UI and the desktop app | ✅ merged: its Windows run passed (#390) | #316 |
| P1 | Portable Windows zip | ✅ merged | #271 |

</details>

<details>
<summary><b>M1d</b> Hardening: 10 of 11 merged</summary>

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
| H8 | Hardware acceleration by default, with GPU fallback and device-loss recovery | 🧪 merged, Windows run pending: #437 on main; the encoder's software fallback (#568), the mid-run GPU capture fallback (#569, #580), device-loss recovery (#604) and sessions after a fallback (#708) landed with wave/m1-3 (#754); landed with wave/m1-5 (#877): a full hardware session limit failing output.start with hardware_session_limit instead of falling back (#829, #443 case 2, protocol 0.44); Windows runs: the mid-run fallback (#576) and a forced TDR (#620); #734's webcam restart fixed by #796 in the same landing; open follow-up #443 (#667 fixed by #672 and #694) | #437, #568, #569, #580, #604, #708, #829 |
| Q6 | Threat-model follow-ups | ⏳ planned: from the Q5 threat model: user-only token, devices and log files (#548) and crash reports (#662) on Windows, browser pages refused every permission and local-network requests decided by policy (#535, #549, #714, #729), the vcam's owner checks (#540), local redacted desktop crash reports (#552) and a warning on cleartext rtmp:// (#555) landed with wave/m1-3 (#754); the audit log and collection file user-only (#574, for #547) and the rtmp:// flag in output.status (#653, for #562) landed with wave/m1-4 (#810); the CLAP IPC hardening (#573, for #351) landed with wave/m3-4 (#827); #349's device-loss event and effect-pack quarantine (#976, after X1's packs in #971), its GPU-time guard and a forced TDR on Windows hardware still open; open: #344, #347 and #353; Windows runs #546 and #557 | #535, #540, #548, #549, #552, #555, #662, #714, #729, #573, #574, #653, #976 |

</details>

<details>
<summary><b>M2</b> UI: 27 of 28 merged</summary>

| ID | Item | Status | PRs |
|---|---|---|---|
| U0 | Leptos UI skeleton | ✅ merged | #250 |
| U2a | ADR-0044 pairing | ✅ merged | #248 |
| U2 | Serve the UI | ✅ merged | #246, #247 |
| U2b | Device pairing | ✅ merged | #265, #288 |
| U3a | Scenes, sources, properties | ✅ merged | #256 |
| U3b | Outputs and go-live panel | ✅ merged | #254 |
| U3c | Mixer panel | ✅ merged | #308 |
| U4 | Canvas editing | ✅ merged | #255, #302 |
| U4p | Live preview on canvas | 🧪 merged, Windows run pending: the canvas's H.264 preview path on Windows (#380) | #302 |
| U1 | Preview stream | ✅ merged | #251 |
| U5 | Tauri desktop app | ✅ merged | #258 |
| Q3 | UI end-to-end tests | ✅ merged | #309 |
| Q4 | Performance benchmarks | ✅ merged | #361 |
| Q5 | Threat model | ✅ merged | #358 |
| T1 | Studio mode and transitions | 🧪 merged, Windows run pending: #260 landed with wave/m1-3 (#754); the transition node's per-tick cost is still to measure on S1 hardware (#357, ADR-0029) | #260 |
| T1u | Studio-mode UI | ✅ merged: #304 landed with wave/m1-3 (#754) | #304 |
| E2 | Video mixes | ✅ merged | #259 |
| E2b | Privacy screen and encoder sharing key | ✅ merged: the encoder owner key and cost reporting (#317) landed with wave/m1-3 (#754); the privacy screen (#317, #804) was removed at the maintainer's request, ADR-0080 (#894) | #317, #804, #894 |
| D8 | Master and output audio chains | ✅ merged | #249, #303 |
| D8p | Audio chain protocol fields | ✅ merged | #303 |
| C9 | Media file source | 🧪 merged, Windows run pending: the media file run passed on main (#382) except an MP3's loop seam, a 56 ms gap at each wrap (#445): #875 cuts it from the LAME tag; a rerun checks it, and the ~12 ms dropout 0.42 s into each pass | #245, #322, #875 |
| W1 | Audio waveform and spectrum sources | ✅ merged | #315, #460, #463, #466 |
| B11 | Folders and labels | ✅ merged: #323 landed with wave/m1-3 (#754) | #323 |
| K1 | Keyframe scenes, folders, organisation metadata and hotkeys | ✅ merged: hotkeys #319 landed with wave/m1-2 (#545); organisation metadata #323 landed with wave/m1-3 (#754); keyframe scenes part 1, in the protocol, graph and engine (#720, protocol 0.39), landed with wave/m1-4 (#810); live values on a keyframe scene (protocol 0.53, #941) and overrides of a group's children (protocol 0.55, #947) and a Move of an unflattened group's children inside its layer (#948) are built; then the UI (#846, #970): the canvas's live values on a keyframe, and a group's children overridden from their rows and the Inspector; keyframe overrides of an item's effects (ADR-0088, protocol 0.63, #985), in the protocol, graph, engine and the Filters panel | #319, #720, #941, #947, #970, #985 |
| K2 | Keyframe scenes: model and evaluator | ✅ merged | #350 |
| U6 | Desktop UI as a 1:1 match of the UI prototype | ⏳ planned: #748; docs/UI-FEATURES.md tracks it row by row. Theme and header (#495), docking (#496), the canvas (#615, #637), the Inspector (#659) and the Transform panel (#664) landed with wave/m1-3 (#754); menus, dialogs and live drag (#766, #747), the Scenes and Scene objects panels (#770), the Filters panel and its dialogs (#785, #788) and the mixer master card (#792) landed with wave/m1-4 (#810); vertical mixer strips (#809) landed with wave/m1-5 (#877); crop on the canvas and source sizes (#869), scene folders, groups and reference ↔ clone (#871), the program output and output chains (#864), the keyframe rows, menus and banner with scene hotkeys (#865), the privacy screen's button, overlay, mic label and Pause key (#866, since removed with the privacy screen, ADR-0080), the polish batch of labels, toasts, fold memory, 3D fields and the eleven panels (#867), mixer monitoring with the Hear button (#863, MX-05), which completes the mixer rows, and studio mode, the scene transitions and the scene Properties dialog (#868) landed with wave/m1-6 (#887); copy and paste (KB-05), Duplicate scene and the canvas's live picture of any scene (CV-03) and the scene-input filter rows (OB-05) are #911; the Agent activity panel and the engine's activity log over the protocol (AG-01, D38, ADR-0083) are #938. Rows that wait on the engine follow B12, V4, D9, K1, X1 and Y4; the IBM Plex fonts are bundled under the SIL OFL 1.1 (WS-02, ADR-0077, #886, landed with wave/m1-6); every blend mode drawn (CV-15, ADR-0082, #918); fps and CPU in the header from stats.tick's load (HD-04, protocol 0.48, #930); Pop out of a source alone on an object's menu (DL-07, CM-02, CM-04, protocol 0.56, #969); the Stream stats panel's own-output half from stats.tick's per-output stats (ST-01 in part, D39, protocol 0.57, #972); video filters on the program output and each output (FL-07, protocol 0.60, #973); encoder profiles and video mixes edited in the Outputs panel, with a Video select on each output card (#992); the Stream stats chart's hover readout (ST-01, #996); the scene Properties dialog's background colour, Show in multiview and Include its audio when nested (DL-03, ADR-0089, protocol 0.65, #986) | #495, #496, #615, #637, #659, #664, #766, #770, #785, #788, #792, #809, #863, #864, #865, #866, #867, #868, #869, #871, #886, #911, #918, #930, #938, #969, #972, #973, #986, #992, #996 |
| B12 | Source sizes as runtime state (source.sizes) | ✅ merged: ADR-0022 decision 4: the source.sizes query and source.size_changed event with the engine's sizes of stored frames and live sources (#806, protocol 0.40) and capture sources' sizes from the render thread (#826, #805) landed with wave/m1-5 (#877); it unblocks crop by dragging (CV-08), size in % (IN-03) and native-size placement | #806, #826 |
| C2b | Crisp text at any item scale | ✅ merged: #749: ADR-0073 (agent tier, accepted) draws a source at the density its most enlarged item needs, and the text kind draws at it; built in #781, landed with wave/m1-4 (#810) | #781 |

</details>

<details>
<summary><b>M3</b> Depth and reach: 35 of 44 merged</summary>

| ID | Item | Status | PRs |
|---|---|---|---|
| F5 | Enhanced RTMP (HEVC/AV1) | ✅ merged | #163 |
| E3c | Encode from GPU frames | ✅ merged | #325 |
| E3d | Composite to NV12 on the GPU | ✅ merged | #280 |
| F6 | HEVC and AV1 encode | ✅ merged: #364 landed with wave/m1-2 (#545); its Windows run passed (#366); choosing HEVC or AV1 from a client is F6b | #364 |
| X1 | Full 3D transforms and WGSL effect packs | ✅ merged: effect packs #368 landed with wave/m1-2 (#545); 3D transforms part 1, the drawn polygon and unplace (#611), part 2, the compositor draw (#622), part 2b, mips and anisotropic sampling (#624), part 2c, mips rebuilt once per source and frame (#643), the mip chains built from the crop and rebuilt once per change (#687), and part 3, accepting 3D at protocol 0.34 (#629), landed with wave/m3-3 (#786) | #359, #368, #611, #622, #624, #643, #687, #629 |
| 3D1 | 3D transforms: rendering ADR and spike | ✅ merged | #360 |
| 3D2 | 3D scenes with a camera | ✅ merged: ADR-0094 (#1038): items placed in one 3D space per scene and seen through the scene's camera; the engine half, scene.set_camera, the camera drawn through and sorted far to near, and a Move moving the camera (#1044, protocol 0.70); the UI's camera fields, depth-plane drag, orbit and dolly (#1053); review follow-ups (#1110); depth of field, a focus distance and blur on the camera (ADR-0108, protocol 0.77, #1122) | #1038, #1044, #1053, #1110, #1122 |
| 3D3 | 3D models with materials and live textures | ⏳ planned: ADR-0107 (#1112): a `model` source kind (glTF files and built-in shapes), lit with a studio rig or unlit, drawn through the scene camera with its own depth, filters and effects over its layer, other sources as its texture and reflection, and picking by its 3D bounds | #1112, #1113, #1118, #1261 |
| Y4 | YouTube chat, metadata, health and audience stats | ⏳ planned: chat reading and the featured message (#363, landed with wave/m1-2, #545); chat preset of the browser source (#375, landed with wave/m1); audience stats in jstream-youtube (#594, landed with wave/m3-2, #663); the engine's stats polling, the youtube.stats event and query, and the quota estimate (#696, protocol 0.38, landed with wave/m1-4, #810); the chat rate waits on the engine polling chat; signed-in features need the OAuth client (ADR-0058) | #363, #375, #594, #696 |
| CL1 | CLAP plugin host | ⏳ planned: parts 1 to 3 merged (wave/m3 landed in #477): the shared-memory contract, the CLAP wrapper, host app, launcher, supervisor and CLAP filter kinds; the host's Windows run passed in #342; CLAP filters in a source's audio chain, part 4 (#488), and the IPC hardening (#573, for #351) landed with wave/m3-4 (#827); CLAP filters on the master's and outputs' chains and in the UI's filter picker (FL-08) built in #913; then the live-audio Windows run (#912) | #272, #281, #407, #488, #573, #913 |
| WA1 | WASM automation plugins | ⏳ planned: WASM host #486 landed with wave/m3-2 (#663); its wall-time deadlines #623, the cap of 50 commands a second #722, the 1,000-event queue #725 and the wire half #642 (protocol 0.35) landed with wave/m3-3 (#786); the engine side part 1, installing, granting, events, forbidden and disabling (#839, ADR-0075), and part 2, saving installed plugins, grants held to the manifest's classes and hosts, needs_grant, memory_limit and the MCP state-text line (#843, ADR-0076), landed with wave/m3-5 (#854); the plugin.list query and the UI's Plugins section with its grant dialog landed (#974, protocol 0.58); removing a plugin and a private data folder wait, the folder for its own design with the file imports it needs | #486, #623, #642, #722, #725, #839, #843, #974 |
| MAC1 | macOS platform crate | ⏳ planned: ADR-0063 accepted (#511); crate part 1 (#513); VideoToolbox H.264/HEVC encoder, part 2 (#533); AudioToolbox AAC-LC encoder, part 3 (#543); ScreenCaptureKit display and window capture with the Screen Recording permission check, part 4 (#554); Keychain store for stream keys and OAuth tokens, part 5 (#566); Core Audio devices and input recording, part 6 (#570); Microphone permission check and the daemon's embedded Info.plist, part 7 (#583); Core Audio output playback, part 8 (#586); all landed with wave/m3-2 (#663); with wave/m3-3 (#786): the list of processes Core Audio knows, part 9 (#589); desktop and application audio through Core Audio process taps, part 10 (#599); AVFoundation webcams behind the Camera permission, part 11 (#608); VideoToolbox H.264/HEVC decoder on CPU frames (#618); media files through AVFoundation's asset reader (#625); the daemon live on macOS (#639); spike S6, IOSurface and Metal interop and its synchronisation, for ADR-0063 decision 8 (#646, its Mac run in #648); a one-command install that builds JStream on a tester's Mac and runs it from Terminal (#1093); per-thread CPU for the performance profile (#1135); the rest of the crate to build | #511, #513, #533, #543, #554, #566, #570, #583, #586, #589, #599, #608, #618, #625, #639, #646, #1093, #1135 |
| LNX1 | Linux codec path and platform crate | ⏳ planned: ADR-0060 accepted (#481); crate part 1 (#490); portal and PipeWire screen capture (#509); VA-API H.264 encode (#510); these landed with wave/m3-2 (#663); with wave/m3-3 (#786): ALSA audio input and output (#572); portal restore tokens per source (#598); DMA-BUF capture frames imported into Vulkan (#606); VA-API H.264 encode from the Vulkan device's NV12 encode targets (#652); PipeWire audio input, output and loopback (#593); the engine's capture per source, which starts each source with its own restore token, forgets it when the source is deleted and honours `show_cursor` (#682, protocol 0.36); with wave/m3-4 (#827): PipeWire processing on the real-time data loop (#612); V4L2 webcams and capture cards (#603); multi-planar V4L2 capture (#617); the pointer as PipeWire cursor metadata or left out, platform only (#610), on the CPU and DMA-BUF paths; the source's show_cursor setting (protocol 0.36, #682) has the OS draw the pointer when it's on and leave it out when it's off (WGC leaves it out too, with #682, and ScreenCaptureKit with #685), and the engine doesn't draw the pointer; webcams paired with their microphones and the Linux backend set choosing PipeWire or ALSA (#689); stream keys and OAuth tokens in the Secret Service, else a 0600 file, ADR-0071, not yet used by an app (#634); capture copies and NV12 submits ordered on the context's queue (#694); with wave/m3-5 (#854): VA-API HEVC encode (#830) and its hardening (#835), still to run on Intel or AMD hardware; the daemon goes live on the Linux backends, with stream keys in LinuxSecrets and OpenH264 taking the compositor's RGBA frames (#1016); a one-command install that builds JStream on a tester's Linux machine, with browser sources and an app-menu entry (#1103); the rest of the codec path to build | #481, #490, #509, #510, #572, #598, #606, #652, #593, #612, #603, #617, #610, #685, #689, #634, #694, #830, #835, #682, #1016, #1103 |
| LNX2 | Linux OpenH264 download (ADR-0061) | ✅ merged: store, check, loader and encoder (#536, landed with wave/m3-2, #663); the pin's two SHA-256 values recorded from Cisco's server, and CI's e2e-stream job downloads OpenH264 and streams on it (#1015); the UI offers the download in Encoders, with Cisco's notice, licence link and full licence text, through protocol 0.69's codec.downloads, codec.download and codec.delete (#1041) | #536, #1015, #1041 |
| LNX3 | AAC-LC encoder in Rust, patent check first (ADR-0061) | ⏳ planned: patent check ADR-0062 accepted (#508); encoder and Linux wiring (#518), landed with wave/m3-2 (#663); TNS (#723, more tests #727) landed with wave/m3-3 (#786); intensity stereo (#799) landed with wave/m3-4 (#827); before the Linux app ships the encoder its licence information must say JStream holds no AAC patent licence and name Via LA (ADR-0062) | #508, #518, #723, #727, #799 |
| F6b | Choose HEVC or AV1 from a client | ⏳ planned: F6 built the encoders; #807 names the encoder a machine lacks at output.start, landed with wave/m3-4 (#827); #814 (protocol 0.41) adds hevc and av1 to VideoCodec and lets an encoder profile take them, landed with wave/m1-5 (#877) | #807, #814 |
| F7 | Direct NVENC and AMF encoders, if measurements justify them | ⏳ planned: ROADMAP M3: measure Media Foundation's hardware encoders against direct NVENC and AMF first, and build the direct ones only where the numbers say so |  |
| CL2 | A too_large error for oversized plugin state | ✅ merged: ROADMAP M3 and #102: ADR-0074 (#794, accepted by the maintainer, maintainer tier, landed with wave/m3-5 (#854)) amends ADR-0037's state clause with a stable too_large error; built in #981 (protocol 0.62): a message over the 4 MiB request limit, read below a 16 MiB frame limit, is refused with too_large and the connection stays open, so a batch carrying two 2 MiB CLAP states fails cleanly | #794, #981 |
| X2 | Shader sources: multipass WGSL packs with feedback, inputs and audio, written over MCP | 🧪 merged, Windows run pending: ADR-0093 (agent tier) and the program in jstream-effects and jstream-render (#1034), and the engine wiring: kinds, stand-ins, scene slots, restart on show and audio (#1048); writing packs over the protocol and MCP is decided by ADR-0097 (maintainer tier, accepted): the quarantine follows the shader digest (#1064), and pack.write, pack.delete, pack.list and pack.read (protocol 0.72, #1085), whose Windows and macOS run is item 20 of the Windows hardware pass (#1088); for the cockpit scenes (warp, reflections, audio glow) that browser sources draw on the CPU today | #1034, #1048, #1050, #1052, #1064, #1085 |
| AG1 | MCP prompts only for what undo can't take back (destructiveHint) | ✅ merged | #1055 |
| AG2 | Agents reach the desktop app: jstream.exe --mcp with a token file, and Connect an agent docs | ✅ merged | #1091 |
| AG3 | Layering and reflection guidance for agents in MCP | ✅ merged | #1079 |
| TM1 | Timer source that keeps counting through edits, with a restart counter | ✅ merged | #1039, #1117 |
| BR1 | A reloading browser page stays transparent until its new document paints | ✅ merged | #1095 |
| MO1 | Native motion: ADR-0099's model and keyframe tracks | ✅ merged | #1074 |
| MO2 | Native motion: continuous wiggle, oscillate and drift on items, drawn each frame (item.set_motion) | 🧪 merged, Windows run pending: Protocol 0.81; landed with wave/alpha4 (#1176), with camera motion and the follow-cursor camera (#1157, ADR-0113, 0.85); the alpha.4 Windows run's step 4 (the cockpit rebuild over MCP) checks it on hardware, and step 5e the follow camera (#1159) | #1125, #1176 |
| MO3 | A one-shot shake that a hotkey fires and that decays | 🧪 merged, Windows run pending: item.trigger_motion, protocol 0.84; landed with wave/alpha4 (#1176); the alpha.4 Windows run's step 4 (the cockpit rebuild over MCP) checks it on hardware | #1156, #1176 |
| MO4 | Audio-reactive bindings: ADR-0100 and its model | ✅ merged | #1076 |
| MO5 | Audio-reactive motion modifiers drawn from the render loop | 🧪 merged, Windows run pending: Protocol 0.86; landed with wave/alpha4 (#1176); the alpha.4 Windows run's step 4 (the cockpit rebuild over MCP) checks it on hardware, with real audio | #1149, #1176 |
| HK1 | Hotkeys show, hide and toggle one item, with the item's own effects (ADR-0101) | 🧪 merged, Windows run pending: Protocol 0.80; landed with wave/alpha4 (#1176); the alpha.4 Windows run's step 4 (the cockpit rebuild over MCP) checks it on hardware | #1082, #1176 |
| FS1 | Frame sheets, so an agent can check motion (video.frames, scene.switch_and_watch) | 🧪 merged, Windows run pending: Protocol 0.82; landed with wave/alpha4 (#1176); the alpha.4 Windows run's step 4 (the cockpit rebuild over MCP) checks it on hardware | #1080, #1176 |
| PA1 | Page API part 1: ADR-0102 and shared values (values.set, values.list) | ✅ merged | #1077, #1127 |
| PA2 | Page API part 2: window.jstream with clock, visibility, scenes and values | 🧪 merged, Windows run pending: Merged; step 5f of the alpha.4 Windows run checks it on Windows (#1147) | #1146, #1178 |
| PA4 | Page API read review: a checkbox and a visible warning per kind of data, and a badge (ADR-0124) | ✅ merged | #1287 |
| PA3 | Page API part 3: a bus's levels and bands for pages (jstream.levels) | 🧪 merged, Windows run pending: Merged; the Windows run's page API step (#1147) checks a granted bus with real audio | #1263 |
| PF1 | Per-item, per-source and per-thread performance profile over the protocol and MCP (ADR-0103) | ✅ merged | #1087 |
| PS1 | A hidden browser page rests while no mix draws it | ✅ merged | #1100 |
| PS2 | Software compositing: a nested scene drawn once a frame, and blur taps bounded lower | ✅ merged | #1106 |
| CB1 | Blend in sRGB-encoded space, so HTML overlays match a browser (normal_srgb, additive_srgb) | ✅ merged | #1109 |
| CB2 | Item effects after placement, over the canvas | ✅ merged | #1111 |
| CB3 | Opacity above 1 as a gain, so additive items can add more than their source | 🧪 merged, Windows run pending: Protocol 0.83; landed with wave/alpha4 (#1176); the alpha.4 Windows run's step 4 (the cockpit rebuild over MCP) checks it on hardware | #1104, #1176 |
| CH1 | File filters such as Apply LUT on the program and output chains | ✅ merged | #1134 |
| LB1 | A local web browser source's first load passes on Linux | ✅ merged | #1094 |
| RL4 | Release 0.1.0-alpha.4: the version bump, then the acceptance run on Windows | 🧪 merged, Windows run pending: Released as v0.1.0-alpha.4 (#1179); the acceptance run on Windows, building a layered, animated scene over MCP and reading its profile, runs on the released build | #1179 |

</details>
<!-- @generated end progress -->

<a id="build"></a>
## Build and run
<!-- @section
-->

To run a release instead, see [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md).

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

- [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md): running a release on Windows,
  a first scene, going live, recording and the virtual camera
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): how the system fits together
- [docs/RULES.md](docs/RULES.md): engineering rules, each with a permanent ID
- [docs/DOCS.md](docs/DOCS.md): how docs stay accurate (hash-checked sections, history from git)
- [docs/adr/](docs/adr/): decision records
- [ROADMAP.md](ROADMAP.md): milestones, and [`docs/progress.toml`](docs/progress.toml):
  the item status the [Progress](#progress) table is generated from
- [docs/runbooks/go-live.md](docs/runbooks/go-live.md): taking the daemon live on Windows,
  and the smoke script that checks it
- [docs/runbooks/release.md](docs/runbooks/release.md): versioning and publishing a release
- [docs/runbooks/linux-testing.md](docs/runbooks/linux-testing.md): one command that
  installs what the build needs, builds JStream and installs it on a tester's Linux machine
- [docs/runbooks/mac-testing.md](docs/runbooks/mac-testing.md): one command that builds
  and installs JStream on a tester's Mac, run from Terminal
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
