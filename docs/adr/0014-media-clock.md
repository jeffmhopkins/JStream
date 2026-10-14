<!-- @doc
id = "ADR-0014"
title = "Monotonic media clock with adaptive resampling"
kind = "adr"
status = "accepted"
-->

# ADR-0014: Monotonic media clock with adaptive resampling

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
Capture devices run on independent clocks that drift. Using one device as the master
breaks the timeline when that device is unplugged. Video mixes with unrelated frame rates
complicate the render loop.

<a id="decision"></a>
## Decision
- One monotonic media clock derived from the OS high-resolution counter (QPC on Windows).
- Every capture timestamp (WGC, MF, WASAPI) is mapped into it.
- Each audio input and output device is resampled adaptively to the engine sample rate,
  with drift estimated from device timestamps.
- One base video frame rate. Every mix runs at the base rate divided by an integer.
- Real-time threads are registered with MMCSS (audio as "Pro Audio").
- **Spike S3 in M0** measures drift and validates the resampler on real devices.

<a id="consequences"></a>
## Consequences
- Unplugging a device never stalls the engine.
- Adaptive resampling adds a little CPU per device and needs a high-quality resampler.
- Arbitrary per-mix frame rates are not possible: 30 and 60 work together, 25 and 30 don't.
