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
keyed mutex, so the copy must be submitted before then. Waiting for it there isn't
needed for that: once flushed, the copy is queued on the GPU ahead of any frame Chromium
draws into that pooled texture later.

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

<a id="consequences"></a>
## Consequences
- CEF's UI thread spends only the time it takes to submit a copy on each paint, so pages
  are no longer capped by one another's copies. A first run with the wait moved but
  every texture opened anew raised the pages from about 19 to 23 paints a second and
  still spent 2 to 5 ms of the UI thread per paint, which is what keeping the textures
  open is for.
- A frame reaches the engine a little later than before by the hand-off to the copy
  thread, which is far less than a frame.
- Chromium could, in principle, draw a later frame into a pooled texture before the GPU
  has run a copy queued earlier from another process, which would show that page's next
  frame early for one frame. Nothing orders the two processes' GPU work, before this
  change or after it; the copy is flushed before the texture goes back to the pool, as it
  was.
- Linux and macOS paint in software, written within the callback, and are unchanged.
- Not measured when decided ([R-RT-005](../RULES.md#R-RT-005)): the Windows hardware run
  that follows compares each page's paints a second with the numbers above.
