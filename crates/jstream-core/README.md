<!-- @doc
id = "CRATE-jstream-core"
title = "jstream-core"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-core

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-core/src/**", "crates/jstream-core/tests/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-core"]
verified = { hash = "b3:f5af83854ebfd993", by = "agent:grok@bc-49a8266d-f517-52bc-954b-ecc4a965f828", note = "Purpose now names snapshot.rs, set_source_frame, the second compositor lock, and the lavapipe test. Capture, audio, encode and real outputs stay in Not yet. The crate-map row is unchanged." }
-->

The engine ([ADR-0001](../../docs/adr/0001-library-first-engine.md)): `Engine` owns the
collection's scene graph and executes every protocol call against it, and
`InProcessControl` serves it through the `Control` trait in the same process. Layer 5,
the top library layer ([ARCH › Crate map](../../docs/ARCHITECTURE.md#crate-map)): only
the host apps may depend on it, and the clients (`jstream-server`, `jstream-mcp`,
`jstream-cli`, `jstream-ui`) never do ([R-ARCH-003](../../docs/RULES.md#R-ARCH-003)); a
host hands them an `InProcessControl` as a `dyn Control`.

This first cut is the **control plane only** (the minimal `jstream-core` of
[ROADMAP › M1](../../ROADMAP.md#m1)). Contents:
- **Engine** (`engine.rs`): `Engine`, a cloneable handle to one engine, built with
  `Engine::new()` and `with_output(id)`; `connect()` opens a session; `rev()`;
  `set_source_frame` stores the latest RGBA8 frame of a source. It holds the
  `jstream-graph` `Graph`, the outputs, those frames, the idempotency keys and the
  subscribers behind one lock, and the snapshot compositor behind another, and
  executes batches, actions and queries.
- **Snapshot** (`snapshot.rs`): `video.snapshot`, the program mix read back as a PNG.
- **Session** (`session.rs`): `InProcessControl`, one client session.
- **Events** (`events.rs`): the per-subscription queues behind `Control::events`.
- **Outputs** (`outputs.rs`): the outputs' runtime state, a placeholder state machine.
- **Tests** (`tests.rs`, `tests/mcp.rs`): the protocol rules through
  `InProcessControl`, the shared parity fixture, a differential run against
  `jstream-control`'s mock, many sessions on many threads, a headless
  `video.snapshot` on the software adapter, and an MCP agent
  (`jstream-mcp`, a dev-dependency) driving the engine in-process.

Not here yet: capture, audio, encode and real outputs, threads and the media
clock (`src/runtime/`, [ARCH › Threads](../../docs/ARCHITECTURE.md#threading)),
platform backends, per-client undo, the audit log, persistence and settings. The
snapshot compositor is the one piece of render this cut calls. See
[Design › Not yet](#design).

Why a crate: it is the crate map's layer-5 engine, the one crate allowed to depend on
every subsystem, and it must sit above `jstream-server` and `jstream-mcp` so that none
of the clients can reach it.

<a id="design"></a>
## Design
<!-- @section
covers = ["crates/jstream-core/src/**", "crates/jstream-core/Cargo.toml"]
depends_on = ["docs/ARCHITECTURE.md#protocol", "docs/adr/0013-command-model.md", "docs/RULES.md#R-API-004", "crates/jstream-protocol/README.md#design", "crates/jstream-control/README.md#design"]
verified = { hash = "b3:b764f2346d968319", by = "agent:grok@bc-49a8266d-f517-52bc-954b-ecc4a965f828", note = "Design now describes the snapshot: first scene, 1920x1080, opacity 1, nearest cap at 1280, collection lock released before the GPU wait. Dependencies include jstream-render and png. The differential script still omits video.snapshot." }
-->

### Engine and sessions

`Engine` is a handle around one `Arc` of shared state: the collection mutex, and a
separate mutex for the snapshot compositor. Cloning it shares the engine, so a
host can give one clone to the server's connector and another to an MCP session.
`Engine::connect()` opens an `InProcessControl`, which holds a clone too: the engine
lives as long as any handle or session does.

A session follows the protocol's envelope
([jstream-protocol › Design](../jstream-protocol/README.md#design)) exactly as the
WebSocket API does, minus serialization:
- `hello` checks the version (`unsupported_version`; a host may hand the session to a
  client built against another protocol version) and assigns a `client_id`
  (`client_1`, `client_2`, …, unique per engine). A second `hello` returns the same
  identity with the current `rev`. A refused `hello` opens nothing.
- Every call before `hello` fails with `handshake_required`. `events()` does not need
  the handshake, as with the mock; `jstream-server` subscribes only after `welcome`.
- Request IDs are `req_1`, `req_2`, … per session, as `WsControl` numbers them, and
  come back in the `Response` and in the `state.changed` event a batch causes.
- Calls run to completion inside `call`, so every future is already complete: any
  executor, or a plain poll loop, drives the session, and it needs no async runtime. It
  never fails with `Disconnected`.

### Dependencies

`jstream-protocol` (the messages), `jstream-control` (the `Control` trait),
`jstream-graph` (the collection and every edit rule), `jstream-media` (the canvas
`FrameSize`), `jstream-render` (the snapshot compositor), `png` (encodes that
readback; default features off, deflate stays pure Rust, allowed by
[ADR-0031](../../docs/adr/0031-source-types-and-decoding-scope.md)), `thiserror`
(`SourceFrameError`), `futures-core` (the `Stream` trait of event streams) and
`tracing` (see *Logging*). No async runtime, and no `jstream-platform` until a backend is
injected. Dev-dependencies: `jstream-control`'s `mock` feature and `jstream-protocol`'s
`fixtures` feature (the differential and parity tests), `jstream-mcp` and `serde_json`
(`tests/mcp.rs`). All of them are in lower layers
([R-ARCH-002](../../docs/RULES.md#R-ARCH-002)), and none of the clients depends on this
crate.

### Calls

Batches, actions and the collection queries execute under the collection lock, so
calls from different sessions are serialized, and their events are queued in the
order the changes happened. `video.snapshot` copies what it needs under that lock
and composites after releasing it (see *Snapshots*).

- **Batches** go to `jstream_graph::Graph::apply_batch`, which owns every edit rule:
  `rev_conflict` for a stale `expected_rev`, checked before `empty_batch`; then the
  edits in order on a working copy, the first failure aborting with `not_found`,
  `already_exists` or `invalid_argument` (`edits.<n>.<field>`) and nothing applied; on
  success `rev` goes up by exactly one. The engine adds the per-client **idempotency
  keys** ([R-API-004](../../docs/RULES.md#R-API-004)): a batch whose key this client
  already used is not applied again and gets the `rev` it produced, even after `rev`
  moved on (the key check comes first) and without an event. Only applied batches
  are remembered, so a retry of a failed batch is applied. Keys are kept for the
  engine's lifetime ([protocol open question 6](../jstream-protocol/README.md#open-questions)).
- **Queries** that read the collection go to `Graph::query`; `output.status` reads the
  outputs (`not_found` for an unknown one). `video.snapshot` is answered here, not by
  the graph.
- **Actions**: `output.start` and `output.stop` (see *Outputs* below).
- A call, query or action this engine does not implement answers `internal`, as the
  mock does.

The engine's answers are the mock's for the differential script: `tests.rs` runs one
script of calls against both and compares results, error codes and fields, and events.
That script does not call `video.snapshot`. The mock has no compositor and answers it
with `internal`, so the two are not compared. The mock stays the reference clients
were developed against, and `jstream-graph` the only implementation of the edit rules
here.

### Snapshots

`video.snapshot` returns the program mix as a PNG
([jstream-protocol › Design](../jstream-protocol/README.md#design)). Until studio
state exists, that scene is the first one in display order. `Program` will follow
`program_scene` once studio mode is built
([ARCH › Data model](../../docs/ARCHITECTURE.md#data-model)); that state is M2
([ROADMAP › M2](../../ROADMAP.md#m2)). An empty collection is an opaque black canvas.

The canvas is [ADR-0022](../../docs/adr/0022-item-transform-and-canvas.md)'s default,
1920×1080. `canvas.set_size` is not a message yet, so every snapshot uses that size.
A nested `scene` source is composited at the canvas size and then placed. Hidden
items are skipped. A source with no stored frame is skipped, not drawn as a
placeholder. Opacity is 1 and the only blend is src-over: the graph does not store
either yet, and the compositor's blend is src-over. The graph already rejects a
cycle at `item.add`; a cycle in the copied shot paints black for the repeated scene.

`Engine::set_source_frame` stores the latest straight sRGB RGBA8 frame for a source
id. It changes no `rev` and emits no event, and a frame may arrive before the source
exists. Width or height of zero, a buffer whose length is not `width * height * 4`,
or a size that does not fit in memory is `SourceFrameError`.

The compositor opens on the first snapshot, software adapter first
(`OpenOptions::headless`: lavapipe or WARP). No adapter is `internal`, and the reason
starts with `no wgpu adapter`. The readback is scaled so its longest side is at most
1280 ([ARCH › MCP](../../docs/ARCHITECTURE.md#mcp)), nearest-neighbour. Equal sizes
are not resampled. The PNG is standard base64 with padding and no line breaks.

The collection lock is not held across the GPU wait
([R-RT-002](../../docs/RULES.md#R-RT-002)). The compositor has its own mutex, taken
only after the collection lock is released. This query is control-plane work, not
the render thread.

### Events

Each `events()` call opens an independent subscription with an unbounded queue, so no
event is dropped ([jstream-control open question 1](../jstream-control/README.md#design)
covers backpressure). `jstream-server` drains its connections' subscriptions as events
arrive, however slowly a client reads, and bounds what it holds per connection itself
([jstream-server › Design](../jstream-server/README.md#design)). A batch emits
`state.changed {rev, client_id, request_id}`, and an action emits one
`output.state_changed` per step. Every subscription of every
session receives every event emitted after it opened, in the engine's order;
`state.changed` events therefore arrive in `rev` order with no gap. Subscribers are
woken after the engine's lock is released. A stream ends, after what it buffered, when
its session is dropped or when the engine goes away (its last handle and session);
an ended stream stays ended.

### Outputs (placeholder)

Outputs are runtime state, not collection state: they never change `rev`. Until the
protocol has edits that configure outputs, the host declares them with
`Engine::with_output(id)`; each starts `stopped`.

There is no media pipeline yet, so the actions only move a state machine, as
`jstream-control`'s mock does. The protocol defines an action's response as "accepted"
and reports progress as events, so that is what the engine does:
- `output.start` on a `stopped` or `failed` output emits `starting` then `active` and
  answers `{"kind":"action"}`; `output.stop` on any other state emits `stopping` then
  `stopped`.
- Starting a running output, or stopping one that isn't, fails with `action_failed`
  (an accepted action that can't be carried out); an unknown output with `not_found`.

Rejecting `output.start` with `action_failed` until media exists was the alternative.
It was not chosen because clients (the CLI, MCP agents, the server's tests) already
drive the start/stop/status flow against the mock, and this keeps them working on the
real engine. When `jstream-outputs` lands, `active` and `failed` will come from the real
output, asynchronously, and `reconnecting` will appear.

### Locking

`std::sync::Mutex` locks, always taken in one order: a session's identity lock (held
by `hello` while it registers the client), then the engine's collection lock, then a
subscription queue's. The snapshot compositor's lock is separate and is taken only
when the collection lock is not held. A stream's poll takes only its queue's lock.
No lock is held across an `await` (there are none). The GPU wait of `video.snapshot`
holds only the compositor lock. A poisoned lock is recovered rather than propagated:
`Graph::apply` changes a working copy and replaces the state only on success, and the
engine's other changes are single inserts or assignments, so the data stays
consistent. Nothing is logged while a lock is held.

### Logging

Spans and events go through `tracing` ([R-RS-005](../../docs/RULES.md#R-RS-005)); the
crate installs no subscriber, the host app does. Each session has an `info` span
`session`, which records `client_id` once `hello` succeeds; its `hello`, its calls and
its drop are entered in it, and a session created inside another span (the server's
`connection`) nests under that one. Each call runs in an `info` span `command` with the
command's name (`batch`, or the action's or query's name), `client_id` and the session's
`request_id`. The outcome is logged after the engine's lock is released: `info` for an
output action (with `output_id`, and the error code when it fails), `debug` for
everything else (the new `rev`, or the error code). A refused `hello`, with the client's
name and protocol version, and a call before `hello` are `debug`. Only names, IDs,
revisions and error codes are logged, never a call's arguments
([R-SEC-001](../../docs/RULES.md#R-SEC-001)).

### Not yet

- **Per-client undo** ([ADR-0013](../../docs/adr/0013-command-model.md)). The protocol
  has no undo or redo request yet, so the engine keeps no history and drops the inverse
  batch `Graph::apply` returns. The history, `history.undo` and `history.redo`, and
  `undo_conflict` come together, as
  [ADR-0032](../../docs/adr/0032-live-edits-and-undo.md) decides.
- **Live values** for continuous gestures (`live.set`, `live.cancel` and their events,
  [ADR-0032](../../docs/adr/0032-live-edits-and-undo.md)), which need the real-time
  parameter channels.
- The audit log, persistence (the collection file) and settings.
- Threads, the media clock and real-time hand-off
  ([ARCH › Threads](../../docs/ARCHITECTURE.md#threading)), and the subsystems that
  feed and consume frames: capture, the render thread, audio, encode and real
  outputs. `video.snapshot` composites from frames the host injects with
  `set_source_frame`.
- `R-SEC-001`'s `secrets_are_redacted` test: the engine holds no secret yet.
