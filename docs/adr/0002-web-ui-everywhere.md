---
id: ADR-0002
title: "One web UI for desktop and browser"
kind: adr
status: accepted
decided: 2026-10-13
rev: 1
review_every: 180d
depends_on: [ADR-0001, ext:leptos, ext:tauri]
verified: {date: 2026-10-13, by: "agent:claude", rev: 1, note: "Initial version, recorded from design Q&A with the maintainer"}
---

# 0002: One web UI for desktop and browser


## Context
We want a desktop app and remote control from a browser without maintaining two UIs.

## Decision
The UI is a Rust web app compiled to WASM (`jstream-ui`). The desktop app is a Tauri
shell embedding the engine in-process; the daemon serves the same UI over HTTP.
Framework: **Leptos** [[ext:leptos]] in a **Tauri** shell [[ext:tauri]]. The framework
choice is provisional: confirm it before M2. Changing it means a new ADR superseding this one.
Video previews reach the UI as a low-latency stream drawn into `<canvas>`.

## Consequences
- One UI codebase; desktop and browser look and behave the same.
- Preview is not zero-copy from the GPU; we accept a few frames of preview latency
  (program output latency is unaffected).
- The UI depends only on `jstream-protocol`, never on core ([[RULES#R-ARCH-003]]).
