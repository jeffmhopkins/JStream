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
mechanically. An agent under pressure can pass the gate by weakening it: narrowing `covers`, adding
`#[allow]`, retiring rules, or editing CI.

<a id="decision"></a>
## Decision
- **`ai-review` required check:** an AI reviewer running in a fresh context that did not
  author the PR. It reviews the diff against the rules and sections that `xtask docs for`
  returns for each changed path, and judges every verification note. Blocking findings
  fail the check.
- **`governance` required check:** changes listed in
  [DOCS › Governance](../DOCS.md#governance) fail unless the PR carries the label
  `governance-approved`.
- **Declare, justify, audit.** The maintainer chose to let agents apply both labels
  (`governance-approved`, `ai-review-override`) rather than reserve them for the
  maintainer. A label only counts when the PR description has a matching, non-empty
  justification section ("Governance change" / "AI review override"). The `governance`
  check enforces this.
- The weekly sweep lists every PR merged with either label in the pinned issue, for the
  maintainer to audit after the fact.

<a id="consequences"></a>
## Consequences
- Agents are never blocked waiting for the maintainer. Weakening a check is still never
  silent: it has to be labeled, justified, and it appears in the weekly audit.
- Both gates are only as strong as that after-the-fact audit. A bad override can merge, and
  is caught later rather than prevented.
- No special GitHub identity is needed for agents.
- The AI review needs an API key secret in CI, and costs money per PR.
