# 0002: One web UI for desktop and browser

- Status: Accepted (framework choice: Proposed)
- Date: 2026-10-13

## Context
We want a desktop app and remote control from a browser without maintaining two UIs.

## Decision
The UI is a Rust web app compiled to WASM (`jstream-ui`). The desktop app is a Tauri
shell embedding the engine in-process; the daemon serves the same UI over HTTP.
Framework: **Leptos** (proposed; Dioxus is the alternative. Confirm before M2).
Video previews reach the UI as a low-latency stream drawn into `<canvas>`.

## Consequences
- One UI codebase; desktop and browser look and behave the same.
- Preview is not zero-copy from the GPU; we accept a few frames of preview latency
  (program output latency is unaffected).
- The UI depends only on `jstream-protocol`, never on core.
