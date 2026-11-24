<!-- @doc
id = "ADR-0224"
title = "An arrival picture fades out and in with a warp that follows a channel or turns ahead"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0224: An arrival picture fades out and in with a warp that follows a channel or turns ahead

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0188](0188-a-backdrop-arrives-at-pictures-pinned-in-its-sky.md) decision 5 says an
arrival picture fades as the deep sky does at warp: out going in, back in as the warp
drops. Its decision 3 says when it is picked and where it goes, and two of its rules
undo that fade in common sets:

- **Dropping out.** A new picture is picked on the first step that the deep sky shows
  again "with the warp still or dropping", and dropping means the sky's warp is above its
  setting ([ADR-0186](0186-the-sky-eases-out-of-warp-toward-rest.md)). A `warp` that
  follows a channel, a sequence's lane or a motion modifier with a `warp_ramp_s` of 0
  is never above its setting: it is its setting, frame by frame. So no picture is picked
  until the warp reaches exactly 0, and it appears there at full strength, in one frame,
  after the stars and nebulae have already faded back in.
- **Going in, aimed ahead.** A warp aimed ahead
  ([ADR-0187](0187-a-warp-can-travel-straight-ahead.md)) lays the sky out anew the
  moment it starts, and decision 3 leaves the picture with the sky it cross-fades from,
  which is gone after 0.5 s (`AIM_FADE_S`). The picture vanishes in half a second at the
  first touch of warp, rather than fading as the warp builds.

Both look like the picture switching off and on, where the ADR promised a fade.

<a id="decision"></a>
## Decision
ADR-0188's decisions stand, with these changes to decision 3:

**1. A falling warp picks too.** A new picture is picked on the first step that the deep
sky shows again with the warp still, dropping, or lower than it was the step before. So
a warp that follows its setting down picks one as the deep sky starts to show, faint,
and it fades in with the deep sky (ADR-0188 decision 5) as the warp falls to rest. A
warp that rises never picks, as before.

**2. A warp aimed ahead keeps the picture where it is seen.** When a warp aimed ahead
lays the sky out anew, the picture it shows is kept, re-pinned in the new layout by the
turn between the two, so it is drawn where it was. The sky it cross-fades from shows it
there too, so the cross-fade doesn't change it. It then fades as the warp builds and is
dropped once the deep sky is gone, as one in a sky not aimed ahead is. An arrival of
several pictures ([ADR-0198](0198-an-arrival-can-be-a-planet-with-its-sun.md)) turns
as one, so each keeps its place from the first; its light shines from where it is
drawn.

It is agent tier ([R-GOV-005](../RULES.md#R-GOV-005)): it changes when a render-thread
pick happens and where an existing picture is drawn, with no setting, protocol field,
licence, security or privacy posture, compatibility promise or spending.

<a id="consequences"></a>
## Consequences
- **Easier:** a set that drives its warp from a channel or a sequence, or aims it ahead,
  gets the fades ADR-0188 describes, with no ramp or scripting of its own.
- **Harder:** a picture can be picked while the warp is still well above rest, on its
  way down, so a warp that stops falling and holds partway shows its picture faint
  until it moves again, and one that dips back below where the deep sky shows on its way
  up picks a picture there, as a drop out does.
- **Must now do:** `jstream-core`'s `SkyState` picks on a falling step and re-pins the
  arrival when a warp aimed ahead starts; tests cover both.
