<!-- @doc
id = "ADR-0070"
title = "PRs into a wave run the Linux tests"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0070: PRs into a wave run the Linux tests

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0055](0055-wave-branches.md) lets a PR into a `wave/*` branch skip `test`, `doc`,
`deny`, `wasm` and the after-review platform jobs, so those run once, on the wave's PR into
`main`. It accepted that a failure surfaces late.

#317, a PR into `wave/m1-3`, showed the cost. At its head `b36ad5d`, `PROTOCOL_VERSION` was
0.31 but the protocol schema snapshots still said 0.30, so two `jstream-protocol` snapshot
tests failed. Every check that ran was green: `test` was skipped. The reviewer found the two
failures only by running the tests locally. A PR into a wave merges on `review-gate` alone,
so a review that didn't run them would have let the wave carry a red test that no check
shows. Whoever lands the wave then has to find which of its PRs caused it.

The Linux `test` job is the check that catches these mistakes: snapshots, protocol bumps,
fixtures. The repository is public, and the job runs on a standard GitHub-hosted runner,
where minutes are free. Running it on more PRs costs queue time, not money, so this is not a
spending decision and is `agent` tier ([ADR-0043](0043-narrow-maintainer-tier.md)).

<a id="decision"></a>
## Decision
A PR into a `wave/*` branch runs the Linux `test` job, like any PR. Draft PRs still skip it.

This amends [ADR-0055](0055-wave-branches.md) in that one point: wave PRs still skip `doc`,
`deny`, `wasm` and the after-review platform jobs (the Windows legs of `clippy` and `test`,
`ui-e2e`, `dist` and `macos`), and those still run on the wave's PR into `main`. Everything
else in ADR-0055 stands.

<a id="consequences"></a>
## Consequences
- Every push of a ready PR into a wave takes one more Linux runner for the length of the
  test suite, so wave PRs wait longer in the runner queue.
- A failing Linux test shows as a red `test (ubuntu-latest)` on the PR that causes it,
  where its own builder fixes it, and the reviewer counts it as a red required check. It
  no longer waits for the wave's PR into `main`.
- Windows-only and macOS-only test failures, and `doc`, `deny` and `wasm` failures, still
  surface only at the wave's PR into `main`.
