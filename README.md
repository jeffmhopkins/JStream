---
id: README
title: JStream
kind: normative
status: active
rev: 2
review_every: 90d
depends_on: [ARCH, ROADMAP]
verified: {date: 2026-10-13, by: "agent:claude", rev: 2, note: "Added doc-system links; facts cited instead of restated"}
---

# JStream

A live-streaming and compositing engine written in Rust: a ground-up rewrite in
the spirit of OBS, built to be driven equally well by a human or by an AI agent.

- **Stream** to YouTube (RTMPS + YouTube Live API) or any RTMP target
- **Virtual camera** output (Windows 11 Media Foundation virtual camera)
- **Scenes, sources, effects**, 3D transforms and crop on a wgpu compositor
- **Independent video and audio routing**: outputs are not tied to scenes; audio
  runs through a bus-matrix mixer
- **Full agentic control over MCP**: anything the UI can do, an agent can do
- **Headless or desktop**: the engine is a library, embedded in a Tauri desktop
  app or run as a daemon that serves the same web UI to any browser

> Status: **pre-M0 (design)**. The repo currently holds the design and the rules.
> Code starts at M0. See [ROADMAP.md](ROADMAP.md).

## Platforms
<!-- @section
id: platforms
rev: 1
depends_on: [ADR-0008]
-->

| Platform | Status | Minimum |
|---|---|---|
| Windows | Primary target | Windows 11 22H2 [[fact:min-windows]] |
| macOS | Supported later (M3) | macOS 13 [[fact:min-macos]] (ScreenCaptureKit) |
| Linux | Supported later (M3) | Wayland/X11 with PipeWire |

## Documentation
<!-- @section
id: documentation
rev: 2
depends_on: [DOCS]
-->

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) `ARCH`: how the system fits together, crate map, layering
- [docs/RULES.md](docs/RULES.md) `RULES`: engineering rules, each with a permanent ID (`R-RT-001`, …)
- [docs/DOCS.md](docs/DOCS.md) `DOCS`: how documentation is identified, referenced, versioned and kept fresh
- [docs/adr/](docs/adr/) `ADR-NNNN`: architecture decision records, i.e. why things are the way they are
- [ROADMAP.md](ROADMAP.md) `ROADMAP`: milestones
- [CLAUDE.md](CLAUDE.md) `CLAUDE`: instructions for AI agents working in this repo

Docs are tracked per section, cross-referenced with checked `[[ID#section]]` links, tied
to the code they describe, and verified by CI. See [[DOCS]].

## License
<!-- @section
id: license
rev: 1
-->

Licensed under MIT OR Apache-2.0 [[fact:license]], i.e. either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this project, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.

## Revision history

<!-- @generated begin history by="xtask docs sync" -->
| rev | date | by | sections | note |
|---|---|---|---|---|
| 1 | 2026-10-13 | agent:claude | all | Initial version |
| 2 | 2026-10-13 | agent:claude | documentation, platforms | Doc-system links; facts cited |
<!-- @generated end history -->
