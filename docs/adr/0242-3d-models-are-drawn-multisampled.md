<!-- @doc
id = "ADR-0242"
title = "3D models are drawn multisampled"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0242: 3D models are drawn multisampled

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
A 3D model is drawn into a layer of its own
([ADR-0107](0107-3d-models.md) decision 6), transparent where the model isn't, which
its filters run over before its scene places it. Every target of that draw had one
sample a pixel, and nothing smoothed it after: a model's slanted edges were stepped.
Thin, bright, slanted geometry showed it worst. A strip of emissive light a pixel or two
wide broke into a staircase, and a bloom over the layer then glowed the staircase.
[ADR-0050](0050-3d-transforms-rendering.md) decision 6 smooths the edges of 3D-placed
flat items with analytic coverage, but a model's triangles have no polygon outline to
measure a distance to, and ADR-0050 says nothing of models.

The candidates were:
- **A post-process pass on the layer (FXAA or SMAA).** It finds edges in the finished
  picture by contrast. The layer is transparent around the model, and what is behind
  it is only known when the scene places it, so a silhouette's edge is in the alpha,
  which such a pass doesn't smooth well. A strip thinner than a pixel is already broken
  into dashes in a one-sample picture, and no pass over it can join them again.
- **Temporal anti-aliasing.** It needs each pixel's motion and a history of earlier
  frames for every model layer, which may change size from frame to frame
  ([ADR-0197](0197-3d-models-draw-with-fewer-pixels-when-the-gpu-is-short-of-time.md)),
  and it smears or ghosts under fast camera moves and warps.
- **Multisampling (MSAA).** The rasteriser measures each triangle's coverage at several
  points of a pixel, and the model's fragment shader still runs once a pixel. A thin
  strip covers some of a pixel's samples and stays continuous; a silhouette's texel
  holds the model's share of it, with that share as its alpha, exactly what placing a
  premultiplied layer needs; and nothing depends on earlier frames.

<a id="decision"></a>
## Decision
1. **A model's layer is multisampled.** It is drawn with several samples a pixel, and
   its colour resolves into the layer as the draw's pass ends, so the layer's filters
   (a bloom among them) run over the smoothed picture. The model's depth prepass and its
   depth target are multisampled with it. A lit card's quad, laid flat over its whole
   layer, has no edge inside it and is drawn with one sample.
2. **The samples are a rendering switch.** The rendering switches
   ([ADR-0170](0170-generic-reflections-and-3d-rendering-switches.md) decision 1) gain
   `model_antialiasing`: the samples each pixel of a model's layer is drawn with, one of
   1 (off), 2, 4 and 8 (`MODEL_ANTIALIASING`). 4 is the default, what a file or client
   that leaves it out reads as: it is the count every GPU draws, and the usual balance of
   smoothness and cost. `studio.set_rendering` refuses any other count, and the
   protocol's minor version rises (0.196). A count the GPU can't draw for the layer's
   formats draws with the most it can below it, so a setting made on one machine draws
   on another; the engine asks its device for the adapter's own format features so it
   can use 2 and 8 where the adapter has them.
3. **A surface buffer keeps one sample a texel.** A model drawn with its surface
   buffer beside its layer ([ADR-0170](0170-generic-reflections-and-3d-rendering-switches.md)
   decision 4) draws the buffer multisampled too, as a pass's targets must share one
   count, and it resolves to the one sample of each texel that is nearest (its largest
   1 / w), not to their average: a normal or depth half one surface and half another is
   neither, as the buffer's merge requires. A texel none of whose samples saw the model
   is zero.
4. **A probe captures single-sampled.** A reflection probe's faces are small and read
   blurred (ADR-0170 decision 3), so the models it draws are drawn with one sample; the
   frame's are drawn with the switch's count again after it.

<a id="consequences"></a>
## Consequences
- Models' slanted and thin edges are smooth in every scene, at the default, with no
  scene asking for it.
- At 4 samples a model's layer costs four times its colour and depth memory while it
  draws (about 100 MB at 1080p), and the GPU's resolve; the shading stays once a pixel
  but at a triangle's edge. A modest GPU short of time lowers the models' size
  ([ADR-0197](0197-3d-models-draw-with-fewer-pixels-when-the-gpu-is-short-of-time.md))
  sooner; 2 samples or off are there for it.
- Multisampling smooths geometry's edges only. A masked material's cut-out edges and
  detail inside a texture are as before; textures are smoothed by their mips.
- The Settings dialog's 3D rendering page sets it as 3D anti-aliasing: Off, 2x, 4x or
  8x multisampling.
- An older engine ignores `model_antialiasing`; an older client leaves it as it is.

Tests: `model::tests::golden_model_multisampling_smooths_a_slanted_edge`,
`a_sample_count_steps_down_to_one_the_device_draws` and
`a_multisampled_surface_matches_the_single_sampled_one_inside`;
`tests::rendering::the_model_antialiasing_is_one_of_the_counts` (engine) and
`rendering::tests::the_model_antialiasing_is_one_of_the_counts` (UI).
