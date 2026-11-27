<!-- @doc
id = "ADR-0256"
title = "Models in one 3D scene share a depth"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0256: Models in one 3D scene share a depth

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0107](0107-3d-models.md) decision 6 draws each model into its own layer with a depth
buffer cleared for that model, then composites the layer like a card. Decision 7 gives the
scene one depth target, but the model's own layer is not tested against it: the layer
writes its depth afterwards, and only items drawn later (cards) test it. Models therefore
order by the depth of their box centres, the same rule [ADR-0094](0094-scene-cameras.md)
uses for cards. A `share_space` group ([ADR-0164](0164-3d-groups-share-their-scenes-space.md))
does not change it: its children are drawn as items of the scene, each still its own layer.

That is wrong for two solids that pass through each other. A cube and a rod along the
depth axis show whichever item's centre sorts nearer: the whole rod over the cube, or the
cube over the whole rod, including the end that sticks out in front. The same thing
happens to a ship and separate plume models. The plume is its own item so it can fade and
use an additive blend; from most angles it draws over the hull. Baking a black copy of the
hull into each plume, and setting the item to additive, hides the plume inside that copy
and adds nothing for the black. It is a workaround for the missing test.

The layer cannot go away. A model's filters, its item opacity and its blend run on the
layer after the model is drawn, and a material's drives (an emissive strength, for one)
are applied in that draw. Windowing a layer to the model's on-screen box sizes that layer;
it does not put two models in one depth. Cards still have no depth of their own.

The choice is the default. A 3D scene is already one space. An option would leave every
existing scene on the centre sort, which is the bug, and a scene whose models do not
overlap is unchanged either way.

<a id="decision"></a>
## Decision
**Models in a 3D scene share one depth test.** There is no new scene, camera or group
option. A model is still drawn into its own layer, depth cleared for that draw, opaque
then blended, and its filters still run on the layer. The shared test is when the scene
composites the layers.

1. **Occluders first.** A model occludes when it has no blended material, its blend is
   `normal`, and its opacity is at least 1. Those layers are drawn before the other items
   of their depth run, far to near among themselves. Each fragment's depth is the model's
   view depth at that texel, `NEAR_W / w` where the model covered it. The test keeps the
   nearer value and writes it, so two solids hide each other per pixel whichever centre
   sorts nearer.
2. **The rest keep the run's order.** A blended model, and any model whose blend is not
   `normal` (an additive plume), is drawn with the cards, far to near. It is tested the
   same way and does not write, so the hull shows through it and it does not hide a card
   drawn after it. A solid model in `normal` at opacity at least 1/2 and below 1 is drawn
   there too, so its colour blends over the occluders, and it does write, so a card drawn
   after it is still hidden. Below half opacity it tests and does not write, as the view
   depth already ignores it.
3. **No view depth means far.** A texel the view depth does not cover — nothing was drawn,
   a blended fragment under half opaque, or colour a filter spread past the model — is at
   the far plane the target is cleared to. An occluder hides it, and it shows only where
   no nearer surface was drawn. A plume that must be tested in front of a hull uses an
   opaque material, or a blended one at least half opaque; the item's opacity and an
   additive blend still fade it.
4. **Cards.** A card seen through the camera is tested against every occluder, not only
   against models that sorted farther. Where the card is nearer than the surface it shows;
   where the surface is nearer it is hidden. An item with no camera is still on the screen,
   untested. A scene with no model still has no depth target.
5. **Sampling.** The view depth is read at the colour layer's texel, not filtered, so a
   layer that holds only part of the canvas tests the depth that lines up with its colour.

A 2D scene is unchanged: a model there has no scene depth.

<a id="consequences"></a>
## Consequences
- **Easier:** a ship and a separate plume occupy one space. The plume is hidden by the hull
  where the hull is nearer, and an additive plume still adds where it is in front. Two
  solids that interpenetrate do too, in either centre order. Item opacity, blend, material
  drives and per-item filters are unchanged in how they are applied.
- **Harder:** a scene that relied on the centre sort to force one model over another now
  shows the nearer surface instead. A blended or additive model no longer hides cards
  drawn after it. A filter's halo has no depth, so a nearer model's surface hides it; the
  halo still shows where nothing is nearer. A blended fragment under half opaque is far, so
  it cannot sit in front of an occluder. The composite builds the blend pipelines once more,
  for the shared test, and each model-layer fragment reads its view depth.
- **Not decided here:** cards that write depth; one far-to-near list of every model's
  blended primitives, rather than each model's own list and then the layers; a test depth
  for a blended fragment under half opaque, separate from the view depth fog reads.
- **Follow-up:** none. The composite, the engine's choice of which models occlude, and the
  cube/rod tests land with this record.
