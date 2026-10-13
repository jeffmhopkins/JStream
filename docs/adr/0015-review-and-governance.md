+++
id = "ADR-0015"
title = "Agent review gate and governance with a single GitHub account"
kind = "adr"
status = "accepted"
+++

# ADR-0015: Agent review gate and governance with a single GitHub account

<a id="context"></a>
## Context
JStream is built entirely by AI agents. All commits and PRs come from the maintainer's one
GitHub account, and GitHub never lets a PR's author approve that PR, so native required
reviews can't work. The maintainer runs their own harness of agents: builders open PRs,
and a reviewer agent watches every PR and comments with its findings. The maintainer wants
a merge to happen once the reviewer finds no issues. Separately, an agent under pressure
could pass the gate by weakening it: narrowing `covers`, adding `#[allow]`, retiring rules,
or editing CI.

<a id="decision"></a>
## Decision
- **The review prompt lives in the repo** as [REVIEW.md](../../REVIEW.md). The
  maintainer's harness loads it. It pulls the applicable rules from the repo rather
  than restating them, and defines the finding format and the verdict marker.
- **Verdict marker:** the reviewer ends each round with
  `<!-- jstream-review v1 verdict=pass|fail|escalate|recuse sha=<head> round=<n> reviewer-session=<id> -->`.
- **`review-gate`** (`.github/workflows/review-gate.yml`) is a required commit status. It
  is `success` only when the newest marker says `pass` **for the current head SHA**, and
  the reviewer's session authored none of the PR's commits (checked against the
  `Claude-Session:` trailers). Any new push resets it. The workflow runs from the
  default branch and never checks out PR code.
- **Merging:** builders open PRs with GitHub auto-merge (squash) enabled. GitHub merges once
  every required check is green. No agent merges by hand.
- **Disputes:** a blocking finding still disputed after three rounds is escalated with
  `needs-human`. The maintainer resolves it, either by fixing the code or by applying
  `review-override` with a "Review override" justification in the PR description.
- **Governance:** changes listed in [DOCS › Governance](../DOCS.md#governance) need the
  `governance-approved` label plus a "Governance change" justification. Agents may
  apply both labels, and the weekly sweep lists every use for the maintainer to audit.

<a id="consequences"></a>
## Consequences
- The whole loop runs without a human, and the maintainer only sees escalations and the
  weekly audit.
- **The gate is a process control, not a security control.** The reviewer posts through
  the same account as the builders, so an agent holding the token *could* forge a
  `pass`. Requiring the verdict's session to differ from the authors' sessions,
  together with the weekly audit, makes forgery visible but not impossible. If that
  ever matters, the reviewer can move to a separate bot identity (a GitHub App
  or a GitHub Actions bot), and the gate can accept only that actor's verdicts.
- `REVIEW.md` becomes part of the trust base. Editing it is a governance change, and the
  reviewer always reviews against the base branch's copy.
- There is no CI cost for review: it runs in the maintainer's harness.
