<!-- @doc
id = "ADR-0252"
title = "Browser pages' paints keep up on a busy GPU: the helper's copies run in the engine's GPU class, and a paint that is dropped is painted again"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0252: Browser pages' paints keep up on a busy GPU: the helper's copies run in the engine's GPU class, and a paint that is dropped is painted again

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
On Windows the browser helper copies each of a page's paints, inside CEF's callback,
into a texture it shares with the engine, on a D3D11 device of its own, and waits up to
two seconds for the copy before it gives up on the paint (ADR-0046, ADR-0048).

[ADR-0195](0195-the-engine-runs-at-top-priority.md) put the engine's process in the
`realtime` GPU scheduling class (or `high` where Windows refuses that) and, in its §5,
left the browser helper in the normal class it starts in. On a GPU the engine keeps
busy, Windows then runs the helper's copies only when the engine's work leaves room for
them. A show on Windows hardware that kept the GPU near its limit logged "a browser
page's paint could not be copied: the copy didn't finish in time", and its pages
stuttered. The pages themselves were cheap: CEF used almost no CPU and painted about
six times a second.

A paint that is lost hurts more than one frame. A page fed by the page API (ADR-0102)
paints only when its data changes, so it keeps the frame before the lost paint until
the next change, which can be seconds away. The same holds for a paint the helper skips
because the engine still holds all three of the page's slots, on every platform.

<a id="decision"></a>
## Decision
This amends ADR-0195 §5 for the browser helper. The plugin hosts stay as §5 has them.

1. **The helper's copies run in the engine's GPU class.** On Windows, the first time the
   helper makes its device, its process asks for the GPU scheduling class the engine
   asks for: the one `JSTREAM_GPU_PRIORITY` names, read the same way
   (`jstream_platform::priority`), `realtime` by default and `high` when Windows refuses
   that. Its only GPU work is these copies, which the engine waits for to show a page.
   A refusal, or a value that names no class, is sent to the engine's log once; nothing
   fails. Chromium's own processes (the GPU process that draws the page, the renderers)
   keep their classes: the helper doesn't start them by itself and has no handle to them.
2. **A paint that couldn't be copied is asked for again.** After a paint fails, the
   helper asks CEF to paint the whole view again 100 ms later, once however many fail
   before then.
3. **A paint skipped for want of a slot is asked for again.** When the engine releases a
   slot after the helper skipped a paint because the engine held them all, the helper
   asks CEF to paint the whole view again. This holds on every platform.

The two-second wait for a copy stays: a copy in the engine's class finishes in far less,
and a shorter wait would drop paints a slower GPU still finishes.

<a id="consequences"></a>
## Consequences
- On a GPU the engine keeps busy, pages' copies no longer wait for whatever the engine
  leaves. A copy is small (one page-sized texture), so it takes little from the engine.
- A page shows its latest state within about one repaint of a lost or skipped paint,
  rather than at its next change.
- A GPU that can't finish a copy for long keeps failing; each failure still holds CEF's
  UI thread for up to two seconds, and the retries add one paint per 100 ms at most.
- Not measured when decided ([R-RT-005](../RULES.md#R-RT-005)): the Windows hardware run
  that follows checks that the warning stops under the load that logged it.
- If page drawing itself (Chromium's GPU process) turns out to fall behind on a busy GPU,
  raising that process's class is a separate decision.
