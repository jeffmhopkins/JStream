<!-- @doc
id = "ADR-0002"
title = "One web UI for desktop and browser"
kind = "adr"
status = "superseded"
superseded_by = "docs/adr/0044-leptos-web-ui.md"
-->

# ADR-0002: One web UI for desktop and browser

<!-- @generated begin adr-status -->
> **Superseded** by [ADR-0044: One web UI on Leptos, confirmed for M2](0044-leptos-web-ui.md). This decision no longer applies.
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
We want a desktop app and remote control from a browser, without maintaining two UIs.

<a id="decision"></a>
## Decision
The UI is a Rust web app compiled to WASM (`jstream-ui`), built with
[Leptos](https://leptos.dev/). The desktop app is a [Tauri](https://v2.tauri.app/) shell
that embeds the engine in-process. The daemon serves the same UI over HTTP. In both cases
the UI talks to the engine over the authenticated WebSocket API. Video preview is a
low-resolution H.264 stream sent as binary WebSocket frames and decoded with WebCodecs
into a `<canvas>`, with JPEG frames as the fallback.

Leptos is provisional until M2. Switching frameworks means writing a new ADR that
supersedes this one.

<a id="consequences"></a>
## Consequences
- One UI codebase, which behaves the same on desktop and in a browser.
- Preview is not zero-copy: a few frames of preview latency are accepted. Program output
  latency is unaffected.
- Preview needs its own small encoder session (see encoder sharing in the architecture
  data model).
