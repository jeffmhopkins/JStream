<!-- @doc
id = "ADR-0068"
title = "Item effects are in v1"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0068: Item effects are in v1

<!-- @generated begin adr-status -->
> **Status:** proposed
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0065](0065-scene-chains-and-item-effects.md) designs item effects: a video filter
chain per placement, stored in the `effects[]` slot that
[ADR-0023](0023-placements-references-and-clones.md) reserves on every scene item. It
leaves one question open, because it is product scope and so the maintainer's
([ADR-0043](0043-narrow-maintainer-tier.md)): whether item effects are built in v1.
Until that is decided, ADR-0065 has the engine reject the `item` owner and keeps
`effects[]` empty.

Three accepted documents keep per-placement effects out of v1:
- ADR-0023, a maintainer-tier ADR, whose consequences shrink M1b so that a different look
  per placement needs a clone "until item effects are built", and which leaves "when,
  and with what render cost" to a future ADR;
- [ADR-0026](0026-source-filter-chains.md)'s context, which repeats ADR-0023's choice;
- ROADMAP › M1b's "Filters and effects" bullet: "per-placement effects are not in v1".

Accepted ADRs can't be edited ([R-DOC-009](../RULES.md#R-DOC-009)), so the answer is
recorded here.

The maintainer answered the question on issue #529: a decision comment relayed in
[ADR-0043](0043-narrow-maintainer-tier.md)'s proxy form, with his instruction
"Adr 0065 approve", which says that item effects are **in v1**
(issue #529, comment 6241811815).

**Tier.** Maintainer: which features are in v1 is product scope (ADR-0043). It changes
an earlier maintainer choice (ADR-0023's) and no design.

<a id="decision"></a>
## Decision
**Item effects are built in v1**, as [ADR-0065](0065-scene-chains-and-item-effects.md)
designs them. This answers ADR-0065's open question, and replaces ADR-0023's choice and
ADR-0026's restatement of it, for item effects only. Everything else in those ADRs
stands: source filters still apply everywhere the source appears, and a placement still
owns its opacity and blend.

- **The plan for v1.** ADR-0065 has the engine reject the `item` owner with
  `invalid_argument`, with `effects[]` empty, until the maintainer schedules item effects.
  That rejection is no longer the end state. It stays only until the item-effect PRs land,
  and then the engine takes the `item` owner and keeps a non-empty `effects[]`.
- **Order of work.** Unchanged from ADR-0065: the protocol changes go through the protocol
  queue after PR #303's `master` and `output` owners, then come the video chain runtime
  (roadmap V0) and ADR-0065's follow-up PRs (protocol, graph, core and render, then the
  UI). This ADR brings no part of it forward and changes no code.
- **Cost.** ADR-0065 §7 stands. Each item with an enabled effect costs a layer and its
  passes, `stats.tick` reports the cost, and the budget is measured when it is built.
  Whether to cap items with effects follows from that measurement, and is not a reason
  to leave them out of v1.

<a id="consequences"></a>
## Consequences
- **Easier:** a vignette, rounded corners or a drop shadow on one placement no longer
  needs a clone or a group in v1. The M1b deliverable that ADR-0023 shrank comes back:
  item effects apply to one placement.
- **Harder:** v1 takes on ADR-0065's item-layer render nodes, the descriptor's outset
  field, a collection-file `schema_version` bump and migration for non-empty `effects[]`,
  and an "Effects" panel in the UI.
- **Docs.** ROADMAP › M1b now says that per-placement effects are in v1 (with ADR-0065's
  design) and that the model reserves their slot. ARCH › Data model says that
  `effects[]` stays empty until item effects are built, not that it stays empty in v1.
  ADR-0065's "Docs to change when built" list still applies when the item-effect PRs
  land.
- **Not decided here:** which video filter kinds exist, keyframe overrides of effects,
  and every other question ADR-0065 leaves open.
