<!-- @doc
id = "ADR-0018"
title = "CI supply-chain policy: pinned actions, Dependabot, current runtimes"
kind = "adr"
status = "accepted"
tier = "maintainer"
-->

# ADR-0018: CI supply-chain policy: pinned actions, Dependabot, current runtimes

<a id="context"></a>
## Context
Required checks, including the `review-gate` merge gate, run third-party GitHub Actions.
PR #6 pinned them to commit SHAs, so a moved tag can't change what runs. Pins don't update
themselves, though, so security fixes stop arriving unless something proposes bumps. Two of
the pinned actions (`actions/checkout` v4, `actions/github-script` v7) still run on Node 20.
Every change under `.github/**` is a governance change ([R-GOV-001](../RULES.md#R-GOV-001)),
and Dependabot can't write the justification that rule requires.

<a id="decision"></a>
## Decision
1. **Pinning.** Every third-party action is referenced by a full commit SHA with its release
   in a trailing comment (`@<sha> # vX.Y.Z`) ([R-DEP-007](../RULES.md#R-DEP-007)). Pins
   are resolved with `git ls-remote --tags`, dereferencing annotated tags.
2. **Dependabot** (`.github/dependabot.yml`) proposes weekly grouped updates for:
   - `github-actions`, as one PR titled `ci(deps): …`;
   - `cargo`, as one PR titled `build(deps): …`.
3. **Pre-approved governance for Dependabot action bumps.** A Dependabot PR whose diff
   **only** changes `uses: <action>@<sha> # <version>` references counts as justified by this
   ADR. It needs no `governance-approved` label or justification section. The reviewer
   confirms the diff is limited to those references. For a major-version bump, the
   reviewer also reads the action's changelog for breaking changes against our usage.
   Any other `.github/**` change follows R-GOV-001 as usual.
4. **Dependabot PRs follow the normal loop.** The reviewer agent reviews them under
   [REVIEW.md](../../REVIEW.md), and a builder agent enables auto-merge once they are opened.
   They carry no `Claude-Session:` trailer, so the reviewer's independence check passes
   trivially.
5. **Runtimes.** Actions run on Node 24. This PR moves `actions/checkout` v4.4.0 → v7.0.1
   and `actions/github-script` v7.1.0 → v9.0.0. Both changelogs were checked: v7 blocks
   fork checkouts under `pull_request_target` (our gate checks nothing out), and v9 breaks
   only `require('@actions/github')` and redeclaring `getOctokit` (our script does neither).
6. **Coverage.** [ARCH › Repository layout](../ARCHITECTURE.md#repo-layout) no longer covers
   `.github/workflows/**`, because that section describes the tree, not workflow contents.
   Otherwise every Dependabot bump would make it stale.

<a id="consequences"></a>
## Consequences
- Upstream fixes reach required checks through reviewed PRs. Nothing changes silently.
- Some recurring PR traffic. Grouping keeps it to at most two Dependabot PRs a week.
- The pre-approval is narrow. A Dependabot PR that touches anything beyond pinned
  references falls back to the normal governance rule.
