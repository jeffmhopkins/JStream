<!-- @doc
id = "ADR-0117"
title = "Real-time video thread model: a compositor-only render thread, producers that hand it GPU-ready frames, and one always-on stage"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0117: Real-time video thread model: a compositor-only render thread, producers that hand it GPU-ready frames, and one always-on stage

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
On the alpha.4 Windows run (#1191) a scene with a webcam, a media file, text and a colour
dropped about 7 ticks a second while the process used 14% of a 16-thread CPU. One thread
was saturated: the render thread. JStream has to hold heavy scenes (many browser and
media layers, item effects, 3D passes) and GPU encoding at once, so the question in #1221
is not how to make that one loop faster, but which work belongs on which thread.

The model in [ARCH › Threads](../ARCHITECTURE.md#threading) already gives audio its own
real-time threads, and those follow the pattern pro audio hosts and OBS use. The video
side does not. Reading the code as built:

1. **Per-pixel CPU work runs on the render thread.** Each changed CPU frame is
   premultiplied into linear `Rgba16Float` on the CPU (`premultiply_to_f16`), and each
   NV12 webcam frame is converted to RGBA pixel by pixel (`rgba_from_surface`), both
   inside the tick. #1193 made both loops faster; they are still on the thread with the
   deadline, and they grow with every CPU source added.
2. **The render thread waits for the GPU.** A mix read back for a CPU-path encoder or a
   virtual camera is mapped with a blocking `poll(wait_indefinitely)` in the same tick
   (`map_snapshot`). The CPU and GPU never overlap: the tick is CPU time plus GPU time plus
   the copy, then one `to_vec` per CPU session.
3. **Live frames travel through the control plane.** A media file's or a page's new
   frame, and each animation's next frame, makes the `jstream-sources` thread re-plan the
   program under the collection lock and publish a new render snapshot (`live.rs`). A
   page asks for that every program frame; N out-of-phase animations ask N times a frame
   (#1138). Planning is control-plane work (it competes with control calls for the lock)
   used as a frame transport.
4. **The preview composites the program a second time.** The `jstream-preview` hub has
   its own compositor on its own device, runs its own display captures, draws the program
   again, reads it back with a blocking map, and encodes it on the CPU (a software H.264
   session, or JPEG) on that same thread, waiting up to 20 ms for each packet.
5. **Four GPU devices.** The program stage, the preview hub, the snapshot thread and the
   browser host each open their own (#1194), so a browser page drawn on one is nothing on
   the others, and each output start moves the pages to a new device (#1183).
6. **The stage exists only while an output runs.** Before the first `output.start`
   nothing runs the program; the preview stands in. Starting an output builds the stage,
   opens a device and starts its captures, which is when sources move and placeholders
   show (#1205).
7. **No thread asks for priority, and the loops poll.** The render and mixer loops sleep
   1 ms at a time until their tick is due. Only the WASAPI device threads and plugin hosts
   register with MMCSS; the mixer, which every output's audio passes through, does not.

What others do, for the parts that matter here:
- **OBS** runs a graphics thread that only uploads and draws, a video output thread that
  feeds encoders, an audio thread, and an encode thread per encoder. Asynchronous sources
  decode on their own threads and hand over YUV planes, which the graphics thread uploads
  and converts with shaders. Readback for CPU encoders cycles through several staging
  surfaces and maps the one written frames earlier, so the graphics thread never waits for
  the GPU. Texture encoders (NVENC, AMF, QSV) take shared textures with no readback. Its
  known weakness is ours: CPU work left on the graphics thread stalls the frame.
- **vMix and similar switchers** keep every input decoded and running, so a cut never
  waits on a source to start, and pay for it in memory and cores.
- **GStreamer** puts a queue, and so a thread, between stages, so a slow stage drops or
  blocks visibly instead of stalling everything.
- **Game engines** split a game thread, a render thread and the GPU, keep two or three
  frames in flight, stream uploads through ring buffers, and run CPU work on a job system
  whose workers the frame thread never waits on mid-frame.

<a id="decision"></a>
## Decision

### 1. The render thread composites and submits, nothing else
Per tick it adopts the render snapshot and live values, reads each source's latest
GPU-ready frame (§2), records and submits the composite of every due mix, the NV12
conversion of each GPU-path encode target, and the copy of each mix read back into a
staging buffer (§4). It never:
- touches pixels on the CPU (no conversion, premultiply, scaling or copy of a frame);
- waits for the GPU (no blocking `poll`, no `map_async` waited on in the tick; fences are
  polled without blocking, as the resource cache already does);
- waits on a source, an encoder, a reader, the collection lock or the control side.

Up to two frames are in flight: the thread records frame N while the GPU runs N − 1. A
tick whose GPU work from two frames back is still running is skipped and counted in
`render_ticks`, as an overrun is today (R-RT-004).

### 2. Producers hand over GPU-ready frames, on their own threads
Each live source (capture, webcam, media file, page, painted source) produces on its own
thread, as captures already do, into a **frame slot**: a single-slot "latest frame"
mailbox like a capture's. A frame in a slot is one of:
- a **texture on the engine's GPU context** (§5): a WGC capture, a page painted on the
  context, and later a media frame decoded on the GPU;
- **planes in a GPU-uploadable layout**: straight sRGB RGBA8 or BGRA8, or NV12, written
  by the producer into a staging ring of its own (`Queue::write_texture` or a mapped
  buffer, which wgpu allows from any thread) and converted on the GPU by the composite's
  first pass: NV12 to RGB, sRGB decode and premultiply. The bits match today's CPU
  conversions (tests compare them, as the #1193 tables do), so nothing looks different.

The render thread only samples a slot's frame, or records the copy out of the producer's
ring. A source that cannot hand over a GPU-uploadable layout converts on the shared
worker pool (§6), never on the render thread.

Still images and painted sources (text, timers, scopes) are uploaded once per change at
the size they are drawn, with headroom as [ADR-0073](0073-sources-drawn-at-their-shown-size.md) does, and the
decoded CPU copy is dropped once the upload is done, keeping the file to reload from
(#1139). Their textures stay premultiplied linear `Rgba16Float`, so edges blend as they do
now.

### 3. Frames leave the control plane
A new frame no longer re-plans. The render snapshot names each source's frame slot, and the
plan runs only when the scene changes (a batch, undo, a live source built or rebuilt, a
transition), as [ARCH › Threads](../ARCHITECTURE.md#threading) intends. Animated sources
are ticked by their producer on the program's frame grid on the media clock, so N
animations cost N paints per frame at most, not N plans (#1138). `video.snapshot`,
`video.frames` and frame sheets keep asking sources for their frame at an explicit time,
so deterministic renders stay deterministic.

### 4. Output preparation leaves the render thread
- **GPU-path encoders** keep today's path: an NV12 encode target from a ring, written by
  the render thread's submit and handed over with its fence.
- **Everything read back** (CPU-path encoders, virtual cameras, preview encoders on the
  CPU) is converted to NV12 on the GPU first, halving the bytes, and copied into one of
  three staging buffers per mix. A `jstream-readback` thread per stage maps the buffer
  written two ticks earlier, hands it to every consumer, and returns it to the ring. A
  consumer that is behind drops and counts (R-RT-004). The pts is the tick's, so A/V
  sync holds; read-back consumers see two frames more latency.

### 5. One engine GPU context
The engine opens one `GpuContext` (adapter, device, queue) and lends it to every
compositor (program stage, preview, snapshots) and to the browser engine, reopening it
only after a device loss, which every holder then follows (#1194). Pages never change
device, and every compositor can draw every source. A device loss still falls back as
[jstream-core › Runtime](../../crates/jstream-core/README.md#runtime) describes, for the
whole engine at once.

### 6. One always-on stage
The program stage (captures, producers, render thread, mixer) starts with the engine's
first consumer (a preview stream, a projector, an output or a virtual camera) and stops
with the last. Outputs attach to the running stage and never rebuild it, so starting an
output moves no source and opens no device. The stage's render thread also composites:
- **The program**, at the canvas size, every tick.
- **Monitored scenes**: the studio's Preview scene and any scene a preview stream or
  projector shows, as extra mixes at their stream's size. They draw after the program,
  at a divisor of the base rate when the tick is over budget, and a monitored mix that
  misses its tick is skipped and counted. The program never waits for one.

The `jstream-preview` hub stops compositing. It becomes an encode-and-send thread: it takes
the program's or a monitored mix's frame (an NV12 target when its encoder is on the GPU,
else the readback of §4), encodes it on a session of its own whose encode thread does the
work, and forwards packets as they come, never waiting for one. Each display is captured
once. A scene shown by a stream is drawn once per tick however many streams show it.

A small **worker pool** (`jstream-work-*`, at most half the cores, at least two) takes
CPU work that has no thread of its own: a producer's fallback conversion, image decodes,
JPEG and PNG frames. No real-time thread submits work to it and waits.

### 7. Warm scenes
A scene is **warm** while its sources run and their resources stay resident, so showing
it never starts anything. Warm, in order of priority: the program; a running transition's
destination; the studio's Preview scene; scenes pinned by the user or bound to a hotkey or
trigger; then the most recently shown scenes. Warm sources keep producing (a media file
held at its show position with its first frame decoded, a page loaded and painting, a
webcam open). A budget (a count of warm scenes and an estimate of GPU bytes) evicts from
the bottom of that order, never the first three. The budget and the pin live in
Application Settings (#1207). Only monitored scenes (§6) are composited; a warm scene that
nobody watches costs its producers, not the render thread.

### 8. Audio never waits on video
The mixer keeps its own clock-driven thread under [R-RT-001](../RULES.md#R-RT-001), with
no lock, channel or snapshot shared with the render, readback or preview threads; what
the render thread reads from audio (the motion modifiers' levels, ADR-0100) comes from
the lock-free scopes. The mixer registers with MMCSS "Pro Audio" on Windows, as the device
threads do.

### 9. Pacing and priority
The render and mixer threads sleep until their next deadline on the media clock, not in
1 ms polls. On Windows the render thread registers with MMCSS "Playback". Everything else
(producers, readback, preview, encoders, the worker pool) runs at normal priority.
Whether the compositor's GPU queue needs a higher GPU priority is left to the bench.

### 10. Every step proves itself on the bench
The build lands in steps, each with a `jstream bench` report (#1220) showing fps, drops at
every stage, switch placeholders and audio underruns no worse than before, and better
where the step aims. Linux CI runs the bench on software rendering for regressions; the
alpha.5 Windows runs gate the release. A claim of a speed-up says where it was measured
(R-RT-005).

<a id="consequences"></a>
## Consequences
- The render thread's cost per tick is the GPU commands it records, which grows with the
  scene's layers and passes, not with how many CPU sources there are or their sizes. CPU
  conversion spreads over producer threads and the pool, so a 16-thread machine's cores
  do the work instead of one.
- The preview is the program's own frame, at the program's resolution when the stream
  asks for it, so it cannot differ from what goes out, and costs one GPU NV12 conversion
  and an encode instead of a second composite. That serves the alpha.5 preview bar
  (lossless-looking, no "Starting preview" on a switch).
- Running the stage with no output means a previewing engine composites at the base rate
  all the time. That is the cost of a preview that matches the program, and it is what
  OBS does.
- Read-back consumers (CPU encoders, virtual cameras) gain two frames of latency; GPU-path
  encoders gain none.
- Warm scenes cost memory, threads and decode while they are warm. The budget bounds it,
  and the profile reports what each warm scene holds.
- [ARCH › Threads](../ARCHITECTURE.md#threading) changes: the render thread's row, the
  preview's row (encode-and-send, no compositor), a readback thread, the worker pool, and
  the stage's lifetime. Each build step edits it as it lands.
- [R-RT-002](../RULES.md#R-RT-002) gains its render-thread half: no per-pixel CPU work and
  no wait on the GPU. It is added with the step that makes it hold, with a test that fails
  on a blocking map in the tick.
- Deterministic renders (`video.snapshot`, `video.frames`, frame sheets) keep their own
  path: they ask sources for frames at explicit times on a compositor of the shared
  context, not the real-time slots.
- The build order and lanes are in issue #1221; none of it changes the protocol except the
  profile and bench additions of #1220, which take the protocol queue's slot.
