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
depends_on = ["docs/adr/0140-source-types-and-decoding-scope-v3.md", "docs/adr/0066-portable-windows-distribution-v2.md"]
-->

- **M1a Thin slice to YouTube:** WGC display capture → compositor (one scene) → MF H.264 +
  MF AAC (WASAPI mic + desktop loopback) → FLV → RTMPS to YouTube with a stream key. Minimal
  `jstream-core`, `jstream-server`, `jstream-mcp`, `jstream-cli`, `video.snapshot`.
- **M1b Scenes and mixing:**
  - Scene graph and source kinds
    ([ADR-0140](docs/adr/0140-source-types-and-decoding-scope-v3.md)): display, window,
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
- Source kinds ([ADR-0140](docs/adr/0140-source-types-and-decoding-scope-v3.md)): **media
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
  macOS stays unsigned, and its browser sources are to come from CEF as on Windows; the
  plan, and what can't work unsigned, is in [macOS without signing](#macos-unsigned).
  A Syphon receiver, the macOS follow-up to Spout2, is its own item when wanted.
- Linux: the platform crate per [ADR-0060](docs/adr/0060-linux-platform-support.md)
  (portal and PipeWire capture, VA-API encode). Per
  [ADR-0140](docs/adr/0140-source-types-and-decoding-scope-v3.md), software H.264 is Cisco's
  OpenH264 binary, downloaded on the user's request, and AAC is an AAC-LC encoder in Rust,
  built once its own patent-check ADR is accepted. The virtual camera via v4l2loopback
  needs root, so it gets its own ADR.

<a id="macos-unsigned"></a>
## macOS without signing: the plan
<!-- @section
depends_on = ["docs/adr/0063-macos-platform-support.md", "docs/adr/0144-the-desktop-app-on-macos.md"]
-->

The maintainer's decision: **JStream on macOS stays unsigned for good.** There will be no
Apple Developer Program membership, so no Developer ID signing and no notarisation. This
answers ADR-0063's question 1 "never" where it said "later". Nothing below is built yet;
it is the plan, and each part gets its own ADR when its work starts (ADR-0144 keeps the
browser source out of the macOS app until then).

- **Browser sources come from CEF**, the Chromium the Windows and Linux builds already
  ship, reusing their integration: the sandboxed `jstream-browser-helper` (ADR-0048) with
  CEF's runtime next to it, and the hosts' `cef` feature. On macOS CEF needs its
  framework in `JStream.app/Contents/Frameworks` and its helper app bundles (the plain,
  GPU, renderer and plugin helpers) beside it. `scripts/mac/bundle-app.sh` would sign each
  helper and the framework ad hoc from the inside out, then the app. To check on a Mac
  before it ships: that the helpers launch from a downloaded, quarantined image once the
  user has allowed the app, and that CEF's macOS sandbox works with no entitlements, as an
  ad-hoc build has none.
- **Releases gain an unsigned `.dmg` later**: a macOS job in `release.yml` runs
  `scripts/mac/bundle-app.sh` and uploads `JStream-<version>-macos-<arch>.dmg`, as the
  after-review `macos-app` job does for PRs. Its release text says how to open an
  unsigned app.

**What works unsigned:** everything ADR-0063 builds on. Screen and window capture,
webcams, microphones and process audio taps (each with the user's permission), the
hardware encoders, streaming and recording, stream keys in the login Keychain, the
desktop app and its global hotkeys, and, with CEF, browser sources.

**What an unsigned build can't do, or does worse:**
- **No camera extension of JStream's own.** macOS's only supported virtual camera is a Core
  Media I/O camera extension, a system extension. Installing one needs the restricted
  System Extension entitlement, which only comes in a provisioning profile from a paid
  team ([Apple's capability table](https://developer.apple.com/help/account/reference/supported-capabilities-macos)
  lists it for paid memberships and Developer ID only, so a free Apple ID's personal team
  can't have it). The virtual camera goes through another app's signed extension
  instead ([below](#macos-vcam-source)).
- **A Gatekeeper detour on first open.** A downloaded copy is blocked until the user
  allows it in System Settings › Privacy & Security; since macOS 15 Control-click no
  longer skips that.
- **Permissions forgotten after each update.** macOS ties Screen Recording, Camera,
  Microphone and Accessibility grants to the code signature, and an ad-hoc signature
  changes with every build, so each new version asks again, and the Keychain asks again
  for stored keys.
- **No data-protection or iCloud Keychain**, which need a signed app's keychain
  entitlements; keys stay in the login keychain on that Mac (ADR-0063).
- **No Mac App Store listing**, which needs a paid membership too.

<a id="macos-vcam-source"></a>
### The virtual camera without a developer account
JStream can't ship a camera extension of its own, and building on your own Mac removes
the quarantine but not the entitlement. So the virtual camera on macOS comes from a
camera extension someone else signed, or from one of the routes after it. None is built
yet; the virtual camera is its own ADR.

1. **Feed OBS Studio's signed camera extension (the plan for everyone, download or
   source).** OBS Studio is free, and from OBS 30 on macOS 13 and later it installs a
   notarised Core Media I/O camera extension, "OBS Virtual Camera". The extension has a
   sink stream that the OBS app writes frames into with the public Core Media I/O calls
   (`CMIOStreamCopyBufferQueue`, then enqueueing sample buffers), and any other process
   can write to it the same way: community tools such as pyvirtualcam and webcam-crop
   already do, without OBS running. JStream would find the device, open its sink stream
   and enqueue Program frames, and Zoom, FaceTime or a browser then see the camera. The
   user installs OBS once, clicks Start Virtual Camera there once so macOS installs and
   approves the extension, and quits it.
   - **Licence:** JStream only calls Apple's APIs on a separately installed program and
     ships none of OBS's GPL code, so its own licence is unaffected. Nothing may be
     copied from OBS's source; the device's identity and frame format are read from the
     running system.
   - **Risks:** OBS doesn't document the sink stream as an interface, so its device ID,
     stream order or formats can change in an OBS release and break the feed; JStream
     would check them and say so instead of failing silently. Only one writer at a time,
     so OBS's own virtual camera and JStream's can't both run. The camera shows as "OBS
     Virtual Camera", at the formats OBS's extension offers. All of this needs a Mac to
     confirm before it ships.
   - **Not chosen:** NDI Tools' Virtual Input (a signed extension fed by an NDI stream)
     would need an NDI sender in JStream on the proprietary NDI SDK, and NDI's own docs
     report it unreliable on recent macOS. Syphon Virtual Webcam and CamTwist rely on
     DAL plug-ins, which no longer load.
2. **Your own paid Apple Developer account (the one that works normally).** You would
   sign the app and its extension with your own team: `scripts/mac/bundle-app.sh` already
   takes a Developer ID identity, and would also take your provisioning profile for the
   extension. Install `JStream.app` in `/Applications`, turn the camera on in JStream,
   and approve it once in System Settings (on recent macOS, Privacy & Security ›
   Extensions, under Camera or Media Extensions).
3. **Apple's developer test mode, with System Integrity Protection off.** This is how
   Apple's own engineers test system extensions, and it lowers the Mac's security, so it
   is for developers only: start in Recovery, run `csrutil disable` in Terminal, restart,
   run `systemextensionsctl developer on`, then install and approve as in 2. Whether an
   ad-hoc signed extension loads this way on current macOS is unverified, and forum
   reports show it failing on some 26.x releases; it needs a Mac to try before the docs
   promise it.
4. **Not a legacy DAL plug-in.** The older plug-in kind stopped loading in macOS 14.1.
   Apple's switch to bring it back
   ([`system-override legacy-camera-plugins-without-sw-camera-indication=on`](https://support.apple.com/HT213969),
   run in Recovery) hides the camera-in-use light, works only in apps that load
   third-party code, and is deprecated, so JStream won't build one.

<a id="beta"></a>
## Beta: what the first beta needs
<!-- @section
depends_on = ["docs/runbooks/release.md#version", "docs/runbooks/windows-hardware-pass.md#setup"]
-->

The alphas add features. The first beta, `0.1.0-beta.1`
([RUNBOOK-RELEASE › The version](docs/runbooks/release.md#version)), adds none: it is what
the alphas ship, made dependable on Windows. Its targets are the `beta.1` release in
[`docs/progress.toml`](docs/progress.toml), which the README shows once alpha.5 ships. The
maintainer set its scope:

- **Windows only.** Linux and macOS stay in [M3](#m3) and their hardware runs don't gate
  it.
- **Unsigned**, like the alphas. A code-signing certificate isn't part of it.
- **No promise that past projects load.** A beta build need not load a project saved by
  an earlier build.

It ships when all of these hold on the candidate build. The list is unranked; the
maintainer orders it when the beta is gathered:

- **The bench passes.** `jstream bench` passes on the Windows pass machine
  ([Setup](docs/runbooks/windows-hardware-pass.md#setup)), with the thread model and the
  seamless preview alpha.5 aims for.
- **Every Windows run passes.** Each merged item marked *Windows run* in the README's
  What's left table passes it, in one acceptance pass on the candidate build rather
  than one run at a time.
- **No known Windows bug breaks a core flow.** Going live, recording, the virtual camera,
  browser pages, media files and snapshots work, and stopping is graceful.
- **The security basics hold**: the threat-model follow-ups, a permission policy for
  browser pages, a GPU-hang story for shader packs, and asking before a new agent
  connects.

Everything else, new features, Linux and macOS included, goes to the alphas before it or
to the releases after it.

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
