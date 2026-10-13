+++
id = "ADR-0015"
title = "Review and governance for an agent-driven repo"
kind = "adr"
status = "accepted"
+++

# ADR-0015: Review and governance for an agent-driven repo

<a id="context"></a>
## Context
Most PRs are written by agents. Thirteen rules can only be judged, not checked
mechanically. The maintainer can't formally approve PRs pushed under their own account.
An agent under pressure can pass the gate by weakening it: narrowing `covers`, adding
`#[allow]`, retiring rules, or editing CI.

<a id="decision"></a>
## Decision
- **`ai-review` required check:** an AI reviewer in a fresh context that did not author
  the PR. It reviews the diff against the rules and sections returned by
  `xtask docs for` for each changed path, and judges every verification note. Blocking
  findings fail the check. Only the maintainer can override, with the label
  `ai-review-override`.
- **`governance` required check:** changes listed in
  [DOCS › Governance](../DOCS.md#governance) fail unless the PR has the label
  `governance-approved`.
- **Agents never apply either label** ([R-GOV-002](../RULES.md#R-GOV-002)). The check
  accepts the label only when it was applied by an account listed in
  `.github/MAINTAINERS`, according to the PR's timeline events.

<a id="consequences"></a>
## Consequences
- The maintainer's attention goes to governance changes and AI-review disputes, not to
  every line.
- **Known limit:** if agents act through the maintainer's own GitHub credentials, the
  label's author can't be told apart from the maintainer. Then R-GOV-002 is honor-system
  only. Agents should use a distinct bot identity (such as a GitHub App) wherever
  possible.
- The AI review needs an API key secret in CI, and costs money per PR.
