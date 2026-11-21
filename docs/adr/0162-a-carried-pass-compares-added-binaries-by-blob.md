<!-- @doc
id = "ADR-0162"
title = "A carried review pass compares an added file without a patch by its blob"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0162: A carried review pass compares an added file without a patch by its blob

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

Amends [ADR-0109](0109-review-pass-carries-across-base-merges.md), which fails closed on
any file without a patch when it measures "the same change". The review process is agent
tier ([ADR-0043](0043-narrow-maintainer-tier.md)).

<a id="context"></a>
## Context
GitHub's compare API gives no `patch` for a binary file, or for a text file too large to
diff. Under ADR-0109, one such file in a PR means its pass never carries across a merge of
the base branch. Every PR that adds test media (all the Linux decoder PRs do) needs a
fresh review round after each `docs merge-main`, although nothing in its own change moved
(#1544; seen on #1537, whose diffs before and after three base merges were identical apart
from the stamp ledger).

<a id="decision"></a>
## Decision
- **An added file without a patch is compared by its blob SHA.** In each diff, the gate
  keys such a file by its path, its `added` status and the blob `sha` the compare API
  reports. An added file has no base content, so the same blob at the same path is the
  same change, and any other blob, path or status still makes the two diffs differ.
- **Every other file without a patch still fails closed.** A modified, removed or renamed
  file's change depends on the base content too, which the compare API doesn't give, so
  the gate stays pending, as ADR-0109 has it. So does an added file without a valid blob
  SHA.
- ADR-0109's other conditions are unchanged, and `gate-test` tests the new cases in
  `sameChange`.

<a id="consequences"></a>
## Consequences
- A passed PR that adds binary test files only needs `docs merge-main` and a push. Its
  pass carries, as a text-only PR's already does.
- A PR that changes an existing binary or very large file still needs a new review round
  after each base merge.
- The gate still makes no extra API calls: the blob SHA is already in the compare
  response.
