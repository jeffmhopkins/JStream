<!-- @doc
id = "ADR-0154"
title = "A carried review pass compares the reviewed head against the base it was reviewed on"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0154: A carried review pass compares the reviewed head against the base it was reviewed on

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

Amends [ADR-0109](0109-review-pass-carries-across-base-merges.md), which measures "the same
change" by diffing both heads against the PR's current base, and the verdict marker of
[ADR-0019](0019-review-gate-v2.md). The review process is agent tier
([ADR-0043](0043-narrow-maintainer-tier.md)).

<a id="context"></a>
## Context
A stacked PR is reviewed against the PR below it. Once that lower PR squash-merges into
the wave, GitHub retargets the stacked PR to the wave, and its thread merges the wave in.
Nothing in the stacked PR's own change moved, but `review-gate` diffs the reviewed head
against the wave. That head doesn't contain the squash commit, so its diff from the merge
base also holds every line of the lower PR, and it never matches the new head's diff. The
pass doesn't carry, and every PR in a stack waits for a fresh review round after its base
lands (#1502; seen on #1485, where the reviewed head showed 13 files and +1000 against the
wave but 10 files and +633 against the base it was reviewed on).

<a id="decision"></a>
## Decision
- **A review may name its base.** Under the verdict marker, the same comment may hold
  `<!-- jstream-review-base v1 sha=<the marker's head SHA> base=<40-char SHA> -->`: the
  head of the PR's base branch that the reviewed diff was taken against
  (`gh pr view --json baseRefOid`). It is a line of its own, not a field of the verdict
  marker, so a gate that doesn't know it still reads the verdict. It counts only in an
  owner's comment, beside a verdict marker for the same `sha`. A review without it is
  read as before.
- **The reviewed head is diffed against that base.** When the gate looks for a carry, it
  takes the reviewed head's diff from `<its review's base>...<reviewed head>` and the
  current head's from `<current base>...<head>`. ADR-0109's other conditions are
  unchanged: the reviewed head is the latest marker's, it is an ancestor of the head,
  every PR commit since merges the base branch in or only restamps the ledger, and the
  two diffs are the same change. A review without a base line uses the current base for both,
  as ADR-0109 did, and every doubt still fails closed.
- `reviewedBase` sits in the workflow's self-contained decide block, and `gate-test`
  tests it.

<a id="consequences"></a>
## Consequences
- A stacked PR whose base landed only needs `docs merge-main --from <wave>` and a push. Its
  pass carries, as a wave PR's already does.
- The carry now trusts the base SHA in an owner-posted review, as it already trusts the
  marker's verdict. A wrong base can only make the two diffs differ (the gate stays
  pending), or match when the PR's own change truly is the same against both.
- A pass from before this ADR, or from a reviewer that leaves the base line out, carries
  as it did before, and a stacked PR's still doesn't.
- Until `review-gate.yml` with this change is on `main`, the gate there ignores the base
  line and decides as ADR-0109 does, so a reviewer can write it from the start.
