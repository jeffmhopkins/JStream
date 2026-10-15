<!-- @doc
id = "ADR-0054"
title = "Run the Windows, macOS and end-to-end CI jobs after review"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0054: Run the Windows, macOS and end-to-end CI jobs after review

<!-- @generated begin adr-status -->
> **Status:** proposed
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
On a PR, CI runs in two phases:
- **Every push** (`pull_request`): the Linux jobs only. That covers `fmt`, the Linux legs of
  `clippy` and `test`, `doc`, `deny`, `wasm`, `docs`, `lint-deps`, `gate-test` and `pr-title`.
- **After review** (`workflow_dispatch`): when `review-gate` sets `success` on the head SHA,
  from a pass verdict or a justified override, it dispatches `ci.yml` on the PR's branch.
  That run has only the Windows legs of `clippy` and `test`, plus `ui-e2e` and `macos`. It
  dispatches at most once per SHA: if a `test (windows-latest)` check run already exists,
  it does nothing.

A push to `main` still runs every job.

The required checks are unchanged. Until the after-review run reports, `clippy
(windows-latest)` and `test (windows-latest)` are missing, so GitHub keeps the PR unmergeable.
The `clippy` and `test` matrices choose their platforms from the event instead of skipping
legs with `if:`. A skipped job would report as passed and could let a PR merge untested.

<a id="consequences"></a>
## Consequences
- Runner use per PR drops to one Windows and macOS run per reviewed SHA.
- A PR merges a Windows run (about 10 minutes) after its review passes, rather than at once.
- A Windows-only failure shows up after review. The fix is a new push, which needs a new
  review round.
- `review-gate` gains `actions: write` and `checks: read`. It still runs from the default
  branch and never checks out PR code. The dispatched run uses the PR branch's `ci.yml` with
  `contents: read`, which is no more than a `pull_request` run already gets. Fork PRs are not
  dispatched.
- A branch whose `ci.yml` predates this ADR has no `workflow_dispatch` trigger, so the
  dispatch fails and logs a warning. That PR gets its Windows checks once it merges `main`.
