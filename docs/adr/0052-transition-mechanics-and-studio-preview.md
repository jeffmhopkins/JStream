<!-- @doc
id = "ADR-0052"
title = "Transition mechanics as built, and the studio's preview scene"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0052: Transition mechanics as built, and the studio's preview scene

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0029](0029-transitions.md) decides what transitions do. Building it (roadmap T1)
found three places where its Decision names a mechanism that the engine is better
without, or that it can't honour yet:

- **§7, retargeting:** "the render thread owns the live interpolated state … takes the
  current sampled state of every visible item … as the new starting point". A render
  thread that keeps state has to be told about a retarget, and it has to answer with that
  state, so the control thread would wait on it or race it. The visible behaviour the ADR
  wants, starting from what is on screen with no jump, doesn't need it.
- **§4, flattening:** a nested scene with no filters, normal blend and full opacity "is
  flattened for pairing". Items have no filters, blend or opacity yet, so under that rule
  every nested scene flattens. But a nested scene's item can be scaled, cropped or rotated.
  Its children's geometries are in the nested scene's canvas, not in the parent's, so
  pairing them with the parent's items needs the item's placement composed into each
  child's, which the 3D-capable geometry can't yet express for a rotated, cropped box.
- **§6, stagger:** "an item's progress starts after its delay plus `stagger_ms` × its
  index in stacking order". It doesn't say whose index. The prototype
  (`design/ui-prototype/`) staggers only the scene being switched to. Leaving items have
  no index in that scene.

[ADR-0030](0030-studio-state-and-scene-switching.md) left the preview scene, studio
mode's next scene, under "Not decided here" for this item.

The decisions below are internal mechanics and runtime state. None changes product
scope, a compatibility promise or a security posture, so this ADR is agent tier
([ADR-0043](0043-narrow-maintainer-tier.md)). It amends ADR-0029 §4, §6 and §7 and
settles ADR-0030's open preview point. The rest of both stands.

<a id="decision"></a>
## Decision

**1. Retargeting samples a stateless plan (amends ADR-0029 §7).**
- A running transition is a plan: one track per item (a pair, an appearance or a
  disappearance) with its timing, built on the control thread and published in the
  render snapshot with the transition's start on the media clock.
- The render thread keeps no transition state. Each frame, it samples every track at the
  frame's media time with one pure function.
- A switch mid-transition samples the running plan at the switch time, on the control
  thread, with that same function. What it gets is what the render thread draws at that
  time, so the new transition starts from what is on screen, including items still
  fading out, with no jump and no wait.

**2. A nested scene flattens only when it fills the canvas (amends ADR-0029 §4, for
now).** A nested scene's items pair individually only when its item places it over the
whole canvas exactly: identity placement, no crop, no rotation and neutral 3D. Otherwise
it moves as one image, paired by its scene source like any item. ADR-0029's conditions
(no filters, normal blend, full opacity) still apply on top once those properties exist.
Flattening a placed nested scene comes when placements compose.

**3. Stagger is for incoming items (clarifies ADR-0029 §6).** An item of the scene
being switched to starts after its own delay plus `stagger_ms` × its index in that
scene's stacking order. A leaving item starts after its own delay only. The `None`
effect still waits for the item's start, then shows or hides the item at once.

**4. Preview (settles ADR-0030's open point).**
- The preview scene is runtime state beside the program scene, under the same rules:
  the collection's first scene goes on preview when it is created, a deleted preview
  scene gives way to the first scene left, and a reorder never moves it.
- `studio.set_preview` sets it. It is an action, so it changes no `rev` and is never
  undone. `studio.get` and `studio.changed` report it beside program.
- A switch never moves preview. There is no studio-mode flag in the engine: whether a
  client shows a preview is the client's choice, and a `Preview` video mix shows the
  preview scene.

<a id="consequences"></a>
## Consequences
- The render thread stays as simple as [R-RT-002](../RULES.md#R-RT-002) wants: it
  samples what it is given and never answers the control thread. A retarget costs the
  control thread one sample of each running track.
- A scaled, cropped or rotated group moves as one image during a Move, unlike in the
  prototype. Its look stays intact, but its children don't glide to their counterparts.
- Leaving items don't cascade. A preset that wants them to has to wait for a setting
  that says so.
- [ARCH › GPU pipeline](../ARCHITECTURE.md#gpu-pipeline) and
  [`jstream-core`'s design](../../crates/jstream-core/README.md#design) describe these
  mechanics and cite this ADR.
