<!-- @doc
id = "ADR-0056"
title = "CI supply-chain policy v2: Dependabot also updates the end-to-end tests' npm packages"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0056: CI supply-chain policy v2: Dependabot also updates the end-to-end tests' npm packages

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

Supersedes [ADR-0018](0018-ci-supply-chain-policy.md).

<a id="context"></a>
## Context
[ADR-0018](0018-ci-supply-chain-policy.md) set the CI supply-chain policy: pinned
actions, weekly Dependabot updates, current runtimes. Its decision lists two Dependabot
ecosystems, `github-actions` and `cargo`, and its consequences promise at most two
Dependabot PRs a week. `.github/dependabot.yml` now has a third entry: `npm`, for the
Playwright end-to-end tests of the web UI in `crates/jstream-ui/tests/e2e`
([Harness](../../crates/jstream-ui/tests/e2e/README.md#harness)). Their lockfile pins
every package with an integrity hash, and CI installs it with install scripts off. The
ADR no longer describes what the config does.

ADRs can't be partially amended ([R-DOC-009](../RULES.md#R-DOC-009)). So this ADR restates
ADR-0018's whole decision with the third ecosystem added, and supersedes ADR-0018. Its
subject is CI policy, not one of the maintainer-tier subjects, so it is agent tier
([ADR-0043](0043-narrow-maintainer-tier.md)).

The background for ADR-0018 is unchanged. Required checks, including the `review-gate`
merge gate, run third-party GitHub Actions, pinned to commit SHAs so a moved tag can't
change what runs. Pins don't update themselves, so security fixes stop arriving unless
something proposes bumps. Every change under `.github/**` is a governance change
([R-GOV-001](../RULES.md#R-GOV-001)), and Dependabot can't write the justification that
rule requires.

<a id="decision"></a>
## Decision
1. **Pinning.** Every third-party action is referenced by a full commit SHA with its release
   in a trailing comment (`@<sha> # vX.Y.Z`) ([R-DEP-007](../RULES.md#R-DEP-007)). Pins
   are resolved with `git ls-remote --tags`, dereferencing annotated tags.
2. **Dependabot** (`.github/dependabot.yml`) proposes weekly grouped updates, each group
   as one PR, for:
   - `github-actions`, titled `ci(deps): …`;
   - `cargo`, titled `build(deps): …`;
   - `npm` in `crates/jstream-ui/tests/e2e`, the end-to-end tests' Playwright packages,
     titled `test(deps): …` (or `test(deps-dev): …` for development dependencies).

   Every entry waits 5 days after a release before proposing it.
3. **Pre-approved governance for Dependabot action bumps.** A Dependabot PR whose diff
   **only** changes `uses: <action>@<sha> # <version>` references counts as justified by this
   ADR. It needs no `governance-approved` label or justification section. The reviewer
   confirms the diff is limited to those references. For a major-version bump, the
   reviewer also reads the action's changelog for breaking changes against our usage.
   Any other `.github/**` change follows R-GOV-001 as usual. The pre-approval covers only
   action pins. A `cargo` or `npm` PR changes manifests and lockfiles outside `.github/**`,
   so this pre-approval does not apply to it: it is a governance change only if it
   touches something [DOCS › Governance](../DOCS.md#governance) lists, and then it follows
   R-GOV-001.
4. **Dependabot PRs follow the normal loop.** The reviewer agent reviews them under
   [REVIEW.md](../../REVIEW.md), and a builder agent enables auto-merge once they are opened.
   They carry no `Claude-Session:` trailer, so the reviewer's independence check passes
   trivially. When one makes a doc section stale, a builder re-reads and verifies it on
   the PR's branch, as for any other PR.
5. **Runtimes.** Actions run on Node 24. ADR-0018 moved `actions/checkout` to v7 and
   `actions/github-script` to v9 for this. Their changelogs were checked then: v7 blocks
   fork checkouts under `pull_request_target` (our gate checks nothing out), and v9 breaks
   only `require('@actions/github')` and redeclaring `getOctokit` (our script does neither).
6. **Coverage.** [ARCH › Repository layout](../ARCHITECTURE.md#repo-layout) does not cover
   `.github/workflows/**`, because that section describes the tree, not workflow contents.
   Otherwise every Dependabot bump would make it stale.

<a id="consequences"></a>
## Consequences
- Upstream fixes reach required checks, and the end-to-end tests' packages, through
  reviewed PRs. Nothing changes silently.
- Some recurring PR traffic. Grouping keeps it to at most three Dependabot PRs a week, one
  per ecosystem.
- The pre-approval is narrow. A Dependabot PR that touches anything beyond pinned
  references falls back to the normal governance rule.
- An `npm` PR changes `crates/jstream-ui/tests/e2e/package.json`, which doc sections
  cover, so it waits for a builder to re-verify them before it can merge.
