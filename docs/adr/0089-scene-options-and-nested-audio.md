<!-- @doc
id = "ADR-0089"
title = "Scene options, and a scene's audio left out of nesting"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0089: Scene options, and a scene's audio left out of nesting

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
   whatever each scene source's own `audio.enabled`. It changes nothing while the scene
   is a root itself (on program, or one side of a transition), and other paths to a
   source still count. ADR-0028 §"What shown means" 2 and 3 read with this exception.
3. Collection files store it as `include_audio` on the scene, omitted for `true`. The
   schema version stays the same: a file that never turns it off is unchanged.

<a id="consequences"></a>
## Consequences
- The UI's scene Properties dialog shows the switch, and Duplicate scene copies it.
- The engine's activity gains (`crate::activity`) check the nested scene's option, and
  ARCH › Data model's "Shown and on program" names it. The mock engine stores it too.
- A plugin needs the edit scope to set it, as for `scene.set_meta`.
- An older build refuses a collection file in which a scene has `include_audio` off,
  because its file format refuses unknown fields. This is the same for any field a newer
  build writes.
