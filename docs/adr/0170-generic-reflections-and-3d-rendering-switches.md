<!-- @doc
id = "ADR-0170"
title = "Generic reflections between scene surfaces, and app-wide 3D rendering switches"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0170: Generic reflections between scene surfaces, and app-wide 3D rendering switches

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

Extends [ADR-0151](0151-the-rest-of-richer-3d-scenes.md) (shadows, the floor reflection,
occlusion, volumetric fog and the scene environment) and
[ADR-0107](0107-3d-models.md) (model materials). Nothing in them changes: every new
setting defaults to what a scene draws today.

<a id="context"></a>
## Context
A 3D scene's surfaces reflect only its environment light, a gradient or a panorama
(ADR-0151 decision 9), and the `floor_reflection` filter mirrors the scene in one level
floor (decision 4). A glossy surface at any other angle can't reflect the other things in
the scene: a tilted desk can't show the bright cards above it, and a window can't carry a
faint sheen of the screens in front of it. What is wanted is the general engine feature,
driven only by each surface's material, not a special case for one layout.

Two facts about the renderer shape the design. Each model and lit card is shaded into
its own layer, then the layers are composited (ADR-0107 decision 6), so while a surface
is shaded the rest of the frame doesn't exist yet. And a scene is drawn by several views
at once (program, scene mixes, previews on their own thread, snapshots), each of which
must agree, and each new pass draws only when something reads it (ADR-0151 decision 11).

Hardware ray tracing is experimental in wgpu and missing on many GPUs, so it is not the
baseline. Replacing the renderer with a general-purpose engine would cost the shared GPU
device and zero-copy live sources the compositor relies on, for features this ADR adds
in place.

The heavier 3D effects also need to be turned off without editing scenes, so a scene
that is too heavy for a machine can still be shown.

<a id="decision"></a>
## Decision
**1. App-wide 3D rendering switches.** An application setting, like the warm budget
(ADR-0117 §7): `studio.set_rendering` sets it, `studio.get`'s `rendering` reports it
(`RenderFeatures`, protocol 0.141), and the engine keeps it across restarts in
`rendering.json` in the data folder when its host names the file. It changes no `rev`,
is never undone, emits no event and holds whatever collection is open. Each switch caps
what every scene may draw:

| Switch | Off means | Default |
|---|---|---|
| `shadows` | no light casts a shadow, whatever its `shadows` setting | on |
| `ambient_occlusion` | the `ambient_occlusion` filter passes its input through | on |
| `volumetric_fog` | the `volumetric_fog` filter passes its input through | on |
| `floor_reflections` | the `floor_reflection` filter passes its input through, and its scene isn't redrawn mirrored | on |

A switch that is off leaves its effect out of the plan, so nothing it reads is drawn
for it; turned back on, each scene gets what it asks for again. `studio.set_rendering`
takes each switch as an optional field and changes only those it names, so a client
that knows fewer switches can't turn off one it doesn't know. A switch a later version
adds is on when an older file or server leaves it out. The decisions below add their own
switches (and a quality and resolution cap) to the same setting as they land. The
desktop app shows them on a "3D rendering" page of its Settings dialog.

**2. What decides how a surface reflects.** A surface's material alone: a `scene`-lit
model's glTF roughness and metalness, and a lit card's existing `surface.roughness` and
`surface.metalness` (ADR-0151 decision 10). Unlit cards and studio-lit models reflect
nothing new. A model gains `reflections` (default `true`) to opt out. A scene gains
`reflections` (default `false`, as a light's `shadows` is), which turns decisions 3 and
4 on for its surfaces, so no existing scene changes.

**3. Reflection probes.** A new item kind, `reflection_probe`: an invisible box placed
like a light, with its size, `refresh` (`once`, or every N frames; every frame is
allowed but never the default) and `resolution` (128, 256 or 512 per face, capped by a
switch). A capture draws the scene six times from the box's centre, at face size, shaded
by its lights and environment only (no probe and no screen-space reflection), so a
capture never reads itself. Cards are sorted again for each face. The six faces are
resampled once into a panorama with mips. A surface inside the box reads it through the
same environment slot and roughness blur the scene's sky panorama uses (ADR-0151
decision 9), with its direction corrected to the box, so nearby walls line up. Where
boxes overlap the smallest wins, and edges blend. A capture's GPU memory counts in the
warm scenes' budget (ADR-0150). A probe has its own switch.

**4. Screen-space reflections.** A depth-aware scene pass, drawn only for a scene whose
`reflections` is on, on the current frame, after the scene is composited and before its
filters. It keeps no history, so it doesn't lag, and every view of a scene agrees. It
reads the picture, the view depth, and a surface buffer (normal, roughness and BRDF
weight) drawn in the same redraw that makes the view depth (ADR-0143 decision 4). Each
pixel's reflected ray marches through the view depth in a fixed number of steps set by
a quality switch, then refines its hit, and roughness picks a blurred mip of the
picture. Where it finds a hit, the pass takes out the environment reflection the
surface's shader added and puts the hit in, under the same BRDF weight. A hit fades out
near the frame's edges, behind thin objects, on rays toward the camera, and between
roughness 0.4 and 0.6, and blends with the fallback by its pixel's alpha. Bright
emissive hits are clamped. The view depth holds only mostly opaque surfaces, so
transparent surfaces such as glass take decision 3 and the environment, not this. It has
its own switch.

**5. One fallback chain.** A pixel's reflection is

```text
R = (c · R_ssr + (1 − c) · (p · R_probe + (1 − p) · a · R_env)) · B
```

with `c` decision 4's confidence, `p` decision 3's weight (0 outside every box), `a` the
scene's ambient scale and `B` the split-sum BRDF the shader already uses. Light found in
the scene isn't scaled by the ambient light, only the environment is, as today. Diffuse
light still comes from the environment.

**6. Order of delivery.** Decision 1 first, then decisions 2 and 3, then decision 4.
Each lands usable on its own.

<a id="consequences"></a>
## Consequences
- Any lit surface can reflect the rest of its scene, sharp or diffuse, with no setup but
  turning a scene's `reflections` on, and a probe where off-screen things must show.
- A heavy scene can be lightened on a slow machine from Settings, without editing it, and
  agents can do the same over the protocol.
- Screen-space reflections show only what is on screen, and not on glass; probes cover
  both, at six scene draws per refresh, which `refresh` and the resolution cap bound.
- A probe set to `once` goes stale when the live content it sees changes, so a probe
  that sees live video needs a refresh rate.
- Reflections don't show the scene's own filters (fog, a floor reflection), since
  decision 4 runs before them and decision 3's captures draw none.
