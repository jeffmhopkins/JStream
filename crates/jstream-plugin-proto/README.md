<!-- @doc
id = "CRATE-jstream-plugin-proto"
title = "jstream-plugin-proto"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-plugin-proto

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-plugin-proto/src/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-plugin-proto", "docs/adr/0035-clap-plugins-out-of-process.md#decision"]
-->

Layer-0 contract between the engine's mixer and the out-of-process CLAP plugin host
([ADR-0035](../../docs/adr/0035-clap-plugins-out-of-process.md)): the shared-memory
block one CLAP filter instance uses. No dependencies, no `unsafe` and no OS bindings,
so the mixer, the host and the tests share one definition and its tests run on any
host. It follows `jstream-vcam-proto`'s pattern.

Contents:
- `Shared`: the `repr(C)` block at offset 0 of the mapping, exactly its size. Every
  field is atomic (samples are `AtomicU32`s holding `f32` bits), so either process can
  read any field at any time and every bit pattern, the OS's zero fill included, is
  valid. Compile-time assertions pin every offset.
- `Status`: the engine's configuration (rate, block size, pipeline depth, channels),
  the host's state (`STATE_*`), error (`ERROR_*`) and plugin latency, the engine's
  `stop` and `reset_seq` requests, the host's heartbeat and counters, and the mixer's
  miss counters. Each field has one writer.
- `Shared::init` (engine): unpublishes the block, clears it, writes the `Config` and
  publishes it with `MAGIC` and `VERSION`. `Shared::config` (host): the configuration
  only when both match and every field is in range. `Shared::publish_ready` (host) and
  `Shared::ready_latency` (engine): the plugin's latency, then the ready state; the
  engine refuses a latency over `MAX_PLUGIN_LATENCY`. `Shared` is `AsRef<Shared>`, so
  a heap block and a mapped one go through the same `AsRef` bound.
- `BlockRing`, one each way: `SLOTS` (8) slots of up to `MAX_BLOCK_FRAMES` (2048)
  interleaved stereo frames, block `n` in slot `n % SLOTS`. A slot's sequence word is
  cleared before it is rewritten and set to `n + 1` after, and `read` re-checks it after
  copying, so a copy that raced a rewrite is rejected (the same seqlock check as the
  mixer's input rings). `head` is one past the newest block written.
- The hand-off: the mixer writes block `n` and reads block `n − depth` in the same
  block, never waiting. `depth` is 1 to `MAX_DEPTH` (6), default `DEFAULT_DEPTH` (2),
  so two slots stay spare and neither side rewrites a slot the other reads in normal
  running. `first_useful(head, depth)` is the oldest block the mixer can still read,
  where a host that fell behind resumes.
- `ParamRing`, one each way: single-producer, single-consumer queues of 256
  `ParamEvent`s (a CLAP parameter id and its `f64` value), from the engine (the
  filter's settings) and from the plugin (its own editor). Counts further apart than
  the ring's size read as empty and full, since the other process wrote them.
- `LIVENESS_MS`: how long a heartbeat may stand still before the host counts as hung.
- `control`: the control channel, framed messages over the host's stdin and stdout
  for what is rare and variable-sized. The engine sends `Start` (with a saved state to
  restore, or none), `SaveState` and `Stop`. The host sends `Params` (the plugin's
  parameter list, after it starts and after a restart), `State` (answering
  `SaveState`), `StateDirty`, `ParamChanged`, `Plugins` (a scan's answer: each audio
  effect's id, name, vendor, version, description and parameters) and `Error`. A frame
  is a little-endian `u32` length and a payload of at most `MAX_FRAME` (4 MiB, room for
  a 2 MiB state): a tag byte, then the fields, strings and byte strings
  length-prefixed. `read_frame` reads one, and `decode` refuses an unknown tag, a field
  past the end, a list over `MAX_LIST`, a string that isn't UTF-8 and trailing bytes,
  since each side treats the other's frames as untrusted.

**One filter per block at a time.** The mixer's side numbers its blocks on from the
input ring's head and fixes its latency from the host's ready latency when it is built.
When the engine rebuilds a chain, a new filter can go on a block whose filter was
removed, and the host carries on. A restarted host gets a new block and the filter is
built again, since its plugin latency may differ.

**Trust.** Each side treats what the other wrote as untrusted: a slot's frame count
must match the reader's buffer exactly, a corrupt parameter ring is empty, the
configuration is range-checked and the plugin's latency is capped. The mixer side also
turns non-finite samples into silence (`jstream-audio`).

**Tests:** ring round trips, rejected block numbers and lengths, overwritten slots,
and a two-thread test that no torn block is ever accepted; parameter rings in order,
full, wrapped, corrupt and across threads; `init` publishing, refusing bad
configurations and clearing what a previous host left; the ready latency's checks; a
host thread halving blocks against a mixer loop that never waits, every block it gets
back being the one `depth` blocks earlier; and every control message round-tripping
through a stream, malformed payloads and every truncation refused, and streams ending
cleanly or inside a frame.

**Not here:** mapping and naming the block (`jstream-platform-windows`'s
`PluginBlock`), starting the host process (its `WindowsPluginHosts`), watching and
restarting it (`jstream-core`'s `HostSupervisor`), and the host itself
(`jstream-plugin-host`, with `jstream-plugin-clap`).
