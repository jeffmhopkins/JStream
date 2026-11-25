<!-- @doc
id = "ADR-0239"
title = "An arrival picture goes as the stars start to move"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0239: An arrival picture goes as the stars start to move

<!-- @generated begin adr-status -->
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0188](0188-a-backdrop-arrives-at-pictures-pinned-in-its-sky.md) decision 5 fades an
arrival picture as the deep sky does at warp, and
[ADR-0224](0224-an-arrival-picture-fades-with-the-warp-it-follows.md) keeps it in view as
a warp aimed ahead starts, so it fades rather than vanishes. Going into warp the deep
sky is gone only at a warp of 0.25. Up to there the stars already stream past, yet the
picture, a nebula or a planet pinned a few degrees off straight ahead, still hangs where
it was, barely dimmed. It reads as a picture stuck on the glass, not a place left behind.

<a id="decision"></a>
## Decision
**1. Going into warp, an arrival picture is gone by a warp of 0.05**, eased out from the
first touch of warp, as the stars start to move. The deep sky keeps its own fade.

**2. Dropping out, it comes back as the deep sky does**, ADR-0188 decision 5 unchanged:
the two fades blend by how much the warp is dropping, as the deep sky's do
([ADR-0186](0186-the-sky-eases-out-of-warp-toward-rest.md)). So it is never shown more
than the deep sky, and a warp followed down with no ramp (ADR-0224 decision 1) brings it
back only below 0.05.

**3. Its light fades with it**, and the sky a warp aimed ahead cross-fades from (ADR-0187)
shows it at the same fade, so the cross-fade doesn't keep it.

When and where a picture is picked and dropped is unchanged: still tied to the deep sky.

It is agent tier ([R-GOV-005](../RULES.md#R-GOV-005)): it changes how fast an existing
picture fades, with no setting, protocol field, licence, security or privacy posture,
compatibility promise or spending.

<a id="consequences"></a>
## Consequences
- **Easier:** a jump to warp leaves the picture behind at once, however slowly the warp
  builds.
- **Harder:** a warp that holds at a crawl, below 0.05, shows its picture faint, and a
  warp followed down with no ramp fades it in over the last 0.05 only, quicker than the
  deep sky.
- **Must now do:** `jstream-core` draws the picture at its own fade (`picture_shown`),
  passed to the sky as each picture's opacity over the deep sky's fade; a test covers the
  curves.
