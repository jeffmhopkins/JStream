<!-- @doc
id = "ADR-0213"
title = "A scene clip's audio is drawn linked under it on the show timeline, derived, not stored"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0213: A scene clip's audio is drawn linked under it on the show timeline, derived, not stored

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
A scene on program is heard through its sources: a source whose audio follows "when
shown" is heard while it is on the Program mix
([ADR-0210](0210-shows-a-run-order-with-bookmarks.md)'s audio tracks only bring other
sources' faders up). The Show panel's Edit view drew a scene clip as picture only, so
nothing said which clips make sound. The maintainer asked for a scene with audio to show
a linked audio clip in an audio row, as an editor's linked audio does, and for a scene
with no audio, or only muted audio, to have none.

<a id="decision"></a>
## Decision
- **Derived, not stored.** The linked audio is drawn from the show and the collection
  as they are: the show format gains nothing, and the runner does nothing new. What is
  heard is what the scene's sources already do when the scene is shown.
- **What counts as audible.** A clip's scene is audible when one of its visible items
  has a source whose audio is enabled, follows "when shown", is not muted and has its
  fader above the bottom (−100 dB), or is a nested scene that is audible by the same
  rule. Sources that follow "always" are heard whatever the show does, so they never
  make a linked clip. Muting a source, or turning its fader to the bottom, takes the
  linked clip away at once.
- **Where.** One read-only row per video track with an audible clip ("V1 audio", "V2
  audio"), first in the audio group, above the A tracks. Each linked clip spans its
  scene clip exactly.
- **Linked.** A press on a linked clip acts on its scene clip: it selects it, and a drag
  moves or trims it, so the two never part.

<a id="consequences"></a>
## Consequences
- No protocol or runner change, and nothing to keep in step: an edit to the scene or the
  mixer shows on the timeline as it is made.
- A clip's own audio can't be faded, ducked or muted per clip. That would need the
  format and the runner (a per-clip audio setting) and is left for a later decision.
- A source that is heard only through a filter or a send the rule doesn't look at is
  not drawn.
