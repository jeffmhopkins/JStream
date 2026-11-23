<!-- @doc
id = "ADR-0189"
title = "Screen-space reflections march at half the picture's size"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0189: Screen-space reflections march at half the picture's size

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0170](0170-generic-reflections-and-3d-rendering-switches.md) decision 4 says each
pixel's reflected ray marches through the view depth, and
[ADR-0185](0185-how-reflections-landed-off-by-default.md) records the pass as it was
built: one full-size draw in which every pixel of a reflecting surface takes 16, 32 or 64
steps, five halvings and up to 12 taps of the picture.

ADR-0185 measured that pass on an integrated GPU at 1080p30: about 10, 11 and 13 ms of GPU
time a frame at Low, Medium and High, in a frame with 33 ms to spend. At High a 3D scene
that held 30 fps without the pass no longer did. The march is the part of that cost that
grows with the picture's size and the step count.

A reflection on anything but a mirror is already blurred by the surface's roughness, and
the surfaces a ray starts from are mostly smooth over a pixel or two. The march can
therefore be drawn for fewer pixels than the picture has, as long as its result is not
spread from one surface onto another.

<a id="decision"></a>
## Decision
ADR-0170 decision 4 and ADR-0185 stand, with this change: **the march is drawn at half
the picture's size, and a second draw spreads it over the picture.**

- **The march** (`screen/trace.wgsl`) is drawn into a layer half the picture's width and
  height, rounded up. Each of its texels stands for a block of two by two pixels and
  marches the ray of one of them, read from the full-size surface buffer and view depth:
  one corner of the block, the other corner on every other texel, so a row or column of
  blocks doesn't always march the same line of the picture. It marches against the
  full-size view depth and reads the full-size picture, as before. It writes what the ray
  found times how sure the hit is, premultiplied, and nothing where it found nothing.
- **The resolve** (`screen/resolve.wgsl`) is drawn at the picture's size. A pixel that
  could not have marched (not replaceable, rougher than 0.6, covered by something nearer)
  passes through, as before. Any other pixel takes what the marched pixels of the four
  nearest blocks found, each weighed by how near it is on screen, by how close its point
  lies to this pixel's own plane (nothing at 2% of the pixel's distance off it), and by
  the fourth power of the cosine between the two normals. The weighted mean replaces the
  pixel's environment reflection under the pixel's own BRDF. A pixel with no marched
  pixel of its own surface nearby keeps its environment's reflection.
- The steps, the hit test, the blur over the hit and every fade are ADR-0185's. The
  switch, its quality counts and the settings do not change, and nothing is added to the
  protocol or to files.
- The pass still keeps no history: the pattern of marched pixels is fixed on the screen,
  so a still scene draws the same picture every frame and every view of a scene agrees.

A march at half the size in one direction only, a checkerboard filled from the previous
frame, and a per-quality choice of size were considered. The first saves half as much
for the same resolve. The second needs history, which ADR-0170 rules out. The third
would give the quality switch two meanings; a full-size march can be added to it later
as a count above High if anything needs it.

<a id="consequences"></a>
## Consequences
- The march covers a quarter of the pixels it did. The resolve adds one full-size draw
  of up to nine texture reads a pixel, and the pass keeps one more layer, half the
  picture's size (about 4 MB at 1080p).
- Measured on the integrated GPU ADR-0185 measured, one reflective 3D scene at 1080p30
  over 15 seconds: the frame's GPU time went from about 28.4, 29.6 and 32.0 ms at Low,
  Medium and High to 26.5, 27.0 and 27.7 ms, against 20.3 ms with the pass off. High,
  which had dropped to 28.6 fps, held 30. The saving grows with the step count because
  only the march is quartered. Motion was not looked at, only stills. Nothing here turns
  the switch on by default.
- A reflection's detail is that of one ray in four. On a mirror-smooth surface a
  reflected edge is softer than before, by about a pixel. The fades by roughness and
  toward the viewer are those of the marched pixels, so where roughness varies from
  pixel to pixel between 0.4 and 0.6, the fade varies over two pixels, not one.
- A feature thinner than two pixels whose own block marched another surface has no
  screen-space reflection and shows its environment's, where before it showed a hit.
- An effect pack's or a plugin's shaders are not affected: the pass is the engine's own.
