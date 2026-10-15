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
`with_publish_opener`); a recording output needs no publish target, nor does a
virtual camera, which publishes to the backends' `VcamSink`. Without backends, an output action stays the placeholder
state machine. Contents:
- **Engine** (`engine.rs`): `Engine`, a cloneable handle to one engine, built with
  `Engine::new()`, `with_output(id)`, `with_backends`, `with_publish`,
  `with_publish_opener`, `with_key_store`, `with_audit`, `with_history_limit` and
  `with_collection_file`;
  `connect()` opens a session; `rev()`; `set_source_frame` stores the latest RGBA8
  frame of a source; `output_stats` reads a running output's pipeline counters
  (`output_stats_of` names the output); `output_kind` reads a
  collection output's target. `output.set_key` and `output.clear_key` go to the
  injected `StreamKeyStore`, never under the collection lock. It holds the
  `jstream-graph` `Graph`, the outputs, those frames, the idempotency keys, the undo
  history and the subscribers behind one lock, and the drop ledger behind another, and executes
  batches, history calls, actions and queries. The pipeline threads do not take the collection lock.
- **Audit log** (`audit.rs`): one `AuditEntry` per executed batch, undo, redo and action, sent to
  the `AuditSink` the host injects with `with_audit` (none by default). `AuditMemory`
  keeps entries in memory; `AuditFile` writes them as JSON lines to a size-capped
  `audit.jsonl` from its own writer thread (see *Audit log* in [Design](#design)).
- **Collection file** (`collection.rs`): loads the collection from the file the host
  names with `with_collection_file` (or creates it), and saves it atomically after every
  change (see *Collection file* in [Design](#design)).
- **Kinds** (`kinds.rs`): the built-in source kinds the graph validates settings
  against and `kind.list` reports ([ADR-0025](../../docs/adr/0025-typed-settings-kind-descriptors.md)),
  taken from `jstream-sources`' built-in registry.
- **Live sources** (`live.rs`): a running `jstream-sources` source for every source
  in the collection, built when it is created, rebuilt when its settings change and
  dropped when it is deleted, on the engine's `jstream-sources` thread. The program
  mix shows their frames and advances animations on the media clock. A frame stored
  with `set_source_frame` wins over the live one, and a kind that is only described
  draws nothing, except that a `display` source shows the output pipeline's capture
  on the render thread (see *Live sources* and *Snapshots* in [Design](#design)).
  Audio-only kinds make no frames and have no live source.
- **Audio sources** (`audio_sources.rs`): while the program stage runs, one mixer
  input for each `audio_input`, `desktop_audio` and `app_audio` source whose audio is
  on, added when the source is created or turned on and removed when it is deleted or
  turned off, and listed for the audio routing,
  which sets its fader, mute and sends; while there is none, the default microphone
  and desktop audio, sent to master at unity (see *Audio sources* in
  [Design](#design)).
- **Snapshot** (`snapshot.rs`): the program mix as both the render thread and
  `video.snapshot` draw it (`plan`, `compose`), and `video.snapshot`, the program mix
  read back as a PNG on the engine's own snapshot thread, which owns the snapshot
  compositor.
- **Studio** (`studio.rs`): which scene is on program, runtime state under the
  collection lock that `scene.switch` sets and `studio.get` reads
  ([ADR-0030](../../docs/adr/0030-studio-state-and-scene-switching.md)).
- **Runtime** (`runtime/`): the `output.start` pipeline. Every running output shares
  one program stage: the render thread reads the platform capture mailbox, composites
  the program mix from the render snapshot the control side publishes
  (`runtime/render.rs`) and reads the frame back, and the mixer thread polls the media
  clock. Encode threads live in the encoder sessions, one per encoder profile in use,
  shared by the outputs on that profile. Each output's own thread muxes FLV and
  publishes it, or muxes MP4 and records it
  ([ARCH › Threads](../../docs/ARCHITECTURE.md#threading)). A virtual camera's
  thread converts and publishes the frames the render thread sends it through a tap
  (`runtime/vcam.rs`).
- **Audio** (`audio.rs`, [ADR-0028](../../docs/adr/0028-audio-model-v2.md)): the
  program mixer follows the collection's audio. While the program stage runs, every bus
  of the collection has a mixer bus with its fader and mute, and every source with a
  mixer input has its fader and its sends (silenced while it is `muted`), scaled by
  its activity gain (`activity.rs`): a `when_shown` source is heard only while the
  Program mix shows it; `meters.tick` carries their levels about ten times a second, with the master's true
  peak and BS.1770 loudness, which the mixer thread measures on each block, and
  `audio.reset_loudness` restarts its integrated loudness.
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
  registers the capture cell, both encode sessions, each mixer input, the mix
  bus, and the packets the output thread drops (muxer rejects and what a stop's
  drain abandons) and the publish link's, and a sampler thread
  emits a tick about once a second until the last output stops. The same thread emits
  `meters.tick` about ten times a second.
- **Tests** (`tests.rs`, `src/tests/history.rs`, `src/tests/audio.rs`,
  `src/tests/audio_routing.rs`, `tests/mcp.rs`, `tests/scenarios.rs`):
  the protocol rules through `InProcessControl`, undo and redo across two clients
  (conflicts, `expected_rev`, idempotency, the limit, a running output, a session's
  end), the shared parity fixture, a differential run against
  `jstream-control`'s mock, many sessions on many threads, a headless
  `video.snapshot` on the software adapter, nested-scene scheduling (order, depth
  bound, cycles) and its pixels through two levels in `snapshot.rs`, the render
  thread's output size (the capture filling it with no scene, the program scaled from
  the canvas, a `display` source showing the capture) and nested-scene layers reused
  frame after frame then freed after the fence, render-snapshot generations, the
  reader slot and the collector in `runtime/render.rs`, the render thread adopting a
  published snapshot (CPU stand-in), the pipeline against the platform fakes
  (on the software adapter, and again on a CPU stand-in for the compositor so the
  stage-failure, late-tick, teardown, final-tick, link-loss, stalled-stop and
  blocked-send stop tests run without one),
  `connect_rtmp` against a slow fake RTMP server on loopback, one that hangs up
  and is reconnected to, one that stops reading while a stop's deadline runs out, and
  one that reads only a trickle of one large tag,
  a publish opener connecting that fake server on every start (start, media, stop,
  twice) and one that cannot connect, a recording output writing MP4 to a temp dir
  (renamed into place on stop, refused when the file exists or the path is not
  absolute on this machine, no `.part` file left by a failed start), recording while
  streaming on one shared encoder session (and separate sessions on separate
  profiles), a virtual camera publishing the program as BGRA at the canvas size
  (fitted into a canvas of another size, started again after a stop, running beside
  a stream, refused without a camera in the backends or while another output uses
  it, a refused start keeping the sink, a failed publish failing only that output), a render panic failing both outputs, overload drops from a
  fake encoder and the mixer reported in `stats.tick`, render and mixer panics
  reported as `failed` on the CPU stand-in (`tests/scenarios.rs` reaches it through
  the hidden `Engine::with_cpu_compositor`), audio sources becoming mixer inputs on the CPU
  stand-in (`src/tests/audio.rs`: the defaults while there is none, sources added and
  deleted while an output runs, an unavailable device or application that leaves the
  output running, a failed stream reopened, an application found again under a new
  process ID, and a settings change reopening its input), the output edits and the
  stream-key actions against `jstream-platform`'s `MemoryKeyStore` (with `secrets_are_redacted`,
  which captures the engine's traces and audit entries), the audit entries of a run of
  batches and actions (`batches_and_actions_are_audited`) and `AuditFile`'s lines and
  rotation (`audit.rs`), and of undo and redo (`undo_and_redo_are_audited`), the
  collection file (`src/tests/collection.rs`: created when missing, batches, an undo
  and a redo each saved, a saved collection loaded by a new engine with its `rev` and
  outputs, refused files left
  as they were, a failed save that the next change recovers, and concurrent sessions
  leaving the latest collection and no temporary file), `scene.switch` and the program
  scene's rules (in `tests.rs`, the differential script and `studio.rs`), a snapshot
  that follows program, and an MCP agent (`jstream-mcp`, a dev-dependency) driving the
  engine in-process. The audio routing (`audio.rs` and
  `src/tests/audio_routing.rs`): a mixer's buses, faders, mutes and sends following
  batches and undo, `meters.tick` while a mixer is attached and none after,
  `audio.reset_loudness`, the master's true peak and loudness, and a `when_shown`
  source's activity gain following scene switches, shown and hidden items and undo
  (`activity.rs` checks reachability itself); `src/tests/audio.rs`
  also checks that a recording audio source is metered, and that turning its audio
  off closes its input and its meter and turning it on opens them again. The parity
  tests add the fixture's `mic` kind, which no built-in kind matches.

Not here yet: live values, the audio and outputs snapshots, a GPU encode target, and
the platform's Windows devices. See
[Design › Not yet](#design).

Why a crate: it is the crate map's layer-5 engine, the one crate allowed to depend on
every subsystem, and it must sit above `jstream-server` and `jstream-mcp` so that none
of the clients can reach it.

<a id="design"></a>
## Design
<!-- @section
covers = ["crates/jstream-core/src/**", "crates/jstream-core/Cargo.toml"]
depends_on = ["docs/ARCHITECTURE.md#protocol", "docs/adr/0013-command-model.md", "docs/RULES.md#R-API-004", "crates/jstream-protocol/README.md#design", "crates/jstream-control/README.md#design"]
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
- `pairing.start`, `device.list` and `device.revoke` fail with `unsupported`: they
  concern whom a listener admits, which only the server in front of the session knows,
  and it answers them before they reach the engine
  ([ADR-0046](../../docs/adr/0046-server-answers-credential-calls.md)). Behind a server
  they never reach the engine, so the audit log doesn't record them; the server logs
  them. A device's `hello` reaches the engine as `device:<name>`, so the audit
  log names it.

### Dependencies

`jstream-protocol` (the messages), `jstream-control` (the `Control` trait),
`jstream-graph` (the collection and every edit rule), `jstream-media` (frames, the
canvas `FrameSize` and the media clock's time base), `jstream-render` (the snapshot
and the pipeline compositor), `jstream-platform` (backend traits the host injects, and
the capture counters a `stats.tick` reads), `jstream-audio` (the mixer and its drop
counters), `jstream-encode` (the H.264 and AAC sessions and their drop counters),
`jstream-mux` (FLV and fragmented MP4), `jstream-outputs` (the reconnecting RTMP
publisher and the recording file sink), `jstream-sources` (the built-in kinds: their
descriptors and the sources that draw them), `png`
(encodes the snapshot readback; default features off, deflate stays pure Rust, allowed by
[ADR-0031](../../docs/adr/0031-source-types-and-decoding-scope.md)), `thiserror`
(`SourceFrameError`), `futures-core` (the `Stream` trait of event streams) and
`tracing` (see *Logging*). No async runtime: `output.start` blocks the call until
the pipeline is up or has failed, and `video.snapshot` runs on a plain thread. Dev-dependencies: `jstream-control`'s `mock`
feature and `jstream-protocol`'s `fixtures` feature (the differential and parity
tests), `jstream-platform`'s `fake` feature (the pipeline test and
`tests/scenarios.rs`), `jstream-mcp` (`tests/mcp.rs`) and `serde_json` (settings
values and saved collection files in `src/tests.rs`, `src/tests/` and `tests/mcp.rs`). All of them
are in lower layers
([R-ARCH-002](../../docs/RULES.md#R-ARCH-002)), and none of the clients depends on this
crate.

### Calls

Batches and the collection queries run under the collection lock, so those calls
are serialized across sessions and their events are queued in the order the changes
happened. Placeholder output actions do too: they only step the state machine.
`video.snapshot` reads `rev` and the published render snapshot under that lock and
hands them to the snapshot thread after releasing it (see *Snapshots*).

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
- **Actions**: `output.start` and `output.stop` (see *Outputs* below),
  `output.set_key` and `output.clear_key` (see *Stream keys* below), and
  `scene.switch` (see *Program scene* below), and `audio.reset_loudness` (see *Audio
  routing*), which succeeds whether or not the mixer runs.
- **`studio.get`** reads the program scene under the collection lock.
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
  the `Touched` set `jstream-graph` returns with it, from its edits and the inverse's
  (so a delete also names what it
  removed): objects created or deleted, objects used (an item's scene and source, both
  sources of an `item.set_source`, a `scene` source's scene, a duplicate's original,
  the scenes an `item.move` and its inverse name, the profile an `output.set_encoder`
  names), properties set (names, a scene's
  role, an item's placement, whether set as a transform or a geometry, crop,
  visibility, an item's index, which `item.move` also sets, which source an item
  places, each settings parameter, an output's target and encoder choice, a profile's
  encoding, each audio field a `source.set_audio` or `bus.set_fader` sets, a bus's
  name, a send's gain), the scenes'
  order, and the canvas size. A send is an object of its own, which uses its source
  and its bus, so changing two sources' sends into one bus doesn't conflict, and
  deleting a bus conflicts with every change to its sends. An undo or redo first applies the step's edits to a copy of the
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

### Program scene

The program scene is runtime state in `studio.rs`, under the collection lock beside the
outputs' state, not in the graph
([ADR-0030](../../docs/adr/0030-studio-state-and-scene-switching.md)). `scene.switch`
sets it (`not_found` for a scene the collection doesn't have) and emits
`studio.changed`, with no `rev` change; a switch to the scene already on program emits
nothing. After every applied batch, undo and redo the engine reconciles it: the collection's first
scene goes on program when it is created, a deleted program scene gives way to the
first scene left (or none), and a reorder never moves it. A scene's `role` changes none
of this: a group is a scene here, and can go on program. A batch, undo or redo that moves
program emits `studio.changed` right after its `state.changed`, naming the same request. The
mock applies the same rules, and the differential script holds the two to each other.
Program is not saved in the collection file, so an engine that loads one (a restart) starts
with its first scene on program. The program mix, on the render thread and in
`video.snapshot`, draws the program scene, and a switch re-plans it, so the next frame
shows the new scene: a cut. Only a cut exists; transitions and the preview scene come
later.

### Snapshots

`video.snapshot` returns the program mix as a PNG
([jstream-protocol › Design](../jstream-protocol/README.md#design)): the program
scene, read under the collection lock when the shot is copied, so a snapshot after a
`scene.switch` shows the new scene. An empty collection is an opaque black canvas.

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
one pass into its own layer (`Compositor::render_into`), which stays on the GPU as
premultiplied linear `Rgba16Float` (no sRGB round trip, no readback) and is kept for
the next frame while the program still nests it. A nested scene has no background yet, so it is transparent where it
draws nothing and the scene under it shows through; the program scene is drawn over
opaque black. A scene placed more than once at the same depth is composited once.
Depth is bounded: the program scene is depth 0, and a scene source more than 8
levels below it draws nothing (logged at `debug`). It is one limit,
`snapshot::MAX_NESTING`, for the render thread and `video.snapshot` alike, and ARCH ›
Data model states it for clients. Width is not bounded: each distinct `(scene, depth)`
keeps its own canvas-sized FP16 layer, so a program with *k* distinct nested scene
sources holds *k* such textures. The graph already rejects cycles at every
edit that adds nesting; if a copied shot had one, the scene source that closes it
draws nothing.

**The render snapshot.** `snapshot::plan` copies the program scene, the scenes it
reaches, the frames they place and the passes that draw them (`schedule`, made once
here) into an immutable `Shot`. The program scene is the one `scene.switch` put on
program (*Program scene*). The engine re-plans after every applied batch, undo
and redo, a `scene.switch` that moves program, every `set_source_frame`, a collection file's load, a live source's build
and a live source's next frame (see *Live sources*), under the collection lock, and
`RenderHub::publish` stores
it through `arc-swap` as the next render generation only when it draws something new
(the canvas, the scenes, their placements or the frames' versions differ). So a batch
that changes nothing on program, or a frame for a source no program scene shows,
publishes nothing. Each stored frame carries a version, so a cache re-uploads it only when it
changed. `video.snapshot` reads `rev` and the published snapshot together under the
lock and hands both to the snapshot thread.

`snapshot::compose` draws a `Shot` for both threads through a `jstream-render`
`ResourceCache` keyed by `GpuKey`: a source's upload, the capture's, a nested scene's
layer at its depth, and the program at the canvas size. An unchanged frame is not
uploaded again, and each nested scene is redrawn into the layer it had
(`render_into`), so a steady frame allocates no texture. Images no frame references
are retired and freed after the GPU has finished with them. The program scene is drawn
at the canvas: straight to the output when that is the canvas size (`video.snapshot`),
else into a layer that a last pass scales to the output (the render thread draws at the
encoder size). Without a program scene the output is the capture filling it, or black.
A `display` source with no stored frame shows the pipeline's capture on the render
thread and nothing in `video.snapshot`, which has no capture; a stored frame wins.
That mapping holds until per-source capture exists.

**Live sources.** `live::LiveSources` keeps one running `jstream-sources` `Source` per
source in the collection with a built-in kind, built from the registry
(`Registry::create`) with the source's settings. `sync` runs under the collection lock
after every applied batch, undo and redo and a collection file's load: it queues a
build for a source that is new or whose settings changed, and drops the instance of a
deleted one. Building reads and decodes files, so it runs on `jstream-sources`, a
thread the engine starts with its first build, never under the collection lock; the
thread then installs the source under the lock and re-plans. Each build is numbered,
and only the latest build asked for a source installs, so a slow build can't replace
a newer one. Until its build installs, a source keeps showing what it showed before,
or nothing. A changed setting always rebuilds the source, live or not, so an
animation restarts with it. A build that fails is logged at `warn` with the source ID
and an error that never names a host path, and the source draws nothing; a kind that
is only described (`NotBuilt`) draws nothing silently. Audio-only kinds
(`audio_input`, `desktop_audio`, `app_audio`, whose descriptors have no video) get no
`Source`: the program stage records them (*Audio sources*).

**Audio sources.** `audio_sources::AudioSources` holds, under the collection lock, the
`jstream_sources::AudioTarget` of every audio-only source, or none while the source's
audio is off (ADR-0028: off, the engine doesn't open its stream); `sync` runs beside the live
sources' and wakes the program stage's thread when that set changes. When the program
stage starts, its `AudioInputs` opens one mixer input per audio source that is on through
`MixerControl` (D1's live API). A source's input opens muted and is listed in
`audio_sources::Routed`, where the audio routing finds it (see *Audio routing*) and
sets it up as the collection says; the default microphone and desktop inputs send to
the master bus at unity, as every new input does. Each input is opened by asking the platform
for the format the device delivers (`AudioIo::input_format`), adding the mixer input
in that format, and opening the stream with the input's producer as its handler; a
stream that opens in another format is closed and tried again. An `audio_input`
with no device records the default input (`AudioInputTarget::DefaultInput`), which
the platform follows when the default changes; `desktop_audio` records the default
output's loopback; `app_audio` records process loopback of the running process whose
executable has the source's `exe` name, ignoring case (of several, one whose parent
is not one of them, lowest ID first).

The program stage's `jstream-audio-sources` thread follows the collection: when it
changes, and every 500 ms, it closes the inputs no longer wanted or wanted
differently, reopens one whose stream failed (`InputStats::failed`: a device
unplugged, or a followed default replaced by one in another format) or whose
application's process has exited (so a restarted application is found again under
its new ID), and opens every input that isn't recording. A source that can't be
recorded (a missing device, an application that isn't running, a refused format) is
unavailable: logged at `warn` once per reason, retried on every pass, and the program
runs without it; it never fails `output.start`. Closing an input stops its stream
first, then removes the input (`InputControl::remove`; the mixer frees the ring on its
collector thread, and a removal its full command queue refuses is retried on the next
pass). Each recording input registers an `AudioInputDrops` source while it records,
so its drops are latched when it closes. Opening a device never runs under the
collection lock; the thread takes only its own lock, the wanted set's and the drop
ledger's. When the program stage stops it joins the thread and closes every input
after the mixer thread has stopped. While the collection has no audio source, the
program records the default microphone and desktop audio, as it did before audio
sources existed, so an earlier collection is not silent; the first audio source
replaces them, and keeps them away while its audio is off. Whether a source is recording or unavailable is not reported to
clients yet: that needs a protocol field (D5's lane).

`snapshot::plan_with` asks for the frame of each source an item it reaches places:
the stored frame if `set_source_frame` gave one, else the live source's frame at the
current time, counted from when the source was built. A live frame is shared with
the source (an `Arc` of its `VideoFrame`, not a copy) and gets a new version only when
the source hands out a different one, so a still image is uploaded once. The plan also
returns the earliest time one of its frames changes (`Source::next_change`), and the
thread re-plans then, so an animated image advances on the clock and a still one is
never re-planned. Sources run on the media clock of the backends the host injects
(`with_backends`), else on a monotonic clock of the engine's own. A clock swap
restarts every running source's timing and drops the re-plan timed on the old clock:
the thread re-plans at once on the new one. `collection` asset
references resolve in the collection file's folder; without a collection file, and
for every `app` reference, the asset is unreadable and the source draws nothing.

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
The `output.start` that opens the program stage registers its sources (capture,
render, mixer inputs, mix bus) and starts a sampler thread, `jstream-stats`, that
calls [`Engine::emit_stats_tick`](src/engine.rs) about once a second. Each encoder
session registers its two sources (input and subscriber drops) when it opens, and each
output its own (muxer, stop drain, publish link) when it starts, so a tick sums every running
output. The sampler is not a real-time thread. Removing a source latches its last
totals so the next tick does not shrink. When the last output stops, the program
stage joins the sampler and emits one final tick, so the run's last drops are
reported when it stops (before `stopped`, or after `failed`). `seq` starts at 1.
Registering two sources for the same drop adds it twice.

A source reports running totals. The tick is the sum, so a client subtracts the previous
tick.

What each stage contributes:

| Stage | Read from | Fields |
|---|---|---|
| Capture mailbox | [`CaptureStats::frames_skipped`](../jstream-platform/src/capture.rs), published by the render thread | `capture_frames` |
| Render loop | ticks skipped to catch up with the media clock, published by the render thread | `render_ticks` |
| Mixer input | overrun and late frames, each input while it records | `audio_frames` |
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

### Audio routing (`meters.tick`)

[ADR-0028](../../docs/adr/0028-audio-model-v2.md)'s mixer lives in the program stage,
so the collection's audio reaches it only while an output runs. When the program
stage opens, it hands the engine its mixer's controls (`MixerControl`, the master's
`BusControl`) and the master meters, and the engine routes the collection into it
(`audio.rs`); when the stage stops, it detaches them again. Each attach is numbered,
so a stage that stops after another has opened detaches only its own. After every
applied batch, undo and redo, and on attach and collection load, `AudioRouting::sync`
brings the mixer in line with the collection under the collection lock:
- each bus other than the master gets a mixer bus (`MixerControl::add_bus`), and a
  mixer bus whose collection bus is gone is removed; a bus the mixer refuses to add or
  remove is retried on the next sync (a refused add is logged);
- every bus's fader and mute, the master's on `Mixer::bus`;
- for each source with a mixer input, its fader and a send into each mixer bus as the
  collection's sends say, removing the others. A `muted` source keeps its sends at
  silence, so its post-fader meter still reads, as ADR-0028's mute requires. A source
  whose audio is off has no input, since the program stage closes it; until it has,
  the routing mutes it and leaves it out of `meters.tick`. The
  inputs are the ones the program stage's audio sources thread lists in `Routed` as it
  opens and closes them (see *Audio sources*); it removes them from the mixer itself.
  An input it opened since the last sync is routed on the next `meters.tick` at the
  latest, and stays muted until then. Every control is a few atomics, so a sync never
  waits on the mixer thread ([R-RT-003](../../docs/RULES.md#R-RT-003)), and the
  `Routed` lock is the last one taken. The program stage's microphone and desktop
  inputs are not sources and keep sending to the master at unity.
- each such input's activity gain (`activity.rs`, ADR-0028's "shown"): 1 for a source
  that follows `always`; for a `when_shown` one, `min(1, Σ w)` over the roots that
  reach it through visible items and nested scene sources whose audio is on. The roots
  are the program scene (weight 1) and every nested scene source whose audio is on and
  follows `always` (weight 1 each). The mixer applies it after the input's fader and
  meter, scaling every send, and ramps to it over one block, so a cut never clicks
  and the meter still reads a source that is off program. `scene.switch` and a batch
  that moves program sync too, after program is reconciled.

The `jstream-stats` sampler also emits `meters.tick` on each of its 100 ms wakes: a
reading of every routed source's meter (if its audio is on) and every bus's, peak (held since the last
reading) and RMS, linear per channel. The master adds its true peak and BS.1770
loudness, which the mixer thread measures on every block it mixes (`MasterMetering`,
whose meters are built before the thread starts, since the loudness meter allocates,
and never allocate after) and publishes through atomics (`MasterMeters`). An
`audio.reset_loudness` sets a flag the mixer thread reads before its next block, which
restarts the integrated loudness. With no program stage running, no `meters.tick` is
emitted and `audio.reset_loudness` does nothing.

### Outputs

An output's configuration is collection state (`output.create`, `output.set_target`,
`output.delete`, kept by `jstream-graph`); whether it runs is runtime state, which never
changes `rev`. After each applied batch the runtime states follow the collection: a
created output starts `stopped`, and a deleted one is forgotten. The graph refuses to
delete a running output (`invalid_argument`), so the batch passes it the running set.
A host can also declare an output with `Engine::with_output(id)`, outside the
collection: it starts `stopped`, has no target and stays known whatever the collection
holds. A live host reads a collection output's target with `output_kind` when its
publish opener runs, as `jstream-daemon` does on Windows
([jstream-daemon › Design](../../apps/jstream-daemon/README.md#design)). A recording output
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

**Encoder profiles.** An output's `encoder_profile` (collection state, set with
`output.set_encoder`) picks the encoding its next start opens, or the protocol's
`EncoderProfile::DEFAULT` when it has none. Running outputs share an encoder session
when they have the same key: the profile ID and its settings as they were at start
(the mix is always program until video mixes exist). So `encoder.update` or
`output.set_encoder` on a running output changes nothing until it restarts, and an
output started after the update opens a session of its own rather than joining one
with stale settings. `output.status` reports, per running output, the profile, the
other outputs on the same session and whether its video encoder is a hardware one,
plus the video sessions and hardware sessions open
([ADR-0027](../../docs/adr/0027-program-master-and-output-chains.md)); it does not
guess a hardware cap. A hardware session limit fails the start with the encoder's own
error for now, not a session-limit code.

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
An opener builds its own errors with `PublishError::new`.
Its error text must not carry a stream key
([R-SEC-001](../../docs/RULES.md#R-SEC-001)); `connect_rtmp`'s does not. Starts run one at a time (a second waits for the first to settle; a stop never
waits for a start). The call checks the output is idle,
takes the publish target, records an in-flight generation, emits `starting`, and
drops the collection lock before it opens devices or joins anything. When no other
output is running it opens the program stage and waits,
up to 10 s, for the render thread to open the software compositor; that bound only
keeps a stuck adapter from blocking the call, and the GPU tests raise it because
lavapipe can open slower than that on a loaded CI runner. When others are running it shares
theirs. It then gets the encoder session for its key (opening one if no running
output has it), opens its sink, subscribes to the session and starts its output
thread, and returns once that thread has started the sink. Success emits `active`.
Failure emits `failed` and `action_failed`, and drops a one-shot publish target so
the host sets another before retrying (an opener is kept); the other outputs keep
running. `output.stop` on an `active` output emits `stopping`, drops its pipeline
outside the lock (the program stage and an encoder session stop with the last output
on them) (its drop sets the stop flag and joins the threads; the output
thread's drain, and the wait for it, are bounded, see the RTMP paragraph below), then emits
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
Until the start has succeeded, the recording is provisional: a start that fails
after opening the file removes the `.part` file, so it doesn't block the next start,
and so does one whose init segment can't be written (`DiskFull` or `Write` from
`Recorder::create`).
Once `active`, the `.part` file is kept whatever happens: a write error (a full disk
among them) settles `failed` and leaves it readable up to its last fragment, and a
failed rename at stop is logged with the `.part` path. A packet the muxer rejects is
dropped and counted in `output_packets`. Disk writes run on the output thread and
may block; the encoder subscriber queues absorb that and drop when full, without
holding up a stream on the same session.

**Virtual camera.** `with_backends` takes the backends' `VcamSink` out and keeps it for
the engine's life. When the collection's output is `VirtualCam`, the start takes that
sink and joins the program stage like any output (opening it when no other output
runs), but with no encoder session (`launch_vcam`, `runtime/vcam.rs`). With no sink it
fails with `action_failed` before `starting`: `this engine has no virtual camera`, or
`the virtual camera is in use by another output` when another `VirtualCam` output has
it. It starts the camera at the canvas size the collection has then, as BGRA8 at
1/30 s (a refusal fails the start with `starting the virtual camera: <reason>`), hands
the render thread a tap, and starts the output's camera thread, `jstream-vcam`. The
tap and that thread share only the output's stop flag and two bounded channels, never
a lock ([R-RT-003](../../docs/RULES.md#R-RT-003)). Each tick the tap copies the
program frame (RGBA) into a buffer the camera thread handed back and sends it,
without blocking: with a frame still waiting, it drops this one and counts it in
`output_stats`' output drops. The camera thread converts each frame to BGRA at the
canvas size (as is when the program frame is the canvas size, otherwise fitted inside
it, centred, by nearest neighbour, with black bars), publishes it and hands the
buffer back. It runs beside streams and recordings on the same program frame, and
`output_stats` counts no encoder input for it. A publish error fails that output
alone, through its own failure line; the others keep running. Once the camera thread
ends (a stop, or a failed publish) it stops the camera and returns the sink to the
engine, as a start that fails does, so the next start finds it. A stop sets the flag
and waits for that as it waits for any output thread (`STOP_DRAIN` plus the grace);
the render thread never waits on it, and the tap leaves on its next tick, to the
collector. A canvas resize applies from the next start. The daemon's backends don't
carry `WindowsVcam` yet; that wiring is the daemon's.

Once the pipeline is `active`, a fatal error on any pipeline thread is sent on a
channel: a `send_tag` or `poll` failure, a recording write failure, an encoder stream that ends before the
pipeline was told to stop (the encoder failed, or its session closed), and the
render and mixer failures below. A render or mixer failure goes to every output on
the program stage; an output's own failure (its sink, its encoder session) only to the
outputs it reaches.
A watcher the control thread started settles `failed`, emits `output.state_changed`,
and drops the output's pipeline; the last output to go stops the render and mixer
threads. The watcher holds weak
pointers between messages, so it does not keep the engine alive: dropping
the last engine handle and its sessions joins the pipeline. `output_stats` is then
empty. `failed` counts as idle, and a later start needs a new one-shot target or an opener. That
start waits until a failed program stage has finished joining, so the new devices do
not overlap the old teardown; it never joins a program stage that has failed. The same holds when a
start fails after `launch` returned threads (a failure reported before `active`, or
the watcher not starting): `failed` is emitted, and the next start waits until that
pipeline has joined. A stop's join that takes the program stage down also holds off
a start of another output; one that leaves it running does not. The
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

The render thread reads the platform capture mailbox, composites the program mix of
the render snapshot (see *Snapshots*) at 30 frames per second, repeats the last
capture when the mailbox is empty, and reads the composite back as RGBA8 because this
cut has no GPU encode target. Each tick first calls `RenderReader::adopt`, which
compares one pointer with the published snapshot and, when a new one arrived, adopts
it: it stores its generation in the hub's reader slot (one `SeqCst` store) and hands
the snapshot it held to the program stage's `jstream-collector` thread. The GPU images the
cache frees go there too, and so does the input of an encoder session that has
finished (the session may already be gone, leaving that input the last reference to
its shared state and channel), and so does a virtual camera's tap that leaves (once
its camera thread has ended it holds the last reference to the channels and the stop
flag), so nothing the render thread lets go of is freed on it
([R-RT-003](../../docs/RULES.md#R-RT-003)). The mixer thread hands a finished audio
input to the same collector. The thread clears the stopped marker
before its first load (`SeqCst`, then a fence), and publishes it again when it stops
or a fatal error stops its drawing, so `RenderHub::caught_up` treats it as caught up
([ARCH › Threads](../../docs/ARCHITECTURE.md#threading), "Live-value handover"; only
tests poll it until live edits exist). The collector ends when the render and mixer
threads have dropped their senders, and the program stage (`Program`) joins it right
after them. While the stage has no encoder session and no virtual camera it draws nothing but still adopts
each new snapshot, so it never holds a gesture up. The compositor
and its cache drop on the render thread when it exits, after the tick loop. What it rides out
and what is fatal ([R-RT-006](../../docs/RULES.md#R-RT-006)):
- **Recoverable:** a capture error or a missing frame repeats the last capture (a
  missing source frame, [ARCH › Threads](../../docs/ARCHITECTURE.md#threading)); a
  composite error repeats the last composited frame, up to 30 in a row.
- **Fatal, reported:** a 31st composite error in a row, a panic inside a tick (the
  compositor's state is unknown after it), and a frame time that overflows. The
  thread then keeps pushing the last frame until the pipeline stops, so the stream
  does not freeze before the teardown. A virtual camera's publish error is not
  the render thread's: it fails only that output, which leaves the stage.
- The render and mixer threads both catch a panic at their boundary. A fatal error
  becomes `output.state_changed` `failed`, the error event this protocol has. A
  mixer panic, a rejected mix block
  or a mixer poll error is fatal: the encoder would get no more audio.

The render thread reuses its capture, composite and readback buffers, and its GPU
textures through the cache. Each tick allocates one frame-sized buffer, the frame the
encoder session takes ownership of (`VideoInput::push` cannot hand buffers back), the
item list `compose` fills once per tick, the frame-fence callback, and the
premultiplied copy of a capture that changed. A virtual camera's tap reuses the
buffers its camera thread hands back, so it allocates only until they circulate; the
camera thread allocates the BGRA frame it publishes, one per frame, off the render
thread. The mixer likewise copies each block
into the buffer `AudioInput::push` keeps. Not measured. The mixer thread
polls the audio sources' inputs into 48 kHz stereo blocks. Encode
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

A stop bounds its drain. The stop fixes a deadline 2 s (`STOP_DRAIN`) after it
begins and raises the stop flag; the output thread, once it sees the flag, sends what
the encoders queued until then and tells the target that deadline
(`LivePublish::stop_by`; the default does nothing). Once it passes, or a send fails
while stopping, the thread drops the packets still queued, counts them in
`output_packets`, and calls `finish`. A failure while stopping is logged, not
reported: the output is already on its way to `stopped`. The stop does not count on
the target honoring the deadline (#138): it waits for a publishing output thread
only until the deadline plus 500 ms (`OUTPUT_GRACE`), then returns without it and
closes that thread's line to the publish watcher, so a send blocked in a target that
ignores `stop_by` cannot hold `output.stop` either. The thread is detached and ends
once that send returns; what it drops after that is not in the final `stats.tick`.
A recording's output thread is always joined, so its file is renamed into place
when the stop returns. `RtmpPublish` honors the deadline: every socket write's
timeout is cut to the time left (`Transport::set_write_deadline`), so a peer that
reads a trickle cannot stretch one large tag past it, a lost link is not
reconnected, and past the deadline sends fail and `finish` drops the connection
without the `FCUnpublish` … `deleteStream` goodbye. So a peer that has stopped
reading holds `output.stop` for about 2 s, not 10 s per blocked write.
`connect_rtmp`'s unit tests drive fake servers on loopback: a slow one, one that
hangs up mid-stream, one that stops reading while the deadline runs out, and one
that reads only a trickle of a 12 MiB tag.

### Locking

`std::sync::Mutex` locks, always taken in one order: a session's identity lock (held
by `hello` while it registers the client), then the start lock (held by
`output.start` for the whole start, so starts run one at a time; nothing else takes
it), then the engine's collection lock, then the pipeline gate, then a subscription
queue's. The program stage's encoder session registry and failure fan-out are leaf
locks, taken with no engine lock held; the render and mixer threads take neither. The
drop ledger is its own mutex, and so is the audit sink's handle, which is only cloned
under it (see *Audit log*). The audio sources' wanted set is a leaf mutex the control
side takes under the collection lock; the program stage's `jstream-audio-sources`
thread takes it alone, and otherwise only its own inputs mutex and, under that, the
drop ledger's, never the collection lock (see *Audio sources*).
The sampler and `output.start` take it only to read or register sources, and they
release it before taking the collection lock. A source must not call back into the
engine while that mutex is held. A stopping program stage detaches its mixer from the
audio routing under the collection lock, which is why it is never dropped while that
lock is held, as its final `stats.tick` already requires. The master's meters are
atomics the mixer thread writes and the sampler reads, with no lock. The snapshot compositor belongs to the snapshot
thread, which takes no engine lock; the queue to it is taken only after the
collection lock is released. The render snapshot is published under the collection
lock (which makes the control side its one writer) and loaded by the render thread
with no lock at all. `video.snapshot` reads it under the collection lock and the
snapshot thread only receives it. A stream's poll takes only its queue's lock. No lock
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
`history.undo`, `history.redo` or the action's name), a batch's edits by name in order, an action's `output_id` or `scene_id` (`scene.switch`), the `rev` before and
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

`jstream-daemon` injects an `AuditFile` in its log directory
([jstream-daemon › Design](../../apps/jstream-daemon/README.md#design)).

### Collection file

`Engine::with_collection_file(path)` loads the collection from `path`
(`jstream-graph`'s `Graph::with_collection_json`, checked against the engine's kinds)
and replaces the engine's collection and `rev` with it; the outputs' runtime state
follows, so a loaded output is `stopped`. With no file at `path` it saves the current,
empty, collection there at once, so an unwritable path fails at startup rather than at
the first change. A file that doesn't read or load is refused with
`CollectionFileError` (`Read`, `Load` or `Create`, each naming the path but never the
contents) and left as it is. It is configured before `connect`, like the other
builders.

After each applied batch, undo or redo, `Engine::execute` saves the collection before
the call returns, so the client whose call it was knows the file holds its `rev`. The save
takes the collection file's mutex, then the collection lock only long enough to
serialize the graph, and writes with only the file's mutex held: saves happen one at a
time, never under the collection lock, and one whose `rev` the file already has (or
passed, because a later change saved first) writes nothing. The write is atomic: a
temporary `.<name>.<pid>.tmp` beside the file, flushed to disk, renamed over it, and on
Unix the directory flushed too; on Unix it is mode `0600`. A failed save is logged at
`error` with the path and `rev`, removes the temporary file, and leaves the call's
result as it is: the change stays applied and the next change saves again. Undo
history, idempotency keys and the program scene are not saved (see *Program scene*).

`jstream-daemon`'s `--collection` option turns this on
([jstream-daemon › Design](../../apps/jstream-daemon/README.md#design)).

### Not yet

- **Live values** for continuous gestures (`live.set`, `live.cancel` and their events,
  [ADR-0032](../../docs/adr/0032-live-edits-and-undo.md)), which need the real-time
  parameter channels.
- An `app` asset folder for live sources.
- Reporting an audio source that is unavailable to clients (it is only logged), and
  listing the devices and applications an audio source can record in
  `kind.describe`: an `Enum` of them would make a collection naming an unplugged
  device fail validation, so it needs a query, which takes a protocol queue slot.
- Whether the default microphone and desktop audio stay as implicit inputs of a
  collection with no audio source once gain, mute and routing reach the protocol
  (D5): ADR-0028 models every mixer input as a source with sends, and these two have
  none.
- Reporting a source that failed to build to clients (it is only logged), and
  updating a source in place for a change to a `live` setting instead of rebuilding
  it.
- Activity gains that fade with a transition (T1: only cuts exist, so every weight
  is 0 or 1), filter
  chains in the protocol (the mixer has them since D7b; the routing sets none yet), and meters for every bus's true peak and loudness.
- The audio and outputs snapshots, the parameter channels and gesture release of the
  live-value handover, and a GPU encode target. `output.start` is a cut of
  [ARCH › Threads](../../docs/ARCHITECTURE.md#threading): the program mix with one
  captured display, a CPU readback into the encoder, and the platform fakes in the
  test. Other sources show frames the host injects with `set_source_frame`.

<a id="runtime"></a>
## Runtime
<!-- @section
covers = ["crates/jstream-core/src/runtime/**"]
depends_on = ["docs/ARCHITECTURE.md#threading", "docs/RULES.md#R-RT-006"]
-->

The output pipeline (`src/runtime/`) is the part of
[ARCH › Threads, clock and sync](../../docs/ARCHITECTURE.md#threading) that is built.
`output.start`, once the host has injected platform backends and a publish target
(a recording output or a virtual camera needs none), starts it and emits `output.state_changed` from the
control thread. The render thread calls the platform capture mailbox and, on each
1/30 s tick, composites the program mix from the render snapshot (see *Snapshots* in
Design) at the encoder size: the program scene and the scenes it nests, with every
`display` source that has no stored frame showing the captured display. With no scene
it draws the capture filling the frame. It repeats the last capture when the mailbox
is empty, and reads the frame back. This cut encodes that CPU readback, and the
readback is counted. The mixer thread polls the media clock and pushes 48 kHz stereo
blocks mixed from the audio sources' inputs (see *Audio sources* in Design; the
microphone and the desktop loopback while the collection has none), and meters each
master block's true peak and loudness. Once the stage is up, the engine routes the
collection's buses, faders and sends into its mixer until it stops (see *Audio
routing* in Design). Every running output shares that one program stage (capture,
render, mixer, audio inputs); the first `output.start` builds it and the
last `output.stop` tears it down, and starts are serialized. Encode threads are the
ones inside the encoder sessions: outputs with the same encoder profile share one
session, and the render and mixer threads take a new session's inputs from a channel,
never a lock, and hand a finished session's input to the collector rather than drop
it. While the stage has no session and no virtual camera it
draws nothing but still adopts the latest render snapshot. Each output but a virtual
camera has its own output thread that muxes; a virtual camera's thread converts and
publishes what the render thread sends it through a tap, which the render thread takes
from a channel like a session's input and hands to the collector when it leaves. An output leaving a session others still use takes what is queued for
it and goes; the last one drains the encoders. The output thread muxes FLV and sends
tags to the publish target, or, for a recording output, muxes fragmented MP4 and
writes it to the file, syncing each fragment (writes may block on the disk; the
encoder queues absorb that and drop when full). The `output.start` that opens the
program stage also starts `jstream-stats`, which is not a real-time thread: it emits
`stats.tick` about once a second from the stage counters, and once more when the last
output stops, and `meters.tick` about ten times a second. The render loop publishes capture mailbox skips, and a tick that falls a
whole period behind the clock skips to the current tick and counts the skipped ones in
`render_ticks`. None of those threads lock the collection: the render thread reads the
render snapshot, and what the render and mixer threads let go of (replaced snapshots,
GPU images, finished sessions' inputs) goes to the program stage's
`jstream-collector` thread. `output.stop` during `starting` cancels that start and
emits `stopped` only after the start has dropped its threads. The render and mixer
threads catch a panic at their boundary ([R-RT-006](../../docs/RULES.md#R-RT-006)); a
capture error or a missing frame repeats the last one, and a render panic repeats the
last frame until teardown. A fatal error on the render, mixer or output thread (a
publish or recording write failure, an encoder stream that ends before stop, a render
or mixer panic) is a channel message to a watcher the control thread started; the
watcher emits `failed` and stops the pipeline. A program-stage failure reaches every
running output. Frame hand-off is the encoder session queues. The audio and outputs
snapshots and a GPU encode target are still rows of the ARCH thread table, not this
cut. The mixer also has its own collector, which frees input rings and their counters,
removed buses' state, and removed filter chains and filters. The program stage's
`jstream-audio-sources` thread opens and closes the audio sources' inputs as the
collection changes (see *Audio sources* in Design); it takes no engine lock and is
joined, and every input closed, after the mixer thread stops.
