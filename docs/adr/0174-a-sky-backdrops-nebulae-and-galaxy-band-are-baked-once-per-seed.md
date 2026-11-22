<!-- @doc
id = "ADR-0174"
title = "A sky backdrop's nebulae and galaxy band are baked once per seed"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0174: A sky backdrop's nebulae and galaxy band are baked once per seed

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
A sky backdrop ([ADR-0167](0167-a-sky-backdrop-for-3d-scenes.md)) draws nebulae and a
galaxy band as fractal value noise on each pixel's direction: about 36 octaves of noise
a pixel with both on, each eight hashed lattice reads. ADR-0167 named the cost as its
first harder consequence and left it unmeasured on a GPU. It has now been measured, on
an integrated GPU (a Radeon 780M) drawing a 3D scene at 1920x1080:

- the backdrop takes 11.3 ms of a 47 ms frame, against a budget of 33 ms at 30 frames a
  second, and 10.6 ms of that is the nebulae and the band; the gradient and the stars
  are the other 0.7 ms;
- the frame is bound by how many pixels are shaded, so the cost scales with the canvas;
- going into warp the nebulae are read at 5 points along each pixel's streak, and coming
  out at 9, so a warp costs 5 to 9 times as much;
- the pipeline's fragment shader is the largest in the engine, which shows as a long
  pause when it is first built.

The noise depends on two things only: the direction on the sky, and the seed. The
camera chooses which directions a frame shows, the warp which ones a pixel blurs
together, and every other setting (the nebulae's amount and two colours, the band's
brightness, the light's intensity) scales the result. So nearly all of that arithmetic
gives the same answers every frame.

Options considered:

- **Cheaper noise** (fewer octaves, a cheaper hash, one warp less). It changes the look
  for a saving of a part: the cost stays per pixel per frame, and times 5 to 9 at warp.
- **Draw the sky at a lower resolution and enlarge it.** A quarter of the pixels is a
  quarter of the cost, still every frame and still times 5 to 9 at warp, and the stars
  would need a pass of their own to stay sharp.
- **Skip the draw when nothing changed.** Exact and worth having, but it only helps a
  still camera on a still sky, and a sky's point is that it turns and streams.
- **Spread the work over frames** (reproject last frame's sky). The warp and a turning
  camera move every pixel each frame, which is where reprojection is weakest, and it
  keeps state per view.
- **Bake what doesn't change.** Work the noise out once per seed for every direction,
  and let a frame read it.

It is agent tier ([R-GOV-005](../RULES.md#R-GOV-005)): it changes how a picture is
drawn, with no setting, protocol field, licence, security or privacy posture,
compatibility promise or spending. It amends how ADR-0167 decisions 4 and 6 draw the
nebulae and the band, and its consequences for their cost and sharpness; what they
look like, and everything else that ADR decides, stays.

<a id="decision"></a>
## Decision
**1. The nebulae and the band are baked per seed.** The first time a sky with `nebula`
or `galaxy` above 0 is drawn with a seed, the compositor works out that seed's nebulae
and band for the whole sky, into the six faces of two cube maps, each face 1024 texels
square: the same noise, style and band ADR-0167 decision 4 describes, evaluated at each
texel's direction. A frame's sky then reads the two maps along each pixel's direction,
with linear filtering, in place of evaluating the noise. Where the warp blurs the
nebulae along a streak, each of its 5 or 9 points is one such read.

**2. A bake holds shapes, and settings scale it.** What is stored leaves out every
setting but the seed:
- the nebulae are four weights (`Rgba16Float`): the light of the first colour, of the
  second, and of each one's sum of channels, which is the paler heart where the gas is
  thickest. The frame's shader multiplies them by the two colours and the amount. The
  result is the sum ADR-0167's shader computed, since that was linear in the colours;
- the band is one value (`R16Float`), multiplied by its brightness;
- the light's intensity multiplies both, as before.

So changing a colour, an amount or the intensity, from a control or from live data,
bakes nothing. Only another seed does. A sky with neither nebulae nor a band has no
bake and reads a one-texel stand-in.

**3. Bakes are kept for the seeds in use.** The compositor keeps the bakes of the seeds
drawn lately, at most 4, and lets go of one that no sky has read for 900 draws. Two
scenes with their own seeds, as in a transition between two skies, each keep theirs.

**4. Stars, the gradient and the panorama are as they were.** Stars stay procedural, a
Gaussian sized in canvas pixels, so they are points at any resolution and field of
view. The warp's streaks, aberration, Doppler shift and background glow are computed
per pixel as before.

**5. Two shaders.** The frame's shader no longer holds the noise. The bake's shader
holds it once, and its pipeline is built the first time a sky needs a bake.

<a id="consequences"></a>
## Consequences
- **Easier:**
  - a frame's nebulae and band cost two texture reads a pixel, whatever the camera does,
    and 10 to 18 at warp, in place of about 36 octaves of noise and 5 to 9 times that.
    On a software rasterizer at 1920x1080 their share of a frame fell from about 175 ms
    to about 8 ms still, 950 to 65 going into warp and 1700 to 130 coming out;
  - colours and amounts driven every frame cost nothing extra;
  - the frame's shader is far smaller, and the noise is compiled once, where the old
    shader held it twice (still and blurred).
- **Harder:**
  - **A bake costs about three 1080p frames of the old sky**, once per seed: 6.3 million
    texels of the full noise. It runs on the frame that first draws the seed, so
    changing `sky_seed` live can drop a frame on a slow GPU, where before every frame
    paid a third of that.
  - **Memory.** A bake is about 63 MB of GPU memory (50 for the nebulae, 13 for the
    band), and up to four are kept.
  - **Sharpness.** The nebulae and the band are now a picture at a fixed resolution:
    about 510 texels a radian at a face's centre and up to twice that toward its edges,
    where a 1080-line canvas has about 1300 pixels a radian at a 45 degree view. Their
    finest noise is 5 to 7 texels wide, so they stay smooth however far a view
    enlarges them, but a long lens shows them softer than the noise itself would be.
    Against the old shader the same frames differ by at most 11 of 255 in a channel at
    1920x1080, at 45 and 20 degree views (a mean of 0.06 at worst), which is under what
    is seen.
  - **Not exact.** Linear filtering between texels replaces the noise's own
    interpolation, so pixels change slightly, as above.
- **Not decided here:** a bake's resolution as a setting or chosen from the canvas;
  baking over several frames or off the render thread; mips for a bake, which a canvas
  much smaller than a face would want; baking the sky a model reflects.
