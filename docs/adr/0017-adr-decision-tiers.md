<!-- @doc
id = "ADR-0017"
title = "Decision tiers: which ADRs need the maintainer"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0017: Decision tiers: which ADRs need the maintainer

<a id="context"></a>
## Context
Builder agents write ADRs when work forces a significant decision. Under
[ADR-0015](0015-review-and-governance.md) an ADR became binding once the reviewer agent
passed it and the PR merged, with no human involved. Every ADR so far came out of
conversations where the maintainer made the call. The maintainer wants to keep deciding
the important questions without approving routine implementation choices.

<a id="decision"></a>
## Decision
Every ADR declares a `tier` in its `@doc` block.

- **`tier = "maintainer"` (Tier 1)**: decisions about
  - **product and platforms**: features in or out of scope, supported platforms and OS
    versions, minimum requirements, adding or dropping a platform;
  - **process and governance**: the review process, the docs system, the rules and how
    they are enforced, CI policy, and **superseding any existing ADR**.

  A Tier-1 ADR is opened as `status = "proposed"` and the PR gets the `needs-human`
  label. The maintainer decides by commenting `/accept ADR-NNNN` or `/reject ADR-NNNN` on
  the PR. The builder then sets `status` to `accepted` or `rejected` to match. The
  reviewer may post `pass` on a PR containing a Tier-1 ADR only when the head commit's
  status matches a maintainer decision comment on that PR.
- **`tier = "agent"` (Tier 2)**: every other decision, for example implementation
  choices within an accepted ADR, the choice of crates or libraries, internal structure,
  or a spike result that confirms the plan. It lands as `accepted` on merge, after the
  usual review.
- **Classification.** The builder picks the tier, and the reviewer checks it against the
  list above. Too low a tier is a blocking finding. Borderline cases are Tier 1.
- **Existing ADRs.** ADR-0001 to ADR-0016 were decided in conversation with the
  maintainer, so they count as Tier 1 and accepted. Their metadata is not edited
  ([R-DOC-009](../RULES.md#R-DOC-009)).
- **Who may post decisions.** Agents never post `/accept` or `/reject`. A decision comment
  that GitHub marks as made through an app ("with Claude") is not a maintainer decision.

<a id="consequences"></a>
## Consequences
- The maintainer sees every product, platform and process decision before it binds,
  and nothing else.
- Work that depends on a proposed Tier-1 ADR waits for the maintainer.
- With one GitHub account, the guarantee that a decision comment came from the
  maintainer rests on the "never post decisions" rule plus GitHub's app attribution. The
  future `governance` check can enforce the same rule mechanically.
