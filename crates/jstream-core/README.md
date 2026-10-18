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
  `with_publish_opener`, `with_key_store`, `with_youtube`, `with_audit`,
  `with_history_limit`, `with_browser` (the browser engine `browser` sources and their
  `youtube_chat` preset open pages in) and `with_collection_file`; `scan_clap_plugins`
  adds the CLAP filter kinds;
  `connect()` opens a session; `rev()`; `set_source_frame` stores the latest RGBA8
  frame of a source; `output_stats` reads a running output's pipeline counters
  (`output_stats_of` names the output); `output_kind` reads a
  collection output's target. `output.set_key` and `output.clear_key` go to the
  injected `StreamKeyStore`, never under the collection lock, and so does the removal
  of a key whose output a batch, undo or redo deleted or moved to another server
  (*Stream keys* below). It holds the
  `jstream-graph` `Graph`, the outputs, those frames, the idempotency keys, the undo
  history, the open gestures and the subscribers behind one lock, and the drop ledger behind another, and executes
  batches, history calls, actions and queries. The pipeline threads do not take the collection lock.
- **YouTube** (`youtube.rs`, [ADR-0058](../../docs/adr/0058-youtube-sign-in-with-a-jstream-client.md)):
  `with_youtube(YoutubeSetup)` gives the engine a Google OAuth client, the HTTPS
  transport and the `OAuthTokenStore`. `YoutubeSetup::built_in` uses JStream's built-in
  client (`jstream_youtube::BUILT_IN_CLIENT_ID`, ADR-0058), and is `None` while that is
  empty, as it is until the maintainer fills it in; `YoutubeSetup::new` takes another
  client, for a developer override. It holds one `jstream-youtube` `LiveClient`
  (one account, its refresh token stored under the fixed opaque ID `TOKEN_ACCOUNT`, so
  no email or other personal detail names the credential) and runs the `youtube.*`
  actions and queries with no engine lock held. `youtube.sign_in` asks for the full
  `youtube` scope alone, returns the consent URL, and finishes on its own thread when
  the loopback redirect arrives (or after `SIGN_IN_TIMEOUT`). Starting an output with
  a `youtube` target reads the broadcast's bound stream and connects to its `rtmps://`
  ingest with its key, which goes only to the connection; while it runs, a thread reads
  the stream's health every `HEALTH_INTERVAL` and emits `youtube.health_changed` when
  it changes. A refresh token Google refuses signs the engine out.
- **Audit log** (`audit.rs`): one `AuditEntry` per executed batch, undo, redo and action
  (but `live.set` and `live.cancel`), and one per gesture that ends without a commit, sent to
  the `AuditSink` the host injects with `with_audit` (none by default). `AuditMemory`
  keeps entries in memory; `AuditFile` writes them as JSON lines to a size-capped
  `audit.jsonl` from its own writer thread (see *Audit log* in [Design](#design)).
- **Collection file** (`collection.rs`): loads the collection from the file the host
  names with `with_collection_file` (or creates it), and saves it atomically after every
  change (see *Collection file* in [Design](#design)).
- **Kinds** (`kinds.rs`): the built-in kinds the graph validates settings
  against and `kind.list` reports ([ADR-0025](../../docs/adr/0025-typed-settings-kind-descriptors.md)):
  the source kinds of `jstream-sources`' built-in registry and the audio filter kinds.
- **Audio filters** (`filters.rs`, [ADR-0026](../../docs/adr/0026-source-filter-chains.md)):
  the `audio_filter` kinds `gain`, `noise_gate`, `compressor`, `limiter` and `delay`,
  one per `jstream-audio` kernel, the mapping from a filter's settings to the kernel's
  `FilterSpec` and `FilterParams`, and `Chain`, which keeps one mixer input's, the
  master's or an output tap's filters in line with its owner's audio chain.
- **CLAP plugins** (`plugins/`, [ADR-0035](../../docs/adr/0035-clap-plugins-out-of-process.md)):
  - `Engine::scan_clap_plugins(&dyn PluginHosts)` scans the CLAP search directories
    (recursively, for `.clap` entries, each scanned in its own host process) and adds
    every audio effect found to the graph's kinds as an audio-filter kind,
    `clap/<plugin id>` (`CLAP_KIND_PREFIX`), so `kind.list` and `kind.describe` report
    it. Its settings are the plugin's parameters, by CLAP id in decimal, all live: a
    stepped one is an `int`, any other a `float`, both with the plugin's range; hidden,
    read-only and bypass parameters and ones without a finite range are left out. When
    two files offer one plugin id, the first found wins. The scan returns a `ClapScan`
    with each kind's file and plugin id, and the files that failed. Kinds stay until
    the engine ends. It is slow, so a host calls it on a worker thread; the engine's
    lock is held only to add the kinds.
  - `HostSupervisor` keeps one CLAP filter instance's host process running, on a
    control thread, driven by `tick(now)`, which returns `HostEvent`s. A host is
    `Ready` (with its block and latency) once its block says so. One that exits, isn't
    ready within `start_timeout` (it may load the plugin without beating until then),
    or, once ready, stops its heartbeat for `LIVENESS_MS` has `Crashed`: it is killed and restarted after a wait that doubles from `first_retry`
    up to `max_retry`. After `max_crashes` crashes with no stable run (`stable_after`)
    between them, or at once when the host marks a failure a restart can't fix (load,
    not an effect, activate, configuration), it is `Disabled` until `enable`. The
    supervisor asks for the plugin's state whenever the plugin marks it changed, keeps
    the latest (`saved_state`, `StateSaved`) and restarts with it. A run of
    `degraded_after` consecutive late blocks in the mixer is `Degraded`, and its end
    `Recovered`. `stop` asks the host to stop and kills it after `STOP_GRACE`.
  - **Not yet:** running a CLAP filter. The engine refuses a `filter.add` of a `clap/`
    kind (`invalid_argument` on its `kind`), since nothing would run it: the mixer's
    filter chain skips every kind that isn't a built-in audio filter. A collection
    holding one also wouldn't load at the next start, because the file is read before
    the scan describes the kind. So nothing hosts a CLAP filter, keeps its saved state,
    runs a supervisor or reports its events. Also not yet: the plugin's editor, and
    rescanning.
- **Live sources** (`live.rs`): a running `jstream-sources` source for every source
  in the collection, built when it is created, rebuilt when its settings change and
  dropped when it is deleted, on the engine's `jstream-sources` thread. The program
  mix shows their frames and advances animations and videos on the media clock, and
  tells a source when it shows it again. `media` sources play through the injected
  backends' media files. A frame stored
  with `set_source_frame` wins over the live one, and a kind that is only described
  draws nothing. A `display` or `window` source shows what the output pipeline
  captures for its `target` on the render thread, and a `webcam` source what it
  captures from its `device` (see *Live sources* and *Snapshots* in
  [Design](#design)).
  Audio-only kinds make no frames and have no live source. The `waveform` and
  `spectrum` sources draw the program mix's audio, a bus's, or another source's, from
  the engine's `AudioScopes`.
- **Audio sources** (`audio_sources.rs`): while the program stage runs, one mixer
  input for each `audio_input`, `desktop_audio` and `app_audio` source whose audio is
  on, added when the source is created or turned on and removed when it is deleted or
  turned off; one for each running source with audio of its own whose audio is on, a
  `browser` page's; and one for each live source with sound of its own whose audio is
  on, a `media` file's, fed while the program shows it. Each runs the source's enabled
  audio filters and is listed for the audio routing, which sets its fader, mute and
  sends. While there is no audio-only source, the default microphone and desktop audio
  are recorded too, sent to master at unity (see *Audio sources* in
  [Design](#design)).
- **Snapshot** (`snapshot.rs`): the program mix and the video mixes as the render
  thread draws them, and the program mix as the preview thread and `video.snapshot`
  draw it (`plan`, `compose`, each with the live values), and `video.snapshot`, the program mix
  read back as a PNG on the engine's own snapshot thread, which owns the snapshot
  compositor.
- **Studio** (`studio.rs`): which scene is on program and which on preview, and the
  transition running onto program, runtime state under the collection lock that
  `scene.switch` and `studio.set_preview` set and `studio.get` reads
  ([ADR-0030](../../docs/adr/0030-studio-state-and-scene-switching.md)).
- **Preview** (`preview/`): `preview.subscribe` and `preview.unsubscribe`, the program
  mix as small pictures sent to the session that asked as binary frames
  (`Control::binary`), in H.264 on a software `jstream-encode` session when the
  platform has an encoder and in JPEG otherwise (`preview/jpeg.rs`, a baseline
  encoder of its own), drawn on the engine's `jstream-preview` thread with its own
  compositor (see *Preview streams* in [Design](#design)).
- **Transitions** (`transition.rs`, [ADR-0029](../../docs/adr/0029-transitions.md)):
  which preset a switch runs (`resolve`), and the plan that pairs what is on screen
  with the new scene's items, one `jstream-render` `Track` per item (`plan`).
- **Privacy** (`privacy.rs`): whether the privacy screen is on and which client turned
  it on, runtime state under the collection lock that `privacy.set` sets and
  `privacy.get` reads, with the atomic flag the render thread reads
  ([ADR-0027](../../docs/adr/0027-program-master-and-output-chains.md)).
- **Runtime** (`runtime/`): the `output.start` pipeline. Every running output shares
  one program stage: the render thread reads the platform capture mailbox of each
  display, window and webcam the program mix or a video mix shows
  (`runtime/captures.rs`, which starts and stops them on a `jstream-capture` thread),
  composites each video
  mix the running outputs show, on its own ticks, from the render snapshot the control
  side publishes (`runtime/render.rs`) and reads the frames back, and the mixer thread
  polls the media clock. The render thread composites on the platform's GPU context
  when the interop opens one, and samples the captured textures there with no CPU copy
  when the capture backend can deliver them; otherwise it uses CPU frames, and its own
  device without a context. If compositing keeps failing on the context mid-run, it
  moves to CPU frames on its own device (see *Runtime*). Encode threads live in the encoder
  sessions: one video session per video mix and encoder profile in use, shared by the
  outputs on that pair, one audio session of the master bus per such pair, shared by
  its outputs with no audio chain, and one per output with an audio chain. Each
  output's own thread muxes FLV and publishes it, or muxes MP4 and records it
  ([ARCH › Threads](../../docs/ARCHITECTURE.md#threading)). A virtual camera's
  thread converts and publishes the frames the render thread sends it through a tap
  (`runtime/vcam.rs`).
- **Audio** (`audio.rs`, [ADR-0028](../../docs/adr/0028-audio-model-v2.md)): the
  program mixer follows the collection's audio. While the program stage runs, every bus
  of the collection has a mixer bus with its fader and mute, and every source with a
  mixer input has its fader and its sends (silenced while it is `muted`), scaled by
  its activity gain (`activity.rs`): a `when_shown` source is heard only while the
  Program mix shows it. The master stage's audio chain runs on the mixer's master
  chain, and an output with an audio chain has an output tap running it
  ([ADR-0027](../../docs/adr/0027-program-master-and-output-chains.md));
  `meters.tick` carries their levels about ten times a second, with the master's true
  peak and BS.1770 loudness, which the mixer thread measures on each block, and
  `audio.reset_loudness` restarts its integrated loudness. Every bus a source's
  settings draw (#362) has a scope the mixer thread feeds from that mixer bus, and
  every source a scope source's `audio_source` draws (#459) one it feeds from that
  source's mixer input.
- **History** (`history.rs`): per-client undo and redo
  ([ADR-0032](../../docs/adr/0032-live-edits-and-undo.md)): each client's undo and redo
  stacks, built from the inverse batches `Graph::apply` returns, and the log of what
  each recent change touched, which `undo_conflict` is checked against.
- **Gestures** (`gesture.rs`): live values
  ([ADR-0032](../../docs/adr/0032-live-edits-and-undo.md)): every client's open
  gestures, which one holds each parameter, the `live.changed` events each owes, and
  the values of committed gestures the render thread has not caught up with (see *Live
  values* in [Design](#design)).
- **Session** (`session.rs`): `InProcessControl`, one client session. Its undo history
  ends with it, and its open gestures expire.
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
  drain abandons), or the stop drops for it when it leaves that thread behind, and
  the publish link's, and a sampler thread
  emits a tick about once a second until the last output stops. The same thread emits
  `meters.tick` about ten times a second.
- **Tests** (`tests.rs`, `src/tests/history.rs`, `src/tests/live_values.rs`,
  `src/tests/audio.rs`, `src/tests/audio_routing.rs`, `src/tests/sources.rs`,
  `src/tests/transitions.rs`, `tests/mcp.rs`,
  `tests/scenarios.rs`):
  the protocol rules through `InProcessControl`, undo and redo across two clients
  (conflicts, `expected_rev`, idempotency, the limit, a running output, a session's
  end, folders and labels), live values (no `rev` or collection change, a commit as one `rev` and one undo
  step with its events in order, values held until a render reader adopts the
  commit, a failed commit, cancel and expiry with their audit entries, validation and
  limits, takeover, a deleted item, coalesced `live.changed`) and a live value drawn
  over the shot in `snapshot.rs`, the shared parity fixture, a differential run against
  `jstream-control`'s mock, many sessions on many threads, a headless
  `video.snapshot` on whatever adapter opens (the software one in CI), nested-scene scheduling (order, depth
  bound, cycles) and its pixels through two levels in `snapshot.rs`, the render
  thread's output size (the primary display's capture filling the canvas with no
  scene, the program fitted from the canvas, a `display` source showing its capture,
  the same for a GPU capture of another size), a mix of another aspect ratio
  letterboxed and a `scene` mix drawing its own scene, the scene each mix plans and the
  captures only a mix shows, the program stage's output size following the canvas,
  captures converted to RGBA8 at
  their own size (NV12 with its matrix and range), and nested-scene layers reused
  frame after frame then freed after the fence, render-snapshot generations, the
  reader slot and the collector in `runtime/render.rs`, the render thread adopting a
  published snapshot (CPU stand-in), an output starting a capture for each display and
  window on program, sharing one between sources, stopping it when its source leaves
  and starting it again after a failed start or a lost target, and opening each webcam
  on program from the fake video devices, shared between sources, closed when it leaves
  or the output stops, and closed when unplugged with the output staying up
  (`src/tests/cpu_pipeline/captures.rs`), the format a webcam opens in, with and
  without an MJPEG decoder, and its frames, decoded or not, read as RGBA
  (`runtime/captures.rs`), which capture path an output takes and each
  fallback, an output compositing GPU capture frames (opaque, whatever alpha the
  texture holds), a browser page painting on
  the stage's context, composited as premultiplied red and given back the device when
  the stage stops, and an output leaving the GPU path mid-run when every GPU frame
  fails to import or the context breaks (taking the context from browser pages, and
  failing when that fallback fails, or at its deadline when it hangs, still repeating
  the last frame meanwhile), an output surviving a lost device (its own, reopened on a
  new one; the context's, left at once) and failing at the third loss in a minute, on
  a software device that stands in for the platform's
  (`src/tests/gpu_capture.rs`), a page's GPU frame drawing only on its own context in
  `snapshot.rs`, the same capture output on WGC and
  the DX12 device of a real Windows machine, with `capture.targets` there naming a
  display and a window that each start a WGC capture (`src/tests/windows_gpu.rs`,
  behind `hw-tests` and ignored), the
  pipeline against the platform fakes
  (on whatever adapter opens, the software one in CI, and again on a CPU stand-in for the compositor so the
  stage-failure, late-tick, teardown, final-tick, link-loss, stalled-stop and
  blocked-send stop tests, and the final tick's count of what a blocked send leaves
  queued, run without one),
  `connect_rtmp` against a slow fake RTMP server on loopback, one that hangs up
  and is reconnected to, one that stops reading while a stop's deadline runs out, and
  one that reads only a trickle of one large tag, and `connect_rtmp` with a plain
  `rtmp://` URL to another machine logging one warning that names the host and
  nothing from the path, and none for `rtmps://` or loopback,
  a publish opener connecting that fake server on every start (start, media, stop,
  twice) and one that cannot connect, a recording output writing MP4 to a temp dir
  (renamed into place on stop, refused when the file exists or the path is not
  absolute on this machine, no `.part` file left by a failed start), recording while
  streaming on one shared encoder session (and separate sessions on separate
  profiles, and on a half-rate video mix held to its rate without being starved), a
  virtual camera publishing the program as BGRA at the canvas size (fitted into a canvas of another size, started again after a stop, running beside
  a stream, refused without a camera in the backends or while another output uses
  it, a refused start keeping the sink, a failed publish failing only that output, black under the privacy screen), a render panic failing both outputs, overload drops from a
  fake encoder and the mixer reported in `stats.tick`, render and mixer panics
  reported as `failed` on the CPU stand-in (`tests/scenarios.rs` reaches it through
  the hidden `Engine::with_cpu_compositor`), audio sources becoming mixer inputs on the CPU
  stand-in (`src/tests/audio.rs`: the defaults while there is none, sources added and
  deleted while an output runs, an unavailable device or application that leaves the
  output running, a failed stream reopened, an application found again under a new
  process ID, a settings change reopening its input, a `browser` page recorded beside
  the defaults and its presets opening once the backend arrives, a media file's sound reaching
  its own input beside the defaults, silent while hidden, routed and metered, quieted
  by a `gain` filter on its source and loud again once it is removed, closed while its
  audio is off and removed with the source, the program mix reaching the
  engine's audio scope while an output runs, and a bus a `waveform` draws reaching its
  own scope while the program mix, with no send from that source, stays silent
  (`a_bus_a_source_draws_feeds_its_scope_while_an_output_runs`), and a source a
  `waveform` draws reaching its own scope after its fader while the program mix carries
  another source, then silence once it is muted
  (`a_source_a_source_draws_feeds_its_scope_after_its_fader`)), the audio filter kinds
  and their kernels (`filters.rs`: defaults, range edges, and a chain updated in place
  or rebuilt on a real mixer, and new settings a full command queue refuses kept off
  the record and tried again), item opacity in linear light through a nested scene
  (`snapshot.rs`), the output edits and the
  stream-key actions against `jstream-platform`'s `MemoryKeyStore` (with `secrets_are_redacted`,
  which captures the engine's traces and audit entries), the audit entries of a run of
  batches and actions (`batches_and_actions_are_audited`) and `AuditFile`'s lines and
  rotation (`audit.rs`), and of undo and redo (`undo_and_redo_are_audited`), the
  preview streams (`src/tests/preview.rs`: no subscription without a binary channel,
  JPEG without an encoder following the mix's changes and the canvas size, H.264 from
  the platform fakes starting at a keyframe and JPEG beside it for a client that asks,
  the subscription checks and limit, unsubscribe and the session's end, and a reader
  that stops reading holding a bounded queue and resuming in order, and a live value
  sent at once with the same `rev`), the
  collection file (`src/tests/collection.rs`: created when missing, batches, an undo
  and a redo each saved, a saved collection loaded by a new engine with its `rev` and
  outputs, a `waveform` drawing another source saved and loaded with the source it
  draws made first, and loaded from a file from before `audio_source` existed
  (`a_source_drawing_a_source_round_trips`), refused files left
  as they were, a failed save that the next change recovers, and concurrent sessions
  leaving the latest collection and no temporary file), `scene.switch` and the program
  scene's rules (in `tests.rs`, the differential script and `studio.rs`), a snapshot
  that follows program, transitions and preview (`src/tests/transitions.rs`: preset
  resolution, a fade that ends on the clock, a switch mid-transition, and a GPU
  snapshot mid-fade), YouTube on `jstream-youtube`'s `FakeTransport` and fixtures
  (`src/tests/youtube.rs`: sign-in through a real loopback redirect, sign-out, the
  broadcast actions and lists, quota and refused tokens, and a `youtube` output
  publishing to its broadcast's ingest on the CPU stand-in, with the key and tokens in
  no event or response), and an MCP agent (`jstream-mcp`, a dev-dependency) driving the
  engine in-process. The audio routing (`audio.rs` and
  `src/tests/audio_routing.rs`): a mixer's buses, faders, mutes and sends following
  batches and undo, `meters.tick` while a mixer is attached and none after,
  `audio.reset_loudness`, the master's true peak and loudness, and a `when_shown`
  source's activity gain following scene switches, shown and hidden items and undo
  (`activity.rs` checks reachability itself), and a drawn bus's scope pointed at the
  mixer bus its sends feed, none for the program mix, a missing bus refused and the
  scope dropped with the bus's references
  (`a_drawn_bus_gets_a_scope_fed_from_its_mixer_bus`), and a drawn source's scope fed
  from its mixer input, silence while it is muted or has none, a source with no audio
  of its own (a waveform, a `scene` source) or a missing one refused, and its delete
  refused until its `delete.plan` clears the reference and drops the scope
  (`a_drawn_source_gets_a_scope_fed_from_its_mixer_input`); `src/tests/audio.rs`
  also checks that a recording audio source is metered, and that turning its audio
  off closes its input and its meter and turning it on opens them again. The live
  sources (`src/tests/sources.rs`): a colour source built, rebuilt and dropped, a text
  source built and rebuilt, a stored frame winning over the live source, a kind that
  isn't built showing the capture, a missing file showing nothing, an animated image
  advancing on the clock, on the sources thread by itself and after a clock swap, and
  a `waveform` source painted
  by the sources thread (`a_waveform_source_is_painted_by_the_sources_thread`). The parity
  tests add the fixture's `mic` kind, which no built-in kind matches.

Not here yet: live audio values (faders), the audio and outputs snapshots, a GPU encode target, and
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
  ([ADR-0057](../../docs/adr/0057-server-answers-credential-calls.md)). Behind a server
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
descriptors and the sources that draw them), `jstream-plugin-proto` (the CLAP host
supervisor's block and control messages), `jstream-youtube` (Google sign-in and the
Live API client behind the YouTube commands and `youtube` outputs), `serde_json` (settings
values: the audio filter kinds' defaults, what the mixer kernels read from a filter's
settings, and the CLAP kinds' parameter defaults), `png`
(encodes the snapshot readback; default features off, deflate stays pure Rust, allowed by
[ADR-0031](../../docs/adr/0031-source-types-and-decoding-scope.md)), `thiserror`
(`SourceFrameError`), `futures-core` (the `Stream` trait of event streams),
`arc-swap` (the published render snapshot and the mixer thread's list of bus and
source scopes)
and `tracing` (see *Logging*). No async runtime: `output.start` blocks the call until
the pipeline is up or has failed, and `video.snapshot` runs on a plain thread. Dev-dependencies: `jstream-control`'s `mock`
feature and `jstream-protocol`'s `fixtures` feature (the differential and parity
tests), `jstream-platform`'s `fake` feature (the pipeline test and
`tests/scenarios.rs`, and the CLAP fake hosts), `jstream-youtube`'s `fake` feature
(`src/tests/youtube.rs`), `jstream-browser`'s `fake` feature (the browser pages in
`src/tests/audio.rs`), `zune-jpeg` (decoding the preview's JPEG pictures in tests),
`jstream-mcp` (`tests/mcp.rs`) and, on Windows,
`jstream-platform-windows` (the GPU capture hardware test, `src/tests/windows_gpu.rs`,
compiled only with this crate's `hw-tests` feature); the tests also use `serde_json`
for settings values and saved collection files. All of them
are in lower layers
([R-ARCH-002](../../docs/RULES.md#R-ARCH-002)), and none of the clients depends on this
crate.

### Calls

Batches and the collection queries run under the collection lock, so those calls
are serialized across sessions and their events are queued in the order the changes
happened. Placeholder output actions do too: they only step the state machine.
`video.snapshot` reads `rev`, the published render snapshot, the live values and the
media clock's time under that lock and hands them to the snapshot thread after
releasing it (see *Snapshots*).

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

- **Batches** go to `jstream_graph::Graph::apply_batch`, which owns every edit rule
  (except, until CLAP filters run, the engine's own refusal of a `clap/` `filter.add`,
  `invalid_argument` on its `kind`, checked before the graph and so before
  `expected_rev`): `rev_conflict` for a stale `expected_rev`, checked before `empty_batch`; then the
  edits in order on a working copy, the first failure aborting with `not_found`,
  `already_exists` or `invalid_argument` (`edits.<n>.<field>`) and nothing applied; on
  success `rev` goes up by exactly one, and the batch result lists the hotkey bindings
  the batch's deletes removed (`removed_hotkeys`). The engine adds the per-client **idempotency
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
  kinds the engine gave it (`kinds.rs`), and the CLAP filter kinds a scan added. `output.status` reads the outputs' runtime state
  (`not_found` for an unknown one) and fills `key_set` from the key store after the
  collection lock is released (`internal` if the store fails). `video.snapshot` is answered here, not by
  the graph.
- **Actions**: `output.start` and `output.stop` (see *Outputs* below),
  `output.set_key` and `output.clear_key` (see *Stream keys* below), `scene.switch`
  and `studio.set_preview` (see *Program scene* below), `privacy.set` (see *Privacy
  screen* below), `audio.reset_loudness` (see *Audio routing*), which succeeds whether
  or not the mixer runs, and `live.set` and `live.cancel` (see *Live values* below).
  `live.list` is a query answered from the gesture table.
- **`studio.get`** reads the program and preview scenes under the collection lock, and
  **`privacy.get`** the privacy screen.
- **`capture.targets`** lists the platform capture backend's displays, then its
  windows, with no engine lock held, since listing windows calls the OS. With no
  backends it answers `action_failed`, and so it does when the OS refuses the list.
- **YouTube** actions and queries (`youtube.*`) run with no engine lock held, since
  most call the Live API (see *YouTube* under *Outputs*). Without `with_youtube`,
  YouTube's API features are off: `youtube.status` answers `unavailable` and the others
  `action_failed` saying the build has no OAuth client, and clients hide them.
- A call, query or action this engine does not implement answers `internal`, as the
  mock does.

The engine's answers are the mock's for the differential script: `tests.rs` runs one
script of calls against both and compares results, error codes and fields, and events.
That script does not call `video.snapshot`. The mock has no compositor and answers it
with `internal`, so the two are not compared. Nor does it call `capture.targets`,
which the mock answers with a made-up display. It does send the folder, label and
metadata edits ([ADR-0024](../../docs/adr/0024-scene-structure.md) §3 and §4): each
refusal, the in-use deletes, the reads, undo and redo, and a filed scene's and a
labelled source's delete, which the test checks applies, undone with their metadata;
`history.rs` checks the engine's undo of them further. The mock stays the reference
clients were developed against, and `jstream-graph` the only implementation of the edit rules
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
  names, the preset a scene override or `transition.set_default` names, what a hotkey
  binding's action names), properties
  set (names, a scene's
  role, an item's placement, whether set as a transform or a geometry, crop,
  visibility, opacity and blend, an item's index, which `item.move` also sets, which
  source an item places, each settings parameter, a filter's switch, settings
  parameters and index, an output's target and encoder choice, a profile's
  encoding, each audio field a `source.set_audio` or `bus.set_fader` sets, a bus's
  name, a send's gain, a preset's fields, a scene's transition overrides, an item's
  transition settings), the scenes' order, the canvas size, and the default preset. A send is an object of its own, which uses its source
  and its bus, so changing two sources' sends into one bus doesn't conflict, and
  deleting a bus conflicts with every change to its sends. An undo or redo first applies the step's edits to a copy of the
  graph, so the step's touched set includes what they touch *now*: undoing
  `scene.create` also deletes the items the scene holds now, even ones whose own step
  was dropped. The step conflicts with another client's later entry, logged after
  the step was last applied or undone, when either creates or deletes an object the
  other touches, when both set the same property, when both reorder scenes, when
  both set the canvas size, or when both set the default preset. Two
  items added to one scene do not conflict. A conflict fails with `undo_conflict`
  naming the first such entry, nothing applied, and the step dropped. Edits that
  pass the check but no longer apply also fail with `undo_conflict`, naming the latest
  entry by another client, and drop the step. Edits that fail only because an output
  is running (undoing `output.create` while it runs) answer the graph's own
  `invalid_argument` and keep the step for a retry once it is stopped; with no other
  client's entry to blame, any other failure does too. The log is pruned to the oldest
  `since` of any kept step. Only when there is no conflict does the copy replace the
  graph.
- **Not steps:** actions (outputs, stream keys, live values), queries and history
  calls. A gesture's step is its committing batch.

### Live values

Live values ([ADR-0032](../../docs/adr/0032-live-edits-and-undo.md)) live in
`gesture.rs`'s table, under the collection lock, and reach the render thread through
the render parameter channel, never through the collection:
- **`live.set`** checks the message's shape (1 to 256 values, each a live-adjustable
  setter, room for a new gesture among the client's 16), then applies the values to a
  copy of the graph exactly as a batch would, naming a failure `values.<n>.<field>`.
  The copy is thrown away: `rev`, the undo history, the collection file and every
  collection read stay as they were. The values go into the gesture, each taking its
  parameter (an item's placement, set by `item.set_transform` or `item.set_geometry`,
  or its crop) from any gesture that held it, and the table is published on the
  channel (`RenderHub::publish_live`, a `LiveParams` of per-item overrides, swapped
  with `arc-swap` only when it changed).
- **The channel.** The render thread reads it after it adopts the snapshot on every
  tick, with a pointer compare, and hands a replaced one to the collector
  (`RenderReader::adopt`, R-RT-003). `compose` applies an item's live placement and
  crop wherever the item is drawn, nested passes included, with no new snapshot and
  no allocation. A running transition's tracks are the exception: they move between
  the items as the collection holds them, so a live value on a program item shows once
  the transition ends. `video.snapshot` and the preview thread read the channel with the
  snapshot under the lock, so a snapshot or a preview picture shows an open gesture as
  the output does; a preview stream is due when the live values change.
- **`live.changed`** is coalesced per gesture: a gesture whose last event is at least
  1/30 s old sends its unsent values at once; otherwise the `jstream-live` thread sends
  the newest values when the interval is up. The thread starts when there is such work
  and exits when there is none, and never holds the lock while it sleeps.
- **Commit.** A batch with `ends_gesture` is refused with `invalid_argument` unless the
  client has that gesture open; it is checked after the idempotency key, so a retried
  commit gets its first answer. Once the batch applies, the gesture's unsent values go
  out, then `state.changed`, then `live.ended` (`committed`, with `rev`). The gesture's
  values move to the releasing list with the render generation current after the
  commit (the generation of the snapshot that carries it), and leave the channel once
  `RenderHub::caught_up` says the render thread holds that generation or is stopped.
  That is checked at once, then by `jstream-live` when it next wakes (within one
  coalescing interval, 1/30 s, if it was already sleeping) and every 4 ms after that,
  so the render thread never waits and no frame shows the value from before the gesture in between
  ([ARCH › Threads](../../docs/ARCHITECTURE.md#threading), "Live-value handover").
- **Cancel and expiry.** `live.cancel` and a session's end (`InProcessControl`'s drop)
  send the gesture's unsent values, then `live.ended` (`cancelled` or `expired`), and
  drop its values from the channel at once. Cancelling a gesture that isn't open is a
  no-op that succeeds. Each such gesture gets one audit entry (see *Audit log*); a
  `live.set` never does.
- **Deletes.** After every applied batch, undo and redo, live values on an item that is
  gone are dropped from every gesture and from the releasing list.
- **Not yet:** audio faders and sends, which need the mixer's own parameter channel and
  its adopted generation in the same handover, and kind parameters marked `live`.

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
`video.snapshot`, draws the program scene, and a switch re-plans it.

**Preview** ([ADR-0052](../../docs/adr/0052-transition-mechanics-and-studio-preview.md))
follows the same rules: `studio.set_preview` sets it (`not_found` for a
missing scene; setting the scene already there emits nothing), the first scene goes on
preview when it is created, and a deleted preview scene gives way to the first scene
left. A switch never moves it, and nothing draws it until E2's preview mix does.

**Transitions** ([ADR-0029](../../docs/adr/0029-transitions.md)). A switch resolves its
preset (`transition::resolve`: the preset it names, else the new scene's
`transition_in`, else the old scene's `transition_out`, else the default). A `cut`, a
zero duration, or a kind this engine can't run puts the scene on program for the next
frame. A `fade` or `move` puts it on program at once too, and keeps a `Running`
transition beside it in the studio: where it starts from, the scene it goes to, its
start on the media clock, and the resolved preset. `studio.changed` names the preset and
the transition's length. While it runs, `snapshot::plan_with` plans the transition
instead of the program scene: `transition::plan` pairs the starting items with the
program scene's (as the collection holds them now, so an edit mid-transition shows),
and the shot carries one `Track` per item, which the render thread samples at each
frame's time. The plan's wake time includes the end, so the engine re-plans then, sees
the transition has ended and lets it go; from that frame the program scene is drawn
alone. A switch while a transition runs samples it at the switch time
(`snapshot::on_screen`, the function the render thread draws with) and starts the new
one from those items, so nothing jumps
([ADR-0052](../../docs/adr/0052-transition-mechanics-and-studio-preview.md) §1). A switch to the scene already on program, even
mid-transition, does nothing. An edit that moves program drops the transition.

Pairing and its inputs: each visible item that has a frame (or places a nested scene,
or a `display` or `window` source showing its capture) becomes a `Visual` at its stored opacity, its
placement converted to a geometry (a legacy placement through `ItemGeometry::from_legacy`
at its frame size; a reflection is not kept mid-transition). `move` pairs by source as
the protocol says; for it, a nested scene's items are flattened into its parent only
when its item covers the canvas exactly (an identity placement, no crop, not rotated)
and is opaque, so a scene shown at another size or faded moves as one image
(ADR-0052 §2). Stagger delays only
the incoming items (§3). Audio does not follow a transition
yet (D29).

### Privacy screen

The privacy screen is runtime state in `privacy.rs`, under the collection lock like the
program scene, never saved and never part of `rev`
([ADR-0027](../../docs/adr/0027-program-master-and-output-chains.md)). `privacy.set`
turns it on or off and emits `privacy.changed` naming the client and request; setting
the state it already has emits nothing. A subscription opened while it is on starts
with `privacy.changed { on: true }` naming the client that turned it on, with no
request. Every `privacy.set` is audited with the state it asked for.

The engine stores an atomic flag with release ordering when the state changes; the
render thread loads it with acquire ordering at the start of each tick and marks every
mix it draws on that tick covered. A covered mix is drawn as usual, then blurred last,
after the letterbox: the frame is halved, rounding up, until its longer side is 32
pixels or less, and the smallest layer is stretched back to the frame (the linear
sampler makes each halving a box filter). Those layers live in the frame's
`ResourceCache` whenever a mix is drawn, and a mix not due on a tick claims them with
its other layers, so turning privacy on allocates nothing on the render thread. A
virtual camera is fed the program frame, so it is covered too. It fails closed: a covered tick whose draw failed blackens the
frames due on it instead of repeating them, including after a fatal draw error. The
CPU stand-in draws covered frames black. `video.snapshot` and the preview streams show
the editing view and are never covered. Only the default blur exists: the look setting, the slate and muting
sources come later.

### Snapshots

`video.snapshot` returns the program mix as a PNG
([jstream-protocol › Design](../jstream-protocol/README.md#design)): the program
scene, read under the collection lock when the shot is copied, so a snapshot after a
`scene.switch` shows the new scene, or the transition onto it at the snapshot's time. An empty collection is an opaque black canvas.

The canvas is the collection's (`State::canvas`, default 1920×1080, changed by
`canvas.set_size`), copied into the shot with the items, so every scene of a snapshot,
nested ones included, is composited at that size; the PNG is that canvas scaled to fit
1280. Each item is drawn in its stored placement form
([ADR-0022](../../docs/adr/0022-item-transform-and-canvas.md)): a geometry by the
shared formula, a legacy transform evaluated against the frame's size.
Hidden items are skipped. A source with no stored frame is skipped, not drawn as a
placeholder. Each item is drawn at its `opacity`, multiplied into its premultiplied
colour in linear light, so a nested scene's layer fades as one image; during a
transition each track's sampled opacity runs between its items' own. The blend is
src-over, the only mode the graph accepts (`normal`).

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

**The render snapshot.** `snapshot::plan` copies the program scene, the scene each
`scene` video mix shows, the scenes they reach, the frames they place and the passes
that draw them (`schedule`, made once here, once per root scene) into an immutable
`Shot`. The program scene is the one `scene.switch` put on program (*Program scene*),
or the transition running onto it, whose tracks the shot holds in place of the
program's pass. A `program` or `preview` mix draws the program's passes, or the
transition while one runs (no mix draws studio mode's preview scene yet), and
so does a mix the shot doesn't have, such as one deleted while an output still
renders it. The engine re-plans after every applied batch, undo and redo, a
`scene.switch` that moves program, a transition's end, every `set_source_frame`, a
collection file's load, a live source's build and a live source's next frame (see
*Live sources*), under the collection lock, and `RenderHub::publish` stores
it through `arc-swap` as the next render generation only when it draws something new
(the canvas, the scenes, their placements, the scene each mix shows, the frames'
versions or the transition differ). So a batch that changes nothing on program or any
mix, or a frame for a source no such scene shows, publishes nothing. Each stored frame
carries a version, so a cache re-uploads it only when it changed. `video.snapshot`
reads `rev`, the published snapshot, the live values and the media clock's time
together under the lock and hands them to the snapshot thread, which draws a running
transition at that time.

`snapshot::compose` draws one mix of a `Shot`, with the live values over it, for the
render, snapshot and preview threads through a `jstream-render` `ResourceCache` keyed
by `GpuKey`: a source's upload (or the import of a `browser` page's texture, see
*Live sources*), each capture's (an upload of a CPU frame, or an import of the
platform's texture, `ResourceCache::import_opaque`; a capture is opaque on both paths,
its alpha read as 255, #451), a nested scene's layer at its depth, and
the mix's scene at the canvas size. An unchanged frame is not uploaded again, and each
nested scene is redrawn into the layer it had (`render_into`), so a steady frame
allocates no texture. On a render tick, a mix that isn't due claims its layers without
drawing (`snapshot::claim_layers`), so mixes at different rates keep theirs between
the ticks they draw on. Images no frame references are retired and freed after the GPU
has finished with them. The mix's scene is drawn at the canvas: straight to the output
when that is the canvas size (`video.snapshot`, and the program at the program stage's
size, which is the canvas the stage opened with), else into a layer that a last pass
fits inside the output, centred, with black bars where the aspect ratios differ, never
stretched ([ADR-0022](../../docs/adr/0022-item-transform-and-canvas.md); the render
thread draws at each mix's encoder size). A transition replaces the program scene's
pass in every mix that shows the program: `compose` samples each track at the frame's
time and draws the sampled items over opaque black, bottom first, the nested scenes
they show composited first. Without a scene to draw, the primary display's capture is
scaled to fill the canvas, which is fitted the same way, or the output is black.

**Video chains** (`chain.rs`, [ADR-0026](../../docs/adr/0026-source-filter-chains.md),
roadmap V0). Planning copies each reached source's video chain into the `Shot`: its
input (the source's upload or its capture) and its enabled filters of a kind
`jstream-render` draws, in order. A disabled filter, or one of a kind it doesn't draw
(an effect pack's), is bypassed, and a source with no filter left has no chain. Every
item that places a source with a chain shows the chain's output (`GpuKey::Filtered`)
instead of its input, in every mix and in a transition's tracks, at the size the chain
reports. A chain over a capture keeps that capture among the shot's captures. Each
compositor that draws shots (render, snapshot, preview) keeps a `Chains`: one
`VideoFilter` per running filter, made when it first draws, updated in place when its
settings change and kept while the shot keeps it, so a stateful kind sees its frames
in order. `compose` runs every chain once per frame of the cache, after the uploads and
captures, however many mixes the tick draws (`ResourceCache::frame`), each filter into
a layer of the cache (`GpuKey::Chain`, the last into `GpuKey::Filtered`) through
`Compositor::apply_filter_into`, so a steady chain allocates no output texture, and
each multi-pass filter's `VideoFilter` keeps its scratch layers while the size holds.
`claim_layers` keeps those layers for a mix that isn't due. A filter no shot runs any
more is retired into the cache (`ResourceCache::retire_filter`) and dropped with the
images it reclaims, on that thread's collector. A chain whose filter fails (its layer
can't be made, or it fails to draw) draws nothing that frame, logged once per filter
until a frame it draws. Unlike a failed audio filter, which ADR-0026 bypasses, it
doesn't show its unfiltered input, so a failing key or mask reveals nothing it hides.
Each filter's layer key is made when the chain's shape changes, so a steady frame
builds none. A shot with a chain counts as
changing with time (`Shot::changes_with_time`), so the preview redraws it like a
running transition. `video.snapshot` runs the chains on its own compositor, so a
stateful kind (a video delay) starts empty there. The built-in video filter kinds are
in `kind.list` and `filter.add` takes them on a video chain.

A filter that reads an asset (`apply_lut`'s `lut`, a `.cube` file or a tiled PNG, and
`image_mask`'s `image`, whose first frame is the mask) gets it through the live
sources (`filter_assets.rs`). Each sync lists the assets the collection's video filters
name, enabled or not, so enabling one needs no load. The `jstream-sources` thread
reads and decodes each new one, as it builds sources, and an asset no filter names any
more is let go. A plan hands each chain filter the asset it names, numbered by its load,
and each compositor makes the filter's reference image from it once: the LUT's tile
layout (`jstream_render::lut_image`) or the mask uploaded as straight sRGB. A reference
kind keeps no state between frames, so a new load makes the filter again with the new
image, and the old filter, which holds the old image, is retired through the cache. A
file that can't be read or decoded is logged and its filter passes its input through
until the setting changes. Moving the collection file reloads every `collection`
asset.

A `scene_mask` reads its `scene` as drawn this frame. Planning collects that scene
like a nested one, and `Shot` holds the order a frame runs its chains in
(`schedule_chains`): each chain after the passes of the scenes it reads, and each such
pass after the chains of the sources it places and the scenes it nests, so a chain
that masks with a scene of filtered sources sees them filtered this frame. The scene is
drawn at depth 1, as a nested scene, into its own layer (`GpuKey::reference`, at the
canvas size), and those passes run once per frame with the chains, before any mix
draws; `claim_layers` keeps their layers too. The filter holds a clone of the layer and
is made again only when the layer is remade, the old one retired through the cache. A
scene pass that fails (its layer can't be made, or the draw fails) fails the frame, as
a failed nested pass does, and the chains after it don't run that tick. The graph
refuses a cycle through a filter's scene reference (ADR-0026). Were one there, the
chain that closes it would find no layer on its first frame and draw nothing, logged
once, and would read the frame before after that.
A `display`, `window` or `webcam` source with no stored frame shows what the pipeline
captures for it on the render thread, and nothing in `video.snapshot`, which has no
capture; a stored frame wins. The `Shot` lists what the passes of the program, or the
tracks of a running transition, and of every mix capture (`Shot::captures`, a
`jstream-sources` `CaptureSpec` each: a display or window by its capture-backend target
ID, or the primary display; a video device by its ID, or the first one listed), and
sources with the same `target` or `device` share one capture, whichever mixes show
them.
Sharing is by `target` value: a `display` source with an empty `target` and one that
names the primary display's ID capture the same monitor twice. A window source with no window chosen shows nothing. The capture is part of
what the shot draws, so a new target publishes a new snapshot.

**Live sources.** `live::LiveSources` keeps one running `jstream-sources` `Source` per
source in the collection with a built-in kind, built from the registry
(`Registry::create`) with the source's settings. `sync` runs under the collection lock
after every applied batch, undo and redo, a collection file's load and the host's
injection of media files (`with_backends`), which rebuilds every source so a `media`
source loaded before it plays: it queues a build for a source that is new or whose
settings changed, and drops the instance of a deleted one. Building reads and decodes files, so it runs on `jstream-sources`, a
thread the engine starts with its first build, never under the collection lock; the
thread then installs the source under the lock and re-plans. Each build is numbered,
and only the latest build asked for a source installs, so a slow build can't replace
a newer one. Until its build installs, a source keeps showing what it showed before,
or nothing. A changed setting always rebuilds the source, live or not, so an
animation restarts with it. A build that fails is logged at `warn` with the source ID
and an error that never names a host path, and the source draws nothing; a kind that
is only described (`NotBuilt`) draws nothing silently. Audio-only kinds
(`audio_input`, `desktop_audio`, `app_audio`, whose descriptors have no video) get no
`Source`: the program stage records them (*Audio sources*). A source with sound of
its own that plays while shown (`Source::sound`, a `media` file's `MediaAudio`) keeps
it beside the source: after each plan (`end_plan`) it is audible exactly when that
plan reached the source, so a hidden item, or a scene that isn't the program, is
silent. `take_audio` hands the engine every running source's sound, with its build's
number and time zero, when an install, a deletion or a new clock changed it; the
engine passes that to the audio sources (`AudioSources::sync_live`) as it publishes
the plan. Every source is built with the engine's one `jstream_sources::AudioScopes`
in its `Context`: the mixer thread pushes each program block into its program scope,
each block of a bus a source draws into that bus's scope, and each block of a source
a source draws into that source's scope (*Audio routing*), and the `waveform` and
`spectrum` sources draw the latest 2048 samples of the scope their `audio_source`
setting names, else their `audio` setting's. The program mix runs only while the
program stage does, so with no output running they draw silence, and once settled on
it they are checked every 250 ms rather than every frame. A `browser` source opens
its page in the backend the host gave `with_browser` (none by default, so it draws
nothing and logs `NoBackend`); giving one rebuilds the sources already there that
open through it (`browser` and its `youtube_chat` preset,
`jstream_sources::opens_browser`). Its page's picture is a GPU frame (`Source::gpu_frame`): while a program stage runs on
a platform GPU context, the stage hands that context to the live sources
(`Engine::use_gpu`, `LiveSources::use_gpu`), which pass it to the browser engine
(`BrowserBackend::use_device`), and takes it back first when the stage stops, or
when its render thread leaves the GPU path mid-run (see [Runtime](#runtime)). The plan
then shows the page's newest texture in place of its transparent stand-in, as a
`Frame` that carries the context's `ContextKey`. `compose` imports it, premultiplied
sRGB (`ResourceCache::import_premultiplied`), only on a compositor of that context;
anywhere else, `video.snapshot` included, it draws nothing. A page repaints on its
own, so while a context is set a source that opens through the browser engine asks for a re-plan every program
frame (1/30 s), painted yet or not, as an animation asks at its next frame; a texture
equal to the last one keeps its version, so the shot doesn't change. Without a
context the page shows its stand-in, transparent. `LiveSources::audio` lists every
running source's own audio (`Source::audio`, heard whether or not it is shown) for
*Audio sources*.

**Audio sources.** `audio_sources::AudioSources` holds, under the collection lock, the
`jstream_sources::AudioTarget` of every audio-only source, and each running source's own
audio, or none while the source's audio is off (ADR-0028: off, the engine doesn't open its stream); `sync` runs beside the live
sources' and wakes the program stage's thread when that set changes. When the program
stage starts, its `AudioInputs` opens one mixer input per audio source that is on through
`MixerControl` (D1's live API). A source's input opens muted and is listed in
`audio_sources::Routed`, where the audio routing finds it (see *Audio routing*) and
sets it up as the collection says; the default microphone and desktop inputs send to
the master bus at unity, as every new input does. Each source's input runs the
source's audio filter chain ([ADR-0026](../../docs/adr/0026-source-filter-chains.md)):
`sync` also records each source's enabled filters of a built-in kind, in order
(`filters::chain_of`; a disabled filter or a pack kind is left out, so bypassed), and
on every pass the thread brings the input's filters in line (`filters::Chain::apply`).
The filters run before the fader and sends the routing sets, which leave them alone.
When only settings changed, each changed filter is updated in place
(`FilterControl::set`), so a gate's envelope or a delay line carries on; a new kind, a
reorder, or a limiter's look-ahead (its latency) rebuilds the chain. A filter the mixer
refuses to add is bypassed, and new settings it refuses (a full command queue) leave
the filter on its old ones until the next pass tries again; each refusal is logged at
`warn`, never an edit failure, and a reopened input gets its chain again. Each input is opened by asking the platform
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
after the mixer thread has stopped.

A running source with audio of its own (a `browser` page's, from
`LiveSources::audio`) is wanted too, as `Recorded::Own`: `sync` takes those beside the
graph, and also runs whenever a build installs, so a page's audio is recorded once its
source is built. Its input is added in the format the source reports and fed by
`SourceAudio::open`, and is replaced when the source is built again (compared by
identity) and closed when it is deleted or its audio is turned off. Like an audio
source's, it opens muted and is listed for the routing. A failed stream is reopened
like a device's. Such audio doesn't count as an audio source for the defaults below.

While the collection has no audio source, the
program records the default microphone and desktop audio, as it did before audio
sources existed, so an earlier collection is not silent; the first audio source
replaces them, and keeps them away while its audio is off. Whether a source is recording or unavailable is not reported to
clients yet: that needs a protocol field (D5's lane).

A live source's sound is one more input of the same set, keyed by its source ID and,
like an audio source's, opened muted and listed in `Routed`, so the routing gives it
its fader and sends (a `media` source gets its master send at creation, as every
source with a channel does) and its activity gain. Whatever that gain, the feeder
hands sound on only while the program's own plan reaches the source (`end_plan`): a
`when_shown` file inside a nested scene that follows `always` gets a gain of 1 off
program, but is fed nothing there, so it is silent. It isn't opened while the source's audio is off (`sync`
also notes which sources' audio is off). The thread opens it by adding a mixer input
in the sound's format and opening the source's own stream on its producer
(`MediaAudio::open`, stamped on the program stage's media clock from the source's
time zero), and closes it like any other. A rebuilt source, or one whose time zero
moved, is a new input. It doesn't count as an audio source for the defaults: a
collection whose only sound is a video still records the microphone and desktop
audio.

`snapshot::plan_with` asks what each source an item it reaches places shows: the
stored frame if `set_source_frame` gave one, else, for a live `display` or `window`
source, what the pipeline captures for it (`Source::capture`), else the live source's
frame at the current time, counted from when the source was built. A live frame is shared with
the source (an `Arc` of its `VideoFrame`, not a copy) and gets a new version only when
the source hands out a different one, so a still image is uploaded once. The plan also
returns the earliest time one of its frames changes (`Source::next_change`), and the
thread re-plans then, so an animated image or a video advances on the clock and a
still one is never re-planned. A source with a `jstream_sources::Painter` (the
`waveform` and `spectrum` kinds, which draw a whole frame and, for the spectrum, an
FFT each 1/30 s, and `chat_message`, which redraws when another message is featured)
is painted by that thread just before each re-plan, holding no engine
lock, so under the collection lock the plan only takes the frame painted last;
`install`, `sync` and a clock swap hand the thread the running painters and their time
zeros. Every other built kind makes its frames when built, or (a `media` file) decodes
them ahead on a thread of its own, so the plan's `frame` calls under the lock never
draw or decode. Each plan is numbered (`begin_plan`), and each source remembers the
last plan that reached it, starting from the plan current when it was built: one that
a plan reaches after plans that didn't (its item was hidden, or its scene wasn't the
program) is told so first (`Source::show`), so a media file can restart, including
the first time it is shown. Sources run on the media clock of the backends the host
injects (`with_backends`), else on a monotonic clock of the engine's own. A clock swap
restarts every running source's timing and drops the re-plan timed on the old clock:
the thread re-plans at once on the new one. `media` sources play through the injected
backends' media files (`Backends::media`); before any are injected a `media` source
fails to build (`NoBackend`), and injecting them rebuilds every source. `collection` asset
references resolve in the collection file's folder; without a collection file, and
for every `app` reference, the asset is unreadable and the source draws nothing.

`Engine::set_source_frame` stores the latest straight sRGB RGBA8 frame for a source
id. It changes no `rev` and emits no event, and a frame may arrive before the source
exists. An applied `source.delete` drops the deleted source's frame, unless the same
batch created a source under that id again. Width or height of zero, a buffer whose length is not `width * height * 4`,
or a size that does not fit in memory is `SourceFrameError`.

The work runs on `jstream-snapshot`, a thread the engine starts on the first
snapshot. It owns the compositor, serves requests in order, and stops when the
engine goes away. The compositor opens on the first snapshot, hardware adapter first,
with lavapipe or WARP as the fallback (`OpenOptions::hardware`). No adapter is `internal`, and the reason
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

### Preview streams

`preview.subscribe` ([jstream-protocol › Design](../jstream-protocol/README.md#design))
sends the program mix, as `video.snapshot` draws it, to the session that asked. The
session first opens its binary channel with `Control::binary` (the server does this
after `hello`; MCP never does, so its subscriptions fail with `action_failed`), a queue
of 8 frames that reopening ends. A session holds at most 4 streams, by the channel
number the client chose, and they end with `preview.unsubscribe` or the session.

The codec is picked when the client subscribes: the first of its `codecs` the engine
can encode, H.264 when `codecs.list()` names an H.264 encoder (only with backends) and
JPEG always. One `jstream-preview` thread per engine, started by the first stream and
gone once none is left, serves them all. It owns a compositor (`Compositor::open`,
hardware adapter first with the software one as the fallback, or the CPU stand-in
under `with_cpu_compositor`) and keeps each stream's
encoder and pacing to itself, so the stream its session holds is only flags. On each
pass it loads the published render snapshot, `rev`, the live values on the render
parameter channel and the media clock's time (a brief collection lock, never held while
drawing), and for each stream that is due draws the shot with the live values over it,
as the render thread does, at the stream's size and that time, reads it back and
encodes it. A stream is due at most `fps` times a second, and only when the snapshot or
the live values changed, a transition is running in it (its picture changes with time),
a keyframe was asked for, or 2 s passed since its last keyframe; one whose queue is full isn't drawn. H.264 runs on a `jstream-encode`
`VideoSession` that avoids hardware (so a preview never takes an output's hardware
session), VBR, with a keyframe every 2 s, reopened when the picture size changes; its
packets go out with the `rev` of the picture they encode. If it can't be opened the
stream falls back to JPEG when the client accepts JPEG, and ends with an end frame
otherwise, as it does when more than 30 draws in a row fail, or after a panic on the thread. When a
frame finds the queue full, the stream's frames are dropped up to the next keyframe,
which the hub then forces (`KeyframeGate`).

The display capture belongs to the program stage, so a preview draws a `display`
source as nothing, as `video.snapshot` does. A `scene.switch` republishes the render
snapshot, so the preview shows the transition onto the new program scene, then the
scene, with the same `rev` (the switch changes none). Not yet: mixes other than program, and the
post-master stage, which come with the video mixes and the master chain.

### Events

Each `events()` call opens an independent subscription with an unbounded queue, so no
event is dropped ([jstream-control open question 1](../jstream-control/README.md#design)
covers backpressure). `jstream-server` drains its connections' subscriptions as events
arrive, however slowly a client reads, and bounds what it holds per connection itself
([jstream-server › Design](../jstream-server/README.md#design)). A batch emits
`state.changed {rev, client_id, request_id}`, and an output action emits one
`output.state_changed` per step. A gesture emits `live.changed`, at most 30 a second,
and `live.ended` when it ends (see *Live values*). Every subscription of every
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
| Stop drain | packets still queued when a stop's drain deadline passes, or when a send fails while stopping, dropped by the output thread, or by the stop when it leaves that thread behind | `output_packets` |
| Publish link | tags the publish target dropped while its link was down (`LivePublish::dropped`; for RTMP, `ReconnectStats::dropped`), published by the output thread | `output_packets` |

A render tick that falls a whole period or more behind the clock is not drawn late:
the loop skips to the current tick, stamps the frame with it and counts the skipped
ticks in `render_ticks`, so video stays on the clock that stamps audio
([R-RT-004](../../docs/RULES.md#R-RT-004)). Both render-thread counters count base
ticks (1/30 s), whatever the video mixes' rates: `render_ticks` counts every skipped
base tick, including ones on which no mix was due, and the capture mailbox is read
only on ticks that draw, so when every running mix has a divisor above 1,
`capture_frames` also counts the captures between its ticks, which no mix wanted. There is no separate output queue in front of the network. Underruns, device gaps and
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
  remove is retried on the next sync (a refused add is logged). The mixer has a slot
  for every bus a collection can hold: `audio.rs` asserts at compile time that
  `jstream_audio::MAX_BUSES` equals `jstream_protocol::MAX_BUSES`;
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
  that moves program sync too, after program is reconciled;
- the bus scopes (#362): every bus a source's settings refer to (a `waveform` or
  `spectrum` drawing it) keeps a scope in the engine's `AudioScopes`, and the others
  are dropped (`AudioScopes::retain`). The list of mixer buses that feed them (the
  master's `BusControl` for `master`, else the bus's own; a bus the mixer doesn't hold
  yet is left out) goes to the mixer thread as an immutable `ScopeTaps` list through
  `arc-swap`, stored only when it changed. The mixer thread loads it without a lock
  after each block, pushes `Mixer::bus_block` of each into its scope, and hands a list
  it replaces, and its last one when it stops, to the collector, since it may hold a
  scope's last reference ([R-RT-003](../../docs/RULES.md#R-RT-003)). The program
  stage makes the list and hands it to the engine with the mixer's controls;
- the source scopes (#459): every source a `waveform` or `spectrum` draws through its
  `audio_source` (`jstream_sources::drawn_source`) keeps a scope too
  (`AudioScopes::retain_sources`), listed in the same `ScopeTaps` after the buses. It
  is fed from the source's mixer input (`Mixer::input_block`: after its filters,
  fader, mute and activity gain, before its sends, the point ADR-0028 gives a
  sidechain key, so what the audience hears from that source) while the source is
  unmuted, its audio on and its input open, and silence otherwise (the mixer thread
  pushes a block of zeros, kept from its start), so a muted, off or not yet playing
  source draws silence at once. A source's mute silences its sends, not its input, so
  the routing turns it into silence here. The graph refuses a source that produces no
  audio, a `scene` source among them (it has no channel of its own), and a drawn
  source's delete until `delete.plan` clears the reference;
- the master stage's audio chain on the mixer's master chain (`MixerControl::master`),
  which runs on every bus that feeds an output, and each running output tap's chain
  (below), with `filters::Chain::apply`: a filter whose kind and position are
  unchanged is updated in place, anything else rebuilds the chain. A filter the mixer
  refuses to add is bypassed, and new settings it refuses leave the filter on its old
  ones until the next routing sync tries again; each refusal is logged at `warn`.

An output whose audio chain holds a filter, even a disabled one
([ADR-0027](../../docs/adr/0027-program-master-and-output-chains.md)), gets an output
tap on the master bus when it starts (`Engine::add_output_tap`), and its own audio
encode fed from the tap's blocks, stamped earlier by its chain's latency. Its video
still shares a session as *Encoder profiles* says. The other outputs on its video
session (the same video mix and encoder profile) share one audio encode of the master
bus, opened when the first of them starts; each video session has its own. The tap is
removed when the output stops; a chain added to a running output takes effect at its
next start, while edits to an existing tap's chain apply live.

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
over 1024 bytes, or with whitespace, control or non-ASCII characters), then stores it
under the output ID with no engine lock held. `output.clear_key` removes it and succeeds
when there is none, even for an output no longer in the collection. With no store, or when
the store refuses, both fail with `action_failed`. A key is bound to its server (#343):
under the collection lock, an applied batch, undo or redo marks every output it deleted
or whose `OutputKind::key_server` it changed in `State::stale_keys`; once the lock is
released the engine removes those keys from the store and unmarks each one removed.
`output.start` refuses a marked output with `action_failed`, so a start that begins
after the batch never reads the old key for the new server. A start that checked the
key just before the batch is covered by `Engine::output_kind`: until the start settles
it answers the kind the start checked (kept in its `Pending`), so a host's opener reads
that server, never the new one with the old key. For the same reason `output.set_key`
fails with `action_failed` while that output's start is opening: a key sent for the new
server then can't be read with the old one. A store that refuses
leaves the output marked (the batch still succeeds, and a warning is logged) until an
`output.set_key` or `output.clear_key` succeeds. The key is write-only
([R-SEC-001](../../docs/RULES.md#R-SEC-001)): every reason is fixed text, nothing logs
the call's arguments, and no response, event or query carries it. The crate-root test
`secrets_are_redacted` sets a key and checks the reply, `state.get`, `output.list`,
`output.status`, the events, every span and event the engine traces, and the audit
entries.

**Encoder profiles.** An output's `encoder_profile` (collection state, set with
`output.set_encoder`) picks the encoding its next start opens, or the protocol's
`EncoderProfile::DEFAULT` when it has none. Running outputs share an encoder session
when they have the same key: the video mix (or none, for the program at the canvas
size and base rate), its resolution and rate, and the profile ID and its settings, all
as they were at start. An output with its own video chain adds its ID to the key, so
its session and its drawn frame are its own; outputs carry no chain yet
(`Output::has_video_chain` is false), so none does so far. So `encoder.update`, `output.set_encoder`, a mix's new
resolution or rate, or `output.set_video_mix` on a running output changes nothing until
it restarts, and an
output started after the update opens a session of its own rather than joining one
with stale settings. `output.status` reports, per running output, the profile, the
other outputs on the same session, whether its video encoder is a hardware one
(read from `VideoSession::codec`, so a hardware encoder that fell back to software at
its first frame reports software) and whether the session is `dedicated` to it, plus
the video sessions and hardware sessions open
([ADR-0027](../../docs/adr/0027-program-master-and-output-chains.md)); it does not
guess a hardware cap. A hardware session limit fails the start with the encoder's own
error for now, not a session-limit code.

**Video mixes.** The render thread keeps one frame per video mix the running sessions
are fed from, each at its session's size, and counts base ticks (1/30 s). On a tick it
composites only the mixes due on it (every `fps_divisor`th tick, counted from the last
one it drew, so a tick an overrun skips delays a mix but can't starve it), all in one
GPU frame, and pushes each due frame to that mix's sessions; a tick with nothing due
draws nothing. A mix's session is opened at the mix's resolution with a frame period
of `fps_divisor` base ticks, so its timestamps and keyframe interval follow its rate.
A running output follows its mix's `input` live: the frame is drawn from the scene the
render snapshot gives the mix on that tick, so a changed input (or a `scene.switch`,
for a `program` mix) shows from the next frame. A mix deleted while an output still
renders it (pointed elsewhere, then deleted, while running) draws the program until
the output restarts. The render thread keeps every display and window that the
program or any mix shows captured (`Shot::captures`), not only those of the mixes due
on a tick, so a mix's captures are already running when it next draws.

**YouTube.** `with_youtube(YoutubeSetup)` gives the engine a Google OAuth client
(JStream's built-in one, empty for now, or a developer's override; without one
YouTube's API features are off), the HTTPS
transport and an `OAuthTokenStore` (`youtube.rs`,
[ADR-0058](../../docs/adr/0058-youtube-sign-in-with-a-jstream-client.md)). JStream holds one
account: its refresh token sits under the fixed account ID `TOKEN_ACCOUNT`
(`youtube`), which names no person, so no email ends up in a credential's name. A
token found there at setup signs the engine in.
- `youtube.sign_in` binds `jstream-youtube`'s loopback redirect, asks
  `OAuthClient::authorize` for `Scope::Manage` alone (one consent screen, ADR-0041 §4, kept by ADR-0058),
  answers the consent URL and emits `signing_in`. A thread waits for the redirect (up
  to `SIGN_IN_TIMEOUT`, 5 minutes), exchanges the code, stores the refresh token and
  emits `signed_in`; a declined or timed-out sign-in goes back to the state before it.
  A second `sign_in` while one waits answers the same URL, and a finished sign-in that
  replaces an account revokes the replaced account's token (best effort).
  `youtube.sign_out` waits for a Live API call in flight, clears the store (its
  failure is `action_failed` and changes nothing), abandons a waiting sign-in, emits
  `signed_out`, then revokes the token at Google, logging a failure. An abandoned sign-in's thread doesn't exchange a code that arrives later,
  and revokes the token of one already exchanged.
- Each Live API call runs on the one `LiveClient` under its own mutex, so YouTube
  calls run one at a time; a rotated refresh token is stored before the client's mutex
  is released, so a sign-out or sign-in waiting for it never leaves an older token
  behind. A refresh
  Google refuses (`SignInRequired`) drops the client and the stored token and emits
  `signed_out`. `jstream-youtube` errors become `action_failed`, prefixed "YouTube's
  API quota is used up" for quota reasons (ADR-0041 §5, kept by ADR-0058); none carries a token.
- An output whose target is `youtube { broadcast_id }` needs a signed-in client, which
  `output.start` checks before `starting` (`action_failed` otherwise, no event). In
  the start's open step, outside the locks, the engine reads the broadcast's bound
  stream and that stream's `rtmps://` ingest address and key, then connects to it
  with `connect_rtmp`; the host's publish target and opener are not used. A broadcast
  with no bound stream fails the start with a reason naming `youtube.bind_broadcast`.
  The key goes only to the connection ([R-SEC-001](../../docs/RULES.md#R-SEC-001)).
- Once such an output is `active`, its own `jstream-youtube-health` thread reads the
  stream's health every `HEALTH_INTERVAL` (15 s, one quota unit each) and emits
  `youtube.health_changed` on the first reading and whenever the status, rating or
  issues change. While signed out it skips its reads. It ends when the output stops,
  fails or the engine goes away.
  Issue text is cut to 512 characters with control characters replaced.

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
up to 10 s, for the render thread to open its compositor (on the platform's GPU
context, or without one on a hardware adapter first and the software adapter as the
fallback; see [Runtime](#runtime)); that bound only
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

**Video codecs.** The encoder profile's video codec picks the encoder and the FLV
track (`video_codec` in `runtime/pipeline.rs`, `runtime/sink.rs`). H.264 goes in legacy
FLV tags with the decoder configuration the start gives the muxer. HEVC and AV1 go in
enhanced RTMP tags, named by FourCC, with the decoder configuration taken from the
first keyframe, which their encoders send in-band. The protocol's `VideoCodec` names
only H.264 so far, so a profile can't select HEVC or AV1 yet. MP4 recording takes H.264
only: another codec fails the start with `recording … is not supported yet`.

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
it, centred, by nearest neighbour, with black bars: when the canvas changed after the
program stage opened, or has an odd edge, which the stage rounds down to even), publishes it and hands the
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

The render thread reads the capture mailbox of each capture the render snapshot places
(see [Runtime](#runtime)), composites the program mix of the render snapshot (see
*Snapshots*) at 30 frames per second, repeats a capture's last frame when its mailbox
is empty, and reads the composite back as RGBA8 because this
cut has no GPU encode target. Without the platform's GPU context it opens its
compositor on a hardware adapter when the machine has one, and on the software adapter
otherwise (`OpenOptions::hardware`): on WARP a 720p program ran at about 10 fps
(#433). It draws at the canvas size the collection has when the
program stage opens, rounded down to even sizes for NV12 (`output_size`), so every
output encodes at the canvas, not the display (#433); a canvas change while outputs
run takes effect when the stage next opens. Each capture keeps its own size, and the
compositor scales it to its item's placement. Which device it composites on, and how
the capture reaches it, is chosen when the program stage opens (see [Runtime](#runtime)). On the GPU path it
keeps each capture's newest `GpuFrame` until a newer one replaces it, and the cache attaches the
frame's hold only to the frame that draws it, so a held capture slot is the frame on
screen and nothing more. On the CPU path it converts each capture's newest frame to
RGBA8 (RGBA copied, BGRA swizzled, NV12, which Windows' capture delivers, converted
with its frame's matrix and range), opaque: alpha is 255 on every pixel, as the GPU
path reads a capture's (#451); only what the program mix shows is captured, so
a program without a `display` or `window` source converts nothing. Each tick first calls `RenderReader::adopt`, which
compares one pointer with the published snapshot and, when a new one arrived, adopts
it: it stores its generation in the hub's reader slot (one `SeqCst` store) and hands
the snapshot it held to the program stage's `jstream-collector` thread. Then it does the
same with the live values on the parameter channel, which the tick draws over the
snapshot (see *Live values*); replaced ones go to the collector too. The GPU images the
cache frees go there too, and so does the input of an encoder session that has
finished (the session may already be gone, leaving that input the last reference to
its shared state and channel), and so does a virtual camera's tap that leaves (once
its camera thread has ended it holds the last reference to the channels and the stop
flag), so nothing the render thread lets go of is freed on it
([R-RT-003](../../docs/RULES.md#R-RT-003)). The mixer thread hands a finished audio
input to the same collector. The thread clears the stopped marker
before its first load (`SeqCst`, then a fence), and publishes it again when it stops
or a fatal error stops its drawing, so `RenderHub::caught_up` treats it as caught up
([ARCH › Threads](../../docs/ARCHITECTURE.md#threading), "Live-value handover"). The collector ends when the render and mixer
threads have dropped their senders, and the program stage (`Program`) joins it right
after them. While the stage has no encoder session and no virtual camera it draws nothing but still adopts
each new snapshot, so it never holds a gesture up. The compositor
and its cache drop on the render thread when it exits, after the tick loop, or when it
leaves the GPU path, and its video chains with them, so after a fallback a stateful
filter (a video delay) starts empty, as on `video.snapshot`'s compositor. What it rides out
and what is fatal ([R-RT-006](../../docs/RULES.md#R-RT-006)):
- **Recoverable:** a capture error or a missing frame repeats that capture's last
  frame (a missing source frame,
  [ARCH › Threads](../../docs/ARCHITECTURE.md#threading)), and a capture that doesn't
  start or whose target is lost is started again 2 s later; a
  composite error repeats the last composited frame, up to 30 in a row. On the
  platform's GPU context, the 10th in a row leaves the GPU path once instead, for CPU
  capture on a device of the thread's own, and the last frames repeat while that opens
  (see [Runtime](#runtime)). A lost GPU device switches at once, to a new device or
  off the context, the same way (#349).
- **Fatal, reported:** a 31st composite error in a row (counted again after a
  fallback), a fallback from the GPU path whose compositor does not open, panics or
  isn't done in time, the third lost device within 60 s, a panic
  inside a tick (the
  compositor's state is unknown after it), and a frame time that overflows. The
  thread then keeps pushing the last frame until the pipeline stops, so the stream
  does not freeze before the teardown. A virtual camera's publish error is not
  the render thread's: it fails only that output, which leaves the stage.
- The render and mixer threads both catch a panic at their boundary. A fatal error
  becomes `output.state_changed` `failed`, the error event this protocol has. A
  mixer panic, a rejected mix block
  or a mixer poll error is fatal: the encoder would get no more audio.

The render thread reuses each capture's buffer, its composite and readback buffers, and
its GPU textures through the cache. Each tick allocates, for every mix it draws, one
frame-sized buffer per encoder session fed from that mix, the frame the session takes
ownership of (`VideoInput::push` cannot hand buffers back), and the item list `compose`
fills for that mix; then once per tick the cache key of each capture that names a
display or window (a copy of its `target`), the frame-fence callback, the premultiplied
copy of a CPU capture that changed, and on the GPU path an import (a bind group) the
first time each of the platform's capture textures is shown. A virtual camera's tap
reuses the buffers its camera thread hands back, so it allocates only until they
circulate; the camera thread allocates the BGRA frame it publishes, one per frame, off
the render thread. The mixer likewise copies each block
into the buffer `AudioInput::push` keeps. Not measured. The mixer thread
polls the audio sources' inputs into 48 kHz stereo blocks. Encode
sessions turn those into H.264 and AAC. The output thread writes FLV tags to the
publish target. Drops on a full encoder queue are counted in `output_stats`. The
test records tags in memory. `connect_rtmp` is the live publisher. When its URL is
plain `rtmp://` to another machine (`RtmpUrl::sends_key_in_cleartext`), it logs a
warning naming the host, never the path, since the key then crosses the network
unencrypted (#353). `RtmpPublish`
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
`output_packets`, and calls `finish`. It checks the deadline before each of a pass's
video and audio sends, so a slow target keeps it past the deadline by one send at
most. A failure while stopping is logged, not
reported: the output is already on its way to `stopped`. The stop does not count on
the target honoring the deadline (#138): it waits for a publishing output thread
only until the deadline plus 500 ms (`OUTPUT_GRACE`), then returns without it and
closes that thread's line to the publish watcher, so a send blocked in a target that
ignores `stop_by` cannot hold `output.stop` either. The thread is detached and ends
once that send returns. Before the stop removes the output's drop source, it drops
and counts the packets still queued for that thread itself (the two share the
encoder subscriptions, each locked only to receive, never over a send), so the
final `stats.tick` has them (#385). Each count takes the packets it counts off the
queue, so none is counted twice. A lock held by whichever side is counting, and by
the stop over its latch, keeps a count the thread makes either wholly in the final
tick or wholly out of it. Only what is queued after the stop's count, and the tag the
blocked send holds, are not in the final tick.
A recording's output thread is always joined, so its file is renamed into place
when the stop returns. `RtmpPublish` honors the deadline: every socket write's
timeout is cut to the time left (`Transport::set_write_deadline`), so a peer that
reads a trickle cannot stretch one large tag past it, a lost link is not
reconnected, and past the deadline sends fail and `finish` drops the connection
without the `FCUnpublish` … `deleteStream` goodbye. So a peer that has stopped
reading holds `output.stop` for about 2 s, not 10 s per blocked write.
`connect_rtmp`'s unit tests drive fake servers on loopback: a slow one, one that
hangs up mid-stream, one that stops reading while the deadline runs out, and one
that reads only a trickle of a 12 MiB tag. One more drives no server: it captures
the log while `connect_rtmp` refuses an empty key, and checks that a plain `rtmp://`
URL to another machine logs one warning naming the host and nothing from the path,
and that `rtmps://` and loopback log none.

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
engine while that mutex is held. An output's two encoder subscriptions each sit
behind a mutex its output thread takes only to receive or read their counters, never
over a send. Whoever drops and counts what is still queued there (that thread, or the
stop that has stopped waiting for it) first takes the output's abandon lock, then the
video and the audio subscription's; the stop latches the output's drop source, taking
the drop ledger, under it. A stopping program stage detaches its mixer from the
audio routing under the collection lock, which is why it is never dropped while that
lock is held, as its final `stats.tick` already requires. The master's meters are
atomics the mixer thread writes and the sampler reads, with no lock. The `jstream-sources`
thread builds sources and paints the painted ones (*Live sources*) holding no engine
lock, then takes the collection lock only to install a build or re-plan, which asks
each live source for a frame it already has. The snapshot compositor belongs to the snapshot
thread, which takes no engine lock; the queue to it is taken only after the
collection lock is released. The preview thread takes its own state lock, and the
collection lock alone and only to load the render snapshot, `rev` and the live values, never while
drawing or encoding; a session's preview streams and binary channel are leaf locks. The render snapshot is published under the collection
lock (which makes the control side its one writer) and loaded by the render thread
with no lock at all, as are the live values on the parameter channel. `video.snapshot`
reads both under the collection lock and the snapshot thread only receives them. The
`jstream-live` thread takes the collection lock only to send due `live.changed` events
and release committed values, and sleeps without it. A stream's poll takes only its queue's lock. No lock
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
consistent. Nothing is logged while a lock is held. The YouTube side has two more
mutexes, the sign-in state and then the `LiveClient`, taken in that order, after the
start lock and never under the collection lock; the client's is held across its
blocking Live API calls, so a start waiting on it holds only the start lock.

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
`history.undo`, `history.redo` or the action's name), a batch's edits by name in order, an action's `output_id` or `scene_id` (`scene.switch`, `studio.set_preview`) or the state `privacy.set` asked for (`privacy`), the `rev` before and
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

Live values are logged once per gesture, never per `live.set`
([ADR-0032](../../docs/adr/0032-live-edits-and-undo.md)): `live.set` and `live.cancel`
get no entry of their own. A committed gesture's entry is its batch's. A gesture that
ends without a commit gets one entry: `call` `live.cancel` with the cancelling
request's `request_id`, or `live.ended` with no `request_id` when it expired with its
session, plus `gesture_id`, `outcome` (`cancelled` or `expired`) and, in `edits`, the
setters it held a value for, one per parameter, without the values. Its `rev` before
and after are the same.

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
history, idempotency keys and the program and preview scenes are not saved (see *Program scene*).

`jstream-daemon`'s `--collection` option turns this on
([jstream-daemon › Design](../../apps/jstream-daemon/README.md#design)).

### Not yet

- **Live audio values** (faders and sends) and kind parameters marked `live`
  (see *Live values*).
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
- Blend modes other than `normal` in the compositor.
- The rest of the video chain runtime (V0): effect-pack kinds in a chain (bypassed),
  and placing a bounds-growing filter's output at the original content box.
- Activity gains that fade with a transition (audio follows a switch at once, so
  every weight is 0 or 1), and meters for every bus's true peak and loudness.
- The audio and outputs snapshots, the mixer's parameter channel and its part in the
  live-value handover, and a GPU encode target. `output.start` is a cut of
  [ARCH › Threads](../../docs/ARCHITECTURE.md#threading): the video mixes with their
  captured displays and windows, a CPU readback into the encoder, and the platform
  fakes in the test. Other sources show frames the host injects with `set_source_frame`.
- Keeping a window source on its window across restarts (its target ID names a window
  handle, which a new session of the app doesn't keep).

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
control thread. The program is the canvas size the collection has when the stage
opens (#433). On each 1/30 s base tick the render thread adopts the render snapshot,
reads the capture mailboxes of every display and window the program or a mix shows,
and composites each video mix due on that tick (see
*Video mixes* in Design) from the render snapshot (see *Snapshots* in Design) at its
encoder's size, the program at the canvas size: the mix's scene and the scenes it
nests, or a running transition sampled at the tick's media time, with every `display`
and `window` source that has no stored frame showing its capture. With no scene it
draws the primary display's capture scaled to fill the canvas, fitted to the frame. A
capture whose mailbox is empty repeats its last frame. It reads the frames back. This
cut encodes that CPU readback, and the readback is counted (`OutputStats::readbacks`,
one per mix drawn).

**Capture path** (ARCH › GPU pipeline, step 6). Opening the program stage asks the
platform's `GpuInterop` for its device. With one, the render thread's compositor opens on that
`GpuContext`, and the capture backend is asked for a copy of itself on the context
(`Capture::on_context`); a capture started from that copy delivers `GpuFrame`s, which
the render thread samples where they are, with no CPU copy, and counts
(`OutputStats::gpu_captures`). Each fallback is logged and keeps the output running on
CPU frames: no device (the fakes, CI, a machine without interop; the compositor then
opens its own device, hardware adapter first with the software one as the fallback), a
backend with no GPU path or one that refuses the context, and a GPU capture that does not start (that capture then runs on
CPU frames). With a context, the stage also lends it to the browser engine, so
`browser` pages paint on it and the render thread samples them there too (*Live
sources* in Design); the stage takes it back first when it stops, and on a failed
open.

The GPU path can also break mid-run: every GPU frame fails to import (a format the
compositor can't take), or the context's device goes away. After 10 composite errors in
a row on the context, a third of a second, the render thread leaves it once. A
`jstream-gpu-fallback` thread tells browser pages to stop painting on the context (that
takes the collection lock) and opens the compositor on a device of its own, while the
render thread keeps repeating the last frames (black under the privacy screen, which
fails closed) and adopting snapshots. Then the render
thread lets go of the frames it holds, has the `jstream-capture` thread drop its GPU
backend and start every capture again on the CPU path (a start asked for before is
stopped when it comes back), and takes the new compositor. Until the re-planned snapshot
arrives, the pages' frames, carrying the context's key, draw nothing on it. The fallback
is logged as a warning and counted (`OutputStats::gpu_fallbacks`). The output fails at
once if the compositor can't open on its own device, the fallback panics, or it isn't
done within the open bound `output.start` uses (10 s): a hung adapter fails the output
instead of freezing it (#577). A fallback thread that times out is left to finish on
its own and drops what it opened; none of it uses the context. Errors that go on after the fallback are counted again from zero
and fail the output as they would on the CPU path. The context itself stays until the
render loop ends.

**A lost device** (#349; a driver reset after a hang, Windows' TDR, a removed GPU) is
one event, not a run of errors. `gpu_draw` turns a draw that failed on a lost device
(`Compositor::lost`) into `DrawError::Lost`, and the render thread starts the same
switch at once, without counting to 10. On the context, it is the fallback above, so
the captures move to the CPU path. On the render thread's own device, it is a new
device (`open(None)` again, hardware adapter first), and the captures stay where they
are. Meanwhile the output repeats its last frames (black under the privacy screen) and stays
`active`. Encoders take the
CPU readback, so they don't reopen. A virtual camera gets the repeated frame, not its
test pattern. Each loss is logged as a warning and counted
(`OutputStats::gpu_device_losses`; leaving the context also counts in
`gpu_fallbacks`). The third loss within 60 s fails the output instead ("the GPU was
lost ... and the compositor could not reopen"), as does a switch that fails as above.
There is no engine event for a loss yet (a protocol change), and no pack attribution
or quarantine (X1).

The render loop drops the held frames, the compositor and the capture streams before
the context, at a fallback and at the end, so the platform's state goes first as
`GpuContext` requires.

**Captures** (`runtime/captures.rs`). Opening the program stage starts no capture. Each tick the
render thread compares what the snapshot captures (`Shot::captures`) with what it runs:
it asks the pipeline's `jstream-capture` thread to start each new one, and hands it
the stream of each one no longer shown, which that thread drops, so starting and
stopping a capture (OS calls that can block) never run on the render thread. Both go
through channels it never waits on, and it takes the started streams with `try_recv`
([R-RT-002](../../docs/RULES.md#R-RT-002),
[R-RT-003](../../docs/RULES.md#R-RT-003)). The primary display is looked up when its
capture starts. A webcam or capture card is opened on the backends' `VideoDevices`, on
the CPU path, in the format `device_format` picks from those it lists: frames that arrive
uncompressed as RGBA, BGRA or NV12 (YUY2 is delivered as NV12), the largest of at most
1920x1080 (else the smallest), then the fastest. An empty `device` is the first device
listed, looked up when it starts. MJPEG is a candidate when the device offers it and the
platform's codecs make a decoder (`Codecs::capture_decoder`, Media Foundation's on
Windows): one decoder per stream, passed to `VideoDevices::open`. At the same size and
rate an uncompressed format wins. Without a decoder (the default, and Linux and macOS
for now), only uncompressed formats are chosen, so a device that offers only MJPEG
doesn't start. Nothing opens its paired microphone. A
capture that doesn't start, or whose stream reports `DeviceLost` (a
closed window, an unplugged display or webcam), keeps its last frame and is started again 2 s
later on the media clock. The skips of every stream the pipeline ran are added up into
`capture_frames`. At the end of the render loop the captures are stopped and the
`jstream-capture` thread joined, before the context is dropped.

The mixer thread polls the media clock and pushes 48 kHz stereo
blocks mixed from the audio sources' inputs, the pages' audio and the media files' sound (see *Audio
sources* in Design; the microphone and the desktop loopback while the collection has
no audio source), meters each master block's true peak and loudness, and also pushes
each block into the engine's program audio scope for the `waveform` and `spectrum`
sources, and each drawn bus's and source's block into its scope (lock-free rings: no
lock, no allocation; the list of scopes comes through `arc-swap`, see *Audio routing*
in Design). Once the stage is up, the engine routes the
collection's buses, faders and sends into its mixer until it stops (see *Audio
routing* in Design). Every running output shares that one program stage (capture,
render, mixer, audio inputs); the first `output.start` builds it and the
last `output.stop` tears it down, and starts are serialized. Encode threads are the
ones inside the encoder sessions: outputs with the same video mix and encoder profile
share one video session (an output with its own video chain would get its own); those of them with no audio chain share one audio session of
the master bus; an output with an audio chain has an audio session of its own that the
mixer thread feeds from the output's tap. The render and mixer threads take a new
session's inputs from a channel, never a lock, and hand a finished session's input to
the collector rather than drop it. While the stage has no session and no virtual
camera, or no mix is due on a tick, it draws nothing but still adopts the latest render
snapshot. A virtual camera is fed the program at the canvas size on every tick, so that
feed is drawn while a camera runs even if no session is on it; its video mix doesn't
apply yet. Each output but a virtual
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
render snapshot and then the live values on the parameter channel, which it draws over
it, and what the render and mixer threads let go of (replaced snapshots and live
values, GPU images, finished sessions' inputs, the frame buffers of a mix no session
needs any more, replaced lists of bus and source scopes) goes to the program stage's
`jstream-collector` thread. `output.stop` during `starting` cancels that start and
emits `stopped` only after the start has dropped its threads. The render and mixer
threads catch a panic at their boundary ([R-RT-006](../../docs/RULES.md#R-RT-006)); a
capture error or a missing frame repeats the last one, and a render panic repeats the
last frame until teardown; under the privacy screen a frame that couldn't be drawn is
black instead (see *Privacy screen* in Design). A fatal error on the render, mixer or output thread (a
publish or recording write failure, an encoder stream that ends before stop, a render
or mixer panic) is a channel message to a watcher the control thread started; the
watcher emits `failed` and stops the pipeline. A program-stage failure reaches every
running output. Frame hand-off is the encoder session queues. The audio and outputs
snapshots and a GPU encode target are still rows of the ARCH thread table, not this
cut. The mixer also has its own collector, which frees input rings, their counters and
their own blocks, removed buses' state, and removed filter chains and filters. The program stage's
`jstream-audio-sources` thread opens and closes the audio sources' inputs as the
collection changes (see *Audio sources* in Design); it takes no engine lock and is
joined, and every input closed, after the mixer thread stops. A media file's input is
fed by that source's `jstream-media-audio` thread, which `jstream-sources` starts when
the input opens and joins when it closes.
