<!-- @doc
id = "ADR-0190"
title = "Cheaper sampling in 3D scenes: fewer reads where they change little"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0190: Cheaper sampling in 3D scenes: fewer reads where they change little

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
A 3D scene's frame on an integrated GPU is bound by how many pixels are shaded and how
many texture reads each takes, not by its triangles. Measured at 1080p30 on one, a
reflective scene with one model spent about 28 ms of a 33 ms frame with screen-space
reflections at their highest quality, 12 ms of it shading the model.

Several passes read more than their result needs. Each is described by an accepted ADR
down to its tap count:

- [ADR-0151](0151-the-rest-of-richer-3d-scenes.md): a shadow is 12 samples of its map at
  every pixel (decision 3); ambient occlusion samples at every pixel (decision 5); the
  sky lights a matte surface through the same ring of reads as a reflection (decision 9).
- [ADR-0170](0170-generic-reflections-and-3d-rendering-switches.md) and
  [ADR-0185](0185-how-reflections-landed-off-by-default.md): a probe's capture replaces
  the sky's reflection, which is read all the same; and a scene's surface buffer is
  merged from its models' layers even when there is one model.
- [ADR-0107](0107-3d-models.md): a model's five material textures share one sampler at
  16× anisotropy.

<a id="decision"></a>
## Decision
Those ADRs stand. Each of these replaces only how many reads a pass takes.

1. **A shadow reads five samples first**: the middle of its disc and four points of its
   rim, turned per pixel as the disc is. Where all five are lit or all five are
   shadowed, that is the answer. Only where they disagree, at a shadow's edge, are the
   12 samples read.
2. **The sky lights a matte surface with one read** of its panorama, a level above the
   one the widest ring reads, where one filtered texel spans about what that ring's nine
   reads did. A gradient sky is worked out, not read, and is unchanged.
3. **The sky's reflection is not read where a probe replaces it**: where the surface is
   wholly inside its probe's box and the capture saw something all along the ray, its
   alpha at least 0.999.
4. **The maps that shape the light are read at 4× anisotropy**: a material's normal,
   metal-roughness, occlusion and emissive textures, through a second sampler. Its base
   colour keeps 16×.
5. **Ambient occlusion samples at half the picture's size.** Its first pass samples for
   one pixel of each block of two by two, alternating the corner, against the full-size
   depth. Its second pass, at the picture's size, averages the 4 × 4 texels around each
   pixel that are about as far as the pixel's own depth, as it did the 4 × 4 pixels. A
   pixel with none as far is left as it is; it took its own share before.
6. **One model's surface is the scene's surface buffer.** When a single model reflects a
   scene and drew its surface with its layer this frame, at the buffer's size, the
   screen-space reflection pass reads those layers. With two or more, or a model whose
   surface must be drawn again, the buffer is cleared and merged as before.

No setting, switch, protocol message or file changes.

<a id="consequences"></a>
## Consequences
- Items 3 and 6 draw the same picture: 3 to within a thousandth of the sky's reflection,
  6 exactly.
- Items 1, 2, 4 and 5 change the picture slightly. A shadow narrower than its filter's disc that misses all
  five first samples is not softened into the pixels beside it; the per-pixel turn makes
  that a little noise at such a shadow's rim, not a missing shadow. A matte surface under
  a panorama takes a slightly different average of it. A normal or roughness map seen at
  a grazing angle is a little softer. Ambient occlusion has the detail of one pixel in
  four.
- How much GPU time they save together is not yet measured on the integrated GPU the
  context's numbers come from.
- Depth of field on a model ([ADR-0165](0165-depth-of-field-on-models.md)) was measured
  there at 9.6 ms more a frame and is left as it is. Reading its gather with fewer taps
  for a small blur was tried with these and showed as a dot pattern near the focus, where
  48 taps read a disc of a few pixels several times over and anything less does not
  cover it evenly. It needs a gather at half size, with its own decision.
