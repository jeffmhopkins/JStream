<!-- @doc
id = "ADR-0103"
title = "A performance profile per item, source and thread, readable over the protocol and MCP"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0103: A performance profile per item, source and thread, readable over the protocol and MCP

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
Scenes are getting heavy: browser layers, shader sources, item effects, nested scenes
with chains, 3D passes. When a scene drops frames, nobody can say which part costs what.
`stats.tick` has the engine's load (process CPU and program frames, protocol 0.48), its
drop counters (R-RT-004) and, since 0.54, `chains`: how many layers and passes the
chains schedule ([ADR-0065](0065-scene-chains-and-item-effects.md) §7). Those are counts
for the whole engine, not times, and nothing names the item, source or thread that
costs the time. The maintainer asked for a built-in way to see where the time goes, per
object and per thread, that an agent can read over MCP to improve its own scenes, and
made it a requirement for 0.1.0-alpha.4.

Where the time can go:
- **The drawing threads.** The program stage's `jstream-render` thread and the preview
  hub's `jstream-preview` thread both draw a shot with `snapshot::compose`: upload the
  source frames that changed, upload or import the captures, run the video chains
  (source chains, so shader sources, scene chains, item effects, output stage) and draw
  the scene passes. Each of those is one unit of work with an owner.
- **The `jstream-sources` thread** paints the sources that draw their own frames, and
  the planner asks every source the program reaches for its frame under the collection
  lock.
- **Every other thread** of the process: decoders, captures, encoders, the mixer, the
  server.

<a id="decision"></a>
## Decision
1. **A query, `profile.get` (protocol 0.71),** answers a `ProfileInfo` over a recent
   window: `seconds` (1 to 60, default 5) asks for it, `top` (default 20, at most 200)
   caps each list. It is a read: plugin class `read`, its own MCP tool (`profile`), and
   `jstream profile get`. Runtime state, so no `rev`. The result has:
   - **drawers**: the program stage's and the preview hub's drawing, each with its
     frames, the time a frame took (average, 95th percentile, worst), how many frames
     took longer than the frame period, the ticks the program skipped, and the time
     split into phases (uploads, captures, chains, passes, finish).
   - **units**: the units of work the drawers did, costliest first, each with its owner
     (a source's upload, a capture, a source's, scene's, item's or output's video
     chain, a nested scene's pass), the frames it ran in, its average time per frame it
     ran in, and its share of its drawer's time.
   - **sources**: per source, the time the `jstream-sources` thread spent painting it
     and the time plans spent asking it for its frame, and how long its last build took.
   - **plan**: how many plans ran and how long they held the collection lock.
   - **threads**: the CPU each thread of the process used, as a percentage of one core,
     threads of one name added together. JStream's own threads are named `jstream-*`;
     another thread's name was set by a library or driver, so it is `remote_name`
     ([ADR-0059](0059-mcp-annotations-and-remote-text.md) §2).
2. **Always on, and cheap.** A drawer times each unit with two clock reads into a map
   it keeps from frame to frame, so a steady frame allocates nothing (R-RT-002). Once a
   second it publishes its running totals as one immutable snapshot through `arc-swap`;
   the render thread hands the snapshot it replaces to its collector (R-RT-003). No
   real-time thread takes a lock for it. The control side keeps the snapshots it reads
   (on each `stats.tick` and each `profile.get`) for a minute, and a window is the
   difference between the newest totals and the ones from `seconds` ago, or the oldest
   it has. The result says how many seconds it covers. The always-on part includes the
   thread CPU (4), read with each sample on the stats thread, never a real-time one:
   on Linux three small `/proc` reads per thread, on Windows one Tool Help snapshot.
3. **Times are what the drawing thread spent,** recording and submitting GPU work and
   uploading, not what the GPU spent running it, which wgpu can only measure with
   timestamp queries. A frame that is read back waits for the GPU in its finish phase,
   so a slow GPU shows there. Each unit has a `gpu_ms` that stays empty until a later
   change fills it from timestamp queries on adapters that support them; the result says
   whether it is filled (`gpu_timing`).
4. **Thread CPU comes from the platform** (`jstream_platform::ThreadCpu`): on Linux
   `/proc/self/task`, on Windows the Tool Help thread list with `GetThreadTimes` and
   `GetThreadDescription`. Where a host gives none (macOS for now), `threads` is empty
   and `thread_cpu` is `false`.
5. **Nothing scene-specific.** Units are named by the IDs the collection already uses,
   so a client maps them to items with `item.list` and `state.get`. An item with no
   effects is drawn in its scene's pass with the others, so it has no unit of its own:
   its cost is its source's upload and chain, and its share of the pass.

<a id="consequences"></a>
## Consequences
- An agent can build a scene, ask `profile.get`, see that one item's effects or one
  browser source take most of the frame, change it, and ask again. The MCP server's
  instructions say how.
- `stats.tick` is unchanged; a client that wants a live graph polls `profile.get`.
- Times on the drawing threads include waits inside wgpu (the submit lock, a full
  queue), so a busy GPU can show as CPU time on whatever unit submitted next. Until GPU
  timestamps land, a GPU-heavy shader shows as a long finish phase on a frame read back,
  or as time on the units after it.
- The browser engine's own processes are not threads of this process: a browser
  source's cost here is its upload or import and its chain. Their processes' CPU is
  future work.
- The desktop UI can show the same data later; this decision adds no panel.
