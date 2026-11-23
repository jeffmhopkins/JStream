<!-- @doc
id = "ADR-0199"
title = "A carried review pass also compares the PR's file modes"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0199: A carried review pass also compares the PR's file modes

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

Amends [ADR-0109](0109-review-pass-carries-across-base-merges.md) and
[ADR-0162](0162-a-carried-pass-compares-added-binaries-by-blob.md), which measure "the same
change" on the compare API's file list. It tightens the gate and adds no new way for a pass
to carry, so it is agent tier, as ADR-0162 is ([ADR-0043](0043-narrow-maintainer-tier.md)).

<a id="context"></a>
## Context
The compare API's `files` entries carry no file mode, and a `patch` leaves out the
`new file mode` header. So a merge of the base branch whose resolution only makes a file of
the PR executable keeps the same blob and the same patch, and the reviewed pass still
carries (#1584). The risk is low, because every PR commit after the review must already be a
base merge or a ledger-only restamp, but the reviewer never saw that mode.

<a id="decision"></a>
## Decision
- **The PR must change its files' modes the same way at both heads.** Once the two diffs
  are the same change, the gate reads the git trees of the reviewed head, the current head
  and the base each was diffed against. For every file of the PR except the stamp ledger,
  it compares the mode at the base (at the old path, for a rename) with the mode at the
  head. That mode change, or its absence, must match at both heads, or the pass doesn't
  carry.
- **A mode change from the base branch itself doesn't count.** If the base branch makes a
  file executable that the PR also edits, both the base and the head carry the new mode,
  so the PR's own change is unchanged.
- **A missing or truncated tree fails closed.** The gate stays pending, as it does for any
  other doubt under ADR-0109.
- **Wording note on ADR-0162.** The status in each file's key is what tells "added on one
  side" from "modified on the other": an added file is keyed by its blob only while its
  status is `added` at both heads, so a file the base branch added meanwhile (and the PR
  now modifies) makes the two diffs differ.
- `gate-test` tests the new cases in `sameModes`.

<a id="consequences"></a>
## Consequences
- A merge that changes a mode on a file of the PR needs a new review round, as a merge that
  changes the PR's lines already does.
- The gate makes up to four more read-only API calls (one recursive tree per commit), and
  only when a pass is about to carry. ADR-0162's "no extra API calls" no longer holds for
  the carry as a whole.
- A repository whose tree is too large for one recursive tree response (GitHub truncates
  it) would never carry a pass. The repository is far below that limit today.
