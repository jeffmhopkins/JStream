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
3. **Servo** (pure Rust): not mature enough for real-world overlay pages today.

<a id="decision"></a>
## Decision
- **Engine (Windows):** WebView2 (option 1).
  - It costs no download and no native dependency, and it fits the portable zip.
  - It reuses the capture and process-loopback paths that spikes S1 and S3 already
    exercise.
  - CEF stays the fallback if WebView2 can't meet the frame-pacing or audio-isolation
    needs.
- **Prove it first:** spike **S4** must show both of these before the source is built:
  - a transparent 1080p60 page reaching the compositor with no CPU copy;
  - per-source audio.

  If either fails, a new ADR revisits CEF.
- **The source:** `jstream-sources` gains a `browser` source kind with these settings:
  URL or local file, width, height, frame-rate cap, custom CSS, "shut down when not
  visible" and "refresh when shown".
  - Page interaction goes through protocol actions such as `source.interact`.
  - Pages run sandboxed. They get no access to the JStream control API unless the user
    grants it to that source.
- **Other platforms:** macOS (WKWebView) and Linux (WebKitGTK) get the equivalent when
  their platform crates land (M3). Each needs its own note in ARCHITECTURE, not a new ADR,
  unless the approach changes.

<a id="consequences"></a>
## Consequences
- **Roadmap:** browser source moves from "Later" into the plan. Spike S4 comes first, and
  the source ships with the M1b sources.
- **Security:** pages are untrusted content inside a process with stream credentials.
  This means:
  - sandboxed WebView2 profiles;
  - no automatic control-API access;
  - a per-source "allow page to control JStream" opt-in, off by default;
  - no file access outside an explicit local file or folder.
- **Frame pacing:** pacing tied to the compositor means a browser source may deliver
  uneven frame timing under load. The compositor already handles sources at independent
  rates.
- **Audio:** per-source audio isolation may cost one WebView2 environment, and so one set
  of browser processes, per audio-producing browser source.
