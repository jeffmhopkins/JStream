<!-- @doc
id = "ADR-0021"
title = "Browser (web page) source"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0021: Browser (web page) source

<a id="context"></a>
## Context
The maintainer requires web pages as sources. These are used for overlays, alerts, chat
widgets and dashboards, the most common OBS source after capture. The roadmap parked it
under "Later" with "needs an ADR".

A browser source must:
- render a URL or local HTML file off-screen at a chosen size and frame rate, with a
  transparent background;
- hand frames to the compositor on the GPU without a CPU copy (ADR-0004);
- capture the page's audio as a separate source audio stream (ADR-0007);
- accept custom CSS and page interaction (click, scroll, keyboard) from the UI;
- fit ADR-0020: a portable zip, nothing to install.

Options on Windows:

1. **WebView2, the OS-provided Chromium engine.** Tauri's UI already uses it
   ([Tauri](https://v2.tauri.app/)), and Windows 11 ships it.
   - **Rendering:** a hidden, composition-hosted WebView2 with a transparent default
     background, captured with
     [Windows.Graphics.Capture](https://learn.microsoft.com/en-us/windows/uwp/audio-video-camera/screen-capture)
     straight into a GPU texture. That is the same path as window capture (spike S1).
   - **Audio:** WASAPI
     [process loopback](https://learn.microsoft.com/en-us/windows/win32/api/audioclientactivationparams/ns-audioclientactivationparams-audioclient_process_loopback_params)
     on the WebView2 browser process (spike S3 covers process loopback).
   - **Cost:** no added download and no native build dependency, only the `windows`
     crate.
   - **Drawbacks:** frame pacing follows the desktop compositor rather than a fixed rate,
     and several pages share browser processes, so separating audio per page needs one
     WebView2 environment per audio-producing source.
2. **CEF (Chromium Embedded Framework)**, the engine OBS uses.
   - **Rendering:** mature off-screen rendering into shared GPU textures, with exact frame
     rates and per-browser audio callbacks.
   - **Cost:** it adds about 150–250 MB to the portable zip and a large C++ native
     dependency (a lint-deps allowlist entry and a governance change). It also needs a
     Chromium update every few weeks for security fixes.
   - **Codec policy:** `libcef.dll` statically links Chromium's media stack, including
     its FFmpeg build (LGPL-2.1+), to play `<audio>` and `<video>` in pages. That conflicts
     with [ADR-0003](0003-media-stack-and-codecs.md) ("no FFmpeg, GStreamer, or bundled
     C/C++ codec libraries") and with the no-LGPL license policy, so choosing CEF means
     an explicit exception to both.
3. **Servo** (pure Rust): not mature enough for real-world overlay pages today.

WebView2 avoids the codec conflict because its Chromium, FFmpeg included, is an OS
component that JStream does not ship.

<a id="decision"></a>
## Decision
- **Engine: CEF (option 2).** The maintainer chose the Chromium Embedded Framework over
  WebView2. CEF gives exact control of frame rate, off-screen rendering into shared GPU
  textures, and per-browser audio callbacks. It is the proven OBS approach.
- **Rendering:** CEF off-screen rendering with accelerated paint (shared D3D11 texture
  handles). Those textures are opened on the engine's device and imported into wgpu, using
  the same interop path as capture (ADR-0004 and spike S1). There is no CPU copy, the
  background is transparent, and each source sets its own frame-rate cap.
- **Audio:** CEF's per-browser audio handler delivers PCM. It feeds the source's audio
  stream directly, so process loopback isn't needed and every source is isolated (ADR-0007).
- **Packaging (fits ADR-0020):** the CEF runtime (`libcef.dll` and its resources, about
  150–250 MB) ships inside the portable zip, in its own folder next to the exes. It needs no
  installer and no admin. CEF's helper subprocesses run as the same exe with a
  subprocess flag, or as a small `jstream-browser-helper.exe` in the zip.
- **Dependency:** Rust bindings to CEF (to be picked in spike S4). The prebuilt CEF runtime
  is native code, so its addition must come with a lint-deps native-code allowlist entry
  and a declared governance change.
- **Where the code lives:** the CEF FFI and backend live in the platform crates
  (`jstream-platform-windows` first), under the `unsafe` rules there. `jstream-sources`
  only gains the `browser` kind, with the backend injected like every other source.
- **Exception to ADR-0003 (partially supersedes it):** this ADR replaces ADR-0003's "no
  bundled codec libraries" rule for one artifact only: the prebuilt CEF runtime and the
  media stack inside it, FFmpeg included. The exception is bounded:
  - It covers decoding inside the browser page only. JStream's own encode and decode
    paths stay on OS and GPU-vendor APIs, exactly as ADR-0003 decides, and no JStream
    crate may call CEF's or FFmpeg's codecs directly.
  - The runtime is a standard CEF binary distribution built with
    `proprietary_codecs` off (the CEF default), so it carries only royalty-free codecs
    (VP8/VP9/AV1, Opus, Vorbis, FLAC, MP3, WAV). H.264 and AAC playback in pages is not
    available.
  - Every other part of ADR-0003 stands.
- **LGPL-2.1 obligations:** `libcef.dll` is a separate DLL that JStream loads dynamically,
  which satisfies LGPL's relinking requirement. The portable zip ships CEF's license and
  credits file (`CREDITS.html`) and the FFmpeg LGPL text. Each release notes, or links to,
  the exact Chromium FFmpeg source revision that runtime was built from. The license policy
  rules ([R-DEP-001](../RULES.md#R-DEP-001), [R-DEP-002](../RULES.md#R-DEP-002)) name this
  exception, a declared governance change made with this ADR.
- **Security updates:** the CEF version is pinned. Release builds track CEF stable for
  Chromium security fixes, and a scheduled check flags when the pinned CEF falls behind.
- **Prove it first:** spike **S4** must show both of these before the source is built:
  - a transparent 1080p60 page reaching the compositor through accelerated paint, with no
    CPU copy;
  - per-source audio through the audio handler.

  If it fails, a new ADR revisits the choice.
- **The source:** `jstream-sources` gains a `browser` source kind with these settings: URL
  or local file, width, height, frame-rate cap, custom CSS, "shut down when not visible"
  and "refresh when shown". Page interaction goes through protocol actions such as
  `source.interact`.
- **Isolation:** pages run with no access to the JStream control API unless the user
  grants it to that source.
- **Other platforms:** CEF also supports macOS and Linux off-screen rendering, so the same
  approach carries over when those platform crates land (M3).
- **The UI is unaffected:** the desktop UI keeps using WebView2 through Tauri (ADR-0002).
  CEF is only for browser sources.

<a id="consequences"></a>
## Consequences
- **Roadmap:** browser source moves from "Later" into the plan. Spike S4 comes first, and
  the source ships with the M1b sources.
- **Download size:** the portable zip grows by about 150–250 MB for the CEF runtime. That is
  the price of exact frame pacing and isolated per-source audio.
- **Upkeep:** CEF needs regular updates for Chromium security fixes, which is an ongoing
  maintenance duty.
- **Security:** pages are untrusted content inside a process that holds stream
  credentials. This means:
  - CEF runs sandboxed where supported;
  - there is no automatic control-API access;
  - a per-source "allow page to control JStream" opt-in exists, off by default;
  - there is no file access outside an explicit local file or folder.
- **A second large native dependency** joins the build. It is declared through lint-deps
  and governance, not hidden.
- **Licensing is no longer purely permissive.** ADR-0003's promise of clean, permissive
  licensing now has one named, LGPL-2.1 exception: the CEF runtime. Releases carry its
  notices and source pointer. A user who never adds a browser source still downloads it.
