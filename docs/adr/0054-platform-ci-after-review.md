<!-- @doc
id = "ADR-0054"
title = "Run the Windows, macOS and end-to-end CI jobs after review"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0054: Run the Windows, macOS and end-to-end CI jobs after review

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
With dozens of PRs open, CI runs wait 25 to 35 minutes for a GitHub-hosted runner and then
take about 8 minutes. Every push ran the full matrix: Linux, Windows, macOS and the UI
end-to-end tests. Most pushes are fixes for reviewer findings, and each one starts a new
review round anyway, so the Windows and macOS runs on commits that will never merge are
the bulk of the queue. Skipping CI on drafts helped, but it doesn't help ready PRs that
are still in review.

<a id="decision"></a>
## Decision
The CI jobs are split across two workflows, and no check name appears in both:
- **`ci.yml`** runs the Linux jobs on every PR push and on `main`: `fmt`, the Linux legs of
  `clippy` and `test`, `doc`, `deny`, `wasm`, `docs`, `lint-deps`, `gate-test` and `pr-title`.
- **`after-review.yml`** runs the Windows legs of `clippy` and `test`, `ui-e2e` and `macos`.
  It runs on `main`, and on a PR when `review-gate` dispatches it. That happens when the gate
  sets `success` on the head SHA, from a pass verdict or a justified override. The gate
  dispatches on the PR's branch unless a `test (windows-latest)` run for that SHA is
  already queued, running or passed. A failed or cancelled one is dispatched again on the
  next gate event, or re-run by hand.

The required checks are unchanged. Until the after-review run reports, `clippy
(windows-latest)` and `test (windows-latest)` are missing, so GitHub keeps the PR
unmergeable. Each required name comes from exactly one workflow. If one workflow skipped a
job that the other also defines, it would publish a skipped run under that name on the same
SHA, and a skipped run counts as passed, so a failed check could read as green.

<a id="consequences"></a>
## Consequences
- Runner use per PR drops to one Windows and macOS run per reviewed SHA.
- A PR merges a Windows run (about 10 minutes) after its review passes, rather than at once.
- A Windows-only failure shows up after review. The fix is a new push, which needs a new
  review round.
- The after-review run tests the PR branch's tip, not the merge with `main` that the Linux
  jobs test. If a push lands between the gate run and the dispatch, it tests that newer
  commit. A new push doesn't cancel an in-flight after-review run for the old SHA; the next
  dispatch on the branch does.
- `review-gate` gains `actions: write` and `checks: read`. It still runs from the default
  branch and never checks out PR code. The dispatched run uses the PR branch's
  `after-review.yml` with `contents: read`, which is no more than a `pull_request` run
  already gets. Fork PRs are not dispatched.
- A branch without `after-review.yml` can't be dispatched, so the gate logs a warning. That
  PR gets its Windows checks once it merges `main`.
- A PR that changes `after-review.yml` runs it on every push. That tests the change, and it
  is how the PR adding the file gets its Windows checks: `workflow_dispatch` can only start
  a workflow that is already on the default branch.
