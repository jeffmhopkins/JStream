<!-- @doc
id = "CRATE-jstream-control"
title = "jstream-control"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-control

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-control/src/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-control"]
verified = { hash = "b3:93ba8331b08319a8", by = "agent:claude@session_01FuozqmqhGn6SKKprNgPvhC", note = "Merge of main (B2 #176, B3 #183, B6 #184, B8 #171, G3a, E3a) into F3, protocol 0.13: re-read; bullets hold" }
-->

The `Control` trait: the one API through which every client talks to the engine
([ADR-0001](../../docs/adr/0001-library-first-engine.md)). The CLI, the MCP server, the
web UI and the HTTP server are written against `dyn Control` and never link the engine
([R-ARCH-003](../../docs/RULES.md#R-ARCH-003)); the host apps hand them an
implementation. Layer 2: it builds on `jstream-protocol` (layer 1), is WASM-safe, and
needs no async runtime from its callers (the native WebSocket client runs its own).

Contents (a first cut; [Design](#design) explains the choices):
- **The trait** (`control.rs`): `Control` with `hello`, `call` and `events`, and the
  `BoxFuture`, `EventStream` and `MaybeSendSync` types that keep it dyn-compatible.
- **Errors** (`error.rs`): `ControlError`, a `thiserror` enum: the engine's `WireError`,
  or a lost connection.
- **Mock** (`mock.rs`, feature `mock`): `MockEngine` and `MockControl`, an in-memory
  implementation for developing and testing clients without the engine, which
  they never depend on. `jstream-core`'s `InProcessControl` is the real in-process
  implementation; its tests hold it to the mock's answers.
- **WebSocket client** (`ws.rs`, feature `ws`, native targets): `WsControl` and
  `WsConfig`, the `Control` trait over the engine's WebSocket API, and `WS_PATH`, the
  API's path, shared with `jstream-server`.
- `jstream_control::protocol` re-exports `jstream-protocol`.

Not here yet: the browser (`wasm32`) WebSocket client (see [Design](#design)).

<a id="design"></a>
## Design
<!-- @section
covers = ["crates/jstream-control/src/**", "crates/jstream-control/Cargo.toml"]
depends_on = ["docs/ARCHITECTURE.md#protocol", "docs/adr/0006-mcp-full-access.md", "docs/adr/0013-command-model.md"]
verified = { hash = "b3:e5acba063316074e", by = "agent:claude@session_01FuozqmqhGn6SKKprNgPvhC", note = "Merge of main (B2 #176, B3 #183, B6 #184, B8 #171, G3a, E3a) into F3, protocol 0.13: reads list merges main's canvas/placement with encoder.list and encoder profiles; restored the with_output and stream-key bullets main's text had lost mid-sentence" }
-->

### The trait

A `Control` value is one client session. Its three methods mirror the protocol's
envelope ([jstream-protocol › Design](../jstream-protocol/README.md#design)):

- `hello(Hello) -> Welcome`: the handshake. It fixes the session's `client_id`, which
  keys its undo history and audit-log entries
  ([ADR-0013](../../docs/adr/0013-command-model.md)). A second `hello` returns the same
  identity.
- `call(Call) -> Response`: one batch, action, query or history call (undo, redo). The
  implementation chooses the request ID and returns it in the `Response`, so the caller
  can match the `state.changed` event its batch or history call causes. Before `hello` it fails with
  `handshake_required`.
- `events() -> EventStream`: an independent subscription to every event emitted after
  the call, buffered until read, in emission order. It ends when the session or the
  engine goes away.

Every method takes `&self`, so a session can be shared as `Arc<dyn Control>` with
several calls in flight.

Errors are `ControlError`: `Protocol(WireError)` when the engine answered with an error
(clients match on its `code`; the message survives even for codes this client doesn't
know), or `Disconnected` when the transport failed before an answer, in which case
whether the call took effect is unknown. Keeping transport failures apart from protocol
errors means no client mistakes a dropped socket for an engine answer.

### Async shape

Clients hold `dyn Control` ([ARCH › Crate map](../../docs/ARCHITECTURE.md#crate-map)), so
the trait must be dyn-compatible. `async fn` in traits (stable since Rust 1.75) is not,
so each method returns a boxed future, `BoxFuture<'_, T>`, and events come as a boxed
`futures_core::Stream`. The cost is one allocation per call, which is negligible next to
a WebSocket round trip or a command execution.

- **Native targets:** the futures and the stream are `Send`, and `Control` requires
  `Send + Sync`, so the server can share a session across tokio tasks.
- **`wasm32`:** the `Send`/`Sync` bounds are dropped (`MaybeSendSync`), because a browser
  WebSocket handle is not `Send`. Nothing in the crate needs tokio or any executor: the
  UI drives the futures with its own (Leptos uses `wasm-bindgen-futures`).

`futures-core` supplies the `Stream` trait. It is the ecosystem's common stream
interface (tokio-stream, axum, gloo and Leptos all accept it), has no dependencies of
its own, and is used with default features off.

### Mock

`MockEngine` (feature `mock`, off by default; clients enable it in
`dev-dependencies`) keeps scenes, items, sources, outputs and encoder profiles in memory, with an
in-memory stand-in for the stream-key store, and follows the protocol's rules for them:
- the version check in `hello`;
- atomic batches that bump `rev` by exactly one, and per-client idempotency keys. A
  retry with a known key answers first; then, in `Graph::apply`'s order, a stale
  `expected_rev` fails with `rev_conflict` before an empty batch fails with
  `empty_batch`;
- every protocol edit but protocol 0.12's `scene.set_role` and `item.move` (which answer
  `internal`), with the checks `jstream-graph` makes, in the same order per
  edit, and the same error codes and `edits.<n>.<field>` paths:
  - `not_found` and `already_exists`;
  - empty IDs in creating edits (`scene.create`'s `scene_id`, `item.add`'s `item_id`,
    `source.create`'s `source_id`, `source.duplicate`'s `new_source_id`), empty or blank
    names, and empty builtin kind names;
  - reorder indices out of range;
  - a placement edit's `scene_id` naming a scene other than the item's own;
  - both `transform` and `geometry` on `item.add`, a geometry that is not positive,
    not finite or not neutral in 3D, and a canvas side of 0 or over 8192;
  - deleting a source that an item places, or a scene that a scene source shows, or
    either while a source's settings refer to it;
  - nesting cycles;
  - RTMP server URLs in `output.create` and `output.set_target` (`jstream-graph`'s
    checks and reasons, which never repeat the URL), and deleting an output while it
    runs, which the engine refuses through `Graph::apply_with_running`;
  - recording paths in `output.create` (`jstream-graph`'s platform-independent rule and
    reasons), `output.set_target` on a recording output, and an `unknown` output kind,
    RTMP target or container;
  - encoder profiles: `EncoderProfile::problem`'s ranges in `encoder.create` and
    `encoder.update`, deleting a profile an output uses, and `output.set_encoder`
    naming a missing output, then a missing profile.

  Error messages may differ; clients match on codes and fields. The mock describes no
  kinds, so it accepts any non-empty builtin kind, stores settings as sent without
  checking them or filling in defaults (a `null` in a `source.set_settings` patch
  removes the entry), and doesn't follow settings references when it looks for cycles.
  `kind.list` and `kind.describe` answer `internal`;
- every collection query, answered from that state with the results
  `jstream-graph`'s `Graph::query` gives for the same edits: `state.get`, `scene.list`,
  `source.list` (ordered by ID), `item.list` (bottom first; `not_found` for a missing
  scene), `output.list` and `encoder.list` (ordered by ID), each at the current `rev`.
  So it keeps everything an edit sets: the canvas, source names, kinds and settings,
  each item's placement (its transform, or its geometry and the transform view of it),
  crop and visibility, each output's kind, target and encoder profile, and each
  profile. `video.snapshot` is not one of those reads: the mock has no compositor and
  answers it with `internal`, as it does for any protocol variant it does not
  implement;
- outputs as the engine (`jstream-core`) keeps them: a configured output starts
  `stopped` and is forgotten once deleted. `MockEngine::with_output` declares an output
  the way `Engine::with_output` does: it can be started, stopped and read with
  `output.status`, but it is not in the collection, so `output.list` and `state.get`
  don't report it and `output.set_key` answers `not_found` for it. It runs no
  encoders, so `output.status` reports no encoder session and zero sessions;
- the stream-key actions with the engine's checks and errors. `output.set_key` answers
  `not_found` for an output not in the collection, then `invalid_argument` (field
  `key`) for an empty, over-long or whitespace key; `output.clear_key` always succeeds.
  The keys live in memory, outside the collection: no `rev`, no event, kept when their
  output is deleted. `output.status` reports `key_set` from them, and
  `MockEngine::stream_key` lets a client test check what was stored. Unlike an
  `Engine` built without `with_key_store`, which answers `action_failed`, the mock
  always has a store, so clients can test the whole flow;
- `state.changed` events carrying the request ID, and `output.start`/`output.stop`
  reported as `output.state_changed` events (`starting` → `active`, `stopping` →
  `stopped`). `MockEngine::emit` injects any other event;
- event streams that end, after what they have buffered, when their `MockControl` or
  the engine is dropped, the way a socket's streams end when it closes. An ended stream
  stays ended: later events, including those other sessions cause, never reach it.

Its futures are always ready. It has no undo, persistence, credential store or real
outputs, and answers a protocol variant it doesn't know with `internal`.

The mock re-implements these checks and reads rather than calling
`jstream_graph::Graph::apply` and `Graph::query`. `jstream-graph` is also layer 2, and
[R-ARCH-002](../../docs/RULES.md#R-ARCH-002) allows only strictly lower layers, even
for dev-dependencies, so the dependency would need a layering change. The duplication
is small, and both crates' tests run the shared fixture `jstream_protocol::parity` (a
dev-dependency with the `fixtures` feature): one batch that builds a collection with
its outputs, the exact answer to every read of it, and the exact error for batches
rejected against it, RTMP URL and recording path rejections included.
If the mock and the graph drift apart on any of those, one of the two tests fails. The
mock's own tests pin the remaining shared error paths.

### WebSocket client

`WsControl` (feature `ws`, off by default) implements `Control` over the engine's
WebSocket API, served by `jstream-server`. The feature's dependencies are declared for
native targets only, so a `wasm32` build never pulls tokio, even with `ws` on.

- **Transport:** `tokio-tungstenite` on native targets, with no TLS feature: plain
  `ws://` only for now, so no `rustls` or `ring` is in the tree. TLS (rustls with the
  ring provider, [R-DEP-006](../../docs/RULES.md#R-DEP-006)) and the browser
  `WebSocket` through `gloo-net` on `wasm32` are open questions below.
- **No runtime required of the caller.** `WsControl::connect` starts one thread per
  connection running a single-threaded tokio runtime. Every future and stream it
  returns is completed through tokio's channels, which work under any executor, so the
  CLI's thread-parking `block_on` and the MCP stdio loop drive it as they drive the mock.
- **Connection:** `ws://<host>:<port>`, with `/api/v1/ws` (`WS_PATH`) when the URL has
  no path, always with the control token
  ([ADR-0006](../../docs/adr/0006-mcp-full-access.md)). It goes in the upgrade request
  rather than the URL, so it stays out of logs: an `Authorization: Bearer` header from
  native clients, and later the `Sec-WebSocket-Protocol` header from browsers, which
  can't set others. The caller supplies the token (the CLI reads it from a token file).
  The server validates `Host`, and `Origin` when present; browsers set `Origin`
  themselves, and the native client sends the server's own origin
  (`http://<host>:<port>` of the URL) unless `WsConfig::origin` names another.
- **Session:** `hello` is the caller's first call. Requests get the IDs `req_1`,
  `req_2`, … per session, and the server reports the `state.changed` event a call
  causes with that same ID. The connection thread routes each `response` or `error` to
  its waiting call by request ID (`welcome`, and an error without an ID, answer the
  oldest pending `hello`) and fans `event`s out to the subscriptions.
- **Failures are `Disconnected`.** An invalid or `wss://` URL, an unreachable engine, or
  a refused upgrade (the reason names HTTP 401 for the token, 403 for `Origin` or
  `Host`) fails `connect`. When the socket closes, pending and later calls fail with
  `Disconnected` carrying the reason (the server's close reason when it gave one), and
  every event stream ends after what it buffered. Dropping the `WsControl` closes the
  socket. Reconnecting is the caller's choice (a new session, then re-read the state),
  because a new connection may get a new `client_id`.

The end-to-end tests live in `jstream-server`, which runs a `MockEngine` behind a real
server on a loopback port and drives it with this client.

### Open questions

1. **Backpressure.** The client's own event buffers are unbounded. `stats.tick` (protocol
   0.6) is emitted about once a second while the pipeline runs, and a slow subscriber of
   it, or of meters, would grow without limit. Bounded buffers that
   drop the oldest events (a gap in `rev` already shows a missed `state.changed`) are
   the likely client-side answer
   ([protocol open question 7](../jstream-protocol/README.md#open-questions)). The
   server side is built: `jstream-server` gives each connection a bounded buffer and, on
   overflow, drops the queued events and sends one `events.dropped` notice
   ([ARCH › Control protocol](../../docs/ARCHITECTURE.md#protocol)). `WsControl` passes
   it on to every subscriber like any other event, and they re-read state with
   `state.get`. The mock and the in-process engine never send it.
2. **Request IDs.** The implementation assigns them. A caller that wants its own
   (correlating with an external log) would need `call` to take a `Request`.
   `jstream-server` works around it by mapping session IDs back to wire IDs
   ([jstream-server › Design](../jstream-server/README.md#design)).
3. **Browser client.** The `wasm32` implementation over `gloo-net`, with the token in
   `Sec-WebSocket-Protocol`, comes with the web UI.
4. **TLS.** `wss://` needs a TLS feature of `tokio-tungstenite` (rustls with ring) and
   its wiring. `ring` is already on the lint-deps native-code allowlist, for
   `jstream-outputs`' RTMPS, so that part needs no further governance change.
5. **Timeouts.** `connect` and calls have no timeout of their own; a caller that needs
   one wraps the future. A connect timeout and keep-alive pings may belong here once
   remote (non-loopback) use exists.
