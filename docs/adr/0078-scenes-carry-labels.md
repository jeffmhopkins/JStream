<!-- @doc
id = "ADR-0078"
title = "Scenes carry labels"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0078: Scenes carry labels

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0024](0024-scene-structure.md) §4 makes labels collection state and gives them to
**sources** only: `Source` carries `labels` and `favourite`, while `Scene` carries
`icon`, `color`, `favourite` and `show_in_multiview`. It follows decision register D7
([design/ui-prototype](../../design/ui-prototype/decision-register.md)), which read the
prototype's label assignments as keyed by source.

The prototype assigns labels to scenes too. Its `DEFAULT_META.assign` puts "Intro /
outro" on Starting Soon and Ending, "Gameplay" on Desktop + Cam and "Overlay" on Intro
overlay and Lower third pack. Its Scenes panel shows each scene's label chips and a
hover tag button, its filter has "Labels · any of" and "Edit labels…", and the
Inspector's header for a scene has the chips and the tag button, exactly as for a source.
The desktop UI is to match the prototype one to one (U6), and the maintainer's alpha
feedback found the label system wrong: with scenes carrying none, the Scenes panel has
no chips, no tag button and no labels filter.

This changes ADR-0024's data model, an accepted decision, so it needs its own ADR
([R-ARCH-004](../RULES.md#R-ARCH-004)). It is agent tier
([R-GOV-005](../RULES.md#R-GOV-005)): it adds a field to match the prototype the
maintainer already chose. It changes no product scope, licence, security or privacy
posture, compatibility promise or spending: the protocol gains an optional field
(a minor version), and an older client keeps working.

<a id="decision"></a>
## Decision
1. **A scene carries labels.** `SceneMeta` gains `labels`: a set of `LabelId`, ordered by
   ID, omitted for none. `scene.set_meta` sets it with the scene's other metadata, and
   checks it as `source.set_meta` checks a source's: a label that doesn't exist is
   `not_found`, one listed twice `invalid_argument`. A keyframe scene has its own, as it
   has its own icon and favourite (ADR-0024 §4).
2. **Deleting a label** is rejected while a scene carries it, as while a source does. The
   blockers list the sources carrying it, by ID, then the scenes, in display order, each
   with field `labels`; `delete.plan` takes it off each of them, sources first, then
   deletes it.
3. **The collection file** saves a scene's labels as `scenes[].labels`, omitted for none.
   An older file has none, so it loads unchanged and `schema_version` stays 2.
4. The protocol version becomes 0.45.

Everything else in ADR-0024 §4 holds: a source's labels belong to the source, so every
item placing it shows them, and `source.duplicate` copies them.

<a id="consequences"></a>
## Consequences
- **Easier:** the Scenes panel and a scene's Inspector header show and set labels as the
  prototype does, and agents can find scenes by label ("switch to the scene labelled
  Gameplay").
- **Harder:** a label's blockers and delete plan cover scenes as well as sources, in the
  graph and in the mock.
- **Docs:** [ARCH › Data model](../ARCHITECTURE.md#data-model) lists `labels` among a
  scene's metadata, and the jstream-protocol README names 0.45. ADR-0024 is unchanged as
  a record; this ADR amends its §4.
