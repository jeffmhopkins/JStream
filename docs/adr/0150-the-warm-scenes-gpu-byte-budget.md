<!-- @doc
id = "ADR-0150"
title = "The warm scenes' GPU-byte budget, and how it is estimated"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0150: The warm scenes' GPU-byte budget, and how it is estimated

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0117](0117-real-time-video-thread-model.md) §7 gives the warm scenes a budget of
"a count of warm scenes and an estimate of GPU bytes", which evicts from the bottom of
their order and never the first three. #1470 built the count (`studio.set_warm`, 0.115),
and [ADR-0149](0149-where-warm-scenes-are-pinned.md) set the order after the Preview
scene. Nothing measures what a warm scene holds, so the byte half has nothing to count.

A warm scene keeps the sources it draws producing, so each of them holds at least its
latest frame: a page's texture, a capture's, an uploaded image or media frame. The
engine knows a running source's frame size, and the size of a frame stored with
`set_source_frame`. It doesn't know what a driver keeps behind them, or what a cold
source will make before it starts.

<a id="decision"></a>
## Decision
1. **The estimate.** A source costs its frame's width × height × 4 bytes: one RGBA8
   frame, the format the compositor holds. Its size is the one its frame has now, else
   the last one the engine saw it make, else the collection's canvas, so a source that
   has never run is counted at the size it most likely draws at. A warm scene costs the
   sources it draws, nested scenes' included, that no scene before it in the warm order
   draws and the program mix didn't reach in its latest plan: a source already running
   for those costs nothing more.
2. **The limit.** `studio.set_warm` gains `gpu_mib` (protocol 0.117): 0 for no limit,
   the default, or 1 to `MAX_WARM_GPU_MIB` (65536) MiB. Left out, the limit doesn't
   change, so a client that only sets `scenes` keeps it. `studio.get` reports it as
   `warm_gpu_mib`. It's kept beside the count, in the same file.
3. **How it evicts.** The count picks the warm scenes first, as before. Then the
   scenes are walked in order, adding up their estimates. The first scene that would
   take the total past the limit is left cold, and so is every scene after it: the
   budget evicts from the bottom (§7), not around a scene. The Preview scene is never
   left cold and its bytes always count. The program and a running transition's
   destination are drawn, not warm, so no budget reaches them. That keeps §7's first
   three.
4. **What the profile shows.** `profile.get` gains `warm`, the scenes the count picked,
   in order, each with its `gpu_bytes` and `cold: true` when the byte limit left it
   cold. Like `network`, it is the state now, not the window's.

<a id="consequences"></a>
## Consequences
- **Easier:** a machine with little GPU memory can keep warm only what fits, and a person
  can see in the profile what each warm scene costs.
- **Harder:** a scene's estimate changes as its sources start and change size, so a
  scene near the limit can turn warm or cold as the program changes. The walk stops at
  the first scene that doesn't fit, so a later, smaller scene doesn't fill the gap.
- **Limits that remain:** the estimate is a lower bound. It doesn't count a driver's
  copies, a filter's intermediate textures, a 3D model's meshes, mipmaps, or the
  formats a decoder holds. A source that has never run counts at the canvas size even
  if it will draw smaller.
