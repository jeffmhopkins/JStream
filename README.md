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

| Platform | Status | Minimum |
|---|---|---|
| Windows | Primary target | Windows 11 22H2 |
| macOS | Supported later (M3) | macOS 13 (ScreenCaptureKit) |
| Linux | Supported later (M3) | Wayland/X11 with PipeWire |

## Documentation

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): how the system fits together, crate map, layering
- [docs/RULES.md](docs/RULES.md): engineering rules (code, unsafe, testing, git, PRs)
- [docs/adr/](docs/adr/): architecture decision records, i.e. why things are the way they are
- [ROADMAP.md](ROADMAP.md): milestones
- [CLAUDE.md](CLAUDE.md): instructions for AI agents working in this repo

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this project, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
