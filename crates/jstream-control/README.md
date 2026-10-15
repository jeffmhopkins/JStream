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
verified = { hash = "b3:a82a8c2831e201b2", by = "agent:claude@session_01D4rY84zMtURygmdMVDikoc", note = "D5 merge of main at G3b (virtual_cam, 0.18): re-read against both sides; mock keeps buses and sends beside main's virtual camera; holds" }
-->

The `Control` trait: the one API through which every client talks to the engine
([ADR-0001](../../docs/adr/0001-library-first-engine.md)). The CLI, the MCP server, the
web UI and the HTTP server are written against `dyn Control` and never link the engine
([R-ARCH-003](../../docs/RULES.md#R-ARCH-003)); the host apps hand them an
implementation. Layer 2: it builds on `jstream-protocol` (layer 1), is WASM-safe, and
needs no async runtime from its callers (the native WebSocket client runs its own, and
the browser one runs on the page's event loop).

Contents (a first cut; [Design](#design) explains the choices):
- **The trait** (`control.rs`): `Control` with `hello`, `call` and `events`, and the
  `BoxFuture`, `EventStream` and `MaybeSendSync` types that keep it dyn-compatible.
- **Errors** (`error.rs`): `ControlError`, a `thiserror` enum: the engine's `WireError`,
  or a lost connection.
- **Mock** (`mock.rs`, feature `mock`): `MockEngine` and `MockControl`, an in-memory
  implementation for developing and testing clients without the engine, which
  they never depend on. `jstream-core`'s `InProcessControl` is the real in-process
  implementation; its tests hold it to the mock's answers.
- **WebSocket client** (feature `ws`): `WsControl`, the `Control` trait over the
  engine's WebSocket API, natively (`ws.rs`) and in the browser (`ws_web.rs`, `wasm32`);
  `WsConfig` and the handshake pieces both share (`ws_config.rs`).
- **Shared names** (`lib.rs`, no feature): `WS_PATH`, the API's path, and
  `WS_PROTOCOL` and `WS_TOKEN_PROTOCOL_PREFIX`, the subprotocols a browser offers;
  `jstream-server` uses the same constants, so client and server can't drift.
- `jstream_control::protocol` re-exports `jstream-protocol`.

<a id="design"></a>
## Design
<!-- @section
covers = ["crates/jstream-control/src/**", "crates/jstream-control/Cargo.toml"]
depends_on = ["docs/ARCHITECTURE.md#protocol", "docs/adr/0006-mcp-full-access.md", "docs/adr/0013-command-model.md"]
verified = { hash = "b3:5bfe78ede6aafeb9", by = "agent:claude@session_016BkTSqJ3kAfTw3EVqkJnMr", note = "ADR-0051 accepted (Jeff, #367): re-read; links re-pointed from superseded ADR-0041 to ADR-0051, which restates its decision with the API optional; text holds" }
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
`dev-dependencies`) keeps scenes, items, sources, outputs, encoder profiles, buses and
sends in memory, with an
in-memory stand-in for the stream-key store, and follows the protocol's rules for them:
- the version check in `hello`;
- atomic batches that bump `rev` by exactly one, and per-client idempotency keys,
  shared with history calls. A retry with a known key gets its first result; then, in `Graph::apply`'s order, a stale
  `expected_rev` fails with `rev_conflict` before an empty batch fails with
  `empty_batch`;
- every protocol edit, with the checks `jstream-graph` makes, in the same order per
  edit, and the same error codes and `edits.<n>.<field>` paths:
  - `not_found` and `already_exists`;
  - empty IDs in creating edits (`scene.create`'s `scene_id`, `item.add`'s `item_id`,
    `source.create`'s `source_id`, `source.duplicate`'s `new_source_id`), empty or blank
    names, and empty builtin kind names;
  - reorder and `item.move` indices out of range (`item.move` into another scene may
    name one past its top);
  - a placement edit's `scene_id` naming a scene other than the item's own;
  - both `transform` and `geometry` on `item.add`, a geometry that is not positive,
    not finite or not neutral in 3D, and a canvas side of 0 or over 8192;
  - deleting a source that an item places, or a scene that a scene source shows, or
    either while a source's settings refer to it;
  - nesting cycles from `item.add`, `item.set_source` and `item.move`;
  - a `source.create` whose `duplicated_from` names the source itself. Like the graph,
    the mock stores `duplicated_from` as a plain link (`source.duplicate` sets it) and
    reports it only while the source it names exists;
  - RTMP server URLs in `output.create` and `output.set_target` (`jstream-graph`'s
    checks and reasons, which never repeat the URL), and deleting an output while it
    runs, which the engine refuses through `Graph::apply_with_running`;
  - recording paths in `output.create` (`jstream-graph`'s platform-independent rule and
    reasons), `output.set_target` on a recording or virtual-camera output (the mock
    accepts a `virtual_cam` output, as the engine does), and an `unknown` output kind,
    RTMP target or container;
  - encoder profiles: `EncoderProfile::problem`'s ranges in `encoder.create` and
    `encoder.update`, deleting a profile an output uses, and `output.set_encoder`
    naming a missing output, then a missing profile;
  - audio (`mock/audio.rs`, ADR-0028): each audio source's `SourceAudio` with its kind's
    defaults and the master send at 0 dB, copied by `source.duplicate` and dropped by
    `source.delete`; `source.set_audio` on a source with no audio, with nothing to set,
    or setting a `scene` source's fader or mute; the bus ID, name and 16-bus limit,
    deleting the master, an empty `bus.set_fader`, sends from a `scene` source or a
    source with no audio, removing a missing send, and gains outside −100 to +24 dB.

  Error messages may differ; clients match on codes and fields. The mock describes no
  kinds, so it accepts any non-empty builtin kind; `MockEngine::with_kinds` only tells
  it which kinds have audio, and whether video too, for their audio defaults (a kind it
  wasn't told about has no audio). It stores settings as sent without
  checking them or filling in defaults (a `null` in a `source.set_settings` patch
  removes the entry), and doesn't follow settings references when it looks for cycles.
  `kind.list`, `kind.describe` and `delete.plan` answer `internal`, and its refused
  deletes carry no `blockers`;
- every collection query, answered from that state with the results
  `jstream-graph`'s `Graph::query` gives for the same edits: `state.get`, `scene.list`,
  `source.list` (ordered by ID), `item.list` (bottom first; `not_found` for a missing
  scene), `output.list`, `encoder.list` and `bus.list` (ordered by ID), each at the
  current `rev`.
  So it keeps everything an edit sets: the canvas, source names, kinds and settings,
  each item's placement (its transform, or its geometry and the transform view of it),
  crop and visibility, each scene's role and each item's scene after an `item.move`,
  each output's kind, target and encoder profile, each profile, each source's audio,
  and the buses and sends. `video.snapshot` is not one of those reads: the mock has no
  compositor and answers it with `internal`, as it does for any protocol variant it
  does not implement;
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
- `audio.reset_loudness`, which succeeds and does nothing: the mock has no mixer, so it
  emits no `meters.tick` either (`MockEngine::emit` can inject one);
- per-client undo and redo (`history.undo`, `history.redo`,
  [ADR-0032](../../docs/adr/0032-live-edits-and-undo.md)). Each batch is a step built
  from the same inverse edits `jstream-graph` returns: a removed item comes back at its
  index with its placement in its own form (transform or geometry), crop and
  visibility, a deleted scene at its position with its role and items, `item.move`
  back to the item's previous scene and index, `scene.set_role` back to the previous
  role, a deleted source with its
  settings and `duplicated_from`, `item.set_source` back to the item's previous source,
  a deleted bus with its fader and sends, a deleted source's audio and sends, and each
  audio edit back to the values it replaced; a placement, crop or
  visibility edit undoes with the `scene_id` it was sent with, a deleted output comes
  back with its encoder profile, encoder profile edits undo to the profile as it was,
  and `canvas.set_size` undoes to the previous size. The checks run in the engine's order (the key, then `expected_rev`, then
  `history_empty`). The result carries `step_rev` and both depths, a new batch clears
  the client's redo stack, undoing the `output.create` of a running output fails with
  the graph's `invalid_argument` and keeps the step, the history is bounded
  (`MockEngine::with_history_limit`, 100 by default) and a session's history goes when
  its `MockControl` is dropped. Conflicts are **stricter than the engine's**: once
  another client has changed the collection since a step was last applied or undone,
  undoing or redoing it fails with `undo_conflict` naming the first such change, and
  the step is dropped, where the engine refuses only a change that touches what the
  step touches. So an undo that succeeds against the mock succeeds against the engine
  too, but not the other way round;
- `state.changed` events carrying the request ID (and `cause` for an undo or redo), and `output.start`/`output.stop`
  reported as `output.state_changed` events (`starting` → `active`, `stopping` →
  `stopped`). `MockEngine::emit` injects any other event;
- the program scene with the engine's rules
  ([ADR-0030](../../docs/adr/0030-studio-state-and-scene-switching.md)): `scene.switch`
  (`not_found` for a missing scene, no `rev`), `studio.get`, and `studio.changed` for a
  switch and after a batch, undo or redo that creates the first scene or deletes the
  program scene;
- event streams that end, after what they have buffered, when their `MockControl` or
  the engine is dropped, the way a socket's streams end when it closes. An ended stream
  stays ended: later events, including those other sessions cause, never reach it.

Its futures are always ready. It has no persistence, credential store or real
outputs, and answers a protocol variant it doesn't know with `internal`.

The mock re-implements these checks and reads rather than calling
`jstream_graph::Graph::apply` and `Graph::query`. `jstream-graph` is also layer 2, and
[R-ARCH-002](../../docs/RULES.md#R-ARCH-002) allows only strictly lower layers, even
for dev-dependencies, so the dependency would need a layering change. The duplication
is small, and both crates' tests run the shared fixture `jstream_protocol::parity` (a
dev-dependency with the `fixtures` feature): one batch that builds a collection with
its outputs, the exact answer to every read of it, and the exact error for batches
rejected against it, RTMP URL, recording path and audio rejections included. The mock's
undo test tears that collection down in one batch and checks that undo gives back
exactly the fixture's state, which `jstream-graph`'s inverse property test guarantees
for the graph.
If the mock and the graph drift apart on any of those, one of the two tests fails. The
mock's own tests pin the remaining shared error paths.

### WebSocket client

`WsControl` (feature `ws`, off by default) implements `Control` over the engine's
WebSocket API, served by `jstream-server`. The feature's dependencies are declared per
target, so a `wasm32` build never pulls tokio, even with `ws` on, and a native one never
pulls `web-sys`.

- **Transport:** `tokio-tungstenite` on native targets, with no TLS feature: plain
  `ws://` only for now, so no `rustls` or `ring` is in the tree. TLS (rustls with the
  ring provider, [R-DEP-006](../../docs/RULES.md#R-DEP-006)) is an open question below.
  On `wasm32` it is the browser's `WebSocket` through `web-sys`
  ([ADR-0045](../../docs/adr/0045-leptos-web-ui.md)), which takes `ws://` and `wss://`
  (the browser does TLS).
- **No runtime required of the caller.** Natively, `WsControl::connect` starts one
  thread per connection running a single-threaded tokio runtime. Every future and
  stream it returns is completed through tokio's channels, which work under any
  executor, so the CLI's thread-parking `block_on` and the MCP stdio loop drive it as
  they drive the mock. In the browser there is one thread: the socket's handlers run on
  the page's event loop and complete calls and feed streams through `futures-channel`,
  so the UI's executor (`wasm-bindgen-futures`) drives them.
- **Connection:** `ws://<host>:<port>`, with `/api/v1/ws` (`WS_PATH`) when the URL has
  no path, always with the control token
  ([ADR-0006](../../docs/adr/0006-mcp-full-access.md)). It goes in the upgrade request
  rather than the URL, so it stays out of logs: an `Authorization: Bearer` header from
  native clients, and from browsers, which can't set that header, two subprotocols in
  `Sec-WebSocket-Protocol`: `jstream` and `jstream.token.<token>`
  ([ADR-0045](../../docs/adr/0045-leptos-web-ui.md)). The server selects `jstream`; the
  browser client fails the connection if it selects anything else. With an empty token
  the browser client offers no token subprotocol: the browser's cached HTTP Basic
  credentials, whose password is the control token, carry it instead (when the engine
  served the page behind Basic). A token that isn't an
  HTTP token (RFC 7230 `tchar`s) can't be a subprotocol and fails `connect`; the
  daemon's tokens are hex. `jstream-server` accepts both
  ([jstream-server › Design](../jstream-server/README.md#design)). The caller supplies the token (the CLI
  reads it from a token file). The server validates `Host`, and `Origin` when present;
  browsers set `Origin` themselves (`WsConfig::origin` is ignored there), and the
  native client sends the server's own origin (`http://<host>:<port>` of the URL)
  unless `WsConfig::origin` names another.
- **Session:** `hello` is the caller's first call. Requests get the IDs `req_1`,
  `req_2`, … per session, and the server reports the `state.changed` event a call
  causes with that same ID. The connection thread routes each `response` or `error` to
  its waiting call by request ID (`welcome`, and an error without an ID, answer the
  oldest pending `hello`) and fans `event`s out to the subscriptions.
- **Failures are `Disconnected`.** An invalid URL (or a `wss://` one, natively), an
  unreachable engine, or a refused upgrade fails `connect`. Natively the reason names
  HTTP 401 for the token and 403 for `Origin` or `Host`; a browser doesn't tell the page
  why an upgrade failed, so there the reason covers all three. When the socket closes, pending and later calls fail with
  `Disconnected` carrying the reason (the server's close reason when it gave one), and
  every event stream ends after what it buffered. Dropping the `WsControl` closes the
  socket. Reconnecting is the caller's choice (a new session, then re-read the state),
  because a new connection may get a new `client_id`.

The native client's end-to-end tests live in `jstream-server`, which runs a
`MockEngine` behind a real server on a loopback port and drives it with this client.
The browser client has no automated test yet: it type-checks in CI's `wasm32` build,
and the URL and token handling it shares are unit-tested natively (`ws_config.rs`).

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
3. **Browser client.** Settled and built: `web-sys`'s `WebSocket` on `wasm32`, with the
   token in a subprotocol ([ADR-0045](../../docs/adr/0045-leptos-web-ui.md)). A
   browser test (wasm-bindgen-test against a server) is still open.
4. **TLS.** `wss://` needs a TLS feature of `tokio-tungstenite` (rustls with ring) and
   its wiring. `ring` is already on the lint-deps native-code allowlist, for
   `jstream-outputs`' RTMPS, so that part needs no further governance change.
5. **Timeouts.** `connect` and calls have no timeout of their own; a caller that needs
   one wraps the future. A connect timeout and keep-alive pings may belong here once
   remote (non-loopback) use exists.
