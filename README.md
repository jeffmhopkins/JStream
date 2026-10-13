+++
id = "README"
title = "JStream"
kind = "normative"
status = "active"
review_every = "90d"
+++

# JStream

A live-streaming and compositing engine written in Rust: a ground-up rewrite in the spirit of
OBS, built to be driven equally well by a human or an AI agent.

- **Stream** to YouTube (RTMPS + YouTube Live API) or any RTMP target
- **Virtual camera** (an OS-native Windows camera that every app can see)
- **Scenes, sources, effects**, 3D transforms and crop on a wgpu compositor
- **Independent video and audio routing**: outputs are not tied to scenes, and audio runs
  through a bus-matrix mixer
- **Full agentic control over MCP**: anything the UI can do, an agent can do
- **Headless or desktop**: the engine is a library, embedded in a Tauri app or run as a
  daemon that serves the same web UI to any browser

> Status: **pre-M0 (design)**. See [ROADMAP.md](ROADMAP.md).

<a id="platforms"></a>
## Platforms
<!-- @section
depends_on = ["docs/adr/0008-windows-support-policy.md"]
verified = { hash = "bootstrap", by = "agent:claude", note = "v2: support policy instead of fixed version" }
-->

| Platform | Status | Support |
|---|---|---|
| Windows | Primary target | Runs on Windows 11 build 22000<!--fact:min-windows-build--> and later; supported on the versions Microsoft currently services ([ADR-0008](docs/adr/0008-windows-support-policy.md)) |
| macOS | Later (M3) | To be decided in the macOS ADR |
| Linux | Later (M3) | Codec path to be decided in an ADR first ([ADR-0003](docs/adr/0003-media-stack-and-codecs.md)) |

<a id="documentation"></a>
## Documentation
<!-- @section
-->

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): how the system fits together
- [docs/RULES.md](docs/RULES.md): engineering rules, each with a permanent ID
- [docs/DOCS.md](docs/DOCS.md): how docs stay accurate (hash-checked sections, history from git)
- [docs/adr/](docs/adr/): decision records
- [ROADMAP.md](ROADMAP.md): milestones
- [CLAUDE.md](CLAUDE.md): the entry point for builder agents
- [REVIEW.md](REVIEW.md): the reviewer agent's instructions; its verdict gates merges

<a id="license"></a>
## License
<!-- @section
-->

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option. Unless you explicitly state otherwise, any
contribution intentionally submitted for inclusion in this project, as defined in the
Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
