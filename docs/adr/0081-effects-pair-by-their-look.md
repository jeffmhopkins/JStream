<!-- @doc
id = "ADR-0081"
title = "Items pair in a Move when their enabled effects look the same"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0081: Items pair in a Move when their enabled effects look the same

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0065](0065-scene-chains-and-item-effects.md) §6 lets a paired item glide in a Move
transition with its effects "only when both sides' effects are equal (the same filters,
in order, with the same settings and switches)". Read literally, that compares every
filter: its ID, its switch and its settings, disabled filters included.

The engine compares less. It pairs two items when their chains look the same: the
enabled filters' kinds and settings, in order (`Chain::look` in `jstream-core`). A
disabled effect is not in the chain, so it doesn't draw, and a filter ID names a filter
without changing what it draws. Two items whose only difference is an extra disabled
effect, or effects with other IDs, draw the same picture, and the engine pairs them.
That is the behaviour wanted: the glide keeps the picture the viewer sees, and
splitting such a pair into a disappear and an appear would change a picture that
hasn't changed. The review of #824 (N2) found the wording and the code apart, and the
code was kept (#861).

<a id="decision"></a>
## Decision
This amends ADR-0065 §6's sentence on which items glide with their effects. The rest of
ADR-0065 stands.

A paired item glides with its effects only when both sides' effects look the same: the
same kinds with the same settings, in order, counting only the enabled effects of kinds
the engine draws. Filter IDs and disabled effects don't count. Otherwise the two items
don't pair, and each takes its unmatched appear or disappear effect, as ADR-0065 §6
says. Effect settings are still never interpolated.

<a id="consequences"></a>
## Consequences
- Disabling an effect on one of two otherwise matching items, or giving their effects
  different IDs, keeps them paired, which matches what each side draws.
- A kind the engine doesn't draw is left out of the comparison, as it is left out of the
  chain, so it can't split a pair either.
- The engine needs no change: `Chain::look` already compares this, and its doc comment
  and the core README's Video chains paragraph already say so.
