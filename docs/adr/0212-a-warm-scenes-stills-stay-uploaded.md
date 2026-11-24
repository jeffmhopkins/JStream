<!-- @doc
id = "ADR-0212"
title = "A warm scene's still images stay uploaded on the program stage"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0212: A warm scene's still images stay uploaded on the program stage

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0117](0117-real-time-video-thread-model.md) §2 drops a still image's decoded CPU
copy once it is uploaded, and keeps the file to reload from (#1139). The
`jstream-stills` thread lets go of the pixels as soon as the program stage's compositor
says it has uploaded them. The stage's cache then keeps the texture while the still is
shown, so a cut back to it draws it at once. The render thread never decodes (R-RT-002),
so a program-stage draw that finds neither pixels nor a texture draws the item as
nothing. It asks for the pixels, and the still appears some frames later (#1562).

That happens whenever the cache that draws a still never uploaded it: a program stage
opened again (its last consumer went and a new one came, or the device was lost), or a
texture the cache let go of. On the Windows streaming PC it showed as stills drawn blank
for a moment on cuts, with "the program stage drew a still image as nothing" in the log
at 2560x1600, 1920x800 and other sizes (#1737).

ADR-0117 §7 keeps a warm scene ready so that showing it starts nothing: its captures
run, its sources produce, and [ADR-0177](0177-models-upload-off-the-render-thread.md)
decision 5 keeps its 3D models uploaded. Its stills had no such step. A still went on
the GPU only when a mix first drew it, which is too late for the cut.

<a id="decision"></a>
## Decision
This adds to ADR-0117 §2 and §7. The rest of both holds, including "only monitored
scenes are composited".

1. **The engine publishes the warm scenes' stills.** Beside the warm models, the
   engine publishes the still that each warm scene's source shows, as the same frame
   (the same version) that a plan of that scene draws. A still that no plan has shown
   yet is shown now, reached by nothing. A source with a frame stored by
   `set_source_frame` is left out.
2. **The program stage uploads each one it holds no texture of.** After each tick's
   draws, the render thread looks at each warm still. If its cache holds a texture, it
   leaves the still alone, so a mix's draw decides its size. If it holds none and the
   pixels are held, it uploads them whole and says so, so the thread lets go of them.
   If the pixels are not held, it asks the stills thread for them, and a later tick
   uploads them. It never decodes or waits. It uploads at most two stills per tick, so a
   stage that opens with many spreads them over its first ticks.
3. **The upload is kept like a drawn still's.** It is kept while no mix draws it, for as
   long as the still is shown (`keep_while`). So a switch to a warm scene draws its
   stills on its first frame. A draw that wants fewer texels holds the upload smaller
   once the stills thread has decoded the pixels again, as for any still drawn smaller
   than its upload.

<a id="consequences"></a>
## Consequences
- A cut to a warm scene never draws its stills blank, even on a program stage that has
  just opened, once the stage has had a few ticks to upload them.
- A still that a warm scene shows but no mix has drawn takes its whole size in GPU
  memory while it is warm, until a draw holds it smaller.
  [ADR-0150](0150-the-warm-scenes-gpu-byte-budget.md)'s estimate already counts each
  warm source's frame at its size.
- The render thread does one cache lookup per warm still per tick, and at most two
  uploads of held pixels per tick. That is the same kind of work a drawn still's first
  upload does.
- A cut to a scene that is not warm, on a stage that never drew its stills, can still
  draw them some frames late. Keeping a scene warm (the Preview scene, a pinned or bound
  scene, the upcoming scene, or a recent one) is how a show avoids that.
