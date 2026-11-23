<!-- @doc
id = "ADR-0177"
title = "A 3D model is uploaded off the render thread, and a warm scene's stays uploaded"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0177: A 3D model is uploaded off the render thread, and a warm scene's stays uploaded

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0107](0107-3d-models.md) decision 2 has the render thread upload a model once per
version, and accepts the cost: "a copy of up to the 512 MiB cap in one tick, so a very
large model can cost the frame it first shows in".

It costs more than a frame. On a Radeon 780M (Ryzen 9 7940HS, Windows 11, D3D12), a
program stage that came to draw a scene with a large model stopped for 0.6 to 1.1 s:
no frame was composited or encoded until the upload returned, for every mix and every
output. That breaks [ADR-0117](0117-real-time-video-thread-model.md) §1, whose render
thread never copies pixels on the CPU, and [R-RT-002](../RULES.md#R-RT-002).

wgpu lets any thread create buffers and textures on a device and write them through its
queue. Such a write is kept by the queue and goes with its next submit, from whichever
thread, so the data is on the GPU before any later draw that reads it.

<a id="decision"></a>
## Decision
This replaces only the last two sentences of ADR-0107 decision 2's "Off the render
thread". The rest of ADR-0107 holds.

1. **The program stage's models are uploaded by a thread of their own.** The render
   thread asks the `jstream-models` thread for a model's upload and goes on with its
   tick. It takes the upload at the start of a later frame and draws the model from
   then on. It never waits for it. The thread makes the upload on the compositor's
   device and submits nothing.
2. **Until a model's upload is back, its item draws what it had.** An item whose model
   was never uploaded draws nothing, as one that isn't there. An item whose model changed
   keeps drawing the version before, so a model edited on air doesn't blink. The two
   versions are both held while the new one uploads; ADR-0107's cap bounds each.
3. **A refused upload is not tried again** until the model changes. Its item draws
   nothing and the refusal is logged once, as before.
4. **Every other compositor uploads where it draws.** `video.snapshot`, `video.frames`,
   frame sheets and a preview stream's own compositor are not real-time threads, and
   what they draw must not depend on when they were asked.

5. **A warm scene's models stay uploaded.** ADR-0117 §7 keeps a warm scene's
   resources resident, so showing it starts nothing. The engine publishes the model each
   warm scene's source shows, and the program stage uploads those too and keeps them
   while their scene is warm, though no frame draws them. So a switch to a warm scene
   uploads nothing, and a project's pinned, bound and Preview scenes have their models
   uploaded when it opens, before any of them is shown.

<a id="consequences"></a>
## Consequences
- The program no longer stops when a model is first drawn. A model of a scene that
  was not warm appears some frames after the rest of its scene instead.
- A warm scene's models take GPU memory while it is warm.
  [ADR-0150](0150-the-warm-scenes-gpu-byte-budget.md)'s estimate doesn't count them yet: it
  counts a source's frame, and a model's frame is a small stand-in.
- A model's version can be on the GPU twice for the length of one upload.
- The engine has one more thread per program stage while it draws models. It starts
  with the first model and ends with the stage's compositor.
- wgpu holds the queue's lock while a write is copied in. So an upload writes each
  texture level in bands of at most 4 MiB, and a tick that submits meanwhile waits for
  one band's copy, not for the model's.
