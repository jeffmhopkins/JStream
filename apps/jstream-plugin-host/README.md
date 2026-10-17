<!-- @doc
id = "CRATE-jstream-plugin-host"
title = "jstream-plugin-host"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-plugin-host

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["apps/jstream-plugin-host/src/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/apps/*", "docs/ARCHITECTURE.md#threading", "docs/adr/0035-clap-plugins-out-of-process.md#decision", "docs/RULES.md#R-RT-001", "crates/jstream-plugin-proto/README.md#purpose", "crates/jstream-plugin-clap/README.md#purpose"]
-->

The out-of-process CLAP plugin host
([ADR-0035](../../docs/adr/0035-clap-plugins-out-of-process.md)). The engine starts one
per CLAP filter instance, so a plugin that crashes or hangs costs only its own filter,
never the engine, the audio thread or an output. Layer 6, an app with no `unsafe`. It is
the only program that links [`jstream-plugin-clap`](../../crates/jstream-plugin-clap/README.md#purpose)
(lint-deps checks it), so third-party native code never loads into the engine's process.
It talks to the engine only through the
[`jstream-plugin-proto`](../../crates/jstream-plugin-proto/README.md#purpose) contract:
the shared block for audio and parameter changes, and the control channel on its stdin
and stdout. It has no protocol connection and no control token.

**Usage.** The engine starts it; it isn't meant to be run by hand.
- `jstream-plugin-host scan <file>` loads a plugin file and writes one `Plugins`
  message: each audio effect in it that the host can run, with its parameters. Each one
  is created and activated once (48 kHz, 1024 frames) to check that it is stereo with
  ports the host takes. Instruments, note effects and effects that won't activate are
  left out.
- `jstream-plugin-host run <file> <plugin-id>` reads the engine's first control
  message, which must be `Block`: the handle of the shared block the engine created,
  already duplicated into this process. It maps the block from it
  (`PluginBlock::from_handle`) and runs that plugin over it until the engine stops it.
  The block has no name, so no other process can open it (#351). A first message that
  is malformed or isn't `Block` is reported in an `Error` message. Windows only for
  now: elsewhere it answers with an `Error` message.
- Exit status: 0 after a clean run or scan, 1 on a failure (also sent as an `Error`
  message, and marked on the block), and 2 for a usage error.

**A run** (`session`), on the host's main thread:
1. Reads the block's configuration. A block that isn't initialised for this version is
   refused with `ERROR_BAD_CONFIG`.
2. Waits for the engine's `Start`, then creates the plugin. A plugin that fails to load
   is `ERROR_LOAD`, and one that isn't an audio effect is `ERROR_NOT_AN_EFFECT`. It
   restores the saved state `Start` carried. A state the plugin refuses is reported in
   an `Error` message, and the plugin runs with its defaults.
3. Activates the plugin at the block's rate and block size. A plugin that isn't stereo
   or has too many ports is `ERROR_NOT_AN_EFFECT`, and one that refuses or reports a
   latency over `MAX_PLUGIN_LATENCY` is `ERROR_ACTIVATE`. It then publishes the plugin's
   latency with the ready state and sends the parameter list (`Params`).
4. Runs the real-time loop on its own thread, registered with MMCSS "Pro Audio"
   (`ProAudioThread`). The processor moves to that thread and is dropped there, so CLAP's
   audio-thread calls stay on it. Meanwhile, every 10 ms or on a control message, the
   main thread:
   - answers `SaveState` with `State` (empty for a plugin without state);
   - runs the plugin's main-thread callback when it asked for one;
   - sends `StateDirty` when the plugin marks its state changed;
   - on the plugin's restart request (a new latency or port layout), stops the loop,
     deactivates and activates again, publishes the latency and sends `Params` again.
     With the real-time loop stopped, another thread beats the heartbeat meanwhile (for
     at most `RESTART_GRACE`, 10 s), so the engine doesn't count a slow re-activation
     as a hang.
   A `Block` or a second `Start` meanwhile changes nothing.
5. Ends on `Stop`, the block's `stop` word, or the end of the control stream (the engine
   is gone, and its job object will end this process anyway). It then stops the loop,
   deactivates the plugin and marks the block `STATE_STOPPED`.

**The real-time loop** (`rt`, [R-RT-001](../../docs/RULES.md#R-RT-001)). Each pass:
- beats the heartbeat;
- resets the plugin once for each new `reset_seq` the mixer sets;
- skips ahead to `first_useful` when it fell behind, counting the blocks it skipped;
- runs the plugin over every new input block, with the parameter changes queued for it
  (up to `MAX_BLOCK_EVENTS`) applied at the block's start, and writes the result under
  the same block number.

The plugin's own parameter changes go back on the outgoing ring, dropped if it is full.
A block that can't be read (torn, or the wrong size) or that the plugin fails on is
counted as skipped and not written, so the mixer bypasses it. Passes sleep a quarter of
a block, from 0.5 to 5 ms. A pass never allocates, locks or waits on the engine: its
buffers are sized when the loop starts. The tests install `assert_no_alloc` as the
global allocator and check a pass over the real test plugin, with a flood of parameter
changes.

**Tests** host the CLAP plugin built into `jstream-plugin-clap` (its `test-plugin`
feature) over a block on the heap, with the control channel over in-process pipes:
- a run processing blocks with a restored state and a parameter change, echoing the
  plugin's change, saving its state, acting on a reset and stopping on `Stop`;
- runs ending on the end of the stream and on the `stop` word;
- every failure marked on the block with its error and reported (an instrument, a mono
  effect, too many ports, an unknown id, no `Start`, an uninitialised block);
- a bad saved state reported, with the plugin running on its defaults;
- the block's handle taken from a first `Block` message, leaving the rest of the stream,
  and an empty stream, a `Start`, an unknown tag, a cut frame or an oversized one
  refused;
- the loop's numbering, skipping, resets, failed and wrong-sized blocks, pacing and
  stop word, and `scan` listing only the hostable effects.

**Not yet:**
- CLAP filters in the graph, so nothing starts this process for a filter yet. The
  launcher that creates the block and starts it in a kill-on-close job object with a
  scrubbed environment is `jstream-platform-windows`'s `WindowsPluginHosts`, and the
  supervisor that watches the heartbeat and restarts it with backoff is `jstream-core`'s
  `HostSupervisor`.
- The plugin's editor window.
- Waking on a high-resolution timer on the media clock instead of sleeping.
- Running a real plugin file, which needs a Windows hardware check.
