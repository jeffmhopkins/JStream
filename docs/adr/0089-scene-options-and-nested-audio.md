<!-- @doc
id = "ADR-0089"
title = "Scene options: a scene's audio left out of nesting, and its background"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0089: Scene options: a scene's audio left out of nesting, and its background

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
The prototype's scene Properties dialog (spec-workspace 6.2.5, UI-FEATURES DL-03) has an
"Include its audio when nested" switch, on by default, with the note "When off, its
sources don't reach the Mixer through this scene." The desktop UI is to match the
prototype one to one (U6), and the engine has no such setting.

[ADR-0028](0028-audio-model-v2.md) defines what is shown, and so heard. A source is
**reachable** from a scene through its visible items, following nested scene sources
whose `audio.enabled` is on. Each nested scene source is one placement of a scene, so its
`audio.enabled` is a per-placement switch. The prototype's switch is per scene: it
covers every place the scene is nested, including a nesting made later. It can't be
written as `source.set_audio` on the scene's current scene sources, because a scene
source added afterwards would bring the audio back.

The same dialog has a Background color field. [ADR-0024](0024-scene-structure.md) §4
decided what a scene background is (a colour with alpha, transparent by default, drawn
wherever the scene renders, nested or not, a keyframe scene using its parent's) and named
an edit for it, `scene.set_background`, which no build has had. It is per-scene behaviour
like the switch, so it belongs with it.

Changing ADR-0028's reachability rule changes an accepted decision, so it needs its own
ADR ([R-ARCH-004](../RULES.md#R-ARCH-004)). It is agent tier
([R-GOV-005](../RULES.md#R-GOV-005)). It adds a setting the maintainer already chose in
the prototype, and it changes no product scope, licence, security or privacy posture,
compatibility promise or spending. The protocol gains an optional field and an edit, a
minor version, and an older client keeps working.

<a id="decision"></a>
## Decision
1. **Scene options.** A scene carries `SceneOptions`, which `state.get` reports
   flattened beside the scene's other fields, each omitted while it has its default.
   `scene.set_options { scene_id, ...options }` replaces them, with every field left out
   taking its default, and its inverse is `scene.set_options` with the previous options.
   They are behaviour, not organisation, so they are kept apart from `SceneMeta`
   ([ADR-0024](0024-scene-structure.md) §4), which "changes no rendering, switching or
   nesting". A keyframe scene has options of its own, as it has its own transitions and
   metadata. Later per-scene behaviour, such as a background colour, is added here as
   another field.
2. **`include_audio`**, default `true`. Off, ADR-0028's reachability does not follow a
   nested scene source into that scene, and such a source set to `follow = always` is
   not an extra root. So no source reaches the Mixer through any nesting of the scene,
   whatever each scene source's own `audio.enabled`. A group is only reached through
   nesting, so on a group it leaves out its children's audio wherever the group shows.
   It changes nothing while the scene
   is a root itself (on program, or one side of a transition), and other paths to a
   source still count. ADR-0028 §"What shown means" 2 and 3 read with this exception.
3. Collection files store it as `include_audio` on the scene, omitted for `true`. The
   schema version stays the same: a file that never turns it off is unchanged.
4. **`background`**, default none. It is ADR-0024 §4's background, set through
   `scene.set_options` instead of the `scene.set_background` it named, and otherwise as
   that section decided: sRGB `#rrggbb` or `#rrggbbaa` with straight alpha, `invalid`
   otherwise; none is transparent. The compositor clears each pass of the scene to it, so
   it fills the scene's canvas under its items whether the scene is the root of a video
   mix or nested, and a scene with a video chain has it in the chain's input. Where a mix
   shows a transparent pixel the output is black, so a background with alpha shows over
   black. A keyframe scene shows its parent's, and setting one on it is `invalid`. A
   group draws none, whatever its options hold: it is part of the scene that holds it,
   and a background would cover what lies under it there. It keeps the value, so a group
   that becomes a scene again (`scene.set_role`) draws it. While a transition runs, the background
   under its tracks fades from the scene it leaves to the one it shows over the
   transition's length, mixed in linear light; one that starts from a cut-short
   transition starts from the shown scene's. Collection files store it as `background`
   on the scene, omitted for none.

<a id="consequences"></a>
## Consequences
- The UI's scene Properties dialog shows the switch and the Background color, and
  Duplicate scene copies both. A keyframe scene's dialog shows its parent's background
  and doesn't edit it.
- The compositor's scene pass gains a clear colour per scene, and `jstream-render`'s
  `Color` the conversions it needs (over black, and a mix of two).
- The engine's activity gains (`crate::activity`) check the nested scene's option, and
  ARCH › Data model's "Shown and on program" names it. The mock engine stores it too.
- A plugin needs the edit scope to set it, as for `scene.set_meta`.
- An older build refuses a collection file in which a scene has `include_audio` off, or a
  background, because its file format refuses unknown fields. This is the same for any
  field a newer build writes.
