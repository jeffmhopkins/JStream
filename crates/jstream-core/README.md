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
verified = { hash = "b3:864787da5351c890", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Re-read after the compositor-open bound became a MediaHost field: the Runtime and Tests bullets still hold (GPU pipeline tests, CPU-draw tests, slow fake RTMP server); the bound is a launch detail described in Design." }
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
`with_backends` and `with_publish`. Without those, an output action stays the
placeholder state machine. Contents:
- **Engine** (`engine.rs`): `Engine`, a cloneable handle to one engine, built with
  `Engine::new()`, `with_output(id)`, `with_backends` and `with_publish`;
  `connect()` opens a session; `rev()`; `set_source_frame` stores the latest RGBA8
  frame of a source; `output_stats` reads the running pipeline. It holds the
  `jstream-graph` `Graph`, the outputs, those frames, the idempotency keys and the
  subscribers behind one lock, and the drop ledger behind another, and executes
  batches, actions and queries. The pipeline threads do not take the collection lock.
- **Snapshot** (`snapshot.rs`): `video.snapshot`, the program mix read back as a PNG
  on the engine's own snapshot thread, which owns the snapshot compositor.
- **Runtime** (`runtime/`): the `output.start` pipeline. The render thread reads the
  platform capture mailbox, composites one scene and reads the frame back. The mixer
  thread polls the media clock. Encode threads live in the encoder sessions. The
  output thread muxes FLV and publishes it
  ([ARCH › Threads](../../docs/ARCHITECTURE.md#threading)).
- **Session** (`session.rs`): `InProcessControl`, one client session.
- **Events** (`events.rs`): the per-subscription queues behind `Control::events`.
- **Outputs** (`outputs.rs`): the outputs' runtime state. The placeholder path steps
  `starting` to `active` at once. The pipeline path emits `starting`, then `active`
  or `failed`. Stop emits `stopping` then `stopped`. A stop during `starting` cancels
  that start and settles `stopped` only after its threads are gone. A publish,
  encoder, render or mixer failure after `active` settles `failed`.
- **Drop counts** (`drops.rs`): the sources a `stats.tick` sums. `output.start`
  registers the capture cell, both encode sessions, both mixer inputs and the mix
  bus, and a sampler thread emits a tick about once a second until the pipeline stops.
- **Tests** (`tests.rs`, `tests/mcp.rs`, `tests/scenarios.rs`): the protocol rules through
  `InProcessControl`, the shared parity fixture, a differential run against
  `jstream-control`'s mock, many sessions on many threads, a headless
  `video.snapshot` on the software adapter, the pipeline against the platform fakes
  (on the software adapter, and again on a CPU stand-in for the compositor so the
  stage-failure, late-tick, teardown and final-tick tests run without one),
  `connect_rtmp` against a slow fake RTMP server on loopback, overload drops from a
  fake encoder and the mixer reported in `stats.tick`, and an MCP agent
  (`jstream-mcp`, a dev-dependency) driving the engine in-process.

Not here yet: per-client undo, the audit log, persistence, settings, arc-swap
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
verified = { hash = "b3:fff4ece39b4cd13e", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Stale via jstream-protocol › Design, now describing render_ticks as catch-up skips; core Design's drop table and late-tick paragraph already say the same, so it holds" }
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
  identity with the current `rev`. A refused `hello` opens nothing.
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
`jstream-mux` (FLV), `jstream-outputs` (the RTMP publisher), `png` (encodes the
snapshot readback; default features off, deflate stays pure Rust, allowed by
[ADR-0031](../../docs/adr/0031-source-types-and-decoding-scope.md)), `thiserror`
(`SourceFrameError`), `futures-core` (the `Stream` trait of event streams) and
`tracing` (see *Logging*). No async runtime: `output.start` blocks the call until
the pipeline is up or has failed, and `video.snapshot` runs on a plain thread. Dev-dependencies: `jstream-control`'s `mock`
feature and `jstream-protocol`'s `fixtures` feature (the differential and parity
tests), `jstream-platform`'s `fake` feature (the pipeline test and
`tests/scenarios.rs`), `jstream-mcp` and `serde_json` (`tests/mcp.rs`). All of them
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

Outputs are runtime state, not collection state: they never change `rev`. Until the
protocol has edits that configure outputs, the host declares them with
`Engine::with_output(id)`; each starts `stopped`.

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

**Pipeline.** `with_backends` plus `with_publish` makes `output.start` launch threads
instead. The call checks the output is idle and that no other pipeline is running,
takes the publish target, records an in-flight generation, emits `starting`, and
drops the collection lock before it opens devices or joins anything. It then waits,
up to 10 s, for the render thread to open the software compositor; that bound only
keeps a stuck adapter from blocking the call, and the GPU tests raise it because
lavapipe can open slower than that on a loaded CI runner. Success emits `active`.
Failure emits `failed` and `action_failed`, and drops the publish target so the
host sets another before retrying. A second start while one pipeline is up, or
while a start is still in flight, fails with `action_failed` and leaves that
pipeline alone. `output.stop` on an `active` pipeline emits `stopping`, drops it
outside the lock (its drop sets the stop flag and joins the threads), then emits
`stopped`. `output.stop` during the in-flight start cancels that generation, emits
`stopping`, waits until the start has dropped whatever it opened, then emits
`stopped`. The start then answers `action_failed` and does not emit `active`. The
publish target is dropped on that path too.

Once the pipeline is `active`, a fatal error on any pipeline thread is sent on a
channel: a `send_tag` or `poll` failure, an encoder stream that ends before the
pipeline was told to stop (the encoder failed, or its session closed), and the
render and mixer failures below.
A watcher the control thread started settles `failed`, emits `output.state_changed`,
and drops the pipeline so the render and mixer threads stop. The watcher holds weak
pointers until that failure arrives, so it does not keep the engine alive: dropping
the last engine handle and its sessions joins the pipeline. `output_stats` is then
empty. This cut does not reconnect. `failed` counts as idle, and a later start needs
a new publish target. That start waits until the failed pipeline has finished
joining, so the new devices do not overlap the old teardown. The same holds when a
start fails after `launch` returned threads (a failure reported before `active`, or
the watcher not starting): `failed` is emitted, and the next start waits until that
pipeline has joined. A stop's join also holds off a start of another output. The
wait also ends if that teardown panics, so a later start is not left waiting on a
join that already unwound. If the teardown panics during `output.stop`, the output
settles `failed` instead of staying `stopping`, and the panic reaches the caller. A
`finish` error, or an encoder stream ending, while a stop is tearing the session
down is not a second failure.

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
test records tags in memory. `connect_rtmp` is the live publisher. It reads with a
10 s timeout through the handshake and the publish exchange (the publisher retries
only interrupted reads, so a shorter timeout fails on a slow server), then drops the
read timeout to 1 ms so `poll` does not block the output thread. Writes keep a 10 s
timeout: a stop drains the queued packets before `finish`, so a peer that has
stopped reading can hold `output.stop` for up to 10 s per blocked write. There is
no shorter drain bound on stop yet. Its unit test drives a fake server on loopback.

### Locking

`std::sync::Mutex` locks, always taken in one order: a session's identity lock (held
by `hello` while it registers the client), then the engine's collection lock, then
the pipeline gate, then a subscription queue's. The drop ledger is its own mutex.
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
publish watcher keeps weak pointers until a failure arrives, then takes the
collection lock only to settle `failed` and joins the pipeline outside it. A later
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
- Arc-swap snapshots, the collector thread, live-value handover, and a GPU encode
  target. `output.start` is the first cut of
  [ARCH › Threads](../../docs/ARCHITECTURE.md#threading): one scene, a CPU readback
  into the encoder, and the platform fakes in the test. `video.snapshot` still
  composites from frames the host injects with `set_source_frame`.
- `R-SEC-001`'s `secrets_are_redacted` test: the engine holds no secret yet.
