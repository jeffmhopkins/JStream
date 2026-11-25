<!-- @doc
id = "ADR-0243"
title = "The macOS GPU interop spike runs on CI's macOS runners"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0243: The macOS GPU interop spike runs on CI's macOS runners

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0063](0063-macos-platform-support.md) decision 8 brings capture, webcam and encode
frames onto the GPU through IOSurface and Metal. It leaves their synchronisation to be
specified in [ARCH › GPU pipeline](../ARCHITECTURE.md#gpu-pipeline) "once a spike on a
real Mac has proven it", and until then the platform crate offers CPU frames only.

There is no Mac hardware to run the spike on. Spike S6 ran instead on a CI macOS runner,
an Apple M1 with Metal's paravirtual GPU: with no sync it read targets before they were
written, and with the shared-event design it had no stale reads and no timeouts. ARCH ›
GPU pipeline records that run and the design built from it, and the GPU path is built
(MAC1 in `docs/progress.toml`). Decision 8 still reads as a gate that's open.

This is agent tier ([R-GOV-005](../RULES.md#R-GOV-005)). It changes where a spike is run,
not what macOS support covers.

<a id="decision"></a>
## Decision
1. **CI's macOS runners are the test bed for decision 8.** The spike that proves the
   IOSurface and Metal synchronisation, and any later check of it, runs on a CI macOS
   runner. S6's run there meets decision 8's gate, so the GPU path is the one the
   platform crate offers, as ARCH › GPU pipeline describes.
2. **What a virtual Mac can't show stays listed as untested.** ARCH › GPU pipeline keeps
   the list: Apple Silicon's own timings, whether holding webcam buffers starves
   AVFoundation's pool, and whether VideoToolbox reads a target render has since
   rewritten. A run on real hardware, if one becomes possible, checks those and needs
   no new decision.

This amends ADR-0063 decision 8 on where its spike runs. The rest of ADR-0063 holds.

<a id="consequences"></a>
## Consequences
- ADR-0063 decision 8 no longer reads as an open gate.
- A paravirtual GPU's timings stand in for real ones until a Mac run, so a timing
  problem only real Apple Silicon has can ship unseen.
