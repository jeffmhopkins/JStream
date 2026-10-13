<!-- @doc
id = "ADR-0019"
title = "Agent review gate v2: owner-only, round-aware verdicts"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0019: Agent review gate v2: owner-only, round-aware verdicts

Supersedes [ADR-0015](0015-review-and-governance.md).

<a id="context"></a>
## Context
JStream is built entirely by AI agents. All commits and PRs come from the maintainer's one
GitHub account, and GitHub never lets a PR's author approve that PR, so native required
reviews can't work. The maintainer runs their own harness of agents: builders open PRs,
and a reviewer agent watches every PR and comments with its findings. The maintainer wants
a merge to happen once the reviewer finds no issues. Separately, an agent under pressure
could pass the gate by weakening it: narrowing `covers`, adding `#[allow]`, retiring rules,
or editing CI.

Two flaws in ADR-0015's gate surfaced on PR #7:
1. **Any GitHub user could pass a PR.** The gate counted the newest verdict marker from
   *any* comment author. The repo is public, so an outsider could post a well-formed
   `pass` for the head SHA, and auto-merge would merge the PR.
2. **Parallel reviewer runs race.** The maintainer's harness posted three round-2 reviews
   within seven seconds (pass, fail, pass). "Newest marker wins" let the last `pass`
   override a blocking `fail`, and the PR merged with an unaddressed finding.

<a id="decision"></a>
## Decision
- **The review prompt lives in the repo** as [REVIEW.md](../../REVIEW.md). The
  maintainer's harness loads it. It pulls the applicable rules from the repo rather
  than restating them, and defines the finding format and the verdict marker.
- **Verdict marker:** the reviewer ends each round with
  `<!-- jstream-review v1 verdict=pass|fail|escalate|recuse sha=<head> round=<n> reviewer-session=<id> -->`.
  A re-review of the same head (e.g. after a maintainer decision) uses a **higher round
  number**.
- **Only the repository owner's account counts.** Verdict markers and ADR decision comments
  (`/accept`, `/reject`) count only when GitHub reports the comment's
  `author_association` as `OWNER`. Comments from anyone else are ignored. *(New.)*
- **`review-gate`** (`.github/workflows/review-gate.yml`) is a required commit status. It
  considers the owner's markers whose `sha` is the current head and takes the **highest
  round**. Within that round, any `fail` or `escalate` beats `pass`, so parallel runs
  can't race. A self-authored verdict (the session appears in the PR's `Claude-Session:`
  trailers) fails. It is `success` only when that round has a `pass` and no blocking
  verdict. Any new push resets it. The workflow runs from the default branch, never
  checks out PR code, and its decision function is tested in CI (`gate-test`) against
  the workflow file itself. *(Changed from "newest marker wins".)*
- **Merging:** builders open PRs with GitHub auto-merge (squash) enabled. GitHub merges once
  every required check is green. No agent merges by hand.
- **Disputes:** a blocking finding still disputed after three rounds is escalated with
  `needs-human`. The maintainer resolves it, either by fixing the code or by applying
  `review-override` with a "Review override" justification in the PR description.
- **Governance:** changes listed in [DOCS › Governance](../DOCS.md#governance) need the
  `governance-approved` label plus a "Governance change" justification. Agents may
  apply both labels, and the weekly sweep lists every use for the maintainer to audit.
- **Harness expectation:** the reviewer harness runs one review per PR head at a time. The
  gate tolerates violations conservatively, but duplicate rounds waste work.

<a id="consequences"></a>
## Consequences
- An outsider can no longer pass a PR, and a parallel `pass` can no longer hide a `fail`.
- The gate is still a **process control, not a security control**, against agents holding
  the owner's token: they post as `OWNER`. The session check and the weekly audit make
  forgery visible but not impossible. A separate bot identity for the reviewer would
  close this, if it ever matters.
- `REVIEW.md` and the gate workflow are part of the trust base. Editing them is a
  governance change, and the reviewer always reviews against the base branch's copies.
- There is no CI cost for review: it runs in the maintainer's harness.
