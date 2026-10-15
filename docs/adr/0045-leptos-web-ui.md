<!-- @doc
id = "ADR-0045"
title = "One web UI on Leptos, confirmed for M2"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0045: One web UI on Leptos, confirmed for M2

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0002](0002-web-ui-everywhere.md) chose one web UI for the desktop app and the
browser, built with [Leptos](https://leptos.dev/), and left Leptos provisional until M2.
M2 has started ([ROADMAP › M2](../../ROADMAP.md#m2)). The maintainer chose Leptos on the
M2 planning decision card, so the framework is no longer open. ADR-0002 is accepted and
can't be edited (R-DOC-009), so this ADR restates its decision without the proviso and
adds what building the first UI crate settled. The framework choice is architecture,
which is agent tier ([ADR-0043](0043-narrow-maintainer-tier.md)).

Building `jstream-ui` also settled three things ADR-0002 left open: how the UI is
rendered and built, where it sits in the crate layers, and how a browser presents the
control token, since a page can't set `Authorization` on a WebSocket.

<a id="decision"></a>
## Decision
This ADR supersedes ADR-0002 and keeps its decision whole:

- The UI is a Rust web app compiled to WASM (`jstream-ui`), built with Leptos. The
  desktop app is a [Tauri](https://v2.tauri.app/) shell that embeds the engine
  in-process. The daemon serves the same UI over HTTP. In both cases the UI talks to the
  engine over the authenticated WebSocket API. Video preview is a low-resolution H.264
  stream sent as binary WebSocket frames and decoded with WebCodecs into a `<canvas>`,
  with JPEG frames as the fallback.
- **Leptos is confirmed.** Switching frameworks means a new ADR that supersedes this
  one.

And adds:

1. **Client-side rendering only.** `jstream-ui` uses Leptos's `csr` feature: no
   server-side rendering, hydration or server functions, because the engine's API is the
   WebSocket protocol and the daemon only serves static files. It is built with
   [trunk](https://github.com/trunk-rs/trunk) into a static `dist/` (an `index.html`, the
   `wasm-bindgen` glue, the `.wasm` and the styles), which the daemon serves and the
   desktop shell loads.
2. **Layer 3.** `jstream-ui` depends on `jstream-control` (layer 2) and
   `jstream-protocol` only ([R-ARCH-003](../RULES.md#R-ARCH-003)), so it sits at
   layer 3 in the crate map, like `jstream-mcp`. It is a workspace member, so native
   `cargo clippy` and `cargo test` cover its logic; CI also builds it for `wasm32` with
   trunk.
3. **The browser transport lives in `jstream-control`.** Its `ws` feature gives
   `WsControl` on `wasm32` too, over the browser's `WebSocket` through `web-sys`, with
   the same `Control` behaviour as the native client. The UI holds a `dyn Control` and
   never touches the socket.
4. **The token rides in a subprotocol.** The browser client offers two subprotocols,
   `jstream` and `jstream.token.<token>`. The server takes the token from the second,
   selects `jstream`, and never echoes the token. The token is an HTTP token (the
   daemon's are lowercase hex), so it is a valid subprotocol name. Native clients keep
   sending `Authorization: Bearer`. The token is the same credential either way, so
   what a client may do is unchanged. Servers must not log the `Sec-WebSocket-Protocol`
   header, as they don't log `Authorization` (R-SEC-001). A page the engine served
   behind credentials the browser already sends (HTTP Basic) can connect with no token:
   the client then offers no subprotocol, and the browser's credentials go with the
   upgrade. How the daemon hands a browser its credentials is the pairing work's
   decision, not this ADR's.

<a id="consequences"></a>
## Consequences
- The UI can be built on now; the panels, the canvas and the desktop shell build on
  `jstream-ui`'s shell.
- Leptos brings about a hundred crates, some of them for features the UI doesn't use
  (`server_fn`, `leptos_config`), and a few duplicates and an unmaintained build-time
  macro crate (`paste`). They are recorded in `deny.toml` as declared exceptions, each
  with the Leptos crate that pins it, to be dropped as Leptos moves on.
- `jstream-server` must accept the token subprotocol before a browser can connect.
  Until it does, the UI connects only to an engine that accepts it.
- Preview remains not zero-copy: a few frames of preview latency are accepted, and
  program output latency is unaffected. Preview needs its own small encoder session.
