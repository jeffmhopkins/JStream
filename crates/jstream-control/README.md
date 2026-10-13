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
depends_on = ["docs/ARCHITECTURE.md#crate-map"]
verified = { hash = "b3:9b348d7a7c6872b9", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "New crate: purpose matches the code (Control trait with hello/call/events, BoxFuture/EventStream/MaybeSendSync in control.rs, ControlError in error.rs, mock behind the mock feature, protocol re-export) and the crate-map row (L2, summary identical, unsafe no); WS client stated as not here yet" }
-->

The `Control` trait: the one API through which every client talks to the engine
([ADR-0001](../../docs/adr/0001-library-first-engine.md)). The CLI, the MCP server, the
web UI and the HTTP server are written against `dyn Control` and never link the engine
([R-ARCH-003](../../docs/RULES.md#R-ARCH-003)); the host apps hand them an
implementation. Layer 2: it builds on `jstream-protocol` (layer 1), is WASM-safe, and
needs no async runtime.

Contents (a first cut; [Design](#design) explains the choices):
- **The trait** (`control.rs`): `Control` with `hello`, `call` and `events`, and the
  `BoxFuture`, `EventStream` and `MaybeSendSync` types that keep it dyn-compatible.
- **Errors** (`error.rs`): `ControlError`, a `thiserror` enum: the engine's `WireError`,
  or a lost connection.
- **Mock** (`mock.rs`, feature `mock`): `MockEngine` and `MockControl`, an in-memory
  implementation for developing and testing clients before the engine exists.
- `jstream_control::protocol` re-exports `jstream-protocol`.

Not here yet: the WebSocket client (see [Design](#design)).

<a id="design"></a>
## Design
<!-- @section
covers = ["crates/jstream-control/src/**", "crates/jstream-control/Cargo.toml"]
depends_on = ["docs/ARCHITECTURE.md#protocol", "docs/adr/0006-mcp-full-access.md", "docs/adr/0013-command-model.md"]
verified = { hash = "b3:d357dccb58785c94", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "New crate: trait semantics (crate-chosen request IDs, second hello keeps identity, handshake_required, per-call event subscriptions) match control.rs and mock.rs and its tests; Send/Sync cfg on wasm32 checked by clippy on both targets; mock behaviour list matches mock.rs; WS client deferral and token/Origin intent consistent with ARCH#protocol and ADR-0006; Cargo.toml has futures-core with default features off" }
-->

### The trait

A `Control` value is one client session. Its three methods mirror the protocol's
envelope ([jstream-protocol › Design](../jstream-protocol/README.md#design)):

- `hello(Hello) -> Welcome`: the handshake. It fixes the session's `client_id`, which
  keys its undo history and audit-log entries
  ([ADR-0013](../../docs/adr/0013-command-model.md)). A second `hello` returns the same
  identity.
- `call(Call) -> Response`: one batch, action or query. The implementation chooses the
  request ID and returns it in the `Response`, so the caller can match the
  `state.changed` event its batch causes. Before `hello` it fails with
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
`dev-dependencies`) keeps scenes, items and outputs in memory and follows the protocol's
rules for them: the version check in `hello`, atomic batches that bump `rev` by exactly
one, `expected_rev` conflicts, per-client idempotency keys, `not_found` and
`already_exists`, `state.changed` events carrying the request ID, and
`output.start`/`output.stop` reported as `output.state_changed` events (`starting` →
`active`, `stopping` → `stopped`). `MockEngine::emit` injects any other event. Its
futures are always ready. It has no undo, persistence or real outputs, and answers a
protocol variant it doesn't know with `internal`.

### WebSocket client (deferred)

The remote implementation comes in a later PR, when `jstream-server` exists to talk to.
The intended design:

- **Transport:** `tokio-tungstenite` (rustls with the ring provider,
  [R-DEP-006](../../docs/RULES.md#R-DEP-006)) on native targets, and the browser
  `WebSocket` through `gloo-net` on `wasm32`, selected by `cfg(target_arch)`. Neither is a
  dependency until then.
- **Connection:** `ws://127.0.0.1:<port>/api/v1/ws`, always with the control token
  ([ADR-0006](../../docs/adr/0006-mcp-full-access.md)). It goes in the upgrade request
  rather than the URL, so it stays out of logs: an `Authorization` header from native
  clients, and the `Sec-WebSocket-Protocol` header from browsers, which can't set
  others. Native clients read the token from the user-only token file. The server
  validates `Origin` and `Host`; browsers set `Origin` themselves, and the native client
  sends a fixed one.
- **Session:** `hello` is the first message; a reader task routes each `response` or
  `error` to its waiting call by request ID and fans `event`s out to the subscriptions.
  When the socket closes, pending calls fail with `Disconnected` and event streams end.
  Reconnecting is the caller's choice (a new session, then re-read the state), because
  a new connection may get a new `client_id`.

### Open questions

1. **Backpressure.** Event buffers are unbounded. A slow subscriber of high-rate events
   (`stats.tick`, meters) would grow without limit. Bounded buffers that drop the oldest
   events (a gap in `rev` already shows a missed `state.changed`) are likely once such
   events exist ([protocol open question 7](../jstream-protocol/README.md#open-questions)).
2. **Request IDs.** The implementation assigns them. A caller that wants its own
   (correlating with an external log) would need `call` to take a `Request`.
