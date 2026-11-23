<!-- @doc
id = "ADR-0179"
title = "Cards that only add light cast no shadow"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0179: Cards that only add light cast no shadow

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0151](0151-the-rest-of-richer-3d-scenes.md) decision 3 makes every card a 3D scene
sees through its camera a shadow caster, where its alpha is at least one half. A card
with the `additive` or `screen` blend (ADR-0082) is light, not matter: a glow, a beam, a
lens flare, a hologram. Drawn on screen it only brightens what is under it, yet in a
shadow map it blocks the light like a solid card, so a beam of light throws a dark
shadow on the set.

Such cards also tend to change every frame (a hologram's scanlines, a flickering glow).
Every caster that changes makes the scene's shadow maps draw again, so one animated glow
in a shadowed scene costs every map, every frame, though it should cast nothing.

<a id="decision"></a>
## Decision
A card whose blend is `additive` or `screen` casts no shadow: the shadow maps leave it
out, and it does not count toward a directional light's caster bounds. Every other card
casts as ADR-0151 decision 3 says. A model's layer casts by its own `casts_shadows`
setting, whatever its blend.

The view depth the scene's chain reads (ADR-0143 decision 4) is unchanged: such a card
still has depth there.

This amends ADR-0151 decision 3 for these cards only.

<a id="consequences"></a>
## Consequences
- Glows, beams and holograms in a shadowed scene throw no false shadow, and an animated
  one no longer redraws the shadow maps.
- A card that should both add light and cast a shadow can't do both; two cards, one
  `normal` at low visibility, would be needed. No such use is known.
- There is still no per-card switch for casting; one can be added if a `normal` card
  needs to opt out.
