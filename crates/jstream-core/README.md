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
verified = { hash = "b3:776be5d7a3dab550", by = "agent:claude@session_01SS5BBdawiuaRV6Pk4PbfKw", note = "Merge of main (B2) into B3: re-read; History bullet (B2) and Snapshot/canvas text (B3) both hold; history.rs now also handles item.set_geometry and canvas.set_size" }
-->

The engine ([ADR-0001](../../docs/adr/0001-library-first-engine.md)): `Engine` owns the
collection's scene graph and executes every protocol call against it, and
`InProcessControl` serves it through the `Control` trait in the same process. Layer 5,
the top library layer ([ARCH › Crate map](../../docs/ARCHITECTURE.md#crate-map)): only
the host apps may depend on it, and the clients (`jstream-server`, `jstream-mcp`,
`jstream-cli`, `jstream-ui`) never do ([R-ARCH-003](../../docs/RULES.md#R-ARCH-003)); a
host hands them an `InProcessControl` as a `dyn Control`.

The control plane is the scene graph, the sessions and `video.snapshot`.
`output.start` runs the media pipeline when the host calls
`with_backends` and gives it a publish target (`with_publish` or
`with_publish_opener`); a recording output needs no publish target. Without backends, an output action stays the placeholder
state machine. Contents:
- **Engine** (`engine.rs`): `Engine`, a cloneable handle to one engine, built with
  `Engine::new()`, `with_output(id)`, `with_backends`, `with_publish`,
  `with_publish_opener`, `with_key_store`, `with_audit` and `with_history_limit`;
  `connect()` opens a session; `rev()`; `set_source_frame` stores the latest RGBA8
  frame of a source; `output_stats` reads the running pipeline; `output_kind` reads a
  collection output's target. `output.set_key` and `output.clear_key` go to the
  injected `StreamKeyStore`, never under the collection lock. It holds the
  `jstream-graph` `Graph`, the outputs, those frames, the idempotency keys, the undo
  history and the subscribers behind one lock, and the drop ledger behind another, and executes
  batches, history calls, actions and queries. The pipeline threads do not take the collection lock.
- **Audit log** (`audit.rs`): one `AuditEntry` per executed batch, undo, redo and action, sent to
  the `AuditSink` the host injects with `with_audit` (none by default). `AuditMemory`
  keeps entries in memory; `AuditFile` writes them as JSON lines to a size-capped
  `audit.jsonl` from its own writer thread (see *Audit log* in [Design](#design)).
- **Kinds** (`kinds.rs`): the built-in source kinds the graph validates settings
  against and `kind.list` reports ([ADR-0025](../../docs/adr/0025-typed-settings-kind-descriptors.md)).
  They are stubs: `color`, `image`, `media` and `text` take a first set of parameters,
  and `display`, `webcam` and `window` none. Nothing renders from them yet; frames
  still come from `set_source_frame`. `jstream-sources` replaces them.
- **Snapshot** (`snapshot.rs`): `video.snapshot`, the program mix read back as a PNG
  on the engine's own snapshot thread, which owns the snapshot compositor.
- **Runtime** (`runtime/`): the `output.start` pipeline. The render thread reads the
  platform capture mailbox, composites one scene and reads the frame back. The mixer
  thread polls the media clock. Encode threads live in the encoder sessions. The
  output thread muxes FLV and publishes it
  ([ARCH › Threads](../../docs/ARCHITECTURE.md#threading)).
- **History** (`history.rs`): per-client undo and redo
  ([ADR-0032](../../docs/adr/0032-live-edits-and-undo.md)): each client's undo and redo
  stacks, built from the inverse batches `Graph::apply` returns, and the log of what
  each recent change touched, which `undo_conflict` is checked against.
- **Session** (`session.rs`): `InProcessControl`, one client session. Its undo history
  ends with it.
- **Events** (`events.rs`): the per-subscription queues behind `Control::events`.
- **Outputs** (`outputs.rs`): the outputs' runtime state, following the collection's
  outputs after each batch and keeping the ones the host declared. The placeholder path steps
  `starting` to `active` at once. The pipeline path emits `starting`, then `active`
  or `failed`. Stop emits `stopping` then `stopped`. A stop during `starting` cancels
  that start and settles `stopped` only after its threads are gone. A lost RTMP
  connection moves `active` to `reconnecting`, and back once it is re-established. A
  publish, encoder, render or mixer failure after `active`, or giving up
  reconnecting, settles `failed`.
- **Drop counts** (`drops.rs`): the sources a `stats.tick` sums. `output.start`
  registers the capture cell, both encode sessions, both mixer inputs, the mix
  bus, and the packets the output thread drops (muxer rejects and what a stop's
  drain abandons) and the publish link's, and a sampler thread
  emits a tick about once a second until the pipeline stops.
- **Tests** (`tests.rs`, `src/tests/history.rs`, `tests/mcp.rs`, `tests/scenarios.rs`):
  the protocol rules through `InProcessControl`, undo and redo across two clients
  (conflicts, `expected_rev`, idempotency, the limit, a running output, a session's
  end), the shared parity fixture, a differential run against
  `jstream-control`'s mock, many sessions on many threads, a headless
  `video.snapshot` on the software adapter, nested-scene scheduling (order, depth
  bound, cycles) and its pixels through two levels in `snapshot.rs`, the pipeline against the platform fakes
  (on the software adapter, and again on a CPU stand-in for the compositor so the
  stage-failure, late-tick, teardown, final-tick, link-loss and stalled-stop tests run
  without one),
  `connect_rtmp` against a slow fake RTMP server on loopback, one that hangs up
  and is reconnected to, and one that stops reading while a stop's deadline runs out,
  a publish opener connecting that fake server on every start (start, media, stop,
  twice) and one that cannot connect, a recording output writing MP4 to a temp dir
  (renamed into place on stop, refused when the file exists or the path is not
  absolute on this machine, no `.part` file left by a failed start), overload drops from a
  fake encoder and the mixer reported in `stats.tick`, render and mixer panics
  reported as `failed` on the CPU stand-in (`tests/scenarios.rs` reaches it through
  the hidden `Engine::with_cpu_compositor`), the output edits and the stream-key
  actions against `jstream-platform`'s `MemoryKeyStore` (with `secrets_are_redacted`,
  which captures the engine's traces and audit entries), the audit entries of a run of
  batches and actions (`batches_and_actions_are_audited`) and `AuditFile`'s lines and
  rotation (`audit.rs`), and of undo and redo (`undo_and_redo_are_audited`), and an MCP agent (`jstream-mcp`, a
  dev-dependency) driving the engine in-process.

Not here yet: live values, persistence, real source kinds, arc-swap
snapshots, a GPU encode target, and the platform's Windows devices. See
[Design › Not yet](#design).

Why a crate: it is the crate map's layer-5 engine, the one crate allowed to depend on
every subsystem, and it must sit above `jstream-server` and `jstream-mcp` so that none
of the clients can reach it.

<a id="design"></a>
## Design
<!-- @section
covers = ["crates/jstream-core/src/**", "crates/jstream-core/Cargo.toml"]
depends_on = ["docs/ARCHITECTURE.md#protocol", "docs/adr/0013-command-model.md", "docs/RULES.md#R-API-004", "crates/jstream-protocol/README.md#design", "crates/jstream-control/README.md#design"]
verified = { hash = "b3:fa86bd19ba047798", by = "agent:claude@session_01WG1uLVazjTqn2gmsarSsqg", note = "Merge of main (#183) into B2-f3 (control mock changed): re-read Undo and redo and the differential-script paragraph; nothing compares history calls; B3's canvas conflict rule unaffected" }
-->

### Engine and sessions

`Engine` is a handle around one `Arc` of shared state: the collection mutex, and
the queue to the snapshot thread. The drop ledger is a second mutex, outside that
`Arc`. Cloning it shares the engine, so a
host can give one clone to the server's connector and another to an MCP session.
`Engine::connect()` opens an `InProcessControl`, which holds a clone too: the engine
lives as long as any handle or session does.

A session follows the protocol's envelope
([jstream-protocol › Design](../jstream-protocol/README.md#design)) exactly as the
WebSocket API does, minus serialization:
- `hello` checks the version (`unsupported_version`; a host may hand the session to a
  client built against another protocol version) and assigns a `client_id`
  (`client_1`, `client_2`, …, unique per engine). A second `hello` returns the same
  identity with the current `rev`, and its `client_name` replaces the first in the audit
  log. A refused `hello` opens nothing.
- Every call before `hello` fails with `handshake_required`. `events()` does not need
  the handshake, as with the mock; `jstream-server` subscribes only after `welcome`.
- Request IDs are `req_1`, `req_2`, … per session, as `WsControl` numbers them, and
  come back in the `Response` and in the `state.changed` event a batch causes.
- Calls run to completion inside `call`, so their futures are already complete,
  except `video.snapshot`'s: it completes, and wakes its waker, when the snapshot
  thread has drawn the frame (see *Snapshots*). Any executor drives the session, or
  a poll loop that parks until woken, and it needs no async runtime. It never fails
  with `Disconnected`. `output.start` and `output.stop` still block inside `call`
  while devices open and threads join.

### Dependencies

`jstream-protocol` (the messages), `jstream-control` (the `Control` trait),
`jstream-graph` (the collection and every edit rule), `jstream-media` (frames, the
canvas `FrameSize` and the media clock's time base), `jstream-render` (the snapshot
and the pipeline compositor), `jstream-platform` (backend traits the host injects, and
the capture counters a `stats.tick` reads), `jstream-audio` (the mixer and its drop
counters), `jstream-encode` (the H.264 and AAC sessions and their drop counters),
`jstream-mux` (FLV and fragmented MP4), `jstream-outputs` (the reconnecting RTMP
publisher and the recording file sink), `png`
(encodes the snapshot readback; default features off, deflate stays pure Rust, allowed by
[ADR-0031](../../docs/adr/0031-source-types-and-decoding-scope.md)), `serde_json`
(settings values in the built-in kind descriptors), `thiserror`
(`SourceFrameError`), `futures-core` (the `Stream` trait of event streams) and
`tracing` (see *Logging*). No async runtime: `output.start` blocks the call until
the pipeline is up or has failed, and `video.snapshot` runs on a plain thread. Dev-dependencies: `jstream-control`'s `mock`
feature and `jstream-protocol`'s `fixtures` feature (the differential and parity
tests), `jstream-platform`'s `fake` feature (the pipeline test and
`tests/scenarios.rs`) and `jstream-mcp` (`tests/mcp.rs`). All of them
are in lower layers
([R-ARCH-002](../../docs/RULES.md#R-ARCH-002)), and none of the clients depends on this
crate.

### Calls

Batches and the collection queries run under the collection lock, so those calls
are serialized across sessions and their events are queued in the order the changes
happened. Placeholder output actions do too: they only step the state machine.
`video.snapshot` copies what it needs under that lock and hands it to the snapshot
thread after releasing it (see *Snapshots*).

A pipeline `output.start` or `output.stop` holds the collection lock only while it
changes that output's state and queues events. Opening devices, waiting for the
compositor and joining threads happen after the lock is released, so another
session can run during that work. The two calls still cannot leave a live pipeline
behind a successful stop. The start records an in-flight generation before it
unlocks, and it settles `active` only while that generation is the one in flight.
`output.stop` in that window cancels the generation, waits until the start has
dropped any threads it opened, then settles `stopped`. The start answers
`action_failed` and does not settle `active`. Once the pipeline is `active`, stop
takes it, joins outside the lock and settles `stopped`. See *Outputs* and *Locking*.

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
  Each applied batch becomes one step of its client's undo history (see *Undo and
  redo*).
- **History calls** (`history.undo`, `history.redo`) apply a step's edits through the
  same `Graph::apply_with_running` (see *Undo and redo*).
- **Queries** that read the collection, and the kind queries, go to `Graph::query`,
  `output.list` and the `outputs` of `state.get` included; the graph holds the built-in
  kinds the engine gave it (`kinds.rs`). `output.status` reads the outputs' runtime state
  (`not_found` for an unknown one) and fills `key_set` from the key store after the
  collection lock is released (`internal` if the store fails). `video.snapshot` is answered here, not by
  the graph.
- **Actions**: `output.start` and `output.stop` (see *Outputs* below), and
  `output.set_key` and `output.clear_key` (see *Stream keys* below).
- A call, query or action this engine does not implement answers `internal`, as the
  mock does.

The engine's answers are the mock's for the differential script: `tests.rs` runs one
script of calls against both and compares results, error codes and fields, and events.
That script does not call `video.snapshot`. The mock has no compositor and answers it
with `internal`, so the two are not compared. The mock stays the reference clients
were developed against, and `jstream-graph` the only implementation of the edit rules
here.

### Undo and redo

Per client ([ADR-0013](../../docs/adr/0013-command-model.md),
[ADR-0032](../../docs/adr/0032-live-edits-and-undo.md)), in `history.rs`, under the
collection lock:
- **Stacks.** Each `client_id` has an undo stack and a redo stack. An applied batch
  pushes one step holding the inverse batch the graph returned, and clears the redo
  stack. An undo applies that inverse as one batch (`rev` + 1, `state.changed` with
  `cause: undo`) and moves the step to the redo stack, now holding the inverse the
  graph returned for the undo, which is the step again; a redo does the reverse. A
  step keeps the `rev` its batch first produced (`step_rev`). The undo stack keeps the
  newest 100 steps by default (`with_history_limit`; 0 keeps none). A session's
  history is dropped when it closes, since a reconnecting client gets a new
  `client_id`.
- **Order of checks.** A used idempotency key returns its first result (keys are
  shared with batches and store the whole result); then `expected_rev`
  (`rev_conflict`); then an empty stack (`history_empty`). None of these drops a step.
- **Conflicts.** Each applied batch, undo and redo is logged with what it *touched*,
  computed in core from its edits and the inverse's (so a delete also names what it
  removed): objects created or deleted, objects used (an item's scene and source, a
  `scene` source's scene, a duplicate's original), properties set (names, an item's
  placement, whether set as a transform or a geometry, crop, visibility, an item's
  index, each settings parameter, an output's target), the scenes' order, and the
  canvas size. An undo or redo first applies the step's edits to a copy of the
  graph, so the step's touched set includes what they touch *now*: undoing
  `scene.create` also deletes the items the scene holds now, even ones whose own step
  was dropped. The step conflicts with another client's later entry, logged after
  the step was last applied or undone, when either creates or deletes an object the
  other touches, when both set the same property, when both reorder scenes, or when
  both set the canvas size. Two
  items added to one scene do not conflict. A conflict fails with `undo_conflict`
  naming the first such entry, nothing applied, and the step dropped. Edits that
  pass the check but no longer apply also fail with `undo_conflict`, naming the latest
  entry by another client, and drop the step. Edits that fail only because an output
  is running (undoing `output.create` while it runs) answer the graph's own
  `invalid_argument` and keep the step for a retry once it is stopped; with no other
  client's entry to blame, any other failure does too. The log is pruned to the oldest
  `since` of any kept step. Only when there is no conflict does the copy replace the
  graph.
- **Not steps:** actions (outputs, stream keys), queries and history calls. Live
  values are not built.

### Snapshots

`video.snapshot` returns the program mix as a PNG
([jstream-protocol › Design](../jstream-protocol/README.md#design)). Until studio
state exists, that scene is the first one in display order. `Program` will follow
`program_scene` once studio mode is built
([ARCH › Data model](../../docs/ARCHITECTURE.md#data-model)); that state is M2
([ROADMAP › M2](../../ROADMAP.md#m2)). An empty collection is an opaque black canvas.

The canvas is the collection's (`State::canvas`, default 1920×1080, changed by
`canvas.set_size`), copied into the shot with the items, so every scene of a snapshot,
nested ones included, is composited at that size; the PNG is that canvas scaled to fit
1280. Each item is drawn in its stored placement form
([ADR-0022](../../docs/adr/0022-item-transform-and-canvas.md)): a geometry by the
shared formula, a legacy transform evaluated against the frame's size.
Hidden items are skipped. A source with no stored frame is skipped, not drawn as a
placeholder. Opacity is 1 and the only blend is src-over: the graph does not store
either yet, and the compositor's blend is src-over.

**Nested scenes.** A `scene` source shows its scene composited at the canvas size,
then placed like any image. The shot is scheduled inside out: each nested scene is
one `Compositor::render_image` pass, which stays on the GPU as premultiplied linear
`Rgba16Float` (no sRGB round trip, no readback), and is dropped after the last pass
that places it. A nested scene has no background yet, so it is transparent where it
draws nothing and the scene under it shows through; the program scene is drawn over
opaque black. A scene placed more than once at the same depth is composited once.
Depth is bounded: the program scene is depth 0, and a scene source more than 8
levels below it draws nothing (logged at `debug`). Width is not bounded: an image
stays alive until the last pass that places it, so a scene with *k* distinct nested
scene sources holds *k* canvas-sized FP16 textures at once. The graph already rejects cycles at every
edit that adds nesting; if a copied shot had one, the scene source that closes it
draws nothing. The live pipeline's render thread (`runtime/`) still composites one
scene and does not draw nested scenes yet.

`Engine::set_source_frame` stores the latest straight sRGB RGBA8 frame for a source
id. It changes no `rev` and emits no event, and a frame may arrive before the source
exists. An applied `source.delete` drops the deleted source's frame, unless the same
batch created a source under that id again. Width or height of zero, a buffer whose length is not `width * height * 4`,
or a size that does not fit in memory is `SourceFrameError`.

The work runs on `jstream-snapshot`, a thread the engine starts on the first
snapshot. It owns the compositor, serves requests in order, and stops when the
engine goes away. The compositor opens on the first snapshot, software adapter first
(`OpenOptions::headless`: lavapipe or WARP). No adapter is `internal`, and the reason
starts with `no wgpu adapter`. A failed open is cached: until its retry time,
snapshots answer that error at once instead of probing adapters again. The first
retry is 5 s after the failure, and each failure in a row doubles the wait, up to
5 minutes; a successful open clears it. A panic while drawing answers `internal`,
and the compositor is reopened for the next snapshot. The readback is scaled so its longest side is at most
1280 ([ARCH › MCP](../../docs/ARCHITECTURE.md#mcp)), nearest-neighbour. Equal sizes
are not resampled. The PNG is standard base64 with padding and no line breaks.

The collection lock is not held across the GPU wait
([R-RT-002](../../docs/RULES.md#R-RT-002)), and neither is the caller's thread: the
call's future waits for the snapshot thread, so an async host's worker thread is
free while the GPU renders, reads back and the PNG is encoded. This query is
control-plane work, not the render thread.

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

### Drop counts (`stats.tick`)

[R-RT-004](../../docs/RULES.md#R-RT-004) says overload drops are counted and reported in
`stats.tick`. The stages already count. The engine does not count a second time.
`output.start` registers one [`DropSource`](src/drops.rs) per stage of that run and
starts a sampler thread, `jstream-stats`, that calls
[`Engine::emit_stats_tick`](src/engine.rs) about once a second. The sampler is not a
real-time thread. Stopping the pipeline joins it and removes the sources, which
latches their last totals so the next tick does not shrink, then emits one final
tick, so the run's last drops are reported when it stops (before `stopped`, or
after `failed`). `seq` starts at 1.
Registering two sources for the same drop adds it twice.

A source reports running totals. The tick is the sum, so a client subtracts the previous
tick.

What each stage contributes:

| Stage | Read from | Fields |
|---|---|---|
| Capture mailbox | [`CaptureStats::frames_skipped`](../jstream-platform/src/capture.rs), published by the render thread | `capture_frames` |
| Render loop | ticks skipped to catch up with the media clock, published by the render thread | `render_ticks` |
| Mixer input | overrun and late frames, both inputs | `audio_frames` |
| Mix bus | frames skipped when the mixer falls behind the clock | `audio_frames` |
| Video encode session | input drops (`dropped_queue_full` + `dropped_encoder_busy`) | `encode_frames` |
| Audio encode session | the same input drops | `encode_blocks` |
| Encode subscriber queue | `subscriber_drops` | `output_packets` |
| Muxer | a packet the FLV tagger rejects | `output_packets` |
| Stop drain | packets still queued when a stop's drain deadline passes, or when a send fails while stopping, dropped by the output thread | `output_packets` |
| Publish link | tags the publish target dropped while its link was down (`LivePublish::dropped`; for RTMP, `ReconnectStats::dropped`), published by the output thread | `output_packets` |

A render tick that falls a whole period or more behind the clock is not drawn late:
the loop skips to the current tick, stamps the frame with it and counts the skipped
ticks in `render_ticks`, so video stays on the clock that stamps audio
([R-RT-004](../../docs/RULES.md#R-RT-004)). There is no separate output queue in front of the network. Underruns, device gaps and
encoder gap-fill silence are not drops: the mixer plays silence for them, and the late
or overwritten frames are already in `audio_frames`. A late plugin block is the same
kind of audio drop ([ADR-0035](../../docs/adr/0035-clap-plugins-out-of-process.md));
the mixer does not count it yet, and when it does that total belongs in `audio_frames`.
The capture cell is a [`DropSnapshot`](src/drops.rs) (atomics only,
[R-RT-003](../../docs/RULES.md#R-RT-003)). Encode sessions and mixer handles are shared,
and their sources load the counters those handles expose. `drops` runs on the sampler
thread and must not call back into the engine. The fake capture generates a frame when
it is read, so it does not simulate mailbox skips.

### Outputs

An output's configuration is collection state (`output.create`, `output.set_target`,
`output.delete`, kept by `jstream-graph`); whether it runs is runtime state, which never
changes `rev`. After each applied batch the runtime states follow the collection: a
created output starts `stopped`, and a deleted one is forgotten. The graph refuses to
delete a running output (`invalid_argument`), so the batch passes it the running set.
A host can also declare an output with `Engine::with_output(id)`, outside the
collection: it starts `stopped`, has no target and stays known whatever the collection
holds. A live host reads a collection output's target with `output_kind` when its
publish opener runs; wiring that into the daemon is not done yet. A recording output
(`OutputKind::Record`) never calls the opener or takes a one-shot target.

**Stream keys.** `with_key_store` injects the OS credential store
(`jstream_platform::StreamKeyStore`). `output.set_key` checks that the output is in the
collection (`not_found`), then the key's shape (`invalid_argument`, field `key`: empty,
over 1024 bytes, or with whitespace or control characters), then stores it under the
output ID with no engine lock held. `output.clear_key` removes it and succeeds when
there is none, even for an output no longer in the collection. With no store, or when
the store refuses, both fail with `action_failed`. The key is write-only
([R-SEC-001](../../docs/RULES.md#R-SEC-001)): every reason is fixed text, nothing logs
the call's arguments, and no response, event or query carries it. The crate-root test
`secrets_are_redacted` sets a key and checks the reply, `state.get`, `output.list`,
`output.status`, the events, every span and event the engine traces, and the audit
entries.

**Placeholder.** With no backends, the actions only move a state machine, as
`jstream-control`'s mock does. The protocol defines an action's response as "accepted"
and reports progress as events, so that is what the engine does:
- `output.start` on a `stopped` or `failed` output emits `starting` then `active` and
  answers `{"kind":"action"}`; `output.stop` on any other state emits `stopping` then
  `stopped`.
- Starting a running output, or stopping one that isn't, fails with `action_failed`
  (an accepted action that can't be carried out); an unknown output with `not_found`.

The placeholder stays so clients that already drive start/stop/status against the mock
keep working when the host has not injected media.

**Pipeline.** `with_backends` plus a publish target makes `output.start` launch
threads instead. The target is either one-shot, set with `with_publish` and consumed
by the next start, or an opener (`OpenPublish`, set with `with_publish_opener`) that
every start calls when no one-shot target is waiting. A live host uses the opener
with `connect_rtmp`: each start connects anew, so the output can start again after a
stop or a failure, and no session sits idle before the start. The opener runs after
`starting`, outside the collection lock and the pipeline gate, before any device
opens; its error fails the start like a launch failure (`failed`, then
`action_failed` with `opening the publish target: <reason>`), and the opener is kept.
Its error text must not carry a stream key
([R-SEC-001](../../docs/RULES.md#R-SEC-001)); `connect_rtmp`'s does not. The call checks the output is idle and that no other pipeline is running,
takes the publish target, records an in-flight generation, emits `starting`, and
drops the collection lock before it opens devices or joins anything. It then waits,
up to 10 s, for the render thread to open the software compositor; that bound only
keeps a stuck adapter from blocking the call, and the GPU tests raise it because
lavapipe can open slower than that on a loaded CI runner. Success emits `active`.
Failure emits `failed` and `action_failed`, and drops a one-shot publish target so
the host sets another before retrying (an opener is kept). A second start while one pipeline is up, or
while a start is still in flight, fails with `action_failed` and leaves that
pipeline alone. `output.stop` on an `active` pipeline emits `stopping`, drops it
outside the lock (its drop sets the stop flag and joins the threads; the output
thread's drain is bounded, see the RTMP paragraph below), then emits
`stopped`. `output.stop` during the in-flight start cancels that generation, emits
`stopping`, waits until the start has dropped whatever it opened, then emits
`stopped`. The start then answers `action_failed` and does not emit `active`. The
publish target (one-shot or opened) is dropped on that path too.

**Recording.** When the collection's output is a `Record { path, container: mp4 }`,
the start sends the encoded packets to a file instead of a publish target
(`runtime/sink.rs`): the output thread muxes fragmented MP4 with
`jstream-outputs`' `Recorder`, which writes `<path>.part`, syncs each fragment (one
per keyframe, every two seconds) and renames the file to `path` when the output
stops. The file is opened in `launch`, after the devices and encoders and before the
threads, so a path that can't be recorded to (one not absolute on this machine, an existing
file or `.part`, a missing directory) fails the start with `action_failed` and `opening the recording: <reason>`.
Until the compositor has opened, the recording is provisional: a start that fails
after opening the file removes the `.part` file, so it doesn't block the next start,
and so does one whose init segment can't be written (`DiskFull` or `Write` from
`Recorder::create`).
Once `active`, the `.part` file is kept whatever happens: a write error (a full disk
among them) settles `failed` and leaves it readable up to its last fragment, and a
failed rename at stop is logged with the `.part` path. A packet the muxer rejects is
dropped and counted in `output_packets`. Disk writes run on the output thread and
may block; the encoder subscriber queues absorb that and drop when full. Only one
pipeline runs at a time, so recording while streaming waits for shared encoders (F3).

Once the pipeline is `active`, a fatal error on any pipeline thread is sent on a
channel: a `send_tag` or `poll` failure, a recording write failure, an encoder stream that ends before the
pipeline was told to stop (the encoder failed, or its session closed), and the
render and mixer failures below.
A watcher the control thread started settles `failed`, emits `output.state_changed`,
and drops the pipeline so the render and mixer threads stop. The watcher holds weak
pointers between messages, so it does not keep the engine alive: dropping
the last engine handle and its sessions joins the pipeline. `output_stats` is then
empty. `failed` counts as idle, and a later start needs a new one-shot target or an opener. That
start waits until the failed pipeline has finished joining, so the new devices do
not overlap the old teardown. The same holds when a
start fails after `launch` returned threads (a failure reported before `active`, or
the watcher not starting): `failed` is emitted, and the next start waits until that
pipeline has joined. A stop's join also holds off a start of another output. The
wait also ends if that teardown panics, so a later start is not left waiting on a
join that already unwound. If the teardown panics during `output.stop`, the output
settles `failed` instead of staying `stopping`, and the panic reaches the caller. A
`finish` or send error, or an encoder stream ending, while a stop is tearing the
session down is not a second failure.

A lost connection is not fatal. After each pass the output thread reads the publish
target's `LivePublish::link_state` (`Active` unless the target can lose its link,
which `RecordingPublish` cannot) and sends a message on the same channel when it
changes: `Active` to `Reconnecting`, and back. The watcher settles and emits
`reconnecting`, then `active`, only while the output is still its pipeline's and in
the state the change starts from, so a stop that has already taken the pipeline wins.
A change reported before the start settles is emitted right after `active`. While
reconnecting, the pipeline keeps running and the output thread keeps sending: the
target drops each tag and counts it (`LivePublish::dropped`, in `output_packets`)
instead of waiting, and the attempt runs on a thread of its own. Giving up
(`RtmpError::ReconnectGaveUp`) fails the next `send_tag` or `poll`, which settles
`failed` from `reconnecting` as from `active`. `output.stop` while reconnecting
emits `stopping` and `stopped` as from `active`.

The render thread reads the platform capture mailbox, composites the primary target
at 30 frames per second, repeats the last frame when capture is empty, and reads the
composite back as RGBA8 because this cut has no GPU encode target. What it rides out
and what is fatal ([R-RT-006](../../docs/RULES.md#R-RT-006)):
- **Recoverable:** a capture error or a missing frame repeats the last capture (a
  missing source frame, [ARCH › Threads](../../docs/ARCHITECTURE.md#threading)); a
  composite error repeats the last composited frame, up to 30 in a row.
- **Fatal, reported:** a 31st composite error in a row, a panic inside a tick (the
  compositor's state is unknown after it), and a frame time that overflows. The
  thread then keeps pushing the last frame until the pipeline stops, so the stream
  does not freeze before the teardown.
- The render and mixer threads both catch a panic at their boundary. A fatal error
  becomes `output.state_changed` `failed`, the error event this protocol has. A
  mixer panic, a rejected mix block
  or a mixer poll error is fatal: the encoder would get no more audio.

The render thread reuses its capture, composite and readback buffers. Each tick
allocates one frame-sized buffer, the frame the encoder session takes ownership of
(`VideoInput::push` cannot hand buffers back); the mixer likewise copies each block
into the buffer `AudioInput::push` keeps. Not measured. The mixer thread
polls microphone and desktop-loopback input into 48 kHz stereo blocks. Encode
sessions turn those into H.264 and AAC. The output thread writes FLV tags to the
publish target. Drops on a full encoder queue are counted in `output_stats`. The
test records tags in memory. `connect_rtmp` is the live publisher: `RtmpPublish`
wraps `jstream-outputs`' `ReconnectingPublisher` with its default `ReconnectPolicy`
(backoff from 1 s up to 30 s, giving up after a 5-minute outage). It opens the first
session itself, on the caller's thread, so a refused first connect is still its
error and is not retried; only a session lost after that reconnects. Every session,
the first and each reconnect attempt (built with
`rtmp_connector` on the same URL and key), reads with a 10 s timeout through the
handshake and the publish exchange (the publisher retries only interrupted reads, so
a shorter timeout fails on a slow server), then drops the read timeout to 1 ms so
`poll` does not block the output thread. Writes keep a 10 s timeout while the output
runs, so a stalled write takes that long to count as a lost link.

A stop bounds its drain. When the output thread sees the stop flag it gives itself
2 s (`STOP_DRAIN`) to send what the encoders queued and tells the target that
deadline (`LivePublish::stop_by`; the default does nothing). Once it passes, or a send
fails while stopping, the thread drops the packets still queued, counts them in
`output_packets`, and calls `finish`. A failure while stopping is logged, not
reported: the output is already on its way to `stopped`. `RtmpPublish` honors the
deadline: each write's timeout is cut to the time left, a lost link is not
reconnected, and past the deadline sends fail and `finish` drops the connection
without the `FCUnpublish` … `deleteStream` goodbye. So a peer that has stopped
reading holds `output.stop` for about 2 s, not 10 s per blocked write; a target that
ignores `stop_by` can still add one blocked send. `connect_rtmp`'s unit tests drive
fake servers on loopback: a slow one, one that hangs up mid-stream, and one that
stops reading while the deadline runs out.

### Locking

`std::sync::Mutex` locks, always taken in one order: a session's identity lock (held
by `hello` while it registers the client), then the engine's collection lock, then
the pipeline gate, then a subscription queue's. The drop ledger is its own mutex, and
so is the audit sink's handle, which is only cloned under it (see *Audit log*).
The sampler and `output.start` take it only to read or register sources, and they
release it before taking the collection lock. A source must not call back into the
engine while that mutex is held. The snapshot compositor belongs to the snapshot
thread, which takes no engine lock; the queue to it is taken only after the
collection lock is released. A stream's poll takes only its queue's lock. No lock
is held across an `await`, and a snapshot's reply slot is not held across a wake.
`output.start`
drops the collection lock before `Compositor::open`. The pipeline gate is not held
across device open, that wait, or a join. `output.stop` waits for an in-flight
start on the gate's condition variable without holding the collection lock. The
publish watcher keeps weak pointers between messages; it takes the collection lock
only to settle `reconnecting`, `active` or `failed`, and joins a failed pipeline
outside it. A later
start waits on that same condition variable, still without the collection lock,
until the join finishes. The render, mixer, encode and output threads do not take the
collection lock; session counters the host reads
sit on their own mutex. A poisoned lock is recovered rather than propagated:
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

### Audit log

Every agent action goes to the audit log
([ADR-0006](../../docs/adr/0006-mcp-full-access.md),
[ARCH › MCP](../../docs/ARCHITECTURE.md#mcp)). `Engine::execute` records one
`AuditEntry` for each batch, undo, redo and action, failed ones included, and none for queries:
the time, `client_id`, the client's name from its latest `hello` (`client_name`, cut
to 128 bytes; `client_id`s restart at `client_1` with the engine, so the name is what
says which agent acted across restarts), the session's `request_id`, the call (`batch`,
`history.undo`, `history.redo` or the action's name), a batch's edits by name in order, an action's `output_id`, the `rev` before and
after, the result (`ok` or the error code), and `replayed` for a batch, undo or redo whose
idempotency key had already been applied. It holds no argument of any call: no names,
settings, URLs or keys, so `output.set_key` records only that action on that output
([R-SEC-001](../../docs/RULES.md#R-SEC-001)). The `rev` before is read just before the
call; an applied batch, undo or redo reports its own `rev` minus one, so another client's batch
landing in between does not skew it, and a failed call or an action reports the same
`rev` twice. The `request_id` is the in-process session's (`req_N`); for a WebSocket
client the server maps its own wire ID to it, so the entry can't yet be matched to the
ID the client sent. The engine keeps each client's name for its lifetime, as it keeps
the IDs.

`AuditSink` is a trait because the host chooses the sink (a file in the data
directory, memory in tests, its own view in a UI), the same kind of host-injection
seam as `DropSource`.

The sink is behind its own mutex, taken only to clone the handle, and `record` is
called after the call completes, with no engine lock held. A sink must not block:
`AuditFile` (`audit.rs`) sends entries over a bounded channel (1024 entries) to its
writer thread, and when that is full counts what it drops; the next line written is
`{"time":…,"dropped":n}`. The writer appends one JSON line per entry to `audit.jsonl`
in the directory the host names (the data directory,
[ARCH › Persistence](../../docs/ARCHITECTURE.md#persistence)), with mode `0600` on Unix.
When a line would take it past 8 MiB (or the limit given to `with_limit`) it is
renamed to `audit.1.jsonl`, replacing the previous one, so the log stays under about
twice the limit. A write error is logged at `warn` and loses the entry; it never fails
the call. Dropping the `AuditFile` writes what is queued and joins the thread. Times
are UTC, RFC 3339 to the millisecond, formatted without a date crate.

No host injects a sink yet: the daemon's wiring, in its data directory, is a later
item.

### Not yet

- **Live values** for continuous gestures (`live.set`, `live.cancel` and their events,
  [ADR-0032](../../docs/adr/0032-live-edits-and-undo.md)), which need the real-time
  parameter channels.
- Persistence (the collection file), the daemon's audit file, and real source kinds with
  settings that change what renders (`jstream-sources`).
- Arc-swap snapshots, the collector thread, live-value handover, and a GPU encode
  target. `output.start` is the first cut of
  [ARCH › Threads](../../docs/ARCHITECTURE.md#threading): one scene, a CPU readback
  into the encoder, and the platform fakes in the test. `video.snapshot` still
  composites from frames the host injects with `set_source_frame`.
