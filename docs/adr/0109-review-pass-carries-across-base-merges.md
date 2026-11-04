<!-- @doc
id = "ADR-0109"
title = "A review pass carries across merges of the base branch"
kind = "adr"
status = "accepted"
tier = "maintainer"
-->

# ADR-0109: A review pass carries across merges of the base branch

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

Amends [ADR-0019](0019-review-gate-v2.md): its gate considers only markers for the head
SHA, and any push resets it.

<a id="context"></a>
## Context
Many builder threads land PRs into `main` at once, and nearly every PR edits the stamp
ledger (`docs/verified.toml`), so a PR that passed review often conflicts by the time its
platform jobs finish. Its thread merges `main`, the push resets `review-gate` to pending,
and the PR waits for a whole new review round. Meanwhile another PR lands, and the loop
starts again. Each lap costs a full CI run and a reviewer's time, and the maintainer has
asked for the loop to stop.

The ruleset on `main` doesn't require branches to be up to date
(`strict_required_status_checks_policy` is off). A PR that passed review and doesn't
conflict lands without ever merging `main`, so a passed PR that merged `main` cleanly is no
riskier than one that landed without the merge. Re-reviewing it buys little.

<a id="decision"></a>
## Decision
- **A pass carries to a head that only merges the base branch in.** When no marker names
  the current head, `review-gate` takes the latest marker (the highest round, the last one
  posted in it). If its head `P` is an ancestor of the current head, and the PR's change is
  the same at both, the gate gives `P`'s verdict to the current head. That verdict goes
  through ADR-0019's rules first: owner-only, the highest round, any `fail` or `escalate`
  beating `pass`, and no self-authored verdict.
- **"The same change" is measured on content.** The gate diffs each head against the base
  branch with GitHub's compare API, from their merge bases. Both diffs must hold the same
  files with the same status, and the same added and removed lines in the same order.
  Context lines and hunk positions may differ, because the base moved under them.
  `docs/verified.toml` is left out, because a merge restamps it. A file without a patch
  (binary or too large), a list of 300 files (it may be truncated), or any API error fails
  closed: the gate stays pending, as before this ADR.
- **What still needs a review round:** a conflict resolution that changes the PR's own
  lines, a protocol renumber, a regenerated snapshot, or any other change to the PR's
  diff. A verdict for the current head always wins over a carried one.
- **The success description says so**, naming the reviewed SHA and this ADR, and it
  dispatches the after-review jobs for the new head as any pass does
  ([ADR-0054](0054-platform-ci-after-review.md)). CI still runs on every push.
- The decision function stays self-contained in the workflow file, and `gate-test` tests
  it, including `sameChange` and `carryCandidate`.

<a id="consequences"></a>
## Consequences
- A passed PR that falls behind `main` only needs `docs merge-main` and a push. It lands
  once CI is green, with no new review round.
- The gate now trusts that the diff from the merge base is the PR's whole change. A
  restamp after a merge isn't re-read by a reviewer. A false restamp is still caught by
  `docs check` when a section is stale, but not when a note is wrong, so builders keep
  the `--merge` note discipline ([waves › Without churn](../runbooks/waves.md#churn)).
- A semantic clash between the PR and the merged-in code (both sides compile, but the
  behaviour is wrong) gets no reviewer. CI's tests are the check, as they already are for
  a PR that lands without merging `main`.
- The gate makes two more read-only API calls when the head has no verdict.
