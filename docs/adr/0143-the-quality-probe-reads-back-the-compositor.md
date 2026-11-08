<!-- @doc
id = "ADR-0143"
title = "The quality probe compares the compositor's readback with the decoded stream"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0143: The quality probe compares the compositor's readback with the decoded stream

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0132](0132-bench-gates-views-and-quality-probe.md) §3 plans `quality.probe`: the
stream's encoder keeps a copy of the next frames it takes and the packets it makes of
them, and the engine answers PSNR and SSIM between each frame and its decoded picture.
Sizing it turned up two blockers (#1337):

- Most real encode paths take GPU frames. A Windows hardware encoder, or VideoToolbox, is
  fed NV12 encode targets on the GPU, so the encoder has no CPU copy to keep. Copying
  them back is a per-platform readback (D3D11 staging, IOSurface) in each platform crate.
- Linux had no H.264 decoder. [ADR-0140](0140-source-types-and-decoding-scope-v3.md) now
  lets Cisco's OpenH264 decode the engine's own H.264 streams, so it has one.

The render thread already reads the program back to the CPU for every session on the
CPU path and for the virtual camera. A readback lands a tick or two after its draw, with
the time it was drawn for, and that is the `pts` the GPU frame of the same draw goes out
with. So the compositor's readback is the frame the encoder was given, for either path,
without any platform code.

<a id="decision"></a>
## Decision
1. **The reference is the compositor's frame, as the render thread pushes it.** Each
   output's encoder session gets a probe tap. While a probe is armed, the render thread
   copies every frame it pushes to that session, with its `pts`: the RGBA8 frame itself
   on the CPU path, and on the GPU path the readback of the same draw, which it reads
   back only while a probe is armed. The thread never blocks on the probe or frees a
   copy: it copies only against credit the probe grants (a few frames between two of the
   probe's reads), under a lock it only tries, and the probe disarms the tap before it
   drops the receiving end. Nothing is copied or read back while no probe is armed.
2. **The packets come from a subscription to the session**, which starts at the next
   keyframe and asks for one. A `jstream-quality-probe` thread decodes them with the
   platform's decoder (NV12 out, software preferred), meets each picture with its
   reference by `pts` in microseconds, whichever comes first, and lets go of either
   after 16 frames without a partner. Both are compared as NV12 in BT.709 limited range
   at the encoded size, the reference converted by `jstream_platform::nv12`, as the CPU
   encoders convert it. It answers luma PSNR, PSNR over every sample, and luma SSIM over
   8 × 8 windows 4 samples apart, per frame and on average. Two identical pictures
   report 100 dB, the ceiling, since JSON has no infinity.
3. **The stream is a tagged object, and this decision probes outputs only:**
   `{"type": "output", "output_id": …}`. A preview stream's H.264 session is the
   preview hub's own and is drawn only when its picture changes, so probing it needs a
   tap of its own on the hub. It is a later variant of the same object; until then the
   ADR-0132 bench has one probe floor, for the recording. One probe of a session runs at
   a time; a second answers `action_failed` at once. A probe answers within 10 seconds
   or fails with `action_failed`, naming how many frames came back.
4. **The bench probes the recording after every switch**, three frames each, and gates
   the worst frame of all of them (`min_probe_psnr_y`, `min_probe_ssim_y`) and the
   switches whose probe failed (`failed_probes`). The defaults are 35 dB and 0.95, and
   the lavapipe gates file loosens them to 30 dB and 0.90 for CI's software run on
   OpenH264.

<a id="consequences"></a>
## Consequences
- One probe works on every platform and both encode paths, with no readback code in the
  platform crates. It measures what the encoder did to the compositor's frame, including
  the GPU's RGB-to-NV12 conversion on the GPU path, which the CPU reference converts with
  the CPU's coefficients: that difference counts against the GPU path's score.
- A GPU session costs one readback a frame while a probe is armed, the same as a CPU
  session always costs; nothing otherwise.
- Linux's CI bench now gates picture quality, since OpenH264 decodes there (ADR-0140).
  ADR-0132's consequence that Linux answers `action_failed` and skips the quality gates
  no longer holds.
- A probe of a preview stream, and the preview floor ADR-0132 §4 names, wait for the
  hub's own tap.
