<!-- @doc
id = "ADR-0055"
title = "Wave branches: PRs batch into a wave, and only the wave's PR into main runs full CI"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0055: Wave branches: PRs batch into a wave, and only the wave's PR into main runs full CI

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
Even with platform jobs moved after review ([ADR-0054](0054-platform-ci-after-review.md)),
every PR still runs the Linux tests, docs build, `deny` and `wasm` on every push, and every
merge into `main` re-conflicts the other open PRs, which then need another merge, another CI
run and another review round. With dozens of PRs open at once, the runner queue and the
re-merge churn dominate how long a change takes to land.

<a id="decision"></a>
## Decision
Work can land in batches through **wave branches** named `wave/<name>`:
- A wave branch starts from `main`. Builders open their PRs against it, and the reviewer
  reviews each one as usual. Those PRs skip only `test`, `doc`, `deny`, `wasm` and the
  after-review platform jobs; every other check, `governance` included, runs as on any PR.
- They squash-merge into the wave branch once `review-gate` passes. A `wave/*` ruleset
  requires a PR and `review-gate`, so auto-merge works there, and allows squash and merge
  commits.
- The wave's owner brings `main` into the wave through a **sync PR** that lands as a merge
  commit, never a squash, so `main` becomes an ancestor of the wave and later merges don't
  re-conflict on what was already resolved.
- One PR from the wave branch into `main` runs every check, including the after-review
  platform jobs. The reviewer reviews what the wave's PRs didn't show: merge commits,
  conflict resolutions and direct commits to the wave branch ([REVIEW.md](../../REVIEW.md#gather)).
- The wave's owner keeps it merged with `main`, fixes failures the full run finds, and
  opens the PR into `main` when the wave is ready.

This amends [ADR-0054](0054-platform-ci-after-review.md) for PRs into `wave/*` only: they
skip the Linux tests and the after-review dispatch. Everything else in ADR-0054 stands.

PRs straight into `main` keep working exactly as before. Which branch each PR targets, who
owns a wave and how it lands are in the [waves runbook](../runbooks/waves.md).

<a id="consequences"></a>
## Consequences
- Tests, docs build, `deny`, `wasm` and the platform jobs run once per wave instead of on every
  push of every PR. PRs inside a wave conflict only with each other, not with all of `main`.
- A failure surfaces late, at the wave's PR into `main`, and blocks the whole wave until
  someone finds which change caused it. The wave branch is the place to fix it.
- The squash of the wave PR puts the whole wave on `main` as one commit. The wave branch's
  history (one squashed commit per PR) shows which PR made each change.
- Protocol-version bumps and doc stamps still have to be right in each PR, since the wave's
  full run is where a mistake shows up.
- The `wave/*` ruleset is a repository setting, which only the maintainer can create.
