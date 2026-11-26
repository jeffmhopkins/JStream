<!-- @doc
id = "ADR-0254"
title = "The browser helper waits for its paint copies off CEF's UI thread"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0254: The browser helper waits for its paint copies off CEF's UI thread

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
On Windows, [ADR-0048](0048-sandboxed-browser-helper.md) §3 has the helper copy each
accelerated paint's texture into one of the page's three shared textures and wait for
the copy before it tells the engine. The helper waited inside CEF's `OnAcceleratedPaint`
callback, on CEF's UI thread.

Every page's paints arrive on that one thread, and it also paces the pages' frames
(windowless pages are driven from it). Measured on Windows hardware with seven pages,
each about a 1920×1080 texture and asking for 30 frames a second, the pages got 19 to
22 paints a second, none skipped and none failed downstream. Each copy held the UI
thread for 3 to 5 ms on average and up to about 21 ms, about 620 ms of every second in
all. The page with the lightest paints reached 30. The engine's side took each frame in
about 1 ms. So the pages were held back by the helper's own waits, which grow with the
number of pages and their size, not by the engine.

CEF hands the texture back to its pool when the callback returns, with no fence and no
keyed mutex. Waiting there for the copy to finish is what guaranteed it read the frame
CEF painted: once the callback returned, the copy was done, before Chromium could draw
into that texture again. Returning before the copy has run gives that guarantee up. The
copy is still submitted and flushed before the callback returns, so it is on the GPU
before Chromium learns the texture is free, but nothing orders it against Chromium's
later work in another process.

<a id="decision"></a>
## Decision
This amends ADR-0048 §3 on where the helper waits.

1. Inside the callback the helper opens CEF's texture, submits the copy into the
   page's next free slot, signals a D3D11 fence after it, flushes, and returns. The slot
   is held from then on.
2. One thread of the helper's, the copy thread, waits for each copy's fence value, up to
   two seconds as before, and only then tells the engine of the frame. A copy that times
   out frees its slot and is reported as before.
3. The copy thread takes its jobs in the order the UI thread queued them. A reload's
   `Reloading` goes through it too, so the engine still sees every frame of the old
   document before it, as #1151 relies on.
4. A slot's shared texture, and its handle, stay open until the copy thread has told the
   engine of it, even if a new size or device remakes the page's textures meanwhile.
5. Each page keeps up to eight of CEF's textures open on the helper's device, so a
   texture CEF's pool hands over again isn't opened again. Each is recognised by the
   object a duplicate of CEF's handle names, compared with the handle CEF passes
   (`CompareObjectHandles`), so a new handle to a pooled texture finds it and a handle
   value reused for another texture doesn't. They are dropped with
   the device.
6. A slot is now held from the copy's submission, a little longer than before, so a
   paint can find all three held. When the engine then frees one, the helper asks CEF to
   paint the page again (on every platform), so a page that only paints when its data
   changes doesn't keep the frame before the skipped paint.

<a id="consequences"></a>
## Consequences
- CEF's UI thread spends only the time it takes to submit a copy on each paint, so pages
  are no longer capped by one another's copies. A first run with the wait moved but
  every texture opened anew raised the pages from about 19 to 23 paints a second and
  still spent 2 to 5 ms of the UI thread per paint, which is what keeping the textures
  open is for.
- A frame reaches the engine a little later than before by the hand-off to the copy
  thread, which is far less than a frame.
- The copy is no longer guaranteed to read the frame CEF painted. If Chromium draws into
  the same pooled texture again before the GPU has run the copy, the engine gets that
  page's frame torn between two paints, or a later frame early, for one frame. This is
  accepted because the window is small: the copy is flushed before the texture goes back
  to the pool, it takes well under a millisecond on the GPU, and Chromium only reuses the
  texture after drawing into the pool's other textures first, a frame or more later.
  On the streaming PC no torn or out-of-order frame was seen with seven pages at 25 to
  30 paints a second. If one shows, the copy thread can hand the texture back late
  instead (CEF has no hook for that today), or the wait can move back for that page.
- The helper's D3D11 device now needs fences (`ID3D11Device5`, Windows 10 1703 or
  later). On an older Windows, making the device fails and pages deliver no frames, with
  the reason in the engine's log.
- Linux and macOS paint in software, written within the callback, and are unchanged.
- Measured on the same Windows machine and pages, with the engine at about 31 fps and no
  frame over budget ([R-RT-005](../RULES.md#R-RT-005)): each paint now holds CEF's UI
  thread 0.3 to 0.7 ms on average, opening CEF's texture 0 to 0.4 ms of it, and the six
  data-driven pages paint 25 to 27 times a second, up from 19 to 22, with none failed. The
  lightest page stays at 30. What keeps the rest below 30 is upstream of the helper
  (Chromium's own pacing of those pages and their scripts), not the copies.
