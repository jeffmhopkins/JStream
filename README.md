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
| [M1b: Scenes and mixing](ROADMAP.md#m1) | `█████████░` 95% | 37 | 0 | 2 |
| [M1c: Virtual camera](ROADMAP.md#m1) | `██████████` 100% | 7 | 0 | 0 |
| [M1d: Hardening](ROADMAP.md#m1) | `█████████░` 91% | 10 | 0 | 1 |
| [M2: UI](ROADMAP.md#m2) | `█████████░` 89% | 25 | 1 | 2 |
| [M3: Depth and reach](ROADMAP.md#m3) | `████░░░░░░` 38% | 6 | 0 | 10 |

114 of 130 items are merged; 12 of those still wait on a run on Windows hardware.

<details open>
<summary><b>What's left</b>: 28 items, by next step: 🔨 build 17, 🧪 Windows run 10, 👤 maintainer 1</summary>

The order these land in, and what done means, is in [ROADMAP › Endgame](ROADMAP.md#endgame).

| ID | Item | Milestone | Needs, in order | Note | PRs |
|---|---|---|---|---|---|
| A4 | Go-live runbook and smoke | M1a | 🧪 Windows run | the go-live smoke and graceful stop (item 11 of the Windows hardware pass); its live YouTube steps are #337 | #234 |
| A6b | Daemon YouTube wiring | M1a | 👤 maintainer, then 🧪 Windows run | #312 landed with wave/m1-2 (#545); sign-in needs the OAuth client (ADR-0058); Windows run #337 | #312 |
| C4c | Webcam source (CPU path, MJPEG on Windows, paired audio) | M1b | 🧪 Windows run | landed with wave/m1-3 (#754); a Windows run with a webcam and its paired microphone is still pending (#673 covers the MJPEG webcam) | #654, #658, #726 |
| C5 | Audio sources | M1b | 🧪 Windows run | the WASAPI device tests on real devices (#778, item 18 of the Windows hardware pass) | #238 |
| C8 | Browser source (CEF) | M1b | 🔨 build, then 🧪 Windows run | the Windows runs #335 (steps 1 to 4) and #336 passed; #341 passed except its refused-request count (2, not 1), so page_check needs to name the refused URLs, then a rerun; Ctrl-C with CEF running skips the graceful stop (#436), a fix to build, then #335's step 5 runs again | #283, #289, #298, #326 |
| V0 | Video filter chain runtime | M1b | 🔨 build | part 1 runs the built-in kinds, part 2 loads apply_lut and image_mask assets, part 3 gives scene_mask its scene, part 4 pools scratch layers and each draw's uniform buffers (#712, #718, landed with wave/m1-4, #810), and #730 uploads LUT tables directly; then effect-pack kinds. No bounds inset: on a source chain shadow, outline and glow draw within the frame (ADR-0065 §5) | #595, #607, #626, #712, #717, #718, #730 |
| V4 | Scene filter chains and item effects | M1b | 🔨 build | ADR-0065 designs a video chain owned by a scene or by one placement, and ADR-0068 puts item effects in v1 (#529). Landed with wave/m1-5 (#877): a vignette kind (#802), the protocol part, the scene and item owners at 0.43 (#813's change, landed through #815), the graph and mock holding them at collection schema 2 (#815), the engine running scene filters over the scene's composite (#823), and item effects over each item's box (#824). Still to build: stats.tick's counts and goldens, and the UI's scene filters (FL-06) and effects | #802, #813, #815, #823, #824 |
| D9 | Audio monitoring: a bus's monitor device and tap point | M1b | 🧪 Windows run | ADR-0028 gives a bus a monitor, a device and a tap point before or after its filters (decision register D22). #801 is the mixer's monitor taps and player, #816 the protocol's bus monitor and bus.set_monitor (0.42), and #822 the engine playing it on its device, all landed with wave/m1-5 (#877); the mixer's Hear: before or after FX row (MX-05) can then be built. #822 ran only against the simulated player and the fake device, so a run on a real output device (WASAPI, Core Audio, PipeWire) is still owed: playback, drift steering and an unplug mid-stream | #801, #816, #822 |
| G2 | vcam media source DLL | M1c | 🧪 Windows run | the pacing rerun (#777, with item 6 of the Windows hardware pass) | #174, #195, #200, #229 |
| G3c | --register-vcam | M1c | 🧪 Windows run | Windows run (#563) | #235, #565 |
| H8 | Hardware acceleration by default, with GPU fallback and device-loss recovery | M1d | 🧪 Windows run | #437 on main; the encoder's software fallback (#568), the mid-run GPU capture fallback (#569, #580), device-loss recovery (#604) and sessions after a fallback (#708) landed with wave/m1-3 (#754); landed with wave/m1-5 (#877): a full hardware session limit failing output.start with hardware_session_limit instead of falling back (#829, #443 case 2, protocol 0.44); Windows runs: the mid-run fallback (#576) and a forced TDR (#620); #734's webcam restart fixed by #796 in the same landing; open follow-up #443 (#667 fixed by #672 and #694) | #437, #568, #569, #580, #604, #708, #829 |
| Q6 | Threat-model follow-ups | M1d | 🔨 build, then 🧪 Windows run | from the Q5 threat model: user-only token, devices and log files (#548) and crash reports (#662) on Windows, browser pages refused every permission and local-network requests decided by policy (#535, #549, #714, #729), the vcam's owner checks (#540), local redacted desktop crash reports (#552) and a warning on cleartext rtmp:// (#555) landed with wave/m1-3 (#754); the audit log and collection file user-only (#574, for #547) and the rtmp:// flag in output.status (#653, for #562) landed with wave/m1-4 (#810); the CLAP IPC hardening (#573, for #351) landed with wave/m3-4 (#827); open: #344, #347, #349 (with X1) and #353; Windows runs #546 and #557 | #535, #540, #548, #549, #552, #555, #662, #714, #729, #573, #574, #653 |
| U4p | Live preview on canvas | M2 | 🧪 Windows run | the canvas's H.264 preview path on Windows (#380) | #302 |
| T1 | Studio mode and transitions | M2 | 🧪 Windows run | #260 landed with wave/m1-3 (#754); the transition node's per-tick cost is still to measure on S1 hardware (#357, ADR-0029) | #260 |
| E2b | Privacy screen and encoder sharing key | M2 | 🔨 build | on/off privacy covering every output, the encoder owner key and cost reporting (#317) landed with wave/m1-3 (#754); muting the microphones, the default muted set (#804), landed with wave/m1-5 (#877); the look setting, the slate and choosing the muted sources to build | #317, #804 |
| C9 | Media file source | M2 | 🧪 Windows run | the media file run passed on main (#382) except an MP3's loop seam, a 56 ms gap at each wrap (#445): #875 cuts it from the LAME tag; a rerun checks it, and the ~12 ms dropout 0.42 s into each pass | #245, #322, #875 |
| K1 | Keyframe scenes, folders, organisation metadata and hotkeys | M2 | 🔨 build | hotkeys #319 landed with wave/m1-2 (#545); organisation metadata #323 landed with wave/m1-3 (#754); keyframe scenes part 1, in the protocol, graph and engine (#720, protocol 0.39), landed with wave/m1-4 (#810); a group's children, live values on a keyframe scene and the UI follow (#846) | #319, #720 |
| U6 | Desktop UI as a 1:1 match of the UI prototype | M2 | 🔨 build | #748; docs/UI-FEATURES.md tracks it row by row. Theme and header (#495), docking (#496), the canvas (#615, #637), the Inspector (#659) and the Transform panel (#664) landed with wave/m1-3 (#754); menus, dialogs and live drag (#766, #747), the Scenes and Scene objects panels (#770), the Filters panel and its dialogs (#785, #788) and the mixer master card (#792) landed with wave/m1-4 (#810); vertical mixer strips (#809) landed with wave/m1-5 (#877); the program output and output chains (#864), the keyframe rows, menus and banner with scene hotkeys (#865), the privacy screen's button, overlay, mic label and Pause key (#866, on E2b) and the polish batch of labels, toasts, fold memory, 3D fields and the eleven panels (#867) landed in wave/m1-6; the rest of the mixer and the keyboard batch are still to build. Rows that wait on the engine follow B12, V4, D9, K1, X1 and Y4, and on two gaps with no item yet: previews of any scene or source (decision register D36) and the audit log over the protocol (D38); the IBM Plex fonts are bundled under the SIL OFL 1.1 (WS-02, ADR-0077, #886) | #495, #496, #615, #637, #659, #664, #766, #770, #785, #788, #792, #809, #864, #865, #866, #867, #886 |
| Y4 | YouTube chat, metadata, health and audience stats | M3 | 🔨 build, then 👤 maintainer | chat reading and the featured message (#363, landed with wave/m1-2, #545); chat preset of the browser source (#375, landed with wave/m1); audience stats in jstream-youtube (#594, landed with wave/m3-2, #663); the engine's stats polling, the youtube.stats event and query, and the quota estimate (#696, protocol 0.38, landed with wave/m1-4, #810); the chat rate waits on the engine polling chat; signed-in features need the OAuth client (ADR-0058) | #363, #375, #594, #696 |
| CL1 | CLAP plugin host | M3 | 🔨 build, then 🧪 Windows run | parts 1 to 3 merged (wave/m3 landed in #477): the shared-memory contract, the CLAP wrapper, host app, launcher, supervisor and CLAP filter kinds; the host's Windows run passed in #342; CLAP filters in a source's audio chain, part 4 (#488), and the IPC hardening (#573, for #351) landed with wave/m3-4 (#827); CLAP filters on the master's and outputs' chains are not built yet; then a live-audio Windows run (new issue once CLAP filters run) | #272, #281, #407, #488, #573 |
| WA1 | WASM automation plugins | M3 | 🔨 build | WASM host #486 landed with wave/m3-2 (#663); its wall-time deadlines #623, the cap of 50 commands a second #722, the 1,000-event queue #725 and the wire half #642 (protocol 0.35) landed with wave/m3-3 (#786); the engine side part 1, installing, granting, events, forbidden and disabling (#839, ADR-0075), and part 2, saving installed plugins, grants held to the manifest's classes and hosts, needs_grant, memory_limit and the MCP state-text line (#843, ADR-0076), landed with wave/m3-5 (#854); the UI's grant flow and a query listing the plugins follow, and a private data folder waits for its own design with the file imports it needs | #486, #623, #642, #722, #725, #839, #843 |
| MAC1 | macOS platform crate | M3 | 🔨 build | ADR-0063 accepted (#511); crate part 1 (#513); VideoToolbox H.264/HEVC encoder, part 2 (#533); AudioToolbox AAC-LC encoder, part 3 (#543); ScreenCaptureKit display and window capture with the Screen Recording permission check, part 4 (#554); Keychain store for stream keys and OAuth tokens, part 5 (#566); Core Audio devices and input recording, part 6 (#570); Microphone permission check and the daemon's embedded Info.plist, part 7 (#583); Core Audio output playback, part 8 (#586); all landed with wave/m3-2 (#663); with wave/m3-3 (#786): the list of processes Core Audio knows, part 9 (#589); desktop and application audio through Core Audio process taps, part 10 (#599); AVFoundation webcams behind the Camera permission, part 11 (#608); VideoToolbox H.264/HEVC decoder on CPU frames (#618); media files through AVFoundation's asset reader (#625); the daemon live on macOS (#639); spike S6, IOSurface and Metal interop and its synchronisation, for ADR-0063 decision 8 (#646, its Mac run in #648); the rest of the crate to build | #511, #513, #533, #543, #554, #566, #570, #583, #586, #589, #599, #608, #618, #625, #639, #646 |
| LNX1 | Linux codec path and platform crate | M3 | 🔨 build, then 🧪 Windows run | ADR-0060 accepted (#481); crate part 1 (#490); portal and PipeWire screen capture (#509); VA-API H.264 encode (#510); these landed with wave/m3-2 (#663); with wave/m3-3 (#786): ALSA audio input and output (#572); portal restore tokens per source (#598); DMA-BUF capture frames imported into Vulkan (#606); VA-API H.264 encode from the Vulkan device's NV12 encode targets (#652); PipeWire audio input, output and loopback (#593); the engine's capture per source, which starts each source with its own restore token, forgets it when the source is deleted and honours `show_cursor` (#682, protocol 0.36); with wave/m3-4 (#827): PipeWire processing on the real-time data loop (#612); V4L2 webcams and capture cards (#603); multi-planar V4L2 capture (#617); the pointer as PipeWire cursor metadata or left out, platform only (#610), on the CPU and DMA-BUF paths; the source's show_cursor setting (protocol 0.36, #682) has the OS draw the pointer when it's on and leave it out when it's off (WGC leaves it out too, with #682, and ScreenCaptureKit with #685), and the engine doesn't draw the pointer; webcams paired with their microphones and the Linux backend set choosing PipeWire or ALSA (#689); stream keys and OAuth tokens in the Secret Service, else a 0600 file, ADR-0071, not yet used by an app (#634); capture copies and NV12 submits ordered on the context's queue (#694); with wave/m3-5 (#854): VA-API HEVC encode (#830) and its hardening (#835), still to run on Intel or AMD hardware; the rest of the codec path to build | #481, #490, #509, #510, #572, #598, #606, #652, #593, #612, #603, #617, #610, #685, #689, #634, #694, #830, #835, #682 |
| LNX2 | Linux OpenH264 download (ADR-0061) | M3 | 🔨 build | store, check, loader and encoder (#536, landed with wave/m3-2, #663); downloading stays off until the pin's two SHA-256 values are recorded from Cisco's server, and the UI's download offer, with Cisco's notice re-read, is still to come | #536 |
| LNX3 | AAC-LC encoder in Rust, patent check first (ADR-0061) | M3 | 🔨 build | patent check ADR-0062 accepted (#508); encoder and Linux wiring (#518), landed with wave/m3-2 (#663); TNS (#723, more tests #727) landed with wave/m3-3 (#786); intensity stereo (#799) landed with wave/m3-4 (#827); before the Linux app ships the encoder its licence information must say JStream holds no AAC patent licence and name Via LA (ADR-0062) | #508, #518, #723, #727, #799 |
| F6b | Choose HEVC or AV1 from a client | M3 | 🔨 build | F6 built the encoders; #807 names the encoder a machine lacks at output.start, landed with wave/m3-4 (#827); #814 (protocol 0.41) adds hevc and av1 to VideoCodec and lets an encoder profile take them, landed with wave/m1-5 (#877) | #807, #814 |
| F7 | Direct NVENC and AMF encoders, if measurements justify them | M3 | 🔨 build | ROADMAP M3: measure Media Foundation's hardware encoders against direct NVENC and AMF first, and build the direct ones only where the numbers say so |  |
| CL2 | A too_large error for oversized plugin state | M3 | 🔨 build | ROADMAP M3 and #102: ADR-0074 (#794, accepted by the maintainer, maintainer tier, landed with wave/m3-5 (#854)) amends ADR-0037's state clause with a stable too_large error, so a batch carrying two 2 MiB CLAP states fails cleanly instead of closing the connection; the build, a protocol minor, follows | #794 |

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
<summary><b>M1b</b> Scenes and mixing: 37 of 39 merged</summary>

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
| V0 | Video filter chain runtime | ⏳ planned: part 1 runs the built-in kinds, part 2 loads apply_lut and image_mask assets, part 3 gives scene_mask its scene, part 4 pools scratch layers and each draw's uniform buffers (#712, #718, landed with wave/m1-4, #810), and #730 uploads LUT tables directly; then effect-pack kinds. No bounds inset: on a source chain shadow, outline and glow draw within the frame (ADR-0065 §5) | #595, #607, #626, #712, #717, #718, #730 |
| D5 | Audio in the protocol | ✅ merged | #240 |
| D6 | when_shown activity gain | ✅ merged | #299 |
| E1 | Render snapshots, GPU lifetime | ✅ merged | #172, #215 |
| F1 | fMP4 muxer | ✅ merged | #151 |
| F2a | Recording file sink | ✅ merged | #156 |
| F2b | Record output kind | ✅ merged | #170 |
| F3 | Encoder profiles and sharing | ✅ merged | #208 |
| I2 | Live values | ✅ merged | #244 |
| V4 | Scene filter chains and item effects | ⏳ planned: ADR-0065 designs a video chain owned by a scene or by one placement, and ADR-0068 puts item effects in v1 (#529). Landed with wave/m1-5 (#877): a vignette kind (#802), the protocol part, the scene and item owners at 0.43 (#813's change, landed through #815), the graph and mock holding them at collection schema 2 (#815), the engine running scene filters over the scene's composite (#823), and item effects over each item's box (#824). Still to build: stats.tick's counts and goldens, and the UI's scene filters (FL-06) and effects | #802, #813, #815, #823, #824 |
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
| Q6 | Threat-model follow-ups | ⏳ planned: from the Q5 threat model: user-only token, devices and log files (#548) and crash reports (#662) on Windows, browser pages refused every permission and local-network requests decided by policy (#535, #549, #714, #729), the vcam's owner checks (#540), local redacted desktop crash reports (#552) and a warning on cleartext rtmp:// (#555) landed with wave/m1-3 (#754); the audit log and collection file user-only (#574, for #547) and the rtmp:// flag in output.status (#653, for #562) landed with wave/m1-4 (#810); the CLAP IPC hardening (#573, for #351) landed with wave/m3-4 (#827); open: #344, #347, #349 (with X1) and #353; Windows runs #546 and #557 | #535, #540, #548, #549, #552, #555, #662, #714, #729, #573, #574, #653 |

</details>

<details>
<summary><b>M2</b> UI: 25 of 28 merged</summary>

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
| E2b | Privacy screen and encoder sharing key | ⏳ planned: on/off privacy covering every output, the encoder owner key and cost reporting (#317) landed with wave/m1-3 (#754); muting the microphones, the default muted set (#804), landed with wave/m1-5 (#877); the look setting, the slate and choosing the muted sources to build | #317, #804 |
| D8 | Master and output audio chains | ✅ merged | #249, #303 |
| D8p | Audio chain protocol fields | ✅ merged | #303 |
| C9 | Media file source | 🧪 merged, Windows run pending: the media file run passed on main (#382) except an MP3's loop seam, a 56 ms gap at each wrap (#445): #875 cuts it from the LAME tag; a rerun checks it, and the ~12 ms dropout 0.42 s into each pass | #245, #322, #875 |
| W1 | Audio waveform and spectrum sources | ✅ merged | #315, #460, #463, #466 |
| B11 | Folders and labels | ✅ merged: #323 landed with wave/m1-3 (#754) | #323 |
| K1 | Keyframe scenes, folders, organisation metadata and hotkeys | ⏳ planned: hotkeys #319 landed with wave/m1-2 (#545); organisation metadata #323 landed with wave/m1-3 (#754); keyframe scenes part 1, in the protocol, graph and engine (#720, protocol 0.39), landed with wave/m1-4 (#810); a group's children, live values on a keyframe scene and the UI follow (#846) | #319, #720 |
| K2 | Keyframe scenes: model and evaluator | ✅ merged | #350 |
| U6 | Desktop UI as a 1:1 match of the UI prototype | 🔍 in review: #748; docs/UI-FEATURES.md tracks it row by row. Theme and header (#495), docking (#496), the canvas (#615, #637), the Inspector (#659) and the Transform panel (#664) landed with wave/m1-3 (#754); menus, dialogs and live drag (#766, #747), the Scenes and Scene objects panels (#770), the Filters panel and its dialogs (#785, #788) and the mixer master card (#792) landed with wave/m1-4 (#810); vertical mixer strips (#809) landed with wave/m1-5 (#877); the program output and output chains (#864), the keyframe rows, menus and banner with scene hotkeys (#865), the privacy screen's button, overlay, mic label and Pause key (#866, on E2b) and the polish batch of labels, toasts, fold memory, 3D fields and the eleven panels (#867) landed in wave/m1-6; the rest of the mixer and the keyboard batch are still to build. Rows that wait on the engine follow B12, V4, D9, K1, X1 and Y4, and on two gaps with no item yet: previews of any scene or source (decision register D36) and the audit log over the protocol (D38); the IBM Plex fonts are bundled under the SIL OFL 1.1 (WS-02, ADR-0077, #886) | #495, #496, #615, #637, #659, #664, #766, #770, #785, #788, #792, #809, #864, #865, #866, #867, #886 |
| B12 | Source sizes as runtime state (source.sizes) | ✅ merged: ADR-0022 decision 4: the source.sizes query and source.size_changed event with the engine's sizes of stored frames and live sources (#806, protocol 0.40) and capture sources' sizes from the render thread (#826, #805) landed with wave/m1-5 (#877); it unblocks crop by dragging (CV-08), size in % (IN-03) and native-size placement | #806, #826 |
| C2b | Crisp text at any item scale | ✅ merged: #749: ADR-0073 (agent tier, accepted) draws a source at the density its most enlarged item needs, and the text kind draws at it; built in #781, landed with wave/m1-4 (#810) | #781 |

</details>

<details>
<summary><b>M3</b> Depth and reach: 6 of 16 merged</summary>

| ID | Item | Status | PRs |
|---|---|---|---|
| F5 | Enhanced RTMP (HEVC/AV1) | ✅ merged | #163 |
| E3c | Encode from GPU frames | ✅ merged | #325 |
| E3d | Composite to NV12 on the GPU | ✅ merged | #280 |
| F6 | HEVC and AV1 encode | ✅ merged: #364 landed with wave/m1-2 (#545); its Windows run passed (#366); choosing HEVC or AV1 from a client is F6b | #364 |
| X1 | Full 3D transforms and WGSL effect packs | ✅ merged: effect packs #368 landed with wave/m1-2 (#545); 3D transforms part 1, the drawn polygon and unplace (#611), part 2, the compositor draw (#622), part 2b, mips and anisotropic sampling (#624), part 2c, mips rebuilt once per source and frame (#643), the mip chains built from the crop and rebuilt once per change (#687), and part 3, accepting 3D at protocol 0.34 (#629), landed with wave/m3-3 (#786) | #359, #368, #611, #622, #624, #643, #687, #629 |
| 3D1 | 3D transforms: rendering ADR and spike | ✅ merged | #360 |
| Y4 | YouTube chat, metadata, health and audience stats | ⏳ planned: chat reading and the featured message (#363, landed with wave/m1-2, #545); chat preset of the browser source (#375, landed with wave/m1); audience stats in jstream-youtube (#594, landed with wave/m3-2, #663); the engine's stats polling, the youtube.stats event and query, and the quota estimate (#696, protocol 0.38, landed with wave/m1-4, #810); the chat rate waits on the engine polling chat; signed-in features need the OAuth client (ADR-0058) | #363, #375, #594, #696 |
| CL1 | CLAP plugin host | ⏳ planned: parts 1 to 3 merged (wave/m3 landed in #477): the shared-memory contract, the CLAP wrapper, host app, launcher, supervisor and CLAP filter kinds; the host's Windows run passed in #342; CLAP filters in a source's audio chain, part 4 (#488), and the IPC hardening (#573, for #351) landed with wave/m3-4 (#827); CLAP filters on the master's and outputs' chains are not built yet; then a live-audio Windows run (new issue once CLAP filters run) | #272, #281, #407, #488, #573 |
| WA1 | WASM automation plugins | ⏳ planned: WASM host #486 landed with wave/m3-2 (#663); its wall-time deadlines #623, the cap of 50 commands a second #722, the 1,000-event queue #725 and the wire half #642 (protocol 0.35) landed with wave/m3-3 (#786); the engine side part 1, installing, granting, events, forbidden and disabling (#839, ADR-0075), and part 2, saving installed plugins, grants held to the manifest's classes and hosts, needs_grant, memory_limit and the MCP state-text line (#843, ADR-0076), landed with wave/m3-5 (#854); the UI's grant flow and a query listing the plugins follow, and a private data folder waits for its own design with the file imports it needs | #486, #623, #642, #722, #725, #839, #843 |
| MAC1 | macOS platform crate | ⏳ planned: ADR-0063 accepted (#511); crate part 1 (#513); VideoToolbox H.264/HEVC encoder, part 2 (#533); AudioToolbox AAC-LC encoder, part 3 (#543); ScreenCaptureKit display and window capture with the Screen Recording permission check, part 4 (#554); Keychain store for stream keys and OAuth tokens, part 5 (#566); Core Audio devices and input recording, part 6 (#570); Microphone permission check and the daemon's embedded Info.plist, part 7 (#583); Core Audio output playback, part 8 (#586); all landed with wave/m3-2 (#663); with wave/m3-3 (#786): the list of processes Core Audio knows, part 9 (#589); desktop and application audio through Core Audio process taps, part 10 (#599); AVFoundation webcams behind the Camera permission, part 11 (#608); VideoToolbox H.264/HEVC decoder on CPU frames (#618); media files through AVFoundation's asset reader (#625); the daemon live on macOS (#639); spike S6, IOSurface and Metal interop and its synchronisation, for ADR-0063 decision 8 (#646, its Mac run in #648); the rest of the crate to build | #511, #513, #533, #543, #554, #566, #570, #583, #586, #589, #599, #608, #618, #625, #639, #646 |
| LNX1 | Linux codec path and platform crate | ⏳ planned: ADR-0060 accepted (#481); crate part 1 (#490); portal and PipeWire screen capture (#509); VA-API H.264 encode (#510); these landed with wave/m3-2 (#663); with wave/m3-3 (#786): ALSA audio input and output (#572); portal restore tokens per source (#598); DMA-BUF capture frames imported into Vulkan (#606); VA-API H.264 encode from the Vulkan device's NV12 encode targets (#652); PipeWire audio input, output and loopback (#593); the engine's capture per source, which starts each source with its own restore token, forgets it when the source is deleted and honours `show_cursor` (#682, protocol 0.36); with wave/m3-4 (#827): PipeWire processing on the real-time data loop (#612); V4L2 webcams and capture cards (#603); multi-planar V4L2 capture (#617); the pointer as PipeWire cursor metadata or left out, platform only (#610), on the CPU and DMA-BUF paths; the source's show_cursor setting (protocol 0.36, #682) has the OS draw the pointer when it's on and leave it out when it's off (WGC leaves it out too, with #682, and ScreenCaptureKit with #685), and the engine doesn't draw the pointer; webcams paired with their microphones and the Linux backend set choosing PipeWire or ALSA (#689); stream keys and OAuth tokens in the Secret Service, else a 0600 file, ADR-0071, not yet used by an app (#634); capture copies and NV12 submits ordered on the context's queue (#694); with wave/m3-5 (#854): VA-API HEVC encode (#830) and its hardening (#835), still to run on Intel or AMD hardware; the rest of the codec path to build | #481, #490, #509, #510, #572, #598, #606, #652, #593, #612, #603, #617, #610, #685, #689, #634, #694, #830, #835, #682 |
| LNX2 | Linux OpenH264 download (ADR-0061) | ⏳ planned: store, check, loader and encoder (#536, landed with wave/m3-2, #663); downloading stays off until the pin's two SHA-256 values are recorded from Cisco's server, and the UI's download offer, with Cisco's notice re-read, is still to come | #536 |
| LNX3 | AAC-LC encoder in Rust, patent check first (ADR-0061) | ⏳ planned: patent check ADR-0062 accepted (#508); encoder and Linux wiring (#518), landed with wave/m3-2 (#663); TNS (#723, more tests #727) landed with wave/m3-3 (#786); intensity stereo (#799) landed with wave/m3-4 (#827); before the Linux app ships the encoder its licence information must say JStream holds no AAC patent licence and name Via LA (ADR-0062) | #508, #518, #723, #727, #799 |
| F6b | Choose HEVC or AV1 from a client | ⏳ planned: F6 built the encoders; #807 names the encoder a machine lacks at output.start, landed with wave/m3-4 (#827); #814 (protocol 0.41) adds hevc and av1 to VideoCodec and lets an encoder profile take them, landed with wave/m1-5 (#877) | #807, #814 |
| F7 | Direct NVENC and AMF encoders, if measurements justify them | ⏳ planned: ROADMAP M3: measure Media Foundation's hardware encoders against direct NVENC and AMF first, and build the direct ones only where the numbers say so |  |
| CL2 | A too_large error for oversized plugin state | ⏳ planned: ROADMAP M3 and #102: ADR-0074 (#794, accepted by the maintainer, maintainer tier, landed with wave/m3-5 (#854)) amends ADR-0037's state clause with a stable too_large error, so a batch carrying two 2 MiB CLAP states fails cleanly instead of closing the connection; the build, a protocol minor, follows | #794 |

</details>
<!-- @generated end progress -->

<a id="build"></a>
## Build and run
<!-- @section
-->

To run a release instead, see [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md).

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
