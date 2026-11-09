<!-- @doc
id = "ADR-0149"
title = "Where a warm scene is pinned, and which scenes count as bound"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0149: Where a warm scene is pinned, and which scenes count as bound

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0117](0117-real-time-video-thread-model.md) §7 orders the warm scenes: the
program, a running transition's destination, the studio's Preview scene, then "scenes
pinned by the user or bound to a hotkey or trigger", then the most recently shown
scenes. A budget evicts from the bottom, never the first three. It says "the budget and
the pin live in Application Settings (#1207)".

The budget is built that way (#1470): `studio.set_warm` sets a number on the engine's
machine, Settings › General shows it, and it holds whatever project is open.

A pin is different. It names a scene, and scene IDs belong to a project. A pin list in
Application Settings would name scenes of whatever project was open when it was made,
so it would be wrong, or name nothing, as soon as another project opens. It would also
miss what people expect of a scene's own setting: that it travels with the project, is
undone, and shows where the scene is edited, beside "Show in multiview".

§7 also doesn't say what "bound to a hotkey or trigger" means for a binding. Most
bindings name no scene, and some name one without showing it (`studio.set_preview`
already counts as the Preview scene; `scene.zoom_camera` changes the program's camera).

<a id="decision"></a>
## Decision
This replaces only §7's "the pin lives in Application Settings". The rest of §7 holds,
including the budget in Application Settings.

1. **A pin is a scene option.** `SceneOptions` gains `keep_warm` (protocol 0.116),
   set with `scene.set_options` like `include_audio` and `background`, and saved in the
   collection file (schema 15). It is an edit: it moves `rev` and undo takes it back. A
   keyframe scene is shown through its parent, so only the parent can be kept warm;
   `scene.set_options` refuses `keep_warm` on a keyframe scene. The UI sets it from the
   scene's Properties and its row menu ("Keep warm").
2. **A bound scene** is one that a hotkey's or a trigger binding's `scene.switch`
   names, since pressing it puts that scene on program. No other bound action makes a
   scene warm.
3. **Order within the budget.** After the Preview scene: the kept-warm scenes in
   display order, then the bound scenes in the bindings' order, then the most recently
   shown. Each scene counts once, and the program never counts. The budget counts every
   warm scene besides the program, the Preview scene included, but Preview stays warm
   even when the budget is spent, as §7 requires.

<a id="consequences"></a>
## Consequences
- Opening another project brings its own pins; the budget stays the machine's.
- A project made with pins can't be opened by a build older than schema 15, as with
  every schema change.
- A budget smaller than the number of pinned and bound scenes leaves some of them cold,
  in the order above. The Properties note says a kept-warm scene counts toward the
  budget, so a person who pins many scenes knows to raise it.
- §7's estimate of GPU bytes is still to come (#1468).
