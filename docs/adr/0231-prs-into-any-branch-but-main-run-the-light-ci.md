<!-- @doc
id = "ADR-0231"
title = "PRs into any branch but main run the light CI"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0231: PRs into any branch but main run the light CI

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0055](0055-wave-branches.md) and [ADR-0070](0070-wave-prs-run-linux-tests.md) give a PR
into a `wave/*` branch a light CI run: the quick jobs and the Linux tests. `doc`, `deny`,
`wasm` and the after-review platform jobs run once, on the wave's own PR into `main`. The
check that decides this is the base branch's name: only `wave/*` qualifies.

Work is also gathered on branches with other names: a release train cut from a builder's
branch, and stacks, where a PR's base is the PR below it. A PR into one of those ran
everything a PR into `main` runs: the Linux end-to-end, virtual camera, browser and desktop
jobs, `doc`, `deny`, `wasm`, `bench` and `perf-linux`, the install workflows, and, once
review passed, the Windows and macOS after-review jobs. The branch's own PR into `main` then
ran all of it again. With a dozen PRs open into one train, that duplicate load filled the
account's shared Actions runners and held up every other repository's checks for most of an
hour. Those branches aren't protected, so a PR into one could merge before its own run
even started: the run bought nothing.

Like ADR-0070, this moves where checks run on free hosted runners. It is not a spending
decision and is `agent` tier ([ADR-0043](0043-narrow-maintainer-tier.md)).

<a id="decision"></a>
## Decision
A PR whose base is any branch but `main` gets the light CI a wave PR gets: `fmt`, the Linux
`clippy` and `test`, `docs`, `lint-deps`, `gate-test`, `pr-title` and `governance`. It skips
`doc`, `deny`, `wasm`, `e2e-stream`, `vcam-linux`, `browser-linux`, `desktop-linux`,
`bench`, `perf-linux`, `linux-install`, `mac-install` and the after-review jobs. The
branch's own PR into `main` runs every check. The test is `base == main` in each workflow,
not a branch-name pattern, so a train or stack needs no naming convention to qualify.

`governance` and `bench` also skip draft PRs, as `ci` already does, and run when the draft
is marked ready.

Everything else in ADR-0055 and ADR-0070 stands.

<a id="consequences"></a>
## Consequences
- A PR into a train or a stack takes about seven Linux runners per push instead of about
  fourteen, plus no Windows or macOS runners. The train's PR into `main` carries the full
  run once for all of them.
- A failure only the skipped jobs catch surfaces on the branch's PR into `main`, as it
  already does for a wave. Whoever lands the branch finds which PR caused it.
- A stacked PR whose base merges is retargeted to `main` without a CI run. Its old light
  run's skipped `doc`, `deny` and `wasm` would count as passing, so the stacked PR merges
  `main` in before it merges, as the [waves runbook](../runbooks/waves.md#churn) already
  asks; that push runs every check. `review-gate` evaluates the retarget's `edited` event
  and dispatches the after-review jobs then, and their required contexts block the merge
  until they pass.
